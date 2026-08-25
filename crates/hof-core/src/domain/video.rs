use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use utoipa::ToSchema;

use crate::domain::profile::{OutputPreset, Quality};
use crate::domain::source::SourceType;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
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
    /// Height of the video stream that was actually delivered, in pixels.
    ///
    /// Reflects what the platform served, which can be lower than the profile's
    /// requested quality. `None` for downloads predating this field.
    pub video_height: Option<i32>,
    /// Codec of the delivered video stream, e.g. `av01.0.12M.08`.
    pub video_codec: Option<String>,
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
    pub video_height: Option<i32>,
    pub video_codec: Option<String>,
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
            video_height: row.video_height,
            video_codec: row.video_codec,
            downloaded_at: row.downloaded_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

/// A video enriched with context about the source it was downloaded from and
/// the download profile governing that source.
///
/// Used by the downloads API to expose genuinely useful context (channel
/// URL, custom name, quality preset, ...) alongside each video without
/// issuing a separate query per row. When a video has no linked source (or
/// the source/profile was deleted), all `source_*`/`profile_*` fields are
/// `None`.
#[derive(Debug, Clone)]
pub struct VideoContext {
    pub video: Video,
    /// ID of the (first-linked) source this video came from.
    pub source_id: Option<Ulid>,
    /// The source's URL (channel/playlist URL).
    pub source_url: Option<String>,
    pub source_type: Option<SourceType>,
    /// User-provided custom name for the source, if set.
    pub source_custom_name: Option<String>,
    /// Platform-specific channel ID (e.g. `YouTube` channel ID).
    pub source_channel_id: Option<String>,
    /// Channel title as reported by the platform.
    pub source_channel_title: Option<String>,
    /// URL to the channel's thumbnail/avatar image.
    pub source_channel_thumbnail_url: Option<String>,
    pub profile_id: Option<Ulid>,
    pub profile_name: Option<String>,
    pub profile_quality: Option<Quality>,
    pub profile_output_preset: Option<OutputPreset>,
}

impl VideoContext {
    /// The effective display name for the linked source, mirroring
    /// `Source::display_name()`: prefers `custom_name`, falls back to
    /// `channel_title`, then to the source URL. Returns `None` when the
    /// video has no linked source.
    #[must_use]
    pub fn source_display_name(&self) -> Option<&str> {
        self.source_custom_name
            .as_deref()
            .or(self.source_channel_title.as_deref())
            .or(self.source_url.as_deref())
    }
}

/// Database row for a video plus its (first-linked) source and profile context.
#[derive(Debug, sqlx::FromRow)]
pub struct VideoContextRow {
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
    pub video_height: Option<i32>,
    pub video_codec: Option<String>,
    pub downloaded_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub source_id: Option<String>,
    pub source_url: Option<String>,
    pub source_type: Option<SourceType>,
    pub source_custom_name: Option<String>,
    pub source_channel_id: Option<String>,
    pub source_channel_title: Option<String>,
    pub source_channel_thumbnail_url: Option<String>,
    pub profile_id: Option<String>,
    pub profile_name: Option<String>,
    pub profile_quality: Option<Quality>,
    pub profile_output_preset: Option<OutputPreset>,
}

impl TryFrom<VideoContextRow> for VideoContext {
    type Error = ulid::DecodeError;

    fn try_from(row: VideoContextRow) -> Result<Self, Self::Error> {
        let video = Video::try_from(VideoRow {
            id: row.id,
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
            video_height: row.video_height,
            video_codec: row.video_codec,
            downloaded_at: row.downloaded_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })?;

        let source_id = row.source_id.map(|id| Ulid::from_string(&id)).transpose()?;
        let profile_id = row
            .profile_id
            .map(|id| Ulid::from_string(&id))
            .transpose()?;

        Ok(Self {
            video,
            source_id,
            source_url: row.source_url,
            source_type: row.source_type,
            source_custom_name: row.source_custom_name,
            source_channel_id: row.source_channel_id,
            source_channel_title: row.source_channel_title,
            source_channel_thumbnail_url: row.source_channel_thumbnail_url,
            profile_id,
            profile_name: row.profile_name,
            profile_quality: row.profile_quality,
            profile_output_preset: row.profile_output_preset,
        })
    }
}

/// A completed video together with its computed retention deletion schedule.
///
/// Used by the "pending deletion" listing to preview which videos the
/// `CleanupActor` will remove and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoPendingDeletion {
    pub video: Video,
    /// When the video is scheduled to be deleted (latest expiry across all
    /// referencing sources).
    pub scheduled_deletion_at: DateTime<Utc>,
    /// The effective retention in days that governs the scheduled deletion.
    pub effective_retention_days: i32,
}

/// Database row for a video plus its computed retention deletion schedule.
#[derive(Debug, sqlx::FromRow)]
pub struct VideoPendingDeletionRow {
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
    pub video_height: Option<i32>,
    pub video_codec: Option<String>,
    pub downloaded_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub scheduled_deletion_at: DateTime<Utc>,
    pub effective_retention_days: i32,
}

impl TryFrom<VideoPendingDeletionRow> for VideoPendingDeletion {
    type Error = ulid::DecodeError;

    fn try_from(row: VideoPendingDeletionRow) -> Result<Self, Self::Error> {
        let scheduled_deletion_at = row.scheduled_deletion_at;
        let effective_retention_days = row.effective_retention_days;
        let video = Video::try_from(VideoRow {
            id: row.id,
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
            video_height: row.video_height,
            video_codec: row.video_codec,
            downloaded_at: row.downloaded_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })?;

        Ok(Self {
            video,
            scheduled_deletion_at,
            effective_retention_days,
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
