# OTel export: authenticate it, and stop needing a firewall hole

**Severity:** hardening — two plaintext ports on the telemetry host exist
solely because this app cannot send a header
**Status:** app side done (branch `feat/actor-tracing`); homelab side ready on pve-nixos-homelab branch `feat/hofvarpnir-otel-auth`, waiting on a hofvarpnir release
**Raised:** 2026-09-25
**Relates to:** `todos/otel.md` (phases 1–6, all complete); this is the
production-auth follow-up that plan deferred

## Where things stand

Export works. `hosts/jellyfin/hofvarpnir.nix` in the homelab repo runs:

```
OTEL_EXPORTER_OTLP_ENDPOINT = "http://otel.homelab.local:4317"   # plain gRPC
OTEL_EXPORTER_OTLP_PROTOCOL = "grpc"
LOKI_URL                    = "http://otel.homelab.local:3100"   # plain HTTP
```

Spans land in Tempo, logs land in Loki. Both go **in clear text to raw ports**.

On 2026-09-14 the otel host closed 9090/3100/3200/4317/4318 in its firewall and
put everything behind Caddy with two bearer tokens (push and query). Everything
in the homelab moved over — except this app, which got a carve-out:

```nix
# hosts/otel/configuration.nix
iptables -A nixos-fw -p tcp -s 192.168.2.180 --dport 4317 -j nixos-fw-accept
iptables -A nixos-fw -p tcp -s 192.168.2.180 --dport 3100 -j nixos-fw-accept
```

with the comment *"Drop this once hofvarpnir can send `Authorization` headers."*
That is this todo. Two app-side changes retire both rules.

## Change 1 — send `Authorization`

The receiving end is already built: Caddy on `otel.homelab.local` has

```
handle /v1/* { reverse_proxy @push localhost:4318; respond 401 }
```

where `@push` matches `Authorization: Bearer <otel-push-token>`. So the app
needs to attach one header, and the standard env var for it is
`OTEL_EXPORTER_OTLP_HEADERS` (`Authorization=Bearer%20…`, comma-separated,
percent-encoded values).

- Check whether the pipeline in the telemetry init already honors it. The Rust
  `opentelemetry-otlp` exporter reads it only on the env-configured builder
  path; if the exporter is constructed with explicit `.with_endpoint(...)`
  calls, the env headers are silently ignored. **Verify by observation, not by
  reading the docs** — a 401 in the collector's logs is the only honest test.
- If it is ignored, read the var and pass the headers explicitly. Also accept
  `OTEL_EXPORTER_OTLP_TRACES_HEADERS` (signal-specific override) if cheap.
- Do the same for the Loki layer: `tracing-loki` takes extra HTTP headers, and
  Loki's vhost accepts the same push token on `/loki/api/v1/push`.

## Change 2 — trust step-ca

Once the endpoint is `https://otel.homelab.local`, TLS is a step-ca leaf, and
the current build almost certainly rejects it. This is a known, repeated
homelab failure mode — reqwest built with rustls verifies against **bundled
webpki-roots** and ignores the system trust store, so `SSL_CERT_FILE` and the
CA bundle mounted into the container do nothing. `hofvarpnir.nix` already
carries a comment predicting exactly this for the Loki path, and the homelab
dashboard's health checks hit it for real.

Fix: build reqwest with `rustls-tls-native-roots` (or native-tls), so the
mounted `/etc/ssl/certs/ca-certificates.crt` — which contains the step-ca root —
is actually consulted. Applies to the OTLP HTTP exporter and to `tracing-loki`.

Watch for a transitive reqwest in the dependency tree with default features
enabling `rustls-tls-webpki-roots`; feature unification means one crate asking
for webpki roots is enough to change behaviour.

## Change 3 — prefer OTLP/HTTP over gRPC

Through Caddy, use `http/protobuf`:

```
OTEL_EXPORTER_OTLP_ENDPOINT = "https://otel.homelab.local"
OTEL_EXPORTER_OTLP_PROTOCOL = "http/protobuf"
OTEL_EXPORTER_OTLP_HEADERS  = "Authorization=Bearer%20<push-token>"
```

The SDK appends `/v1/traces` itself, which is what the `handle /v1/*` block
above expects. gRPC would need h2c upstream config on Caddy that does not exist
and would have to be maintained for one client. Make sure the app does not
hardcode a protocol that contradicts `OTEL_EXPORTER_OTLP_PROTOCOL`.

## Change 4 — metrics over OTLP (now that they have somewhere to go)

`METRICS_ENABLED=true` today only serves `/metrics`, and nothing scrapes it:
there is no `hofvarpnir` job in the otel host's Prometheus. Meanwhile the OTLP
metrics pipeline in the collector used to export to `nop` — every OTLP metric
point was dropped on the floor.

As of 2026-09-25 the homelab side is fixed: the collector exports OTLP metrics
to Prometheus's own OTLP receiver (`--web.enable-otlp-receiver`). So OTLP
metrics from this app will now be stored. Worth wiring up the
`opentelemetry_sdk` metrics pipeline alongside the trace one — it gets the
app's metrics in without adding a scrape job, and they arrive already
correlated with the traces by resource attributes.

Keep `/metrics` as-is either way; it is useful locally and costs nothing.

## Change 5 — honor the sampling env vars

Tempo stores blocks on a ZFS mirror of two spinning disks that is the homelab's
standing IO bottleneck; the block cadence there is already tuned down to one
block per 30 minutes because of it. 100% sampling is fine at today's volume and
will not stay fine.

Make sure `OTEL_TRACES_SAMPLER` / `OTEL_TRACES_SAMPLER_ARG` are respected
(`parentbased_traceidratio` + `0.1`) so the ratio can be turned down from the
NixOS config without a release. Again: verify, do not assume — an explicitly
constructed `TracerProvider` overrides the env-derived sampler.

## Acceptance

1. `OTEL_EXPORTER_OTLP_ENDPOINT` is an `https://` URL and spans still arrive in
   Tempo (check via Grafana, or `tempo_search` once that MCP server exists).
2. Logs still arrive in Loki over HTTPS with a bearer token.
3. The two `iptables` lines in `hosts/otel/configuration.nix` are deleted and
   nothing breaks. **That deletion is the actual deliverable** — this work is
   not done while they are still there.
4. A wrong token produces a visible 401 in the app's own logs, not silent data
   loss.

## Progress (2026-09-26)

Verified by observation in `crates/hof-core/tests/otel_export.rs`: an E2E test
that runs the real `init_tracing` in a child process against a stand-in for
Caddy (HTTPS with a private-CA leaf, 401 unless the bearer token matches).
Before the fix, all three scenarios failed as predicted below.

- **Change 1 — done.** The OTLP HTTP exporter already reads
  `OTEL_EXPORTER_OTLP_HEADERS` / `_TRACES_HEADERS` itself. The real gap was
  that no HTTP client feature was compiled in, so `http/protobuf` could not
  build an exporter at all ("no HTTP client is configured") and tracing
  silently turned off. Added `reqwest-blocking-client`. Loki: new
  `LOKI_HEADERS`, same format as the OTLP var.
- **Change 2 — done.** OTLP (reqwest 0.13) already verifies via the system
  store. Loki (reqwest 0.12, `rustls-tls`) trusted webpki roots only and
  failed the TLS handshake against the private CA. Fixed through feature
  unification (`rustls-tls-native-roots`); removing it re-breaks the test.
- **Change 3 — done.** `http/protobuf` works; the protocol comes from
  `OTEL_EXPORTER_OTLP_PROTOCOL`. Requests hit exactly `/v1/traces` and
  `/loki/api/v1/push`.
- **Change 4 — not needed.** The premise was stale: otel's Prometheus already
  has a `hofvarpnir` scrape job (`https://hofvarpnir.homelab.local/metrics`),
  and on 2026-09-26 it reported `up=1` with 170 series.
- **Change 5 — done, nothing to change.** The provider sets no sampler, so the
  SDK's env-derived default applies; ratio 0 exports no spans.
- **Acceptance 4 — done.** Wrong token →
  `ERROR opentelemetry_sdk: … BatchSpanProcessor.ExportError … 401` and
  `ERROR tracing_loki: couldn't send logs to loki … 401 Unauthorized`.

### Homelab rollout (acceptance 1–3, not in this repo)

Done on pve-nixos-homelab branch `feat/hofvarpnir-otel-auth`: the env below,
headers rendered at container start from the fleet `otel-push-token.age`
(no new secret), and both `iptables` lines deleted. **Deploy order:**
release hofvarpnir with this branch's fixes, bump the image tag in
`hosts/jellyfin/hofvarpnir.nix` on that homelab branch, apply jellyfin, confirm
spans in Tempo and logs in Loki, then apply otel (which closes the ports).
0.12.0 has no OTLP HTTP client and a Loki client that rejects step-ca, so
applying the homelab branch before the release switches its telemetry off.

Reference env:

`hosts/jellyfin/hofvarpnir.nix`, token via the host's secret mechanism:

```
OTEL_EXPORTER_OTLP_ENDPOINT = "https://otel.homelab.local"
OTEL_EXPORTER_OTLP_PROTOCOL = "http/protobuf"
OTEL_EXPORTER_OTLP_HEADERS  = "Authorization=Bearer%20<push-token>"
LOKI_URL                    = "https://otel.homelab.local"
LOKI_HEADERS                = "Authorization=Bearer%20<push-token>"
SSL_CERT_FILE               = "/etc/ssl/certs/ca-certificates.crt"  # must contain the step-ca root
OTEL_TRACES_SAMPLER         = "parentbased_traceidratio"            # optional
OTEL_TRACES_SAMPLER_ARG     = "0.1"
```

Then confirm spans in Tempo and logs in Loki, and only then delete the two
`iptables` lines in `hosts/otel/configuration.nix`.
