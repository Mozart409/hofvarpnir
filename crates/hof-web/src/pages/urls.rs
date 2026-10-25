//! Split out of `pages/mod.rs`.

use super::*;

/// Characters that must be escaped inside a query-string value.
///
/// Beyond the standard non-ASCII/control set, `&` and `#` would terminate the
/// value and `+`/space would be decoded as a space by form parsers.
pub(super) const QUERY_VALUE_ENCODE_SET: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'_')
        .remove(b'.')
        .remove(b'~');

pub(super) const fn parse_downloads_page_size(value: Option<i64>) -> i64 {
    match value {
        Some(size) if matches!(size, 25 | 50 | 100) => size,
        _ => 25,
    }
}

/// The canonical slug for a status, matching [`parse_download_status`].
///
/// Rendered into `data-status` so the bulk toolbar can decide client-side
/// which actions apply to the current selection.
pub(super) const fn download_status_slug(status: &VideoStatus) -> &'static str {
    match status {
        VideoStatus::Pending => "pending",
        VideoStatus::AwaitingQuality => "awaiting_quality",
        VideoStatus::Downloading => "downloading",
        VideoStatus::Completed => "completed",
        VideoStatus::Failed => "failed",
        VideoStatus::Skipped => "skipped",
        VideoStatus::Cleaned => "cleaned",
        VideoStatus::PermanentlyFailed => "permanently_failed",
    }
}

pub(super) fn parse_download_status(status: Option<&str>) -> Option<VideoStatus> {
    status.and_then(|s| match s {
        "pending" => Some(VideoStatus::Pending),
        "awaiting_quality" => Some(VideoStatus::AwaitingQuality),
        "downloading" => Some(VideoStatus::Downloading),
        "completed" => Some(VideoStatus::Completed),
        "failed" => Some(VideoStatus::Failed),
        "skipped" => Some(VideoStatus::Skipped),
        "cleaned" => Some(VideoStatus::Cleaned),
        "permanently_failed" => Some(VideoStatus::PermanentlyFailed),
        _ => None,
    })
}

pub(super) fn normalized_download_status(status: Option<&str>) -> &str {
    match status {
        Some(
            value @ ("pending" | "awaiting_quality" | "downloading" | "completed" | "failed"
            | "skipped" | "cleaned" | "permanently_failed"),
        ) => value,
        _ => "all",
    }
}

pub(super) fn downloads_page_url(
    status: Option<&str>,
    search: Option<&str>,
    page: i64,
    per_page: i64,
) -> String {
    downloads_url("/downloads", status, search, page, per_page)
}

pub(super) fn downloads_list_url(
    status: Option<&str>,
    search: Option<&str>,
    page: i64,
    per_page: i64,
) -> String {
    downloads_url("/web/downloads/list", status, search, page, per_page)
}

pub(super) fn downloads_events_url(
    status: Option<&str>,
    search: Option<&str>,
    page: i64,
    per_page: i64,
) -> String {
    downloads_url("/web/downloads/events", status, search, page, per_page)
}

/// Percent-encode a value for use in a query string.
///
/// Search terms are user-supplied and routinely contain spaces and `&`, which
/// would otherwise truncate the URL or inject a spurious parameter.
pub(super) fn encode_query_value(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, QUERY_VALUE_ENCODE_SET).to_string()
}

/// Rebuild the `/downloads` URL a mutating action should return to.
///
/// Retry/cancel/delete carry the list state (status, search, page, `per_page`) as
/// query params on their form action, so the post-redirect lands back on the row
/// the user acted from instead of resetting to page 1 with no filters.
pub(super) fn downloads_return_url(query: &DownloadsQuery) -> String {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = parse_downloads_page_size(query.per_page);
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    downloads_page_url(query.status.as_deref(), search, page, per_page)
}

pub(super) fn downloads_url(
    base: &str,
    status: Option<&str>,
    search: Option<&str>,
    page: i64,
    per_page: i64,
) -> String {
    let mut params = Vec::new();

    if let Some(value) = status.filter(|value| *value != "all") {
        params.push(format!("status={value}"));
    }
    if let Some(value) = search.filter(|value| !value.is_empty()) {
        params.push(format!("search={}", encode_query_value(value)));
    }
    if page > 1 {
        params.push(format!("page={page}"));
    }
    if per_page != 25 {
        params.push(format!("per_page={per_page}"));
    }

    if params.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", params.join("&"))
    }
}
