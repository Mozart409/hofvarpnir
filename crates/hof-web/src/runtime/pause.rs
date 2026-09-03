//! Pause section of the runtime control panel.
//!
//! Indexing and downloads pause independently (two columns on the singleton
//! settings row), so this section renders one card per module rather than a
//! single global toggle.

use axum::Form;
use axum::extract::State;
use axum::response::Redirect;
use chrono::{DateTime, Utc};
use hof_api::AppState;
use hof_core::db::{self, RuntimeSettingsPatch};
use hof_core::runtime_config::{indefinite_pause, sleep_duration_until};
use maud::Markup;
use serde::Deserialize;
use tower_sessions::Session;

use super::{PanelView, panel_section};
use crate::auth::AuthUser;
use crate::pages::set_flash;

/// Which module a pause/resume request targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Module {
    Indexing,
    Downloads,
    All,
}

impl Module {
    /// The value used in the `module` form field.
    ///
    /// Also rendered back into the hidden inputs of this section's own forms.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Indexing => "indexing",
            Self::Downloads => "downloads",
            Self::All => "all",
        }
    }

    /// A human-readable label for flash messages.
    const fn label(self) -> &'static str {
        match self {
            Self::Indexing => "Indexing",
            Self::Downloads => "Downloads",
            Self::All => "Indexing and downloads",
        }
    }

    /// Set `until` on exactly the column(s) this module covers.
    ///
    /// Every other field of `patch` is left at its outer `None`, so the
    /// caller's `updated_by` (and nothing else) is the only other column
    /// touched.
    const fn apply_pause(self, patch: &mut RuntimeSettingsPatch, until: DateTime<Utc>) {
        match self {
            Self::Indexing => patch.indexing_paused_until = Some(Some(until)),
            Self::Downloads => patch.downloads_paused_until = Some(Some(until)),
            Self::All => {
                patch.indexing_paused_until = Some(Some(until));
                patch.downloads_paused_until = Some(Some(until));
            }
        }
    }

    /// Null out exactly the column(s) this module covers.
    ///
    /// `Some(None)` is a genuine SQL `NULL`, which lets the resolver fall
    /// back to the env/default layer for that column.
    const fn apply_resume(self, patch: &mut RuntimeSettingsPatch) {
        match self {
            Self::Indexing => patch.indexing_paused_until = Some(None),
            Self::Downloads => patch.downloads_paused_until = Some(None),
            Self::All => {
                patch.indexing_paused_until = Some(None);
                patch.downloads_paused_until = Some(None);
            }
        }
    }
}

/// The fixed set of durations the picker offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PauseDuration {
    #[serde(rename = "1h")]
    OneHour,
    #[serde(rename = "6h")]
    SixHours,
    #[serde(rename = "12h")]
    TwelveHours,
    #[serde(rename = "24h")]
    OneDay,
    #[serde(rename = "3d")]
    ThreeDays,
    #[serde(rename = "7d")]
    SevenDays,
    Indefinite,
}

impl PauseDuration {
    /// The offset from now this duration represents.
    ///
    /// `None` for `Indefinite`, which is a sentinel timestamp rather than an
    /// offset — see [`indefinite_pause`] — and (in principle, though
    /// unreachable for these fixed small constants) for a `chrono::Duration`
    /// that would overflow.
    const fn delta(self) -> Option<chrono::Duration> {
        match self {
            Self::OneHour => chrono::Duration::try_hours(1),
            Self::SixHours => chrono::Duration::try_hours(6),
            Self::TwelveHours => chrono::Duration::try_hours(12),
            Self::OneDay => chrono::Duration::try_days(1),
            Self::ThreeDays => chrono::Duration::try_days(3),
            Self::SevenDays => chrono::Duration::try_days(7),
            Self::Indefinite => None,
        }
    }

    /// Compute the absolute expiry for this duration from `now`.
    ///
    /// `None` only if the arithmetic genuinely overflows `DateTime`'s
    /// representable range; callers must flash that as an error rather than
    /// falling back to a default.
    fn expiry(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if matches!(self, Self::Indefinite) {
            return Some(indefinite_pause());
        }
        let delta = self.delta()?;
        now.checked_add_signed(delta)
    }
}

/// Render the Pause section.
///
/// Indexing and downloads are gated independently, so each gets its own card
/// rather than a single global toggle.
pub(crate) fn section(view: &PanelView) -> Markup {
    panel_section(
        "Pause",
        &maud::html! {
            p class="text-sm text-slate-600 dark:text-slate-400" {
                "Indexing and downloads pause independently. Pausing blocks new work; anything already running is unaffected."
            }
            div class="mt-4 grid gap-4 sm:grid-cols-2" {
                (module_card(
                    Module::Indexing,
                    "Indexing",
                    view.settings.indexing_paused_until,
                    view.settings.indexing_paused(view.now),
                    view.now,
                ))
                (module_card(
                    Module::Downloads,
                    "Downloads",
                    view.settings.downloads_paused_until,
                    view.settings.downloads_paused(view.now),
                    view.now,
                ))
            }
        },
    )
}

/// One module's current pause state plus its control.
fn module_card(
    module: Module,
    label: &str,
    until: Option<DateTime<Utc>>,
    is_paused: bool,
    now: DateTime<Utc>,
) -> Markup {
    maud::html! {
        div class="rounded-lg border border-slate-200 dark:border-slate-700 bg-white dark:bg-slate-900/40 p-4" {
            h3 class="text-sm font-semibold text-slate-900 dark:text-slate-100" { (label) }
            @if is_paused {
                @if let Some(deadline) = until {
                    @if deadline == indefinite_pause() {
                        // ADR-0003: never render the sentinel timestamp itself.
                        p class="mt-2 text-sm text-amber-700 dark:text-amber-300" {
                            "Paused indefinitely."
                        }
                    } @else {
                        // The absolute timestamp is the "correct on its own" part of
                        // the countdown contract; the `js-countdown` span only ever
                        // carries the relative "in ..." fallback, so a script ticking
                        // it down never collides with the word "until" in the sentence.
                        p class="mt-2 text-sm text-amber-700 dark:text-amber-300" {
                            "Paused until "
                            span class="font-medium tabular-nums" {
                                (deadline.format("%Y-%m-%d %H:%M:%S UTC"))
                            }
                            " ("
                            span
                                class="js-countdown font-medium tabular-nums"
                                data-deadline=(deadline.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                            {
                                (relative_fallback(deadline, now))
                            }
                            ")."
                        }
                    }
                } @else {
                    // `is_paused` is only true when `until` is `Some`; render a safe
                    // placeholder instead of failing the page if that ever drifts.
                    p class="mt-2 text-sm text-slate-500 dark:text-slate-400" { "Paused." }
                }
                (resume_form(module))
            } @else {
                p class="mt-2 text-sm text-slate-500 dark:text-slate-400" { "Not paused." }
                (pause_form(module))
            }
        }
    }
}

/// A short "how long until" fallback for the countdown span's no-JS text.
///
/// `sleep_duration_until` saturates at zero, so this never goes negative even
/// for a deadline that has technically just lapsed by the time of render.
fn relative_fallback(deadline: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let remaining = sleep_duration_until(deadline, now).as_secs();
    let days = remaining / 86_400;
    let hours = (remaining % 86_400) / 3600;
    let minutes = (remaining % 3600) / 60;

    if days > 0 {
        format!("in {days}d {hours}h")
    } else if hours > 0 {
        format!("in {hours}h {minutes}m")
    } else if minutes > 0 {
        format!("in {minutes}m")
    } else {
        "in under a minute".to_string()
    }
}

/// The duration picker and "Pause" button for one module.
fn pause_form(module: Module) -> Markup {
    maud::html! {
        form method="post" action="/settings/runtime/pause" class="mt-3 flex flex-wrap items-center gap-2" {
            input type="hidden" name="module" value=(module.as_str());
            select
                name="duration"
                class="rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-800 px-2 py-1.5 text-sm text-slate-700 dark:text-slate-200"
            {
                option value="1h" { "1 hour" }
                option value="6h" { "6 hours" }
                option value="12h" { "12 hours" }
                option value="24h" { "24 hours" }
                option value="3d" { "3 days" }
                option value="7d" { "7 days" }
                option value="indefinite" { "Indefinite" }
            }
            button
                type="submit"
                class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-sm font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900"
            {
                "Pause"
            }
        }
    }
}

/// The "Resume" button for one module.
fn resume_form(module: Module) -> Markup {
    maud::html! {
        form method="post" action="/settings/runtime/resume" class="mt-3" {
            input type="hidden" name="module" value=(module.as_str());
            button
                type="submit"
                class="rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-800 px-3 py-1.5 text-sm font-medium text-slate-700 dark:text-slate-200 hover:bg-slate-50 dark:hover:bg-slate-700"
            {
                "Resume"
            }
        }
    }
}

/// Form body for `POST /settings/runtime/pause`.
#[derive(Debug, Deserialize)]
pub(crate) struct PauseForm {
    module: Module,
    duration: PauseDuration,
}

/// Form body for `POST /settings/runtime/resume`.
#[derive(Debug, Deserialize)]
pub(crate) struct ResumeForm {
    module: Module,
}

/// `POST /settings/runtime/pause` — pause a module for a chosen duration.
pub(crate) async fn pause_submit(
    auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    form: Result<Form<PauseForm>, axum::extract::rejection::FormRejection>,
) -> Redirect {
    let Ok(Form(form)) = form else {
        set_flash(
            &session,
            "error",
            "Unrecognised pause request; choose a module and duration from the form.",
        )
        .await;
        return Redirect::to("/settings/runtime");
    };

    let now = Utc::now();
    let Some(until) = form.duration.expiry(now) else {
        set_flash(
            &session,
            "error",
            "That pause duration is out of range; choose a shorter one.",
        )
        .await;
        return Redirect::to("/settings/runtime");
    };

    let mut patch = RuntimeSettingsPatch {
        updated_by: Some(auth.user_id.to_string()),
        ..RuntimeSettingsPatch::default()
    };
    form.module.apply_pause(&mut patch, until);

    match db::patch_runtime_settings(&state.pool, &patch).await {
        Ok(_row) => {
            set_flash(
                &session,
                "success",
                &format!("{} paused.", form.module.label()),
            )
            .await;
        }
        Err(error) => {
            tracing::error!(%error, "failed to pause via the runtime control panel");
            set_flash(&session, "error", "Failed to update pause state.").await;
        }
    }

    Redirect::to("/settings/runtime")
}

/// `POST /settings/runtime/resume` — clear a module's pause.
pub(crate) async fn resume_submit(
    auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    form: Result<Form<ResumeForm>, axum::extract::rejection::FormRejection>,
) -> Redirect {
    let Ok(Form(form)) = form else {
        set_flash(
            &session,
            "error",
            "Unrecognised resume request; choose a module from the form.",
        )
        .await;
        return Redirect::to("/settings/runtime");
    };

    let mut patch = RuntimeSettingsPatch {
        updated_by: Some(auth.user_id.to_string()),
        ..RuntimeSettingsPatch::default()
    };
    form.module.apply_resume(&mut patch);

    match db::patch_runtime_settings(&state.pool, &patch).await {
        Ok(_row) => {
            set_flash(
                &session,
                "success",
                &format!("{} resumed.", form.module.label()),
            )
            .await;
        }
        Err(error) => {
            tracing::error!(%error, "failed to resume via the runtime control panel");
            set_flash(&session, "error", "Failed to update pause state.").await;
        }
    }

    Redirect::to("/settings/runtime")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration as StdDuration;

    use hof_core::runtime_config::{EffectiveSettings, Provenance, Resolved};

    use super::*;

    fn sample_settings(
        indexing_paused_until: Option<DateTime<Utc>>,
        downloads_paused_until: Option<DateTime<Utc>>,
    ) -> EffectiveSettings {
        EffectiveSettings {
            indexing_paused_until,
            downloads_paused_until,
            max_concurrent_downloads: Resolved {
                value: 3,
                provenance: Provenance::Default,
            },
            max_indexers_per_tick: Resolved {
                value: 5,
                provenance: Provenance::Default,
            },
            rate_limit_delay: Resolved {
                value: StdDuration::from_secs(5),
                provenance: Provenance::Default,
            },
            check_interval: Resolved {
                value: StdDuration::from_mins(1),
                provenance: Provenance::Default,
            },
            cleanup_interval: Resolved {
                value: StdDuration::from_hours(1),
                provenance: Provenance::Default,
            },
            drain_timeout: Resolved {
                value: StdDuration::from_mins(30),
                provenance: Provenance::Default,
            },
        }
    }

    fn sample_view(settings: EffectiveSettings) -> PanelView {
        PanelView {
            now: Utc::now(),
            settings: Arc::new(settings),
            row: None,
            drain_started_at: None,
            drain_deadline: None,
            supervisor: None,
            scheduler: None,
            cleanup: None,
            download_timeout: StdDuration::from_secs(30),
            ytdlp_timeout: StdDuration::from_secs(30),
            min_index_interval: StdDuration::from_mins(1),
        }
    }

    #[test]
    fn duration_variants_map_to_expected_chrono_duration() {
        assert_eq!(
            PauseDuration::OneHour.delta(),
            chrono::Duration::try_hours(1)
        );
        assert_eq!(
            PauseDuration::SixHours.delta(),
            chrono::Duration::try_hours(6)
        );
        assert_eq!(
            PauseDuration::TwelveHours.delta(),
            chrono::Duration::try_hours(12)
        );
        assert_eq!(PauseDuration::OneDay.delta(), chrono::Duration::try_days(1));
        assert_eq!(
            PauseDuration::ThreeDays.delta(),
            chrono::Duration::try_days(3)
        );
        assert_eq!(
            PauseDuration::SevenDays.delta(),
            chrono::Duration::try_days(7)
        );
        assert_eq!(PauseDuration::Indefinite.delta(), None);
    }

    #[test]
    fn indefinite_duration_expires_to_exactly_the_sentinel() {
        let now = Utc::now();
        assert_eq!(
            PauseDuration::Indefinite.expiry(now),
            Some(indefinite_pause())
        );
    }

    #[test]
    fn finite_duration_expires_in_the_future_and_not_at_the_sentinel() {
        let now = Utc::now();
        let until = PauseDuration::SevenDays
            .expiry(now)
            .expect("seven days does not overflow");
        assert!(until > now);
        assert_ne!(until, indefinite_pause());
    }

    #[test]
    fn module_indexing_pause_touches_only_the_indexing_column() {
        let now = Utc::now();
        let mut patch = RuntimeSettingsPatch::default();
        Module::Indexing.apply_pause(&mut patch, now);
        assert_eq!(patch.indexing_paused_until, Some(Some(now)));
        assert_eq!(patch.downloads_paused_until, None);
    }

    #[test]
    fn module_downloads_pause_touches_only_the_downloads_column() {
        let now = Utc::now();
        let mut patch = RuntimeSettingsPatch::default();
        Module::Downloads.apply_pause(&mut patch, now);
        assert_eq!(patch.downloads_paused_until, Some(Some(now)));
        assert_eq!(patch.indexing_paused_until, None);
    }

    #[test]
    fn module_all_pause_touches_both_columns() {
        let now = Utc::now();
        let mut patch = RuntimeSettingsPatch::default();
        Module::All.apply_pause(&mut patch, now);
        assert_eq!(patch.indexing_paused_until, Some(Some(now)));
        assert_eq!(patch.downloads_paused_until, Some(Some(now)));
    }

    #[test]
    fn module_indexing_resume_nulls_only_the_indexing_column() {
        let mut patch = RuntimeSettingsPatch::default();
        Module::Indexing.apply_resume(&mut patch);
        assert_eq!(patch.indexing_paused_until, Some(None));
        assert_eq!(patch.downloads_paused_until, None);
    }

    #[test]
    fn module_all_resume_nulls_both_columns() {
        let mut patch = RuntimeSettingsPatch::default();
        Module::All.apply_resume(&mut patch);
        assert_eq!(patch.indexing_paused_until, Some(None));
        assert_eq!(patch.downloads_paused_until, Some(None));
    }

    #[test]
    fn indefinitely_paused_view_renders_the_word_not_the_sentinel_year() {
        let settings = sample_settings(Some(indefinite_pause()), None);
        let view = sample_view(settings);

        let rendered = section(&view).into_string();

        assert!(rendered.contains("indefinite"));
        assert!(!rendered.contains("9999"));
    }

    #[test]
    fn finitely_paused_view_emits_the_countdown_markup_contract() {
        let now = Utc::now();
        let deadline = now
            .checked_add_signed(chrono::Duration::try_hours(6).expect("6h fits"))
            .expect("does not overflow");
        let settings = sample_settings(None, Some(deadline));
        let view = sample_view(settings);

        let rendered = section(&view).into_string();

        assert!(rendered.contains("js-countdown"));
        assert!(rendered.contains("data-deadline="));
        assert!(!rendered.contains("9999"));

        // The absolute timestamp is rendered plainly ("Paused until <time>"); the
        // `js-countdown` span itself only ever carries the relative fallback, so a
        // script ticking that span's text never produces "until in 5h 58m". The
        // deadline was computed from a slightly earlier `now` than the one the
        // view renders against, so allow for that sub-minute jitter.
        assert!(rendered.contains("in 5h"));
    }

    #[test]
    fn relative_fallback_reports_days_hours_minutes_and_the_sub_minute_floor() {
        let now = Utc::now();
        let in_3d_2h = now
            .checked_add_signed(chrono::Duration::try_hours(74).expect("74h fits"))
            .expect("does not overflow");
        assert_eq!(relative_fallback(in_3d_2h, now), "in 3d 2h");

        let in_5h_30m = now
            .checked_add_signed(chrono::Duration::try_minutes(330).expect("330m fits"))
            .expect("does not overflow");
        assert_eq!(relative_fallback(in_5h_30m, now), "in 5h 30m");

        let in_10m = now
            .checked_add_signed(chrono::Duration::try_minutes(10).expect("10m fits"))
            .expect("does not overflow");
        assert_eq!(relative_fallback(in_10m, now), "in 10m");

        assert_eq!(relative_fallback(now, now), "in under a minute");
    }

    #[test]
    fn unpaused_view_offers_the_seven_fixed_durations() {
        let settings = sample_settings(None, None);
        let view = sample_view(settings);

        let rendered = section(&view).into_string();

        for value in ["1h", "6h", "12h", "24h", "3d", "7d", "indefinite"] {
            assert!(
                rendered.contains(&format!("value=\"{value}\"")),
                "missing duration option {value}"
            );
        }
    }
}
