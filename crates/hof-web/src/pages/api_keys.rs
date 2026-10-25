//! Split out of `pages/mod.rs`.

use super::*;

/// Session key for storing newly created API key token (shown once).
pub(super) const NEW_API_KEY_TOKEN: &str = "new_api_key_token";

pub(super) async fn api_keys_page(
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

pub(super) fn api_keys_content(api_keys: &[ApiKey], new_token: Option<&str>) -> Markup {
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

pub(super) fn api_key_card(key: &ApiKey) -> Markup {
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

pub(super) fn scope_badge(scope: ApiKeyScope) -> Markup {
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
pub(super) fn parse_expires_in(token: &str) -> Result<Option<chrono::Duration>, ()> {
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

pub(super) async fn create_api_key(
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
        }
    }
    Redirect::to("/settings/api-keys").into_response()
}

pub(super) async fn roll_api_key(
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
        }
        Err(error) => {
            tracing::error!(%error, "failed to roll API key");
            set_flash(&session, "error", "Failed to roll API key").await;
        }
    }
    Redirect::to("/settings/api-keys").into_response()
}

pub(super) async fn delete_api_key(
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
        }
        Err(db::DbError::NotFound) => {
            set_flash(&session, "error", "API key not found").await;
        }
        Err(error) => {
            tracing::error!(%error, "failed to delete API key");
            set_flash(&session, "error", "Failed to delete API key").await;
        }
    }
    Redirect::to("/settings/api-keys").into_response()
}

pub(super) async fn api_key_events(
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
