//! Split out of `pages/mod.rs`.

use super::*;

pub(super) async fn index() -> Redirect {
    Redirect::to("/dashboard")
}

pub(super) async fn login_page(session: Session, oidc_enabled: bool) -> impl IntoResponse {
    // If already logged in, redirect to dashboard
    if let Ok(Some(_)) = session.get::<String>("user_id").await {
        return Redirect::to("/dashboard").into_response();
    }

    auth_layout("Login", login_form(None, oidc_enabled)).into_response()
}

pub(super) async fn login(
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

pub(super) async fn register_page(session: Session) -> impl IntoResponse {
    // If already logged in, redirect to dashboard
    if let Ok(Some(_)) = session.get::<String>("user_id").await {
        return Redirect::to("/dashboard").into_response();
    }

    auth_layout("Register", register_form(None)).into_response()
}

pub(super) async fn register(
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

pub(super) async fn logout(State(_state): State<AppState>, session: Session) -> impl IntoResponse {
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

pub(super) fn login_form(error: Option<&str>, oidc_enabled: bool) -> Markup {
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

pub(super) fn register_form(error: Option<&str>) -> Markup {
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

pub(super) fn auth_layout(title: &str, content: impl Render) -> Markup {
    let heading = format!("{title} · Hofvarpnir");
    html! {
        (DOCTYPE)
        // `min-h-full`, not `h-full`: the body gradient is propagated to the
        // canvas but sized to the *root* box, so pinning html to height:100%
        // capped it at one viewport and Tailwind's default background-repeat
        // tiled it down the page — a hard slate-950/slate-900 seam every 100vh.
        html lang="en" class="min-h-full" {
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
