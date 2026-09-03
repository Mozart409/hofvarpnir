//! Timings section of the runtime control panel.
//!
//! Design 7.1: every time value the system is currently operating under is
//! surfaced, not just the tunables. Anything counting down is shown both as
//! an absolute timestamp (what you need to reason about a schedule) and as a
//! live relative countdown (what you need to decide whether to wait). The
//! live half of that contract is `crate::runtime::timings` markup plus
//! `assets/runtime-countdown.js`, which ticks every `.js-countdown` span
//! this module emits.

use std::time::Duration;

use chrono::{DateTime, Utc};
use hof_core::runtime_config::{Provenance, indefinite_pause};
use maud::Markup;

use super::{PanelView, panel_section};

/// Render the Timings section.
pub(crate) fn section(view: &PanelView) -> Markup {
    let now = view.now;
    let s = &view.settings;

    panel_section(
        "Timings",
        &maud::html! {
            p class="text-sm text-slate-600 dark:text-slate-400" {
                "Everything the system is currently counting toward. A live value shows "
                "both the absolute instant and a running countdown next to it — the "
                "absolute time is what you need to reason about a schedule, the "
                "countdown is what you need to decide whether to wait."
            }

            div class="mt-4 overflow-x-auto" {
                table class="w-full text-left text-sm" {
                    thead class="text-xs uppercase tracking-wide text-slate-500 dark:text-slate-400" {
                        tr {
                            th class="py-2 pr-4" { "Timing" }
                            th class="py-2 pr-4" { "Value" }
                            th class="py-2 pr-4" { "Source" }
                            th class="py-2" { "Detail" }
                        }
                    }
                    tbody class="divide-y divide-slate-200 dark:divide-slate-700" {
                        (pause_row(
                            "Indexing pause expiry",
                            s.indexing_paused_until,
                            now,
                            "Indexing resumes automatically once this passes.",
                        ))
                        (pause_row(
                            "Downloads pause expiry",
                            s.downloads_paused_until,
                            now,
                            "Downloads resume automatically once this passes.",
                        ))
                        (drain_deadline_row(view))
                        (drain_elapsed_row(view))
                        (next_cleanup_row(view))
                        (next_tick_row(view))
                        (readonly_row(
                            "Download timeout",
                            view.download_timeout,
                            view.download_timeout_provenance,
                            "Per-download network timeout for in-flight transfers.",
                        ))
                        (readonly_row(
                            "yt-dlp command timeout",
                            view.ytdlp_timeout,
                            Provenance::Default,
                            "Per-invocation timeout applied to every yt-dlp subprocess call.",
                        ))
                        (readonly_row(
                            "Minimum re-index interval",
                            view.min_index_interval,
                            Provenance::Default,
                            "Floor between re-indexing the same source, regardless of its \
                             configured index frequency.",
                        ))
                        (plain_row(
                            "Inter-invocation rate-limit delay",
                            &humanize(s.rate_limit_delay.value),
                            "Pause inserted between yt-dlp invocations to avoid upstream \
                             rate limiting.",
                        ))
                    }
                }
            }
        },
    )
}

/// Which way a countdown moves.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    /// Counts time remaining until the instant — the common case.
    Down,
    /// Counts elapsed time since the instant, e.g. "drained for".
    Up,
}

impl Direction {
    /// The `data-direction` attribute value.
    ///
    /// `None` omits the attribute entirely: the script's default already
    /// matches `Down`, so a finite/down countdown needs no attribute at all.
    const fn data_attr(self) -> Option<&'static str> {
        match self {
            Self::Down => None,
            Self::Up => Some("up"),
        }
    }
}

/// One row: label, value markup, a source badge (or none), and detail text.
fn row(label: &str, value: &Markup, source: &Markup, detail: &str) -> Markup {
    maud::html! {
        tr {
            td class="py-2 pr-4 align-top font-medium text-slate-900 dark:text-slate-100" { (label) }
            td class="py-2 pr-4 align-top" { (value) }
            td class="py-2 pr-4 align-top" { (source) }
            td class="py-2 align-top text-slate-600 dark:text-slate-400" { (detail) }
        }
    }
}

/// Absolute timestamp plus a live `js-countdown` span.
///
/// This is the markup contract `runtime-countdown.js` reads: a
/// `.js-countdown` span carrying an RFC3339 `data-deadline` and an optional
/// `data-direction="up"`, pre-filled with a server-rendered fallback that is
/// already correct without JavaScript.
fn live_cell(instant: DateTime<Utc>, now: DateTime<Utc>, direction: Direction) -> Markup {
    let absolute = instant.format("%Y-%m-%d %H:%M:%S UTC").to_string();
    let fallback = countdown_fallback(instant, now, direction);
    maud::html! {
        span class="tabular-nums text-slate-900 dark:text-slate-100" { (absolute) }
        " "
        span
            class="js-countdown tabular-nums text-slate-500 dark:text-slate-400"
            data-deadline=(instant.to_rfc3339())
            data-direction=[direction.data_attr()]
        {
            (fallback)
        }
    }
}

/// Server-rendered fallback text for a countdown span.
///
/// This is what a JS-disabled browser sees permanently, and what a
/// JS-enabled one sees for the first frame before the script takes over —
/// both must already be correct, per the progressive-enhancement contract.
fn countdown_fallback(instant: DateTime<Utc>, now: DateTime<Utc>, direction: Direction) -> String {
    match direction {
        Direction::Down => {
            let remaining = instant.signed_duration_since(now);
            // Millisecond granularity here (not `num_seconds`) so this
            // agrees with the script's own `remainingMs <= 0` check: a
            // deadline a few hundred milliseconds out renders "in 0s", not
            // a premature "overdue".
            if remaining.num_milliseconds() <= 0 {
                "overdue".to_string()
            } else {
                match remaining.to_std() {
                    Ok(d) => format!("in {}", humanize(d)),
                    Err(_) => "overdue".to_string(),
                }
            }
        }
        Direction::Up => {
            let elapsed = now.signed_duration_since(instant);
            match elapsed.to_std() {
                // A negative elapsed (clock skew, or `now` sampled before
                // `instant`) has no honest positive rendering; settle on
                // "0s so far" rather than surfacing a negative duration.
                Ok(d) => format!("{} so far", humanize(d)),
                Err(_) => "0s so far".to_string(),
            }
        }
    }
}

/// Muted placeholder for a section whose backing actor ask or DB read failed.
///
/// The panel degrades a part of the row rather than the whole page.
fn placeholder(text: &str) -> Markup {
    maud::html! { span class="italic text-slate-500 dark:text-slate-400" { (text) } }
}

/// One pause-expiry row.
///
/// Never renders the [`indefinite_pause`] sentinel timestamp (ADR-0003): a
/// pause equal to it displays as the word "indefinite" instead of a
/// year-9999 date.
fn pause_row(
    label: &str,
    until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    detail: &str,
) -> Markup {
    let value = match until {
        None => maud::html! { span class="text-slate-500 dark:text-slate-400" { "Not paused" } },
        Some(t) if t == indefinite_pause() => maud::html! {
            span class="font-medium text-amber-700 dark:text-amber-300" { "Paused indefinitely" }
        },
        Some(t) => live_cell(t, now, Direction::Down),
    };
    row(label, &value, &maud::html! {}, detail)
}

/// Drain deadline; the countdown next to it doubles as time remaining.
///
/// The absolute instant and the relative wait are the same live value,
/// rendered once, per design 7.1.
fn drain_deadline_row(view: &PanelView) -> Markup {
    let value = match view.drain_deadline {
        None => maud::html! { span class="text-slate-500 dark:text-slate-400" { "Not draining" } },
        Some(deadline) => live_cell(deadline, view.now, Direction::Down),
    };
    row(
        "Drain deadline",
        &value,
        &maud::html! {},
        "Shutdown proceeds regardless of in-flight work once this passes.",
    )
}

/// Time drained so far, counting **up** from when the drain began.
fn drain_elapsed_row(view: &PanelView) -> Markup {
    let value = match view.drain_started_at {
        None => maud::html! { span class="text-slate-500 dark:text-slate-400" { "Not draining" } },
        Some(started) => live_cell(started, view.now, Direction::Up),
    };
    row(
        "Time drained so far",
        &value,
        &maud::html! {},
        "Counts up from the moment the drain began.",
    )
}

/// `last_run_at + interval`, computed with checked arithmetic.
///
/// An absurd interval (one that overflows `chrono::Duration` or pushes
/// `last_run_at` past what `DateTime<Utc>` can represent) yields `None`
/// rather than panicking or silently wrapping.
fn next_run_after(last_run_at: DateTime<Utc>, interval: Duration) -> Option<DateTime<Utc>> {
    chrono::Duration::from_std(interval)
        .ok()
        .and_then(|delta| last_run_at.checked_add_signed(delta))
}

/// Next cleanup run: `cleanup.last_run_at + settings.cleanup_interval`.
fn next_cleanup_row(view: &PanelView) -> Markup {
    let value = match &view.cleanup {
        None => placeholder("Cleanup status unavailable."),
        Some(status) => match status.last_run_at {
            None => maud::html! {
                span class="text-slate-500 dark:text-slate-400" { "Pending — cleanup has not run yet." }
            },
            Some(last) => match next_run_after(last, view.settings.cleanup_interval.value) {
                Some(next) => live_cell(next, view.now, Direction::Down),
                None => placeholder(
                    "Unable to compute — the interval overflows the representable range.",
                ),
            },
        },
    };
    row(
        "Next cleanup run",
        &value,
        &maud::html! {},
        "Retention, quota, and temp-file cleanup: last run plus the cleanup interval.",
    )
}

/// Next scheduler tick.
///
/// `SchedulerStatus` exposes only its tick interval, never a last-tick
/// timestamp, so a true next-tick instant cannot be derived truthfully from
/// what the actor reports. Rendering one anyway (e.g. built from
/// `Utc::now()`) would silently reset to a full interval on every page
/// load — a countdown that lies. This row states the interval plainly and
/// says so, rather than fabricating a deadline.
fn next_tick_row(view: &PanelView) -> Markup {
    let value = match &view.scheduler {
        None => placeholder("Scheduler status unavailable."),
        Some(status) => {
            let interval = Duration::from_secs(status.check_interval_secs);
            maud::html! {
                span class="tabular-nums text-slate-900 dark:text-slate-100" { "every " (humanize(interval)) }
            }
        }
    };
    row(
        "Next scheduler tick",
        &value,
        &maud::html! {},
        "The scheduler actor reports only its tick interval, not a last-tick timestamp, \
         so the next tick's instant is not directly observable.",
    )
}

/// A compiled-in or env-derived timing: read-only, never runtime-mutable.
///
/// Carries a provenance badge for the same reason `settings_table` badges
/// the effective-settings rows — so the panel never implies an operator
/// could edit this value here.
fn readonly_row(label: &str, value: Duration, provenance: Provenance, detail: &str) -> Markup {
    row(
        label,
        &maud::html! { span class="tabular-nums text-slate-900 dark:text-slate-100" { (humanize(value)) } },
        &badge(provenance),
        detail,
    )
}

/// A plain, already-humanized duration with no badge and no countdown.
fn plain_row(label: &str, value: &str, detail: &str) -> Markup {
    row(
        label,
        &maud::html! { span class="tabular-nums text-slate-900 dark:text-slate-100" { (value) } },
        &maud::html! {},
        detail,
    )
}

/// Read-only provenance badge, matching `settings_table`'s visual idiom.
///
/// Colour alone would fail for colour-blind readers and in monochrome, so
/// the layer name is always spelled out in the badge text too.
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
    use std::sync::Arc;

    use chrono::TimeZone;
    use hof_core::actors::cleanup::CleanupStatus;
    use hof_core::actors::scheduler::SchedulerStatus;
    use hof_core::runtime_config::{EffectiveSettings, Resolved};

    use super::*;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0)
            .single()
            .expect("valid fixed instant")
    }

    /// `base` shifted by `hours` (negative for the past).
    ///
    /// Uses checked arithmetic rather than plain `DateTime + Duration`,
    /// which panics on overflow — house style avoids that even in
    /// fixtures where it isn't literally denied.
    fn hours_from(base: DateTime<Utc>, hours: i64) -> DateTime<Utc> {
        base.checked_add_signed(chrono::Duration::hours(hours))
            .expect("in range for a small test offset")
    }

    fn base_settings() -> EffectiveSettings {
        EffectiveSettings {
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
        }
    }

    fn base_view(now: DateTime<Utc>, settings: EffectiveSettings) -> PanelView {
        PanelView {
            now,
            settings: Arc::new(settings),
            row: None,
            drain_started_at: None,
            drain_deadline: None,
            supervisor: None,
            scheduler: Some(SchedulerStatus {
                running: true,
                active_indexers: 0,
                check_interval_secs: 60,
            }),
            cleanup: Some(CleanupStatus {
                running: true,
                global_retention_days: None,
                cleanup_interval_secs: 10800,
                last_run_at: Some(now),
            }),
            download_timeout: Duration::from_hours(4),
            download_timeout_provenance: Provenance::Default,
            ytdlp_timeout: Duration::from_mins(30),
            min_index_interval: Duration::from_mins(5),
        }
    }

    #[test]
    fn indefinite_pause_renders_the_word_not_the_sentinel() {
        let now = fixed_now();
        let mut settings = base_settings();
        settings.indexing_paused_until = Some(indefinite_pause());
        let view = base_view(now, settings);

        let out = section(&view).into_string();

        assert!(out.contains("indefinite"));
        assert!(!out.contains("9999"));
    }

    #[test]
    fn absent_pause_renders_not_paused() {
        let now = fixed_now();
        let view = base_view(now, base_settings());

        let out = section(&view).into_string();

        assert!(out.contains("Not paused"));
    }

    #[test]
    fn absent_drain_renders_not_draining() {
        let now = fixed_now();
        let view = base_view(now, base_settings());

        let out = section(&view).into_string();

        assert_eq!(out.matches("Not draining").count(), 2);
    }

    #[test]
    fn next_cleanup_run_uses_checked_addition_of_interval() {
        let last = fixed_now();
        let interval = Duration::from_hours(3);

        let next = next_run_after(last, interval).expect("no overflow for a small interval");

        let expected = last
            .checked_add_signed(chrono::Duration::from_std(interval).expect("valid duration"))
            .expect("no overflow");
        assert_eq!(next, expected);
    }

    #[test]
    fn next_cleanup_row_shows_last_run_plus_interval() {
        let now = fixed_now();
        let mut settings = base_settings();
        settings.cleanup_interval = Resolved {
            value: Duration::from_hours(3),
            provenance: Provenance::Default,
        };
        let mut view = base_view(now, settings);
        view.cleanup = Some(CleanupStatus {
            running: true,
            global_retention_days: None,
            cleanup_interval_secs: 10800,
            last_run_at: Some(now),
        });

        let out = next_cleanup_row(&view).into_string();
        let expected = next_run_after(now, Duration::from_hours(3)).expect("no overflow");

        assert!(out.contains(&expected.format("%Y-%m-%d %H:%M:%S UTC").to_string()));
        assert!(out.contains("js-countdown"));
    }

    #[test]
    fn missing_actor_status_degrades_to_placeholder() {
        let now = fixed_now();
        let mut view = base_view(now, base_settings());
        view.cleanup = None;
        view.scheduler = None;

        let out = section(&view).into_string();

        assert!(out.contains("Cleanup status unavailable"));
        assert!(out.contains("Scheduler status unavailable"));
    }

    #[test]
    fn readonly_rows_carry_exactly_three_provenance_badges() {
        let now = fixed_now();
        let view = base_view(now, base_settings());

        let out = section(&view).into_string();
        let badge_markers = out.matches("inline-flex items-center rounded-full").count();

        assert_eq!(badge_markers, 3);
    }

    #[test]
    fn badge_text_distinguishes_default_from_env() {
        let default_badge = badge(Provenance::Default).into_string();
        let env_badge = badge(Provenance::Env).into_string();

        assert!(default_badge.contains("default"));
        assert!(env_badge.contains("env"));
        assert_ne!(default_badge, env_badge);
    }

    #[test]
    fn every_live_row_emits_a_parseable_rfc3339_deadline() {
        let now = fixed_now();
        let mut settings = base_settings();
        settings.indexing_paused_until = Some(hours_from(now, 1));
        settings.downloads_paused_until = Some(hours_from(now, 2));
        let mut view = base_view(now, settings);
        view.drain_started_at = Some(hours_from(now, -1));
        view.drain_deadline = Some(hours_from(now, 1));
        view.cleanup = Some(CleanupStatus {
            running: true,
            global_retention_days: None,
            cleanup_interval_secs: 10800,
            last_run_at: Some(now),
        });

        let out = section(&view).into_string();
        let deadlines: Vec<&str> = out
            .split("data-deadline=\"")
            .skip(1)
            .map(|chunk| chunk.split('"').next().expect("closing quote present"))
            .collect();

        // Five live rows: indexing pause, downloads pause, drain deadline,
        // drain elapsed, next cleanup run. The read-only rows and the
        // scheduler-interval row never emit a countdown span.
        assert_eq!(deadlines.len(), 5);
        for raw in deadlines {
            DateTime::parse_from_rfc3339(raw)
                .unwrap_or_else(|error| panic!("data-deadline {raw:?} not RFC3339: {error}"));
        }
    }

    #[test]
    fn countdown_fallback_down_settles_on_overdue_not_a_negative_number() {
        let now = fixed_now();
        let past = hours_from(now, -1);

        assert_eq!(countdown_fallback(past, now, Direction::Down), "overdue");
    }

    #[test]
    fn countdown_fallback_up_counts_elapsed_time() {
        let start = fixed_now();
        let now = hours_from(start, 1);

        assert_eq!(countdown_fallback(start, now, Direction::Up), "1h so far");
    }

    #[test]
    fn humanize_covers_each_magnitude() {
        assert_eq!(humanize(Duration::from_secs(0)), "0s");
        assert_eq!(humanize(Duration::from_secs(45)), "45s");
        assert_eq!(humanize(Duration::from_secs(90)), "1m 30s");
        assert_eq!(humanize(Duration::from_hours(3)), "3h");
    }
}
