//! Runtime settings, pause, and shutdown endpoints.
#![deny(clippy::arithmetic_side_effects, clippy::string_slice)]

use std::time::Duration;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

use hof_core::{
    config::EnvOverrides,
    db::{self, RuntimeSettingsPatch},
    domain::api_key::ApiKeyScope,
    runtime_config::{
        DrainToken, EffectiveSettings, Provenance, indefinite_pause, resolve, sleep_duration_until,
    },
};

use crate::{
    AppState,
    auth::{ApiErrorResponse, Auth},
};

// ============================================================================
// Shared response types
// ============================================================================
//
// Agent A appends request/response types and handlers, and the
// `pub fn router() -> OpenApiRouter<AppState>`, to this same file.

/// A resolved integer knob together with the layer that supplied it.
#[derive(Debug, Serialize, ToSchema)]
pub struct ResolvedU32 {
    pub value: u32,
    pub provenance: Provenance,
}

/// A resolved duration knob, in whole seconds, with its provenance.
#[derive(Debug, Serialize, ToSchema)]
pub struct ResolvedSecs {
    pub value: u64,
    pub provenance: Provenance,
}

/// Pause state for one module.
///
/// `until` is `None` when not paused AND when the pause is indefinite —
/// an indefinite pause is signalled by `indefinite: true`, never by
/// serializing the sentinel timestamp (ADR-0003).
#[derive(Debug, Serialize, ToSchema)]
pub struct PauseStateResponse {
    pub paused: bool,
    pub until: Option<DateTime<Utc>>,
    pub indefinite: bool,
}

/// Pause state for both gated modules.
#[derive(Debug, Serialize, ToSchema)]
pub struct PauseSummaryResponse {
    pub indexing: PauseStateResponse,
    pub downloads: PauseStateResponse,
}

/// Drain progress.
#[derive(Debug, Serialize, ToSchema)]
pub struct DrainStatusResponse {
    pub draining: bool,
    pub started_at: Option<DateTime<Utc>>,
    pub deadline: Option<DateTime<Utc>>,
    /// Whole seconds left before the drain is forced. Saturates at zero;
    /// `None` when not draining.
    pub remaining_secs: Option<u64>,
}

/// Error response body for this module.
#[derive(Debug, Serialize, ToSchema)]
pub struct SettingsErrorResponse {
    pub error: String,
}

impl PauseStateResponse {
    /// Build from a raw `*_paused_until` column value.
    ///
    /// A pause equal to [`indefinite_pause`] reports
    /// `{ paused: true, until: None, indefinite: true }`. An expired pause
    /// (`until <= now`) reports not-paused but still echoes `until`, so an
    /// operator can see the pause that just lapsed.
    #[must_use]
    pub fn new(paused_until: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Self {
        match paused_until {
            None => Self {
                paused: false,
                until: None,
                indefinite: false,
            },
            Some(t) if t == indefinite_pause() => Self {
                paused: true,
                until: None,
                indefinite: true,
            },
            Some(t) => Self {
                paused: t > now,
                until: Some(t),
                indefinite: false,
            },
        }
    }
}

impl PauseSummaryResponse {
    #[must_use]
    pub fn from_settings(settings: &EffectiveSettings, now: DateTime<Utc>) -> Self {
        Self {
            indexing: PauseStateResponse::new(settings.indexing_paused_until, now),
            downloads: PauseStateResponse::new(settings.downloads_paused_until, now),
        }
    }
}

impl DrainStatusResponse {
    /// `deadline` comes from `DrainToken::deadline(timeout)`, which derives it
    /// from the stored drain start time — never from `now` (see R-N).
    #[must_use]
    pub fn new(drain: &DrainToken, drain_timeout: Duration, now: DateTime<Utc>) -> Self {
        let deadline = drain.deadline(drain_timeout);
        let remaining_secs = deadline.map(|d| sleep_duration_until(d, now).as_secs());
        Self {
            draining: drain.is_draining(),
            started_at: drain.started_at(),
            deadline,
            remaining_secs,
        }
    }
}

// ============================================================================
// Router
// ============================================================================

/// Build the settings/pause/shutdown router.
///
/// Mounted (via `.merge()`, not a second `.nest()` — see `lib.rs`) under the
/// same `/api/v1/system` prefix as `system::router()`, so paths here are
/// relative: `/settings`, `/pause`, `/shutdown`.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_settings, patch_settings))
        .routes(routes!(pause, resume))
        .routes(routes!(shutdown))
}

// ============================================================================
// Request/response types (Agent A)
// ============================================================================

/// Current runtime settings: resolved knobs plus pause state and the last
/// audit stamp.
#[derive(Debug, Serialize, ToSchema)]
pub struct SettingsResponse {
    pub pause: PauseSummaryResponse,
    pub max_concurrent_downloads: ResolvedU32,
    pub max_indexers_per_tick: ResolvedU32,
    pub rate_limit_delay_secs: ResolvedSecs,
    pub check_interval_secs: ResolvedSecs,
    pub cleanup_interval_secs: ResolvedSecs,
    pub drain_timeout_secs: ResolvedSecs,
    /// When the underlying row was last written. `None` if the audit read
    /// (a separate query from the resolved settings) failed — see
    /// [`get_settings`].
    pub updated_at: Option<DateTime<Utc>>,
    /// Who last wrote the underlying row. `None` under the same condition as
    /// `updated_at`, or if no patch has ever named an actor.
    pub updated_by: Option<String>,
}

impl SettingsResponse {
    /// Build from resolved settings plus the audit fields read separately
    /// from the database row. Shared by `GET /settings` (which reads
    /// `RuntimeConfig::current()`) and `PATCH /settings` (which resolves the
    /// row `patch_runtime_settings` just returned), so the field mapping
    /// lives here exactly once.
    #[must_use]
    pub fn from_parts(
        settings: &EffectiveSettings,
        now: DateTime<Utc>,
        updated_at: Option<DateTime<Utc>>,
        updated_by: Option<String>,
    ) -> Self {
        Self {
            pause: PauseSummaryResponse::from_settings(settings, now),
            max_concurrent_downloads: ResolvedU32 {
                value: settings.max_concurrent_downloads.value,
                provenance: settings.max_concurrent_downloads.provenance,
            },
            max_indexers_per_tick: ResolvedU32 {
                value: settings.max_indexers_per_tick.value,
                provenance: settings.max_indexers_per_tick.provenance,
            },
            rate_limit_delay_secs: ResolvedSecs {
                value: settings.rate_limit_delay.value.as_secs(),
                provenance: settings.rate_limit_delay.provenance,
            },
            check_interval_secs: ResolvedSecs {
                value: settings.check_interval.value.as_secs(),
                provenance: settings.check_interval.provenance,
            },
            cleanup_interval_secs: ResolvedSecs {
                value: settings.cleanup_interval.value.as_secs(),
                provenance: settings.cleanup_interval.provenance,
            },
            drain_timeout_secs: ResolvedSecs {
                value: settings.drain_timeout.value.as_secs(),
                provenance: settings.drain_timeout.provenance,
            },
            updated_at,
            updated_by,
        }
    }
}

/// `serde`'s derive for `Option<Option<T>>` cannot by itself distinguish a
/// JSON key that is absent from one that is present with an explicit
/// `null`: both would otherwise deserialize to the outer `None`. This
/// deserializer is only invoked when the key IS present in the input (that
/// is what `#[serde(default, deserialize_with = "double_option")]` buys —
/// `default` supplies the outer `None` when the key is missing entirely,
/// and `deserialize_with` only runs for a present key), so wrapping
/// whatever `Option<T>::deserialize` produces in an extra `Some` recovers
/// the third state. Walking through all three cases:
///   - key absent          -> `double_option` never runs -> `#[serde(default)]` gives outer `None`
///   - key present, `null` -> `Option::<T>::deserialize` yields `Ok(None)`    -> wrapped -> `Some(None)`
///   - key present, value  -> `Option::<T>::deserialize` yields `Ok(Some(v))` -> wrapped -> `Some(Some(v))`
//
// The `Option<Option<T>>` return type is the deliberate point of this
// function, not an accident `option_option` should catch: it is the exact
// three-state encoding (absent / explicit null / value) that
// `hof_core::db::RuntimeSettingsPatch` already uses for the same reason, so
// this mirrors that shape rather than inventing a new one.
#[allow(clippy::option_option)]
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

/// Partial update. A field absent from the JSON body is left untouched; an
/// explicit JSON `null` resets that knob to its env/default fallback.
///
/// `indexing_paused_until` / `downloads_paused_until` are deliberately not
/// here — pause has its own endpoints (`POST`/`DELETE /pause`).
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct PatchSettingsRequest {
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<u32>)]
    pub max_concurrent_downloads: Option<Option<u32>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<u32>)]
    pub max_indexers_per_tick: Option<Option<u32>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<u64>)]
    pub rate_limit_delay_secs: Option<Option<u64>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<u64>)]
    pub check_interval_secs: Option<Option<u64>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<u64>)]
    pub cleanup_interval_secs: Option<Option<u64>>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<u64>)]
    pub drain_timeout_secs: Option<Option<u64>>,
}

/// Validate one knob against its DB `CHECK` bound and `i32` column width.
///
/// `None` (untouched) and `Some(None)` (explicit reset) pass through
/// unchanged; `Some(Some(v))` is checked against `min` and, separately,
/// against whether it fits `i32` (the column is `INTEGER`), since `T` here
/// is `u32` or `u64` and can exceed `i32::MAX` even when it clears `min`.
//
// Both the parameter and the return type are `Option<Option<_>>`
// deliberately, for the same reason as `double_option` above: this function
// exists specifically to move a `PatchSettingsRequest` field's three-state
// value (absent / explicit null / value) into the matching
// `RuntimeSettingsPatch` field without collapsing that third state.
#[allow(clippy::option_option)]
fn validate_min<T>(
    field: Option<Option<T>>,
    min: T,
    name: &str,
) -> Result<Option<Option<i32>>, String>
where
    T: Copy + PartialOrd + std::fmt::Display,
    i32: TryFrom<T>,
{
    match field {
        None => Ok(None),
        Some(None) => Ok(Some(None)),
        Some(Some(v)) if v < min => Err(format!("{name} must be >= {min}, got {v}")),
        Some(Some(v)) => match i32::try_from(v) {
            Ok(v) => Ok(Some(Some(v))),
            Err(_) => Err(format!(
                "{name} must fit in a 32-bit signed integer, got {v}"
            )),
        },
    }
}

/// Validate a [`PatchSettingsRequest`] against the same bounds the database
/// `CHECK` constraints enforce, translating a violation into a
/// human-readable message that names the offending field rather than
/// letting the database reject it.
fn validate_patch(request: &PatchSettingsRequest) -> Result<RuntimeSettingsPatch, String> {
    Ok(RuntimeSettingsPatch {
        max_concurrent_downloads: validate_min(
            request.max_concurrent_downloads,
            1,
            "max_concurrent_downloads",
        )?,
        max_indexers_per_tick: validate_min(
            request.max_indexers_per_tick,
            1,
            "max_indexers_per_tick",
        )?,
        rate_limit_delay_secs: validate_min(
            request.rate_limit_delay_secs,
            0,
            "rate_limit_delay_secs",
        )?,
        check_interval_secs: validate_min(request.check_interval_secs, 1, "check_interval_secs")?,
        cleanup_interval_secs: validate_min(
            request.cleanup_interval_secs,
            1,
            "cleanup_interval_secs",
        )?,
        drain_timeout_secs: validate_min(request.drain_timeout_secs, 1, "drain_timeout_secs")?,
        ..RuntimeSettingsPatch::default()
    })
}

/// Which module a pause/resume request targets.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PauseModule {
    Indexing,
    Downloads,
    All,
}

const fn default_pause_module() -> PauseModule {
    PauseModule::All
}

/// Query parameters for `DELETE /pause`.
#[derive(Debug, Deserialize)]
pub struct ResumeQuery {
    /// Which module to resume. Defaults to `all` when absent, so a bare
    /// `DELETE /api/v1/system/pause` resumes everything.
    #[serde(default = "default_pause_module")]
    pub module: PauseModule,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PauseRequest {
    pub module: PauseModule,
    /// Pause length in seconds. Omit (or `null`) to pause indefinitely.
    #[serde(default)]
    pub duration_secs: Option<u64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PauseResponse {
    pub pause: PauseSummaryResponse,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ShutdownResponse {
    pub message: String,
    pub drain: DrainStatusResponse,
}

/// Compute a pause expiry from an optional duration.
///
/// `None` pauses indefinitely, via [`indefinite_pause`] — never
/// `DateTime::<Utc>::MAX_UTC` (see the doc comment on `indefinite_pause`
/// for why that sentinel does not round-trip through Postgres).
///
/// `Some(0)` is rejected: a zero-second pause is indistinguishable from not
/// pausing and is almost certainly an operator error. Any duration that
/// overflows `DateTime<Utc>`'s representable range, or whose expiry would
/// land at or beyond `indefinite_pause()`, is also rejected rather than
/// silently clamped or aliased to the sentinel.
fn pause_expiry(duration_secs: Option<u64>) -> Result<DateTime<Utc>, String> {
    let Some(secs) = duration_secs else {
        return Ok(indefinite_pause());
    };
    if secs == 0 {
        return Err(
            "duration_secs must be greater than 0; omit it to pause indefinitely".to_string(),
        );
    }
    let secs = i64::try_from(secs).map_err(|_| "duration_secs is too large".to_string())?;
    let delta = chrono::Duration::try_seconds(secs)
        .ok_or_else(|| "duration_secs is too large".to_string())?;
    let until = Utc::now()
        .checked_add_signed(delta)
        .ok_or_else(|| "duration_secs is too large; the resulting expiry overflows".to_string())?;
    if until >= indefinite_pause() {
        return Err(
            "duration_secs is too large; the resulting expiry would land at or beyond \
                     the indefinite-pause sentinel"
                .to_string(),
        );
    }
    Ok(until)
}

// ============================================================================
// Handlers
// ============================================================================

/// Get the current runtime settings.
///
/// Resolved knobs and pause state come from the in-memory `RuntimeConfig`
/// cache (`state.runtime_config.current()`), which is what actors actually
/// act on. `updated_at`/`updated_by` are not tracked in `EffectiveSettings`,
/// so this makes a best-effort separate read of the database row for them;
/// if that read fails, this still returns 200 with the resolved settings and
/// both audit fields `None`, rather than failing the whole request over an
/// audit-trail read.
#[utoipa::path(
    get,
    path = "/settings",
    tag = "system",
    responses(
        (status = 200, description = "Current runtime settings", body = SettingsResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse)
    )
)]
pub async fn get_settings(State(state): State<AppState>, auth: Auth) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Read) {
        return e.into_response();
    }

    let effective = state.runtime_config.current();
    let now = Utc::now();

    let (updated_at, updated_by) = match db::get_runtime_settings(&state.pool).await {
        Ok(row) => (row.updated_at, row.updated_by),
        Err(error) => {
            tracing::error!(%error, "Failed to read runtime settings audit fields");
            (None, None)
        }
    };

    (
        StatusCode::OK,
        Json(SettingsResponse::from_parts(
            &effective, now, updated_at, updated_by,
        )),
    )
        .into_response()
}

/// Partially update runtime settings.
///
/// Validates the six patchable knobs against the same bounds the database
/// `CHECK` constraints enforce (400 on violation), applies the patch, and
/// returns settings resolved from the row `patch_runtime_settings` just
/// returned — NOT from `state.runtime_config.current()`, which is updated
/// asynchronously by the LISTEN/NOTIFY listener and may still hold the
/// pre-patch value immediately after this call returns.
#[utoipa::path(
    patch,
    path = "/settings",
    tag = "system",
    request_body = PatchSettingsRequest,
    responses(
        (status = 200, description = "Updated settings", body = SettingsResponse),
        (status = 400, description = "Invalid request", body = SettingsErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = SettingsErrorResponse)
    )
)]
pub async fn patch_settings(
    State(state): State<AppState>,
    auth: Auth,
    Json(request): Json<PatchSettingsRequest>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let mut patch = match validate_patch(&request) {
        Ok(patch) => patch,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(SettingsErrorResponse { error }),
            )
                .into_response();
        }
    };
    patch.updated_by = Some(auth.user_id().to_string());

    match db::patch_runtime_settings(&state.pool, &patch).await {
        Ok(row) => {
            let effective = resolve(&row, &EnvOverrides::from_env());
            (
                StatusCode::OK,
                Json(SettingsResponse::from_parts(
                    &effective,
                    Utc::now(),
                    row.updated_at,
                    row.updated_by,
                )),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(%error, "Failed to patch runtime settings");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SettingsErrorResponse {
                    error: "Failed to update settings".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Pause indexing, downloads, or both, for a bounded or indefinite duration.
///
/// See [`pause_expiry`] for the exact rejection rules around
/// `duration_secs`.
#[utoipa::path(
    post,
    path = "/pause",
    tag = "system",
    request_body = PauseRequest,
    responses(
        (status = 200, description = "Updated pause state", body = PauseResponse),
        (status = 400, description = "Invalid request", body = SettingsErrorResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = SettingsErrorResponse)
    )
)]
pub async fn pause(
    State(state): State<AppState>,
    auth: Auth,
    Json(request): Json<PauseRequest>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let until = match pause_expiry(request.duration_secs) {
        Ok(until) => until,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(SettingsErrorResponse { error }),
            )
                .into_response();
        }
    };

    let mut patch = RuntimeSettingsPatch {
        updated_by: Some(auth.user_id().to_string()),
        ..RuntimeSettingsPatch::default()
    };
    match request.module {
        PauseModule::Indexing => patch.indexing_paused_until = Some(Some(until)),
        PauseModule::Downloads => patch.downloads_paused_until = Some(Some(until)),
        PauseModule::All => {
            patch.indexing_paused_until = Some(Some(until));
            patch.downloads_paused_until = Some(Some(until));
        }
    }

    match db::patch_runtime_settings(&state.pool, &patch).await {
        Ok(row) => {
            let effective = resolve(&row, &EnvOverrides::from_env());
            (
                StatusCode::OK,
                Json(PauseResponse {
                    pause: PauseSummaryResponse::from_settings(&effective, Utc::now()),
                }),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(%error, "Failed to set pause state");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SettingsErrorResponse {
                    error: "Failed to update pause state".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Resume indexing, downloads, or both.
///
/// Sets the selected column(s) to a genuine SQL `NULL` (`Some(None)` in the
/// patch), which is what makes the resolver fall through to the env/default
/// layers, rather than merely writing a past timestamp.
#[utoipa::path(
    delete,
    path = "/pause",
    tag = "system",
    params(
        ("module" = Option<PauseModule>, Query, description = "Which module to resume (default: all)")
    ),
    responses(
        (status = 200, description = "Updated pause state", body = PauseResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse),
        (status = 500, description = "Internal server error", body = SettingsErrorResponse)
    )
)]
pub async fn resume(
    State(state): State<AppState>,
    auth: Auth,
    Query(query): Query<ResumeQuery>,
) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let mut patch = RuntimeSettingsPatch {
        updated_by: Some(auth.user_id().to_string()),
        ..RuntimeSettingsPatch::default()
    };
    match query.module {
        PauseModule::Indexing => patch.indexing_paused_until = Some(None),
        PauseModule::Downloads => patch.downloads_paused_until = Some(None),
        PauseModule::All => {
            patch.indexing_paused_until = Some(None);
            patch.downloads_paused_until = Some(None);
        }
    }

    match db::patch_runtime_settings(&state.pool, &patch).await {
        Ok(row) => {
            let effective = resolve(&row, &EnvOverrides::from_env());
            (
                StatusCode::OK,
                Json(PauseResponse {
                    pause: PauseSummaryResponse::from_settings(&effective, Utc::now()),
                }),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(%error, "Failed to resume");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SettingsErrorResponse {
                    error: "Failed to update pause state".to_string(),
                }),
            )
                .into_response()
        }
    }
}

/// Begin draining the process for a clean shutdown.
///
/// Idempotent: `DrainToken::begin` is first-write-wins, so a repeated call
/// reports the deadline derived from the ORIGINAL drain start time, never a
/// freshly recomputed `now + timeout` (see `DrainStatusResponse::new`, which
/// reads the deadline back from the token rather than being handed one).
#[utoipa::path(
    post,
    path = "/shutdown",
    tag = "system",
    responses(
        (status = 202, description = "Drain started (or already in progress)", body = ShutdownResponse),
        (status = 401, description = "Unauthorized", body = ApiErrorResponse),
        (status = 403, description = "Forbidden - insufficient scope", body = ApiErrorResponse)
    )
)]
pub async fn shutdown(State(state): State<AppState>, auth: Auth) -> impl IntoResponse {
    if let Err(e) = auth.require_scope(ApiKeyScope::Write) {
        return e.into_response();
    }

    let now = Utc::now();
    state.drain.begin(now);
    let timeout = state.runtime_config.current().drain_timeout.value;
    let drain = DrainStatusResponse::new(&state.drain, timeout, now);

    tracing::info!(deadline = ?drain.deadline, "Drain triggered");

    (
        StatusCode::ACCEPTED,
        Json(ShutdownResponse {
            message: "Draining".to_string(),
            drain,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    // Used only by `sample_effective_settings` below; the non-test build of
    // this file never constructs an `EffectiveSettings` by hand (it always
    // comes from `RuntimeConfig::current()` or `resolve()`), so importing
    // this at file scope would be flagged as unused outside `#[cfg(test)]`.
    use hof_core::runtime_config::Resolved;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn sample_effective_settings() -> EffectiveSettings {
        EffectiveSettings {
            indexing_paused_until: None,
            downloads_paused_until: None,
            max_concurrent_downloads: Resolved {
                value: 3,
                provenance: Provenance::Default,
            },
            max_indexers_per_tick: Resolved {
                value: 5,
                provenance: Provenance::Env,
            },
            rate_limit_delay: Resolved {
                value: Duration::from_secs(5),
                provenance: Provenance::Database,
            },
            check_interval: Resolved {
                value: Duration::from_mins(1),
                provenance: Provenance::Default,
            },
            cleanup_interval: Resolved {
                value: Duration::from_hours(1),
                provenance: Provenance::Default,
            },
            drain_timeout: Resolved {
                value: Duration::from_mins(30),
                provenance: Provenance::Default,
            },
        }
    }

    #[test]
    fn pause_state_none_is_not_paused() {
        let state = PauseStateResponse::new(None, now());
        assert!(!state.paused);
        assert_eq!(state.until, None);
        assert!(!state.indefinite);
    }

    #[test]
    fn pause_state_future_finite_pause_is_paused() {
        let now = now();
        let until = now
            .checked_add_signed(chrono::Duration::seconds(60))
            .expect("in range");
        let state = PauseStateResponse::new(Some(until), now);
        assert!(state.paused);
        assert_eq!(state.until, Some(until));
        assert!(!state.indefinite);
    }

    #[test]
    fn pause_state_indefinite_pause_hides_sentinel() {
        let state = PauseStateResponse::new(Some(indefinite_pause()), now());
        assert!(state.paused);
        assert_eq!(state.until, None);
        assert!(state.indefinite);
    }

    #[test]
    fn pause_state_expired_pause_is_not_paused() {
        let now = now();
        let until = now
            .checked_sub_signed(chrono::Duration::seconds(60))
            .expect("in range");
        let state = PauseStateResponse::new(Some(until), now);
        assert!(!state.paused);
        assert_eq!(state.until, Some(until));
        assert!(!state.indefinite);
    }

    #[test]
    fn pause_state_indefinite_json_has_no_sentinel_leak() {
        let state = PauseStateResponse::new(Some(indefinite_pause()), now());
        let json = serde_json::to_string(&state).expect("serialization cannot fail");
        assert!(!json.contains("infinity"));
        assert!(!json.contains("9999"));
    }

    #[test]
    fn drain_status_fresh_token_is_not_draining() {
        let drain = DrainToken::new();
        let status = DrainStatusResponse::new(&drain, Duration::from_mins(30), now());
        assert!(!status.draining);
        assert_eq!(status.started_at, None);
        assert_eq!(status.deadline, None);
        assert_eq!(status.remaining_secs, None);
    }

    #[test]
    fn drain_status_after_begin_reports_deadline_and_saturates_remaining() {
        let drain = DrainToken::new();
        let t0 = now();
        drain.begin(t0);
        let timeout = Duration::from_mins(30);

        let status = DrainStatusResponse::new(&drain, timeout, t0);
        assert!(status.draining);
        assert_eq!(status.started_at, Some(t0));
        let expected_deadline = t0
            .checked_add_signed(
                chrono::Duration::from_std(timeout).expect("timeout fits chrono::Duration"),
            )
            .expect("no overflow");
        assert_eq!(status.deadline, Some(expected_deadline));

        // `now` far past the deadline must saturate to zero, not underflow.
        let long_after_deadline = expected_deadline
            .checked_add_signed(chrono::Duration::seconds(3600))
            .expect("in range");
        let status_after = DrainStatusResponse::new(&drain, timeout, long_after_deadline);
        assert_eq!(status_after.remaining_secs, Some(0));
    }

    // ------------------------------------------------------------------
    // double_option
    // ------------------------------------------------------------------

    #[test]
    fn double_option_distinguishes_absent_null_and_value() {
        let absent: PatchSettingsRequest = serde_json::from_str("{}").expect("empty body");
        assert_eq!(absent.max_concurrent_downloads, None);

        let explicit_null: PatchSettingsRequest =
            serde_json::from_str(r#"{"max_concurrent_downloads": null}"#)
                .expect("explicit null body");
        assert_eq!(explicit_null.max_concurrent_downloads, Some(None));

        let with_value: PatchSettingsRequest =
            serde_json::from_str(r#"{"max_concurrent_downloads": 7}"#).expect("value body");
        assert_eq!(with_value.max_concurrent_downloads, Some(Some(7)));
    }

    // ------------------------------------------------------------------
    // validate_patch
    // ------------------------------------------------------------------

    #[test]
    fn validate_patch_max_concurrent_downloads_boundary() {
        let ok = PatchSettingsRequest {
            max_concurrent_downloads: Some(Some(1)),
            ..PatchSettingsRequest::default()
        };
        assert_eq!(
            validate_patch(&ok)
                .expect("1 is the minimum accepted value")
                .max_concurrent_downloads,
            Some(Some(1))
        );

        let bad = PatchSettingsRequest {
            max_concurrent_downloads: Some(Some(0)),
            ..PatchSettingsRequest::default()
        };
        assert!(validate_patch(&bad).is_err());
    }

    #[test]
    fn validate_patch_max_indexers_per_tick_boundary() {
        let ok = PatchSettingsRequest {
            max_indexers_per_tick: Some(Some(1)),
            ..PatchSettingsRequest::default()
        };
        assert_eq!(
            validate_patch(&ok)
                .expect("1 is the minimum accepted value")
                .max_indexers_per_tick,
            Some(Some(1))
        );

        let bad = PatchSettingsRequest {
            max_indexers_per_tick: Some(Some(0)),
            ..PatchSettingsRequest::default()
        };
        assert!(validate_patch(&bad).is_err());
    }

    #[test]
    fn validate_patch_rate_limit_delay_secs_boundary() {
        // 0 is the minimum accepted value for this knob (the DB CHECK is
        // `>= 0`, unlike the other five). Because the field is `u64`, there
        // is no smaller value to construct a below-minimum rejection from —
        // the type itself already rules that out; the only way this knob
        // can be rejected is the i32-width check exercised separately below.
        let ok = PatchSettingsRequest {
            rate_limit_delay_secs: Some(Some(0)),
            ..PatchSettingsRequest::default()
        };
        assert_eq!(
            validate_patch(&ok)
                .expect("0 is valid")
                .rate_limit_delay_secs,
            Some(Some(0))
        );
    }

    #[test]
    fn validate_patch_check_interval_secs_boundary() {
        let ok = PatchSettingsRequest {
            check_interval_secs: Some(Some(1)),
            ..PatchSettingsRequest::default()
        };
        assert_eq!(
            validate_patch(&ok)
                .expect("1 is the minimum accepted value")
                .check_interval_secs,
            Some(Some(1))
        );

        let bad = PatchSettingsRequest {
            check_interval_secs: Some(Some(0)),
            ..PatchSettingsRequest::default()
        };
        assert!(validate_patch(&bad).is_err());
    }

    #[test]
    fn validate_patch_cleanup_interval_secs_boundary() {
        let ok = PatchSettingsRequest {
            cleanup_interval_secs: Some(Some(1)),
            ..PatchSettingsRequest::default()
        };
        assert_eq!(
            validate_patch(&ok)
                .expect("1 is the minimum accepted value")
                .cleanup_interval_secs,
            Some(Some(1))
        );

        let bad = PatchSettingsRequest {
            cleanup_interval_secs: Some(Some(0)),
            ..PatchSettingsRequest::default()
        };
        assert!(validate_patch(&bad).is_err());
    }

    #[test]
    fn validate_patch_drain_timeout_secs_boundary() {
        let ok = PatchSettingsRequest {
            drain_timeout_secs: Some(Some(1)),
            ..PatchSettingsRequest::default()
        };
        assert_eq!(
            validate_patch(&ok)
                .expect("1 is the minimum accepted value")
                .drain_timeout_secs,
            Some(Some(1))
        );

        let bad = PatchSettingsRequest {
            drain_timeout_secs: Some(Some(0)),
            ..PatchSettingsRequest::default()
        };
        assert!(validate_patch(&bad).is_err());
    }

    #[test]
    fn validate_patch_rejects_values_exceeding_i32_max() {
        let over_u64 = u64::from(u32::MAX); // 4294967295, well past i32::MAX
        let bad = PatchSettingsRequest {
            rate_limit_delay_secs: Some(Some(over_u64)),
            ..PatchSettingsRequest::default()
        };
        assert!(validate_patch(&bad).is_err());

        let bad_u32 = PatchSettingsRequest {
            max_concurrent_downloads: Some(Some(u32::MAX)),
            ..PatchSettingsRequest::default()
        };
        assert!(validate_patch(&bad_u32).is_err());
    }

    #[test]
    fn validate_patch_only_touches_patched_fields() {
        let request = PatchSettingsRequest {
            rate_limit_delay_secs: Some(Some(10)),
            ..PatchSettingsRequest::default()
        };
        let patch = validate_patch(&request).expect("valid patch");
        assert_eq!(patch.max_concurrent_downloads, None);
        assert_eq!(patch.max_indexers_per_tick, None);
        assert_eq!(patch.rate_limit_delay_secs, Some(Some(10)));
        assert_eq!(patch.check_interval_secs, None);
        assert_eq!(patch.cleanup_interval_secs, None);
        assert_eq!(patch.drain_timeout_secs, None);
    }

    #[test]
    fn validate_patch_some_none_resets_field() {
        let request = PatchSettingsRequest {
            max_concurrent_downloads: Some(None),
            ..PatchSettingsRequest::default()
        };
        let patch = validate_patch(&request).expect("explicit reset is valid");
        assert_eq!(patch.max_concurrent_downloads, Some(None));
    }

    // ------------------------------------------------------------------
    // PauseRequest / pause_expiry
    // ------------------------------------------------------------------

    #[test]
    fn pause_request_deserializes_all_module_without_duration() {
        let req: PauseRequest = serde_json::from_str(r#"{"module":"all"}"#).expect("valid body");
        assert!(matches!(req.module, PauseModule::All));
        assert_eq!(req.duration_secs, None);
    }

    #[test]
    fn pause_request_deserializes_indexing_with_duration() {
        let req: PauseRequest =
            serde_json::from_str(r#"{"module":"indexing","duration_secs":3600}"#)
                .expect("valid body");
        assert!(matches!(req.module, PauseModule::Indexing));
        assert_eq!(req.duration_secs, Some(3600));
    }

    #[test]
    fn pause_expiry_zero_duration_is_rejected() {
        assert!(pause_expiry(Some(0)).is_err());
    }

    #[test]
    fn pause_expiry_none_is_exactly_indefinite_pause() {
        assert_eq!(pause_expiry(None), Ok(indefinite_pause()));
    }

    #[test]
    fn pause_expiry_finite_duration_is_in_the_future() {
        let before = now();
        let until = pause_expiry(Some(3600)).expect("valid duration");
        assert!(until > before);
        assert_ne!(until, indefinite_pause());
    }

    // ------------------------------------------------------------------
    // SettingsResponse
    // ------------------------------------------------------------------

    #[test]
    fn settings_response_serializes_provenance_lowercase() {
        let settings = sample_effective_settings();
        let response = SettingsResponse::from_parts(&settings, now(), None, None);
        let json = serde_json::to_value(&response).expect("serializes");
        assert_eq!(json["max_concurrent_downloads"]["provenance"], "default");
        assert_eq!(json["max_indexers_per_tick"]["provenance"], "env");
        assert_eq!(json["rate_limit_delay_secs"]["provenance"], "database");
        assert_eq!(json["check_interval_secs"]["provenance"], "default");
        assert_eq!(json["cleanup_interval_secs"]["provenance"], "default");
        assert_eq!(json["drain_timeout_secs"]["provenance"], "default");
    }

    #[test]
    fn settings_response_converts_durations_to_whole_seconds() {
        let settings = sample_effective_settings();
        let response = SettingsResponse::from_parts(&settings, now(), None, None);
        assert_eq!(response.rate_limit_delay_secs.value, 5);
        assert_eq!(response.check_interval_secs.value, 60);
        assert_eq!(response.cleanup_interval_secs.value, 3600);
        assert_eq!(response.drain_timeout_secs.value, 1800);
    }

    // ------------------------------------------------------------------
    // ShutdownResponse / drain idempotency (Ruling R-N)
    // ------------------------------------------------------------------

    #[test]
    fn shutdown_response_serializes() {
        let drain = DrainToken::new();
        let t0 = now();
        drain.begin(t0);
        let status = DrainStatusResponse::new(&drain, Duration::from_mins(30), t0);
        let response = ShutdownResponse {
            message: "Draining".to_string(),
            drain: status,
        };
        let json = serde_json::to_value(&response).expect("serializes");
        assert_eq!(json["message"], "Draining");
        assert_eq!(json["drain"]["draining"], true);
    }

    /// Ruling R-N: a repeated `begin` must not push the deadline out. This
    /// covers the exact behavior `POST /shutdown` depends on to be
    /// idempotent, without spinning up an axum test server.
    #[test]
    fn repeated_begin_leaves_deadline_anchored_to_first_start() {
        let drain = DrainToken::new();
        let t0 = now();
        drain.begin(t0);
        let timeout = Duration::from_mins(30);
        let first = DrainStatusResponse::new(&drain, timeout, t0);

        let t1 = t0
            .checked_add_signed(chrono::Duration::seconds(30))
            .expect("in range");
        drain.begin(t1); // second call must not move the deadline
        let second = DrainStatusResponse::new(&drain, timeout, t1);

        assert_eq!(second.started_at, Some(t0));
        assert_eq!(first.deadline, second.deadline);
    }
}
