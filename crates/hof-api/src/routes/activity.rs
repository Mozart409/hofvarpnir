//! Activity log endpoints.
//!
//! Provides endpoints to view system activity events including downloads,
//! indexing, errors, and other operations.

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use hof_core::{
    db,
    domain::{
        activity::{
            ActivityEvent, ActivityEventType, ActivitySeverity, SourceIndexingSummary,
            UnhealthySource,
        },
        api_key::ApiKeyScope,
    },
};

use crate::{
    AppState,
    auth::{ApiErrorResponse, Auth},
};

/// Build the activity router.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_activity))
        .routes(routes!(list_unhealthy_sources))
}

// ============================================================================
// Request/Response Types
// ============================================================================

/// Query parameters for listing activity events.
#[derive(Debug, Deserialize)]
pub struct ListActivityQuery {
    /// Maximum number of events to return (default: 50, max: 200).
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// Number of events to skip for pagination.
    #[serde(default)]
    pub offset: i64,
    /// Filter by severity level.
    pub severity: Option<ActivitySeverity>,
    /// Filter by activity event type.
    pub event_type: Option<ActivityEventType>,
    /// Filter by source ID.
    pub source_id: Option<String>,
    /// Case-insensitive substring match against the event message.
    pub search: Option<String>,
}

const fn default_limit() -> i64 {
    50
}

/// Response body for an activity event.
#[derive(Debug, Serialize, ToSchema)]
pub struct ActivityEventResponse {
    pub id: String,
    pub event_type: ActivityEventType,
    pub severity: ActivitySeverity,
    pub message: String,
    pub source_id: Option<String>,
    pub video_id: Option<String>,
    pub profile_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub source_indexing: Option<SourceIndexingSummary>,
}

impl From<ActivityEvent> for ActivityEventResponse {
    fn from(event: ActivityEvent) -> Self {
        let source_indexing = event.source_indexing_summary();

        Self {
            id: event.id.to_string(),
            event_type: event.event_type,
            severity: event.severity,
            message: event.message,
            source_id: event.source_id.map(|id| id.to_string()),
            video_id: event.video_id.map(|id| id.to_string()),
            profile_id: event.profile_id.map(|id| id.to_string()),
            created_at: event.created_at,
            source_indexing,
        }
    }
}

/// Paginated response for activity events.
#[derive(Debug, Serialize, ToSchema)]
pub struct ActivityListResponse {
    pub events: Vec<ActivityEventResponse>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

/// Query parameters for listing unhealthy sources.
#[derive(Debug, Deserialize)]
pub struct UnhealthySourcesQuery {
    /// Minimum number of consecutive errors for a source to be reported
    /// (default: 3, min: 1).
    #[serde(default = "default_min_errors")]
    pub min_errors: i64,
}

const fn default_min_errors() -> i64 {
    3
}

/// Response body for an unhealthy (persistently erroring) source.
#[derive(Debug, Serialize, ToSchema)]
pub struct UnhealthySourceResponse {
    pub source_id: String,
    pub custom_name: Option<String>,
    pub url: String,
    pub enabled: bool,
    pub consecutive_errors: i64,
    pub first_error_at: DateTime<Utc>,
    pub last_error_at: DateTime<Utc>,
    pub last_error_message: Option<String>,
}

impl From<UnhealthySource> for UnhealthySourceResponse {
    fn from(source: UnhealthySource) -> Self {
        Self {
            source_id: source.source_id.to_string(),
            custom_name: source.custom_name,
            url: source.url,
            enabled: source.enabled,
            consecutive_errors: source.consecutive_errors,
            first_error_at: source.first_error_at,
            last_error_at: source.last_error_at,
            last_error_message: source.last_error_message,
        }
    }
}

/// Response for the unhealthy-sources report.
#[derive(Debug, Serialize, ToSchema)]
pub struct UnhealthySourcesResponse {
    pub sources: Vec<UnhealthySourceResponse>,
    pub total: usize,
    /// The `min_errors` threshold that was applied.
    pub threshold: i64,
}

/// Error response body.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

// ============================================================================
// Handlers
// ============================================================================

/// List activity events.
///
/// Returns a paginated list of system activity events in reverse-chronological order.
/// Can be filtered by severity and source ID.
#[utoipa::path(
    get,
    path = "",
    tag = "activity",
    params(
        ("limit" = Option<i64>, Query, description = "Maximum number of events (default: 50, max: 200)"),
        ("offset" = Option<i64>, Query, description = "Number of events to skip"),
        ("severity" = Option<ActivitySeverity>, Query, description = "Filter by severity"),
        ("event_type" = Option<ActivityEventType>, Query, description = "Filter by activity event type"),
        ("source_id" = Option<String>, Query, description = "Filter by source ID")
    ),
    responses(
        (status = 200, description = "List of activity events", body = ActivityListResponse),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn list_activity(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<ListActivityQuery>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Read) {
        return e.into_response();
    }

    // Validate and cap limit
    let limit = query.limit.clamp(1, 200);

    // Parse source_id if provided
    let source_id = match &query.source_id {
        Some(id_str) => match Ulid::from_string(id_str) {
            Ok(id) => Some(id),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: "Invalid source_id format".to_string(),
                    }),
                )
                    .into_response();
            }
        },
        None => None,
    };

    // Get total count
    let severity_filter = query.severity.clone();
    let search: Option<String> = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let total = match db::count_activity_events(
        &state.pool,
        severity_filter,
        query.event_type.clone(),
        source_id,
        search.as_deref(),
    )
    .await
    {
        Ok(count) => count,
        Err(e) => {
            tracing::error!(error = %e, "Failed to count activity events");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to count activity events".to_string(),
                }),
            )
                .into_response();
        }
    };

    // Get events
    match db::list_activity_events(
        &state.pool,
        limit,
        query.offset,
        query.severity,
        query.event_type,
        source_id,
        search.as_deref(),
    )
    .await
    {
        Ok(events) => {
            let responses: Vec<ActivityEventResponse> =
                events.into_iter().map(Into::into).collect();
            (
                StatusCode::OK,
                Json(ActivityListResponse {
                    events: responses,
                    total,
                    limit,
                    offset: query.offset,
                }),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to list activity events");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to list activity events".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// List unhealthy sources.
///
/// Returns sources that are enabled but persistently failing to index — those
/// with at least `min_errors` consecutive `SourceError` events since their last
/// successful index. Useful for surfacing silent indexing failures that
/// otherwise only appear buried in the activity log.
#[utoipa::path(
    get,
    path = "/unhealthy-sources",
    tag = "activity",
    params(
        ("min_errors" = Option<i64>, Query, description = "Minimum consecutive errors to report (default: 3, min: 1)")
    ),
    responses(
        (status = 200, description = "List of unhealthy sources", body = UnhealthySourcesResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn list_unhealthy_sources(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<UnhealthySourcesQuery>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Read) {
        return e.into_response();
    }

    let threshold = query.min_errors.max(1);

    match db::list_unhealthy_sources(&state.pool, threshold).await {
        Ok(sources) => {
            let responses: Vec<UnhealthySourceResponse> =
                sources.into_iter().map(Into::into).collect();
            (
                StatusCode::OK,
                Json(UnhealthySourcesResponse {
                    total: responses.len(),
                    sources: responses,
                    threshold,
                }),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to list unhealthy sources");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to list unhealthy sources".to_string(),
                }),
            )
                .into_response()
        }
    }
}
