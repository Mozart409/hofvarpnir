//! End-to-end check of telemetry export as the homelab runs it: OTLP/HTTP and
//! Loki pushes, both over HTTPS to a bearer-token proxy whose certificate is
//! issued by a private CA (step-ca in production).
//!
//! Failure modes this guards against, written down before the fix:
//!
//! 1. No HTTP client compiled into the OTLP exporter, so `http/protobuf`
//!    cannot export at all and tracing silently switches off.
//! 2. `Authorization` never sent: the OTLP headers env var ignored, or no way
//!    to give the Loki layer a header. The proxy answers 401; data is lost.
//! 3. The private CA rejected because a client trusts only bundled webpki
//!    roots and never reads the CA bundle mounted into the container.
//! 4. Requests sent to a path the proxy does not route (`/v1/traces` and
//!    `/loki/api/v1/push` are the only ones it forwards).
//! 5. A rejected token failing silently instead of logging the 401.
//! 6. `OTEL_TRACES_SAMPLER` ignored because the provider is built by hand.
//!
//! `init_tracing` installs a process-global subscriber and reads the process
//! environment, and `std::env::set_var` is `unsafe` (forbidden workspace-wide).
//! So each scenario re-runs this test binary as a child with its own
//! environment and only `child_probe` selected; the parent plays the proxy.

// `clippy.toml`'s test exemptions only cover `#[test]` fns and `#[cfg(test)]`
// modules; helpers in an integration-test crate need them declared here.
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::indexing_slicing)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// Set only in the child process; `child_probe` is a no-op without it.
const CHILD_ENV: &str = "HOF_OTEL_PROBE_CHILD";
const PROBE_SPAN: &str = "hof_otel_probe";
const PUSH_TOKEN: &str = "s3cret-push-token";
const TRACES_PATH: &str = "/v1/traces";
const LOKI_PATH: &str = "/loki/api/v1/push";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/otel-tls")
}

fn bearer_header(token: &str) -> String {
    // The OTLP headers format: comma-separated key=value, values percent-encoded.
    format!("Authorization=Bearer%20{token}")
}

#[derive(Debug, Clone)]
struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Stand-in for Caddy on the otel host: HTTPS with a leaf certificate from a
/// private CA, 401 unless `Authorization: Bearer <token>` matches.
struct Proxy {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Proxy {
    async fn start(token: &'static str) -> Self {
        let fixtures = fixtures_dir();
        let certs = CertificateDer::pem_file_iter(fixtures.join("leaf.pem"))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key = PrivateKeyDer::from_pem_file(fixtures.join("leaf.key")).unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);

        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let captured = Arc::clone(&captured);
                tokio::spawn(async move {
                    // A client that rejects our certificate aborts the
                    // handshake. That is a scenario outcome the assertions
                    // see as "no requests", not a proxy failure.
                    if let Ok(tls) = acceptor.accept(tcp).await {
                        serve_connection(tls, token, captured).await;
                    }
                });
            }
        });

        Self {
            url: format!("https://localhost:{port}"),
            requests,
        }
    }

    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    fn requests_to(&self, path: &str) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|r| r.path == path)
            .collect()
    }
}

/// Minimal HTTP/1.1 keep-alive loop. Both clients send `content-length`
/// bodies, which is all this needs to parse.
async fn serve_connection<S>(stream: S, token: &str, captured: Arc<Mutex<Vec<Request>>>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let expected = format!("Bearer {token}");
    let mut reader = BufReader::new(stream);
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).await.unwrap_or(0) == 0 {
            return;
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();

        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }

        let len = headers
            .iter()
            .find(|(k, _)| k == "content-length")
            .and_then(|(_, v)| v.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0; len];
        if reader.read_exact(&mut body).await.is_err() {
            return;
        }

        let request = Request {
            method,
            path,
            headers,
            body,
        };
        let authorized = request
            .header("authorization")
            .is_some_and(|v| v == expected);
        captured.lock().unwrap().push(request);

        let response: &[u8] = if authorized {
            b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n"
        } else {
            b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n"
        };
        let stream = reader.get_mut();
        if stream.write_all(response).await.is_err() || stream.flush().await.is_err() {
            return;
        }
    }
}

/// Runs `child_probe` in a fresh process configured like production, plus
/// `extra` env vars. Returns the child's combined stdout and stderr.
async fn run_child(proxy: &Proxy, extra: &[(&str, &str)]) -> String {
    let mut cmd = tokio::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["child_probe", "--exact", "--nocapture", "--test-threads=1"])
        // No inherited OTEL_*/LOKI_*/SSL_* from the developer's shell.
        .env_clear()
        .env(CHILD_ENV, "1")
        .env("RUST_LOG", "info")
        // The only trust anchor the child gets is our private CA, the way the
        // container only gets the mounted bundle carrying the step-ca root.
        .env("SSL_CERT_FILE", fixtures_dir().join("ca.pem"))
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", &proxy.url)
        .env("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")
        .env("OTEL_SERVICE_NAME", "hofvarpnir-otel-probe")
        .env("LOKI_URL", &proxy.url);
    for (key, value) in extra {
        cmd.env(key, value);
    }

    let output = tokio::time::timeout(Duration::from_mins(2), cmd.output())
        .await
        .expect("child probe timed out")
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "child probe failed:\n{text}");
    text
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The child half of every scenario. Emits one span and one log line through
/// the real `init_tracing`, gives the Loki task time to push, then flushes.
#[tokio::test]
async fn child_probe() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }

    let guard = hof_core::telemetry::init_tracing();
    {
        let span = tracing::info_span!(PROBE_SPAN, probe = true);
        let _entered = span.enter();
        tracing::info!("hof otel probe log line");
    }
    // tracing-loki pushes from a background task on its own schedule.
    tokio::time::sleep(Duration::from_secs(3)).await;
    guard.shutdown();
    // Let the Loki task report a failed push before the process exits.
    tokio::time::sleep(Duration::from_secs(1)).await;
    drop(guard);
}

/// The production shape: HTTPS to a private-CA proxy, bearer token on both
/// pipelines. Covers failure modes 1-4.
#[tokio::test]
async fn test_export_over_https_with_bearer_token() {
    let proxy = Proxy::start(PUSH_TOKEN).await;
    let header = bearer_header(PUSH_TOKEN);
    let output = run_child(
        &proxy,
        &[
            ("OTEL_EXPORTER_OTLP_HEADERS", &header),
            ("LOKI_HEADERS", &header),
        ],
    )
    .await;

    let expected_auth = format!("Bearer {PUSH_TOKEN}");

    let traces = proxy.requests_to(TRACES_PATH);
    assert!(
        !traces.is_empty(),
        "no OTLP export reached {TRACES_PATH}\nrequests: {:?}\nchild output:\n{output}",
        proxy.requests()
    );
    for request in &traces {
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.header("authorization"),
            Some(expected_auth.as_str())
        );
        assert_eq!(
            request.header("content-type"),
            Some("application/x-protobuf")
        );
    }
    // Protobuf carries strings as raw UTF-8, so the span name is findable.
    assert!(
        traces
            .iter()
            .any(|r| contains(&r.body, PROBE_SPAN.as_bytes())),
        "the probe span is not in any exported batch"
    );

    let logs = proxy.requests_to(LOKI_PATH);
    assert!(
        !logs.is_empty(),
        "no Loki push reached {LOKI_PATH}\nrequests: {:?}\nchild output:\n{output}",
        proxy.requests()
    );
    for request in &logs {
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.header("authorization"),
            Some(expected_auth.as_str())
        );
    }

    let stray: Vec<_> = proxy
        .requests()
        .into_iter()
        .filter(|r| r.path != TRACES_PATH && r.path != LOKI_PATH)
        .map(|r| r.path)
        .collect();
    assert!(stray.is_empty(), "requests to unrouted paths: {stray:?}");

    // A clean run logs no export or shutdown trouble (the probe calls
    // `shutdown()` and then drops the guard, which shuts down again).
    for noise in [
        "401",
        "couldn't send logs",
        "ExportError",
        "Failed to shut down",
    ] {
        assert!(
            !output.contains(noise),
            "unexpected `{noise}` in:\n{output}"
        );
    }
}

/// A wrong token must show up in the app's own logs as a 401 on both
/// pipelines (failure mode 5), not vanish.
#[tokio::test]
async fn test_rejected_token_is_logged() {
    let proxy = Proxy::start(PUSH_TOKEN).await;
    let header = bearer_header("wrong-token");
    let output = run_child(
        &proxy,
        &[
            ("OTEL_EXPORTER_OTLP_HEADERS", &header),
            ("LOKI_HEADERS", &header),
        ],
    )
    .await;

    // Both pipelines reached the proxy and were refused ...
    assert!(!proxy.requests_to(TRACES_PATH).is_empty(), "{output}");
    assert!(!proxy.requests_to(LOKI_PATH).is_empty(), "{output}");

    // ... and each refusal is visible in the child's log output.
    let rejections: Vec<&str> = output.lines().filter(|l| l.contains("401")).collect();
    assert!(
        rejections.iter().any(|l| l.to_lowercase().contains("loki")),
        "no Loki 401 in child output:\n{output}"
    );
    assert!(
        rejections.iter().any(|l| l.contains("opentelemetry")),
        "no OTLP 401 in child output:\n{output}"
    );
}

/// The sampling ratio must be tunable from the environment (failure mode 6).
/// Ratio 0 samples nothing; the Loki push proves the child ran and exported.
#[tokio::test]
async fn test_sampler_env_is_honored() {
    let proxy = Proxy::start(PUSH_TOKEN).await;
    let header = bearer_header(PUSH_TOKEN);
    let output = run_child(
        &proxy,
        &[
            ("OTEL_EXPORTER_OTLP_HEADERS", &header),
            ("LOKI_HEADERS", &header),
            ("OTEL_TRACES_SAMPLER", "parentbased_traceidratio"),
            ("OTEL_TRACES_SAMPLER_ARG", "0.0"),
        ],
    )
    .await;

    assert!(
        !proxy.requests_to(LOKI_PATH).is_empty(),
        "control failed: no Loki push, so the child never exported anything\n{output}"
    );
    let exported_probe = proxy
        .requests_to(TRACES_PATH)
        .iter()
        .any(|r| contains(&r.body, PROBE_SPAN.as_bytes()));
    assert!(
        !exported_probe,
        "a span was exported at sample ratio 0\n{output}"
    );
}
