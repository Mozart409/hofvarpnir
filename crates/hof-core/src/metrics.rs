//! Application metrics using the `metrics` crate with a Prometheus exporter.
//!
//! Metrics are recorded throughout the application using the `metrics` macros
//! (`counter!`, `gauge!`, `histogram!`). The Prometheus exporter collects them
//! and renders text for the `/metrics` HTTP endpoint.

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Initialize the Prometheus metrics recorder and return a handle for rendering.
///
/// The handle's [`PrometheusHandle::render`] method produces the Prometheus
/// exposition format text to serve at `/metrics`.
///
/// # Panics
///
/// Panics if a global metrics recorder has already been installed.
pub fn init_metrics() -> PrometheusHandle {
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    metrics::set_global_recorder(recorder).expect("failed to install Prometheus metrics recorder");
    handle
}

// ---------------------------------------------------------------------------
// Metric name constants
// ---------------------------------------------------------------------------

// HTTP
pub const HTTP_REQUESTS_TOTAL: &str = "http_requests_total";
pub const HTTP_REQUEST_DURATION_SECONDS: &str = "http_request_duration_seconds";

// Downloads
pub const DOWNLOADS_ACTIVE: &str = "downloads_active";
pub const DOWNLOADS_COMPLETED_TOTAL: &str = "downloads_completed_total";
pub const DOWNLOADS_FAILED_TOTAL: &str = "downloads_failed_total";
pub const DOWNLOAD_DURATION_SECONDS: &str = "download_duration_seconds";

// Source indexing
pub const SOURCE_INDEX_TOTAL: &str = "source_index_total";
pub const SOURCE_INDEX_DURATION_SECONDS: &str = "source_index_duration_seconds";
pub const SOURCE_INDEX_NEW_VIDEOS: &str = "source_index_new_videos";

// Cleanup
pub const VIDEOS_CLEANED_TOTAL: &str = "videos_cleaned_total";
pub const CLEANUP_BYTES_FREED: &str = "cleanup_bytes_freed";
pub const CLEANUP_TEMP_FILES_REMOVED_TOTAL: &str = "cleanup_temp_files_removed_total";
