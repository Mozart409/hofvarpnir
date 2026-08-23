# Hofvarpnir Indexing Timeout Investigation
**Date:** 2026-08-23
**System:** Hofvarpnir (`hofvarpnir.homelab.internal`, deployed on the `jellyfin`
NixOS host — 4 vCPU / ~1.9GiB RAM)

---

## 1. Summary

Since **2026-08-21**, indexing has been failing across almost the entire
catalog (~100 of 112 sources) with `Failed to fetch playlist info: Timeout
after 300s while executing command: .../yt-dlp`. Upgrading hofvarpnir
0.5.1 → 0.6.1 (newer yt-dlp/crates) on 2026-08-23 did **not** fix it —
`/api/health/ready` stayed `200` throughout, so this is not the known
`DownloadSupervisor` panic wedge. Root-caused by re-running the exact failing
`yt-dlp` commands directly inside the deployed container: **the underlying
`yt-dlp`/YouTube path is healthy** (verified multiple times, including
back-to-back repeats). The real cause is **unbounded indexing concurrency in
`SchedulerActor`** colliding with YouTube's new mandatory per-request
JS-challenge solving on a host with very little CPU/RAM headroom.

---

## 2. Affected Scope

| Field | Value |
|---|---|
| **Sources affected** | ~100 of 112 (`GET /api/v1/activity/unhealthy-sources`) |
| **Error** | `Indexing had 1 error(s): Failed to index source: Failed to fetch playlist info: Timeout after 300s while executing command: /nix/store/.../yt-dlp` |
| **Onset (bulk)** | Burst of ~50 sources with `first_error_at` clustered in a **2-minute window: 2026-08-21T02:00:19Z – 2026-08-21T02:02:26Z** |
| **Onset (early stragglers)** | A handful of sources errored earlier and individually: 2026-07-21, 2026-08-13, 2026-08-14, 2026-08-17, 2026-08-18 |
| **Not affected** | `/api/health` and `/api/health/ready` both `200` throughout — the app itself, DB, and web UI are healthy |
| **System status at investigation time** | `active_downloads: 0`, `pending_downloads: 0`, `permanently_failed: 80`, `total_videos: 1365` |

---

## 3. Root Cause

### 3.1 `SchedulerActor` has no indexing concurrency cap

`crates/hof-core/src/actors/scheduler.rs` (~line 214) loops over every due
source and spawns an indexer for each with **no limit**:

```rust
for source in sources {
    // ...
    if let Err(e) = self.spawn_indexer(&source, ctx.actor_ref().clone()).await {
        // ...
    }
}
```

Compare `DownloadSupervisor`, which gates concurrency with a
`tokio::sync::Semaphore` (3 permits — see `GOALS.md`'s Actor Architecture
section). Indexing has no equivalent. When a batch of sources becomes due
around the same scheduler tick, every one of them spawns a `SourceIndexerActor`
→ a `yt-dlp` subprocess **concurrently and unbounded**.

### 3.2 Why this only recently became fatal

This gap was harmless while `yt-dlp` calls were cheap. It stopped being
harmless because YouTube's SABR/PO-token anti-bot rollout now makes **every**
`yt-dlp` call — including plain `--flat-playlist` indexing — spin up a `deno`
JS-challenge-solver subprocess:

```
[youtube] [jsc:deno] Solving JS challenges using deno
```

See [yt-dlp/yt-dlp#12482](https://github.com/yt-dlp/yt-dlp/issues/12482). On a
4-core/~2GB host (already running with ~445MiB free and some swap in use at
baseline), a batch of a few dozen concurrent `yt-dlp` + `deno` processes is
enough to starve CPU/RAM so badly that jobs which take 2-31s in isolation blow
straight through the fixed 300s command timeout
(`DEFAULT_TIMEOUT`, `patches/yt-dlp-patched/src/client/mod.rs:19`).

Once a due-batch fails together, it keeps re-colliding on every subsequent
scheduler tick, since none of them ever complete successfully — this is a
**self-sustaining outage**, not a transient blip, which is consistent with it
having persisted for 2+ days untouched.

### 3.3 Evidence that yt-dlp / YouTube itself is fine

Re-ran the *exact* command hofvarpnir uses
(`--no-progress --dump-single-json --flat-playlist <url>`) directly inside the
running container against real failing sources:

- Single video extraction (`--simulate` on a known video URL): succeeded in
  ~20s including a fresh JS-challenge solve.
- `@IWDominatelol/videos` (2135-entry channel, one of the failing sources):
  flat-playlist enumeration finished in **~28-31s**.
- `playlist?list=PL5JK9SjdCJp9qAtYUWuBhTjURx6OAFa5x` (198-entry Pietsmiet
  playlist, also failing in production): finished in **2-3s**.
- **5 back-to-back runs** alternating between the two sources above: all
  succeeded, 2-31s each, **no throttling or slowdown observed** — rules out a
  simple "YouTube blocked our IP" explanation.

This means the 300s production timeouts are a **resource-contention** problem
under concurrent load, not a `yt-dlp`, network, or YouTube-blocking problem —
which is also why bumping the `yt-dlp`/hofvarpnir version didn't help; a
version bump can't fix unbounded fan-out.

---

## 4. Impact

| Impact | Detail |
|---|---|
| **New uploads missed everywhere** | Almost the entire catalog (100/112 sources) has been unable to index since 2026-08-21, so no new videos are being discovered from any of them. |
| **Self-sustaining** | The same batch of sources collides on every scheduler tick and keeps failing — this does not resolve itself over time. |
| **Silent-ish** | `/api/health/ready` stays `200` (unlike the DownloadSupervisor wedge), so basic liveness/readiness monitoring does not catch this. `GET /api/v1/activity/unhealthy-sources` is the only place this is visible today. |
| **Version-bump was a dead end** | The 0.5.1 → 0.6.1 upgrade attempted on 2026-08-23 did not and could not fix this, since the root cause is unrelated to `yt-dlp` version. |

---

## 5. Recommended Fixes

### 5.1 Critical: Cap indexing concurrency

Add a `tokio::sync::Semaphore`-gated concurrency limit around
`spawn_indexer()` calls in `SchedulerActor`, mirroring
`DownloadSupervisor`'s existing pattern:

- [ ] Add the semaphore/cap in `crates/hof-core/src/actors/scheduler.rs`.
- [ ] Expose it as config (e.g. `MAX_CONCURRENT_INDEXERS` env var +
      `hof-core/src/config.rs`), default small (2-4) given how
      resource-constrained the deployment host is.
- [ ] Verify: trigger a reindex burst and confirm
      `GET /api/v1/system/status` → `scheduler.active_indexers` stays bounded
      at the cap instead of spiking with batch size.
- [ ] Verify: confirm `GET /api/v1/activity/unhealthy-sources` clears over
      subsequent ticks instead of the same sources re-failing every time.

### 5.2 High: Indexing timeout shouldn't be a flat, shared 300s

`DEFAULT_TIMEOUT` (300s) is shared across indexing and other short-lived
`yt-dlp` calls (e.g. `search_first`). Indexing runs out-of-band from
user-facing requests, so a longer/independently-configurable timeout has low
cost and adds headroom if YouTube's per-request overhead increases further.

- [ ] Give indexing its own configurable timeout, separate from
      `DEFAULT_TIMEOUT`.
- [ ] Consider scaling it with expected entry count if that's knowable
      up front.

### 5.3 Medium: yt-dlp cache directory isn't writable in the container

Every invocation logs:

```
WARNING: Writing cache to '/home/hofvarpnir/.cache/yt-dlp/youtube-sigfuncs/...' failed:
PermissionError: [Errno 13] Permission denied: '/home/hofvarpnir/.cache'
```

No volume is mounted at that path in the deployment
(`hosts/jellyfin/hofvarpnir.nix` in the infra repo), so the JS-challenge cache
never persists across restarts — every call re-solves challenges from
scratch, compounding the CPU cost that made 5.1 fatal in the first place.

- [ ] Fix container user permissions on the cache directory, or document that
      deployments should mount a persistent, writable volume there.

### 5.4 Low / unrelated: Loki log-push 500s

Deployed logs also show recurring, unrelated failures:

```
tracing_loki: couldn't send logs to loki error_count=1 backoff_time=0ns
error=HTTP status server error (500 Internal Server Error) for url
(http://otel.homelab.local:3100/loki/api/v1/push)
```

Server-side 500 from the Loki push endpoint — logs are silently dropped. Does
not affect downloads/indexing. Not investigated further; likely an infra-side
Loki config issue rather than something to change in hofvarpnir.

### 5.5 Monitoring gap

`/api/health/ready` didn't catch this outage. Consider exposing
`scheduler.active_indexers` and/or unhealthy-source count as a Prometheus
metric so an infra-side alert (blackbox probe pattern already used for the
DownloadSupervisor wedge) can catch the next occurrence of either failure mode
automatically instead of relying on someone checking
`/api/v1/activity/unhealthy-sources` by hand.

---

## 6. Files / References

- **Root cause:** `crates/hof-core/src/actors/scheduler.rs` (~line 214,
  unbounded `spawn_indexer()` loop)
- **Timeout constant:** `patches/yt-dlp-patched/src/client/mod.rs:19`
  (`DEFAULT_TIMEOUT`, vendored `[patch.crates-io]` fork of the `yt-dlp` crate)
- **Cache permission warning:** yt-dlp stderr,
  `/home/hofvarpnir/.cache/yt-dlp/youtube-sigfuncs/`
- **API:** `GET /api/v1/system/status`,
  `GET /api/v1/activity/unhealthy-sources`
- **yt-dlp JS challenge / SABR background:**
  https://github.com/yt-dlp/yt-dlp/issues/12482
- Related prior incident: `todos/hofvarpnir-error-report-2026-06-19.md`
  (different root cause — age-restricted videos aborting a playlist scan —
  but same symptom class: silent indexing failure not caught by liveness
  checks).
