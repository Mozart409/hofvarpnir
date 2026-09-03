//! Drain section of the runtime control panel.
//!
//! Draining is the operator-visible half of a graceful shutdown: a
//! `DrainToken::begin` call is first-write-wins and freezes the deadline at
//! drain start, so this section must show that frozen value rather than
//! recomputing anything.

use std::time::Duration;

use chrono::{DateTime, Utc};
use hof_core::runtime_config::sleep_duration_until;
use maud::Markup;

use super::{PanelView, panel_section};

/// Render the Drain section.
pub(crate) fn section(view: &PanelView) -> Markup {
    panel_section(
        "Drain",
        &maud::html! {
            @if view.is_draining() {
                (draining_view(view))
            } @else {
                (not_draining_view(view))
            }
        },
    )
}

/// Not-draining state: a confirm-gated shutdown control, plus a preview of
/// the live job counts a drain would have to wait for.
fn not_draining_view(view: &PanelView) -> Markup {
    maud::html! {
        p class="text-sm text-slate-600 dark:text-slate-400" {
            "Shutting down stops the server from accepting new work and exits "
            "once in-flight downloads and indexing finish, or the drain "
            "timeout (" (humanize(view.settings.drain_timeout.value)) ") "
            "elapses — whichever comes first."
        }
        (quiescence_counts(view))
        form
            method="post"
            action="/settings/runtime/shutdown"
            class="mt-4"
            onsubmit="return confirm('Shut down the server? It will stop accepting new work and exit once in-flight downloads and indexing finish, or the drain timeout elapses.')"
        {
            button
                type="submit"
                class="rounded-lg border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-900/40 px-4 py-2 text-sm font-medium text-red-700 dark:text-red-300 hover:bg-red-100 dark:hover:bg-red-900"
            {
                "Shut down"
            }
        }
    }
}

/// Draining state: no button — the deadline is already frozen — just the
/// frozen deadline, live progress toward it, and the same job counts.
///
/// This is the one destructive state in the panel, so it gets its own red
/// card rather than blending into the surrounding slate palette.
fn draining_view(view: &PanelView) -> Markup {
    maud::html! {
        div class="rounded-xl border-2 border-red-300 dark:border-red-700 bg-red-50 dark:bg-red-950/40 p-4" {
            p class="text-sm font-semibold text-red-800 dark:text-red-200" {
                "Draining — the server is shutting down. It will exit once "
                "in-flight work finishes, or the deadline below is reached, "
                "whichever comes first."
            }
            div class="mt-4 grid gap-4 sm:grid-cols-2" {
                @match view.drain_started_at {
                    Some(started_at) => {
                        (drain_stat("Drain started", &maud::html! {
                            (started_at.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                        }))
                        (drain_stat("Time drained so far", &countdown(
                            started_at,
                            &humanize(elapsed_since(view.now, started_at)),
                            true,
                        )))
                    }
                    None => { (drain_stat_unavailable("Drain started")) }
                }
                @match view.drain_deadline {
                    Some(deadline) => {
                        (drain_stat("Deadline", &maud::html! {
                            (deadline.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                        }))
                        (drain_stat("Time remaining", &countdown(
                            deadline,
                            &humanize(sleep_duration_until(deadline, view.now)),
                            false,
                        )))
                    }
                    None => { (drain_stat_unavailable("Deadline")) }
                }
            }
        }
        (quiescence_counts(view))
    }
}

/// One red-themed stat card for the draining state.
fn drain_stat(label: &str, value: &Markup) -> Markup {
    maud::html! {
        div {
            p class="text-xs font-medium uppercase tracking-wide text-red-700/80 dark:text-red-300/80" { (label) }
            p class="mt-1 text-sm font-semibold text-red-900 dark:text-red-100 tabular-nums" { (value) }
        }
    }
}

/// A drain stat whose source (`DrainToken`) failed to report a value.
///
/// In practice `drain_started_at` and `drain_deadline` are set together, so
/// this path is defensive rather than expected — but an `Option` in
/// `PanelView` still means "never fail the page" here too.
fn drain_stat_unavailable(label: &str) -> Markup {
    maud::html! {
        div {
            p class="text-xs font-medium uppercase tracking-wide text-red-700/80 dark:text-red-300/80" { (label) }
            p class="mt-1 text-sm italic text-red-700/70 dark:text-red-300/70" { "unavailable" }
        }
    }
}

/// Live counts a drain would wait for, reused by both states: a preview
/// before triggering, and live progress while draining.
fn quiescence_counts(view: &PanelView) -> Markup {
    maud::html! {
        div class="mt-4 grid gap-4 sm:grid-cols-3" {
            (count_stat("Active downloads", view.supervisor.as_ref().map(|s| s.active_downloads)))
            (count_stat("Dispatching", view.supervisor.as_ref().map(|s| s.dispatching)))
            (count_stat("Active indexers", view.scheduler.as_ref().map(|s| s.active_indexers)))
        }
    }
}

/// One job-count stat card. `None` means the actor ask failed; degrade to a
/// muted placeholder rather than failing the page.
fn count_stat(label: &str, value: Option<usize>) -> Markup {
    maud::html! {
        div class="rounded-lg border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 p-3" {
            p class="text-xs font-medium uppercase tracking-wide text-slate-500 dark:text-slate-400" { (label) }
            p class="mt-1 text-sm font-semibold text-slate-900 dark:text-slate-100 tabular-nums" {
                @match value {
                    Some(v) => { (v.to_string()) }
                    None => { span class="italic text-slate-400 dark:text-slate-500" { "unavailable" } }
                }
            }
        }
    }
}

/// The shared countdown markup contract: the timings agent's script drives
/// this element, keyed off `data-deadline` (the reference instant) and an
/// optional `data-direction="up"` for values that count up rather than down.
/// The server-rendered fallback text must stand on its own with JS disabled.
fn countdown(reference: DateTime<Utc>, fallback: &str, counts_up: bool) -> Markup {
    maud::html! {
        @if counts_up {
            span class="js-countdown" data-deadline=(reference.to_rfc3339()) data-direction="up" { (fallback) }
        } @else {
            span class="js-countdown" data-deadline=(reference.to_rfc3339()) { (fallback) }
        }
    }
}

/// Wall-clock time since `since`, saturating at zero rather than going
/// negative if `now` is somehow earlier (clock skew, or a race between
/// reading `now` and `drain_started_at`).
fn elapsed_since(now: DateTime<Utc>, since: DateTime<Utc>) -> Duration {
    now.signed_duration_since(since)
        .to_std()
        .unwrap_or(Duration::ZERO)
}

/// Render a duration the way an operator reads it: "45s", "1m 30s", "3h".
fn humanize(d: Duration) -> String {
    let total = d.as_secs();
    if total == 0 {
        return "0s".to_string();
    }
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;

    let mut parts: Vec<String> = Vec::new();
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

/// `POST /settings/runtime/shutdown` — begin draining, then redirect back to
/// the panel.
///
/// Reading `state.runtime_config.current()` here, at the moment of the
/// click, is correct: that IS the `drain_timeout` in force at drain start,
/// which `begin` freezes into the deadline it records — a later retune of
/// `drain_timeout_secs` cannot move it. The deadline is never computed by
/// hand here; `state.drain.deadline()` reads back the frozen value, which is
/// also what a repeat trigger reports (`begin` is first-write-wins).
pub(crate) async fn shutdown_submit(
    auth: crate::auth::AuthUser,
    axum::extract::State(state): axum::extract::State<hof_api::AppState>,
    session: tower_sessions::Session,
) -> axum::response::Redirect {
    let now = Utc::now();
    state
        .drain
        .begin(now, state.runtime_config.current().drain_timeout.value);
    let deadline = state.drain.deadline();

    // Process-ending and operator-visible: always leave a trace, even though
    // the HTTP response is just a redirect.
    tracing::info!(user_id = %auth.user_id, ?deadline, "Drain triggered from the runtime control panel");

    let message = deadline.map_or_else(
        || "Shutting down.".to_string(),
        |deadline| {
            format!(
                "Shutting down — draining until {} UTC.",
                deadline.format("%Y-%m-%d %H:%M:%S")
            )
        },
    );
    crate::pages::set_flash(&session, "info", &message).await;

    axum::response::Redirect::to("/settings/runtime")
}

#[cfg(test)]
mod tests {
    use hof_core::actors::download_supervisor::SupervisorStatus;
    use hof_core::actors::scheduler::SchedulerStatus;
    use hof_core::runtime_config::{DrainToken, Provenance, Resolved};

    use super::*;

    fn sample_settings() -> std::sync::Arc<hof_core::runtime_config::EffectiveSettings> {
        std::sync::Arc::new(hof_core::runtime_config::EffectiveSettings {
            indexing_paused_until: None,
            downloads_paused_until: None,
            max_concurrent_downloads: Resolved {
                value: 3,
                provenance: Provenance::Default,
            },
            max_indexers_per_tick: Resolved {
                value: 5,
                provenance: Provenance::Default,
            },
            rate_limit_delay: Resolved {
                value: Duration::from_secs(5),
                provenance: Provenance::Default,
            },
            check_interval: Resolved {
                value: Duration::from_mins(1),
                provenance: Provenance::Default,
            },
            cleanup_interval: Resolved {
                value: Duration::from_hours(3),
                provenance: Provenance::Default,
            },
            drain_timeout: Resolved {
                value: Duration::from_mins(30),
                provenance: Provenance::Default,
            },
        })
    }

    fn base_view(now: DateTime<Utc>) -> PanelView {
        PanelView {
            now,
            settings: sample_settings(),
            row: None,
            drain_started_at: None,
            drain_deadline: None,
            supervisor: Some(SupervisorStatus {
                active_downloads: 2,
                dispatching: 1,
                available_permits: 0,
                rate_limit_backoff: 0,
            }),
            scheduler: Some(SchedulerStatus {
                running: true,
                active_indexers: 4,
                check_interval_secs: 60,
            }),
            cleanup: None,
            download_timeout: Duration::from_hours(1),
            download_timeout_provenance: Provenance::Default,
            ytdlp_timeout: Duration::from_mins(5),
            min_index_interval: Duration::from_secs(30),
        }
    }

    #[test]
    fn not_draining_renders_a_shutdown_control() {
        let view = base_view(Utc::now());
        let html = section(&view).into_string();

        assert!(html.contains("Shut down"));
        assert!(html.contains("confirm("));
        assert!(html.contains("/settings/runtime/shutdown"));
        // Draining-only wording must not leak into the not-draining state.
        assert!(!html.contains("Draining —"));
    }

    #[test]
    fn draining_view_has_no_shutdown_button() {
        let now = Utc::now();
        let token = DrainToken::new();
        token.begin(now, Duration::from_mins(30));

        let mut view = base_view(now);
        view.drain_started_at = token.started_at();
        view.drain_deadline = token.deadline();

        let html = section(&view).into_string();

        assert!(html.contains("Draining —"));
        assert!(!html.contains("Shut down"));
        assert!(!html.contains("confirm("));
    }

    #[test]
    fn draining_view_shows_all_three_quiescence_counts() {
        let now = Utc::now();
        let token = DrainToken::new();
        token.begin(now, Duration::from_mins(30));

        let mut view = base_view(now);
        view.drain_started_at = token.started_at();
        view.drain_deadline = token.deadline();

        let html = section(&view).into_string();

        assert!(html.contains("Active downloads"));
        assert!(html.contains("Dispatching"));
        assert!(html.contains("Active indexers"));
        // Scope the count assertions to an element body (`>N<`). A bare
        // `contains("2")` would match any stray digit -- a Tailwind class
        // like `px-2`, or a digit inside the rendered timestamp -- and so
        // would pass even if the counts were never rendered at all.
        assert!(html.contains(">2<"), "active_downloads count not rendered");
        assert!(html.contains(">4<"), "active_indexers count not rendered");
    }

    #[test]
    fn missing_actor_status_degrades_to_placeholder() {
        let mut view = base_view(Utc::now());
        view.supervisor = None;
        view.scheduler = None;

        // Must not panic, and must say so rather than pretending zero.
        let html = section(&view).into_string();
        assert!(html.contains("unavailable"));
    }

    #[test]
    fn missing_drain_timestamps_degrade_to_placeholder_while_draining() {
        // is_draining() is driven by drain_started_at alone; force the
        // draining branch while leaving the deadline unset, to exercise the
        // defensive None arm without needing a second DrainToken trick.
        let mut view = base_view(Utc::now());
        view.drain_started_at = Some(view.now);
        view.drain_deadline = None;

        let html = section(&view).into_string();
        assert!(html.contains("unavailable"));
    }

    #[test]
    fn frozen_deadline_does_not_move_when_rendered_later() {
        let t0 = Utc::now();
        let token = DrainToken::new();
        token.begin(t0, Duration::from_mins(30));

        let mut view = base_view(t0);
        view.drain_started_at = token.started_at();
        view.drain_deadline = token.deadline();
        let first_render = section(&view).into_string();

        // A repeat trigger, and a later render, must not move the deadline:
        // `begin` is first-write-wins even with a different timeout.
        let t1 = t0 + chrono::Duration::hours(2);
        token.begin(t1, Duration::from_mins(5));
        view.now = t1;
        view.drain_started_at = token.started_at();
        view.drain_deadline = token.deadline();
        let second_render = section(&view).into_string();

        let deadline_text = token
            .deadline()
            .expect("token was begun")
            .format("%Y-%m-%d %H:%M:%S UTC")
            .to_string();
        assert!(first_render.contains(&deadline_text));
        assert!(second_render.contains(&deadline_text));
        assert_eq!(token.started_at(), Some(t0));
    }
}
