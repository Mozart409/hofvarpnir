//! Profile CRUD endpoints.
//!
//! Profiles define download configurations that can apply to sources from any platform.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use hof_core::{
    db::{self, CreateProfile, UpdateProfile},
    domain::{
        api_key::ApiKeyScope,
        profile::{OutputPreset, Profile, Quality},
    },
    ytdlp::validate_output_template,
};

use crate::{
    AppState,
    auth::{ApiErrorResponse, Auth},
};

/// Build the profiles router.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_profiles, create_profile))
        .routes(routes!(get_profile, update_profile, delete_profile))
}

// ============================================================================
// Request/Response Types
// ============================================================================

/// Query parameters for listing profiles.
#[derive(Debug, Deserialize)]
pub struct ListProfilesQuery {
    /// Filter by user ID (optional).
    pub user_id: Option<String>,
}

/// Response body for a profile.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProfileResponse {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub quality: Quality,
    pub output_preset: OutputPreset,
    pub naming_template: String,
    pub output_dir: String,
    pub include_livestreams: bool,
    pub include_shorts: bool,
    pub storage_quota_bytes: i64,
    pub retention_days: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<Profile> for ProfileResponse {
    fn from(p: Profile) -> Self {
        Self {
            id: p.id.to_string(),
            user_id: p.user_id.to_string(),
            name: p.name,
            quality: p.quality,
            output_preset: p.output_preset,
            naming_template: p.naming_template,
            output_dir: p.output_dir,
            include_livestreams: p.include_livestreams,
            include_shorts: p.include_shorts,
            storage_quota_bytes: p.storage_quota_bytes,
            retention_days: p.retention_days,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

/// Request body for creating a profile.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateProfileRequest {
    /// User ID that owns this profile.
    pub user_id: String,
    /// Profile name.
    pub name: String,
    /// Download quality preset.
    pub quality: Quality,
    /// Output preset for codec/container strategy.
    pub output_preset: Option<OutputPreset>,
    /// Naming template for downloaded files (e.g., "{title}-{id}.{ext}").
    pub naming_template: String,
    /// Output directory for downloads.
    pub output_dir: String,
    /// Whether to download livestream VODs.
    #[serde(default)]
    pub include_livestreams: bool,
    /// Whether to download Shorts.
    #[serde(default)]
    pub include_shorts: bool,
    /// Maximum disk usage for this profile (bytes).
    #[serde(default = "default_storage_quota")]
    pub storage_quota_bytes: i64,
    /// Auto-cleanup after N days (profile-wide).
    pub retention_days: Option<i32>,
}

const fn default_storage_quota() -> i64 {
    // 100 GB default
    100_000_000_000
}

/// Request body for updating a profile.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateProfileRequest {
    /// Profile name.
    pub name: Option<String>,
    /// Download quality preset.
    pub quality: Option<Quality>,
    /// Output preset for codec/container strategy.
    pub output_preset: Option<OutputPreset>,
    /// Naming template for downloaded files.
    pub naming_template: Option<String>,
    /// Output directory for downloads.
    pub output_dir: Option<String>,
    /// Whether to download livestream VODs.
    pub include_livestreams: Option<bool>,
    /// Whether to download Shorts.
    pub include_shorts: Option<bool>,
    /// Maximum disk usage for this profile (bytes).
    pub storage_quota_bytes: Option<i64>,
    /// Auto-cleanup after N days. Use `null` to clear.
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

/// Error response body.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

// ============================================================================
// Handlers
// ============================================================================

/// List all profiles.
///
/// Optionally filter by user ID using the `user_id` query parameter.
#[utoipa::path(
    get,
    path = "",
    tag = "profiles",
    params(
        ("user_id" = Option<String>, Query, description = "Filter by user ID")
    ),
    responses(
        (status = 200, description = "List of profiles", body = Vec<ProfileResponse>),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn list_profiles(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<ListProfilesQuery>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Read) {
        return e.into_response();
    }

    let result = if let Some(user_id_str) = query.user_id {
        let Ok(user_id) = Ulid::from_string(&user_id_str) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Invalid user_id format".to_string(),
                }),
            )
                .into_response();
        };
        db::list_profiles_for_user(&state.pool, user_id).await
    } else {
        db::list_profiles(&state.pool).await
    };

    match result {
        Ok(profiles) => {
            let responses: Vec<ProfileResponse> = profiles.into_iter().map(Into::into).collect();
            (StatusCode::OK, Json(responses)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to list profiles");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to list profiles".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Create a new profile.
#[utoipa::path(
    post,
    path = "",
    tag = "profiles",
    request_body = CreateProfileRequest,
    responses(
        (status = 201, description = "Profile created", body = ProfileResponse),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn create_profile(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<CreateProfileRequest>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let Ok(user_id) = Ulid::from_string(&req.user_id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid user_id format".to_string(),
            }),
        )
            .into_response();
    };

    if let Err(message) = validate_output_template(req.naming_template.trim()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: message }),
        )
            .into_response();
    }

    let data = CreateProfile {
        user_id,
        name: &req.name,
        quality: req.quality,
        output_preset: req.output_preset.unwrap_or(OutputPreset::Browser),
        naming_template: req.naming_template.trim(),
        output_dir: &req.output_dir,
        include_livestreams: req.include_livestreams,
        include_shorts: req.include_shorts,
        storage_quota_bytes: req.storage_quota_bytes,
        retention_days: req.retention_days,
    };

    match db::create_profile(&state.pool, data).await {
        Ok(profile) => {
            let response: ProfileResponse = profile.into();
            (StatusCode::CREATED, Json(response)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to create profile");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to create profile".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Get a profile by ID.
#[utoipa::path(
    get,
    path = "/{id}",
    tag = "profiles",
    params(
        ("id" = String, Path, description = "Profile ID (ULID)")
    ),
    responses(
        (status = 200, description = "Profile found", body = ProfileResponse),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 404, description = "Profile not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn get_profile(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Read) {
        return e.into_response();
    }

    let Ok(profile_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid profile ID format".to_string(),
            }),
        )
            .into_response();
    };

    match db::get_profile(&state.pool, profile_id).await {
        Ok(profile) => {
            let response: ProfileResponse = profile.into();
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(db::DbError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Profile not found".to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to get profile");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to get profile".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Update a profile.
#[utoipa::path(
    put,
    path = "/{id}",
    tag = "profiles",
    params(
        ("id" = String, Path, description = "Profile ID (ULID)")
    ),
    request_body = UpdateProfileRequest,
    responses(
        (status = 200, description = "Profile updated", body = ProfileResponse),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 404, description = "Profile not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn update_profile(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(req): Json<UpdateProfileRequest>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let Ok(profile_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid profile ID format".to_string(),
            }),
        )
            .into_response();
    };

    let validated_naming_template = match req.naming_template.as_deref() {
        Some(template) => {
            let template = template.trim();
            if let Err(message) = validate_output_template(template) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse { error: message }),
                )
                    .into_response();
            }
            Some(template.to_string())
        }
        None => None,
    };

    let data = UpdateProfile {
        name: req.name.as_deref(),
        quality: req.quality,
        output_preset: req.output_preset,
        naming_template: validated_naming_template.as_deref(),
        output_dir: req.output_dir.as_deref(),
        include_livestreams: req.include_livestreams,
        include_shorts: req.include_shorts,
        storage_quota_bytes: req.storage_quota_bytes,
        retention_days: req.retention_days,
    };

    match db::update_profile(&state.pool, profile_id, data).await {
        Ok(profile) => {
            let response: ProfileResponse = profile.into();
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(db::DbError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Profile not found".to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to update profile");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to update profile".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Delete a profile.
#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "profiles",
    params(
        ("id" = String, Path, description = "Profile ID (ULID)")
    ),
    responses(
        (status = 204, description = "Profile deleted"),
        (status = 400, description = "Invalid ID format", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 404, description = "Profile not found", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub async fn delete_profile(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Delete) {
        return e.into_response();
    }

    let Ok(profile_id) = Ulid::from_string(&id) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid profile ID format".to_string(),
            }),
        )
            .into_response();
    };

    match db::delete_profile(&state.pool, profile_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(db::DbError::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "Profile not found".to_string(),
            }),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to delete profile");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "Failed to delete profile".to_string(),
                }),
            )
                .into_response()
        }
    }
}
