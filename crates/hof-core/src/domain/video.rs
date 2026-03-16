use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "video_status", rename_all = "snake_case")]
pub enum VideoStatus {
    Pending,
    Downloading,
    Completed,
    Failed,
    Skipped,
    Cleaned,
    PermanentlyFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Video {
    pub id: Ulid,
    /// yt-dlp extractor name (e.g. "youtube", "vimeo", "twitter")
    pub platform: String,
    pub platform_video_id: String,
    pub title: String,
    pub description: Option<String>,
    pub duration_secs: Option<i64>,
    pub published_at: Option<DateTime<Utc>>,
    pub thumbnail_url: Option<String>,
    pub status: VideoStatus,
    pub attempts: i32,
    pub next_retry: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub file_path: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub downloaded_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Progress data emitted during a download.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub video_id: Ulid,
    pub platform_video_id: String,
    pub percent: f64,
    pub speed: Option<String>,
    pub eta: Option<String>,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}
