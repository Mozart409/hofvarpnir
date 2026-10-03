//! Effective-settings table for the runtime control panel.
//!
//! Every knob is resolved through three layers — compiled-in default, then
//! environment variable, then the database row. ADR-0002 makes the
//! provenance badge **required, not decorative**: without it the precedence
//! chain is opaque, and an operator cannot tell why a value is what it is or
//! whether editing the database row would even take effect.

use axum::Form;
use axum::extract::State;
use axum::response::Redirect;
use hof_api::AppState;
use hof_api::routes::settings::{PatchSettingsRequest, validate_patch};
use hof_core::db::{self, RuntimeSettingsRow};
use hof_core::runtime_config::Provenance;
use maud::Markup;
use serde::Deserialize;
use tower_sessions::Session;

use super::{PanelView, badge, humanize, panel_section};
use crate::auth::AuthUser;
use crate::pages::set_flash;

/// One editable knob: how to label it, read it, and name it in the form.
struct KnobSpec {
    /// Form field name; identical to the API's JSON key.
    key: &'static str,
    label: &'static str,
    description: &'static str,
    /// Plain count (`false`) or duration in seconds (`true`).
    is_duration: bool,
    /// Lower bound, mirrored into the input's `min` attribute. The server
    /// re-checks it through `hof_api`'s `validate_patch`.
    min: u32,
}

const KNOBS: [KnobSpec; 6] = [
    KnobSpec {
        key: "max_concurrent_downloads",
        label: "Max concurrent downloads",
        description: "How many downloads may run at once.",
        is_duration: false,
        min: 1,
    },
    KnobSpec {
        key: "max_indexers_per_tick",
        label: "Max indexers per tick",
        description: "How many sources the scheduler may start indexing in a single tick.",
        is_duration: false,
        min: 1,
    },
    KnobSpec {
        key: "rate_limit_delay_secs",
        label: "Rate-limit delay",
        description: "Pause inserted between yt-dlp invocations to avoid upstream rate limiting.",
        is_duration: true,
        min: 0,
    },
    KnobSpec {
        key: "check_interval_secs",
        label: "Scheduler interval",
        description: "How often the scheduler wakes to look for due sources and pending downloads.",
        is_duration: true,
        min: 1,
    },
    KnobSpec {
        key: "cleanup_interval_secs",
        label: "Cleanup interval",
        description: "How often retention, quota, and temp-file cleanup runs.",
        is_duration: true,
        min: 1,
    },
    KnobSpec {
        key: "drain_timeout_secs",
        label: "Drain timeout",
        description: "How long a shutdown waits for in-flight work before forcing the exit.",
        is_duration: true,
        min: 1,
    },
];

/// Effective value (as displayed) and provenance of a knob.
fn effective(view: &PanelView, key: &str) -> (String, Provenance) {
    let s = &view.settings;
    match key {
        "max_concurrent_downloads" => (
            s.max_concurrent_downloads.value.to_string(),
            s.max_concurrent_downloads.provenance,
        ),
        "max_indexers_per_tick" => (
            s.max_indexers_per_tick.value.to_string(),
            s.max_indexers_per_tick.provenance,
        ),
        "rate_limit_delay_secs" => (
            humanize(s.rate_limit_delay.value),
            s.rate_limit_delay.provenance,
        ),
        "check_interval_secs" => (
            humanize(s.check_interval.value),
            s.check_interval.provenance,
        ),
        "cleanup_interval_secs" => (
            humanize(s.cleanup_interval.value),
            s.cleanup_interval.provenance,
        ),
        "drain_timeout_secs" => (humanize(s.drain_timeout.value), s.drain_timeout.provenance),
        _ => (String::new(), Provenance::Default),
    }
}

/// The database-layer override for a knob, if one is set.
fn db_override(row: &RuntimeSettingsRow, key: &str) -> Option<i32> {
    match key {
        "max_concurrent_downloads" => row.max_concurrent_downloads,
        "max_indexers_per_tick" => row.max_indexers_per_tick,
        "rate_limit_delay_secs" => row.rate_limit_delay_secs,
        "check_interval_secs" => row.check_interval_secs,
        "cleanup_interval_secs" => row.cleanup_interval_secs,
        "drain_timeout_secs" => row.drain_timeout_secs,
        _ => None,
    }
}

/// Render the effective-settings table, with an override input per knob.
pub(crate) fn section(view: &PanelView) -> Markup {
    maud::html! {
        (panel_section("Effective settings", &maud::html! {
            p class="text-sm text-slate-600 dark:text-slate-400" {
                "Each value is resolved by precedence: "
                span class="font-medium" { "database" }
                " overrides "
                span class="font-medium" { "env" }
                " overrides the compiled-in "
                span class="font-medium" { "default" }
                ". The badge shows which layer supplied the value in force right now. "
                "An override is stored in the database; clear it to fall back to env or default."
            }

            form id="runtime-settings-form" method="post" action="/settings/runtime/settings" {
                div class="mt-4 overflow-x-auto" {
                    table class="w-full text-left text-sm" {
                        thead class="text-xs uppercase tracking-wide text-slate-500 dark:text-slate-400" {
                            tr {
                                th class="py-2 pr-4" { "Setting" }
                                th class="py-2 pr-4" { "Value" }
                                th class="py-2 pr-4" { "Source" }
                                th class="py-2 pr-4" { "Override" }
                                th class="py-2" { "What it controls" }
                            }
                        }
                        tbody class="divide-y divide-slate-200 dark:divide-slate-700" {
                            @for knob in &KNOBS {
                                (row(view, knob))
                            }
                        }
                    }
                }

                div class="mt-4 flex flex-wrap items-center gap-3" {
                    button
                        type="submit"
                        class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-sm font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900"
                    {
                        "Save overrides"
                    }
                    // Unhidden by runtime-live.js when a live update arrives
                    // while this form has unsaved edits.
                    p id="runtime-settings-stale" hidden
                        class="text-sm text-amber-700 dark:text-amber-300"
                    {
                        "Settings were changed elsewhere. Save to apply your edits on top, or reload to discard them."
                    }
                }
            }

            (audit_stamp(view))
        }))
    }
}

/// One knob row, wired to the panel view and its override input.
fn row(view: &PanelView, knob: &KnobSpec) -> Markup {
    let (value, provenance) = effective(view, knob.key);
    let current = view.row.as_ref().and_then(|r| db_override(r, knob.key));
    let input = maud::html! {
        div class="flex items-center gap-1" {
            input
                type="number"
                name=(knob.key)
                min=(knob.min)
                max=(i32::MAX)
                step="1"
                inputmode="numeric"
                value=[current]
                placeholder="not set"
                aria-label={ (knob.label) " override" }
                class="w-28 rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-800 px-2 py-1 text-sm tabular-nums text-slate-700 dark:text-slate-200";
            @if knob.is_duration {
                span class="text-xs text-slate-500 dark:text-slate-400" { "s" }
            }
        }
    };
    row_markup(knob.label, &value, provenance, &input, knob.description)
}

/// One knob: label, value, provenance badge, override input, and what it does.
fn row_markup(
    label: &str,
    value: &str,
    provenance: Provenance,
    input: &Markup,
    description: &str,
) -> Markup {
    maud::html! {
        tr {
            td class="py-2 pr-4 font-medium text-slate-900 dark:text-slate-100" { (label) }
            td class="py-2 pr-4 tabular-nums text-slate-700 dark:text-slate-200" { (value) }
            td class="py-2 pr-4" { (badge(provenance)) }
            td class="py-2 pr-4" { (input) }
            td class="py-2 text-slate-600 dark:text-slate-400" { (description) }
        }
    }
}

/// Form body for `POST /settings/runtime/settings`: one text field per knob,
/// keyed like the API. Empty means "no override".
#[derive(Debug, Default, Deserialize)]
pub(crate) struct SettingsForm {
    #[serde(default)]
    max_concurrent_downloads: String,
    #[serde(default)]
    max_indexers_per_tick: String,
    #[serde(default)]
    rate_limit_delay_secs: String,
    #[serde(default)]
    check_interval_secs: String,
    #[serde(default)]
    cleanup_interval_secs: String,
    #[serde(default)]
    drain_timeout_secs: String,
}

impl SettingsForm {
    fn field(&self, key: &str) -> &str {
        match key {
            "max_concurrent_downloads" => &self.max_concurrent_downloads,
            "max_indexers_per_tick" => &self.max_indexers_per_tick,
            "rate_limit_delay_secs" => &self.rate_limit_delay_secs,
            "check_interval_secs" => &self.check_interval_secs,
            "cleanup_interval_secs" => &self.cleanup_interval_secs,
            "drain_timeout_secs" => &self.drain_timeout_secs,
            _ => "",
        }
    }
}

/// Turn the submitted form into an API-shaped patch holding only the knobs
/// whose override actually changed, so an untouched form does not bump the
/// audit stamp. Returns the changed labels alongside.
fn diff_form(
    form: &SettingsForm,
    row: &RuntimeSettingsRow,
) -> Result<(PatchSettingsRequest, Vec<&'static str>), String> {
    let mut request = PatchSettingsRequest::default();
    let mut changed = Vec::new();
    for knob in &KNOBS {
        let raw = form.field(knob.key).trim();
        let desired: Option<u64> =
            if raw.is_empty() {
                None
            } else {
                Some(raw.parse().map_err(|_| {
                    format!("{} must be a whole number, got \"{raw}\".", knob.label)
                })?)
            };
        let current = db_override(row, knob.key).and_then(|v| u64::try_from(v).ok());
        if desired == current {
            continue;
        }
        changed.push(knob.label);
        let to_u32 =
            |v: u64| u32::try_from(v).map_err(|_| format!("{} is too large: {v}.", knob.label));
        match knob.key {
            "max_concurrent_downloads" => {
                request.max_concurrent_downloads = Some(desired.map(to_u32).transpose()?);
            }
            "max_indexers_per_tick" => {
                request.max_indexers_per_tick = Some(desired.map(to_u32).transpose()?);
            }
            "rate_limit_delay_secs" => request.rate_limit_delay_secs = Some(desired),
            "check_interval_secs" => request.check_interval_secs = Some(desired),
            "cleanup_interval_secs" => request.cleanup_interval_secs = Some(desired),
            "drain_timeout_secs" => request.drain_timeout_secs = Some(desired),
            _ => {}
        }
    }
    Ok((request, changed))
}

/// `POST /settings/runtime/settings` — save database-layer overrides.
pub(crate) async fn settings_submit(
    auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    form: Result<Form<SettingsForm>, axum::extract::rejection::FormRejection>,
) -> Redirect {
    const BACK: &str = "/settings/runtime";
    let Ok(Form(form)) = form else {
        set_flash(&session, "error", "Unrecognised settings request.").await;
        return Redirect::to(BACK);
    };

    let row = match db::get_runtime_settings(&state.pool).await {
        Ok(row) => row,
        Err(error) => {
            tracing::error!(%error, "failed to read runtime_settings before saving overrides");
            set_flash(&session, "error", "Failed to read the current settings.").await;
            return Redirect::to(BACK);
        }
    };

    let (request, changed) = match diff_form(&form, &row) {
        Ok(diff) => diff,
        Err(message) => {
            set_flash(&session, "error", &message).await;
            return Redirect::to(BACK);
        }
    };
    if changed.is_empty() {
        set_flash(&session, "info", "No changes to save.").await;
        return Redirect::to(BACK);
    }

    // Same bounds as `PATCH /api/v1/system/settings`.
    let mut patch = match validate_patch(&request) {
        Ok(patch) => patch,
        Err(message) => {
            set_flash(&session, "error", &message).await;
            return Redirect::to(BACK);
        }
    };
    patch.updated_by = Some(auth.user_id.to_string());

    match db::patch_runtime_settings(&state.pool, &patch).await {
        Ok(_row) => {
            set_flash(
                &session,
                "success",
                &format!("Saved: {}.", changed.join(", ")),
            )
            .await;
        }
        Err(error) => {
            tracing::error!(%error, "failed to save runtime settings overrides");
            set_flash(&session, "error", "Failed to save settings.").await;
        }
    }
    Redirect::to(BACK)
}

/// When the database layer was last written, and by whom.
///
/// `updated_by` holds a ULID string rather than a display name; it is rendered
/// as stored rather than resolved, since the panel has no user lookup.
fn audit_stamp(view: &PanelView) -> Markup {
    maud::html! {
        p class="mt-4 text-xs text-slate-500 dark:text-slate-400" {
            @match view.row.as_ref() {
                Some(row) => {
                    @match row.updated_at {
                        Some(at) => {
                            "Database layer last written "
                            span class="font-medium" { (at.format("%Y-%m-%d %H:%M:%S UTC")) }
                            " by "
                            span class="font-medium" {
                                (row.updated_by.as_deref().unwrap_or("system"))
                            }
                            "."
                        }
                        None => { "Database layer has never been written." }
                    }
                }
                None => { "Audit stamp unavailable — the settings row could not be read." }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0002 makes the provenance badge required, not decorative — without
    /// it the three-layer precedence chain is opaque. The `badge()` unit test
    /// below passes even if `row` never calls it, so this asserts the badge
    /// actually reaches rendered output. Deleting `(badge(provenance))` from
    /// `row_markup` fails here and nowhere else.
    #[test]
    fn row_renders_the_provenance_badge() {
        let html = row_markup(
            "Max concurrent downloads",
            "3",
            Provenance::Database,
            &maud::html! {},
            "How many downloads may run at once.",
        )
        .into_string();

        assert!(html.contains("Max concurrent downloads"));
        assert!(
            html.contains(">3<"),
            "value not rendered into an element body"
        );
        assert!(
            html.contains("database"),
            "provenance badge missing from the rendered row (ADR-0002)"
        );
    }

    #[test]
    fn every_provenance_renders_its_own_text() {
        let default = badge(Provenance::Default).into_string();
        let env = badge(Provenance::Env).into_string();
        let db = badge(Provenance::Database).into_string();

        assert!(default.contains("default"));
        assert!(env.contains("env"));
        assert!(db.contains("database"));
        // The three must be distinguishable by text, not only by colour.
        assert_ne!(default, env);
        assert_ne!(env, db);
        assert_ne!(default, db);
    }
}
