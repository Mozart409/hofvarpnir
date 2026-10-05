//! Mirror types for the Hofvarpnir REST API (`hof-api`).
//!
//! The TUI talks to the server over HTTP only (see GOALS.md: "hof-tui depends
//! only on reqwest"), so these types re-declare the API's wire format. Enum
//! variant names must match `hof-core`'s serde output exactly: the domain
//! enums use `rename_all` only for sqlx, never for serde, so JSON carries the
//! bare variant name (`"Q1080p"`, `"PermanentlyFailed"`, ...).
//!
//! Fields are `#[serde(default)]` where plausible so an older server that
//! predates a field still deserializes.

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// Download quality preset requested by a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Quality {
    Best,
    Q4320p,
    Q2160p,
    Q1440p,
    Q1080p,
    Q720p,
    Q480p,
    AudioOnly,
}

impl Quality {
    /// Short human label for table cells.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Best => "best",
            Self::Q4320p => "4320p",
            Self::Q2160p => "2160p",
            Self::Q1440p => "1440p",
            Self::Q1080p => "1080p",
            Self::Q720p => "720p",
            Self::Q480p => "480p",
            Self::AudioOnly => "audio",
        }
    }
}

/// Output preset governing codec/container strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum OutputPreset {
    Auto,
    Browser,
    Tv,
}

impl OutputPreset {
    /// Short human label for table cells.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Browser => "browser",
            Self::Tv => "tv",
        }
    }
}

/// Download lifecycle status of a video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum VideoStatus {
    Pending,
    Downloading,
    Completed,
    Failed,
    Skipped,
    Cleaned,
    PermanentlyFailed,
}

impl VideoStatus {
    /// Short human label for table cells.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Downloading => "downloading",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Cleaned => "cleaned",
            Self::PermanentlyFailed => "perm-failed",
        }
    }

    /// Serde wire name, used as the `status` query parameter value.
    ///
    /// Must match the variant name: `hof-api` deserializes the query string
    /// with serde, and the domain enum carries no serde rename.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Downloading => "Downloading",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Skipped => "Skipped",
            Self::Cleaned => "Cleaned",
            Self::PermanentlyFailed => "PermanentlyFailed",
        }
    }
}

/// Whether a source is a channel or a playlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum SourceType {
    Channel,
    Playlist,
}

impl SourceType {
    /// Short human label for table cells.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Channel => "channel",
            Self::Playlist => "playlist",
        }
    }
}

/// Detected ordering of entries in a source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum EntryOrder {
    #[default]
    Unknown,
    Ascending,
    Descending,
    Unordered,
}

impl EntryOrder {
    /// Short human label for table cells.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Ascending => "asc",
            Self::Descending => "desc",
            Self::Unordered => "unordered",
        }
    }
}

/// Severity of an activity event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ActivitySeverity {
    Info,
    Success,
    Warning,
    Error,
}

impl ActivitySeverity {
    /// Short human label for table cells.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Success => "ok",
            Self::Warning => "warn",
            Self::Error => "error",
        }
    }
}

/// Type of an activity event. Unknown variants degrade to `Other` so a newer
/// server never breaks the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ActivityEventType {
    SourceIndexed,
    SourceError,
    DownloadStarted,
    DownloadCompleted,
    DownloadFailed,
    RetryScheduled,
    MetadataGenerated,
    VideoCleaned,
    ProfileCreated,
    ProfileUpdated,
    ProfileDeleted,
    SourceCreated,
    SourceUpdated,
    SourceDeleted,
    SelfRestart,
    #[serde(other)]
    Other,
}

impl ActivityEventType {
    /// Short human label for table cells.
    pub const fn label(self) -> &'static str {
        match self {
            Self::SourceIndexed => "source_indexed",
            Self::SourceError => "source_error",
            Self::DownloadStarted => "dl_started",
            Self::DownloadCompleted => "dl_completed",
            Self::DownloadFailed => "dl_failed",
            Self::RetryScheduled => "retry_scheduled",
            Self::MetadataGenerated => "metadata",
            Self::VideoCleaned => "video_cleaned",
            Self::ProfileCreated => "profile_created",
            Self::ProfileUpdated => "profile_updated",
            Self::ProfileDeleted => "profile_deleted",
            Self::SourceCreated => "source_created",
            Self::SourceUpdated => "source_updated",
            Self::SourceDeleted => "source_deleted",
            Self::SelfRestart => "self_restart",
            Self::Other => "other",
        }
    }
}

/// `GET /api/v1/downloads` item.
#[derive(Debug, Clone, Deserialize)]
pub struct VideoResponse {
    pub id: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub platform_video_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<i64>,
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
    pub status: VideoStatus,
    #[serde(default)]
    pub attempts: i32,
    #[serde(default)]
    pub next_retry: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_error_code: Option<String>,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub file_size_bytes: Option<i64>,
    #[serde(default)]
    pub video_height: Option<i32>,
    #[serde(default)]
    pub video_codec: Option<String>,
    #[serde(default)]
    pub downloaded_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub source_display_name: Option<String>,
    #[serde(default)]
    pub profile_name: Option<String>,
    #[serde(default)]
    pub profile_quality: Option<Quality>,
    #[serde(default)]
    pub profile_output_preset: Option<OutputPreset>,
}

/// `GET /api/v1/profiles` item.
#[derive(Debug, Clone, Deserialize)]
pub struct ProfileResponse {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub quality: Option<Quality>,
    #[serde(default)]
    pub output_preset: Option<OutputPreset>,
    #[serde(default)]
    pub naming_template: String,
    #[serde(default)]
    pub output_dir: String,
    #[serde(default)]
    pub include_livestreams: bool,
    #[serde(default)]
    pub include_shorts: bool,
    #[serde(default)]
    pub storage_quota_bytes: i64,
    #[serde(default)]
    pub retention_days: Option<i32>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
}

/// `GET /api/v1/sources` item.
#[derive(Debug, Clone, Deserialize)]
pub struct SourceResponse {
    pub id: String,
    pub profile_id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub source_type: Option<SourceType>,
    #[serde(default)]
    pub custom_name: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub exclude_from_cleanup: bool,
    #[serde(default)]
    pub index_frequency_secs: i64,
    #[serde(default)]
    pub cutoff_date: String,
    #[serde(default)]
    pub retention_days: Option<i32>,
    #[serde(default)]
    pub entry_order: EntryOrder,
    #[serde(default)]
    pub last_indexed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
}

/// `GET /api/v1/activity` item.
#[derive(Debug, Clone, Deserialize)]
pub struct ActivityEventResponse {
    pub id: String,
    #[serde(default)]
    pub event_type: Option<ActivityEventType>,
    #[serde(default)]
    pub severity: Option<ActivitySeverity>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
}

/// `GET /api/v1/activity` envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct ActivityListResponse {
    #[serde(default)]
    pub events: Vec<ActivityEventResponse>,
    #[serde(default)]
    pub total: i64,
}

/// Pause state of one module.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct PauseStateResponse {
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub indefinite: bool,
}

/// Pause state for both gated modules.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct PauseSummaryResponse {
    pub indexing: PauseStateResponse,
    pub downloads: PauseStateResponse,
}

impl PauseSummaryResponse {
    /// Whether anything is currently paused (drives the status-bar badge and
    /// the pause/resume toggle).
    pub const fn any_paused(&self) -> bool {
        self.indexing.paused || self.downloads.paused
    }
}

/// `GET /api/v1/system/status` — the parts the TUI renders.
#[derive(Debug, Clone, Deserialize)]
pub struct SystemStatusResponse {
    #[serde(default)]
    pub scheduler: Option<SchedulerStatusResponse>,
    #[serde(default)]
    pub downloads: Option<DownloadsStatusResponse>,
    #[serde(default)]
    pub statistics: Option<StatisticsResponse>,
    #[serde(default)]
    pub pause: Option<PauseSummaryResponse>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct SchedulerStatusResponse {
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub active_indexers: usize,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct DownloadsStatusResponse {
    #[serde(default)]
    pub supervisor_reachable: bool,
    #[serde(default)]
    pub active_downloads: Option<usize>,
    #[serde(default)]
    pub dispatching: Option<usize>,
    #[serde(default)]
    pub max_concurrent_downloads: u32,
    #[serde(default)]
    pub db_backoff_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct StatisticsResponse {
    #[serde(default)]
    pub total_videos: i64,
    #[serde(default)]
    pub pending_downloads: i64,
    #[serde(default)]
    pub downloading: i64,
    #[serde(default)]
    pub completed: i64,
    #[serde(default)]
    pub failed: i64,
    #[serde(default)]
    pub permanently_failed: i64,
}

/// SSE payload from `GET /api/v1/downloads/progress` (`event: progress`).
#[derive(Debug, Clone, Deserialize)]
pub struct ProgressEvent {
    pub video_id: String,
    #[serde(default)]
    pub percent: f64,
    #[serde(default)]
    pub speed: Option<String>,
    #[serde(default)]
    pub eta: Option<String>,
    #[serde(default)]
    pub downloaded_bytes: Option<u64>,
    #[serde(default)]
    pub total_bytes: Option<u64>,
}

/// Error body returned by `hof-api` (`ApiErrorResponse` / `ErrorResponse`).
#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorResponse {
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub message: String,
}

/// Permission scope of an API key (`hof_core::domain::api_key::ApiKeyScope`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ApiKeyScope {
    Read,
    Write,
    Delete,
}

impl ApiKeyScope {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Delete => "delete",
        }
    }
}

/// How the caller authenticated (`GET /api/v1/system/whoami`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Session,
    ApiKey,
}

/// Response of `GET /api/v1/system/whoami`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WhoAmIResponse {
    pub auth_method: AuthMethod,
    #[serde(default)]
    pub scopes: Vec<ApiKeyScope>,
}

impl WhoAmIResponse {
    #[must_use]
    pub fn has(&self, scope: ApiKeyScope) -> bool {
        self.scopes.contains(&scope)
    }

    /// Scopes joined in canonical order, e.g. `read+write+delete`.
    #[must_use]
    pub fn label(&self) -> String {
        let parts: Vec<&str> = [ApiKeyScope::Read, ApiKeyScope::Write, ApiKeyScope::Delete]
            .into_iter()
            .filter(|s| self.has(*s))
            .map(ApiKeyScope::label)
            .collect();
        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join("+")
        }
    }
}

/// Which layer supplied a resolved setting (`hof_core::runtime_config`).
/// Precedence is database > env > default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    Default,
    Env,
    Database,
}

impl Provenance {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Env => "env",
            Self::Database => "database",
        }
    }
}

/// One resolved knob. The server sends `u32` for counts and `u64` for
/// seconds; both fit `u64`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct ResolvedValue {
    pub value: u64,
    pub provenance: Provenance,
}

/// Response of `GET`/`PATCH /api/v1/system/settings`.
#[derive(Debug, Clone, Deserialize)]
pub struct SettingsResponse {
    pub pause: PauseSummaryResponse,
    pub max_concurrent_downloads: ResolvedValue,
    pub max_indexers_per_tick: ResolvedValue,
    pub rate_limit_delay_secs: ResolvedValue,
    pub check_interval_secs: ResolvedValue,
    pub cleanup_interval_secs: ResolvedValue,
    pub drain_timeout_secs: ResolvedValue,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updated_by: Option<String>,
}

/// Target of the pause/resume endpoints (`?module=` / `{"module": ...}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseModule {
    Indexing,
    Downloads,
    All,
}

impl PauseModule {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Indexing => "indexing",
            Self::Downloads => "downloads",
            Self::All => "all",
        }
    }
}
