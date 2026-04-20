use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "source_type", rename_all = "lowercase")]
pub enum SourceType {
    Channel,
    Playlist,
}

/// Detected ordering of entries in a playlist/channel.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema,
)]
#[sqlx(type_name = "entry_order", rename_all = "lowercase")]
pub enum EntryOrder {
    /// Not yet checked — trigger detection on next index.
    #[default]
    Unknown,
    /// Oldest entries first.
    Ascending,
    /// Newest entries first.
    Descending,
    /// No consistent order detected — requires full scan.
    Unordered,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: Ulid,
    pub profile_id: Ulid,
    pub url: String,
    pub source_type: SourceType,
    pub custom_name: Option<String>,
    /// Whether this source is enabled for indexing and downloading.
    pub enabled: bool,
    /// How often to check for new videos, stored as seconds.
    pub index_frequency_secs: i64,
    /// Ignore videos published before this date.
    pub cutoff_date: NaiveDate,
    /// Per-source retention override (days).
    pub retention_days: Option<i32>,
    /// Detected ordering of entries in this source.
    pub entry_order: EntryOrder,
    /// When entry order was last detected.
    pub entry_order_detected_at: Option<DateTime<Utc>>,
    pub last_indexed_at: Option<DateTime<Utc>>,
    /// Last error encountered during indexing.
    pub last_error: Option<String>,
    /// Number of consecutive indexing errors.
    pub index_error_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    // Channel metadata for Jellyfin integration
    /// Platform-specific channel/playlist ID (e.g., `YouTube` channel ID).
    pub channel_id: Option<String>,
    /// Channel title from the platform.
    pub channel_title: Option<String>,
    /// Channel description from the platform.
    pub channel_description: Option<String>,
    /// URL to the channel's thumbnail/avatar image.
    pub channel_thumbnail_url: Option<String>,
    /// When Jellyfin metadata (NFO, images) was last generated.
    pub jellyfin_metadata_at: Option<DateTime<Utc>>,
}

/// Database row representation for Source (with String ids).
#[derive(Debug, sqlx::FromRow)]
pub struct SourceRow {
    pub id: String,
    pub profile_id: String,
    pub url: String,
    pub source_type: SourceType,
    pub custom_name: Option<String>,
    pub enabled: bool,
    pub index_frequency_secs: i64,
    pub cutoff_date: NaiveDate,
    pub retention_days: Option<i32>,
    pub entry_order: EntryOrder,
    pub entry_order_detected_at: Option<DateTime<Utc>>,
    pub last_indexed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub index_error_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    // Channel metadata
    pub channel_id: Option<String>,
    pub channel_title: Option<String>,
    pub channel_description: Option<String>,
    pub channel_thumbnail_url: Option<String>,
    pub jellyfin_metadata_at: Option<DateTime<Utc>>,
}

impl TryFrom<SourceRow> for Source {
    type Error = ulid::DecodeError;

    fn try_from(row: SourceRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Ulid::from_string(&row.id)?,
            profile_id: Ulid::from_string(&row.profile_id)?,
            url: row.url,
            source_type: row.source_type,
            custom_name: row.custom_name,
            enabled: row.enabled,
            index_frequency_secs: row.index_frequency_secs,
            cutoff_date: row.cutoff_date,
            retention_days: row.retention_days,
            entry_order: row.entry_order,
            entry_order_detected_at: row.entry_order_detected_at,
            last_indexed_at: row.last_indexed_at,
            last_error: row.last_error,
            index_error_count: row.index_error_count,
            created_at: row.created_at,
            updated_at: row.updated_at,
            channel_id: row.channel_id,
            channel_title: row.channel_title,
            channel_description: row.channel_description,
            channel_thumbnail_url: row.channel_thumbnail_url,
            jellyfin_metadata_at: row.jellyfin_metadata_at,
        })
    }
}

impl Source {
    /// Returns the display name for this source.
    /// Prefers `custom_name`, falls back to `channel_title`, then URL.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.custom_name
            .as_deref()
            .or(self.channel_title.as_deref())
            .unwrap_or(&self.url)
    }

    /// Returns the completed output directory for this source.
    ///
    /// Path: `{output_dir}/completed/{sanitized_source_name}/`
    #[must_use]
    pub fn completed_dir(&self, output_dir: &str) -> PathBuf {
        use crate::ytdlp::sanitize_filename_component;

        Path::new(output_dir)
            .join("completed")
            .join(sanitize_filename_component(self.display_name()))
    }
}
