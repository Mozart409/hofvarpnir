use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Database row representation for Video (with String id).
#[derive(Debug, sqlx::FromRow)]
pub struct VideoRow {
    pub id: String,
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

impl TryFrom<VideoRow> for Video {
    type Error = ulid::DecodeError;

    fn try_from(row: VideoRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Ulid::from_string(&row.id)?,
            platform: row.platform,
            platform_video_id: row.platform_video_id,
            title: row.title,
            description: row.description,
            duration_secs: row.duration_secs,
            published_at: row.published_at,
            thumbnail_url: row.thumbnail_url,
            status: row.status,
            attempts: row.attempts,
            next_retry: row.next_retry,
            last_error: row.last_error,
            file_path: row.file_path,
            file_size_bytes: row.file_size_bytes,
            downloaded_at: row.downloaded_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
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
