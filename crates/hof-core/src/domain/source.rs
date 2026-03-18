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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Last error encountered during indexing.
    pub last_error: Option<String>,
    /// Number of consecutive indexing errors.
    pub index_error_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Database row representation for Source (with String ids).
#[derive(Debug, sqlx::FromRow)]
pub struct SourceRow {
    pub id: String,
    pub profile_id: String,
    pub url: String,
    pub source_type: SourceType,
    pub custom_name: Option<String>,
    pub index_frequency_secs: i64,
    pub cutoff_date: NaiveDate,
    pub retention_days: Option<i32>,
    pub last_indexed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub index_error_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
            index_frequency_secs: row.index_frequency_secs,
            cutoff_date: row.cutoff_date,
            retention_days: row.retention_days,
            last_indexed_at: row.last_indexed_at,
            last_error: row.last_error,
            index_error_count: row.index_error_count,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}
