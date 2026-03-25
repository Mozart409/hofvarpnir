//! Maud page templates and htmx partial endpoints.

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Form, Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{
        IntoResponse, Redirect, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use chrono::{NaiveDate, Utc};
use futures::stream::{Stream, StreamExt};
use hof_api::AppState;
use hof_core::{
    actors::{
        cleanup::{GetCleanupStatus, RunCleanup},
        download_supervisor::{CancelDownload, EnqueueDownload},
        jellyfin_metadata::TriggerSourceMetadata,
        scheduler::IndexSource,
    },
    db::{self, CreateProfile, CreateSource, UpdateProfile, UpdateSource},
    domain::{
        activity::{ActivityEventType, ActivitySeverity},
        profile::{Profile, Quality},
        source::{Source, SourceType},
        system::IssueSeverity,
        video::{DownloadProgress, VideoStatus},
    },
    ytdlp::validate_output_template,
};
use maud::{DOCTYPE, Markup, PreEscaped, Render, html};
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;
use tower_sessions::Session;
use ulid::Ulid;

use crate::auth::AuthUser;

/// Static assets embedded at compile time.
#[derive(Embed)]
#[folder = "assets/"]
struct Assets;

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

#[derive(Debug, Deserialize)]
struct DownloadsQuery {
    status: Option<String>,
    search: Option<String>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        // Auth routes (no session required)
        .route("/login", get(login_page).post(login))
        .route("/register", get(register_page).post(register))
        .route("/logout", post(logout))
        // Protected routes
        .route("/", get(index))
        .route("/dashboard", get(dashboard_page))
        .route("/profiles", get(profiles_page).post(create_profile))
        .route("/profiles/{id}", post(update_profile))
        .route("/profiles/{id}/delete", post(delete_profile))
        .route("/sources", get(sources_page).post(create_source))
        .route("/sources/{id}", post(update_source))
        .route("/sources/{id}/delete", post(delete_source))
        .route("/sources/{id}/index", post(trigger_index))
        .route("/sources/{id}/metadata", post(trigger_metadata))
        .route("/downloads", get(downloads_page))
        .route("/downloads/{id}/retry", post(retry_download))
        .route("/downloads/{id}/cancel", post(cancel_download))
        .route("/downloads/{id}/delete", post(delete_download))
        .route("/web/downloads/progress", get(download_progress_sse))
        .route("/web/system-banner", get(system_banner))
        .route("/activity", get(activity_page))
        .route("/schedule", get(schedule_page))
        .route("/schedule/cleanup", post(trigger_cleanup))
        // Static assets (embedded at compile time)
        .route("/assets/{*path}", get(serve_asset))
        .with_state(state)
}

async fn index() -> Redirect {
    Redirect::to("/dashboard")
}

// ============================================================================
// Auth Pages
// ============================================================================

async fn login_page(session: Session) -> impl IntoResponse {
    // If already logged in, redirect to dashboard
    if let Ok(Some(_)) = session.get::<String>("user_id").await {
        return Redirect::to("/dashboard").into_response();
    }

    auth_layout("Login", login_form(None)).into_response()
}

async fn login(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<LoginForm>,
) -> impl IntoResponse {
    // Try to find user by email
    let Ok(user) = db::get_user_by_email(&state.pool, &form.email).await else {
        return auth_layout("Login", login_form(Some("Invalid email or password"))).into_response();
    };

    // Verify password
    if hof_core::auth::verify_password(&form.password, &user.password_hash).is_err() {
        return auth_layout("Login", login_form(Some("Invalid email or password"))).into_response();
    }

    // Create session
    if let Err(e) = AuthUser::login(&session, user.id).await {
        tracing::error!(error = ?e, "Failed to create session");
        return auth_layout("Login", login_form(Some("Failed to create session"))).into_response();
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
            password_hash: &password_hash,
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

async fn logout(session: Session) -> impl IntoResponse {
    let _ = AuthUser::logout(&session).await;
    Redirect::to("/login")
}

fn login_form(error: Option<&str>) -> Markup {
    html! {
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
                link rel="icon" href="https://fav.farm/🔥";
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

async fn dashboard_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;
    let (profiles_result, sources_result, videos_result) = tokio::join!(
        db::list_profiles(&state.pool),
        db::list_sources(&state.pool),
        db::list_videos(&state.pool, None)
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
            div class="grid gap-4 md:grid-cols-2 xl:grid-cols-4" {
                (metric_card("Profiles", profiles.len(), "Active download configurations"))
                (metric_card("Sources", sources.len(), "Channels and playlists being tracked"))
                (metric_card("Pending", pending, "Queued for download"))
                (metric_card("In Progress", downloading, "Currently downloading"))
            }
            div class="mt-4 grid gap-4 md:grid-cols-2" {
                (metric_card("Completed", completed, "Successfully archived videos"))
                (metric_card("Failed", failed, "Need retry or manual check"))
            }
            section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Recent Downloads" }
                @if recent.is_empty() {
                    p class="mt-3 text-sm text-slate-500 dark:text-slate-400" { "No downloads found yet." }
                } @else {
                    ul class="mt-4 space-y-3" {
                        @for video in recent {
                            li class="flex items-center justify-between gap-3 rounded-xl border border-slate-100 dark:border-slate-700 bg-slate-50/80 dark:bg-slate-800/80 px-3 py-2" {
                                div class="min-w-0" {
                                    p class="truncate text-sm font-medium text-slate-900 dark:text-slate-100" { (video.title) }
                                    p class="text-xs text-slate-500 dark:text-slate-400" { (video.platform) " / " (video.platform_video_id) }
                                }
                                (status_badge(&video.status))
                            }
                        }
                    }
                }
            }
        },
    );

    (StatusCode::OK, page)
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
                    (input_text("Name", "name", "Daily Archive", true, ""))
                    (input_text("Naming Template", "naming_template", "{{source_custom_name/or default}}/{{title}}.{{ext}}", true, "{{source_custom_name/or default}}/{{title}}.{{ext}}"))
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
        naming_template,
        output_dir: form.output_dir.trim(),
        include_livestreams: form.include_livestreams.is_some(),
        include_shorts: form.include_shorts.is_some(),
        storage_quota_bytes: form.storage_quota_gb * 1_000_000_000, // Convert GB to bytes
        retention_days,
    };

    match db::create_profile(&state.pool, create).await {
        Ok(profile) => {
            db::log_activity(
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
        naming_template: Some(naming_template),
        output_dir: Some(form.output_dir.trim()),
        include_livestreams: Some(form.include_livestreams.is_some()),
        include_shorts: Some(form.include_shorts.is_some()),
        storage_quota_bytes: Some(form.storage_quota_gb * 1_000_000_000), // Convert GB to bytes
        retention_days: Some(retention_days),
    };

    match db::update_profile(&state.pool, profile_id, update).await {
        Ok(_) => {
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
            db::log_activity(
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

async fn sources_page(
    auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;
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
    let sources = match db::list_sources(&state.pool).await {
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
                h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Existing Sources" }
                @if sources.is_empty() {
                    p class="mt-3 text-sm text-slate-500 dark:text-slate-400" { "No sources yet." }
                } @else {
                    div class="mt-4 space-y-4" {
                        @for source in &sources {
                            (source_editor(source))
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
            db::log_activity(
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
            db::log_activity(
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

async fn trigger_index(
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

    match state.scheduler.ask(IndexSource { source_id }).await {
        Ok(()) => {
            set_flash(&session, "info", "Indexing triggered").await;
            Redirect::to("/sources").into_response()
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

#[allow(clippy::too_many_lines)]
async fn downloads_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(query): axum::extract::Query<DownloadsQuery>,
) -> impl IntoResponse {
    let flash = take_flash(&session).await;

    let status_filter = query.status.as_deref().and_then(|s| match s {
        "pending" => Some(VideoStatus::Pending),
        "downloading" => Some(VideoStatus::Downloading),
        "completed" => Some(VideoStatus::Completed),
        "failed" => Some(VideoStatus::Failed),
        "skipped" => Some(VideoStatus::Skipped),
        "cleaned" => Some(VideoStatus::Cleaned),
        "permanently_failed" => Some(VideoStatus::PermanentlyFailed),
        _ => None,
    });

    let mut videos = match db::list_videos(&state.pool, status_filter).await {
        Ok(data) => data,
        Err(error) => {
            tracing::error!(%error, "failed to load downloads page");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                error_page("Failed to load downloads page"),
            );
        }
    };

    // In-memory title search
    if let Some(ref search) = query.search {
        let q = search.to_lowercase();
        videos.retain(|v| v.title.to_lowercase().contains(&q));
    }

    let current_status = query.status.as_deref().unwrap_or("all");

    let page = layout_with_flash(
        "Downloads",
        NavItem::Downloads,
        flash,
        html! {
            section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                div class="flex items-center justify-between" {
                    h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Live Progress" }
                    p class="text-xs text-slate-500 dark:text-slate-400" { "Streaming from /web/downloads/progress" }
                }
                div
                    class="mt-4 space-y-2"
                    id="download-progress-feed"
                    hx-ext="sse"
                    sse-connect="/web/downloads/progress"
                    sse-swap="message"
                    hx-swap="afterbegin"
                {
                    p class="rounded-lg border border-dashed border-slate-300 dark:border-slate-600 bg-slate-50 dark:bg-slate-800 px-3 py-2 text-sm text-slate-500 dark:text-slate-400" {
                        "Waiting for progress events..."
                    }
                }
            }

            // Filter & search bar
            section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                div class="flex flex-wrap items-center gap-4" {
                    nav class="flex flex-wrap gap-1" {
                        (download_status_filter_link("all", "All", current_status, query.search.as_deref()))
                        (download_status_filter_link("pending", "Pending", current_status, query.search.as_deref()))
                        (download_status_filter_link("downloading", "Downloading", current_status, query.search.as_deref()))
                        (download_status_filter_link("completed", "Completed", current_status, query.search.as_deref()))
                        (download_status_filter_link("failed", "Failed", current_status, query.search.as_deref()))
                        (download_status_filter_link("cleaned", "Cleaned", current_status, query.search.as_deref()))
                        (download_status_filter_link("permanently_failed", "Perm. Failed", current_status, query.search.as_deref()))
                        (download_status_filter_link("skipped", "Skipped", current_status, query.search.as_deref()))
                    }
                    form method="get" action="/downloads" class="flex gap-2" {
                        @if let Some(ref status) = query.status {
                            input type="hidden" name="status" value=(status);
                        }
                        input
                            type="text"
                            name="search"
                            placeholder="Search by title..."
                            value=(query.search.as_deref().unwrap_or(""))
                            class="rounded-lg border border-slate-300 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-900 dark:text-slate-100";
                        button type="submit"
                            class="rounded-lg bg-slate-900 px-3 py-1.5 text-sm font-medium text-white hover:bg-slate-700"
                        { "Search" }
                        @if query.search.is_some() || query.status.is_some() {
                            a href="/downloads"
                                class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                            { "Clear" }
                        }
                    }
                }
            }

            section class="mt-4 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" {
                    "Downloads"
                    span class="ml-2 text-sm font-normal text-slate-500 dark:text-slate-400" {
                        "(" (videos.len()) " results)"
                    }
                }
                @if videos.is_empty() {
                    p class="mt-3 text-sm text-slate-500 dark:text-slate-400" { "No downloads match your filters." }
                } @else {
                    div class="mt-4 overflow-x-auto" {
                        table class="min-w-full divide-y divide-slate-200 text-sm" {
                            thead class="bg-slate-50 dark:bg-slate-800" {
                                tr {
                                    th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Title" }
                                    th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Platform" }
                                    th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Status" }
                                    th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Attempts" }
                                    th class="px-3 py-2 text-left font-semibold text-slate-700 dark:text-slate-300" { "Actions" }
                                }
                            }
                            tbody class="divide-y divide-slate-100 dark:divide-slate-700 bg-white dark:bg-slate-900" {
                                @for video in &videos {
                                    tr {
                                        td class="max-w-lg px-3 py-2 text-slate-900 dark:text-slate-100" {
                                            p class="truncate font-medium" { (video.title) }
                                            p class="truncate text-xs text-slate-500 dark:text-slate-400" { (video.id.to_string()) }
                                        }
                                        td class="px-3 py-2 text-slate-600 dark:text-slate-400" { (video.platform) }
                                        td class="px-3 py-2" { (status_badge(&video.status)) }
                                        td class="px-3 py-2 text-slate-600 dark:text-slate-400" { (video.attempts) }
                                        td class="px-3 py-2" {
                                            div class="flex gap-2" {
                                                @if matches!(video.status, VideoStatus::Failed | VideoStatus::PermanentlyFailed | VideoStatus::Cleaned) {
                                                    form method="post" action=(format!("/downloads/{}/retry", video.id)) {
                                                        button class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-xs font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900" type="submit" {
                                                            "Retry"
                                                        }
                                                    }
                                                }
                                                @if matches!(video.status, VideoStatus::Pending | VideoStatus::Downloading) {
                                                    form method="post" action=(format!("/downloads/{}/cancel", video.id)) {
                                                        button
                                                            class="rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/50 px-3 py-1.5 text-xs font-medium text-amber-700 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900"
                                                            type="submit"
                                                            onclick="return confirm('Cancel this download?')"
                                                        {
                                                            "Cancel"
                                                        }
                                                    }
                                                }
                                                @if video.status == VideoStatus::Completed {
                                                    form method="post" action=(format!("/downloads/{}/delete", video.id)) {
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
                        }
                    }
                }
            }
        },
    );

    (StatusCode::OK, page)
}

#[allow(clippy::too_many_lines)]
async fn retry_download(
    _auth: AuthUser,
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
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
            set_flash(&session, "info", "Download re-queued").await;
            Redirect::to("/downloads").into_response()
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
            set_flash(&session, "info", "Download cancelled").await;
            Redirect::to("/downloads").into_response()
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
    db::log_activity(
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
    Redirect::to("/downloads").into_response()
}

async fn download_progress_sse(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.progress_tx.subscribe();

    let stream = BroadcastStream::new(receiver).filter_map(|result| async move {
        match result {
            Ok(progress) => {
                let fragment = progress_fragment(&progress).into_string();
                Some(Ok(Event::default().event("message").data(fragment)))
            }
            Err(error) => {
                tracing::debug!(%error, "dropped lagged SSE progress event");
                None
            }
        }
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
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

#[derive(Debug, Deserialize)]
struct ActivityQuery {
    severity: Option<String>,
    page: Option<i64>,
}

async fn activity_page(
    _auth: AuthUser,
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<ActivityQuery>,
) -> impl IntoResponse {
    let page_num = query.page.unwrap_or(1).max(1);
    let per_page: i64 = 50;
    let offset = (page_num - 1) * per_page;

    let severity_filter = query.severity.as_deref().and_then(|s| match s {
        "info" => Some(ActivitySeverity::Info),
        "success" => Some(ActivitySeverity::Success),
        "warning" => Some(ActivitySeverity::Warning),
        "error" => Some(ActivitySeverity::Error),
        _ => None,
    });

    let (events_result, count_result) = tokio::join!(
        db::list_activity_events(&state.pool, per_page, offset, severity_filter.clone(), None),
        db::count_activity_events(&state.pool, severity_filter.clone(), None)
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

    let total = count_result.unwrap_or(0);
    let total_pages = (total + per_page - 1) / per_page;

    let current_severity = query.severity.as_deref().unwrap_or("all");

    let page = layout(
        "Activity",
        NavItem::Activity,
        html! {
            section class="rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                div class="flex flex-wrap items-center justify-between gap-3" {
                    h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Activity Log" }
                    nav class="flex gap-1" {
                        (severity_filter_link("all", "All", current_severity))
                        (severity_filter_link("info", "Info", current_severity))
                        (severity_filter_link("success", "Success", current_severity))
                        (severity_filter_link("warning", "Warning", current_severity))
                        (severity_filter_link("error", "Error", current_severity))
                    }
                }

                @if events.is_empty() {
                    p class="mt-4 rounded-lg border border-dashed border-slate-300 dark:border-slate-600 bg-slate-50 dark:bg-slate-800 px-4 py-8 text-center text-sm text-slate-500 dark:text-slate-400" {
                        "No activity events recorded yet."
                    }
                } @else {
                    div class="mt-4 space-y-2" {
                        @for event in &events {
                            (activity_event_row(event))
                        }
                    }

                    // Pagination
                    @if total_pages > 1 {
                        nav class="mt-6 flex items-center justify-center gap-2" {
                            @if page_num > 1 {
                                a
                                    class="rounded-lg border border-slate-200 dark:border-slate-600 bg-white dark:bg-slate-700 px-3 py-1.5 text-sm text-slate-700 dark:text-slate-300 hover:bg-slate-50 dark:hover:bg-slate-600"
                                    href=(format!("/activity?severity={}&page={}", current_severity, page_num - 1))
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
                                    href=(format!("/activity?severity={}&page={}", current_severity, page_num + 1))
                                {
                                    "Next"
                                }
                            }
                        }
                    }
                }
            }
        },
    );

    (StatusCode::OK, page)
}

fn download_status_filter_link(
    value: &str,
    label: &str,
    current: &str,
    search: Option<&str>,
) -> Markup {
    let active = value == current;
    let classes = if active {
        "rounded-full bg-slate-900 dark:bg-slate-100 px-3 py-1 text-xs font-medium text-white dark:text-slate-900"
    } else {
        "rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
    };
    let mut href = if value == "all" {
        "/downloads".to_string()
    } else {
        format!("/downloads?status={value}")
    };
    if let Some(q) = search.filter(|q| !q.is_empty()) {
        let sep = if href.contains('?') { '&' } else { '?' };
        href.push(sep);
        href.push_str("search=");
        href.push_str(q);
    }

    html! {
        a class=(classes) href=(href) { (label) }
    }
}

fn severity_filter_link(value: &str, label: &str, current: &str) -> Markup {
    let active = value == current;
    let classes = if active {
        "rounded-full bg-slate-900 dark:bg-slate-100 px-3 py-1 text-xs font-medium text-white dark:text-slate-900"
    } else {
        "rounded-full bg-slate-100 dark:bg-slate-700 px-3 py-1 text-xs font-medium text-slate-600 dark:text-slate-300 hover:bg-slate-200 dark:hover:bg-slate-600"
    };
    let href = if value == "all" {
        "/activity".to_string()
    } else {
        format!("/activity?severity={value}")
    };

    html! {
        a class=(classes) href=(href) { (label) }
    }
}

fn activity_event_row(event: &hof_core::domain::activity::ActivityEvent) -> Markup {
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
        ActivityEventType::SourceDeleted => "Source Deleted",
    };

    let time_ago = format_time_ago(event.created_at);

    html! {
        div class=(format!("flex items-start gap-3 rounded-lg border border-slate-200 dark:border-slate-700 border-l-4 {} bg-white dark:bg-slate-800 p-3", border_color)) {
            span class="mt-0.5 flex h-6 w-6 items-center justify-center rounded-full bg-slate-100 dark:bg-slate-700 text-xs font-bold text-slate-600 dark:text-slate-300" {
                (icon)
            }
            div class="min-w-0 flex-1" {
                div class="flex flex-wrap items-center gap-2" {
                    span class=(format!("inline-flex rounded-full px-2 py-0.5 text-xs font-medium {}", severity_badge.1)) {
                        (severity_badge.0)
                    }
                    span class="text-xs font-medium text-slate-500 dark:text-slate-400" { (event_label) }
                    span class="text-xs text-slate-400 dark:text-slate-500" title=(event.created_at.to_rfc3339()) { (time_ago) }
                }
                p class="mt-1 text-sm text-slate-700 dark:text-slate-300" { (event.message) }
            }
        }
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
) -> impl IntoResponse {
    let flash = take_flash(&session).await;
    let (sources_result, profiles_result, recent_activity_result, cleanup_status) = tokio::join!(
        db::list_sources(&state.pool),
        db::list_profiles(&state.pool),
        db::list_activity_events(&state.pool, 20, 0, None, None),
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

    let now = Utc::now();

    // Build schedule entries
    let mut entries: Vec<ScheduleEntry> = sources
        .into_iter()
        .map(|source| {
            let profile_name = profiles
                .iter()
                .find(|p| p.id == source.profile_id)
                .map_or_else(|| "Unknown".to_string(), |p| p.name.clone());

            let next_index_at = source
                .last_indexed_at
                .map(|last| last + chrono::Duration::seconds(source.index_frequency_secs));

            let is_overdue =
                next_index_at.is_some_and(|next| next < now) && source.last_error.is_some();

            ScheduleEntry {
                source,
                profile_name,
                next_index_at,
                is_overdue,
            }
        })
        .collect();

    // Sort: overdue first, then by next index time (soonest first)
    entries.sort_by(|a, b| {
        b.is_overdue
            .cmp(&a.is_overdue)
            .then_with(|| a.next_index_at.cmp(&b.next_index_at))
    });

    let page = layout_with_flash(
        "Schedule",
        NavItem::Schedule,
        flash,
        html! {
            (cleanup_status_section(cleanup_status.ok().as_ref(), now))

            section class="mt-8 rounded-2xl border border-slate-200 dark:border-slate-700 bg-white/80 dark:bg-slate-800/80 p-6 shadow-sm" {
                h2 class="text-lg font-semibold text-slate-900 dark:text-slate-100" { "Upcoming Indexing" }
                @if entries.is_empty() {
                    p class="mt-4 rounded-lg border border-dashed border-slate-300 dark:border-slate-600 bg-slate-50 dark:bg-slate-800 px-4 py-8 text-center text-sm text-slate-500 dark:text-slate-400" {
                        "No sources configured yet. Add sources to start scheduling."
                    }
                } @else {
                    div class="mt-4 space-y-2" {
                        @for entry in &entries {
                            (schedule_entry_row(entry, now))
                        }
                    }
                }
            }

            (recent_runs_section(&recent_runs))
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

fn recent_runs_section(recent_runs: &[&hof_core::domain::activity::ActivityEvent]) -> Markup {
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
                                    td class="px-3 py-2 text-slate-700 dark:text-slate-300" {
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

    let (status_text, status_classes) = if entry.is_overdue {
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

    let border = if entry.is_overdue {
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
                form method="post" action=(format!("/sources/{}/index", entry.source.id)) {
                    button class="rounded-lg border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-1.5 text-xs font-medium text-sky-700 dark:text-sky-300 hover:bg-sky-100 dark:hover:bg-sky-900" type="submit" {
                        "Index Now"
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
    // Determine border color based on error state
    let border_class = if source.last_error.is_some() {
        "border-rose-300 dark:border-rose-700 bg-rose-50/60 dark:bg-rose-900/30"
    } else {
        "border-slate-200 dark:border-slate-700 bg-slate-50/60 dark:bg-slate-800/60"
    };

    html! {
        details class=(format!("group rounded-xl border {} p-4 open:bg-white dark:open:bg-slate-800", border_class)) {
            summary class="cursor-pointer list-none" {
                div class="flex flex-wrap items-center justify-between gap-3" {
                    div class="min-w-0" {
                        p class="truncate text-sm font-semibold text-slate-900 dark:text-slate-100" {
                            (source.custom_name.as_deref().unwrap_or(&source.url))
                        }
                        p class="truncate text-xs text-slate-500 dark:text-slate-400" { (source.id.to_string()) }
                    }
                    div class="flex items-center gap-2" {
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
                    button class="rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/50 px-4 py-2 text-sm font-medium text-amber-700 dark:text-amber-300 hover:bg-amber-100 dark:hover:bg-amber-900" type="submit" formaction={(format!("/sources/{}/index", source.id))} {
                        "Trigger Index"
                    }
                    button class="rounded-lg border border-emerald-200 dark:border-emerald-800 bg-emerald-50 dark:bg-emerald-900/50 px-4 py-2 text-sm font-medium text-emerald-700 dark:text-emerald-300 hover:bg-emerald-100 dark:hover:bg-emerald-900" type="submit" formaction={(format!("/sources/{}/metadata", source.id))} {
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

fn progress_fragment(progress: &DownloadProgress) -> Markup {
    let percentage = format!("{:.2}", progress.percent);
    let speed = progress.speed.as_deref().unwrap_or("n/a");
    let eta = progress.eta.as_deref().unwrap_or("n/a");
    let now = Utc::now().format("%H:%M:%S").to_string();

    html! {
        article class="rounded-xl border border-sky-200 dark:border-sky-800 bg-sky-50 dark:bg-sky-900/50 px-3 py-2 text-sm text-sky-900 dark:text-sky-100" {
            div class="flex flex-wrap items-center justify-between gap-2" {
                p class="font-medium" {
                    (progress.platform_video_id.clone()) " · " (percentage) "%"
                }
                span class="text-xs text-sky-700" { (now) }
            }
            div class="mt-1 text-xs text-sky-800" {
                "Speed: " (speed) " · ETA: " (eta)
            }
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

const fn source_type_label(source_type: &SourceType) -> &'static str {
    match source_type {
        SourceType::Channel => "Channel",
        SourceType::Playlist => "Playlist",
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
                p class="mt-2 text-sm text-rose-800 dark:text-rose-200" { (message) }
                div class="mt-4" {
                    a class="inline-flex rounded-lg bg-rose-700 dark:bg-rose-600 px-4 py-2 text-sm font-medium text-white hover:bg-rose-800 dark:hover:bg-rose-700" href="/dashboard" {
                        "Back to dashboard"
                    }
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
                link rel="icon" href="https://fav.farm/🔥";
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
                            div {
                                p class="text-xs uppercase tracking-[0.2em] text-slate-500 dark:text-slate-400" { "Hofvarpnir" }
                                h1 class="text-2xl font-semibold text-slate-900 dark:text-slate-100" { (title) }
                            }
                            nav class="flex flex-wrap items-center gap-2" {
                                (nav_link("/dashboard", "Dashboard", active == NavItem::Dashboard))
                                (nav_link("/profiles", "Profiles", active == NavItem::Profiles))
                                (nav_link("/sources", "Sources", active == NavItem::Sources))
                                (nav_link("/downloads", "Downloads", active == NavItem::Downloads))
                                (nav_link("/activity", "Activity", active == NavItem::Activity))
                                (nav_link("/schedule", "Schedule", active == NavItem::Schedule))
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

                    // SSE placeholder removal
                    document.body.addEventListener('htmx:sseMessage', function (event) {
                      var feed = document.getElementById('download-progress-feed');
                      if (!feed) return;
                      var placeholder = feed.querySelector('p');
                      if (placeholder) placeholder.remove();
                    });

                    // Loading state on form submit
                    document.addEventListener('submit', function(e) {
                      var form = e.target;
                      if (form.tagName !== 'FORM') return;
                      var btn = e.submitter;
                      if (!btn || btn.disabled) return;
                      btn.disabled = true;
                      btn.dataset.originalText = btn.textContent;
                      btn.textContent = 'Working\u2026';
                    });
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
