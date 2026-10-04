//! End to end: a config file on disk resolves to a token that reaches the
//! server as `Authorization: Bearer ...`.
//!
//! Each scenario ends with the mock server verifying it received exactly one
//! authenticated status request carrying the expected token.

// `allow-unwrap-in-tests` covers `#[test]` fns, not the shared helpers below.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use hof_tui::client::ApiClient;
use hof_tui::config::{self, Config, PartialConfig};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Mount a status endpoint that only answers 200 for `token`; the mock fails
/// verification on drop unless it saw exactly one such request.
async fn server_expecting(token: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/system/status"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .expect(1)
        .mount(&server)
        .await;
    server
}

fn env_with(vars: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
    move |key| vars.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())
}

async fn connect(config: &Config) {
    let client = ApiClient::new(&config.api_url, &config.token).unwrap();
    client.system_status().await.unwrap();
}

/// The `NixOS` setup: `~/.config` is read-only and the config file is a
/// home-manager symlink into a world-readable store path. The key is never in
/// that file; a `token_command` reads it from an agenix-style secret file
/// (mode 400, trailing newline) through a pipeline. `HOF_API_URL` is exported
/// but empty, which must not shadow the file's `api_url`.
#[tokio::test]
async fn token_command_from_readonly_symlinked_config_authenticates() {
    let server = server_expecting("hof_sk_agenix_secret").await;
    let root = tempfile::tempdir().unwrap();

    let secret = root.path().join("run-agenix-hof");
    fs::write(&secret, "hof_sk_agenix_secret\n").unwrap();
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o400)).unwrap();

    let store_file = root.path().join("store-hm_tui.toml");
    fs::write(
        &store_file,
        format!(
            "api_url = \"{}/\"\ntoken_command = \"cat '{}' | head -n 1\"\n",
            server.uri(),
            secret.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&store_file, fs::Permissions::from_mode(0o444)).unwrap();

    let xdg = root.path().join("config");
    let app_dir = xdg.join("hofvarpnir");
    fs::create_dir_all(&app_dir).unwrap();
    std::os::unix::fs::symlink(&store_file, app_dir.join("tui.toml")).unwrap();
    fs::set_permissions(&app_dir, fs::Permissions::from_mode(0o555)).unwrap();

    let env = env_with(vec![
        ("XDG_CONFIG_HOME", xdg.display().to_string()),
        ("HOF_API_URL", String::new()),
    ]);
    let partial = PartialConfig::resolve(Vec::<String>::new(), &env).unwrap();
    fs::set_permissions(&app_dir, fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(
        partial.save_path, None,
        "would offer to overwrite a store file"
    );
    let config = partial.into_config().unwrap();
    assert_eq!(config.api_url, server.uri());
    connect(&config).await;
}

/// First run then second run: nothing on disk, the setup screen saves what the
/// user typed, and the next start authenticates from that file alone.
#[tokio::test]
async fn saved_setup_config_authenticates_next_start() {
    let server = server_expecting("hof_sk_typed_in_setup").await;
    let root = tempfile::tempdir().unwrap();
    let env = env_with(vec![("HOME", root.path().display().to_string())]);

    let first = PartialConfig::resolve(Vec::<String>::new(), &env).unwrap();
    assert_eq!(first.token, None, "first run must go to the setup screen");
    let save_path = first.save_path.unwrap();
    assert_eq!(
        save_path,
        root.path().join(".config/hofvarpnir/tui.toml"),
        "XDG fallback to ~/.config"
    );
    let typed = Config::new(&format!("  {}/ ", server.uri()), " hof_sk_typed_in_setup\n").unwrap();
    config::save(&save_path, &typed).unwrap();

    let second = PartialConfig::resolve(Vec::<String>::new(), &env).unwrap();
    assert_eq!(second.save_path, None);
    connect(&second.into_config().unwrap()).await;

    assert_private(&save_path);
}

fn assert_private(path: &Path) {
    let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "saved config holds a key; mode {mode:o}");
}
