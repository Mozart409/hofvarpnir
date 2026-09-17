//! Actors section of the runtime control panel.
//!
//! The panel half of the supervision fix. A supervised actor can die on a
//! transient fault; before this section existed, the only signal was
//! `/api/health` reporting `degraded` to nobody in particular — a download
//! supervisor stayed dead for 27 hours while nine videos sat in `pending`.
//!
//! Two deliberate restraints on what this renders:
//!
//! - A `[Restart]` button appears **only** for a dead actor. A healthy system
//!   shows four quiet rows, so a button anywhere in this panel means something
//!   needs doing.
//! - An actor that has exhausted its restart budget gets a sentence instead of
//!   a button. The root supervisor would refuse the restart, so a button there
//!   would look like a fix and do nothing — worse than no control at all.

use chrono::{DateTime, Utc};
use hof_core::actors::root_supervisor::{ActorHealthReport, SupervisedActor};
use hof_core::runtime_config::sleep_duration_until;
use maud::Markup;

use super::{PanelView, humanize, panel_section};

/// Render the Actors section.
pub(crate) fn section(view: &PanelView) -> Markup {
    panel_section(
        "Actors",
        &maud::html! {
            @if view.actor_health.is_empty() {
                (health_unavailable())
            } @else {
                div class="space-y-3" {
                    @for report in &view.actor_health {
                        (actor_row(view, report))
                    }
                }
            }
        },
    )
}

/// The root supervisor itself did not answer.
///
/// Not a muted "unavailable" placeholder like the other sections use: the
/// root supervisor is the one actor nothing else can restart, so its silence
/// is the most serious thing this panel can report.
fn health_unavailable() -> Markup {
    maud::html! {
        div class="rounded-xl border-2 border-red-300 dark:border-red-700 bg-red-50 dark:bg-red-950/40 p-4" {
            p class="text-sm font-semibold text-red-800 dark:text-red-200" {
                "Actor health unavailable — the root supervisor did not respond."
            }
            p class="mt-1 text-sm text-red-700 dark:text-red-300" {
                "Nothing in-process supervises the root supervisor, so restart "
                "the server to recover."
            }
        }
    }
}

/// One actor: identity, liveness, restart history, and whatever recovery is
/// actually available.
fn actor_row(view: &PanelView, report: &ActorHealthReport) -> Markup {
    // A dead actor gets the same emphatic red card the drain section uses for
    // its one destructive state — this panel's rows are otherwise uniform
    // slate, so red reads as "here".
    let card_classes = if report.alive {
        "rounded-lg border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 p-4"
    } else {
        "rounded-xl border-2 border-red-300 dark:border-red-700 bg-red-50 dark:bg-red-950/40 p-4"
    };

    maud::html! {
        div class=(card_classes) {
            div class="flex flex-wrap items-center justify-between gap-2" {
                p class="text-sm font-semibold text-slate-900 dark:text-slate-100" {
                    (label(report.actor))
                }
                (liveness_pill(report.alive))
            }
            (restart_history(report, view.now))
            @if let Some(failure) = report.last_failure.as_ref() {
                p class="mt-2 text-sm text-slate-700 dark:text-slate-300" {
                    span class="font-medium" { "Last failure: " }
                    span class="font-mono text-xs break-words" { (failure) }
                }
            }
            // Backoff state belongs to the download supervisor alone, and is
            // read from its own status rather than from the health report:
            // it is a self-healing pause, not a supervision event.
            @if matches!(report.actor, SupervisedActor::DownloadSupervisor) {
                (db_backoff(view))
            }
            (recovery_control(report))
        }
    }
}

/// Liveness pill. Mirrors [`super::badge`]: the state is spelled out in text,
/// never encoded in colour alone.
fn liveness_pill(alive: bool) -> Markup {
    let (text, classes) = if alive {
        (
            "alive",
            "bg-emerald-100 text-emerald-800 dark:bg-emerald-900/40 dark:text-emerald-200",
        )
    } else {
        (
            "not running",
            "bg-red-100 text-red-800 dark:bg-red-900/40 dark:text-red-200",
        )
    };
    maud::html! {
        span class={ "inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium " (classes) } {
            (text)
        }
    }
}

/// Restart history, in the one line an operator actually reads: how many
/// times, and how long ago.
fn restart_history(report: &ActorHealthReport, now: DateTime<Utc>) -> Markup {
    maud::html! {
        p class="mt-1 text-sm text-slate-600 dark:text-slate-400 tabular-nums" {
            @if report.restart_count == 0 {
                "No restarts since startup."
            } @else {
                (report.restart_count)
                @if report.restart_count == 1 { " restart" } @else { " restarts" }
                " since startup."
                @if let Some(at) = report.last_restart_at {
                    " Last at " (at.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                    " (" (humanize(elapsed_since(now, at))) " ago)."
                }
            }
        }
    }
}

/// The download supervisor's database backoff, shown only while it is
/// actually in force.
///
/// A lapsed `db_backoff_until` is not worth screen space — the supervisor has
/// already resumed — and a countdown to an instant in the past would render
/// as a permanent "0s".
fn db_backoff(view: &PanelView) -> Markup {
    let Some(status) = view.supervisor.as_ref() else {
        return maud::html! {};
    };
    let Some(until) = status.db_backoff_until else {
        return maud::html! {};
    };
    if until <= view.now {
        return maud::html! {};
    }

    maud::html! {
        div class="mt-3 rounded-lg border border-amber-300 dark:border-amber-700 bg-amber-50 dark:bg-amber-950/40 p-3" {
            p class="text-sm font-semibold text-amber-900 dark:text-amber-100" {
                "Database backoff — no new downloads are being dispatched."
            }
            p class="mt-1 text-sm text-amber-800 dark:text-amber-200 tabular-nums" {
                (status.consecutive_db_failures)
                @if status.consecutive_db_failures == 1 {
                    " consecutive database failure. "
                } @else {
                    " consecutive database failures. "
                }
                "Retrying in "
                (countdown(until, &humanize(sleep_duration_until(until, view.now))))
                "."
            }
            @if let Some(error) = status.last_db_error.as_ref() {
                p class="mt-1 text-sm text-amber-800 dark:text-amber-200" {
                    span class="font-medium" { "Last error: " }
                    span class="font-mono text-xs break-words" { (error) }
                }
            }
        }
    }
}

/// What can actually be done about a dead actor, which is one of three things.
fn recovery_control(report: &ActorHealthReport) -> Markup {
    maud::html! {
        // A live actor gets no control: a "restart" button on a healthy actor
        // is an invitation to cause the outage it claims to fix.
        @if !report.alive {
            @if report.unrecoverable {
                p class="mt-3 text-sm font-medium text-red-800 dark:text-red-200" {
                    "Restart limit exhausted — process restart required."
                }
            } @else {
                (restart_form(report.actor))
            }
        }
    }
}

/// The restart control for a dead-but-recoverable actor.
///
/// No `confirm()` gate, unlike the shutdown control: the actor is already
/// dead, so there is nothing to lose, and an incident is the wrong moment to
/// put a modal between an operator and the recovery button.
fn restart_form(actor: SupervisedActor) -> Markup {
    maud::html! {
        form
            method="post"
            action=(format!("/settings/runtime/restart/{}", actor.as_str()))
            class="mt-3"
        {
            button
                type="submit"
                class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-sm font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900"
            {
                "Restart"
            }
        }
    }
}

/// Operator-facing name for an actor. `as_str()` is the URL spelling, which
/// is not what a heading should read like.
const fn label(actor: SupervisedActor) -> &'static str {
    match actor {
        SupervisedActor::DownloadSupervisor => "Download supervisor",
        SupervisedActor::Scheduler => "Scheduler",
        SupervisedActor::Cleanup => "Cleanup",
        SupervisedActor::JellyfinMetadata => "Jellyfin metadata",
    }
}

/// The shared countdown markup contract (see `drain.rs` and `timings.rs`):
/// `assets/runtime-countdown.js` ticks every `.js-countdown` span from its
/// RFC3339 `data-deadline`. The server-rendered fallback must stand on its
/// own with JS disabled.
fn countdown(deadline: DateTime<Utc>, fallback: &str) -> Markup {
    maud::html! {
        span class="js-countdown font-medium tabular-nums" data-deadline=(deadline.to_rfc3339()) { (fallback) }
    }
}

/// Wall-clock time since `since`, saturating at zero rather than going
/// negative under clock skew (same guard as `drain::elapsed_since`).
fn elapsed_since(now: DateTime<Utc>, since: DateTime<Utc>) -> std::time::Duration {
    now.signed_duration_since(since)
        .to_std()
        .unwrap_or(std::time::Duration::ZERO)
}

/// `POST /settings/runtime/restart/{name}` — ask the root supervisor to
/// restart one actor, then redirect back to the panel.
///
/// Every outcome becomes a flash message rather than an error page: the
/// operator is mid-incident on the panel, and the panel itself is the answer
/// to "did that work?" once it re-renders with the actor alive again.
pub(crate) async fn restart_submit(
    auth: crate::auth::AuthUser,
    axum::extract::State(state): axum::extract::State<hof_api::AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    session: tower_sessions::Session,
) -> axum::response::Redirect {
    let Some(which) = hof_api::routes::system::parse_supervised_actor(&name) else {
        // Only reachable by a hand-typed URL — the panel's forms are built
        // from `as_str()`, so they cannot produce a name that fails here.
        tracing::warn!(user_id = %auth.user_id, %name, "Restart requested for an unknown actor");
        crate::pages::set_flash(&session, "error", &format!("Unknown actor '{name}'.")).await;
        return axum::response::Redirect::to("/settings/runtime");
    };

    // Operator-visible and state-changing, so it always leaves a trace even
    // though the HTTP response is just a redirect (same reason as
    // `drain::shutdown_submit`).
    tracing::info!(
        user_id = %auth.user_id,
        actor = which.as_str(),
        "Actor restart triggered from the runtime control panel"
    );

    let (level, message) = match state
        .root_supervisor
        .ask(hof_core::actors::root_supervisor::RestartActor { which })
        .await
    {
        Ok(Ok(())) => ("info", format!("Restarted {}.", label(which))),
        // The supervisor answered "no" — in practice the restart budget is
        // spent. Surface its reason verbatim rather than a generic failure:
        // it is the difference between "try again" and "restart the server".
        Ok(Err(reason)) => (
            "error",
            format!("Could not restart {}: {reason}", label(which)),
        ),
        Err(error) => {
            tracing::error!(actor = which.as_str(), %error, "Failed to reach the root supervisor");
            (
                "error",
                format!(
                    "Could not reach the root supervisor to restart {}. A server restart is required.",
                    label(which)
                ),
            )
        }
    };

    crate::pages::set_flash(&session, level, &message).await;
    axum::response::Redirect::to("/settings/runtime")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use hof_core::actors::download_supervisor::SupervisorStatus;
    use hof_core::runtime_config::{EffectiveSettings, Provenance, Resolved};

    use super::*;

    fn sample_settings() -> Arc<EffectiveSettings> {
        Arc::new(EffectiveSettings {
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

    fn healthy(actor: SupervisedActor) -> ActorHealthReport {
        ActorHealthReport {
            actor,
            alive: true,
            restart_count: 0,
            last_restart_at: None,
            last_failure: None,
            unrecoverable: false,
        }
    }

    fn all_healthy() -> Vec<ActorHealthReport> {
        vec![
            healthy(SupervisedActor::DownloadSupervisor),
            healthy(SupervisedActor::Scheduler),
            healthy(SupervisedActor::Cleanup),
            healthy(SupervisedActor::JellyfinMetadata),
        ]
    }

    fn base_view(now: DateTime<Utc>, actor_health: Vec<ActorHealthReport>) -> PanelView {
        PanelView {
            now,
            settings: sample_settings(),
            row: None,
            drain_started_at: None,
            drain_deadline: None,
            supervisor: None,
            scheduler: None,
            cleanup: None,
            actor_health,
            download_timeout: Duration::from_hours(1),
            download_timeout_provenance: Provenance::Default,
            ytdlp_timeout: Duration::from_mins(5),
            min_index_interval: Duration::from_secs(30),
        }
    }

    fn supervisor_status(
        db_backoff_until: Option<DateTime<Utc>>,
        consecutive_db_failures: u32,
        last_db_error: Option<String>,
    ) -> SupervisorStatus {
        SupervisorStatus {
            active_downloads: 0,
            dispatching: 0,
            available_permits: 3,
            rate_limit_backoff: 0,
            db_backoff_until,
            consecutive_db_failures,
            last_db_error,
        }
    }

    /// The panel stays quiet when nothing is wrong: four rows, no controls.
    #[test]
    fn all_actors_alive_renders_no_restart_button() {
        let html = section(&base_view(Utc::now(), all_healthy())).into_string();

        assert!(!html.contains("/settings/runtime/restart/"));
        assert!(!html.contains(">Restart<"));
        // All four are still listed, alive.
        assert!(html.contains("Download supervisor"));
        assert!(html.contains("Scheduler"));
        assert!(html.contains("Cleanup"));
        assert!(html.contains("Jellyfin metadata"));
        assert!(html.contains("alive"));
        assert!(!html.contains("not running"));
    }

    /// The whole point of the section: a dead actor is recoverable from here.
    #[test]
    fn dead_actor_renders_a_restart_button_for_that_actor_only() {
        let mut reports = all_healthy();
        reports[0].alive = false;
        reports[0].last_failure = Some("pool timed out while connecting".to_string());

        let html = section(&base_view(Utc::now(), reports)).into_string();

        assert!(html.contains("/settings/runtime/restart/download_supervisor"));
        assert!(html.contains(">Restart<"));
        assert!(html.contains("not running"));
        // The failure reason is what turns "it is dead" into an actionable
        // report, so it must reach the page.
        assert!(html.contains("pool timed out while connecting"));
        // Exactly one button: the three live actors must not grow one.
        assert_eq!(html.matches("/settings/runtime/restart/").count(), 1);
    }

    /// A button the supervisor would refuse is worse than no button.
    #[test]
    fn unrecoverable_actor_renders_the_sentence_instead_of_a_button() {
        let mut reports = all_healthy();
        reports[1].alive = false;
        reports[1].unrecoverable = true;
        reports[1].restart_count = 5;

        let html = section(&base_view(Utc::now(), reports)).into_string();

        assert!(html.contains("Restart limit exhausted — process restart required."));
        assert!(!html.contains("/settings/runtime/restart/"));
        assert!(!html.contains(">Restart<"));
        assert!(html.contains("5 restarts since startup."));
    }

    #[test]
    fn missing_actor_health_says_the_root_supervisor_is_silent() {
        let html = section(&base_view(Utc::now(), Vec::new())).into_string();

        assert!(html.contains("the root supervisor did not respond"));
        // No rows, so no controls either.
        assert!(!html.contains("/settings/runtime/restart/"));
    }

    #[test]
    fn active_db_backoff_shows_failures_error_and_a_countdown() {
        let now = Utc::now();
        let until = now + chrono::Duration::seconds(90);
        let mut view = base_view(now, all_healthy());
        view.supervisor = Some(supervisor_status(
            Some(until),
            3,
            Some("connection refused".to_string()),
        ));

        let html = section(&view).into_string();

        assert!(html.contains("Database backoff"));
        assert!(html.contains("3 consecutive database failures."));
        assert!(html.contains("connection refused"));
        assert!(html.contains("js-countdown"));
        assert!(html.contains("1m 30s"));
    }

    /// A lapsed backoff is history, not state: the supervisor has already
    /// resumed, and a countdown to the past renders as a stuck "0s".
    #[test]
    fn lapsed_db_backoff_is_not_shown() {
        let now = Utc::now();
        let mut view = base_view(now, all_healthy());
        view.supervisor = Some(supervisor_status(
            Some(now - chrono::Duration::seconds(30)),
            3,
            Some("connection refused".to_string()),
        ));

        let html = section(&view).into_string();

        assert!(!html.contains("Database backoff"));
        assert!(!html.contains("connection refused"));
    }

    #[test]
    fn no_backoff_state_renders_no_backoff_block() {
        let mut view = base_view(Utc::now(), all_healthy());
        view.supervisor = Some(supervisor_status(None, 0, None));

        let html = section(&view).into_string();

        assert!(!html.contains("Database backoff"));
    }

    /// The form action must be the API's accepted spelling; a mismatch would
    /// 400 on every click.
    #[test]
    fn restart_form_actions_use_the_documented_url_names() {
        for actor in hof_api::routes::system::SUPERVISED_ACTORS {
            let name = actor.as_str();
            let html = restart_form(actor).into_string();
            assert!(
                html.contains(&format!("/settings/runtime/restart/{name}")),
                "form action for {name} is not the documented spelling"
            );
            assert!(hof_api::routes::system::parse_supervised_actor(name).is_some());
        }
    }

    /// Liveness must be readable without colour (same rule as the provenance
    /// badge in `mod.rs`).
    #[test]
    fn liveness_is_spelled_out_not_only_coloured() {
        assert!(liveness_pill(true).into_string().contains("alive"));
        assert!(liveness_pill(false).into_string().contains("not running"));
    }
}
