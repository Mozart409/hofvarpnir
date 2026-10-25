//! Split out of `pages/mod.rs`.

use super::*;

pub(super) fn format_relative_time(dt: chrono::DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(dt);

    if duration.num_seconds() < 0 {
        // Future date
        let future_duration = dt.signed_duration_since(now);
        if future_duration.num_days() > 0 {
            return format!("in {}d", future_duration.num_days());
        }
        if future_duration.num_hours() > 0 {
            return format!("in {}h", future_duration.num_hours());
        }
        return format!("in {}m", future_duration.num_minutes().max(1));
    }

    if duration.num_days() > 30 {
        return dt.format("%Y-%m-%d").to_string();
    }
    if duration.num_days() > 0 {
        return format!("{}d ago", duration.num_days());
    }
    if duration.num_hours() > 0 {
        return format!("{}h ago", duration.num_hours());
    }
    if duration.num_minutes() > 0 {
        return format!("{}m ago", duration.num_minutes());
    }
    "just now".to_string()
}

pub(super) const fn quality_options() -> &'static [QualityOption] {
    &[
        QualityOption {
            value: "best",
            label: "Best",
        },
        QualityOption {
            value: "4320p",
            label: "4320p",
        },
        QualityOption {
            value: "2160p",
            label: "2160p",
        },
        QualityOption {
            value: "1440p",
            label: "1440p",
        },
        QualityOption {
            value: "1080p",
            label: "1080p",
        },
        QualityOption {
            value: "720p",
            label: "720p",
        },
        QualityOption {
            value: "480p",
            label: "480p",
        },
        QualityOption {
            value: "audio_only",
            label: "Audio only",
        },
    ]
}

pub(super) struct QualityOption {
    pub(super) value: &'static str,
    pub(super) label: &'static str,
}

pub(super) const fn output_preset_options() -> &'static [OutputPresetOption] {
    &[
        OutputPresetOption {
            value: "auto",
            label: "Auto (best quality)",
        },
        OutputPresetOption {
            value: "browser",
            label: "Browser (Jellyfin/web direct-play)",
        },
        OutputPresetOption {
            value: "tv",
            label: "TV (smart TV direct-play)",
        },
    ]
}

pub(super) struct OutputPresetOption {
    pub(super) value: &'static str,
    pub(super) label: &'static str,
}

pub(super) const fn quality_label(quality: &Quality) -> &'static str {
    match quality {
        Quality::Best => "Best",
        Quality::Q4320p => "4320p",
        Quality::Q2160p => "2160p",
        Quality::Q1440p => "1440p",
        Quality::Q1080p => "1080p",
        Quality::Q720p => "720p",
        Quality::Q480p => "480p",
        Quality::AudioOnly => "Audio only",
    }
}

pub(super) const fn quality_value(quality: &Quality) -> &'static str {
    match quality {
        Quality::Best => "best",
        Quality::Q4320p => "4320p",
        Quality::Q2160p => "2160p",
        Quality::Q1440p => "1440p",
        Quality::Q1080p => "1080p",
        Quality::Q720p => "720p",
        Quality::Q480p => "480p",
        Quality::AudioOnly => "audio_only",
    }
}

pub(super) const fn output_preset_value(output_preset: &OutputPreset) -> &'static str {
    match output_preset {
        OutputPreset::Auto => "auto",
        OutputPreset::Browser => "browser",
        OutputPreset::Tv => "tv",
    }
}

pub(super) const fn source_type_label(source_type: &SourceType) -> &'static str {
    match source_type {
        SourceType::Channel => "Channel",
        SourceType::Playlist => "Playlist",
    }
}

pub(super) const fn entry_order_label(order: EntryOrder) -> &'static str {
    match order {
        EntryOrder::Unknown => "yt-dlp: Unknown",
        EntryOrder::Ascending => "yt-dlp: Oldest first",
        EntryOrder::Descending => "yt-dlp: Newest first",
        EntryOrder::Unordered => "yt-dlp: Unordered",
    }
}

pub(super) const fn entry_order_badge_class(order: EntryOrder) -> &'static str {
    match order {
        EntryOrder::Unknown => {
            "rounded bg-amber-100 dark:bg-amber-900/50 text-amber-900 dark:text-amber-100 px-2 py-1"
        }
        EntryOrder::Ascending | EntryOrder::Descending => {
            "rounded bg-slate-100 dark:bg-slate-700 px-2 py-1"
        }
        EntryOrder::Unordered => {
            "rounded bg-orange-100 dark:bg-orange-900/50 text-orange-900 dark:text-orange-100 px-2 py-1"
        }
    }
}

/// Maps a delivered pixel height to the resolution tier users recognise.
///
/// Platforms occasionally serve off-tier heights (letterboxed or vertical
/// video), so anything below a known tier is rendered as a raw height.
pub(super) fn resolution_label(height: i32) -> String {
    match height {
        h if h >= 4320 => "4320p".to_string(),
        h if h >= 2160 => "2160p".to_string(),
        h if h >= 1440 => "1440p".to_string(),
        h if h >= 1080 => "1080p".to_string(),
        h if h >= 720 => "720p".to_string(),
        h if h >= 480 => "480p".to_string(),
        h if h >= 360 => "360p".to_string(),
        h => format!("{h}p"),
    }
}

/// Normalises an extractor codec string into a short display name.
///
/// Codec strings carry profile/level detail (`av01.0.12M.08`) that is noise in a
/// list view; what matters for direct play is the family.
pub(super) fn codec_label(codec: &str) -> &'static str {
    let lower = codec.to_ascii_lowercase();
    if lower.contains("av01") || lower.starts_with("av1") {
        "AV1"
    } else if lower.contains("avc1") || lower.contains("h264") {
        "H.264"
    } else if lower.contains("hev1") || lower.contains("hvc1") || lower.contains("hevc") {
        "HEVC"
    } else if lower.contains("vp9") || lower.contains("vp09") {
        "VP9"
    } else if lower.contains("vp8") {
        "VP8"
    } else {
        "Video"
    }
}

/// Badge showing the resolution and codec that were actually delivered.
///
/// Renders nothing when no resolution was recorded, which covers audio-only
/// downloads and anything completed before delivered quality was tracked.
pub(super) fn delivered_quality_badge(video: &Video) -> Markup {
    let Some(height) = video.video_height else {
        return html! {};
    };

    let resolution = resolution_label(height);
    let codec = video.video_codec.as_deref().map(codec_label);
    let title = video.video_codec.as_deref().map_or_else(
        || format!("Downloaded at {resolution}"),
        |raw| format!("Downloaded at {resolution} ({raw})"),
    );

    html! {
        span
            title=(title)
            class="inline-flex items-center gap-1 rounded-full bg-indigo-100 dark:bg-indigo-900/50 px-2 py-0.5 text-xs font-medium text-indigo-900 dark:text-indigo-100"
        {
            (resolution)
            @if let Some(codec) = codec {
                span class="text-indigo-700 dark:text-indigo-300 font-normal" { (codec) }
            }
        }
    }
}

pub(super) fn status_badge(status: &VideoStatus) -> Markup {
    let (label, color) = match status {
        VideoStatus::Pending => (
            "Pending",
            "bg-amber-100 dark:bg-amber-900/50 text-amber-900 dark:text-amber-100",
        ),
        VideoStatus::AwaitingQuality => (
            "Awaiting Quality",
            "bg-yellow-100 dark:bg-yellow-900/50 text-yellow-900 dark:text-yellow-100",
        ),
        VideoStatus::Downloading => (
            "Downloading",
            "bg-sky-100 dark:bg-sky-900/50 text-sky-900 dark:text-sky-100",
        ),
        VideoStatus::Completed => (
            "Completed",
            "bg-emerald-100 dark:bg-emerald-900/50 text-emerald-900 dark:text-emerald-100",
        ),
        VideoStatus::Failed => (
            "Failed",
            "bg-rose-100 dark:bg-rose-900/50 text-rose-900 dark:text-rose-100",
        ),
        VideoStatus::Skipped => (
            "Skipped",
            "bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-300",
        ),
        VideoStatus::Cleaned => (
            "Cleaned",
            "bg-violet-100 dark:bg-violet-900/50 text-violet-900 dark:text-violet-100",
        ),
        VideoStatus::PermanentlyFailed => (
            "Permanently Failed",
            "bg-red-100 dark:bg-red-900/50 text-red-900 dark:text-red-100",
        ),
    };

    html! {
        span class={(format!("inline-flex rounded-full px-2.5 py-1 text-xs font-medium {}", color))} {
            (label)
        }
    }
}

pub(super) fn status_chip(label: &str, tone: &str) -> Markup {
    let class_name = match tone {
        "sky" => "bg-sky-100 dark:bg-sky-900/50 text-sky-900 dark:text-sky-100",
        "slate" => "bg-slate-200 dark:bg-slate-700 text-slate-900 dark:text-slate-100",
        "rose" => "bg-rose-100 dark:bg-rose-900/50 text-rose-900 dark:text-rose-100",
        "emerald" => "bg-emerald-100 dark:bg-emerald-900/50 text-emerald-900 dark:text-emerald-100",
        _ => "bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-300",
    };

    html! {
        span class={(format!("inline-flex rounded-full px-2.5 py-1 text-xs font-medium {}", class_name))} {
            (label)
        }
    }
}

pub(super) fn input_text(
    label: &str,
    name: &str,
    placeholder: &str,
    required: bool,
    value: &str,
) -> Markup {
    html! {
        div {
            label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for=(name) { (label) }
            input
                class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm"
                type="text"
                id=(name)
                name=(name)
                placeholder=(placeholder)
                required[required]
                value=(value);
        }
    }
}

pub(super) fn input_number(
    label: &str,
    name: &str,
    placeholder: &str,
    required: bool,
    value: &str,
) -> Markup {
    html! {
        div {
            label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for=(name) { (label) }
            input
                class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm"
                type="number"
                id=(name)
                name=(name)
                placeholder=(placeholder)
                required[required]
                value=(value);
        }
    }
}

pub(super) struct IndexFrequencyOption {
    pub(super) label: &'static str,
    pub(super) value: i64,
}

pub(super) fn index_frequency_options() -> Vec<IndexFrequencyOption> {
    vec![
        IndexFrequencyOption {
            label: "1 hour",
            value: 3600,
        },
        IndexFrequencyOption {
            label: "3 hours",
            value: 10800,
        },
        IndexFrequencyOption {
            label: "6 hours",
            value: 21600,
        },
        IndexFrequencyOption {
            label: "12 hours",
            value: 43200,
        },
        IndexFrequencyOption {
            label: "24 hours",
            value: 86400,
        },
        IndexFrequencyOption {
            label: "3 days",
            value: 259_200,
        },
        IndexFrequencyOption {
            label: "7 days",
            value: 604_800,
        },
        IndexFrequencyOption {
            label: "30 days",
            value: 2_592_000,
        },
    ]
}

pub(super) fn input_index_frequency(label: &str, name: &str, selected_value: i64) -> Markup {
    html! {
        div {
            label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for=(name) { (label) }
            select
                class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm"
                name=(name)
                id=(name)
                required {
                @for option in index_frequency_options() {
                    option value=(option.value) selected[option.value == selected_value] { (option.label) }
                }
            }
        }
    }
}

pub(super) fn input_cutoff_date(label: &str, name: &str, value: &str) -> Markup {
    html! {
        div {
            label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for=(name) { (label) }
            input
                class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm"
                type="date"
                id=(name)
                name=(name)
                required
                value=(value);
            div class="mt-2 flex flex-wrap gap-2" {
                // Offset by the viewer's timezone so "today" is their local
                // date, not the UTC date `toISOString` would otherwise yield.
                button
                    class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                    type="button"
                    onclick={"document.getElementById('" (name) "').value = new Date(Date.now() - new Date().getTimezoneOffset()*60*1000).toISOString().split('T')[0]"} {
                    "Today"
                }
                button
                    class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                    type="button"
                    onclick={"document.getElementById('" (name) "').value = new Date(Date.now() - 7*24*60*60*1000).toISOString().split('T')[0]"} {
                    "7 days"
                }
                button
                    class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                    type="button"
                    onclick={"document.getElementById('" (name) "').value = new Date(Date.now() - 14*24*60*60*1000).toISOString().split('T')[0]"} {
                    "14 days"
                }
                button
                    class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                    type="button"
                    onclick={"document.getElementById('" (name) "').value = new Date(Date.now() - 30*24*60*60*1000).toISOString().split('T')[0]"} {
                    "30 days"
                }
                button
                    class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                    type="button"
                    onclick={"document.getElementById('" (name) "').value = new Date(Date.now() - 90*24*60*60*1000).toISOString().split('T')[0]"} {
                    "90 days"
                }
                button
                    class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                    type="button"
                    onclick={"document.getElementById('" (name) "').value = new Date(Date.now() - 180*24*60*60*1000).toISOString().split('T')[0]"} {
                    "180 days"
                }
            }
        }
    }
}

pub(super) const QUALITY_WAIT_LABEL: &str = "Quality Wait (hours)";

pub(super) const QUALITY_WAIT_PLACEHOLDER: &str = "Default (QUALITY_WAIT_HOURS, 24h); 0 = off";

/// Parse the profile form's quality wait: blank means "use the server
/// default", and the column's `CHECK` rejects negatives, so catch them here
/// with a readable message instead of a database error.
pub(super) fn parse_quality_wait_hours(raw: Option<&str>) -> Result<Option<i32>, String> {
    match parse_optional_i32(raw, "Quality wait")? {
        Some(hours) if hours < 0 => Err("Quality wait must be 0 or greater".to_string()),
        value => Ok(value),
    }
}

pub(super) fn parse_optional_i32(
    raw: Option<&str>,
    field_name: &str,
) -> Result<Option<i32>, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => value
            .parse::<i32>()
            .map(Some)
            .map_err(|_| format!("{field_name} must be a valid whole number")),
    }
}

pub(crate) fn error_page(message: &str) -> Markup {
    layout(
        "Error",
        NavItem::Dashboard,
        html! {
            section class="rounded-2xl border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/30 p-6 shadow-sm" {
                h2 class="text-lg font-semibold text-rose-900 dark:text-rose-100" { "Something went wrong" }
                p class="mt-2 text-sm text-rose-800 dark:text-rose-200 break-words wrap-anywhere" { (message) }
                div class="mt-4" {
                    a class="inline-flex rounded-lg bg-rose-700 dark:bg-rose-600 px-4 py-2 text-sm font-medium text-white hover:bg-rose-800 dark:hover:bg-rose-700" href="/dashboard" {
                        "Back to dashboard"
                    }
                }
            }
        },
    )
}

/// Error markup for htmx partial endpoints.
///
/// Partials are swapped into an existing element with `innerHTML`, so they must
/// never return a full document. Returning [`error_page`] here nests a
/// `<!DOCTYPE><html><head>` inside a `<div>`, which browsers reparent — dropping
/// the stylesheet link and leaving the page unstyled.
pub(super) fn error_fragment(message: &str) -> Markup {
    html! {
        section class="rounded-2xl border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/30 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-rose-900 dark:text-rose-100" { "Something went wrong" }
            p class="mt-2 text-sm text-rose-800 dark:text-rose-200 break-words wrap-anywhere" { (message) }
        }
    }
}

/// Handler for 404 Not Found responses.
pub(super) async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, not_found_page())
}

pub(super) fn not_found_page() -> Markup {
    auth_layout(
        "Page Not Found",
        html! {
            div class="text-center" {
                div class="mb-6 text-6xl font-bold text-slate-300 dark:text-slate-600" { "404" }
                h2 class="text-xl font-semibold text-slate-900 dark:text-slate-100 mb-2" {
                    "Page Not Found"
                }
                p class="text-slate-600 dark:text-slate-400 mb-6" {
                    "The page you're looking for doesn't exist or has been moved."
                }
                a
                    href="/dashboard"
                    class="inline-flex rounded-lg bg-slate-900 dark:bg-slate-100 px-4 py-2.5 text-sm font-medium text-white dark:text-slate-900 hover:bg-slate-800 dark:hover:bg-slate-200 transition"
                {
                    "Go to Dashboard"
                }
            }
        },
    )
}

pub(super) fn layout(title: &str, active: NavItem, content: impl Render) -> Markup {
    layout_with_flash(title, active, None, content)
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub(crate) fn layout_with_flash(
    title: &str,
    active: NavItem,
    flash: Option<FlashMessage>,
    content: impl Render,
) -> Markup {
    let heading = format!("{title} · Hofvarpnir");
    html! {
        (DOCTYPE)
        // `min-h-full`, not `h-full`: the body gradient is propagated to the
        // canvas but sized to the *root* box, so pinning html to height:100%
        // capped it at one viewport and Tailwind's default background-repeat
        // tiled it down the page — a hard slate-950/slate-900 seam every 100vh.
        html lang="en" class="min-h-full" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (heading) }
                link rel="icon" type="image/x-icon" href="/assets/favicon.ico";
                link rel="icon" type="image/png" sizes="32x32" href="/assets/favicon-32x32.png";
                link rel="apple-touch-icon" href="/assets/apple-touch-icon.png";
                link rel="stylesheet" href="/assets/app.css";
                script src="https://unpkg.com/htmx.org@2.0.4" defer {}
                script src="https://unpkg.com/htmx-ext-sse@2.2.2/sse.js" defer {}
                // Dark mode initialization (runs before body renders to prevent flash)
                (PreEscaped(r"<script>
                    (function() {
                        var stored = localStorage.getItem('darkMode');
                        if (stored === 'true' || (stored === null && window.matchMedia('(prefers-color-scheme: dark)').matches)) {
                            document.documentElement.classList.add('dark');
                        }
                    })();
                </script>"))
            }
            body class="min-h-full bg-gradient-to-b from-slate-100 via-slate-50 to-white dark:from-slate-900 dark:via-slate-900 dark:to-slate-950 text-slate-900 dark:text-slate-100" {
                // Toast notification
                @if let Some(ref flash) = flash {
                    @let flash_classes = match flash.level.as_str() {
                        "success" => "bg-emerald-100 dark:bg-emerald-900/50 text-emerald-900 dark:text-emerald-100 border-emerald-200 dark:border-emerald-700",
                        "error" => "bg-rose-100 dark:bg-rose-900/50 text-rose-900 dark:text-rose-100 border-rose-200 dark:border-rose-700",
                        _ => "bg-sky-100 dark:bg-sky-900/50 text-sky-900 dark:text-sky-100 border-sky-200 dark:border-sky-700",
                    };
                    div id="toast"
                        class=(format!("fixed top-4 right-4 z-50 rounded-lg border px-4 py-3 text-sm font-medium shadow-lg transition-opacity duration-300 {flash_classes}"))
                    {
                        (flash.message)
                    }
                }

                div class="mx-auto flex min-h-screen w-full max-w-7xl flex-col px-4 py-8 sm:px-6 lg:px-8" {
                    // System issues banner (loaded via htmx on page load)
                    div
                        hx-get="/web/system-banner"
                        hx-trigger="load"
                        hx-swap="innerHTML"
                    {}

                    header class="mb-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-5 shadow-sm backdrop-blur" {
                        div class="flex flex-wrap items-center justify-between gap-4" {
                            div class="flex items-center gap-3" {
                                img src="/assets/logo.png" alt="Hofvarpnir logo" class="h-12 w-12 shrink-0";
                                div {
                                    p class="text-xs uppercase tracking-[0.2em] text-slate-500 dark:text-slate-400" { "Hofvarpnir" }
                                    h1 class="text-2xl font-semibold text-slate-900 dark:text-slate-100" { (title) }
                                }
                            }
                            nav class="flex flex-wrap items-center gap-2" {
                                (nav_link("/dashboard", "Dashboard", active == NavItem::Dashboard))
                                (nav_link("/profiles", "Profiles", active == NavItem::Profiles))
                                (nav_link("/sources", "Sources", active == NavItem::Sources))
                                (nav_link("/downloads", "Downloads", active == NavItem::Downloads))
                                (nav_link("/activity", "Activity", active == NavItem::Activity))
                                (nav_link("/schedule", "Schedule", active == NavItem::Schedule))
                                (nav_link("/settings/api-keys", "API Keys", active == NavItem::ApiKeys))
                                (nav_link("/settings/runtime", "Runtime", active == NavItem::Runtime))
                                // Dark mode toggle
                                button
                                    type="button"
                                    id="dark-toggle"
                                    class="inline-flex items-center rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1.5 text-sm font-medium text-slate-600 dark:text-slate-300 transition hover:bg-slate-200 dark:hover:bg-slate-600"
                                    onclick="toggleDarkMode()"
                                {
                                    span class="dark:hidden" { "🌙" }
                                    span class="hidden dark:inline" { "☀️" }
                                }
                                form method="post" action="/logout" class="ml-2" {
                                    button
                                        type="submit"
                                        class="inline-flex items-center rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1.5 text-sm font-medium text-slate-500 dark:text-slate-300 transition hover:bg-red-100 hover:text-red-700 dark:hover:bg-red-900/50 dark:hover:text-red-400"
                                    {
                                        "Logout"
                                    }
                                }
                            }
                        }
                    }
                    main class="flex-1" { (content) }
                }
                (PreEscaped(r"<script>
                    // Dark mode toggle
                    function toggleDarkMode() {
                      var html = document.documentElement;
                      var isDark = html.classList.toggle('dark');
                      localStorage.setItem('darkMode', isDark);
                    }

                    // Auto-dismiss toast
                    (function() {
                      var t = document.getElementById('toast');
                      if (t) {
                        setTimeout(function() { t.style.opacity = '0'; }, 3500);
                        setTimeout(function() { t.remove(); }, 4000);
                      }
                    })();

                    // Loading state on form submit. Skips a submit already
                    // cancelled (e.g. a declined confirm), which would otherwise
                    // leave the button stuck disabled on a form that never went.
                    document.addEventListener('submit', function(e) {
                      var form = e.target;
                      if (form.tagName !== 'FORM') return;
                      if (e.defaultPrevented) return;
                      var btn = e.submitter;
                      if (!btn || btn.disabled) return;
                      btn.disabled = true;
                      btn.dataset.originalText = btn.textContent;
                      btn.textContent = 'Working\u2026';
                    });

                    // A changed search term invalidates the current page offset,
                    // so snap the companion page field back to 1. Delegated so it
                    // survives htmx swaps replacing the filter bar.
                    document.addEventListener('input', function(e) {
                      var el = e.target;
                      if (!el || !el.dataset || !el.dataset.resetPage) return;
                      var field = document.getElementById(el.dataset.resetPage);
                      if (field) field.value = '1';
                    });

                    // ---- Bulk selection on the downloads table ----
                    // All handlers are delegated from document: the table is
                    // replaced wholesale by htmx and SSE, so anything bound to
                    // the elements themselves would be lost on the next swap.
                    // Selection deliberately resets on swap, since the rows (and
                    // their statuses) may no longer be the ones that were picked.
                    var BULK_ELIGIBLE = {
                      retry: ['failed', 'permanently_failed', 'cleaned'],
                      cancel: ['pending', 'awaiting_quality', 'downloading'],
                      delete: ['completed']
                    };

                    function bulkBoxes() {
                      return Array.prototype.slice.call(
                        document.querySelectorAll('[data-bulk-select]'));
                    }

                    function bulkRefresh() {
                      var form = document.querySelector('[data-bulk-form]');
                      if (!form) return;
                      var selected = bulkBoxes().filter(function(b) { return b.checked; });
                      var ids = selected.map(function(b) { return b.dataset.bulkSelect; });
                      var statuses = selected.map(function(b) { return b.dataset.status; });

                      form.hidden = ids.length === 0;
                      var idField = form.querySelector('[data-bulk-ids]');
                      if (idField) idField.value = ids.join(',');
                      var count = form.querySelector('[data-bulk-count]');
                      if (count) count.textContent = String(ids.length);

                      Object.keys(BULK_ELIGIBLE).forEach(function(action) {
                        var btn = form.querySelector('[data-bulk-button=' + action + ']');
                        if (!btn) return;
                        var ok = statuses.some(function(s) {
                          return BULK_ELIGIBLE[action].indexOf(s) !== -1;
                        });
                        btn.disabled = !ok;
                        btn.title = ok ? '' : 'No selected download is eligible';
                      });

                      var all = document.querySelector('[data-bulk-select-all]');
                      if (all) {
                        var boxes = bulkBoxes();
                        all.checked = boxes.length > 0 && selected.length === boxes.length;
                        all.indeterminate = selected.length > 0 && selected.length < boxes.length;
                      }
                    }

                    document.addEventListener('change', function(e) {
                      var el = e.target;
                      if (!el || !el.dataset) return;
                      if (el.dataset.bulkSelectAll !== undefined) {
                        bulkBoxes().forEach(function(b) { b.checked = el.checked; });
                        bulkRefresh();
                      } else if (el.dataset.bulkSelect !== undefined) {
                        bulkRefresh();
                      }
                    });

                    document.addEventListener('click', function(e) {
                      var el = e.target.closest ? e.target.closest('[data-bulk-clear]') : null;
                      if (!el) return;
                      e.preventDefault();
                      bulkBoxes().forEach(function(b) { b.checked = false; });
                      var all = document.querySelector('[data-bulk-select-all]');
                      if (all) { all.checked = false; all.indeterminate = false; }
                      bulkRefresh();
                    });

                    // Record which button submitted, and confirm destructive ones.
                    // `submitter.value` is not sent for us because the action is
                    // carried in a hidden field the server reads.
                    // Capture phase so this runs before the loading-state handler
                    // above and can cancel the submit cleanly.
                    document.addEventListener('submit', function(e) {
                      var form = e.target;
                      if (!form.dataset || form.dataset.bulkForm === undefined) return;
                      var btn = e.submitter;
                      if (!btn) return;
                      if (btn.dataset.confirm && !confirm(btn.dataset.confirm)) {
                        e.preventDefault();
                        return;
                      }
                      var field = form.querySelector('[data-bulk-action]');
                      if (field) field.value = btn.value;
                    }, true);

                    document.body.addEventListener('htmx:afterSwap', bulkRefresh);
                    bulkRefresh();
                    </script>"))
            }
        }
    }
}

pub(super) fn nav_link(href: &str, label: &str, selected: bool) -> Markup {
    let classes = if selected {
        "inline-flex items-center rounded-full bg-slate-900 dark:bg-slate-100 px-3 py-1.5 text-sm font-medium text-white dark:text-slate-900"
    } else {
        "inline-flex items-center rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1.5 text-sm font-medium text-slate-700 dark:text-slate-200 transition hover:bg-slate-200 dark:hover:bg-slate-600"
    };

    html! {
        a class=(classes) href=(href) { (label) }
    }
}

#[cfg(test)]
mod input_cutoff_date_tests {
    use super::input_cutoff_date;

    /// The "Today" shortcut is a convenience only — the field itself must
    /// still render pre-filled with whatever default the caller passed
    /// (a week ago, for the create form).
    #[test]
    fn renders_today_pill_without_changing_the_prefilled_value() {
        let markup = input_cutoff_date("Cutoff Date", "cutoff_date", "2026-07-24").into_string();

        assert!(
            markup.contains(">Today<"),
            "expected a Today pill: {markup}"
        );
        assert!(
            markup.contains(r#"value="2026-07-24""#),
            "expected the passed-in default to stay pre-filled: {markup}"
        );
        assert!(
            markup.contains("getTimezoneOffset"),
            "Today must resolve to the viewer's local date, not the UTC date: {markup}"
        );
    }
}
