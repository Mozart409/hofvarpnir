use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "source_type", rename_all = "lowercase")]
pub enum SourceType {
    Channel,
    Playlist,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Source {
    pub id: Ulid,
    pub profile_id: Ulid,
    pub url: String,
    pub source_type: SourceType,
    pub custom_name: Option<String>,
    /// How often to check for new videos, stored as seconds.
    pub index_frequency_secs: i64,
    /// Ignore videos published before this date.
    pub cutoff_date: NaiveDate,
    /// Per-source retention override (days).
    pub retention_days: Option<i32>,
    pub last_indexed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
