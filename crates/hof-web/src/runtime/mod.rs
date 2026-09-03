//! Runtime control panel: pause, drain, effective settings, and timings.
//!
//! Lives in its own module rather than in `pages.rs` (6.6k lines) so the four
//! panel sections can be developed and reviewed independently. Each section
//! renders from one shared [`PanelView`] that the page handler assembles once,
//! so no section re-queries the database or re-asks an actor.

pub(crate) mod drain;
pub(crate) mod pause;
pub(crate) mod settings_table;
pub(crate) mod timings;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use chrono::{DateTime, Utc};
use hof_api::AppState;
use hof_core::actors::cleanup::{CleanupStatus, GetCleanupStatus};
use hof_core::actors::download_supervisor::{GetSupervisorStatus, SupervisorStatus};
use hof_core::actors::scheduler::{GetSchedulerStatus, MIN_INDEX_INTERVAL_SECS, SchedulerStatus};
use hof_core::db::{self, RuntimeSettingsRow};
use hof_core::runtime_config::{EffectiveSettings, Provenance, YTDLP_COMMAND_TIMEOUT};
use maud::Markup;
use tower_sessions::Session;

use crate::auth::AuthUser;
use crate::pages::{NavItem, layout_with_flash, take_flash};

/// Routes owned by the control panel. Merged into `pages::router` before the
/// shared `.with_state(..)`, so this returns a stateful-but-unapplied router.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings/runtime", get(runtime_page))
        .route("/settings/runtime/pause", post(pause::pause_submit))
        .route("/settings/runtime/resume", post(pause::resume_submit))
        .route("/settings/runtime/shutdown", post(drain::shutdown_submit))
}

/// Everything the four panel sections render from, gathered once per request.
///
/// Actor asks can fail (a supervisor may be restarting); those arrive as
/// `None` and each section degrades to a placeholder rather than failing the
/// whole page. Settings, by contrast, always resolve — they come from the
/// in-process watch channel, not from a fallible ask.
pub(crate) struct PanelView {
    /// Single timestamp for the whole render, so every countdown on the page
    /// is computed against the same instant.
    pub(crate) now: DateTime<Utc>,
    pub(crate) settings: Arc<EffectiveSettings>,
    /// The raw settings row, for the `updated_at` / `updated_by` audit stamp.
    /// `None` if the read failed.
    pub(crate) row: Option<RuntimeSettingsRow>,
    pub(crate) drain_started_at: Option<DateTime<Utc>>,
    pub(crate) drain_deadline: Option<DateTime<Utc>>,
    pub(crate) supervisor: Option<SupervisorStatus>,
    pub(crate) scheduler: Option<SchedulerStatus>,
    pub(crate) cleanup: Option<CleanupStatus>,
    /// Read-only timings (design 7.1): compiled-in or env-derived, displayed
    /// with a `default`/`env` badge but not runtime-mutable.
    pub(crate) download_timeout: Duration,
    /// Which layer supplied `download_timeout`. `DOWNLOAD_TIMEOUT_HOURS`
    /// falls back to a compiled-in default when unset, so this must be
    /// derived rather than assumed — ADR-0002 makes the badge load-bearing,
    /// and a badge that says "env" on a stock deployment is worse than none.
    pub(crate) download_timeout_provenance: Provenance,
    pub(crate) ytdlp_timeout: Duration,
    pub(crate) min_index_interval: Duration,
}

impl PanelView {
    pub(crate) const fn is_draining(&self) -> bool {
        self.drain_started_at.is_some()
    }
}

async fn runtime_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;
    let now = Utc::now();
    let settings = state.runtime_config.current();

    let (row_result, supervisor, scheduler, cleanup) = tokio::join!(
        db::get_runtime_settings(&state.pool),
        state.supervisor.ask(GetSupervisorStatus),
        state.scheduler.ask(GetSchedulerStatus),
        state.cleanup.ask(GetCleanupStatus),
    );

    let row = match row_result {
        Ok(row) => Some(row),
        Err(error) => {
            // Non-fatal: only the audit stamp is lost. Every knob value comes
            // from the watch channel above, so the panel still renders truthfully.
            tracing::error!(%error, "failed to read runtime_settings for the control panel");
            None
        }
    };

    let view = PanelView {
        now,
        settings,
        row,
        drain_started_at: state.drain.started_at(),
        drain_deadline: state.drain.deadline(),
        supervisor: supervisor.ok(),
        scheduler: scheduler.ok(),
        cleanup: cleanup.ok(),
        download_timeout: state.download_timeout,
        // Presence is not enough: `DownloadConfig::from_env` PARSES this var
        // and silently falls back to the compiled-in default on any non-`u64`
        // input, so `DOWNLOAD_TIMEOUT_HOURS=abc` yields the default value.
        // Checking only `is_ok()` would badge that default as "env" -- exactly
        // the failure this field's doc comment warns against. Mirror the
        // parse-then-check shape `EnvOverrides::from_env` uses for every other
        // tunable.
        download_timeout_provenance: if std::env::var("DOWNLOAD_TIMEOUT_HOURS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .is_some()
        {
            Provenance::Env
        } else {
            Provenance::Default
        },
        ytdlp_timeout: YTDLP_COMMAND_TIMEOUT,
        min_index_interval: Duration::from_secs(MIN_INDEX_INTERVAL_SECS),
    };

    layout_with_flash(
        "Runtime",
        NavItem::Runtime,
        flash,
        maud::html! {
            (pause::section(&view))
            (drain::section(&view))
            (settings_table::section(&view))
            (timings::section(&view))
            script src="/assets/runtime-countdown.js" defer {}
        },
    )
    .into_response()
}

/// Provenance badge.
///
/// Colour alone would fail for colour-blind readers and in monochrome, so the
/// layer name is always spelled out in the badge text as well.
pub(crate) fn badge(provenance: Provenance) -> Markup {
    let (text, classes) = match provenance {
        Provenance::Default => (
            "default",
            "bg-slate-100 text-slate-700 dark:bg-slate-700 dark:text-slate-200",
        ),
        Provenance::Env => (
            "env",
            "bg-amber-100 text-amber-800 dark:bg-amber-900/40 dark:text-amber-200",
        ),
        Provenance::Database => (
            "database",
            "bg-emerald-100 text-emerald-800 dark:bg-emerald-900/40 dark:text-emerald-200",
        ),
    };
    maud::html! {
        span class={ "inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium " (classes) } {
            (text)
        }
    }
}

/// Render a duration the way an operator reads it: "45s", "1m 30s", "3h".
pub(crate) fn humanize(d: Duration) -> String {
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

/// Shared shell so the four sections look like one panel.
pub(crate) fn panel_section(title: &str, body: &Markup) -> Markup {
    maud::html! {
        section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { (title) }
            div class="mt-4" { (body) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_covers_each_magnitude() {
        assert_eq!(humanize(Duration::from_secs(0)), "0s");
        assert_eq!(humanize(Duration::from_secs(45)), "45s");
        assert_eq!(humanize(Duration::from_secs(90)), "1m 30s");
        assert_eq!(humanize(Duration::from_mins(5)), "5m");
        // Exact-hour boundary: no stray "0m 0s" tail.
        assert_eq!(humanize(Duration::from_hours(3)), "3h");
        assert_eq!(humanize(Duration::from_secs(3661)), "1h 1m 1s");
    }

    /// ADR-0002 makes the badge load-bearing, so the three layers must stay
    /// distinguishable by TEXT, not only by colour -- colour-only encoding
    /// fails for colour-blind readers and in monochrome.
    #[test]
    fn every_provenance_renders_its_own_text() {
        let default = badge(Provenance::Default).into_string();
        let env = badge(Provenance::Env).into_string();
        let db = badge(Provenance::Database).into_string();

        assert!(default.contains("default"));
        assert!(env.contains("env"));
        assert!(db.contains("database"));
        assert_ne!(default, env);
        assert_ne!(env, db);
        assert_ne!(default, db);
    }
}
