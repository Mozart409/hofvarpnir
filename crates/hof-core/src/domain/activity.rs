use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use utoipa::ToSchema;

const SOURCE_INDEXED_PREFIX: &str = "Indexed successfully — ";

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
    SourceUpdated,
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

/// Structured indexing summary extracted from a `SourceIndexed` activity message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SourceIndexingSummary {
    pub new_videos: usize,
    pub existing_videos: usize,
    pub filtered_total: usize,
    pub filtered_before_cutoff: usize,
    pub filtered_shorts: usize,
    pub filtered_livestreams: usize,
    pub filtered_unavailable: usize,
    pub filtered_private: usize,
    pub filtered_other: usize,
}

impl ActivityEvent {
    /// Parse `SourceIndexed` event message into a structured summary.
    #[must_use]
    pub fn source_indexing_summary(&self) -> Option<SourceIndexingSummary> {
        if self.event_type != ActivityEventType::SourceIndexed {
            return None;
        }

        parse_source_indexing_summary(&self.message)
    }
}

#[must_use]
pub fn parse_source_indexing_summary(message: &str) -> Option<SourceIndexingSummary> {
    let rest = message.strip_prefix(SOURCE_INDEXED_PREFIX)?;
    let (counts, breakdown_raw) = rest.split_once(" (")?;
    let breakdown = breakdown_raw.strip_suffix(')')?;

    let (new_videos, existing_videos, filtered_total) = parse_primary_counts(counts)?;

    let mut filtered_before_cutoff = None;
    let mut filtered_shorts = None;
    let mut filtered_livestreams = None;
    let mut filtered_unavailable = None;
    let mut filtered_private = None;
    let mut filtered_other = None;

    for part in breakdown.split(", ") {
        let (key, value) = part.split_once('=')?;
        let parsed = value.parse().ok()?;
        match key {
            "cutoff" => filtered_before_cutoff = Some(parsed),
            "shorts" => filtered_shorts = Some(parsed),
            "livestreams" => filtered_livestreams = Some(parsed),
            "unavailable" => filtered_unavailable = Some(parsed),
            "private" => filtered_private = Some(parsed),
            "other" => filtered_other = Some(parsed),
            _ => return None,
        }
    }

    Some(SourceIndexingSummary {
        new_videos,
        existing_videos,
        filtered_total,
        filtered_before_cutoff: filtered_before_cutoff?,
        filtered_shorts: filtered_shorts?,
        filtered_livestreams: filtered_livestreams?,
        filtered_unavailable: filtered_unavailable?,
        filtered_private: filtered_private?,
        filtered_other: filtered_other?,
    })
}

#[must_use]
fn parse_primary_counts(counts: &str) -> Option<(usize, usize, usize)> {
    let mut parts = counts.split(", ");

    let new_videos = parts.next()?.strip_suffix(" new")?.parse().ok()?;
    let existing_videos = parts.next()?.strip_suffix(" existing")?.parse().ok()?;
    let filtered_total = parts.next()?.strip_suffix(" filtered")?.parse().ok()?;

    if parts.next().is_some() {
        return None;
    }

    Some((new_videos, existing_videos, filtered_total))
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

#[cfg(test)]
mod tests {
    use super::{SourceIndexingSummary, parse_source_indexing_summary};

    #[test]
    fn parses_source_indexing_summary_message() {
        let message = "Indexed successfully — 0 new, 1 existing, 5 filtered (cutoff=3, shorts=0, livestreams=0, unavailable=1, private=1, other=0)";

        let parsed = parse_source_indexing_summary(message);

        assert_eq!(
            parsed,
            Some(SourceIndexingSummary {
                new_videos: 0,
                existing_videos: 1,
                filtered_total: 5,
                filtered_before_cutoff: 3,
                filtered_shorts: 0,
                filtered_livestreams: 0,
                filtered_unavailable: 1,
                filtered_private: 1,
                filtered_other: 0,
            })
        );
    }

    #[test]
    fn rejects_non_matching_message() {
        let parsed = parse_source_indexing_summary("Started downloading video");
        assert!(parsed.is_none());
    }
}
