use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "quality", rename_all = "lowercase")]
pub enum Quality {
    Best,
    #[sqlx(rename = "4320p")]
    Q4320p,
    #[sqlx(rename = "2160p")]
    Q2160p,
    #[sqlx(rename = "1440p")]
    Q1440p,
    #[sqlx(rename = "1080p")]
    Q1080p,
    #[sqlx(rename = "720p")]
    Q720p,
    #[sqlx(rename = "480p")]
    Q480p,
    AudioOnly,
}

/// A download configuration that can apply to sources from any platform.
/// yt-dlp auto-detects the platform from each source URL.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Profile {
    pub id: Ulid,
    pub user_id: Ulid,
    pub name: String,
    pub quality: Quality,
    pub naming_template: String,
    pub output_dir: String,
    pub include_livestreams: bool,
    pub include_shorts: bool,
    pub storage_quota_bytes: i64,
    pub retention_days: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
