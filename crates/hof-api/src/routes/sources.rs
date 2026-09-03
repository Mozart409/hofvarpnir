//! Source CRUD endpoints and manual index trigger.
//!
//! Sources represent channels, playlists, or other feeds to monitor.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use hof_core::{
    actors::jellyfin_metadata::TriggerSourceMetadata,
    actors::scheduler::{DRAINING_REFUSAL_MESSAGE, IndexSource, PAUSED_REFUSAL_PREFIX},
    db::{self, CreateSource, UpdateSource},
    domain::{
        api_key::ApiKeyScope,
        source::{EntryOrder, Source, SourceType},
    },
};

use crate::{
    AppState,
    auth::{ApiErrorResponse, Auth},
};

/// Build the sources router.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_sources, create_source))
        .routes(routes!(get_source, update_source, delete_source))
        .routes(routes!(trigger_index))
        .routes(routes!(trigger_metadata))
        .routes(routes!(reset_entry_order))
}

// ============================================================================
// Request/Response Types
// ============================================================================

/// Query parameters for listing sources.
#[derive(Debug, Deserialize)]
pub struct ListSourcesQuery {
    /// Filter by profile ID (optional).
    pub profile_id: Option<String>,
}

/// Response body for a source.
#[derive(Debug, Serialize, ToSchema)]
pub struct SourceResponse {
    pub id: String,
    pub profile_id: String,
    pub url: String,
    pub source_type: SourceType,
    pub custom_name: Option<String>,
    /// Whether this source is enabled for indexing and downloading.
    pub enabled: bool,
    /// When true, this source's videos are exempt from automatic cleanup
    /// (both retention expiry and profile quota enforcement).
    pub exclude_from_cleanup: bool,
    /// How often to check for new videos (seconds).
    pub index_frequency_secs: i64,
    /// Ignore videos published before this date (YYYY-MM-DD).
    pub cutoff_date: String,
    /// Per-source retention override (days).
    pub retention_days: Option<i32>,
    /// Detected entry ordering for this source.
    pub entry_order: EntryOrder,
    pub last_indexed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Source> for SourceResponse {
    fn from(s: Source) -> Self {
        Self {
            id: s.id.to_string(),
            profile_id: s.profile_id.to_string(),
            url: s.url,
            source_type: s.source_type,
            custom_name: s.custom_name,
            enabled: s.enabled,
            exclude_from_cleanup: s.exclude_from_cleanup,
            index_frequency_secs: s.index_frequency_secs,
            cutoff_date: s.cutoff_date.to_string(),
            retention_days: s.retention_days,
            entry_order: s.entry_order,
            last_indexed_at: s.last_indexed_at,
            created_at: s.created_at,
            updated_at: s.updated_at,
        }
    }
}

/// Request body for creating a source.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSourceRequest {
    /// Profile ID this source belongs to.
    pub profile_id: String,
    /// Channel or playlist URL.
    pub url: String,
    /// Type of source (channel or playlist).
    pub source_type: SourceType,
    /// User-defined label (optional).
    pub custom_name: Option<String>,
    /// How often to check for new videos (seconds). Default: 1 hour.
    #[serde(default = "default_index_frequency")]
    pub index_frequency_secs: i64,
    /// Ignore videos published before this date (YYYY-MM-DD).
    pub cutoff_date: String,
    /// Per-source retention override (days).
    pub retention_days: Option<i32>,
}

const fn default_index_frequency() -> i64 {
    3600 // 1 hour
}

/// Request body for updating a source.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSourceRequest {
    /// Channel or playlist URL.
    pub url: Option<String>,
    /// Type of source (channel or playlist).
    pub source_type: Option<SourceType>,
    /// User-defined label. Use `null` to clear.
    #[serde(default, deserialize_with = "deserialize_optional_optional_string")]
    pub custom_name: Option<Option<String>>,
    /// How often to check for new videos (seconds).
    pub index_frequency_secs: Option<i64>,
    /// Ignore videos published before this date (YYYY-MM-DD).
    pub cutoff_date: Option<String>,
    /// Per-source retention override (days). Use `null` to clear.
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub retention_days: Option<Option<i32>>,
}

/// Custom deserializer for `Option<Option<T>>` to distinguish between
/// "field not present" vs "field is null".
#[allow(clippy::option_option)]
fn deserialize_optional_optional<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

/// Custom deserializer for `Option<Option<String>>`.
#[allow(clippy::option_option)]
fn deserialize_optional_optional_string<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

/// Response for manual index trigger.
#[derive(Debug, Serialize, ToSchema)]
pub struct IndexTriggerResponse {
    pub message: String,
    pub source_id: String,
}

/// Response for manual metadata generation trigger.
#[derive(Debug, Serialize, ToSchema)]
pub struct MetadataTriggerResponse {
    pub message: String,
    pub source_id: String,
}

/// Error response body.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

// ============================================================================
// Handlers
// ============================================================================

/// List all sources.
///
/// Optionally filter by profile ID using the `profile_id` query parameter.
#[utoipa::path(
    get,
    path = "",
    tag = "sources",
    params(
        ("profile_id" = Option<String>, Query, description = "Filter by profile ID")
    ),
    responses(
        (status = 200, description = "List of sources", body = Vec<SourceResponse>),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn list_sources(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<ListSourcesQuery>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Read) {
        return e.into_response();
    }

    let result = if let Some(profile_id_str) = query.profile_id {
        let Ok(profile_id) = Ulid::from_string(&profile_id_str) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Invalid profile_id format".to_string(),
                }),
            )
                .into_response();
        };
        db::list_sources_for_profile(&state.pool, profile_id).await
    } else {
        db::list_sources(&state.pool).await
    };

    match result {
        Ok(sources) => {
            let responses: Vec<SourceResponse> = sources.into_iter().map(Into::into).collect();
            (StatusCode::OK, Json(responses)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to list sources");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to list sources".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Create a new source.
#[utoipa::path(
    post,
    path = "",
    tag = "sources",
    request_body = CreateSourceRequest,
    responses(
        (status = 201, description = "Source created", body = SourceResponse),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn create_source(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<CreateSourceRequest>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let Ok(profile_id) = Ulid::from_string(&req.profile_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid profile_id format".to_string(),
            }),
        )
            .into_response();
    };

    let Ok(cutoff_date) = NaiveDate::parse_from_str(&req.cutoff_date, "%Y-%m-%d") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid cutoff_date format. Use YYYY-MM-DD.".to_string(),
            }),
        )
            .into_response();
    };

    let data = CreateSource {
        profile_id,
        url: &req.url,
        source_type: req.source_type,
        custom_name: req.custom_name.as_deref(),
        index_frequency_secs: req.index_frequency_secs,
        cutoff_date,
        retention_days: req.retention_days,
    };

    match db::create_source(&state.pool, data).await {
        Ok(source) => {
            let response: SourceResponse = source.into();
            (StatusCode::CREATED, Json(response)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to create source");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to create source".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Get a source by ID.
#[utoipa::path(
    get,
    path = "/{id}",
    tag = "sources",
    params(
        ("id" = String, Path, description = "Source ID (ULID)")
    ),
    responses(
        (status = 200, description = "Source found", body = SourceResponse),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 404, description = "Source not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn get_source(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Read) {
        return e.into_response();
    }

    let Ok(source_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid source ID format".to_string(),
            }),
        )
            .into_response();
    };

    match db::get_source(&state.pool, source_id).await {
        Ok(source) => {
            let response: SourceResponse = source.into();
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(db::DbError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Source not found".to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to get source");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get source".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Update a source.
#[utoipa::path(
    put,
    path = "/{id}",
    tag = "sources",
    params(
        ("id" = String, Path, description = "Source ID (ULID)")
    ),
    request_body = UpdateSourceRequest,
    responses(
        (status = 200, description = "Source updated", body = SourceResponse),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 404, description = "Source not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn update_source(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(req): Json<UpdateSourceRequest>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let Ok(source_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid source ID format".to_string(),
            }),
        )
            .into_response();
    };

    let cutoff_date = match &req.cutoff_date {
        Some(date_str) => match NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
            Ok(date) => Some(date),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        error: "Invalid cutoff_date format. Use YYYY-MM-DD.".to_string(),
                    }),
                )
                    .into_response();
            }
        },
        None => None,
    };

    // Convert Option<Option<String>> to Option<Option<&str>>
    let custom_name = req
        .custom_name
        .as_ref()
        .map(|opt| opt.as_ref().map(String::as_str));

    let data = UpdateSource {
        url: req.url.as_deref(),
        source_type: req.source_type,
        custom_name,
        index_frequency_secs: req.index_frequency_secs,
        cutoff_date,
        retention_days: req.retention_days,
    };

    match db::update_source(&state.pool, source_id, data).await {
        Ok(source) => {
            let response: SourceResponse = source.into();
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(db::DbError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Source not found".to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to update source");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to update source".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Delete a source.
#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "sources",
    params(
        ("id" = String, Path, description = "Source ID (ULID)")
    ),
    responses(
        (status = 204, description = "Source deleted"),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 404, description = "Source not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn delete_source(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Delete) {
        return e.into_response();
    }

    let Ok(source_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid source ID format".to_string(),
            }),
        )
            .into_response();
    };

    match db::delete_source(&state.pool, source_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(db::DbError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Source not found".to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to delete source");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to delete source".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Trigger manual indexing of a source.
///
/// This starts an immediate indexing job for the specified source,
/// bypassing the normal schedule.
#[utoipa::path(
    post,
    path = "/{id}/index",
    tag = "sources",
    params(
        ("id" = String, Path, description = "Source ID (ULID)")
    ),
    responses(
        (status = 202, description = "Indexing started", body = IndexTriggerResponse),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 404, description = "Source not found", body = ErrorResponse),
        (status = 409, description = "Source already being indexed, or indexing is paused", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse),
        (status = 503, description = "System is draining for shutdown", body = ErrorResponse)
    )
)]
pub async fn trigger_index(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let Ok(source_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid source ID format".to_string(),
            }),
        )
            .into_response();
    };

    // Verify source exists first
    if matches!(
        db::get_source(&state.pool, source_id).await,
        Err(db::DbError::NotFound)
    ) {
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Source not found".to_string(),
            }),
        )
            .into_response();
    }

    // Send message to scheduler to trigger indexing
    // Kameo flattens Result<(), String> reply into Result<(), SendError<..., String>>
    // where SendError can contain the String error from the handler
    match state.scheduler.ask(IndexSource { source_id }).await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(IndexTriggerResponse {
                message: "Indexing started".to_string(),
                source_id: source_id.to_string(),
            }),
        )
            .into_response(),
        Err(send_err) => {
            // Extract the error message from SendError
            let error_msg = send_err.to_string();
            let status = index_trigger_error_status(&error_msg);
            // Only genuinely unexpected failures (e.g. a DB error bubbling
            // up from `reset_source_indexing_errors`/`get_source`) are
            // logged as errors here — the already-indexed/paused/draining
            // refusals are expected, operator-visible outcomes, not server
            // faults.
            if status == StatusCode::INTERNAL_SERVER_ERROR {
                tracing::error!(error = %send_err, "Failed to trigger indexing");
            }
            (status, Json(ErrorResponse { error: error_msg })).into_response()
        }
    }
}

/// Map an `IndexSource` handler refusal message to the HTTP status that best
/// describes it.
///
/// Kept as a small pure function on the raw message (rather than inlined in
/// the handler) so it can be unit-tested directly — the real path is only
/// reachable through a live actor `SendError`, which this crate cannot spin
/// up in a unit test.
///
/// Matches `DRAINING_REFUSAL_MESSAGE` and `PAUSED_REFUSAL_PREFIX` from
/// `hof_core::actors::scheduler`, the same constants
/// `SchedulerActor::indexing_refusal_for` builds its refusal messages from,
/// so the producer and this matcher cannot silently drift apart. The
/// "already being indexed" check remains a literal match, matching that
/// existing (pre-Task-6) message's style.
///
/// - Already indexing: 409 — a conflict with an in-progress operation.
/// - Paused: 409 — a conflict with a currently-configured, operator-
///   controlled state that can be resolved by resuming (same status as the
///   already-indexing case, and for the same reason: it is not a server
///   fault, and it is resolvable by the operator without retrying blindly).
/// - Draining: 503 — the server is deliberately refusing new work because
///   it is going away; unlike a pause, this cannot be resolved by resuming
///   in the current process, so it gets the "temporarily unavailable"
///   status rather than "conflict".
/// - Anything else (e.g. a DB error surfaced as a string): 500.
fn index_trigger_error_status(error_msg: &str) -> StatusCode {
    if error_msg.contains("already being indexed") {
        StatusCode::CONFLICT
    } else if error_msg.contains(DRAINING_REFUSAL_MESSAGE) {
        StatusCode::SERVICE_UNAVAILABLE
    } else if error_msg.starts_with(PAUSED_REFUSAL_PREFIX) {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

/// Trigger manual Jellyfin metadata generation for a source.
///
/// This generates tvshow.nfo, poster.jpg, fanart.jpg, and banner.jpg
/// for the specified source.
///
/// Note: The source must have been indexed first (via Trigger Index) to have
/// channel metadata (thumbnail URL, channel ID) available for image downloads.
#[utoipa::path(
    post,
    path = "/{id}/metadata",
    tag = "sources",
    params(
        ("id" = String, Path, description = "Source ID (ULID)")
    ),
    responses(
        (status = 202, description = "Metadata generation started", body = MetadataTriggerResponse),
        (status = 400, description = "Invalid ID format or missing channel metadata", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 404, description = "Source not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn trigger_metadata(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let Ok(source_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid source ID format".to_string(),
            }),
        )
            .into_response();
    };

    // Verify source exists and has channel metadata
    let source = match db::get_source(&state.pool, source_id).await {
        Ok(s) => s,
        Err(db::DbError::NotFound) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "Source not found".to_string(),
                }),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to get source");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get source".to_string(),
                }),
            )
                .into_response();
        }
    };

    if source.channel_thumbnail_url.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Source has no channel metadata. Run 'Trigger Index' first to fetch \
                        channel information from YouTube."
                    .to_string(),
            }),
        )
            .into_response();
    }

    // Send message to jellyfin metadata actor
    match state
        .jellyfin_metadata
        .ask(TriggerSourceMetadata { source_id })
        .await
    {
        Ok(result) if result.success => (
            StatusCode::ACCEPTED,
            Json(MetadataTriggerResponse {
                message: "Metadata generated successfully".to_string(),
                source_id: source_id.to_string(),
            }),
        )
            .into_response(),
        Ok(result) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: result.error.unwrap_or_else(|| "Unknown error".to_string()),
            }),
        )
            .into_response(),
        Err(send_err) => {
            tracing::error!(error = %send_err, "Failed to trigger metadata generation");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: send_err.to_string(),
                }),
            )
                .into_response()
        }
    }
}

// ============================================================================
// Reset Entry Order
// ============================================================================

/// Reset entry order detection for a source.
///
/// Sets the `entry_order` back to `Unknown`, triggering re-detection on next index.
#[utoipa::path(
    post,
    path = "/api/sources/{id}/reset-order",
    params(
        ("id" = String, Path, description = "Source ID (ULID)")
    ),
    responses(
        (status = 200, description = "Entry order reset successfully", body = SourceResponse),
        (status = 400, body = ApiErrorResponse, description = "Invalid source ID format"),
        (status = 401, body = ApiErrorResponse, description = "Unauthorized"),
        (status = 404, body = ApiErrorResponse, description = "Source not found"),
        (status = 500, body = ApiErrorResponse, description = "Internal server error"),
    ),
    security(
        ("api_key" = [])
    ),
    tag = "sources"
)]
pub async fn reset_entry_order(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let Ok(source_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid source ID format".to_string(),
            }),
        )
            .into_response();
    };

    // Reset entry order to Unknown
    if let Err(e) = db::update_source_entry_order(&state.pool, source_id, EntryOrder::Unknown).await
    {
        if matches!(e, db::DbError::NotFound) {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "Source not found".to_string(),
                }),
            )
                .into_response();
        }
        tracing::error!(error = %e, "Failed to reset entry order");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "Failed to reset entry order".to_string(),
            }),
        )
            .into_response();
    }

    // Return updated source
    match db::get_source(&state.pool, source_id).await {
        Ok(source) => (StatusCode::OK, Json(SourceResponse::from(source))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to get source after reset");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Entry order reset but failed to fetch updated source".to_string(),
                }),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_trigger_response_schema() {
        let response = MetadataTriggerResponse {
            message: "Metadata generated successfully".to_string(),
            source_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        };
        assert_eq!(response.message, "Metadata generated successfully");
        assert_eq!(response.source_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
    }

    #[test]
    fn test_index_trigger_response_schema() {
        let response = IndexTriggerResponse {
            message: "Indexing started".to_string(),
            source_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
        };
        assert_eq!(response.message, "Indexing started");
        assert_eq!(response.source_id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
    }

    // `index_trigger_error_status` is the pure string->status mapping behind
    // `trigger_index`'s error branch. The real path is only reachable
    // through a live actor `SendError`, which these tests do not attempt to
    // construct — they exercise the mapping function directly instead, on
    // exactly the message shapes `SchedulerActor::indexing_refusal_for`
    // (and the pre-existing "already being indexed" refusal) produce.
    #[test]
    fn already_indexing_maps_to_conflict() {
        assert_eq!(
            index_trigger_error_status("Source is already being indexed"),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn draining_refusal_maps_to_service_unavailable() {
        assert_eq!(
            index_trigger_error_status(DRAINING_REFUSAL_MESSAGE),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn paused_refusal_variants_map_to_conflict() {
        assert_eq!(
            index_trigger_error_status(PAUSED_REFUSAL_PREFIX),
            StatusCode::CONFLICT
        );
        assert_eq!(
            index_trigger_error_status("Indexing is paused indefinitely"),
            StatusCode::CONFLICT
        );
        assert_eq!(
            index_trigger_error_status("Indexing is paused until 2026-09-03T00:00:00Z"),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn unrecognized_error_maps_to_internal_server_error() {
        assert_eq!(
            index_trigger_error_status("relation \"videos\" does not exist"),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
