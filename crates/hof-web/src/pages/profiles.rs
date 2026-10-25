//! Split out of `pages/mod.rs`.

use super::*;

pub(super) async fn profiles_page(
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
                    (input_number(QUALITY_WAIT_LABEL, "quality_wait_hours", QUALITY_WAIT_PLACEHOLDER, false, ""))
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

pub(super) async fn create_profile(
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

    let quality_wait_hours = match parse_quality_wait_hours(form.quality_wait_hours.as_deref()) {
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
        quality_wait_hours,
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

pub(super) async fn update_profile(
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

    let quality_wait_hours = match parse_quality_wait_hours(form.quality_wait_hours.as_deref()) {
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
        quality_wait_hours: Some(quality_wait_hours),
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

pub(super) async fn delete_profile(
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

pub(super) fn profile_editor(profile: &Profile) -> Markup {
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
                (input_number(QUALITY_WAIT_LABEL, "quality_wait_hours", QUALITY_WAIT_PLACEHOLDER, false, &profile.quality_wait_hours.map_or_else(String::new, |hours| hours.to_string())))
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
