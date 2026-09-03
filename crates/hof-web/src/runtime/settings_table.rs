//! Effective-settings table for the runtime control panel.
//!
//! Every knob is resolved through three layers — compiled-in default, then
//! environment variable, then the database row. ADR-0002 makes the
//! provenance badge **required, not decorative**: without it the precedence
//! chain is opaque, and an operator cannot tell why a value is what it is or
//! whether editing the database row would even take effect.

use std::time::Duration;

use hof_core::runtime_config::Provenance;
use maud::Markup;

use super::{PanelView, panel_section};

/// Render the effective-settings table.
pub(crate) fn section(view: &PanelView) -> Markup {
    let s = &view.settings;
    maud::html! {
        (panel_section("Effective settings", &maud::html! {
            p class="text-sm text-slate-600 dark:text-slate-400" {
                "Each value is resolved by precedence: "
                span class="font-medium" { "database" }
                " overrides "
                span class="font-medium" { "env" }
                " overrides the compiled-in "
                span class="font-medium" { "default" }
                ". The badge shows which layer supplied the value in force right now."
            }

            div class="mt-4 overflow-x-auto" {
                table class="w-full text-left text-sm" {
                    thead class="text-xs uppercase tracking-wide text-slate-500 dark:text-slate-400" {
                        tr {
                            th class="py-2 pr-4" { "Setting" }
                            th class="py-2 pr-4" { "Value" }
                            th class="py-2 pr-4" { "Source" }
                            th class="py-2" { "What it controls" }
                        }
                    }
                    tbody class="divide-y divide-slate-200 dark:divide-slate-700" {
                        (row("Max concurrent downloads", &s.max_concurrent_downloads.value.to_string(),
                             s.max_concurrent_downloads.provenance,
                             "How many downloads may run at once."))
                        (row("Max indexers per tick", &s.max_indexers_per_tick.value.to_string(),
                             s.max_indexers_per_tick.provenance,
                             "How many sources the scheduler may start indexing in a single tick."))
                        (row("Rate-limit delay", &humanize(s.rate_limit_delay.value),
                             s.rate_limit_delay.provenance,
                             "Pause inserted between yt-dlp invocations to avoid upstream rate limiting."))
                        (row("Scheduler interval", &humanize(s.check_interval.value),
                             s.check_interval.provenance,
                             "How often the scheduler wakes to look for due sources and pending downloads."))
                        (row("Cleanup interval", &humanize(s.cleanup_interval.value),
                             s.cleanup_interval.provenance,
                             "How often retention, quota, and temp-file cleanup runs."))
                        (row("Drain timeout", &humanize(s.drain_timeout.value),
                             s.drain_timeout.provenance,
                             "How long a shutdown waits for in-flight work before forcing the exit."))
                    }
                }
            }

            (audit_stamp(view))
        }))
    }
}

/// One knob: label, value, provenance badge, and what it actually does.
fn row(label: &str, value: &str, provenance: Provenance, description: &str) -> Markup {
    maud::html! {
        tr {
            td class="py-2 pr-4 font-medium text-slate-900 dark:text-slate-100" { (label) }
            td class="py-2 pr-4 tabular-nums text-slate-700 dark:text-slate-200" { (value) }
            td class="py-2 pr-4" { (badge(provenance)) }
            td class="py-2 text-slate-600 dark:text-slate-400" { (description) }
        }
    }
}

/// Provenance badge.
///
/// Colour alone would fail for colour-blind readers and in monochrome, so the
/// layer name is always spelled out in the badge text as well.
fn badge(provenance: Provenance) -> Markup {
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
