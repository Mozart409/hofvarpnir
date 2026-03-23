use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use utoipa::ToSchema;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "activity_severity", rename_all = "lowercase")]
pub enum ActivitySeverity {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::Type, ToSchema)]
#[sqlx(type_name = "activity_event_type", rename_all = "snake_case")]
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
    SourceDeleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub id: Ulid,
    pub event_type: ActivityEventType,
    pub severity: ActivitySeverity,
    pub message: String,
    pub source_id: Option<Ulid>,
    pub video_id: Option<Ulid>,
    pub profile_id: Option<Ulid>,
    pub created_at: DateTime<Utc>,
}

/// Database row representation for `ActivityEvent` (with String ids).
#[derive(Debug, sqlx::FromRow)]
pub struct ActivityEventRow {
    pub id: String,
    pub event_type: ActivityEventType,
    pub severity: ActivitySeverity,
    pub message: String,
    pub source_id: Option<String>,
    pub video_id: Option<String>,
    pub profile_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl TryFrom<ActivityEventRow> for ActivityEvent {
    type Error = ulid::DecodeError;

    fn try_from(row: ActivityEventRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: Ulid::from_string(&row.id)?,
            event_type: row.event_type,
            severity: row.severity,
            message: row.message,
            source_id: row
                .source_id
                .as_deref()
                .map(Ulid::from_string)
                .transpose()?,
            video_id: row.video_id.as_deref().map(Ulid::from_string).transpose()?,
            profile_id: row
                .profile_id
                .as_deref()
                .map(Ulid::from_string)
                .transpose()?,
            created_at: row.created_at,
        })
    }
}
