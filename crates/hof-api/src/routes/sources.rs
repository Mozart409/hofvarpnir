//! Source CRUD endpoints and manual index trigger.
//!
//! Sources represent channels, playlists, or other feeds to monitor.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use utoipa::ToSchema;

use hof_core::{
    actors::scheduler::IndexSource,
    db::{self, CreateSource, UpdateSource},
    domain::source::{Source, SourceType},
};

use crate::AppState;

/// Build the sources router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_sources).post(create_source))
        .route(
            "/{id}",
            get(get_source).put(update_source).delete(delete_source),
        )
        .route("/{id}/index", post(trigger_index))
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
    /// How often to check for new videos (seconds).
    pub index_frequency_secs: i64,
    /// Ignore videos published before this date (YYYY-MM-DD).
    pub cutoff_date: String,
    /// Per-source retention override (days).
    pub retention_days: Option<i32>,
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
            index_frequency_secs: s.index_frequency_secs,
            cutoff_date: s.cutoff_date.to_string(),
            retention_days: s.retention_days,
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

fn default_index_frequency() -> i64 {
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
    path = "/api/v1/sources",
    tag = "sources",
    params(
        ("profile_id" = Option<String>, Query, description = "Filter by profile ID")
    ),
    responses(
        (status = 200, description = "List of sources", body = Vec<SourceResponse>),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn list_sources(
    State(state): State<AppState>,
    Query(query): Query<ListSourcesQuery>,
) -> impl IntoResponse {
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
    path = "/api/v1/sources",
    tag = "sources",
    request_body = CreateSourceRequest,
    responses(
        (status = 201, description = "Source created", body = SourceResponse),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn create_source(
    State(state): State<AppState>,
    Json(req): Json<CreateSourceRequest>,
) -> impl IntoResponse {
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
    path = "/api/v1/sources/{id}",
    tag = "sources",
    params(
        ("id" = String, Path, description = "Source ID (ULID)")
    ),
    responses(
        (status = 200, description = "Source found", body = SourceResponse),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 404, description = "Source not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn get_source(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
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
    path = "/api/v1/sources/{id}",
    tag = "sources",
    params(
        ("id" = String, Path, description = "Source ID (ULID)")
    ),
    request_body = UpdateSourceRequest,
    responses(
        (status = 200, description = "Source updated", body = SourceResponse),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 404, description = "Source not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn update_source(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateSourceRequest>,
) -> impl IntoResponse {
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
    path = "/api/v1/sources/{id}",
    tag = "sources",
    params(
        ("id" = String, Path, description = "Source ID (ULID)")
    ),
    responses(
        (status = 204, description = "Source deleted"),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 404, description = "Source not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn delete_source(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
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
    path = "/api/v1/sources/{id}/index",
    tag = "sources",
    params(
        ("id" = String, Path, description = "Source ID (ULID)")
    ),
    responses(
        (status = 202, description = "Indexing started", body = IndexTriggerResponse),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 404, description = "Source not found", body = ErrorResponse),
        (status = 409, description = "Source already being indexed", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn trigger_index(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
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
    if let Err(db::DbError::NotFound) = db::get_source(&state.pool, source_id).await {
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
            if error_msg.contains("already being indexed") {
                (
                    StatusCode::CONFLICT,
                    Json(ErrorResponse { error: error_msg }),
                )
                    .into_response()
            } else {
                tracing::error!(error = %send_err, "Failed to trigger indexing");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse { error: error_msg }),
                )
                    .into_response()
            }
        }
    }
}
