//! Maud page templates and htmx partial endpoints.

use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Form, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{
        IntoResponse, Redirect, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use chrono::{NaiveDate, Utc};
use futures::stream::{Stream, StreamExt, unfold};
use hof_api::AppState;
use hof_core::{
    actors::{
        cleanup::{GetCleanupStatus, RunCleanup},
        download_supervisor::{CancelDownload, EnqueueDownload},
        jellyfin_metadata::TriggerSourceMetadata,
        scheduler::IndexSource,
    },
    auth::generate_api_key,
    db::{self, CreateApiKey, CreateProfile, CreateSource, UpdateProfile, UpdateSource},
    domain::{
        activity::{ActivityEventType, ActivitySeverity},
        api_key::{ApiKey, ApiKeyEventType, ApiKeyScope},
        profile::{OutputPreset, Profile, Quality},
        source::{EntryOrder, Source, SourceType},
        system::IssueSeverity,
        video::{Video, VideoStatus},
    },
    ytdlp::validate_output_template,
};
use maud::{DOCTYPE, Markup, PreEscaped, Render, html};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tower_sessions::Session;
use ulid::Ulid;

use crate::auth::AuthUser;

/// Static assets embedded at compile time.
#[derive(Embed)]
#[folder = "assets/"]
struct Assets;

/// Characters that must be escaped inside a query-string value.
///
/// Beyond the standard non-ASCII/control set, `&` and `#` would terminate the
/// value and `+`/space would be decoded as a space by form parsers.
const QUERY_VALUE_ENCODE_SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

// ============================================================================
// SSE Utilities
// ============================================================================

/// Coalesce window for SSE debouncing: rapid bursts collapse into one push.
const SSE_COALESCE_WINDOW: Duration = Duration::from_millis(500);

/// Wrap a `broadcast::Receiver<()>` into a debounced `Stream`.
///
/// On the first signal, starts a coalesce window. Any signals that arrive
/// during the window are drained and discarded. One item is yielded after the
/// window expires. Returns `None` (ends the stream) only when the channel is
/// closed (no more senders).
fn debounced_broadcast(rx: broadcast::Receiver<()>) -> impl Stream<Item = ()> {
    use tokio::sync::broadcast::error::RecvError;
    unfold(rx, |mut rx| async move {
        // Wait for the first signal (or detect channel closure).
        match rx.recv().await {
            Ok(()) | Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => return None,
        }
        // Coalesce additional signals within the window.
        tokio::time::sleep(SSE_COALESCE_WINDOW).await;
        while rx.try_recv().is_ok() {}
        Some(((), rx))
    })
}

/// Serve embedded static assets.
async fn serve_asset(Path(path): Path<String>) -> Response {
    match Assets::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref())],
                content.data.into_owned(),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NavItem {
    Dashboard,
    Profiles,
    Sources,
    Downloads,
    Activity,
    Schedule,
    ApiKeys,
}

#[derive(Debug, Deserialize)]
enum QualityForm {
    #[serde(rename = "best")]
    Best,
    #[serde(rename = "4320p")]
    Q4320p,
    #[serde(rename = "2160p")]
    Q2160p,
    #[serde(rename = "1440p")]
    Q1440p,
    #[serde(rename = "1080p")]
    Q1080p,
    #[serde(rename = "720p")]
    Q720p,
    #[serde(rename = "480p")]
    Q480p,
    #[serde(rename = "audio_only")]
    AudioOnly,
}

impl From<QualityForm> for Quality {
    fn from(value: QualityForm) -> Self {
        match value {
            QualityForm::Best => Self::Best,
            QualityForm::Q4320p => Self::Q4320p,
            QualityForm::Q2160p => Self::Q2160p,
            QualityForm::Q1440p => Self::Q1440p,
            QualityForm::Q1080p => Self::Q1080p,
            QualityForm::Q720p => Self::Q720p,
            QualityForm::Q480p => Self::Q480p,
            QualityForm::AudioOnly => Self::AudioOnly,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OutputPresetForm {
    Auto,
    Browser,
    Tv,
}

impl From<OutputPresetForm> for OutputPreset {
    fn from(value: OutputPresetForm) -> Self {
        match value {
            OutputPresetForm::Auto => Self::Auto,
            OutputPresetForm::Browser => Self::Browser,
            OutputPresetForm::Tv => Self::Tv,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SourceTypeForm {
    Channel,
    Playlist,
}

impl From<SourceTypeForm> for SourceType {
    fn from(value: SourceTypeForm) -> Self {
        match value {
            SourceTypeForm::Channel => Self::Channel,
            SourceTypeForm::Playlist => Self::Playlist,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ProfileForm {
    name: String,
    quality: QualityForm,
    output_preset: OutputPresetForm,
    naming_template: String,
    output_dir: String,
    include_livestreams: Option<String>,
    include_shorts: Option<String>,
    storage_quota_gb: i64,
    retention_days: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SourceForm {
    profile_id: String,
    url: String,
    source_type: SourceTypeForm,
    custom_name: Option<String>,
    index_frequency_secs: i64,
    cutoff_date: String,
    retention_days: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApiKeyForm {
    name: String,
    scope_read: Option<String>,
    scope_write: Option<String>,
    scope_delete: Option<String>,
    expires_in: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    email: String,
    password: String,
}

#[derive(Debug, Deserialize)]
pub struct RegisterForm {
    name: String,
    email: String,
    password: String,
    password_confirm: String,
}

// ============================================================================
// Flash Messages
// ============================================================================

const FLASH_KEY: &str = "flash_message";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FlashMessage {
    level: String,
    message: String,
}

async fn set_flash(session: &Session, level: &str, message: &str) {
    let flash = FlashMessage {
        level: level.to_string(),
        message: message.to_string(),
    };
    let _ = session.insert(FLASH_KEY, flash).await;
}

async fn take_flash(session: &Session) -> Option<FlashMessage> {
    let flash: Option<FlashMessage> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some() {
        let _ = session.remove::<FlashMessage>(FLASH_KEY).await;
    }
    flash
}

// ============================================================================
// Downloads Query
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
struct DownloadsQuery {
    status: Option<String>,
    search: Option<String>,
    page: Option<i64>,
    per_page: Option<i64>,
}

/// Free-text filter for the sources and schedule pages.
///
/// Both pages already load every source to render, so the term is applied
/// in-memory rather than pushed into SQL.
#[derive(Debug, Clone, Deserialize)]
struct SourcesQuery {
    search: Option<String>,
}

pub fn router(state: AppState, oidc_enabled: bool) -> Router {
    Router::new()
        // Auth routes (no session required)
        .route(
            "/login",
            get(move |session: Session| async move { login_page(session, oidc_enabled).await })
                .post(login),
        )
        .route("/register", get(register_page).post(register))
        .route("/logout", post(logout))
        // Protected routes
        .route("/", get(index))
        .route("/dashboard", get(dashboard_page))
        .route("/profiles", get(profiles_page).post(create_profile))
        .route("/profiles/{id}", post(update_profile))
        .route("/profiles/{id}/delete", post(delete_profile))
        .route("/sources", get(sources_page).post(create_source))
        .route("/sources/{id}", get(source_detail_page).post(update_source))
        .route("/sources/{id}/delete", post(delete_source))
        .route("/sources/{id}/toggle", post(toggle_source_enabled))
        .route("/sources/{id}/index", post(trigger_index))
        .route("/sources/{id}/metadata", post(trigger_metadata))
        .route("/web/sources/events", get(sources_events_sse))
        .route("/web/sources/{id}/events", get(source_detail_events_sse))
        .route("/downloads", get(downloads_page))
        .route("/downloads/{id}/retry", post(retry_download))
        .route("/downloads/{id}/cancel", post(cancel_download))
        .route("/downloads/{id}/delete", post(delete_download))
        .route("/downloads/bulk", post(bulk_download_action))
        .route("/web/downloads/list", get(downloads_list_partial))
        .route("/web/downloads/events", get(downloads_events_sse))
        .route("/web/system-banner", get(system_banner))
        .route("/web/dashboard/events", get(dashboard_events_sse))
        .route("/activity", get(activity_page))
        .route("/web/activity/list", get(activity_list_partial))
        .route("/web/activity/events", get(activity_events_sse))
        .route("/schedule", get(schedule_page))
        .route("/schedule/cleanup", post(trigger_cleanup))
        // API Keys management
        .route(
            "/settings/api-keys",
            get(api_keys_page).post(create_api_key),
        )
        .route("/settings/api-keys/{id}/roll", post(roll_api_key))
        .route("/settings/api-keys/{id}/delete", post(delete_api_key))
        .route("/settings/api-keys/{id}/events", get(api_key_events))
        // Static assets (embedded at compile time)
        .route("/assets/{*path}", get(serve_asset))
        // Fallback for unmatched routes
        .fallback(not_found)
        .with_state(state)
}

async fn index() -> Redirect {
    Redirect::to("/dashboard")
}

// ============================================================================
// Auth Pages
// ============================================================================

async fn login_page(session: Session, oidc_enabled: bool) -> impl IntoResponse {
    // If already logged in, redirect to dashboard
    if let Ok(Some(_)) = session.get::<String>("user_id").await {
        return Redirect::to("/dashboard").into_response();
    }

    auth_layout("Login", login_form(None, oidc_enabled)).into_response()
}

async fn login(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<LoginForm>,
) -> impl IntoResponse {
    // Check if OIDC is enabled for error form rendering
    let oidc_enabled = hof_core::oidc::OidcConfig::is_configured();

    // Try to find user by email
    let Ok(user) = db::get_user_by_email(&state.pool, &form.email).await else {
        return auth_layout(
            "Login",
            login_form(Some("Invalid email or password"), oidc_enabled),
        )
        .into_response();
    };

    // Verify password (OIDC-only users have no password)
    let password_valid = user.password_hash.as_ref().is_some_and(|hash| {
        !hash.is_empty() && hof_core::auth::verify_password(&form.password, hash).is_ok()
    });

    if !password_valid {
        return auth_layout(
            "Login",
            login_form(Some("Invalid email or password"), oidc_enabled),
        )
        .into_response();
    }

    // Create session
    if let Err(e) = AuthUser::login(&session, user.id).await {
        tracing::error!(error = ?e, "Failed to create session");
        return auth_layout(
            "Login",
            login_form(Some("Failed to create session"), oidc_enabled),
        )
        .into_response();
    }

    Redirect::to("/dashboard").into_response()
}

async fn register_page(session: Session) -> impl IntoResponse {
    // If already logged in, redirect to dashboard
    if let Ok(Some(_)) = session.get::<String>("user_id").await {
        return Redirect::to("/dashboard").into_response();
    }

    auth_layout("Register", register_form(None)).into_response()
}

async fn register(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<RegisterForm>,
) -> impl IntoResponse {
    // Validate passwords match
    if form.password != form.password_confirm {
        return auth_layout("Register", register_form(Some("Passwords do not match")))
            .into_response();
    }

    // Validate password length
    if form.password.len() < 8 {
        return auth_layout(
            "Register",
            register_form(Some("Password must be at least 8 characters")),
        )
        .into_response();
    }

    // Check if email already exists
    if db::get_user_by_email(&state.pool, &form.email)
        .await
        .is_ok()
    {
        return auth_layout("Register", register_form(Some("Email already registered")))
            .into_response();
    }

    // Hash password
    let password_hash = match hof_core::auth::hash_password(&form.password) {
        Ok(hash) => hash,
        Err(e) => {
            tracing::error!(error = ?e, "Failed to hash password");
            return auth_layout("Register", register_form(Some("Registration failed")))
                .into_response();
        }
    };

    // Create user
    let user = match db::create_user(
        &state.pool,
        db::CreateUser {
            email: &form.email,
            name: &form.name,
            password_hash: Some(&password_hash),
        },
    )
    .await
    {
        Ok(user) => user,
        Err(e) => {
            tracing::error!(error = ?e, "Failed to create user");
            return auth_layout("Register", register_form(Some("Registration failed")))
                .into_response();
        }
    };

    // Create session
    if let Err(e) = AuthUser::login(&session, user.id).await {
        tracing::error!(error = ?e, "Failed to create session");
        return auth_layout("Register", register_form(Some("Failed to create session")))
            .into_response();
    }

    Redirect::to("/dashboard").into_response()
}

async fn logout(State(_state): State<AppState>, session: Session) -> impl IntoResponse {
    // Check if OIDC is configured with logout redirect
    let oidc_config = hof_core::oidc::OidcConfig::from_env();
    let logout_redirect = oidc_config.as_ref().is_some_and(|c| c.logout_redirect);

    // Clear local session first
    let _ = AuthUser::logout(&session).await;

    // If OIDC logout redirect is enabled, we would redirect to the provider
    // Note: Full implementation would need to track the ID token and provider logout URL
    // For now, we just redirect to login page after clearing session
    if logout_redirect {
        // In a full implementation, this would redirect to the provider's end_session_endpoint
        // using the stored ID token hint. The OIDC client would provide the logout URL.
        tracing::info!("OIDC logout redirect is enabled - local session cleared");
    }

    Redirect::to("/login")
}

fn login_form(error: Option<&str>, oidc_enabled: bool) -> Markup {
    html! {
        @if oidc_enabled {
            div class="space-y-4" {
                a
                    href="/auth/oidc/login"
                    class="w-full inline-flex items-center justify-center gap-2 rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-4 py-2.5 text-sm font-medium text-slate-700 dark:text-slate-200 hover:bg-slate-50 dark:hover:bg-slate-600 transition"
                {
                    svg class="h-5 w-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" {
                        path d="M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4" {}
                        polyline points="10 17 15 12 10 7" {}
                        line x1="15" y1="12" x2="3" y2="12" {}
                    }
                    "Sign in with SSO"
                }
            }
            div class="relative my-6" {
                div class="absolute inset-0 flex items-center" {
                    div class="w-full border-t border-slate-300 dark:border-slate-600" {}
                }
                div class="relative flex justify-center text-sm" {
                    span class="bg-white dark:bg-slate-800 px-2 text-slate-500 dark:text-slate-400" { "Or continue with email" }
                }
            }
        }
        form method="post" action="/login" class="space-y-6" {
            @if let Some(err) = error {
                div class="rounded-lg bg-red-50 dark:bg-red-900/30 border border-red-200 dark:border-red-800 p-4 text-sm text-red-700 dark:text-red-300" {
                    (err)
                }
            }
            div {
                label for="email" class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" { "Email" }
                input
                    type="email"
                    id="email"
                    name="email"
                    required
                    class="w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-4 py-2.5 text-slate-900 dark:text-slate-100 placeholder-slate-400 dark:placeholder-slate-500 focus:border-slate-500 dark:focus:border-slate-400 focus:ring-1 focus:ring-slate-500 dark:focus:ring-slate-400"
                    placeholder="you@example.com";
            }
            div {
                label for="password" class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" { "Password" }
                input
                    type="password"
                    id="password"
                    name="password"
                    required
                    class="w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-4 py-2.5 text-slate-900 dark:text-slate-100 placeholder-slate-400 dark:placeholder-slate-500 focus:border-slate-500 dark:focus:border-slate-400 focus:ring-1 focus:ring-slate-500 dark:focus:ring-slate-400"
                    placeholder="••••••••";
            }
            button
                type="submit"
                class="w-full rounded-lg bg-slate-900 dark:bg-slate-100 px-4 py-2.5 text-sm font-medium text-white dark:text-slate-900 hover:bg-slate-800 dark:hover:bg-slate-200 transition"
            {
                "Sign In"
            }
        }
        p class="mt-6 text-center text-sm text-slate-600 dark:text-slate-400" {
            "Don't have an account? "
            a href="/register" class="font-medium text-slate-900 dark:text-slate-100 hover:underline" { "Register" }
        }
    }
}

fn register_form(error: Option<&str>) -> Markup {
    html! {
        form method="post" action="/register" class="space-y-6" {
            @if let Some(err) = error {
                div class="rounded-lg bg-red-50 dark:bg-red-900/30 border border-red-200 dark:border-red-800 p-4 text-sm text-red-700 dark:text-red-300" {
                    (err)
                }
            }
            div {
                label for="name" class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" { "Name" }
                input
                    type="text"
                    id="name"
                    name="name"
                    required
                    class="w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-4 py-2.5 text-slate-900 dark:text-slate-100 placeholder-slate-400 dark:placeholder-slate-500 focus:border-slate-500 dark:focus:border-slate-400 focus:ring-1 focus:ring-slate-500 dark:focus:ring-slate-400"
                    placeholder="Your name";
            }
            div {
                label for="email" class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" { "Email" }
                input
                    type="email"
                    id="email"
                    name="email"
                    required
                    class="w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-4 py-2.5 text-slate-900 dark:text-slate-100 placeholder-slate-400 dark:placeholder-slate-500 focus:border-slate-500 dark:focus:border-slate-400 focus:ring-1 focus:ring-slate-500 dark:focus:ring-slate-400"
                    placeholder="you@example.com";
            }
            div {
                label for="password" class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" { "Password" }
                input
                    type="password"
                    id="password"
                    name="password"
                    required
                    minlength="8"
                    class="w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-4 py-2.5 text-slate-900 dark:text-slate-100 placeholder-slate-400 dark:placeholder-slate-500 focus:border-slate-500 dark:focus:border-slate-400 focus:ring-1 focus:ring-slate-500 dark:focus:ring-slate-400"
                    placeholder="••••••••";
            }
            div {
                label for="password_confirm" class="block text-sm font-medium text-slate-700 dark:text-slate-300 mb-1" { "Confirm Password" }
                input
                    type="password"
                    id="password_confirm"
                    name="password_confirm"
                    required
                    minlength="8"
                    class="w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-4 py-2.5 text-slate-900 dark:text-slate-100 placeholder-slate-400 dark:placeholder-slate-500 focus:border-slate-500 dark:focus:border-slate-400 focus:ring-1 focus:ring-slate-500 dark:focus:ring-slate-400"
                    placeholder="••••••••";
            }
            button
                type="submit"
                class="w-full rounded-lg bg-slate-900 dark:bg-slate-100 px-4 py-2.5 text-sm font-medium text-white dark:text-slate-900 hover:bg-slate-800 dark:hover:bg-slate-200 transition"
            {
                "Create Account"
            }
        }
        p class="mt-6 text-center text-sm text-slate-600 dark:text-slate-400" {
            "Already have an account? "
            a href="/login" class="font-medium text-slate-900 dark:text-slate-100 hover:underline" { "Sign In" }
        }
    }
}

fn auth_layout(title: &str, content: impl Render) -> Markup {
    let heading = format!("{title} · Hofvarpnir");
    html! {
        (DOCTYPE)
        html lang="en" class="h-full" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (heading) }
                link rel="icon" type="image/x-icon" href="/assets/favicon.ico";
                link rel="icon" type="image/png" sizes="32x32" href="/assets/favicon-32x32.png";
                link rel="apple-touch-icon" href="/assets/apple-touch-icon.png";
                link rel="stylesheet" href="/assets/app.css";
                // Dark mode initialization (runs before body renders to prevent flash)
                (PreEscaped(r"<script>
                    (function() {
                        var stored = localStorage.getItem('darkMode');
                        if (stored === 'true' || (stored === null && window.matchMedia('(prefers-color-scheme: dark)').matches)) {
                            document.documentElement.classList.add('dark');
                        }
                    })();
                </script>"))
            }
            body class="min-h-full bg-gradient-to-b from-slate-100 via-slate-50 to-white dark:from-slate-900 dark:via-slate-900 dark:to-slate-950 text-slate-900 dark:text-slate-100" {
                div class="flex min-h-screen items-center justify-center px-4 py-12" {
                    div class="w-full max-w-md" {
                        div class="mb-8 text-center" {
                            // Dark mode toggle for auth pages
                            button
                                type="button"
                                class="absolute top-4 right-4 inline-flex items-center rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1.5 text-sm font-medium text-slate-600 dark:text-slate-300 transition hover:bg-slate-200 dark:hover:bg-slate-600"
                                onclick="(function(){ var h=document.documentElement; var d=h.classList.toggle('dark'); localStorage.setItem('darkMode',d); })()"
                            {
                                span class="dark:hidden" { "🌙" }
                                span class="hidden dark:inline" { "☀️" }
                            }
                            p class="text-xs uppercase tracking-[0.2em] text-slate-500 dark:text-slate-400" { "Hofvarpnir" }
                            h1 class="text-2xl font-semibold text-slate-900 dark:text-slate-100" { (title) }
                        }
                        div class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-8 shadow-sm backdrop-blur" {
                            (content)
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn dashboard_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;
    let (profiles_result, sources_result, videos_result, source_names_result, storage_usage_result) = tokio::join!(
        db::list_profiles(&state.pool),
        db::list_sources(&state.pool),
        db::list_videos(&state.pool, None),
        db::get_source_names_for_videos(&state.pool),
        db::get_storage_usage_by_profile(&state.pool)
    );

    let profiles = match profiles_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load profiles for dashboard");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load dashboard"),
            );
        }
    };

    let sources = match sources_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load sources for dashboard");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load dashboard"),
            );
        }
    };

    let videos = match videos_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load videos for dashboard");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load dashboard"),
            );
        }
    };

    let source_names = match source_names_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load source names for dashboard");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load dashboard"),
            );
        }
    };

    let storage_usage = match storage_usage_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load storage usage for dashboard");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load dashboard"),
            );
        }
    };

    let pending = videos
        .iter()
        .filter(|video| video.status == VideoStatus::Pending)
        .count();
    let downloading = videos
        .iter()
        .filter(|video| video.status == VideoStatus::Downloading)
        .count();
    let completed = videos
        .iter()
        .filter(|video| video.status == VideoStatus::Completed)
        .count();
    let failed = videos
        .iter()
        .filter(|video| {
            matches!(
                video.status,
                VideoStatus::Failed | VideoStatus::PermanentlyFailed
            )
        })
        .count();

    let recent = videos.iter().take(8).collect::<Vec<_>>();

    let page = layout_with_flash(
        "Dashboard",
        NavItem::Dashboard,
        flash,
        html! {
            div
                hx-ext="sse"
                sse-connect="/web/dashboard/events"
            {
                div id="dashboard-metrics" sse-swap="dashboard-update" hx-swap="innerHTML" {
                    (dashboard_metrics_markup(
                        profiles.len(),
                        sources.len(),
                        pending,
                        downloading,
                        completed,
                        failed,
                        &recent,
                        &source_names,
                        &storage_usage,
                    ))
                }
            }
        },
    );

    (StatusCode::OK, page)
}

#[allow(clippy::too_many_arguments)]
fn dashboard_metrics_markup(
    profiles: usize,
    sources: usize,
    pending: usize,
    downloading: usize,
    completed: usize,
    failed: usize,
    recent: &[&Video],
    source_names: &HashMap<Ulid, String>,
    storage_usage: &[db::ProfileStorageUsage],
) -> Markup {
    html! {
        div class="grid gap-4 md:grid-cols-2 xl:grid-cols-4" {
            (metric_card("Profiles", profiles, "Active download configurations"))
            (metric_card("Sources", sources, "Channels and playlists being tracked"))
            (metric_card("Pending", pending, "Queued for download"))
            (metric_card("In Progress", downloading, "Currently downloading"))
        }
        div class="mt-4 grid gap-4 md:grid-cols-2" {
            (metric_card("Completed", completed, "Successfully archived videos"))
            (metric_card("Failed", failed, "Need retry or manual check"))
        }
        (storage_usage_card_markup(storage_usage))
        section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Recent Downloads" }
            @if recent.is_empty() {
                p class="mt-3 text-sm text-slate-500 dark:text-slate-400" { "No downloads found yet." }
            } @else {
                ul class="mt-4 space-y-3" {
                    @for video in recent {
                        li class="flex items-center justify-between gap-3 rounded-xl border border-slate-100 dark:border-slate-700 bg-slate-50/80 dark:bg-slate-800/80 px-3 py-2" {
                            div class="min-w-0 flex-1" {
                                p class="truncate text-sm font-medium text-slate-900 dark:text-slate-100" { (video.title) }
                                p class="text-xs text-slate-500 dark:text-slate-400" { (video.platform) " / " (video.platform_video_id) }
                            }
                            @if let Some(source_name) = source_names.get(&video.id) {
                                span class="shrink-0 truncate max-w-[10rem] text-xs text-slate-600 dark:text-slate-300" title=(source_name) { (source_name) }
                            }
                            (status_badge(&video.status))
                        }
                    }
                }
            }
        }
    }
}

async fn dashboard_events_sse(
    _auth: AuthUser,
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.broadcaster.subscribe_invalidate();

    let stream = debounced_broadcast(rx).filter_map(move |()| {
        let pool = state.pool.clone();
        async move {
            let (
                profiles_result,
                sources_result,
                videos_result,
                source_names_result,
                storage_usage_result,
            ) = tokio::join!(
                db::list_profiles(&pool),
                db::list_sources(&pool),
                db::list_videos(&pool, None),
                db::get_source_names_for_videos(&pool),
                db::get_storage_usage_by_profile(&pool)
            );
            let profiles = profiles_result.ok()?;
            let sources = sources_result.ok()?;
            let videos = videos_result.ok()?;
            let source_names = source_names_result.ok()?;
            let storage_usage = storage_usage_result.ok()?;

            let pending = videos
                .iter()
                .filter(|v| v.status == VideoStatus::Pending)
                .count();
            let downloading = videos
                .iter()
                .filter(|v| v.status == VideoStatus::Downloading)
                .count();
            let completed = videos
                .iter()
                .filter(|v| v.status == VideoStatus::Completed)
                .count();
            let failed = videos
                .iter()
                .filter(|v| {
                    matches!(
                        v.status,
                        VideoStatus::Failed | VideoStatus::PermanentlyFailed
                    )
                })
                .count();

            let recent = videos.iter().take(8).collect::<Vec<_>>();
            let fragment = dashboard_metrics_markup(
                profiles.len(),
                sources.len(),
                pending,
                downloading,
                completed,
                failed,
                &recent,
                &source_names,
                &storage_usage,
            )
            .into_string();

            Some(Ok(Event::default()
                .event("dashboard-update")
                .data(fragment)))
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn downloads_events_sse(
    _auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.broadcaster.subscribe_invalidate();

    let stream = debounced_broadcast(rx).filter_map(move |()| {
        let pool = state.pool.clone();
        let query = query.clone();
        async move {
            let page_num = query.page.unwrap_or(1).max(1);
            let per_page = parse_downloads_page_size(query.per_page);
            let offset = (page_num - 1) * per_page;
            let search_query = query
                .search
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let status_filter = parse_download_status(query.status.as_deref());

            let (videos_result, count_result, source_names_result) = tokio::join!(
                db::list_videos_paginated(
                    &pool,
                    status_filter.clone(),
                    search_query,
                    per_page,
                    offset
                ),
                db::count_videos(&pool, status_filter, search_query),
                db::get_source_names_for_videos(&pool),
            );

            let videos = match videos_result {
                Ok(data) => data,
                Err(error) => {
                    tracing::warn!(%error, "failed to render downloads SSE fragment");
                    return None;
                }
            };
            let total = count_result.unwrap_or(0);
            let total_pages = if total == 0 {
                1
            } else {
                (total + per_page - 1) / per_page
            };
            let source_names = source_names_result.unwrap_or_default();

            let fragment = downloads_list_markup(
                &videos,
                &source_names,
                page_num,
                total_pages,
                total,
                query.status.as_deref(),
                search_query,
                per_page,
            )
            .into_string();

            Some(Ok(Event::default()
                .event("downloads-update")
                .data(fragment)))
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn activity_events_sse(
    _auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.broadcaster.subscribe_activity();

    let stream = debounced_broadcast(rx).filter_map(move |()| {
        let pool = state.pool.clone();
        let query = query.clone();
        async move {
            let params = ActivityParams::from_query(&query);
            let ActivityParams {
                page_num,
                per_page,
                offset,
                ref severity_filter,
                ..
            } = params;

            let (events_result, count_result) = tokio::join!(
                db::list_activity_events(
                    &pool,
                    per_page,
                    offset,
                    severity_filter.clone(),
                    None,
                    params.source_id,
                    params.search.as_deref(),
                ),
                db::count_activity_events(
                    &pool,
                    severity_filter.clone(),
                    None,
                    params.source_id,
                    params.search.as_deref(),
                )
            );

            let events = match events_result {
                Ok(data) => data,
                Err(error) => {
                    tracing::warn!(%error, "failed to render activity SSE fragment");
                    return None;
                }
            };
            let source_names = load_activity_source_names(&pool).await;
            let video_source_names = db::get_source_names_for_videos(&pool)
                .await
                .unwrap_or_default();
            let total = count_result.unwrap_or(0);
            let total_pages = if total == 0 {
                1
            } else {
                (total + per_page - 1) / per_page
            };
            let fragment = activity_content_markup(
                &events,
                &source_names,
                &video_source_names,
                page_num,
                total_pages,
                &params.severity_label,
                per_page,
                params.search.as_deref(),
                params.source.as_deref(),
            )
            .into_string();

            Some(Ok(Event::default().event("activity-update").data(fragment)))
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn sources_events_sse(
    auth: AuthUser,
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.broadcaster.subscribe_invalidate();
    let user_id = auth.user_id;

    let stream = debounced_broadcast(rx).filter_map(move |()| {
        let pool = state.pool.clone();
        async move {
            let profiles = db::list_profiles_for_user(&pool, user_id).await.ok()?;
            let all_sources = db::list_sources(&pool).await.ok()?;

            let profile_ids: std::collections::HashSet<_> = profiles.iter().map(|p| p.id).collect();
            let mut sources: Vec<_> = all_sources
                .into_iter()
                .filter(|s| profile_ids.contains(&s.profile_id))
                .collect();
            sources.sort_by_key(|s| s.display_name().to_lowercase());

            let fragment = sources_list_markup(&sources, None).into_string();
            Some(Ok(Event::default().event("sources-update").data(fragment)))
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn source_detail_events_sse(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let source_id = Ulid::from_string(id.trim()).ok();
    let rx = state.broadcaster.subscribe_invalidate();

    let stream = debounced_broadcast(rx).filter_map(move |()| {
        let pool = state.pool.clone();
        async move {
            let source_id = source_id?;
            let source = db::get_source(&pool, source_id).await.ok()?;
            let videos = db::list_videos_for_source(&pool, source_id).await.ok()?;

            let fragment = source_detail_content_markup(&source, &videos).into_string();
            Some(Ok(Event::default()
                .event("source-detail-update")
                .data(fragment)))
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

async fn profiles_page(
    auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;
    // List profiles for the current user only
    let profiles = match db::list_profiles_for_user(&state.pool, auth.user_id).await {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load profiles for profiles page");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load profiles page"),
            );
        }
    };

    let page = layout_with_flash(
        "Profiles",
        NavItem::Profiles,
        flash,
        html! {
            section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Create Profile" }
                form class="mt-4 grid gap-4 md:grid-cols-2" method="post" action="/profiles" {
                    div {
                        label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for="quality" { "Quality" }
                        select class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm" name="quality" id="quality" required {
                            @for quality in quality_options() {
                                option value=(quality.value) { (quality.label) }
                            }
                        }
                    }
                    div {
                        label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for="output_preset" { "Output Preset" }
                        select class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm" name="output_preset" id="output_preset" required {
                            @for preset in output_preset_options() {
                                option value=(preset.value) selected[(preset.value == "browser")] { (preset.label) }
                            }
                        }
                    }
                    (input_text("Name", "name", "Daily Archive", true, ""))
                    (input_text("Naming Template", "naming_template", "{{source_custom_name/or default}}/{{year}}/{{title}}.{{ext}}", true, "{{source_custom_name/or default}}/{{year}}/{{title}}.{{ext}}"))
                    (input_text("Output Directory", "output_dir", "/data/videos", true, ""))
                    (input_number("Storage Quota (GB)", "storage_quota_gb", "100", true, "100"))
                    (input_number("Retention Days", "retention_days", "Optional", false, ""))
                    div class="flex items-center gap-4" {
                        label class="inline-flex items-center gap-2 text-sm text-slate-700 dark:text-slate-300" {
                            input type="checkbox" name="include_livestreams";
                            "Include Livestream VODs"
                        }
                        label class="inline-flex items-center gap-2 text-sm text-slate-700 dark:text-slate-300" {
                            input type="checkbox" name="include_shorts";
                            "Include Shorts"
                        }
                    }
                    div class="md:col-span-2" {
                        button class="inline-flex items-center rounded-lg bg-sky-600 dark:bg-sky-500 px-4 py-2 text-sm font-medium text-white transition hover:bg-sky-700 dark:hover:bg-sky-600" type="submit" { "Create Profile" }
                    }
                }
            }

            section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Existing Profiles" }
                @if profiles.is_empty() {
                    p class="mt-3 text-sm text-slate-500 dark:text-slate-400" { "No profiles yet." }
                } @else {
                    div class="mt-4 space-y-4" {
                        @for profile in &profiles {
                            (profile_editor(profile))
                        }
                    }
                }
            }
        },
    );

    (StatusCode::OK, page)
}

async fn create_profile(
    auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<ProfileForm>,
) -> impl IntoResponse {
    let retention_days = match parse_optional_i32(form.retention_days.as_deref(), "Retention days")
    {
        Ok(value) => value,
        Err(message) => {
            return (StatusCode::BAD_REQUEST, error_page(&message)).into_response();
        }
    };

    let naming_template = form.naming_template.trim();
    if let Err(message) = validate_output_template(naming_template) {
        return (StatusCode::BAD_REQUEST, error_page(&message)).into_response();
    }

    let create = CreateProfile {
        user_id: auth.user_id,
        name: form.name.trim(),
        quality: form.quality.into(),
        output_preset: form.output_preset.into(),
        naming_template,
        output_dir: form.output_dir.trim(),
        include_livestreams: form.include_livestreams.is_some(),
        include_shorts: form.include_shorts.is_some(),
        storage_quota_bytes: form.storage_quota_gb * 1_000_000_000, // Convert GB to bytes
        retention_days,
    };

    match db::create_profile(&state.pool, create).await {
        Ok(profile) => {
            state
                .broadcaster
                .log_and_broadcast(
                    &state.pool,
                    ActivityEventType::ProfileCreated,
                    ActivitySeverity::Info,
                    &format!("Created profile \"{}\"", profile.name),
                    None,
                    None,
                    Some(profile.id),
                )
                .await;
            set_flash(
                &session,
                "success",
                &format!("Profile \"{}\" created", profile.name),
            )
            .await;
            Redirect::to("/profiles").into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to create profile from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to create profile"),
            )
                .into_response()
        }
    }
}

async fn update_profile(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    Form(form): Form<ProfileForm>,
) -> impl IntoResponse {
    let Ok(profile_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid profile ID provided"),
        )
            .into_response();
    };

    let retention_days = match parse_optional_i32(form.retention_days.as_deref(), "Retention days")
    {
        Ok(value) => value,
        Err(message) => {
            return (StatusCode::BAD_REQUEST, error_page(&message)).into_response();
        }
    };

    let naming_template = form.naming_template.trim();
    if let Err(message) = validate_output_template(naming_template) {
        return (StatusCode::BAD_REQUEST, error_page(&message)).into_response();
    }

    let update = UpdateProfile {
        name: Some(form.name.trim()),
        quality: Some(form.quality.into()),
        output_preset: Some(form.output_preset.into()),
        naming_template: Some(naming_template),
        output_dir: Some(form.output_dir.trim()),
        include_livestreams: Some(form.include_livestreams.is_some()),
        include_shorts: Some(form.include_shorts.is_some()),
        storage_quota_bytes: Some(form.storage_quota_gb * 1_000_000_000), // Convert GB to bytes
        retention_days: Some(retention_days),
    };

    match db::update_profile(&state.pool, profile_id, update).await {
        Ok(_) => {
            state.broadcaster.invalidate();
            set_flash(&session, "success", "Profile updated").await;
            Redirect::to("/profiles").into_response()
        }
        Err(db::DbError::NotFound) => {
            (StatusCode::NOT_FOUND, error_page("Profile not found")).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to update profile from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to update profile"),
            )
                .into_response()
        }
    }
}

async fn delete_profile(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(profile_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid profile ID provided"),
        )
            .into_response();
    };

    match db::delete_profile(&state.pool, profile_id).await {
        Ok(()) => {
            state
                .broadcaster
                .log_and_broadcast(
                    &state.pool,
                    ActivityEventType::ProfileDeleted,
                    ActivitySeverity::Info,
                    &format!("Deleted profile {profile_id}"),
                    None,
                    None,
                    Some(profile_id),
                )
                .await;
            set_flash(&session, "success", "Profile deleted").await;
            Redirect::to("/profiles").into_response()
        }
        Err(db::DbError::NotFound) => {
            (StatusCode::NOT_FOUND, error_page("Profile not found")).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to delete profile from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to delete profile"),
            )
                .into_response()
        }
    }
}

/// Plain GET search form shared by the sources and schedule pages.
///
/// Both filter the same `Source` set in-memory, so neither needs htmx here —
/// a full navigation keeps the URL shareable and the back button meaningful.
fn source_search_form(action: &str, placeholder: &str, search: Option<&str>) -> Markup {
    html! {
        form method="get" action=(action) class="flex gap-2" {
            input
                type="text"
                name="search"
                placeholder=(placeholder)
                value=(search.unwrap_or(""))
                class="rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-900 dark:text-slate-100";
            button type="submit"
                class="rounded-lg bg-slate-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-slate-700"
            { "Search" }
            @if search.is_some() {
                a href=(action)
                    class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                { "Clear" }
            }
        }
    }
}

/// Case-insensitive match of a source against a search term.
///
/// Covers every name a source can be known by — the user's custom label, the
/// platform channel/playlist title, and the URL — because which one is
/// populated varies by source type and indexing state.
fn source_matches_search(source: &Source, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    let haystacks = [
        source.custom_name.as_deref(),
        source.channel_title.as_deref(),
        Some(source.url.as_str()),
    ];
    haystacks
        .into_iter()
        .flatten()
        .any(|value| value.to_lowercase().contains(&needle))
}

async fn sources_page(
    auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(query): axum::extract::Query<SourcesQuery>,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    // Get profiles for the current user to populate the dropdown
    let profiles = match db::list_profiles_for_user(&state.pool, auth.user_id).await {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load profiles for sources page");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load sources page"),
            );
        }
    };

    // Get sources for the user's profiles
    let mut sources = match db::list_sources(&state.pool).await {
        Ok(data) => {
            // Filter to only show sources belonging to user's profiles
            let profile_ids: std::collections::HashSet<_> = profiles.iter().map(|p| p.id).collect();
            data.into_iter()
                .filter(|s| profile_ids.contains(&s.profile_id))
                .collect::<Vec<_>>()
        }
        Err(error) => {
            tracing::error!(%error, "failed to load sources for sources page");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load sources page"),
            );
        }
    };

    if let Some(needle) = search {
        sources.retain(|source| source_matches_search(source, needle));
    }

    // Sort sources alphabetically by display name
    sources.sort_by_key(|s| s.display_name().to_lowercase());

    // Calculate default cutoff date (7 days ago)
    let default_cutoff_date = (Utc::now() - chrono::Duration::days(7))
        .format("%Y-%m-%d")
        .to_string();

    let page = layout_with_flash(
        "Sources",
        NavItem::Sources,
        flash,
        html! {
            section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Create Source" }
                form class="mt-4 grid gap-4 md:grid-cols-2" method="post" action="/sources" {
                    div {
                        label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for="profile_id" { "Profile" }
                        select class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm" name="profile_id" id="profile_id" required {
                            @for profile in &profiles {
                                option value=(profile.id.to_string()) {
                                    (profile.name) " (" (profile.id.to_string()) ")"
                                }
                            }
                        }
                    }
                    div {
                        label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for="source_type" { "Source Type" }
                        select class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm" name="source_type" id="source_type" required {
                            option value="channel" { "Channel" }
                            option value="playlist" { "Playlist" }
                        }
                    }
                    (input_text("URL", "url", "https://youtube.com/@channel", true, ""))
                    (input_text("Custom Name", "custom_name", "Optional label", false, ""))
                    (input_index_frequency("Index Frequency", "index_frequency_secs", 43200))
                    (input_cutoff_date("Cutoff Date", "cutoff_date", &default_cutoff_date))
                    (input_number("Retention Days", "retention_days", "Optional", false, ""))
                    div class="md:col-span-2" {
                        button class="inline-flex items-center rounded-lg bg-sky-600 dark:bg-sky-500 px-4 py-2 text-sm font-medium text-white transition hover:bg-sky-700 dark:hover:bg-sky-600" type="submit" { "Create Source" }
                    }
                }
            }

            section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                div class="flex flex-wrap items-center justify-between gap-3" {
                    h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Existing Sources" }
                    (source_search_form("/sources", "Search name, channel or URL...", search))
                }
                // No SSE here: a live refresh would re-render the unfiltered
                // list and silently discard the user's search.
                @if search.is_some() {
                    div id="sources-list" {
                        (sources_list_markup(&sources, search))
                    }
                } @else {
                    div hx-ext="sse" sse-connect="/web/sources/events" {
                        div id="sources-list" sse-swap="sources-update" hx-swap="innerHTML show:none" {
                            (sources_list_markup(&sources, search))
                        }
                    }
                }
            }
        },
    );

    (StatusCode::OK, page)
}

async fn create_source(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<SourceForm>,
) -> impl IntoResponse {
    let Ok(profile_id) = Ulid::from_string(form.profile_id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid profile ID provided"),
        )
            .into_response();
    };

    let Ok(cutoff_date) = NaiveDate::parse_from_str(form.cutoff_date.trim(), "%Y-%m-%d") else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid cutoff date. Use YYYY-MM-DD."),
        )
            .into_response();
    };

    let retention_days = match parse_optional_i32(form.retention_days.as_deref(), "Retention days")
    {
        Ok(value) => value,
        Err(message) => {
            return (StatusCode::BAD_REQUEST, error_page(&message)).into_response();
        }
    };

    let custom_name = form
        .custom_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());

    let create = CreateSource {
        profile_id,
        url: form.url.trim(),
        source_type: form.source_type.into(),
        custom_name,
        index_frequency_secs: form.index_frequency_secs,
        cutoff_date,
        retention_days,
    };

    match db::create_source(&state.pool, create).await {
        Ok(source) => {
            let name = source.display_name();
            state
                .broadcaster
                .log_and_broadcast(
                    &state.pool,
                    ActivityEventType::SourceCreated,
                    ActivitySeverity::Info,
                    &format!("Added source \"{name}\""),
                    Some(source.id),
                    None,
                    Some(profile_id),
                )
                .await;
            set_flash(&session, "success", &format!("Source \"{name}\" added")).await;
            Redirect::to("/sources").into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to create source from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to create source"),
            )
                .into_response()
        }
    }
}

async fn update_source(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    Form(form): Form<SourceForm>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        )
            .into_response();
    };

    let Ok(cutoff_date) = NaiveDate::parse_from_str(form.cutoff_date.trim(), "%Y-%m-%d") else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid cutoff date. Use YYYY-MM-DD."),
        )
            .into_response();
    };

    let retention_days = match parse_optional_i32(form.retention_days.as_deref(), "Retention days")
    {
        Ok(value) => value,
        Err(message) => {
            return (StatusCode::BAD_REQUEST, error_page(&message)).into_response();
        }
    };

    let custom_name = form
        .custom_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());

    let update = UpdateSource {
        url: Some(form.url.trim()),
        source_type: Some(form.source_type.into()),
        custom_name: Some(custom_name),
        index_frequency_secs: Some(form.index_frequency_secs),
        cutoff_date: Some(cutoff_date),
        retention_days: Some(retention_days),
    };

    match db::update_source(&state.pool, source_id, update).await {
        Ok(_) => {
            state.broadcaster.invalidate();
            set_flash(&session, "success", "Source updated").await;
            Redirect::to("/sources").into_response()
        }
        Err(db::DbError::NotFound) => {
            (StatusCode::NOT_FOUND, error_page("Source not found")).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to update source from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to update source"),
            )
                .into_response()
        }
    }
}

async fn delete_source(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        )
            .into_response();
    };

    match db::delete_source(&state.pool, source_id).await {
        Ok(()) => {
            state
                .broadcaster
                .log_and_broadcast(
                    &state.pool,
                    ActivityEventType::SourceDeleted,
                    ActivitySeverity::Info,
                    &format!("Deleted source {source_id}"),
                    Some(source_id),
                    None,
                    None,
                )
                .await;
            set_flash(&session, "success", "Source deleted").await;
            Redirect::to("/sources").into_response()
        }
        Err(db::DbError::NotFound) => {
            (StatusCode::NOT_FOUND, error_page("Source not found")).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to delete source from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to delete source"),
            )
                .into_response()
        }
    }
}

async fn toggle_source_enabled(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        )
            .into_response();
    };

    // First get the current state
    let source = match db::get_source(&state.pool, source_id).await {
        Ok(s) => s,
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Source not found")).into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to get source for toggle");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to toggle source"),
            )
                .into_response();
        }
    };

    let new_enabled = !source.enabled;
    match db::set_source_enabled(&state.pool, source_id, new_enabled).await {
        Ok(()) => {
            let status = if new_enabled { "enabled" } else { "disabled" };
            let name = source.display_name();
            state
                .broadcaster
                .log_and_broadcast(
                    &state.pool,
                    ActivityEventType::SourceUpdated,
                    ActivitySeverity::Info,
                    &format!("Source \"{name}\" {status}"),
                    Some(source_id),
                    None,
                    Some(source.profile_id),
                )
                .await;
            set_flash(&session, "success", &format!("Source {status}")).await;
            Redirect::to("/sources").into_response()
        }
        Err(db::DbError::NotFound) => {
            (StatusCode::NOT_FOUND, error_page("Source not found")).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to toggle source enabled state");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to toggle source"),
            )
                .into_response()
        }
    }
}

async fn trigger_index(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        )
            .into_response();
    };

    // Redirect back to the referring page, defaulting to /sources
    let redirect_to = headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|referer| referer.rsplit_once('/').map(|(_, path)| path))
        .map_or("/sources", |path| {
            if path == "schedule" {
                "/schedule"
            } else {
                "/sources"
            }
        });

    match state.scheduler.ask(IndexSource { source_id }).await {
        Ok(()) => {
            set_flash(&session, "info", "Indexing triggered").await;
            Redirect::to(redirect_to).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to trigger source index from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to trigger indexing for this source"),
            )
                .into_response()
        }
    }
}

async fn trigger_cleanup(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
) -> impl IntoResponse {
    match state.cleanup.ask(RunCleanup).await {
        Ok(result) => {
            tracing::info!(
                retention = result.retention_cleaned,
                quota = result.quota_cleaned,
                bytes_freed = result.bytes_freed,
                "Manual cleanup triggered from web UI"
            );
            let total = result.retention_cleaned + result.quota_cleaned;
            set_flash(
                &session,
                "success",
                &format!("Cleanup done: {total} files cleaned"),
            )
            .await;
            Redirect::to("/schedule").into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to trigger cleanup from web UI");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to trigger cleanup"),
            )
                .into_response()
        }
    }
}

// ============================================================================
// API Keys Page
// ============================================================================

/// Session key for storing newly created API key token (shown once).
const NEW_API_KEY_TOKEN: &str = "new_api_key_token";

async fn api_keys_page(
    auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;

    // Check if we have a newly created key token to display
    let new_token: Option<String> = session.get(NEW_API_KEY_TOKEN).await.ok().flatten();
    if new_token.is_some() {
        let _ = session.remove::<String>(NEW_API_KEY_TOKEN).await;
    }

    let api_keys = match db::list_api_keys(&state.pool, auth.user_id).await {
        Ok(keys) => keys,
        Err(error) => {
            tracing::error!(%error, "failed to load API keys");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load API keys"),
            );
        }
    };

    let page = layout_with_flash(
        "API Keys",
        NavItem::ApiKeys,
        flash,
        api_keys_content(&api_keys, new_token.as_deref()),
    );

    (StatusCode::OK, page)
}

fn api_keys_content(api_keys: &[ApiKey], new_token: Option<&str>) -> Markup {
    html! {
        // Show new token modal if just created
        @if let Some(token) = new_token {
            div class="mb-6 rounded-2xl border-2 border-emerald-500 dark:border-emerald-400 bg-emerald-50 dark:bg-emerald-900/30 p-6" {
                div class="flex items-start gap-3" {
                    span class="text-2xl" { "🔑" }
                    div class="flex-1" {
                        h3 class="text-lg font-semibold text-emerald-900 dark:text-emerald-100" {
                            "API Key Created"
                        }
                        p class="mt-1 text-sm text-emerald-800 dark:text-emerald-200" {
                            "Copy this key now. You won't be able to see it again."
                        }
                        div class="mt-3 rounded-lg bg-slate-900 dark:bg-slate-950 p-3" {
                            code class="block break-all text-sm text-emerald-400 select-all" {
                                (token)
                            }
                        }
                    }
                }
            }
        }

        // Create form
        section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Create API Key" }
            form class="mt-4 space-y-4" method="post" action="/settings/api-keys" {
                (input_text("Name", "name", "Bot, backup script, etc.", true, ""))

                div {
                    label class="block text-sm font-medium text-slate-700 dark:text-slate-300" { "Scopes" }
                    p class="mt-1 text-xs text-slate-500 dark:text-slate-400" { "Select at least one scope" }
                    div class="mt-2 flex flex-wrap gap-4" {
                        label class="inline-flex items-center gap-2 text-sm text-slate-700 dark:text-slate-300" {
                            input type="checkbox" name="scope_read" value="1" checked;
                            "Read"
                            span class="text-xs text-slate-500" { "(list, get)" }
                        }
                        label class="inline-flex items-center gap-2 text-sm text-slate-700 dark:text-slate-300" {
                            input type="checkbox" name="scope_write" value="1";
                            "Write"
                            span class="text-xs text-slate-500" { "(create, update, trigger)" }
                        }
                        label class="inline-flex items-center gap-2 text-sm text-slate-700 dark:text-slate-300" {
                            input type="checkbox" name="scope_delete" value="1";
                            "Delete"
                        }
                    }
                }

                div {
                    label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for="expires_in" { "Expiration" }
                    select class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm" name="expires_in" id="expires_in" {
                        option value="1h" { "1 hour" }
                        option value="1d" { "1 day" }
                        option value="7d" { "7 days" }
                        option value="30d" selected { "30 days" }
                        option value="90d" { "90 days" }
                        option value="365d" { "1 year" }
                        option value="" { "Never" }
                    }
                }

                button class="inline-flex items-center rounded-lg bg-sky-600 dark:bg-sky-500 px-4 py-2 text-sm font-medium text-white transition hover:bg-sky-700 dark:hover:bg-sky-600" type="submit" {
                    "Generate Key"
                }
            }
        }

        // Existing keys
        section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Your API Keys" }
            @if api_keys.is_empty() {
                p class="mt-4 rounded-lg border border-dashed border-slate-300 dark:border-slate-600 bg-slate-50 dark:bg-slate-800 px-4 py-8 text-center text-sm text-slate-500 dark:text-slate-400" {
                    "No API keys yet. Create one above to get started."
                }
            } @else {
                div class="mt-4 space-y-4" {
                    @for key in api_keys {
                        (api_key_card(key))
                    }
                }
            }
        }
    }
}

fn api_key_card(key: &ApiKey) -> Markup {
    let is_expired = key.is_expired();
    let card_classes = if is_expired {
        "rounded-xl border border-rose-200 dark:border-rose-800 bg-rose-50/50 dark:bg-rose-900/20 p-4"
    } else {
        "rounded-xl border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 p-4"
    };

    html! {
        div class=(card_classes) {
            div class="flex flex-wrap items-start justify-between gap-4" {
                div class="flex-1 min-w-0" {
                    div class="flex items-center gap-2" {
                        h3 class="font-medium text-slate-900 dark:text-slate-100 truncate" { (key.name) }
                        @if is_expired {
                            span class="inline-flex rounded-full bg-rose-100 dark:bg-rose-900/50 px-2 py-0.5 text-xs font-medium text-rose-700 dark:text-rose-300" {
                                "Expired"
                            }
                        }
                    }
                    p class="mt-1 font-mono text-sm text-slate-500 dark:text-slate-400" {
                        (key.prefix) "..."
                    }
                    div class="mt-2 flex flex-wrap gap-1" {
                        @for scope in &key.scopes {
                            (scope_badge(*scope))
                        }
                    }
                    div class="mt-2 text-xs text-slate-500 dark:text-slate-400" {
                        @if let Some(last_used) = key.last_used_at {
                            "Last used " (format_relative_time(last_used))
                        } @else {
                            "Never used"
                        }
                        " · Created " (format_relative_time(key.created_at))
                        @if let Some(expires) = key.expires_at {
                            @if is_expired {
                                " · Expired " (format_relative_time(expires))
                            } @else {
                                " · Expires " (format_relative_time(expires))
                            }
                        }
                    }
                }
                div class="flex items-center gap-2" {
                    // Events toggle
                    button
                        class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                        hx-get=(format!("/settings/api-keys/{}/events", key.id))
                        hx-target=(format!("#events-{}", key.id))
                        hx-swap="innerHTML"
                    {
                        "History"
                    }
                    // Roll button
                    form method="post" action=(format!("/settings/api-keys/{}/roll", key.id)) style="display:inline" {
                        button
                            class="rounded-lg border border-amber-200 dark:border-amber-700 bg-amber-50 dark:bg-amber-900/50 px-3 py-1.5 text-xs font-medium text-amber-700 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900"
                            type="submit"
                            onclick="return confirm('Roll this key? The current key will stop working immediately.')"
                        {
                            "Roll"
                        }
                    }
                    // Delete button
                    form method="post" action=(format!("/settings/api-keys/{}/delete", key.id)) style="display:inline" {
                        button
                            class="rounded-lg border border-rose-200 dark:border-rose-700 bg-rose-50 dark:bg-rose-900/50 px-3 py-1.5 text-xs font-medium text-rose-700 dark:text-rose-300 hover:bg-rose-100 dark:hover:bg-rose-900"
                            type="submit"
                            onclick="return confirm('Delete this API key? This cannot be undone.')"
                        {
                            "Delete"
                        }
                    }
                }
            }
            // Events container (loaded via htmx)
            div id=(format!("events-{}", key.id)) class="mt-3" {}
        }
    }
}

fn scope_badge(scope: ApiKeyScope) -> Markup {
    let (label, class) = match scope {
        ApiKeyScope::Read => (
            "read",
            "bg-sky-100 dark:bg-sky-900/50 text-sky-700 dark:text-sky-300",
        ),
        ApiKeyScope::Write => (
            "write",
            "bg-amber-100 dark:bg-amber-900/50 text-amber-700 dark:text-amber-300",
        ),
        ApiKeyScope::Delete => (
            "delete",
            "bg-rose-100 dark:bg-rose-900/50 text-rose-700 dark:text-rose-300",
        ),
    };
    html! {
        span class=(format!("inline-flex rounded-full px-2 py-0.5 text-xs font-medium {class}")) {
            (label)
        }
    }
}

/// Parse a duration token from the API key expiration select field.
///
/// - `""` means "never expires" (`Ok(None)`).
/// - A token ending in `h` is parsed as hours; a token ending in `d` is parsed as days.
/// - Any other suffix, an empty/non-positive numeric prefix, or a non-numeric prefix is
///   rejected with `Err(())`.
fn parse_expires_in(token: &str) -> Result<Option<chrono::Duration>, ()> {
    if token.is_empty() {
        return Ok(None);
    }

    let (digits, build_duration): (&str, fn(i64) -> chrono::Duration) =
        if let Some(digits) = token.strip_suffix('h') {
            (digits, chrono::Duration::hours)
        } else if let Some(digits) = token.strip_suffix('d') {
            (digits, chrono::Duration::days)
        } else {
            return Err(());
        };

    if digits.is_empty() {
        return Err(());
    }

    let value: i64 = digits.parse().map_err(|_| ())?;
    if value <= 0 {
        return Err(());
    }

    Ok(Some(build_duration(value)))
}

#[cfg(test)]
mod expires_in_tests {
    use super::parse_expires_in;

    #[test]
    fn never_token_is_none() {
        assert_eq!(parse_expires_in(""), Ok(None));
    }

    #[test]
    fn hour_token() {
        assert_eq!(parse_expires_in("1h"), Ok(Some(chrono::Duration::hours(1))));
    }

    #[test]
    fn day_tokens() {
        assert_eq!(parse_expires_in("1d"), Ok(Some(chrono::Duration::days(1))));
        assert_eq!(parse_expires_in("7d"), Ok(Some(chrono::Duration::days(7))));
        assert_eq!(
            parse_expires_in("30d"),
            Ok(Some(chrono::Duration::days(30)))
        );
    }

    #[test]
    fn invalid_tokens_are_rejected() {
        assert_eq!(parse_expires_in("abc"), Err(()));
        assert_eq!(parse_expires_in("0d"), Err(()));
        assert_eq!(parse_expires_in("5x"), Err(()));
        assert_eq!(parse_expires_in("d"), Err(()));
        assert_eq!(parse_expires_in("-1h"), Err(()));
    }
}

async fn create_api_key(
    auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<ApiKeyForm>,
) -> impl IntoResponse {
    let name = form.name.trim();
    if name.is_empty() {
        set_flash(&session, "error", "Name is required").await;
        return Redirect::to("/settings/api-keys").into_response();
    }

    // Collect scopes
    let mut scopes = Vec::new();
    if form.scope_read.is_some() {
        scopes.push(ApiKeyScope::Read);
    }
    if form.scope_write.is_some() {
        scopes.push(ApiKeyScope::Write);
    }
    if form.scope_delete.is_some() {
        scopes.push(ApiKeyScope::Delete);
    }

    if scopes.is_empty() {
        set_flash(&session, "error", "At least one scope is required").await;
        return Redirect::to("/settings/api-keys").into_response();
    }

    // Parse expiration
    let Ok(duration) = parse_expires_in(form.expires_in.as_deref().unwrap_or("")) else {
        set_flash(&session, "error", "Invalid expiration value").await;
        return Redirect::to("/settings/api-keys").into_response();
    };
    let expires_at = duration.map(|d| Utc::now() + d);

    // Generate the key
    let generated = generate_api_key();

    let create = CreateApiKey {
        user_id: auth.user_id,
        name,
        prefix: &generated.prefix,
        key_hash: &generated.hash,
        scopes: &scopes,
        expires_at,
    };

    match db::create_api_key(&state.pool, create).await {
        Ok(_) => {
            // Store the token in session to display once
            let _ = session.insert(NEW_API_KEY_TOKEN, generated.token).await;
            set_flash(&session, "success", "API key created").await;
            Redirect::to("/settings/api-keys").into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to create API key");
            if error.to_string().contains("duplicate") || error.to_string().contains("unique") {
                set_flash(
                    &session,
                    "error",
                    "An API key with that name already exists",
                )
                .await;
            } else {
                set_flash(&session, "error", "Failed to create API key").await;
            }
            Redirect::to("/settings/api-keys").into_response()
        }
    }
}

async fn roll_api_key(
    auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(key_id) = Ulid::from_string(id.trim()) else {
        set_flash(&session, "error", "Invalid key ID").await;
        return Redirect::to("/settings/api-keys").into_response();
    };

    // Verify ownership
    let existing = match db::get_api_key(&state.pool, key_id).await {
        Ok(Some(key)) if key.user_id == auth.user_id => key,
        Ok(_) => {
            set_flash(&session, "error", "API key not found").await;
            return Redirect::to("/settings/api-keys").into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to get API key for roll");
            set_flash(&session, "error", "Failed to roll API key").await;
            return Redirect::to("/settings/api-keys").into_response();
        }
    };

    // Generate new key
    let generated = generate_api_key();

    // Keep same expiration policy (reset from now if it had one)
    let new_expires_at = existing.expires_at.map(|old_exp| {
        let duration = old_exp - existing.created_at;
        Utc::now() + duration
    });

    match db::roll_api_key(
        &state.pool,
        key_id,
        auth.user_id,
        &generated.prefix,
        &generated.hash,
        new_expires_at,
        None,
    )
    .await
    {
        Ok(_) => {
            // Store the new token in session to display once
            let _ = session.insert(NEW_API_KEY_TOKEN, generated.token).await;
            set_flash(&session, "success", "API key rolled - copy the new key").await;
            Redirect::to("/settings/api-keys").into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to roll API key");
            set_flash(&session, "error", "Failed to roll API key").await;
            Redirect::to("/settings/api-keys").into_response()
        }
    }
}

async fn delete_api_key(
    auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(key_id) = Ulid::from_string(id.trim()) else {
        set_flash(&session, "error", "Invalid key ID").await;
        return Redirect::to("/settings/api-keys").into_response();
    };

    match db::delete_api_key(&state.pool, key_id, auth.user_id, None).await {
        Ok(()) => {
            set_flash(&session, "success", "API key deleted").await;
            Redirect::to("/settings/api-keys").into_response()
        }
        Err(db::DbError::NotFound) => {
            set_flash(&session, "error", "API key not found").await;
            Redirect::to("/settings/api-keys").into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to delete API key");
            set_flash(&session, "error", "Failed to delete API key").await;
            Redirect::to("/settings/api-keys").into_response()
        }
    }
}

async fn api_key_events(
    auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(key_id) = Ulid::from_string(id.trim()) else {
        return (StatusCode::BAD_REQUEST, "Invalid key ID").into_response();
    };

    // Verify ownership first
    match db::get_api_key(&state.pool, key_id).await {
        Ok(Some(key)) if key.user_id == auth.user_id => {}
        _ => {
            // Also check if we own events for a deleted key
            let events = db::list_api_key_events(&state.pool, key_id).await.ok();
            if !events
                .as_ref()
                .is_some_and(|e| e.iter().any(|ev| ev.user_id == auth.user_id))
            {
                return (StatusCode::NOT_FOUND, "API key not found").into_response();
            }
        }
    }

    let events = match db::list_api_key_events(&state.pool, key_id).await {
        Ok(events) => events,
        Err(error) => {
            tracing::error!(%error, "failed to load API key events");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to load events").into_response();
        }
    };

    let markup = html! {
        @if events.is_empty() {
            p class="text-xs text-slate-500 dark:text-slate-400" { "No events recorded." }
        } @else {
            div class="rounded-lg border border-slate-200 dark:border-slate-700 bg-white dark:bg-slate-900 divide-y divide-slate-100 dark:divide-slate-800" {
                @for event in &events {
                    div class="px-3 py-2 flex items-center justify-between text-xs" {
                        span class="font-medium text-slate-700 dark:text-slate-300" {
                            @match event.event_type {
                                ApiKeyEventType::Created => "Created",
                                ApiKeyEventType::Rolled => "Rolled",
                                ApiKeyEventType::Deleted => "Deleted",
                            }
                        }
                        span class="text-slate-500 dark:text-slate-400" {
                            (event.created_at.format("%Y-%m-%d %H:%M"))
                        }
                    }
                }
            }
        }
    };

    (StatusCode::OK, markup).into_response()
}

fn format_relative_time(dt: chrono::DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(dt);

    if duration.num_seconds() < 0 {
        // Future date
        let future_duration = dt.signed_duration_since(now);
        if future_duration.num_days() > 0 {
            return format!("in {}d", future_duration.num_days());
        }
        if future_duration.num_hours() > 0 {
            return format!("in {}h", future_duration.num_hours());
        }
        return format!("in {}m", future_duration.num_minutes().max(1));
    }

    if duration.num_days() > 30 {
        return dt.format("%Y-%m-%d").to_string();
    }
    if duration.num_days() > 0 {
        return format!("{}d ago", duration.num_days());
    }
    if duration.num_hours() > 0 {
        return format!("{}h ago", duration.num_hours());
    }
    if duration.num_minutes() > 0 {
        return format!("{}m ago", duration.num_minutes());
    }
    "just now".to_string()
}

async fn trigger_metadata(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        )
            .into_response();
    };

    // Check if source has channel metadata
    match db::get_source(&state.pool, source_id).await {
        Ok(source) if source.channel_thumbnail_url.is_none() => {
            return (
                StatusCode::BAD_REQUEST,
                error_page(
                    "Source has no channel metadata. Run 'Trigger Index' first to fetch \
                     channel information from YouTube.",
                ),
            )
                .into_response();
        }
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Source not found")).into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to get source");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to get source"),
            )
                .into_response();
        }
        Ok(_) => {}
    }

    match state
        .jellyfin_metadata
        .ask(TriggerSourceMetadata { source_id })
        .await
    {
        Ok(result) if result.success => {
            set_flash(&session, "success", "Metadata generation started").await;
            Redirect::to("/sources").into_response()
        }
        Ok(result) => {
            let error_msg = result.error.unwrap_or_else(|| "Unknown error".to_string());
            tracing::error!(error = %error_msg, "failed to generate metadata from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page(&format!("Failed to generate metadata: {error_msg}")),
            )
                .into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to trigger metadata generation from web form");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to trigger metadata generation for this source"),
            )
                .into_response()
        }
    }
}

async fn source_detail_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Ok(source_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid source ID provided"),
        );
    };

    let source = match db::get_source(&state.pool, source_id).await {
        Ok(s) => s,
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Source not found"));
        }
        Err(error) => {
            tracing::error!(%error, "failed to load source");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load source"),
            );
        }
    };

    let videos = match db::list_videos_for_source(&state.pool, source_id).await {
        Ok(v) => v,
        Err(error) => {
            tracing::error!(%error, "failed to load videos for source");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load videos"),
            );
        }
    };

    let events_url = format!("/web/sources/{source_id}/events");

    let page = layout(
        &format!("Source: {}", source.display_name()),
        NavItem::Sources,
        html! {
            div class="mb-4" {
                a href="/sources" class="text-sm text-sky-600 dark:text-sky-400 hover:underline" {
                    "← Back to Sources"
                }
            }
            div hx-ext="sse" sse-connect=(events_url) {
                div id="source-detail-content" sse-swap="source-detail-update" hx-swap="innerHTML" {
                    (source_detail_content_markup(&source, &videos))
                }
            }
        },
    );

    (StatusCode::OK, page)
}

#[allow(clippy::too_many_lines)]
async fn downloads_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;
    let page_num = query.page.unwrap_or(1).max(1);
    let per_page = parse_downloads_page_size(query.per_page);
    let offset = (page_num - 1) * per_page;
    let search_query = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let status_filter = parse_download_status(query.status.as_deref());
    let current_status = normalized_download_status(query.status.as_deref());

    let (videos_result, count_result, source_names_result) = tokio::join!(
        db::list_videos_paginated(
            &state.pool,
            status_filter.clone(),
            search_query,
            per_page,
            offset
        ),
        db::count_videos(&state.pool, status_filter, search_query),
        db::get_source_names_for_videos(&state.pool),
    );

    let videos = match videos_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load downloads page");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load downloads page"),
            );
        }
    };

    let total = count_result.unwrap_or(0);
    let total_pages = if total == 0 {
        1
    } else {
        (total + per_page - 1) / per_page
    };
    let source_names = source_names_result.unwrap_or_default();

    let list_url = downloads_list_url(query.status.as_deref(), search_query, page_num, per_page);

    let page = layout_with_flash(
        "Downloads",
        NavItem::Downloads,
        flash,
        html! {
            // Filter & search bar
            section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                div class="flex flex-wrap items-center gap-4" {
                    nav class="flex flex-wrap gap-1" {
                        (download_status_filter_link("all", "All", current_status, search_query, per_page))
                        (download_status_filter_link("pending", "Pending", current_status, search_query, per_page))
                        (download_status_filter_link("downloading", "Downloading", current_status, search_query, per_page))
                        (download_status_filter_link("completed", "Completed", current_status, search_query, per_page))
                        (download_status_filter_link("failed", "Failed", current_status, search_query, per_page))
                        (download_status_filter_link("cleaned", "Cleaned", current_status, search_query, per_page))
                        (download_status_filter_link("permanently_failed", "Perm. Failed", current_status, search_query, per_page))
                        (download_status_filter_link("skipped", "Skipped", current_status, search_query, per_page))
                    }
                    form
                        method="get"
                        action="/downloads"
                        class="flex gap-2"
                        hx-get="/web/downloads/list"
                        hx-target="#downloads-list"
                        hx-push-url=(downloads_page_url(query.status.as_deref(), search_query, page_num, per_page))
                    {
                        @if let Some(ref status) = query.status {
                            input type="hidden" name="status" value=(status);
                        }
                        // Keep the current page across per_page changes. Editing the
                        // search term narrows the result set, so `data-reset-page`
                        // snaps this back to 1 before the form is serialised.
                        input id="downloads-page-field" type="hidden" name="page" value=(page_num);
                        select
                            name="per_page"
                            class="rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-2 py-1.5 text-sm text-slate-900 dark:text-slate-100"
                        {
                            (page_size_option(25, per_page))
                            (page_size_option(50, per_page))
                            (page_size_option(100, per_page))
                        }
                        input
                            type="text"
                            name="search"
                            placeholder="Search title or channel..."
                            value=(search_query.unwrap_or(""))
                            data-reset-page="downloads-page-field"
                            class="rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-900 dark:text-slate-100";
                        button type="submit"
                            class="rounded-lg bg-slate-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-slate-700"
                        { "Search" }
                        @if search_query.is_some() || query.status.is_some() {
                            a
                                href=(downloads_page_url(None, None, 1, per_page))
                                hx-get=(downloads_list_url(None, None, 1, per_page))
                                hx-target="#downloads-list"
                                hx-push-url=(downloads_page_url(None, None, 1, per_page))
                                class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                            { "Clear" }
                        }
                    }
                }
            }

            div
                hx-ext="sse"
                sse-connect=(downloads_events_url(query.status.as_deref(), search_query, page_num, per_page))
            {
                div
                    id="downloads-list"
                    sse-swap="downloads-update"
                    // `show:none` keeps background SSE refreshes from scrolling the
                    // viewport out from under someone reading mid-list.
                    hx-swap="innerHTML show:none"
                    hx-get=(list_url)
                    hx-trigger="load"
                    hx-target="this"
                {
                    (downloads_list_markup(
                        &videos,
                        &source_names,
                        page_num,
                        total_pages,
                        total,
                        query.status.as_deref(),
                        search_query,
                        per_page,
                    ))
                }
            }
        },
    );

    (StatusCode::OK, page)
}

async fn downloads_list_partial(
    _auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> impl IntoResponse {
    let page_num = query.page.unwrap_or(1).max(1);
    let per_page = parse_downloads_page_size(query.per_page);
    let offset = (page_num - 1) * per_page;
    let search_query = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let status_filter = parse_download_status(query.status.as_deref());

    let (videos_result, count_result, source_names_result) = tokio::join!(
        db::list_videos_paginated(
            &state.pool,
            status_filter.clone(),
            search_query,
            per_page,
            offset
        ),
        db::count_videos(&state.pool, status_filter, search_query),
        db::get_source_names_for_videos(&state.pool),
    );

    let videos = match videos_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load downloads partial");
            return error_fragment("Failed to load downloads page").into_response();
        }
    };

    let total = count_result.unwrap_or(0);
    let total_pages = if total == 0 {
        1
    } else {
        (total + per_page - 1) / per_page
    };
    let source_names = source_names_result.unwrap_or_default();

    downloads_list_markup(
        &videos,
        &source_names,
        page_num,
        total_pages,
        total,
        query.status.as_deref(),
        search_query,
        per_page,
    )
    .into_response()
}

/// A bulk action submitted from the downloads table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BulkAction {
    Retry,
    Cancel,
    Delete,
}

impl BulkAction {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "retry" => Some(Self::Retry),
            "cancel" => Some(Self::Cancel),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    /// Whether a video in `status` can take this action.
    ///
    /// Mirrors the guards in the single-item handlers; keep the two in step.
    const fn allows(self, status: &VideoStatus) -> bool {
        match self {
            Self::Retry => matches!(
                status,
                VideoStatus::Failed | VideoStatus::PermanentlyFailed | VideoStatus::Cleaned
            ),
            Self::Cancel => matches!(status, VideoStatus::Pending | VideoStatus::Downloading),
            Self::Delete => matches!(status, VideoStatus::Completed),
        }
    }

    const fn past_tense(self) -> &'static str {
        match self {
            Self::Retry => "re-queued",
            Self::Cancel => "cancelled",
            Self::Delete => "deleted",
        }
    }
}

/// Bulk action form payload.
///
/// `ids` is a comma-separated list rather than repeated checkbox fields:
/// `serde_urlencoded` (what `axum::Form` uses) cannot collect repeated keys
/// into a `Vec`, and selection already requires JS to drive the toolbar.
#[derive(Debug, Deserialize)]
struct BulkDownloadForm {
    action: String,
    #[serde(default)]
    ids: String,
}

/// Apply retry/cancel/delete to a selected set of downloads.
///
/// Ineligible and missing ids are skipped rather than failing the batch, and
/// the flash message reports both counts so a partial result is never silently
/// presented as a complete one.
#[allow(clippy::too_many_lines)]
async fn bulk_download_action(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
    axum::extract::Form(form): axum::extract::Form<BulkDownloadForm>,
) -> impl IntoResponse {
    let return_url = downloads_return_url(&query);

    let Some(action) = BulkAction::parse(form.action.trim()) else {
        set_flash(&session, "error", "Unknown bulk action").await;
        return Redirect::to(&return_url).into_response();
    };

    let ids: Vec<Ulid> = form
        .ids
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter_map(|value| Ulid::from_string(value).ok())
        .collect();

    if ids.is_empty() {
        set_flash(&session, "error", "No downloads selected").await;
        return Redirect::to(&return_url).into_response();
    }

    let videos = match db::list_videos_by_ids(&state.pool, &ids).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to load videos for bulk action");
            set_flash(&session, "error", "Failed to load selected downloads").await;
            return Redirect::to(&return_url).into_response();
        }
    };

    let requested = ids.len();
    let (eligible, ineligible): (Vec<_>, Vec<_>) = videos
        .into_iter()
        .partition(|video| action.allows(&video.status));
    // Ids that matched no row at all (deleted since the page rendered).
    let missing = requested - eligible.len() - ineligible.len();

    let mut succeeded = 0usize;
    let mut failed = 0usize;

    match action {
        BulkAction::Retry => {
            for video in &eligible {
                if bulk_retry_one(&state, video).await {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
            }
        }
        BulkAction::Cancel => {
            for video in &eligible {
                match state
                    .supervisor
                    .ask(CancelDownload { video_id: video.id })
                    .await
                {
                    Ok(()) => succeeded += 1,
                    Err(error) => {
                        tracing::error!(%error, video_id = %video.id, "bulk cancel failed");
                        failed += 1;
                    }
                }
            }
        }
        BulkAction::Delete => {
            for video in &eligible {
                if bulk_delete_one(&state, video).await {
                    succeeded += 1;
                } else {
                    failed += 1;
                }
            }
        }
    }

    if succeeded > 0 {
        state.broadcaster.invalidate();
    }

    let mut parts = vec![format!("{succeeded} {}", action.past_tense())];
    if !ineligible.is_empty() {
        parts.push(format!("{} not eligible", ineligible.len()));
    }
    if missing > 0 {
        parts.push(format!("{missing} no longer exist"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    let level = if failed > 0 || succeeded == 0 {
        "error"
    } else {
        "success"
    };
    set_flash(&session, level, &parts.join(", ")).await;

    Redirect::to(&return_url).into_response()
}

/// Reset one video to pending and re-enqueue it. Returns whether it worked.
async fn bulk_retry_one(state: &AppState, video: &hof_core::domain::video::Video) -> bool {
    if let Err(error) = db::update_video_status(&state.pool, video.id, VideoStatus::Pending).await {
        tracing::error!(%error, video_id = %video.id, "bulk retry: status reset failed");
        return false;
    }

    let Ok(source_ids) = db::get_sources_for_video(&state.pool, video.id).await else {
        tracing::error!(video_id = %video.id, "bulk retry: source lookup failed");
        return false;
    };
    let Some(source_id) = source_ids.first() else {
        tracing::warn!(video_id = %video.id, "bulk retry: video has no linked source");
        return false;
    };
    let Ok(source) = db::get_source(&state.pool, *source_id).await else {
        tracing::error!(video_id = %video.id, "bulk retry: source load failed");
        return false;
    };
    let Ok(profile) = db::get_profile(&state.pool, source.profile_id).await else {
        tracing::error!(video_id = %video.id, "bulk retry: profile load failed");
        return false;
    };
    // Re-read so the enqueued copy carries the pending status just written.
    let Ok(refreshed) = db::get_video(&state.pool, video.id).await else {
        tracing::error!(video_id = %video.id, "bulk retry: video reload failed");
        return false;
    };

    match state
        .supervisor
        .tell(EnqueueDownload {
            video: refreshed,
            profile,
            source,
        })
        .await
    {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(%error, video_id = %video.id, "bulk retry: enqueue failed");
            false
        }
    }
}

/// Remove one completed video's file and mark it cleaned.
async fn bulk_delete_one(state: &AppState, video: &hof_core::domain::video::Video) -> bool {
    if let Some(path) = video.file_path.as_ref() {
        // A file already gone from disk should still let the row be cleaned.
        if let Err(error) = tokio::fs::remove_file(path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::error!(%error, video_id = %video.id, "bulk delete: file removal failed");
            return false;
        }
    }

    match db::update_video_status(&state.pool, video.id, VideoStatus::Cleaned).await {
        Ok(()) => true,
        Err(error) => {
            tracing::error!(%error, video_id = %video.id, "bulk delete: status update failed");
            false
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn retry_download(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> impl IntoResponse {
    let Ok(video_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid video ID provided"),
        )
            .into_response();
    };

    let video = match db::get_video(&state.pool, video_id).await {
        Ok(value) => value,
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Video not found")).into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to load video before retry");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load video"),
            )
                .into_response();
        }
    };

    if !matches!(
        video.status,
        VideoStatus::Failed | VideoStatus::PermanentlyFailed | VideoStatus::Cleaned
    ) {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Only failed, permanently_failed, or cleaned videos can be retried"),
        )
            .into_response();
    }

    if let Err(error) = db::update_video_status(&state.pool, video_id, VideoStatus::Pending).await {
        tracing::error!(%error, "failed to reset video status before retry");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_page("Failed to reset video status"),
        )
            .into_response();
    }

    let source_ids = match db::get_sources_for_video(&state.pool, video_id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to load source ids for video retry");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load source mapping for video"),
            )
                .into_response();
        }
    };

    let Some(source_id) = source_ids.first() else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Video has no linked sources, cannot retry"),
        )
            .into_response();
    };

    let source = match db::get_source(&state.pool, *source_id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to load source for video retry");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load source for video"),
            )
                .into_response();
        }
    };

    let profile = match db::get_profile(&state.pool, source.profile_id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to load profile for video retry");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load profile for video"),
            )
                .into_response();
        }
    };

    let refreshed_video = match db::get_video(&state.pool, video_id).await {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to reload video for retry enqueue");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to reload video"),
            )
                .into_response();
        }
    };

    match state
        .supervisor
        .tell(EnqueueDownload {
            video: refreshed_video,
            profile,
            source,
        })
        .await
    {
        Ok(()) => {
            state.broadcaster.invalidate();
            set_flash(&session, "info", "Download re-queued").await;
            Redirect::to(&downloads_return_url(&query)).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to enqueue retry download");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to enqueue retry"),
            )
                .into_response()
        }
    }
}

async fn cancel_download(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> impl IntoResponse {
    let Ok(video_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid video ID provided"),
        )
            .into_response();
    };

    let video = match db::get_video(&state.pool, video_id).await {
        Ok(value) => value,
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Video not found")).into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to load video for cancel");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load video"),
            )
                .into_response();
        }
    };

    if !matches!(
        video.status,
        VideoStatus::Pending | VideoStatus::Downloading
    ) {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Only pending or downloading videos can be cancelled"),
        )
            .into_response();
    }

    let cancel_result = state.supervisor.ask(CancelDownload { video_id }).await;

    match cancel_result {
        Ok(()) => {
            state.broadcaster.invalidate();
            set_flash(&session, "info", "Download cancelled").await;
            Redirect::to(&downloads_return_url(&query)).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to cancel download");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to cancel download"),
            )
                .into_response()
        }
    }
}

async fn delete_download(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> impl IntoResponse {
    let Ok(video_id) = Ulid::from_string(id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Invalid video ID provided"),
        )
            .into_response();
    };

    let video = match db::get_video(&state.pool, video_id).await {
        Ok(value) => value,
        Err(db::DbError::NotFound) => {
            return (StatusCode::NOT_FOUND, error_page("Video not found")).into_response();
        }
        Err(error) => {
            tracing::error!(%error, "failed to load video for delete");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load video"),
            )
                .into_response();
        }
    };

    if video.status != VideoStatus::Completed {
        return (
            StatusCode::BAD_REQUEST,
            error_page("Only completed videos can be deleted"),
        )
            .into_response();
    }

    // Delete the file from disk
    if let Some(ref file_path) = video.file_path {
        let path = std::path::Path::new(file_path);
        if path.exists()
            && let Err(error) = tokio::fs::remove_file(path).await
        {
            tracing::error!(%error, file_path, "failed to delete video file");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to delete video file from disk"),
            )
                .into_response();
        }
    }

    // Mark as cleaned in DB
    if let Err(error) = db::update_video_status(&state.pool, video_id, VideoStatus::Cleaned).await {
        tracing::error!(%error, "failed to mark video as cleaned");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            error_page("Failed to update video status"),
        )
            .into_response();
    }

    #[allow(clippy::cast_precision_loss)]
    let size_mb = video.file_size_bytes.unwrap_or(0) as f64 / 1_048_576.0;
    let title = &video.title;
    state
        .broadcaster
        .log_and_broadcast(
            &state.pool,
            ActivityEventType::VideoCleaned,
            ActivitySeverity::Info,
            &format!("Manually deleted \"{title}\" ({size_mb:.1} MB freed)"),
            None,
            Some(video_id),
            None,
        )
        .await;

    set_flash(
        &session,
        "success",
        &format!("\"{title}\" deleted ({size_mb:.1} MB freed)"),
    )
    .await;
    Redirect::to(&downloads_return_url(&query)).into_response()
}

/// Returns an HTML fragment for the system issues banner.
/// If there are no issues, returns an empty response.
async fn system_banner(State(state): State<AppState>) -> impl IntoResponse {
    let issues = &state.startup_issues;

    if issues.is_empty() {
        return html! {}.into_response();
    }

    // Find the most severe issue level
    let has_errors = issues.iter().any(|i| i.severity == IssueSeverity::Error);
    let (bg_class, border_class, text_class, icon) = if has_errors {
        (
            "bg-rose-50",
            "border-rose-200",
            "text-rose-800",
            "⚠", // Warning sign
        )
    } else {
        (
            "bg-amber-50",
            "border-amber-200",
            "text-amber-800",
            "⚡", // Lightning for warning
        )
    };

    html! {
        div
            id="system-banner"
            class=(format!("mb-4 rounded-xl border {border_class} {bg_class} p-4 shadow-sm"))
        {
            div class="flex items-start gap-3" {
                span class="text-xl" { (icon) }
                div class="flex-1" {
                    h3 class=(format!("font-semibold {text_class}")) {
                        "System Issues Detected"
                    }
                    ul class=(format!("mt-1 list-inside list-disc text-sm {text_class}")) {
                        @for issue in issues.iter() {
                            li { (issue.message) }
                        }
                    }
                }
            }
        }
    }
    .into_response()
}

// ============================================================================
// Activity Page
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
struct ActivityQuery {
    severity: Option<String>,
    search: Option<String>,
    /// Restrict to one source; set by clicking a source pill on an event row.
    source: Option<String>,
    page: Option<i64>,
    per_page: Option<i64>,
}

/// Request-scoped activity filter state, normalized once.
///
/// The page, list partial and SSE stream all need the same derived values;
/// deriving them in one place keeps the three views from drifting apart.
struct ActivityParams {
    page_num: i64,
    per_page: i64,
    offset: i64,
    severity_filter: Option<ActivitySeverity>,
    severity_label: String,
    search: Option<String>,
    source_id: Option<Ulid>,
    source: Option<String>,
}

impl ActivityParams {
    fn from_query(query: &ActivityQuery) -> Self {
        let page_num = query.page.unwrap_or(1).max(1);
        let per_page = parse_activity_page_size(query.per_page);
        let search = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        // Only keep a source filter we can actually parse; a malformed id would
        // otherwise silently match nothing while still showing an active pill.
        let source_id = query
            .source
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(|value| Ulid::from_string(value).ok());

        Self {
            page_num,
            per_page,
            offset: (page_num - 1) * per_page,
            severity_filter: parse_activity_severity(query.severity.as_deref()),
            severity_label: normalized_activity_severity(query.severity.as_deref()).to_owned(),
            search,
            source_id,
            source: source_id.map(|id| id.to_string()),
        }
    }

    fn severity_param(&self) -> Option<&str> {
        if self.severity_label == "all" {
            None
        } else {
            Some(&self.severity_label)
        }
    }

    fn filter(&self) -> ActivityFilter<'_> {
        ActivityFilter {
            severity: self.severity_param(),
            search: self.search.as_deref(),
            source: self.source.as_deref(),
        }
    }
}

async fn activity_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> impl IntoResponse {
    let params = ActivityParams::from_query(&query);
    let ActivityParams {
        page_num,
        per_page,
        offset,
        ref severity_filter,
        ..
    } = params;

    let (events_result, count_result) = tokio::join!(
        db::list_activity_events(
            &state.pool,
            per_page,
            offset,
            severity_filter.clone(),
            None,
            params.source_id,
            params.search.as_deref(),
        ),
        db::count_activity_events(
            &state.pool,
            severity_filter.clone(),
            None,
            params.source_id,
            params.search.as_deref(),
        )
    );

    let events = match events_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load activity events");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load activity log"),
            );
        }
    };

    let source_names = load_activity_source_names(&state.pool).await;
    let video_source_names = db::get_source_names_for_videos(&state.pool)
        .await
        .unwrap_or_default();

    let total = count_result.unwrap_or(0);
    let total_pages = (total + per_page - 1) / per_page;

    let current_severity = params.severity_label.as_str();
    let list_url = activity_list_url(params.filter(), page_num, per_page);
    let events_url = activity_events_url(params.filter(), page_num, per_page);

    let page = layout(
        "Activity",
        NavItem::Activity,
        html! {
            section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Activity Log" }
                div hx-ext="sse" sse-connect=(events_url) {
                    div
                        id="activity-content"
                        sse-swap="activity-update"
                        // `show:none` keeps background SSE refreshes from scrolling the
                        // viewport out from under someone reading mid-list.
                        hx-swap="innerHTML show:none"
                        hx-get=(list_url)
                        hx-trigger="load"
                        hx-target="this"
                    {
                        (activity_content_markup(
                            &events,
                            &source_names,
                            &video_source_names,
                            page_num,
                            total_pages,
                            current_severity,
                            per_page,
                            params.search.as_deref(),
                            params.source.as_deref(),
                        ))
                    }
                }
            }
        },
    );

    (StatusCode::OK, page)
}

async fn activity_list_partial(
    _auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> impl IntoResponse {
    let params = ActivityParams::from_query(&query);
    let ActivityParams {
        page_num,
        per_page,
        offset,
        ref severity_filter,
        ..
    } = params;

    let (events_result, count_result) = tokio::join!(
        db::list_activity_events(
            &state.pool,
            per_page,
            offset,
            severity_filter.clone(),
            None,
            params.source_id,
            params.search.as_deref(),
        ),
        db::count_activity_events(
            &state.pool,
            severity_filter.clone(),
            None,
            params.source_id,
            params.search.as_deref(),
        )
    );

    let events = match events_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load activity partial");
            return error_fragment("Failed to load activity log").into_response();
        }
    };

    let source_names = load_activity_source_names(&state.pool).await;
    let video_source_names = db::get_source_names_for_videos(&state.pool)
        .await
        .unwrap_or_default();

    let total = count_result.unwrap_or(0);
    let total_pages = if total == 0 {
        1
    } else {
        (total + per_page - 1) / per_page
    };
    activity_content_markup(
        &events,
        &source_names,
        &video_source_names,
        page_num,
        total_pages,
        &params.severity_label,
        per_page,
        params.search.as_deref(),
        params.source.as_deref(),
    )
    .into_response()
}

/// Load a `source_id -> display_name` lookup map for enriching activity rows.
///
/// Reuses `db::list_sources` rather than adding a new query, so this stays
/// offline-cache-free. Returns an empty map on error so a DB hiccup only
/// degrades the display (no source name pill) instead of failing the page.
async fn load_activity_source_names(pool: &sqlx::PgPool) -> HashMap<Ulid, String> {
    match db::list_sources(pool).await {
        Ok(sources) => sources
            .iter()
            .map(|s| (s.id, s.display_name().to_string()))
            .collect(),
        Err(error) => {
            tracing::warn!(%error, "failed to load source names for activity log");
            HashMap::new()
        }
    }
}

fn download_status_filter_link(
    value: &str,
    label: &str,
    current: &str,
    search: Option<&str>,
    per_page: i64,
) -> Markup {
    let active = value == current;
    let classes = if active {
        "rounded-full bg-slate-900 dark:bg-slate-100 px-3 py-1 text-xs font-medium text-white dark:text-slate-900"
    } else {
        "rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
    };
    let status = if value == "all" { None } else { Some(value) };
    let href = downloads_page_url(status, search, 1, per_page);
    let hx_get = downloads_list_url(status, search, 1, per_page);

    html! {
        a
            class=(classes)
            href=(href)
            hx-get=(hx_get)
            hx-target="#downloads-list"
            hx-push-url=(href)
        {
            (label)
        }
    }
}

fn severity_filter_link(
    value: &str,
    label: &str,
    current: &str,
    per_page: i64,
    search: Option<&str>,
    source: Option<&str>,
) -> Markup {
    let active = value == current;
    let classes = if active {
        "rounded-full bg-slate-900 dark:bg-slate-100 px-3 py-1 text-xs font-medium text-white dark:text-slate-900"
    } else {
        "rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
    };
    let severity = if value == "all" { None } else { Some(value) };
    let filter = ActivityFilter {
        severity,
        search,
        source,
    };
    let href = activity_page_url(filter, 1, per_page);
    let hx_get = activity_list_url(filter, 1, per_page);

    html! {
        a
            class=(classes)
            href=(href)
            hx-get=(hx_get)
            hx-target="#activity-content"
            hx-push-url=(href)
        {
            (label)
        }
    }
}

fn activity_page_size_link(
    size: i64,
    current_size: i64,
    severity: Option<&str>,
    search: Option<&str>,
    source: Option<&str>,
) -> Markup {
    let active = size == current_size;
    let classes = if active {
        "rounded-full bg-slate-900 dark:bg-slate-100 px-2.5 py-1 text-xs font-medium text-white dark:text-slate-900"
    } else {
        "rounded-full bg-slate-100 dark:bg-slate-700 px-2.5 py-1 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
    };
    let filter = ActivityFilter {
        severity,
        search,
        source,
    };
    let href = activity_page_url(filter, 1, size);
    let hx_get = activity_list_url(filter, 1, size);

    html! {
        a
            class=(classes)
            href=(href)
            hx-get=(hx_get)
            hx-target="#activity-content"
            hx-push-url=(href)
        {
            (size)
        }
    }
}

fn page_size_option(size: i64, current_size: i64) -> Markup {
    if size == current_size {
        html! {
            option value=(size) selected { (size) " / page" }
        }
    } else {
        html! {
            option value=(size) { (size) " / page" }
        }
    }
}

const fn parse_downloads_page_size(value: Option<i64>) -> i64 {
    match value {
        Some(size) if matches!(size, 25 | 50 | 100) => size,
        _ => 25,
    }
}

const fn parse_activity_page_size(value: Option<i64>) -> i64 {
    match value {
        Some(size) if matches!(size, 25 | 50 | 100) => size,
        _ => 50,
    }
}

/// The canonical slug for a status, matching [`parse_download_status`].
///
/// Rendered into `data-status` so the bulk toolbar can decide client-side
/// which actions apply to the current selection.
const fn download_status_slug(status: &VideoStatus) -> &'static str {
    match status {
        VideoStatus::Pending => "pending",
        VideoStatus::Downloading => "downloading",
        VideoStatus::Completed => "completed",
        VideoStatus::Failed => "failed",
        VideoStatus::Skipped => "skipped",
        VideoStatus::Cleaned => "cleaned",
        VideoStatus::PermanentlyFailed => "permanently_failed",
    }
}

fn parse_download_status(status: Option<&str>) -> Option<VideoStatus> {
    status.and_then(|s| match s {
        "pending" => Some(VideoStatus::Pending),
        "downloading" => Some(VideoStatus::Downloading),
        "completed" => Some(VideoStatus::Completed),
        "failed" => Some(VideoStatus::Failed),
        "skipped" => Some(VideoStatus::Skipped),
        "cleaned" => Some(VideoStatus::Cleaned),
        "permanently_failed" => Some(VideoStatus::PermanentlyFailed),
        _ => None,
    })
}

fn normalized_download_status(status: Option<&str>) -> &str {
    match status {
        Some(
            value @ ("pending" | "downloading" | "completed" | "failed" | "skipped" | "cleaned"
            | "permanently_failed"),
        ) => value,
        _ => "all",
    }
}

fn parse_activity_severity(value: Option<&str>) -> Option<ActivitySeverity> {
    value.and_then(|severity| match severity {
        "info" => Some(ActivitySeverity::Info),
        "success" => Some(ActivitySeverity::Success),
        "warning" => Some(ActivitySeverity::Warning),
        "error" => Some(ActivitySeverity::Error),
        _ => None,
    })
}

fn normalized_activity_severity(value: Option<&str>) -> &str {
    match value {
        Some(severity @ ("info" | "success" | "warning" | "error")) => severity,
        _ => "all",
    }
}

fn downloads_page_url(
    status: Option<&str>,
    search: Option<&str>,
    page: i64,
    per_page: i64,
) -> String {
    downloads_url("/downloads", status, search, page, per_page)
}

fn downloads_list_url(
    status: Option<&str>,
    search: Option<&str>,
    page: i64,
    per_page: i64,
) -> String {
    downloads_url("/web/downloads/list", status, search, page, per_page)
}

fn downloads_events_url(
    status: Option<&str>,
    search: Option<&str>,
    page: i64,
    per_page: i64,
) -> String {
    downloads_url("/web/downloads/events", status, search, page, per_page)
}

/// Percent-encode a value for use in a query string.
///
/// Search terms are user-supplied and routinely contain spaces and `&`, which
/// would otherwise truncate the URL or inject a spurious parameter.
fn encode_query_value(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, QUERY_VALUE_ENCODE_SET).to_string()
}

/// Rebuild the `/downloads` URL a mutating action should return to.
///
/// Retry/cancel/delete carry the list state (status, search, page, `per_page`) as
/// query params on their form action, so the post-redirect lands back on the row
/// the user acted from instead of resetting to page 1 with no filters.
fn downloads_return_url(query: &DownloadsQuery) -> String {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = parse_downloads_page_size(query.per_page);
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    downloads_page_url(query.status.as_deref(), search, page, per_page)
}

fn downloads_url(
    base: &str,
    status: Option<&str>,
    search: Option<&str>,
    page: i64,
    per_page: i64,
) -> String {
    let mut params = Vec::new();

    if let Some(value) = status.filter(|value| *value != "all") {
        params.push(format!("status={value}"));
    }
    if let Some(value) = search.filter(|value| !value.is_empty()) {
        params.push(format!("search={}", encode_query_value(value)));
    }
    if page > 1 {
        params.push(format!("page={page}"));
    }
    if per_page != 25 {
        params.push(format!("per_page={per_page}"));
    }

    if params.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", params.join("&"))
    }
}

/// The filter state shared by every activity URL variant.
///
/// Grouped into a struct because these four values must travel together through
/// the page, the list partial, the SSE stream and every filter link — passing
/// them positionally invited dropping one and silently resetting the view.
#[derive(Debug, Clone, Copy, Default)]
struct ActivityFilter<'a> {
    severity: Option<&'a str>,
    search: Option<&'a str>,
    source: Option<&'a str>,
}

fn activity_page_url(filter: ActivityFilter<'_>, page: i64, per_page: i64) -> String {
    activity_url("/activity", filter, page, per_page)
}

fn activity_list_url(filter: ActivityFilter<'_>, page: i64, per_page: i64) -> String {
    activity_url("/web/activity/list", filter, page, per_page)
}

fn activity_events_url(filter: ActivityFilter<'_>, page: i64, per_page: i64) -> String {
    activity_url("/web/activity/events", filter, page, per_page)
}

fn activity_url(base: &str, filter: ActivityFilter<'_>, page: i64, per_page: i64) -> String {
    let mut params = Vec::new();

    if let Some(value) = filter.severity.filter(|value| *value != "all") {
        params.push(format!("severity={value}"));
    }
    if let Some(value) = filter.search.filter(|value| !value.is_empty()) {
        params.push(format!("search={}", encode_query_value(value)));
    }
    if let Some(value) = filter.source.filter(|value| !value.is_empty()) {
        params.push(format!("source={}", encode_query_value(value)));
    }
    if page > 1 {
        params.push(format!("page={page}"));
    }
    if per_page != 50 {
        params.push(format!("per_page={per_page}"));
    }

    if params.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", params.join("&"))
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
/// Toolbar for acting on the current checkbox selection.
///
/// Hidden until something is selected. The buttons stay disabled unless the
/// selection contains at least one video eligible for that action, so the
/// affordance matches what the server will actually do.
fn bulk_action_bar(
    status: Option<&str>,
    search: Option<&str>,
    page_num: i64,
    per_page: i64,
) -> Markup {
    let action_url = downloads_url("/downloads/bulk", status, search, page_num, per_page);

    html! {
        form
            method="post"
            action=(action_url)
            data-bulk-form="1"
            hidden
            class="mt-4 flex flex-wrap items-center gap-2 rounded-xl border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 px-4 py-3"
        {
            input type="hidden" name="ids" data-bulk-ids="1" value="";
            input type="hidden" name="action" data-bulk-action="1" value="";
            span class="text-sm font-medium text-slate-700 dark:text-slate-300" {
                span data-bulk-count="1" { "0" }
                " selected"
            }
            button
                type="submit"
                value="retry"
                data-bulk-button="retry"
                class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-xs font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900 disabled:opacity-40"
            { "Retry selected" }
            button
                type="submit"
                value="cancel"
                data-bulk-button="cancel"
                data-confirm="Cancel the selected downloads?"
                class="rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/50 px-3 py-1.5 text-xs font-medium text-amber-700 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900 disabled:opacity-40"
            { "Cancel selected" }
            button
                type="submit"
                value="delete"
                data-bulk-button="delete"
                data-confirm="Delete the selected videos? Their files will be removed from disk."
                class="rounded-lg border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/50 px-3 py-1.5 text-xs font-medium text-rose-700 dark:text-rose-300 hover:bg-rose-100 dark:hover:bg-rose-900 disabled:opacity-40"
            { "Delete selected" }
            button
                type="button"
                data-bulk-clear="1"
                class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
            { "Clear selection" }
        }
    }
}

/// One download row, including its per-item action buttons.
///
/// The action forms carry the current list state so the post-redirect returns
/// to the same page and filters rather than resetting to the top.
#[allow(clippy::too_many_arguments)]
fn downloads_row(
    video: &Video,
    source_names: &HashMap<Ulid, String>,
    status_param: Option<&str>,
    search: Option<&str>,
    page_num: i64,
    per_page: i64,
) -> Markup {
    let action_url = |verb: &str| {
        downloads_url(
            &format!("/downloads/{}/{verb}", video.id),
            status_param,
            search,
            page_num,
            per_page,
        )
    };

    html! {
        tr id=(format!("video-{}", video.id)) {
            td class="px-3 py-2" {
                input
                    type="checkbox"
                    data-bulk-select=(video.id.to_string())
                    data-status=(download_status_slug(&video.status))
                    aria-label=(format!("Select {}", video.title))
                    class="h-4 w-4 rounded border-slate-300 dark:border-slate-600";
            }
            td class="max-w-lg px-3 py-2 text-slate-900 dark:text-slate-100" {
                p class="truncate font-medium" { (video.title) }
                p class="truncate text-xs text-slate-500 dark:text-slate-400" { (video.id.to_string()) }
            }
            td class="max-w-xs px-3 py-2 text-slate-600 dark:text-slate-400" {
                p class="truncate" {
                    @if let Some(name) = source_names.get(&video.id) {
                        (name)
                    } @else {
                        span class="text-slate-400 dark:text-slate-500 italic" { "—" }
                    }
                }
            }
            td class="px-3 py-2 text-slate-600 dark:text-slate-400" { (video.platform) }
            td class="px-3 py-2" { (status_badge(&video.status)) }
            td class="px-3 py-2 text-slate-600 dark:text-slate-400" { (video.attempts) }
            td class="px-3 py-2" {
                div class="flex gap-2" {
                    @if BulkAction::Retry.allows(&video.status) {
                        form method="post" action=(action_url("retry")) {
                            button class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-xs font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900" type="submit" {
                                "Retry"
                            }
                        }
                    }
                    @if BulkAction::Cancel.allows(&video.status) {
                        form method="post" action=(action_url("cancel")) {
                            button
                                class="rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/50 px-3 py-1.5 text-xs font-medium text-amber-700 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900"
                                type="submit"
                                onclick="return confirm('Cancel this download?')"
                            {
                                "Cancel"
                            }
                        }
                    }
                    @if BulkAction::Delete.allows(&video.status) {
                        form method="post" action=(action_url("delete")) {
                            button
                                class="rounded-lg border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/50 px-3 py-1.5 text-xs font-medium text-rose-700 dark:text-rose-300 hover:bg-rose-100 dark:hover:bg-rose-900"
                                type="submit"
                                onclick="return confirm('Delete this video? The file will be removed from disk.')"
                            {
                                "Delete"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn downloads_list_markup(
    videos: &[Video],
    source_names: &HashMap<Ulid, String>,
    page_num: i64,
    total_pages: i64,
    total: i64,
    status: Option<&str>,
    search: Option<&str>,
    per_page: i64,
) -> Markup {
    let current_status = normalized_download_status(status);
    let status_param = if current_status == "all" {
        None
    } else {
        Some(current_status)
    };

    html! {
        section class="mt-4 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" {
                "Downloads"
                span class="ml-2 text-sm font-normal text-slate-500 dark:text-slate-400" {
                    "(" (total) " results)"
                }
            }
            @if videos.is_empty() {
                p class="mt-3 text-sm text-slate-500 dark:text-slate-400" { "No downloads match your filters." }
            } @else {
                (bulk_action_bar(status_param, search, page_num, per_page))
                div class="mt-4 overflow-x-auto" {
                    table class="min-w-full divide-y divide-slate-200 text-sm" data-bulk-table="1" {
                        thead class="bg-slate-50 dark:bg-slate-800" {
                            tr {
                                th class="w-8 px-3 py-2 text-left" {
                                    input
                                        type="checkbox"
                                        data-bulk-select-all="1"
                                        aria-label="Select all downloads on this page"
                                        class="h-4 w-4 rounded border-slate-300 dark:border-slate-600";
                                }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Title" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Source" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Platform" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Status" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Attempts" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Actions" }
                            }
                        }
                        tbody class="divide-y divide-slate-100 dark:divide-slate-700 bg-white dark:bg-slate-900" {
                            @for video in videos {
                                (downloads_row(video, source_names, status_param, search, page_num, per_page))
                            }
                        }
                    }
                }
            }

            @if total_pages > 1 {
                nav class="mt-6 flex items-center justify-center gap-2" {
                    @if page_num > 1 {
                        a
                            class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-700 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                            href=(downloads_page_url(status_param, search, page_num - 1, per_page))
                            hx-get=(downloads_list_url(status_param, search, page_num - 1, per_page))
                            hx-target="#downloads-list"
                            hx-push-url=(downloads_page_url(status_param, search, page_num - 1, per_page))
                        {
                            "Previous"
                        }
                    }
                    span class="text-sm text-slate-500 dark:text-slate-400" {
                        (format!("Page {} of {}", page_num, total_pages))
                    }
                    @if page_num < total_pages {
                        a
                            class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-700 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                            href=(downloads_page_url(status_param, search, page_num + 1, per_page))
                            hx-get=(downloads_list_url(status_param, search, page_num + 1, per_page))
                            hx-target="#downloads-list"
                            hx-push-url=(downloads_page_url(status_param, search, page_num + 1, per_page))
                        {
                            "Next"
                        }
                    }
                }
            }
        }
    }
}

/// Severity pills, page-size pills, and the message search box.
fn activity_filter_bar(
    filter: ActivityFilter<'_>,
    current_severity: &str,
    page_num: i64,
    per_page: i64,
) -> Markup {
    let ActivityFilter {
        severity,
        search,
        source,
    } = filter;

    html! {
        div class="mt-3 flex flex-wrap items-center gap-3" {
            nav class="flex gap-1" {
                (severity_filter_link("all", "All", current_severity, per_page, search, source))
                (severity_filter_link("info", "Info", current_severity, per_page, search, source))
                (severity_filter_link("success", "Success", current_severity, per_page, search, source))
                (severity_filter_link("warning", "Warning", current_severity, per_page, search, source))
                (severity_filter_link("error", "Error", current_severity, per_page, search, source))
            }
            nav class="flex items-center gap-1" {
                span class="px-2 text-xs font-medium text-slate-500 dark:text-slate-400" { "Rows" }
                (activity_page_size_link(25, per_page, severity, search, source))
                (activity_page_size_link(50, per_page, severity, search, source))
                (activity_page_size_link(100, per_page, severity, search, source))
            }
            form
                method="get"
                action="/activity"
                class="flex gap-2"
                hx-get="/web/activity/list"
                hx-target="#activity-content"
                hx-push-url=(activity_page_url(filter, page_num, per_page))
            {
                // Severity and source live outside this form as pills, so they
                // ride along as hidden fields to survive a search submit.
                @if let Some(value) = severity {
                    input type="hidden" name="severity" value=(value);
                }
                @if let Some(value) = source {
                    input type="hidden" name="source" value=(value);
                }
                input type="hidden" name="per_page" value=(per_page);
                input id="activity-page-field" type="hidden" name="page" value=(page_num);
                input
                    type="text"
                    name="search"
                    placeholder="Search messages..."
                    value=(search.unwrap_or(""))
                    data-reset-page="activity-page-field"
                    class="rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-900 dark:text-slate-100";
                button type="submit"
                    class="rounded-lg bg-slate-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-slate-700"
                { "Search" }
            }
        }
    }
}

/// The active source filter, rendered as a pill with a clear affordance.
///
/// `display_name` is `None` for a source that has since been deleted; the raw
/// id is shown instead so the filter is still identifiable and clearable.
fn activity_source_pill(
    filter: ActivityFilter<'_>,
    display_name: Option<&str>,
    per_page: i64,
) -> Markup {
    let Some(source_id) = filter.source else {
        return html! {};
    };
    let cleared = ActivityFilter {
        source: None,
        ..filter
    };

    html! {
        div class="mt-3 flex flex-wrap items-center gap-2" {
            span class="text-xs font-medium text-slate-500 dark:text-slate-400" { "Filtered to source" }
            span class="inline-flex items-center gap-2 rounded-full bg-slate-900 dark:bg-slate-100 px-3 py-1 text-xs font-medium text-white dark:text-slate-900" {
                (display_name.unwrap_or(source_id))
                a
                    href=(activity_page_url(cleared, 1, per_page))
                    hx-get=(activity_list_url(cleared, 1, per_page))
                    hx-target="#activity-content"
                    hx-push-url=(activity_page_url(cleared, 1, per_page))
                    class="text-slate-300 dark:text-slate-600 hover:text-white dark:hover:text-slate-900"
                    aria-label="Clear source filter"
                { "\u{00d7}" }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn activity_content_markup(
    events: &[hof_core::domain::activity::ActivityEvent],
    source_names: &HashMap<Ulid, String>,
    video_source_names: &HashMap<Ulid, String>,
    page_num: i64,
    total_pages: i64,
    current_severity: &str,
    per_page: i64,
    search: Option<&str>,
    source: Option<&str>,
) -> Markup {
    let severity_param = if current_severity == "all" {
        None
    } else {
        Some(current_severity)
    };
    let filter = ActivityFilter {
        severity: severity_param,
        search,
        source,
    };
    let has_filters = search.is_some() || source.is_some() || severity_param.is_some();
    // The pill shows the source's display name when we can resolve it, falling
    // back to the raw id for a source that has since been deleted.
    let active_source_name = source
        .and_then(|id| Ulid::from_string(id).ok())
        .and_then(|id| source_names.get(&id))
        .map(String::as_str);

    html! {
        (activity_filter_bar(filter, current_severity, page_num, per_page))
        (activity_source_pill(filter, active_source_name, per_page))

        @if events.is_empty() {
            p class="mt-4 rounded-lg border border-dashed border-slate-300 dark:border-slate-600 bg-slate-50 dark:bg-slate-800 px-4 py-8 text-center text-sm text-slate-500 dark:text-slate-400" {
                @if has_filters {
                    "No activity events match your filters."
                } @else {
                    "No activity events recorded yet."
                }
            }
        } @else {
            div class="mt-4 space-y-2" {
                @for event in events {
                    (activity_event_row(event, source_names, video_source_names, filter, per_page))
                }
            }

            @if total_pages > 1 {
                nav class="mt-6 flex items-center justify-center gap-2" {
                    @if page_num > 1 {
                        a
                            class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-700 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                            href=(activity_page_url(filter, page_num - 1, per_page))
                            hx-get=(activity_list_url(filter, page_num - 1, per_page))
                            hx-target="#activity-content"
                            hx-push-url=(activity_page_url(filter, page_num - 1, per_page))
                        {
                            "Previous"
                        }
                    }
                    span class="text-sm text-slate-500 dark:text-slate-400" {
                        (format!("Page {} of {}", page_num, total_pages))
                    }
                    @if page_num < total_pages {
                        a
                            class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-700 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                            href=(activity_page_url(filter, page_num + 1, per_page))
                            hx-get=(activity_list_url(filter, page_num + 1, per_page))
                            hx-target="#activity-content"
                            hx-push-url=(activity_page_url(filter, page_num + 1, per_page))
                        {
                            "Next"
                        }
                    }
                }
            }
        }
    }
}

/// Render a single activity log entry.
///
/// `source_names` maps `source_id -> display_name` (from `list_sources`), used
/// directly for source-level events (e.g. `SourceIndexed`). Download events
/// don't carry a `source_id` on the row, so `video_source_names` (a
/// `video_id -> display_name` map derived by joining through `source_videos`,
/// see `db::get_source_names_for_videos`) is used as a fallback so the source
/// pill still resolves via the event's `video_id`. If neither resolves (e.g.
/// the source was since deleted), the pill is simply omitted.
fn activity_event_row(
    event: &hof_core::domain::activity::ActivityEvent,
    source_names: &HashMap<Ulid, String>,
    video_source_names: &HashMap<Ulid, String>,
    filter: ActivityFilter<'_>,
    per_page: i64,
) -> Markup {
    let (icon, border_color) = match event.severity {
        ActivitySeverity::Info => ("i", "border-l-sky-400"),
        ActivitySeverity::Success => ("✓", "border-l-emerald-400"),
        ActivitySeverity::Warning => ("!", "border-l-amber-400"),
        ActivitySeverity::Error => ("✗", "border-l-rose-400"),
    };

    let severity_badge = match event.severity {
        ActivitySeverity::Info => (
            "Info",
            "bg-sky-100 dark:bg-sky-900/50 text-sky-800 dark:text-sky-200",
        ),
        ActivitySeverity::Success => (
            "Success",
            "bg-emerald-100 dark:bg-emerald-900/50 text-emerald-800 dark:text-emerald-200",
        ),
        ActivitySeverity::Warning => (
            "Warning",
            "bg-amber-100 dark:bg-amber-900/50 text-amber-800 dark:text-amber-200",
        ),
        ActivitySeverity::Error => (
            "Error",
            "bg-rose-100 dark:bg-rose-900/50 text-rose-800 dark:text-rose-200",
        ),
    };

    let event_label = match event.event_type {
        ActivityEventType::SourceIndexed => "Source Indexed",
        ActivityEventType::SourceError => "Source Error",
        ActivityEventType::DownloadStarted => "Download Started",
        ActivityEventType::DownloadCompleted => "Download Completed",
        ActivityEventType::DownloadFailed => "Download Failed",
        ActivityEventType::RetryScheduled => "Retry Scheduled",
        ActivityEventType::MetadataGenerated => "Metadata Generated",
        ActivityEventType::VideoCleaned => "Video Cleaned",
        ActivityEventType::ProfileCreated => "Profile Created",
        ActivityEventType::ProfileUpdated => "Profile Updated",
        ActivityEventType::ProfileDeleted => "Profile Deleted",
        ActivityEventType::SourceCreated => "Source Created",
        ActivityEventType::SourceUpdated => "Source Updated",
        ActivityEventType::SourceDeleted => "Source Deleted",
    };

    let time_ago = format_time_ago(event.created_at);
    let source_indexing = event.source_indexing_summary();
    let source_name = event
        .source_id
        .and_then(|id| source_names.get(&id))
        .or_else(|| event.video_id.and_then(|id| video_source_names.get(&id)));
    // Only events carrying a source id can be turned into a filter link; names
    // resolved via `video_source_names` have no id to filter on.
    let pill_source_id = event.source_id.filter(|id| source_names.contains_key(id));

    html! {
        div id=(format!("activity-{}", event.id)) class=(format!("flex items-start gap-3 rounded-lg border border-slate-200 dark:border-slate-700 border-l-4 {} bg-white dark:bg-slate-800 p-3", border_color)) {
            span class="mt-0.5 flex h-6 w-6 items-center justify-center rounded-full bg-slate-100 dark:bg-slate-700 text-xs font-bold text-slate-600 dark:text-slate-300" {
                (icon)
            }
            div class="min-w-0 flex-1" {
                div class="flex flex-wrap items-center gap-2" {
                    span class=(format!("inline-flex rounded-full px-2 py-0.5 text-xs font-medium {}", severity_badge.1)) {
                        (severity_badge.0)
                    }
                    span class="text-xs font-medium text-slate-500 dark:text-slate-400" { (event_label) }
                    @if let Some(name) = source_name {
                        @if let Some(source_id) = pill_source_id {
                            @let pill_filter = ActivityFilter {
                                severity: filter.severity,
                                search: filter.search,
                                source: Some(&source_id.to_string()),
                            };
                            a
                                href=(activity_page_url(pill_filter, 1, per_page))
                                hx-get=(activity_list_url(pill_filter, 1, per_page))
                                hx-target="#activity-content"
                                hx-push-url=(activity_page_url(pill_filter, 1, per_page))
                                title=(format!("Show only activity from {name}"))
                                class="inline-flex rounded-full bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                            {
                                (name)
                            }
                        } @else {
                            span class="inline-flex rounded-full bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs font-medium text-slate-600 dark:text-slate-300" {
                                (name)
                            }
                        }
                    }
                    span class="text-xs text-slate-400 dark:text-slate-500" title=(event.created_at.to_rfc3339()) { (time_ago) }
                }
                p class="mt-1 text-sm text-slate-700 dark:text-slate-300 break-words wrap-anywhere" { (event.message) }
                @if let Some(summary) = source_indexing {
                    div class="mt-2 flex flex-wrap gap-1.5" {
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "new: " (summary.new_videos) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "existing: " (summary.existing_videos) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "filtered: " (summary.filtered_total) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "cutoff: " (summary.filtered_before_cutoff) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "shorts: " (summary.filtered_shorts) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "live: " (summary.filtered_livestreams) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "unavailable: " (summary.filtered_unavailable) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "private: " (summary.filtered_private) }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-600 dark:text-slate-300" { "other: " (summary.filtered_other) }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod activity_event_row_tests {
    use super::{
        ActivityEventType, ActivityFilter, ActivitySeverity, HashMap, Ulid, activity_event_row,
    };
    use hof_core::domain::activity::ActivityEvent;

    fn base_event(event_type: ActivityEventType, video_id: Option<Ulid>) -> ActivityEvent {
        ActivityEvent {
            id: Ulid::from_string("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid literal"),
            event_type,
            severity: ActivitySeverity::Success,
            message: "Completed \"300G.mp4\" (13.0 MB)".to_string(),
            source_id: None,
            video_id,
            profile_id: None,
            created_at: chrono::Utc::now(),
        }
    }

    /// Default (unfiltered) view state for rendering a row under test.
    fn filter() -> ActivityFilter<'static> {
        ActivityFilter::default()
    }

    /// Download activity rows never carry a `source_id` (it's only set for
    /// source-level events like `SourceIndexed`). The pill should still
    /// resolve, via the event's `video_id`, using the `video_id ->
    /// display_name` map that `db::get_source_names_for_videos` provides.
    #[test]
    fn download_completed_event_resolves_source_pill_via_video_id() {
        let video_id = Ulid::from_string("01BX5ZZKBKACTAV9WEVGEMMVRZ").expect("valid ulid literal");
        let event = base_event(ActivityEventType::DownloadCompleted, Some(video_id));
        assert!(
            event.source_id.is_none(),
            "precondition: no source_id on row"
        );

        let source_names: HashMap<Ulid, String> = HashMap::new();
        let mut video_source_names: HashMap<Ulid, String> = HashMap::new();
        video_source_names.insert(video_id, "Kobosil 300G".to_string());

        let markup = activity_event_row(&event, &source_names, &video_source_names, filter(), 50)
            .into_string();

        assert!(
            markup.contains("Kobosil 300G"),
            "expected source pill text in rendered markup: {markup}"
        );
    }

    /// When the source can't be resolved at all (e.g. it was deleted, or the
    /// event has no video association), the pill must be omitted rather than
    /// rendered empty.
    #[test]
    fn download_event_without_resolvable_source_omits_pill() {
        let video_id = Ulid::from_string("01BX5ZZKBKACTAV9WEVGEMMVRZ").expect("valid ulid literal");
        let event = base_event(ActivityEventType::DownloadCompleted, Some(video_id));

        let source_names: HashMap<Ulid, String> = HashMap::new();
        let video_source_names: HashMap<Ulid, String> = HashMap::new();

        let markup = activity_event_row(&event, &source_names, &video_source_names, filter(), 50)
            .into_string();

        assert!(
            !markup.contains("inline-flex rounded-full bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs font-medium text-slate-600 dark:text-slate-300"),
            "did not expect a source pill span when the source cannot be resolved: {markup}"
        );
    }

    /// Download failure messages embed unbroken tokens (googlevideo URLs with
    /// long query strings). Without wrapping classes they overflow the card
    /// and stretch the page horizontally, so the message paragraph must keep
    /// both `break-words` and `wrap-anywhere`.
    #[test]
    fn long_error_message_paragraph_carries_wrapping_classes() {
        let mut event = base_event(ActivityEventType::DownloadFailed, None);
        event.severity = ActivitySeverity::Error;
        event.message = "[DOWNLOAD_FORMAT_UNAVAILABLE] Permanently failed after 2 attempts — \
             Download 128 failed: Unexpected HTTP status 401 for \
             https://rr2---sn-hoxu-h0jz.googlevideo.com/videoplayback?expire=1785506552&itag=278&source=youtube"
            .to_string();

        let source_names: HashMap<Ulid, String> = HashMap::new();
        let video_source_names: HashMap<Ulid, String> = HashMap::new();

        let markup = activity_event_row(&event, &source_names, &video_source_names, filter(), 50)
            .into_string();

        assert!(
            markup.contains("break-words wrap-anywhere"),
            "expected wrapping classes on the message paragraph: {markup}"
        );
    }
}

// ============================================================================
// Schedule Page
// ============================================================================

/// A view model combining source data with computed schedule info.
struct ScheduleEntry {
    source: Source,
    profile_name: String,
    next_index_at: Option<chrono::DateTime<Utc>>,
    is_overdue: bool,
}

async fn schedule_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(query): axum::extract::Query<SourcesQuery>,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let (sources_result, profiles_result, recent_activity_result, cleanup_status) = tokio::join!(
        db::list_sources(&state.pool),
        db::list_profiles(&state.pool),
        db::list_activity_events(&state.pool, 20, 0, None, None, None, None),
        state.cleanup.ask(GetCleanupStatus)
    );

    let sources = match sources_result {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load sources for schedule");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load schedule"),
            );
        }
    };

    let profiles = profiles_result.unwrap_or_default();
    let recent_activity = recent_activity_result.unwrap_or_default();

    // Filter to only source-related activity
    let recent_runs: Vec<_> = recent_activity
        .iter()
        .filter(|e| {
            matches!(
                e.event_type,
                ActivityEventType::SourceIndexed | ActivityEventType::SourceError
            )
        })
        .collect();

    // Build source name lookup for recent runs
    let source_names: std::collections::HashMap<Ulid, String> = sources
        .iter()
        .map(|s| (s.id, s.display_name().to_string()))
        .collect();

    let now = Utc::now();

    // Build schedule entries. The name lookup above is built from the full set
    // so recent runs still resolve names for sources filtered out of the table.
    let mut entries: Vec<ScheduleEntry> = sources
        .into_iter()
        .filter(|source| search.is_none_or(|needle| source_matches_search(source, needle)))
        .map(|source| {
            let profile_name = profiles
                .iter()
                .find(|p| p.id == source.profile_id)
                .map_or_else(|| "Unknown".to_string(), |p| p.name.clone());

            let next_index_at = source
                .last_indexed_at
                .map(|last| last + chrono::Duration::seconds(source.index_frequency_secs));

            let is_overdue = source.enabled
                && next_index_at.is_some_and(|next| next < now)
                && source.last_error.is_some();

            ScheduleEntry {
                source,
                profile_name,
                next_index_at,
                is_overdue,
            }
        })
        .collect();

    // Sort: enabled sources first (disabled sink to the bottom), then overdue first,
    // then by next index time (soonest first)
    entries.sort_by(|a, b| {
        b.source
            .enabled
            .cmp(&a.source.enabled)
            .then_with(|| b.is_overdue.cmp(&a.is_overdue))
            .then_with(|| a.next_index_at.cmp(&b.next_index_at))
    });

    let page = layout_with_flash(
        "Schedule",
        NavItem::Schedule,
        flash,
        html! {
            (cleanup_status_section(cleanup_status.ok().as_ref(), now))

            section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                div class="flex flex-wrap items-center justify-between gap-3" {
                    h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Upcoming Indexing" }
                    (source_search_form("/schedule", "Search channel name...", search))
                }
                @if entries.is_empty() {
                    p class="mt-4 rounded-lg border border-dashed border-slate-300 dark:border-slate-600 bg-slate-50 dark:bg-slate-800 px-4 py-8 text-center text-sm text-slate-500 dark:text-slate-400" {
                        @if search.is_some() {
                            "No sources match your search."
                        } @else {
                            "No sources configured yet. Add sources to start scheduling."
                        }
                    }
                } @else {
                    div class="mt-4 space-y-2" {
                        @for entry in &entries {
                            (schedule_entry_row(entry, now))
                        }
                    }
                }
            }

            (recent_runs_section(&recent_runs, &source_names))
        },
    );

    (StatusCode::OK, page)
}

fn cleanup_status_section(
    status: Option<&hof_core::actors::cleanup::CleanupStatus>,
    now: chrono::DateTime<Utc>,
) -> Markup {
    html! {
        section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            div class="flex flex-wrap items-center justify-between gap-4" {
                div {
                    h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Cleanup" }
                    p class="mt-1 text-sm text-slate-500 dark:text-slate-400" {
                        "Enforces retention policies and storage quotas by removing old files."
                    }
                }
                form method="post" action="/schedule/cleanup" {
                    button
                        class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-4 py-2 text-sm font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900"
                        type="submit"
                        onclick="return confirm('Run cleanup now? This will delete files past their retention period.')"
                    {
                        "Run Now"
                    }
                }
            }
            @if let Some(status) = status {
                div class="mt-4 grid gap-4 sm:grid-cols-3" {
                    // Status
                    div class="rounded-lg border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 p-3" {
                        p class="text-xs font-medium uppercase tracking-wide text-slate-500 dark:text-slate-400" { "Status" }
                        p class="mt-1 text-sm font-semibold text-slate-900 dark:text-slate-100" {
                            @if status.running { "Running" } @else { "Stopped" }
                        }
                    }
                    // Interval
                    @let interval_secs = i64::try_from(status.cleanup_interval_secs).unwrap_or(i64::MAX);
                    div class="rounded-lg border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 p-3" {
                        p class="text-xs font-medium uppercase tracking-wide text-slate-500 dark:text-slate-400" { "Interval" }
                        p class="mt-1 text-sm font-semibold text-slate-900 dark:text-slate-100" {
                            "every " (format_duration_human(interval_secs))
                        }
                    }
                    // Next run
                    div class="rounded-lg border border-slate-200 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 p-3" {
                        p class="text-xs font-medium uppercase tracking-wide text-slate-500 dark:text-slate-400" { "Next Run" }
                        p class="mt-1 text-sm font-semibold text-slate-900 dark:text-slate-100" {
                            @if let Some(last) = status.last_run_at {
                                @let next = last + chrono::Duration::seconds(interval_secs);
                                @if next > now {
                                    "in " (format_time_delta(next - now))
                                } @else {
                                    "due now"
                                }
                            } @else {
                                "pending"
                            }
                        }
                    }
                }
                @if let Some(days) = status.global_retention_days {
                    p class="mt-3 text-xs text-slate-500 dark:text-slate-400" {
                        "Global retention: " (days) " days"
                    }
                }
            } @else {
                p class="mt-4 text-sm text-slate-500 dark:text-slate-400" { "Unable to retrieve cleanup status." }
            }
        }
    }
}

fn recent_runs_section(
    recent_runs: &[&hof_core::domain::activity::ActivityEvent],
    source_names: &std::collections::HashMap<Ulid, String>,
) -> Markup {
    html! {
        section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Recent Indexing Runs" }
            @if recent_runs.is_empty() {
                p class="mt-4 rounded-lg border border-dashed border-slate-300 dark:border-slate-600 bg-slate-50 dark:bg-slate-800 px-4 py-8 text-center text-sm text-slate-500 dark:text-slate-400" {
                    "No indexing runs recorded yet."
                }
            } @else {
                div class="mt-4 overflow-x-auto" {
                    table class="min-w-full divide-y divide-slate-200 text-sm" {
                        thead class="bg-slate-50 dark:bg-slate-800" {
                            tr {
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Time" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Source" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Result" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Details" }
                            }
                        }
                        tbody class="divide-y divide-slate-100 dark:divide-slate-700 bg-white dark:bg-slate-900" {
                            @for run in recent_runs {
                                tr {
                                    td class="whitespace-nowrap px-3 py-2 text-slate-600 dark:text-slate-400" title=(run.created_at.to_rfc3339()) {
                                        (format_time_ago(run.created_at))
                                    }
                                    td class="px-3 py-2 text-slate-700 dark:text-slate-300" {
                                        @if let Some(source_id) = run.source_id {
                                            @if let Some(name) = source_names.get(&source_id) {
                                                (name)
                                            } @else {
                                                span class="text-slate-400 dark:text-slate-500 italic" { "deleted" }
                                            }
                                        } @else {
                                            span class="text-slate-400 dark:text-slate-500" { "—" }
                                        }
                                    }
                                    td class="px-3 py-2" {
                                        @if run.event_type == ActivityEventType::SourceIndexed {
                                            span class="inline-flex rounded-full bg-emerald-100 dark:bg-emerald-900/50 px-2.5 py-1 text-xs font-medium text-emerald-900 dark:text-emerald-100" {
                                                "OK"
                                            }
                                        } @else {
                                            span class="inline-flex rounded-full bg-rose-100 dark:bg-rose-900/50 px-2.5 py-1 text-xs font-medium text-rose-900 dark:text-rose-100" {
                                                "Error"
                                            }
                                        }
                                    }
                                    td class="max-w-md px-3 py-2 text-slate-700 dark:text-slate-300 break-words wrap-anywhere" {
                                        (run.message)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn schedule_entry_row(entry: &ScheduleEntry, now: chrono::DateTime<Utc>) -> Markup {
    let frequency = format_duration_human(entry.source.index_frequency_secs);

    let (status_text, status_classes) = if !entry.source.enabled {
        ("disabled".to_string(), "text-slate-500 dark:text-slate-400")
    } else if entry.is_overdue {
        let overdue_duration = entry.next_index_at.map_or_else(
            || "unknown".to_string(),
            |next| format_time_delta(now - next),
        );
        (
            format!("overdue by {overdue_duration}"),
            "text-rose-600 dark:text-rose-400 font-medium",
        )
    } else if let Some(next) = entry.next_index_at {
        if next > now {
            (
                format!("in {}", format_time_delta(next - now)),
                "text-slate-600 dark:text-slate-400",
            )
        } else {
            (
                "due now".to_string(),
                "text-amber-600 dark:text-amber-400 font-medium",
            )
        }
    } else {
        (
            "not yet indexed".to_string(),
            "text-slate-500 dark:text-slate-400 italic",
        )
    };

    let border = if !entry.source.enabled {
        "border-l-4 border-l-slate-300 dark:border-l-slate-600"
    } else if entry.is_overdue {
        "border-l-4 border-l-rose-400"
    } else if entry.source.last_error.is_some() {
        "border-l-4 border-l-amber-400"
    } else {
        "border-l-4 border-l-emerald-400"
    };

    html! {
        div class=(format!("flex flex-wrap items-center justify-between gap-3 rounded-lg border border-slate-200 dark:border-slate-700 {} bg-white dark:bg-slate-800 p-4", border)) {
            div class="min-w-0 flex-1" {
                div class="flex flex-wrap items-center gap-2" {
                    p class="text-sm font-semibold text-slate-900 dark:text-slate-100" { (entry.source.display_name()) }
                    span class="rounded-full bg-slate-100 dark:bg-slate-700 px-2 py-0.5 text-xs text-slate-500 dark:text-slate-400" {
                        (entry.profile_name)
                    }
                    @if !entry.source.enabled {
                        (status_chip("Disabled", "slate"))
                    }
                }
                @if let Some(ref error) = entry.source.last_error {
                    p class="mt-1 truncate text-xs text-rose-600 dark:text-rose-400" title=(error) {
                        "Error: " (error)
                    }
                }
            }
            div class="flex items-center gap-4 text-right" {
                div {
                    p class=(format!("text-sm {}", status_classes)) { (status_text) }
                    p class="text-xs text-slate-400 dark:text-slate-500" { "every " (frequency) }
                }
                @if entry.source.enabled {
                    form method="post" action=(format!("/sources/{}/index", entry.source.id)) {
                        button class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-xs font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900" type="submit" {
                            "Index Now"
                        }
                    }
                }
            }
        }
    }
}

fn format_time_ago(timestamp: chrono::DateTime<Utc>) -> String {
    let delta = Utc::now() - timestamp;
    format_time_delta(delta) + " ago"
}

fn format_time_delta(delta: chrono::TimeDelta) -> String {
    let secs = delta.num_seconds().max(0);
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        let mins = secs / 60;
        format!("{mins}m")
    } else if secs < 86_400 {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        if mins > 0 {
            format!("{hours}h {mins}m")
        } else {
            format!("{hours}h")
        }
    } else {
        let days = secs / 86_400;
        format!("{days}d")
    }
}

fn format_duration_human(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m > 0 {
            format!("{h}h {m}m")
        } else {
            format!("{h}h")
        }
    } else {
        let d = secs / 86_400;
        format!("{d}d")
    }
}

fn metric_card(title: &str, value: impl std::fmt::Display, description: &str) -> Markup {
    html! {
        article class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-5 shadow-sm" {
            p class="text-xs uppercase tracking-wide text-slate-500 dark:text-slate-400" { (title) }
            p class="mt-2 text-3xl font-semibold text-slate-900 dark:text-slate-100" { (value) }
            p class="mt-2 text-sm text-slate-600 dark:text-slate-400" { (description) }
        }
    }
}

/// Format a byte count using decimal (1000-based) SI units with a single decimal place,
/// e.g. `0.0 B`, `13.0 MB`, `1.4 GB`.
fn format_bytes_human(bytes: i64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes.max(0) as f64;
    let mut unit_index = 0usize;
    while value >= 1000.0 && unit_index < UNITS.len() - 1 {
        value /= 1000.0;
        unit_index += 1;
    }
    format!("{value:.1} {}", UNITS[unit_index])
}

/// Percentage of quota used, clamped to `0.0..=100.0`.
///
/// Returns `0.0` when the quota is non-positive and nothing has been used, so callers never
/// divide by zero. A non-positive quota with any usage at all is treated as fully over budget.
fn storage_usage_percent(used_bytes: i64, quota_bytes: i64) -> f64 {
    let quota = quota_bytes.max(0);
    if quota == 0 {
        return if used_bytes > 0 { 100.0 } else { 0.0 };
    }

    #[allow(clippy::cast_precision_loss)]
    let percent = (used_bytes.max(0) as f64 / quota as f64) * 100.0;
    percent.clamp(0.0, 100.0)
}

/// Whether usage exceeds the quota, treating a non-positive quota as leaving no room at all.
fn is_storage_over_quota(used_bytes: i64, quota_bytes: i64) -> bool {
    used_bytes > quota_bytes.max(0)
}

/// Render the storage quota usage card for the dashboard: a headline "used / quota" figure with
/// a progress bar, plus a per-profile breakdown when more than one profile exists.
fn storage_usage_card_markup(usage: &[db::ProfileStorageUsage]) -> Markup {
    let total_used: i64 = usage.iter().map(|profile| profile.used_bytes).sum();
    let total_quota: i64 = usage.iter().map(|profile| profile.quota_bytes).sum();
    let percent = storage_usage_percent(total_used, total_quota);
    let over_quota = is_storage_over_quota(total_used, total_quota);
    let headline_class = if over_quota {
        "text-rose-600 dark:text-rose-400"
    } else {
        "text-slate-900 dark:text-slate-100"
    };
    let bar_class = if over_quota {
        "bg-rose-500 dark:bg-rose-400"
    } else {
        "bg-emerald-500 dark:bg-emerald-400"
    };

    html! {
        section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Storage Quota" }
            @if usage.is_empty() {
                p class="mt-3 text-sm text-slate-500 dark:text-slate-400" { "No profiles configured yet." }
            } @else {
                p class=(format!("mt-3 text-2xl font-semibold {headline_class}")) {
                    (format_bytes_human(total_used)) " / " (format_bytes_human(total_quota))
                }
                div class="mt-2 h-2 w-full overflow-hidden rounded-full bg-slate-200 dark:bg-slate-700" {
                    div
                        class=(format!("h-full rounded-full {bar_class}"))
                        style=(format!("width: {percent:.1}%"))
                    {}
                }
                @if usage.len() > 1 {
                    ul class="mt-4 space-y-2" {
                        @for profile in usage {
                            @let profile_over = is_storage_over_quota(profile.used_bytes, profile.quota_bytes);
                            @let profile_class = if profile_over {
                                "text-rose-600 dark:text-rose-400 font-medium"
                            } else {
                                "text-slate-500 dark:text-slate-400"
                            };
                            li class="flex items-center justify-between gap-3 text-sm" {
                                span class="text-slate-700 dark:text-slate-300" { (profile.profile_name) }
                                span class=(profile_class) {
                                    (format_bytes_human(profile.used_bytes)) " / " (format_bytes_human(profile.quota_bytes))
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod storage_usage_tests {
    use super::{format_bytes_human, is_storage_over_quota, storage_usage_percent};

    #[test]
    fn format_bytes_human_zero() {
        assert_eq!(format_bytes_human(0), "0.0 B");
    }

    #[test]
    fn format_bytes_human_sub_kb() {
        assert_eq!(format_bytes_human(512), "512.0 B");
    }

    #[test]
    fn format_bytes_human_exact_kb_boundary() {
        assert_eq!(format_bytes_human(1000), "1.0 KB");
    }

    #[test]
    fn format_bytes_human_exact_mb_boundary() {
        assert_eq!(format_bytes_human(1000 * 1000), "1.0 MB");
    }

    #[test]
    fn format_bytes_human_mb_example() {
        assert_eq!(format_bytes_human(13 * 1000 * 1000), "13.0 MB");
    }

    #[test]
    fn format_bytes_human_gb_example() {
        // 1.4 GB, expressed as an exact byte count to avoid a lossy float-to-int cast.
        let bytes = 1_000_000_000_i64 + 400_000_000;
        assert_eq!(format_bytes_human(bytes), "1.4 GB");
    }

    #[test]
    fn format_bytes_human_exact_tb_boundary() {
        assert_eq!(format_bytes_human(1000_i64.pow(4)), "1.0 TB");
    }

    #[test]
    fn format_bytes_human_negative_clamped_to_zero() {
        assert_eq!(format_bytes_human(-5), "0.0 B");
    }

    #[test]
    fn percent_zero_quota_zero_used_is_zero() {
        assert!((storage_usage_percent(0, 0) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn percent_zero_quota_with_usage_is_fully_over() {
        assert!((storage_usage_percent(5, 0) - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn percent_normal_ratio() {
        assert!((storage_usage_percent(50, 100) - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn percent_over_quota_clamps_to_100() {
        assert!((storage_usage_percent(150, 100) - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn over_quota_detection() {
        assert!(!is_storage_over_quota(50, 100));
        assert!(is_storage_over_quota(150, 100));
        assert!(is_storage_over_quota(1, 0));
        assert!(!is_storage_over_quota(0, 0));
    }
}

fn profile_editor(profile: &Profile) -> Markup {
    html! {
        details class="group rounded-xl border border-slate-200 dark:border-slate-700 bg-slate-50/60 dark:bg-slate-800/60 p-4 open:bg-white dark:open:bg-slate-800" {
            summary class="cursor-pointer list-none" {
                div class="flex flex-wrap items-center justify-between gap-3" {
                    div {
                        p class="text-sm font-semibold text-slate-900 dark:text-slate-100" { (&profile.name) }
                        p class="text-xs text-slate-500 dark:text-slate-400" { (profile.id.to_string()) }
                    }
                    (status_chip(quality_label(&profile.quality), "sky"))
                }
            }
            form class="mt-4 grid gap-4 md:grid-cols-2" method="post" action={(format!("/profiles/{}", profile.id))} {
                (input_text("User ID", "user_id", "", true, &profile.user_id.to_string()))
                div {
                    label class="block text-sm font-medium text-slate-700 dark:text-slate-300" { "Quality" }
                    select class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm" name="quality" required {
                        @for quality in quality_options() {
                            option value=(quality.value) selected[(quality.value == quality_value(&profile.quality))] { (quality.label) }
                        }
                    }
                }
                div {
                    label class="block text-sm font-medium text-slate-700 dark:text-slate-300" { "Output Preset" }
                    select class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm" name="output_preset" required {
                        @for preset in output_preset_options() {
                            option value=(preset.value) selected[(preset.value == output_preset_value(&profile.output_preset))] { (preset.label) }
                        }
                    }
                }
                (input_text("Name", "name", "", true, &profile.name))
                (input_text("Naming Template", "naming_template", "", true, &profile.naming_template))
                (input_text("Output Directory", "output_dir", "", true, &profile.output_dir))
                (input_number("Storage Quota (GB)", "storage_quota_gb", "", true, &(profile.storage_quota_bytes / 1_000_000_000).to_string()))
                (input_number("Retention Days", "retention_days", "Optional", false, &profile.retention_days.map_or_else(String::new, |days| days.to_string())))
                div class="flex items-center gap-4" {
                    label class="inline-flex items-center gap-2 text-sm text-slate-700 dark:text-slate-300" {
                        input type="checkbox" name="include_livestreams" checked[profile.include_livestreams];
                        "Include Livestream VODs"
                    }
                    label class="inline-flex items-center gap-2 text-sm text-slate-700 dark:text-slate-300" {
                        input type="checkbox" name="include_shorts" checked[profile.include_shorts];
                        "Include Shorts"
                    }
                }
                div class="md:col-span-2 flex flex-wrap gap-2" {
                    button class="rounded-lg bg-slate-900 dark:bg-slate-100 px-4 py-2 text-sm font-medium text-white dark:text-slate-900 hover:bg-slate-700 dark:hover:bg-slate-200" type="submit" {
                        "Save Profile"
                    }
                    button
                        class="rounded-lg border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/50 px-4 py-2 text-sm font-medium text-rose-700 dark:text-rose-300 hover:bg-rose-100 dark:hover:bg-rose-900"
                        type="submit"
                        formaction={(format!("/profiles/{}/delete", profile.id))}
                        onclick="return confirm('Delete this profile? This cannot be undone.')"
                    {
                        "Delete"
                    }
                }
            }
        }
    }
}

fn source_editor(source: &Source) -> Markup {
    // Determine border color based on enabled and error state
    let border_class = if !source.enabled {
        "border-slate-300 dark:border-slate-600 bg-slate-100/60 dark:bg-slate-900/60 opacity-60"
    } else if source.last_error.is_some() {
        "border-rose-300 dark:border-rose-700 bg-rose-50/60 dark:bg-rose-900/30"
    } else {
        "border-slate-200 dark:border-slate-700 bg-slate-50/60 dark:bg-slate-800/60"
    };

    html! {
        details class=(format!("group rounded-xl border {} p-4 open:bg-white dark:open:bg-slate-800 open:opacity-100", border_class)) {
            summary class="cursor-pointer list-none" {
                div class="flex flex-wrap items-center justify-between gap-3" {
                    div class="min-w-0" {
                        p class="truncate text-sm font-semibold text-slate-900 dark:text-slate-100" {
                            (source.custom_name.as_deref().unwrap_or(&source.url))
                        }
                        p class="truncate text-xs text-slate-500 dark:text-slate-400" { (source.id.to_string()) }
                    }
                    div class="flex items-center gap-2" {
                        a href=(format!("/sources/{}", source.id))
                            class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-xs font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900"
                        {
                            "View Videos"
                        }
                        @if !source.enabled {
                            (status_chip("Disabled", "amber"))
                        }
                        @if source.last_error.is_some() {
                            (status_chip(&format!("Error ({})", source.index_error_count), "rose"))
                        }
                        (status_chip(source_type_label(&source.source_type), "slate"))
                    }
                }
            }

            // Show error message if present
            @if let Some(ref error) = source.last_error {
                div class="mt-3 rounded-lg border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/30 p-3" {
                    p class="text-sm font-medium text-rose-800 dark:text-rose-200" { "Last Indexing Error:" }
                    p class="mt-1 text-sm text-rose-700 dark:text-rose-300 font-mono whitespace-pre-wrap break-all" { (error) }
                    p class="mt-2 text-xs text-rose-600 dark:text-rose-400" {
                        "Consecutive errors: " (source.index_error_count)
                    }
                }
            }
            form class="mt-4 grid gap-4 md:grid-cols-2" method="post" action={(format!("/sources/{}", source.id))} {
                (input_text("Profile ID", "profile_id", "", true, &source.profile_id.to_string()))
                div {
                    label class="block text-sm font-medium text-slate-700 dark:text-slate-300" { "Source Type" }
                    select class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm" name="source_type" required {
                        option value="channel" selected[matches!(source.source_type, SourceType::Channel)] { "Channel" }
                        option value="playlist" selected[matches!(source.source_type, SourceType::Playlist)] { "Playlist" }
                    }
                }
                (input_text("URL", "url", "", true, &source.url))
                (input_text("Custom Name", "custom_name", "Optional", false, &source.custom_name.clone().unwrap_or_default()))
                (input_index_frequency("Index Frequency", "index_frequency_secs", source.index_frequency_secs))
                (input_cutoff_date("Cutoff Date", "cutoff_date", &source.cutoff_date.to_string()))
                (input_number("Retention Days", "retention_days", "Optional", false, &source.retention_days.map_or_else(String::new, |days| days.to_string())))
                div class="md:col-span-2 flex flex-wrap gap-2" {
                    button class="rounded-lg bg-slate-900 dark:bg-slate-100 px-4 py-2 text-sm font-medium text-white dark:text-slate-900 hover:bg-slate-700 dark:hover:bg-slate-200" type="submit" {
                        "Save Source"
                    }
                    @if source.enabled {
                        button class="rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/50 px-4 py-2 text-sm font-medium text-amber-700 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900" type="submit" formaction={(format!("/sources/{}/toggle", source.id))} {
                            "Disable"
                        }
                    } @else {
                        button class="rounded-lg border border-emerald-200 dark:border-emerald-800 bg-emerald-50 dark:bg-emerald-900/50 px-4 py-2 text-sm font-medium text-emerald-700 dark:text-emerald-300 hover:bg-emerald-100 dark:hover:bg-emerald-900" type="submit" formaction={(format!("/sources/{}/toggle", source.id))} {
                            "Enable"
                        }
                    }
                    button class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-4 py-2 text-sm font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900" type="submit" formaction={(format!("/sources/{}/index", source.id))} {
                        "Trigger Index"
                    }
                    button class="rounded-lg border border-violet-200 dark:border-violet-800 bg-violet-50 dark:bg-violet-900/50 px-4 py-2 text-sm font-medium text-violet-700 dark:text-violet-300 hover:bg-violet-100 dark:hover:bg-violet-900" type="submit" formaction={(format!("/sources/{}/metadata", source.id))} {
                        "Trigger Image Download"
                    }
                    button
                        class="rounded-lg border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/50 px-4 py-2 text-sm font-medium text-rose-700 dark:text-rose-300 hover:bg-rose-100 dark:hover:bg-rose-900"
                        type="submit"
                        formaction={(format!("/sources/{}/delete", source.id))}
                        onclick="return confirm('Delete this source? This cannot be undone.')"
                    {
                        "Delete"
                    }
                }
            }
        }
    }
}

fn sources_list_markup(sources: &[Source], search: Option<&str>) -> Markup {
    html! {
        @if sources.is_empty() {
            p class="mt-3 text-sm text-slate-500 dark:text-slate-400" {
                @if search.is_some() {
                    "No sources match your search."
                } @else {
                    "No sources yet."
                }
            }
        } @else {
            div class="mt-4 space-y-4" {
                @for source in sources {
                    (source_editor(source))
                }
            }
        }
    }
}

fn source_detail_content_markup(
    source: &Source,
    videos: &[hof_core::domain::video::Video],
) -> Markup {
    html! {
        (source_detail_header(source, videos.len()))
        (source_videos_table(videos))
    }
}

fn source_detail_header(source: &Source, video_count: usize) -> Markup {
    html! {
        section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            div class="flex items-start gap-4" {
                @if let Some(ref thumb) = source.channel_thumbnail_url {
                    img src=(thumb) alt="Channel thumbnail" class="h-16 w-16 rounded-full object-cover";
                }
                div {
                    h2 class="text-xl font-semibold text-slate-900 dark:text-slate-100" {
                        (source.display_name())
                    }
                    p class="mt-1 text-sm text-slate-500 dark:text-slate-400" {
                        a href=(source.url) target="_blank" class="hover:underline" { (source.url) }
                    }
                    div class="mt-2 flex flex-wrap gap-2 text-xs text-slate-600 dark:text-slate-400" {
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1" {
                            (format!("{:?}", source.source_type))
                        }
                        span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1" {
                            (format!("{video_count} videos"))
                        }
                        span class=(entry_order_badge_class(source.entry_order)) {
                            (entry_order_label(source.entry_order))
                        }
                        @if let Some(indexed_at) = source.last_indexed_at {
                            span class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1" {
                                "Last indexed: " (format_time_ago(indexed_at))
                            }
                        }
                    }
                }
            }
        }
    }
}

fn source_videos_table(videos: &[hof_core::domain::video::Video]) -> Markup {
    html! {
        section class="mt-6 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
            h3 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Videos" }
            @if videos.is_empty() {
                p class="mt-3 text-sm text-slate-500 dark:text-slate-400" {
                    "No videos indexed yet. Try triggering an index from the Sources page."
                }
            } @else {
                div class="mt-4 overflow-x-auto" {
                    table class="min-w-full divide-y divide-slate-200 dark:divide-slate-700 text-sm" {
                        thead class="bg-slate-50 dark:bg-slate-800" {
                            tr {
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Thumbnail" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Title" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Duration" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Published" }
                                th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Status" }
                            }
                        }
                        tbody class="divide-y divide-slate-100 dark:divide-slate-700 bg-white dark:bg-slate-900" {
                            @for video in videos {
                                (video_table_row(video))
                            }
                        }
                    }
                }
            }
        }
    }
}

fn video_table_row(video: &hof_core::domain::video::Video) -> Markup {
    html! {
        tr {
            td class="px-3 py-2" {
                @if let Some(ref thumb) = video.thumbnail_url {
                    img src=(thumb) alt="" class="h-12 w-20 rounded object-cover";
                } @else {
                    div class="h-12 w-20 rounded bg-slate-200 dark:bg-slate-700" {}
                }
            }
            td class="max-w-md px-3 py-2 text-slate-900 dark:text-slate-100" {
                p class="truncate font-medium" { (video.title) }
                p class="truncate text-xs text-slate-500 dark:text-slate-400" {
                    (video.platform_video_id)
                }
            }
            td class="px-3 py-2 text-slate-600 dark:text-slate-400" {
                @if let Some(secs) = video.duration_secs {
                    (format_duration_human(secs))
                } @else {
                    "—"
                }
            }
            td class="px-3 py-2 text-slate-600 dark:text-slate-400" {
                @if let Some(published) = video.published_at {
                    (published.format("%Y-%m-%d"))
                } @else {
                    "—"
                }
            }
            td class="px-3 py-2" { (status_badge(&video.status)) }
        }
    }
}

const fn quality_options() -> &'static [QualityOption] {
    &[
        QualityOption {
            value: "best",
            label: "Best",
        },
        QualityOption {
            value: "4320p",
            label: "4320p",
        },
        QualityOption {
            value: "2160p",
            label: "2160p",
        },
        QualityOption {
            value: "1440p",
            label: "1440p",
        },
        QualityOption {
            value: "1080p",
            label: "1080p",
        },
        QualityOption {
            value: "720p",
            label: "720p",
        },
        QualityOption {
            value: "480p",
            label: "480p",
        },
        QualityOption {
            value: "audio_only",
            label: "Audio only",
        },
    ]
}

struct QualityOption {
    value: &'static str,
    label: &'static str,
}

const fn output_preset_options() -> &'static [OutputPresetOption] {
    &[
        OutputPresetOption {
            value: "auto",
            label: "Auto (best quality)",
        },
        OutputPresetOption {
            value: "browser",
            label: "Browser (Jellyfin/web direct-play)",
        },
        OutputPresetOption {
            value: "tv",
            label: "TV (smart TV direct-play)",
        },
    ]
}

struct OutputPresetOption {
    value: &'static str,
    label: &'static str,
}

const fn quality_label(quality: &Quality) -> &'static str {
    match quality {
        Quality::Best => "Best",
        Quality::Q4320p => "4320p",
        Quality::Q2160p => "2160p",
        Quality::Q1440p => "1440p",
        Quality::Q1080p => "1080p",
        Quality::Q720p => "720p",
        Quality::Q480p => "480p",
        Quality::AudioOnly => "Audio only",
    }
}

const fn quality_value(quality: &Quality) -> &'static str {
    match quality {
        Quality::Best => "best",
        Quality::Q4320p => "4320p",
        Quality::Q2160p => "2160p",
        Quality::Q1440p => "1440p",
        Quality::Q1080p => "1080p",
        Quality::Q720p => "720p",
        Quality::Q480p => "480p",
        Quality::AudioOnly => "audio_only",
    }
}

const fn output_preset_value(output_preset: &OutputPreset) -> &'static str {
    match output_preset {
        OutputPreset::Auto => "auto",
        OutputPreset::Browser => "browser",
        OutputPreset::Tv => "tv",
    }
}

const fn source_type_label(source_type: &SourceType) -> &'static str {
    match source_type {
        SourceType::Channel => "Channel",
        SourceType::Playlist => "Playlist",
    }
}

const fn entry_order_label(order: EntryOrder) -> &'static str {
    match order {
        EntryOrder::Unknown => "yt-dlp: Unknown",
        EntryOrder::Ascending => "yt-dlp: Oldest first",
        EntryOrder::Descending => "yt-dlp: Newest first",
        EntryOrder::Unordered => "yt-dlp: Unordered",
    }
}

const fn entry_order_badge_class(order: EntryOrder) -> &'static str {
    match order {
        EntryOrder::Unknown => {
            "rounded bg-amber-100 dark:bg-amber-900/50 text-amber-900 dark:text-amber-100 px-2 py-1"
        }
        EntryOrder::Ascending | EntryOrder::Descending => {
            "rounded bg-slate-100 dark:bg-slate-700 px-2 py-1"
        }
        EntryOrder::Unordered => {
            "rounded bg-orange-100 dark:bg-orange-900/50 text-orange-900 dark:text-orange-100 px-2 py-1"
        }
    }
}

fn status_badge(status: &VideoStatus) -> Markup {
    let (label, color) = match status {
        VideoStatus::Pending => (
            "Pending",
            "bg-amber-100 dark:bg-amber-900/50 text-amber-900 dark:text-amber-100",
        ),
        VideoStatus::Downloading => (
            "Downloading",
            "bg-sky-100 dark:bg-sky-900/50 text-sky-900 dark:text-sky-100",
        ),
        VideoStatus::Completed => (
            "Completed",
            "bg-emerald-100 dark:bg-emerald-900/50 text-emerald-900 dark:text-emerald-100",
        ),
        VideoStatus::Failed => (
            "Failed",
            "bg-rose-100 dark:bg-rose-900/50 text-rose-900 dark:text-rose-100",
        ),
        VideoStatus::Skipped => (
            "Skipped",
            "bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-300",
        ),
        VideoStatus::Cleaned => (
            "Cleaned",
            "bg-violet-100 dark:bg-violet-900/50 text-violet-900 dark:text-violet-100",
        ),
        VideoStatus::PermanentlyFailed => (
            "Permanently Failed",
            "bg-red-100 dark:bg-red-900/50 text-red-900 dark:text-red-100",
        ),
    };

    html! {
        span class={(format!("inline-flex rounded-full px-2.5 py-1 text-xs font-medium {}", color))} {
            (label)
        }
    }
}

fn status_chip(label: &str, tone: &str) -> Markup {
    let class_name = match tone {
        "sky" => "bg-sky-100 dark:bg-sky-900/50 text-sky-900 dark:text-sky-100",
        "slate" => "bg-slate-200 dark:bg-slate-700 text-slate-900 dark:text-slate-100",
        "rose" => "bg-rose-100 dark:bg-rose-900/50 text-rose-900 dark:text-rose-100",
        _ => "bg-slate-100 dark:bg-slate-700 text-slate-700 dark:text-slate-300",
    };

    html! {
        span class={(format!("inline-flex rounded-full px-2.5 py-1 text-xs font-medium {}", class_name))} {
            (label)
        }
    }
}

fn input_text(label: &str, name: &str, placeholder: &str, required: bool, value: &str) -> Markup {
    html! {
        div {
            label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for=(name) { (label) }
            input
                class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm"
                type="text"
                id=(name)
                name=(name)
                placeholder=(placeholder)
                required[required]
                value=(value);
        }
    }
}

fn input_number(label: &str, name: &str, placeholder: &str, required: bool, value: &str) -> Markup {
    html! {
        div {
            label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for=(name) { (label) }
            input
                class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm"
                type="number"
                id=(name)
                name=(name)
                placeholder=(placeholder)
                required[required]
                value=(value);
        }
    }
}

struct IndexFrequencyOption {
    label: &'static str,
    value: i64,
}

fn index_frequency_options() -> Vec<IndexFrequencyOption> {
    vec![
        IndexFrequencyOption {
            label: "1 hour",
            value: 3600,
        },
        IndexFrequencyOption {
            label: "3 hours",
            value: 10800,
        },
        IndexFrequencyOption {
            label: "6 hours",
            value: 21600,
        },
        IndexFrequencyOption {
            label: "12 hours",
            value: 43200,
        },
        IndexFrequencyOption {
            label: "24 hours",
            value: 86400,
        },
        IndexFrequencyOption {
            label: "3 days",
            value: 259_200,
        },
        IndexFrequencyOption {
            label: "7 days",
            value: 604_800,
        },
        IndexFrequencyOption {
            label: "30 days",
            value: 2_592_000,
        },
    ]
}

fn input_index_frequency(label: &str, name: &str, selected_value: i64) -> Markup {
    html! {
        div {
            label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for=(name) { (label) }
            select
                class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm"
                name=(name)
                id=(name)
                required {
                @for option in index_frequency_options() {
                    option value=(option.value) selected[option.value == selected_value] { (option.label) }
                }
            }
        }
    }
}

fn input_cutoff_date(label: &str, name: &str, value: &str) -> Markup {
    html! {
        div {
            label class="block text-sm font-medium text-slate-700 dark:text-slate-300" for=(name) { (label) }
            input
                class="mt-1 w-full rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 shadow-sm"
                type="date"
                id=(name)
                name=(name)
                required
                value=(value);
            div class="mt-2 flex flex-wrap gap-2" {
                // Offset by the viewer's timezone so "today" is their local
                // date, not the UTC date `toISOString` would otherwise yield.
                button
                    class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                    type="button"
                    onclick={"document.getElementById('" (name) "').value = new Date(Date.now() - new Date().getTimezoneOffset()*60*1000).toISOString().split('T')[0]"} {
                    "Today"
                }
                button
                    class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                    type="button"
                    onclick={"document.getElementById('" (name) "').value = new Date(Date.now() - 7*24*60*60*1000).toISOString().split('T')[0]"} {
                    "7 days"
                }
                button
                    class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                    type="button"
                    onclick={"document.getElementById('" (name) "').value = new Date(Date.now() - 14*24*60*60*1000).toISOString().split('T')[0]"} {
                    "14 days"
                }
                button
                    class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                    type="button"
                    onclick={"document.getElementById('" (name) "').value = new Date(Date.now() - 30*24*60*60*1000).toISOString().split('T')[0]"} {
                    "30 days"
                }
                button
                    class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                    type="button"
                    onclick={"document.getElementById('" (name) "').value = new Date(Date.now() - 90*24*60*60*1000).toISOString().split('T')[0]"} {
                    "90 days"
                }
                button
                    class="rounded bg-slate-100 dark:bg-slate-700 px-2 py-1 text-xs font-medium text-slate-700 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
                    type="button"
                    onclick={"document.getElementById('" (name) "').value = new Date(Date.now() - 180*24*60*60*1000).toISOString().split('T')[0]"} {
                    "180 days"
                }
            }
        }
    }
}

#[cfg(test)]
mod input_cutoff_date_tests {
    use super::input_cutoff_date;

    /// The "Today" shortcut is a convenience only — the field itself must
    /// still render pre-filled with whatever default the caller passed
    /// (a week ago, for the create form).
    #[test]
    fn renders_today_pill_without_changing_the_prefilled_value() {
        let markup = input_cutoff_date("Cutoff Date", "cutoff_date", "2026-07-24").into_string();

        assert!(
            markup.contains(">Today<"),
            "expected a Today pill: {markup}"
        );
        assert!(
            markup.contains(r#"value="2026-07-24""#),
            "expected the passed-in default to stay pre-filled: {markup}"
        );
        assert!(
            markup.contains("getTimezoneOffset"),
            "Today must resolve to the viewer's local date, not the UTC date: {markup}"
        );
    }
}

fn parse_optional_i32(raw: Option<&str>, field_name: &str) -> Result<Option<i32>, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => value
            .parse::<i32>()
            .map(Some)
            .map_err(|_| format!("{field_name} must be a valid whole number")),
    }
}

fn error_page(message: &str) -> Markup {
    layout(
        "Error",
        NavItem::Dashboard,
        html! {
            section class="rounded-2xl border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/30 p-6 shadow-sm" {
                h2 class="text-lg font-semibold text-rose-900 dark:text-rose-100" { "Something went wrong" }
                p class="mt-2 text-sm text-rose-800 dark:text-rose-200 break-words wrap-anywhere" { (message) }
                div class="mt-4" {
                    a class="inline-flex rounded-lg bg-rose-700 dark:bg-rose-600 px-4 py-2 text-sm font-medium text-white hover:bg-rose-800 dark:hover:bg-rose-700" href="/dashboard" {
                        "Back to dashboard"
                    }
                }
            }
        },
    )
}

/// Error markup for htmx partial endpoints.
///
/// Partials are swapped into an existing element with `innerHTML`, so they must
/// never return a full document. Returning [`error_page`] here nests a
/// `<!DOCTYPE><html><head>` inside a `<div>`, which browsers reparent — dropping
/// the stylesheet link and leaving the page unstyled.
fn error_fragment(message: &str) -> Markup {
    html! {
        section class="rounded-2xl border border-rose-200 dark:border-rose-800 bg-rose-50 dark:bg-rose-900/30 p-6 shadow-sm" {
            h2 class="text-lg font-semibold text-rose-900 dark:text-rose-100" { "Something went wrong" }
            p class="mt-2 text-sm text-rose-800 dark:text-rose-200 break-words wrap-anywhere" { (message) }
        }
    }
}

/// Handler for 404 Not Found responses.
async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, not_found_page())
}

fn not_found_page() -> Markup {
    auth_layout(
        "Page Not Found",
        html! {
            div class="text-center" {
                div class="mb-6 text-6xl font-bold text-slate-300 dark:text-slate-600" { "404" }
                h2 class="text-xl font-semibold text-slate-900 dark:text-slate-100 mb-2" {
                    "Page Not Found"
                }
                p class="text-slate-600 dark:text-slate-400 mb-6" {
                    "The page you're looking for doesn't exist or has been moved."
                }
                a
                    href="/dashboard"
                    class="inline-flex rounded-lg bg-slate-900 dark:bg-slate-100 px-4 py-2.5 text-sm font-medium text-white dark:text-slate-900 hover:bg-slate-800 dark:hover:bg-slate-200 transition"
                {
                    "Go to Dashboard"
                }
            }
        },
    )
}

fn layout(title: &str, active: NavItem, content: impl Render) -> Markup {
    layout_with_flash(title, active, None, content)
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
fn layout_with_flash(
    title: &str,
    active: NavItem,
    flash: Option<FlashMessage>,
    content: impl Render,
) -> Markup {
    let heading = format!("{title} · Hofvarpnir");
    html! {
        (DOCTYPE)
        html lang="en" class="h-full" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (heading) }
                link rel="icon" type="image/x-icon" href="/assets/favicon.ico";
                link rel="icon" type="image/png" sizes="32x32" href="/assets/favicon-32x32.png";
                link rel="apple-touch-icon" href="/assets/apple-touch-icon.png";
                link rel="stylesheet" href="/assets/app.css";
                script src="https://unpkg.com/htmx.org@2.0.4" defer {}
                script src="https://unpkg.com/htmx-ext-sse@2.2.2/sse.js" defer {}
                // Dark mode initialization (runs before body renders to prevent flash)
                (PreEscaped(r"<script>
                    (function() {
                        var stored = localStorage.getItem('darkMode');
                        if (stored === 'true' || (stored === null && window.matchMedia('(prefers-color-scheme: dark)').matches)) {
                            document.documentElement.classList.add('dark');
                        }
                    })();
                </script>"))
            }
            body class="min-h-full bg-gradient-to-b from-slate-100 via-slate-50 to-white dark:from-slate-900 dark:via-slate-900 dark:to-slate-950 text-slate-900 dark:text-slate-100" {
                // Toast notification
                @if let Some(ref flash) = flash {
                    @let flash_classes = match flash.level.as_str() {
                        "success" => "bg-emerald-100 dark:bg-emerald-900/50 text-emerald-900 dark:text-emerald-100 border-emerald-200 dark:border-emerald-700",
                        "error" => "bg-rose-100 dark:bg-rose-900/50 text-rose-900 dark:text-rose-100 border-rose-200 dark:border-rose-700",
                        _ => "bg-sky-100 dark:bg-sky-900/50 text-sky-900 dark:text-sky-100 border-sky-200 dark:border-sky-700",
                    };
                    div id="toast"
                        class=(format!("fixed top-4 right-4 z-50 rounded-lg border px-4 py-3 text-sm font-medium shadow-lg transition-opacity duration-300 {flash_classes}"))
                    {
                        (flash.message)
                    }
                }

                div class="mx-auto flex min-h-screen w-full max-w-7xl flex-col px-4 py-8 sm:px-6 lg:px-8" {
                    // System issues banner (loaded via htmx on page load)
                    div
                        hx-get="/web/system-banner"
                        hx-trigger="load"
                        hx-swap="innerHTML"
                    {}

                    header class="mb-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-5 shadow-sm backdrop-blur" {
                        div class="flex flex-wrap items-center justify-between gap-4" {
                            div class="flex items-center gap-3" {
                                img src="/assets/logo.png" alt="Hofvarpnir logo" class="h-12 w-12 shrink-0";
                                div {
                                    p class="text-xs uppercase tracking-[0.2em] text-slate-500 dark:text-slate-400" { "Hofvarpnir" }
                                    h1 class="text-2xl font-semibold text-slate-900 dark:text-slate-100" { (title) }
                                }
                            }
                            nav class="flex flex-wrap items-center gap-2" {
                                (nav_link("/dashboard", "Dashboard", active == NavItem::Dashboard))
                                (nav_link("/profiles", "Profiles", active == NavItem::Profiles))
                                (nav_link("/sources", "Sources", active == NavItem::Sources))
                                (nav_link("/downloads", "Downloads", active == NavItem::Downloads))
                                (nav_link("/activity", "Activity", active == NavItem::Activity))
                                (nav_link("/schedule", "Schedule", active == NavItem::Schedule))
                                (nav_link("/settings/api-keys", "API Keys", active == NavItem::ApiKeys))
                                // Dark mode toggle
                                button
                                    type="button"
                                    id="dark-toggle"
                                    class="inline-flex items-center rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1.5 text-sm font-medium text-slate-600 dark:text-slate-300 transition hover:bg-slate-200 dark:hover:bg-slate-600"
                                    onclick="toggleDarkMode()"
                                {
                                    span class="dark:hidden" { "🌙" }
                                    span class="hidden dark:inline" { "☀️" }
                                }
                                form method="post" action="/logout" class="ml-2" {
                                    button
                                        type="submit"
                                        class="inline-flex items-center rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1.5 text-sm font-medium text-slate-500 dark:text-slate-300 transition hover:bg-red-100 hover:text-red-700 dark:hover:bg-red-900/50 dark:hover:text-red-400"
                                    {
                                        "Logout"
                                    }
                                }
                            }
                        }
                    }
                    main class="flex-1" { (content) }
                }
                (PreEscaped(r"<script>
                    // Dark mode toggle
                    function toggleDarkMode() {
                      var html = document.documentElement;
                      var isDark = html.classList.toggle('dark');
                      localStorage.setItem('darkMode', isDark);
                    }

                    // Auto-dismiss toast
                    (function() {
                      var t = document.getElementById('toast');
                      if (t) {
                        setTimeout(function() { t.style.opacity = '0'; }, 3500);
                        setTimeout(function() { t.remove(); }, 4000);
                      }
                    })();

                    // Loading state on form submit. Skips a submit already
                    // cancelled (e.g. a declined confirm), which would otherwise
                    // leave the button stuck disabled on a form that never went.
                    document.addEventListener('submit', function(e) {
                      var form = e.target;
                      if (form.tagName !== 'FORM') return;
                      if (e.defaultPrevented) return;
                      var btn = e.submitter;
                      if (!btn || btn.disabled) return;
                      btn.disabled = true;
                      btn.dataset.originalText = btn.textContent;
                      btn.textContent = 'Working\u2026';
                    });

                    // A changed search term invalidates the current page offset,
                    // so snap the companion page field back to 1. Delegated so it
                    // survives htmx swaps replacing the filter bar.
                    document.addEventListener('input', function(e) {
                      var el = e.target;
                      if (!el || !el.dataset || !el.dataset.resetPage) return;
                      var field = document.getElementById(el.dataset.resetPage);
                      if (field) field.value = '1';
                    });

                    // ---- Bulk selection on the downloads table ----
                    // All handlers are delegated from document: the table is
                    // replaced wholesale by htmx and SSE, so anything bound to
                    // the elements themselves would be lost on the next swap.
                    // Selection deliberately resets on swap, since the rows (and
                    // their statuses) may no longer be the ones that were picked.
                    var BULK_ELIGIBLE = {
                      retry: ['failed', 'permanently_failed', 'cleaned'],
                      cancel: ['pending', 'downloading'],
                      delete: ['completed']
                    };

                    function bulkBoxes() {
                      return Array.prototype.slice.call(
                        document.querySelectorAll('[data-bulk-select]'));
                    }

                    function bulkRefresh() {
                      var form = document.querySelector('[data-bulk-form]');
                      if (!form) return;
                      var selected = bulkBoxes().filter(function(b) { return b.checked; });
                      var ids = selected.map(function(b) { return b.dataset.bulkSelect; });
                      var statuses = selected.map(function(b) { return b.dataset.status; });

                      form.hidden = ids.length === 0;
                      var idField = form.querySelector('[data-bulk-ids]');
                      if (idField) idField.value = ids.join(',');
                      var count = form.querySelector('[data-bulk-count]');
                      if (count) count.textContent = String(ids.length);

                      Object.keys(BULK_ELIGIBLE).forEach(function(action) {
                        var btn = form.querySelector('[data-bulk-button=' + action + ']');
                        if (!btn) return;
                        var ok = statuses.some(function(s) {
                          return BULK_ELIGIBLE[action].indexOf(s) !== -1;
                        });
                        btn.disabled = !ok;
                        btn.title = ok ? '' : 'No selected download is eligible';
                      });

                      var all = document.querySelector('[data-bulk-select-all]');
                      if (all) {
                        var boxes = bulkBoxes();
                        all.checked = boxes.length > 0 && selected.length === boxes.length;
                        all.indeterminate = selected.length > 0 && selected.length < boxes.length;
                      }
                    }

                    document.addEventListener('change', function(e) {
                      var el = e.target;
                      if (!el || !el.dataset) return;
                      if (el.dataset.bulkSelectAll !== undefined) {
                        bulkBoxes().forEach(function(b) { b.checked = el.checked; });
                        bulkRefresh();
                      } else if (el.dataset.bulkSelect !== undefined) {
                        bulkRefresh();
                      }
                    });

                    document.addEventListener('click', function(e) {
                      var el = e.target.closest ? e.target.closest('[data-bulk-clear]') : null;
                      if (!el) return;
                      e.preventDefault();
                      bulkBoxes().forEach(function(b) { b.checked = false; });
                      var all = document.querySelector('[data-bulk-select-all]');
                      if (all) { all.checked = false; all.indeterminate = false; }
                      bulkRefresh();
                    });

                    // Record which button submitted, and confirm destructive ones.
                    // `submitter.value` is not sent for us because the action is
                    // carried in a hidden field the server reads.
                    // Capture phase so this runs before the loading-state handler
                    // above and can cancel the submit cleanly.
                    document.addEventListener('submit', function(e) {
                      var form = e.target;
                      if (!form.dataset || form.dataset.bulkForm === undefined) return;
                      var btn = e.submitter;
                      if (!btn) return;
                      if (btn.dataset.confirm && !confirm(btn.dataset.confirm)) {
                        e.preventDefault();
                        return;
                      }
                      var field = form.querySelector('[data-bulk-action]');
                      if (field) field.value = btn.value;
                    }, true);

                    document.body.addEventListener('htmx:afterSwap', bulkRefresh);
                    bulkRefresh();
                    </script>"))
            }
        }
    }
}

fn nav_link(href: &str, label: &str, selected: bool) -> Markup {
    let classes = if selected {
        "inline-flex items-center rounded-full bg-slate-900 dark:bg-slate-100 px-3 py-1.5 text-sm font-medium text-white dark:text-slate-900"
    } else {
        "inline-flex items-center rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1.5 text-sm font-medium text-slate-700 dark:text-slate-200 transition hover:bg-slate-200 dark:hover:bg-slate-600"
    };

    html! {
        a class=(classes) href=(href) { (label) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Query-value encoding
    // ------------------------------------------------------------------

    #[test]
    fn encode_query_value_escapes_ampersand_and_space() {
        assert_eq!(encode_query_value("rust & go"), "rust%20%26%20go");
    }

    #[test]
    fn encode_query_value_escapes_fragment_and_equals() {
        assert_eq!(encode_query_value("a#b=c"), "a%23b%3Dc");
    }

    #[test]
    fn encode_query_value_leaves_unreserved_characters_alone() {
        assert_eq!(encode_query_value("a-b_c.d~e9"), "a-b_c.d~e9");
    }

    // ------------------------------------------------------------------
    // Downloads URL construction
    // ------------------------------------------------------------------

    #[test]
    fn downloads_url_omits_defaults() {
        assert_eq!(downloads_page_url(None, None, 1, 25), "/downloads");
    }

    #[test]
    fn downloads_url_includes_non_default_params() {
        assert_eq!(
            downloads_page_url(Some("failed"), Some("cats"), 3, 50),
            "/downloads?status=failed&search=cats&page=3&per_page=50"
        );
    }

    #[test]
    fn downloads_url_treats_all_status_as_unfiltered() {
        assert_eq!(downloads_page_url(Some("all"), None, 1, 25), "/downloads");
    }

    #[test]
    fn downloads_url_percent_encodes_search() {
        assert_eq!(
            downloads_page_url(None, Some("a&b"), 1, 25),
            "/downloads?search=a%26b"
        );
    }

    /// A search term containing `&` must not smuggle in an extra parameter.
    #[test]
    fn downloads_url_search_cannot_inject_parameters() {
        let url = downloads_page_url(None, Some("x&status=completed"), 1, 25);
        assert_eq!(url, "/downloads?search=x%26status%3Dcompleted");
        assert!(!url.contains("&status=completed"));
    }

    // ------------------------------------------------------------------
    // Regression: the pushed URL must be the navigable page, never the
    // partial endpoint. Pushing `/web/downloads/list` meant a reload served
    // a bare fragment with no <head>, leaving the page unstyled.
    // ------------------------------------------------------------------

    #[test]
    fn page_urls_and_partial_urls_are_distinct() {
        let page = downloads_page_url(Some("failed"), None, 2, 25);
        let list = downloads_list_url(Some("failed"), None, 2, 25);
        let events = downloads_events_url(Some("failed"), None, 2, 25);

        assert!(page.starts_with("/downloads"));
        assert!(list.starts_with("/web/downloads/list"));
        assert!(events.starts_with("/web/downloads/events"));
        assert_ne!(page, list);
    }

    #[test]
    fn pushed_download_urls_are_never_partial_endpoints() {
        for page in [1, 2, 7] {
            for status in [None, Some("failed"), Some("all")] {
                let pushed = downloads_page_url(status, Some("q"), page, 25);
                assert!(
                    !pushed.starts_with("/web/"),
                    "pushed URL must be navigable, got {pushed}"
                );
            }
        }
    }

    #[test]
    fn pushed_activity_urls_are_never_partial_endpoints() {
        for page in [1, 4] {
            for severity in [None, Some("error"), Some("all")] {
                let filter = ActivityFilter {
                    severity,
                    search: Some("q"),
                    source: Some("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
                };
                let pushed = activity_page_url(filter, page, 50);
                assert!(
                    !pushed.starts_with("/web/"),
                    "pushed URL must be navigable, got {pushed}"
                );
            }
        }
    }

    // ------------------------------------------------------------------
    // Activity URL construction
    // ------------------------------------------------------------------

    #[test]
    fn activity_url_omits_defaults() {
        assert_eq!(
            activity_page_url(ActivityFilter::default(), 1, 50),
            "/activity"
        );
    }

    #[test]
    fn activity_url_includes_non_default_params() {
        let filter = ActivityFilter {
            severity: Some("error"),
            ..ActivityFilter::default()
        };
        assert_eq!(
            activity_page_url(filter, 2, 100),
            "/activity?severity=error&page=2&per_page=100"
        );
    }

    #[test]
    fn activity_url_carries_search_and_source() {
        let filter = ActivityFilter {
            severity: Some("error"),
            search: Some("disk full"),
            source: Some("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        };
        assert_eq!(
            activity_page_url(filter, 1, 50),
            "/activity?severity=error&search=disk%20full&source=01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
    }

    #[test]
    fn activity_url_treats_all_severity_as_unfiltered() {
        let filter = ActivityFilter {
            severity: Some("all"),
            ..ActivityFilter::default()
        };
        assert_eq!(activity_page_url(filter, 1, 50), "/activity");
    }

    #[test]
    fn activity_url_search_cannot_inject_parameters() {
        let filter = ActivityFilter {
            search: Some("x&severity=error"),
            ..ActivityFilter::default()
        };
        let url = activity_page_url(filter, 1, 50);
        assert_eq!(url, "/activity?search=x%26severity%3Derror");
        assert!(!url.contains("&severity=error"));
    }

    // ------------------------------------------------------------------
    // Post-action redirect target preserves list state
    // ------------------------------------------------------------------

    #[test]
    fn downloads_return_url_preserves_page_and_filters() {
        let query = DownloadsQuery {
            status: Some("failed".to_owned()),
            search: Some("cats".to_owned()),
            page: Some(4),
            per_page: Some(50),
        };
        assert_eq!(
            downloads_return_url(&query),
            "/downloads?status=failed&search=cats&page=4&per_page=50"
        );
    }

    #[test]
    fn downloads_return_url_defaults_to_bare_path() {
        let query = DownloadsQuery {
            status: None,
            search: None,
            page: None,
            per_page: None,
        };
        assert_eq!(downloads_return_url(&query), "/downloads");
    }

    #[test]
    fn downloads_return_url_ignores_blank_search() {
        let query = DownloadsQuery {
            status: None,
            search: Some("   ".to_owned()),
            page: Some(2),
            per_page: None,
        };
        assert_eq!(downloads_return_url(&query), "/downloads?page=2");
    }

    // ------------------------------------------------------------------
    // Bulk action eligibility
    // ------------------------------------------------------------------

    #[test]
    fn bulk_action_parses_known_verbs() {
        assert_eq!(BulkAction::parse("retry"), Some(BulkAction::Retry));
        assert_eq!(BulkAction::parse("cancel"), Some(BulkAction::Cancel));
        assert_eq!(BulkAction::parse("delete"), Some(BulkAction::Delete));
    }

    #[test]
    fn bulk_action_rejects_unknown_verbs() {
        assert_eq!(BulkAction::parse("purge"), None);
        assert_eq!(BulkAction::parse(""), None);
        assert_eq!(BulkAction::parse("RETRY"), None);
    }

    /// These must stay in step with the guards in the single-item handlers,
    /// or a bulk action would accept work the individual button refuses.
    #[test]
    fn bulk_retry_matches_single_retry_eligibility() {
        for status in [
            VideoStatus::Failed,
            VideoStatus::PermanentlyFailed,
            VideoStatus::Cleaned,
        ] {
            assert!(BulkAction::Retry.allows(&status));
        }
        for status in [
            VideoStatus::Pending,
            VideoStatus::Downloading,
            VideoStatus::Completed,
            VideoStatus::Skipped,
        ] {
            assert!(!BulkAction::Retry.allows(&status));
        }
    }

    #[test]
    fn bulk_cancel_matches_single_cancel_eligibility() {
        for status in [VideoStatus::Pending, VideoStatus::Downloading] {
            assert!(BulkAction::Cancel.allows(&status));
        }
        for status in [
            VideoStatus::Completed,
            VideoStatus::Failed,
            VideoStatus::Cleaned,
            VideoStatus::PermanentlyFailed,
            VideoStatus::Skipped,
        ] {
            assert!(!BulkAction::Cancel.allows(&status));
        }
    }

    #[test]
    fn bulk_delete_only_accepts_completed() {
        assert!(BulkAction::Delete.allows(&VideoStatus::Completed));
        for status in [
            VideoStatus::Pending,
            VideoStatus::Downloading,
            VideoStatus::Failed,
            VideoStatus::Cleaned,
            VideoStatus::PermanentlyFailed,
            VideoStatus::Skipped,
        ] {
            assert!(!BulkAction::Delete.allows(&status));
        }
    }

    /// The slug is rendered into `data-status` and parsed back by the status
    /// filter links, so the two directions must agree exactly.
    #[test]
    fn download_status_slug_round_trips_through_parse() {
        for status in [
            VideoStatus::Pending,
            VideoStatus::Downloading,
            VideoStatus::Completed,
            VideoStatus::Failed,
            VideoStatus::Skipped,
            VideoStatus::Cleaned,
            VideoStatus::PermanentlyFailed,
        ] {
            let slug = download_status_slug(&status);
            assert_eq!(
                parse_download_status(Some(slug)),
                Some(status.clone()),
                "slug {slug} did not round-trip"
            );
        }
    }

    #[test]
    fn bulk_action_url_carries_list_state() {
        let url = downloads_url("/downloads/bulk", Some("failed"), Some("cats"), 3, 50);
        assert_eq!(
            url,
            "/downloads/bulk?status=failed&search=cats&page=3&per_page=50"
        );
    }

    // ------------------------------------------------------------------
    // Source search predicate (sources + schedule pages)
    // ------------------------------------------------------------------

    fn test_source(custom_name: Option<&str>, channel_title: Option<&str>, url: &str) -> Source {
        Source {
            id: Ulid::from_string("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid ulid literal"),
            profile_id: Ulid::from_string("01BX5ZZKBKACTAV9WEVGEMMVRZ")
                .expect("valid ulid literal"),
            url: url.to_owned(),
            source_type: hof_core::domain::source::SourceType::Channel,
            custom_name: custom_name.map(ToOwned::to_owned),
            enabled: true,
            index_frequency_secs: 43200,
            cutoff_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date literal"),
            retention_days: None,
            entry_order: hof_core::domain::source::EntryOrder::Unknown,
            entry_order_detected_at: None,
            last_indexed_at: None,
            last_error: None,
            index_error_count: 0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            channel_id: None,
            channel_title: channel_title.map(ToOwned::to_owned),
            channel_description: None,
            channel_thumbnail_url: None,
            jellyfin_metadata_at: None,
        }
    }

    #[test]
    fn source_search_matches_custom_name_case_insensitively() {
        let source = test_source(Some("My Cooking Channel"), None, "https://example.com/x");
        assert!(source_matches_search(&source, "cooking"));
        assert!(source_matches_search(&source, "COOKING"));
    }

    #[test]
    fn source_search_matches_channel_title() {
        let source = test_source(None, Some("Veritasium"), "https://example.com/x");
        assert!(source_matches_search(&source, "veritas"));
    }

    #[test]
    fn source_search_matches_url() {
        let source = test_source(None, None, "https://youtube.com/@somechannel");
        assert!(source_matches_search(&source, "somechannel"));
    }

    /// A custom name must not mask a channel-title match, and vice versa —
    /// which field is populated varies by source type and indexing state.
    #[test]
    fn source_search_checks_every_name_field() {
        let source = test_source(Some("Label"), Some("Channel"), "https://example.com/slug");
        assert!(source_matches_search(&source, "Label"));
        assert!(source_matches_search(&source, "Channel"));
        assert!(source_matches_search(&source, "slug"));
    }

    #[test]
    fn source_search_rejects_non_matches() {
        let source = test_source(Some("Cooking"), Some("Food"), "https://example.com/x");
        assert!(!source_matches_search(&source, "astronomy"));
    }

    #[test]
    fn downloads_return_url_clamps_non_positive_page() {
        let query = DownloadsQuery {
            status: None,
            search: None,
            page: Some(0),
            per_page: None,
        };
        assert_eq!(downloads_return_url(&query), "/downloads");
    }
}
