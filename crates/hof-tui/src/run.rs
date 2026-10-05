//! The TUI event loop: terminal setup/teardown, fetch orchestration, and
//! dispatch of keyboard, timer, action-result and SSE messages.
//!
//! Three message sources feed one `select!` loop:
//! - crossterm key events (via `EventStream`),
//! - `LoopMsg` (refresh timer, fetch results, action results),
//! - `ProgressMsg` from the reconnecting SSE task (see [`crate::client`]).

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Context, Result, eyre};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyEventKind,
};
use futures::StreamExt;
use reqwest::StatusCode;
use tokio::sync::mpsc;

use crate::app::{Action, App};
use crate::client::{ApiClient, ApiClientError, ProgressMsg};
use crate::config::{self, Config, ConfigError, PartialConfig};
use crate::setup::{self, Setup, SetupOutcome};
use crate::types::{
    ActivityListResponse, ProfileResponse, SettingsResponse, SourceResponse, SystemStatusResponse,
    VideoResponse, WhoAmIResponse,
};
use crate::ui;

/// How often the current tab's data is refetched. SSE covers live progress;
/// this only picks up state changes (new rows, status flips, stats).
const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

/// Upper bound for the polling interval while the server is unreachable.
/// Matches the SSE reconnect cap in [`crate::client`].
const REFRESH_INTERVAL_MAX: Duration = Duration::from_mins(5);

/// Polling interval after `failures` consecutive outage-type failures:
/// [`REFRESH_INTERVAL`] doubled per failure, capped at
/// [`REFRESH_INTERVAL_MAX`].
fn poll_delay(failures: u32) -> Duration {
    REFRESH_INTERVAL
        .saturating_mul(2_u32.saturating_pow(failures))
        .min(REFRESH_INTERVAL_MAX)
}

/// Show a failed data fetch: an outage stays until a refresh succeeds,
/// anything else (e.g. a 403) is ordinary feedback that fades.
fn report_fetch_error(app: &mut App, error: &ApiClientError) {
    if is_outage(error) {
        app.set_sticky_error(error.to_string());
    } else {
        app.set_message(error.to_string(), true);
    }
}

/// Whether an error means the server is down or unreachable (back off),
/// as opposed to a request it answered and refused (keep polling normally).
fn is_outage(error: &ApiClientError) -> bool {
    match error {
        ApiClientError::Transport(_) => true,
        ApiClientError::Api { status, .. } => status.is_server_error(),
        ApiClientError::InvalidToken(_) => false,
    }
}

/// How many activity events to fetch per refresh.
const ACTIVITY_LIMIT: i64 = 100;

/// Messages from background tasks back to the event loop.
#[derive(Debug)]
enum LoopMsg {
    Status(Result<SystemStatusResponse, ApiClientError>),
    Downloads(Result<Vec<VideoResponse>, ApiClientError>),
    Sources(Result<Vec<SourceResponse>, ApiClientError>),
    Profiles(Result<Vec<ProfileResponse>, ApiClientError>),
    Activity(Result<ActivityListResponse, ApiClientError>),
    Settings(Result<SettingsResponse, ApiClientError>),
    /// An action finished; the string is a short human-readable confirmation.
    Action(Result<String, ApiClientError>),
}

/// Outcome of running the app.
#[derive(Debug)]
pub enum Exit {
    /// Normal exit (`q` / Ctrl-C).
    Quit,
    /// `--help` was requested; the payload is the usage text.
    Help(String),
}

/// Errors that end the program before or after the UI starts.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Other(#[from] color_eyre::Report),
}

/// A verified connection: the client plus what the key may do.
struct Session {
    client: ApiClient,
    /// `None` when the server predates `GET /api/v1/system/whoami`.
    access: Option<WhoAmIResponse>,
    /// Status line to show once the main UI is up, `(text, is_error)`.
    notice: Option<(String, bool)>,
}

/// Upper bound for one connection attempt from the setup screen, so an
/// unroutable host does not freeze the form for a TCP timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Entry point used by `main`: parse config, verify the token, run the loop.
///
/// With no token from `--token` / `HOF_API_TOKEN` / the config file, a setup
/// screen asks for URL and token before the main UI starts.
///
/// # Returns
///
/// `Ok(Exit::Quit)` on a clean quit, `Ok(Exit::Help)` when usage was printed.
///
/// # Errors
///
/// `RunError` for bad configuration, an unreachable server, an
/// invalid/expired token, or terminal/IO failures.
pub async fn run(args: Vec<String>) -> Result<Exit, RunError> {
    let partial = match PartialConfig::from_args_and_env(args) {
        Ok(partial) => partial,
        Err(ConfigError::Help(usage)) => return Ok(Exit::Help(usage)),
        Err(e) => return Err(RunError::Config(e)),
    };

    // A token given up front keeps the fail-fast path: report problems on
    // stderr before taking over the terminal.
    let preconnected = match &partial.token {
        Some(token) => {
            let config = Config::new(&partial.api_url, token)?;
            Some(connect(&config).await.map_err(RunError::Other)?)
        }
        None => None,
    };

    color_eyre::install().map_err(RunError::Other)?;
    install_panic_hook();
    let mut terminal = ratatui::try_init()
        .map_err(|e| RunError::Other(eyre!("failed to initialize terminal: {e}")))?;

    let result = async {
        let session = match preconnected {
            Some(session) => session,
            None => match run_setup(&mut terminal, partial.api_url, partial.save_path).await? {
                Some(session) => session,
                None => return Ok(()),
            },
        };
        run_loop(&mut terminal, session).await
    }
    .await;
    ratatui::restore();
    result.map_err(RunError::Other)?;
    Ok(Exit::Quit)
}

/// Verify the server is reachable and the token can read, then look up the
/// key's scopes.
///
/// # Errors
///
/// A readable report for an unreachable server, a rejected token, or a key
/// without the `read` scope.
async fn connect(config: &Config) -> Result<Session> {
    let client = ApiClient::new(&config.api_url, &config.token).map_err(|e| eyre!(e))?;

    client.system_status().await.map_err(|e| match &e {
        ApiClientError::Api { status, .. } if *status == StatusCode::UNAUTHORIZED => eyre!(
            "authentication failed against {}: {e}\n\
             hint: create an API key in the web UI (Settings) and pass it via --token or HOF_API_TOKEN",
            config.api_url
        ),
        ApiClientError::Api { status, .. } if *status == StatusCode::FORBIDDEN => {
            eyre!("the API key lacks the `read` scope: {e}")
        }
        _ => eyre!(
            "cannot reach the Hofvarpnir server at {}: {e}",
            config.api_url
        ),
    })?;

    // Scopes are informational; an older server without the endpoint (404)
    // or any other hiccup must not block an otherwise working session.
    let access = match client.whoami().await {
        Ok(access) => Some(access),
        Err(e) => {
            tracing::debug!(error = %e, "whoami unavailable; scopes unknown");
            None
        }
    };

    Ok(Session {
        client,
        access,
        notice: None,
    })
}

/// Run the setup form until a connection succeeds (`Some`) or the user quits
/// (`None`).
async fn run_setup(
    terminal: &mut ratatui::DefaultTerminal,
    api_url: String,
    save_path: Option<PathBuf>,
) -> Result<Option<Session>> {
    // Bracketed paste delivers a pasted token as one `Event::Paste` instead
    // of a burst of key presses (where a trailing newline would submit).
    crossterm::execute!(std::io::stdout(), EnableBracketedPaste)
        .context("failed to enable bracketed paste")?;
    let result = setup_loop(terminal, Setup::new(api_url, save_path)).await;
    drop(crossterm::execute!(
        std::io::stdout(),
        DisableBracketedPaste
    ));
    result
}

async fn setup_loop(
    terminal: &mut ratatui::DefaultTerminal,
    mut setup: Setup,
) -> Result<Option<Session>> {
    let mut events = EventStream::new();
    loop {
        terminal
            .draw(|frame| setup::draw(frame, &setup))
            .context("failed to draw frame")?;

        let outcome = match events.next().await {
            Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => setup.handle_key(key),
            Some(Ok(Event::Paste(text))) => {
                setup.handle_paste(&text);
                None
            }
            Some(Ok(_)) => None,
            Some(Err(e)) => return Err(eyre!("failed to read terminal events: {e}")),
            None => return Ok(None),
        };

        match outcome {
            Some(SetupOutcome::Quit) => return Ok(None),
            Some(SetupOutcome::Submit) => {
                let config = match Config::new(&setup.url, &setup.token) {
                    Ok(config) => config,
                    Err(e) => {
                        setup.error = Some(e.to_string());
                        continue;
                    }
                };
                setup.url.clone_from(&config.api_url);
                setup.error = None;
                setup.connecting = true;
                terminal
                    .draw(|frame| setup::draw(frame, &setup))
                    .context("failed to draw frame")?;

                let attempt = tokio::time::timeout(CONNECT_TIMEOUT, connect(&config)).await;
                setup.connecting = false;
                match attempt {
                    Ok(Ok(mut session)) => {
                        if setup.save
                            && let Some(path) = &setup.save_path
                        {
                            // A failed save (e.g. a read-only ~/.config) must
                            // not cost the user a working connection.
                            session.notice = Some(match config::save(path, &config) {
                                Ok(()) => (format!("saved to {}", path.display()), false),
                                Err(e) => (format!("could not save {}: {e}", path.display()), true),
                            });
                        }
                        return Ok(Some(session));
                    }
                    Ok(Err(e)) => setup.error = Some(e.to_string()),
                    Err(_) => {
                        setup.error = Some(format!(
                            "timed out after {}s connecting to {}",
                            CONNECT_TIMEOUT.as_secs(),
                            config.api_url
                        ));
                    }
                }
            }
            None => {}
        }
    }
}

/// Restore the terminal before the default panic printer runs, so a panic
/// does not leave the user's shell inside the alternate screen.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        original(info);
    }));
}

/// Execute an [`Action`] against the API. Returns a short confirmation line
/// for the status-message area.
pub async fn execute_action(
    client: &ApiClient,
    action: &Action,
    currently_paused: bool,
) -> Result<String, ApiClientError> {
    fn or_else(msg: String, fallback: &str) -> String {
        if msg.is_empty() {
            fallback.to_string()
        } else {
            msg
        }
    }

    match action {
        Action::RetryDownload(id) => client
            .retry_download(id)
            .await
            .map(|m| or_else(m, "retry enqueued")),
        Action::CancelDownload(id) => client
            .cancel_download(id)
            .await
            .map(|m| or_else(m, "cancel requested")),
        Action::DeleteDownload(id) => client
            .delete_download(id)
            .await
            .map(|m| or_else(m, "video deleted")),
        Action::TriggerIndex(id) => client
            .trigger_index(id)
            .await
            .map(|m| or_else(m, "index triggered")),
        Action::DeleteSource(id) => client
            .delete_source(id)
            .await
            .map(|m| or_else(m, "source deleted")),
        Action::DeleteProfile(id) => client
            .delete_profile(id)
            .await
            .map(|m| or_else(m, "profile deleted")),
        Action::TogglePause => {
            if currently_paused {
                client.resume_all().await.map(|_| "resumed".to_string())
            } else {
                client.pause_all().await.map(|_| "paused".to_string())
            }
        }
        Action::SetPause(module, true) => client
            .pause(*module)
            .await
            .map(|_| format!("{} paused", module.as_str())),
        Action::SetPause(module, false) => client
            .resume(*module)
            .await
            .map(|_| format!("{} resumed", module.as_str())),
        Action::UpdateSetting(knob, Some(value)) => client
            .patch_setting(knob.key(), Some(*value))
            .await
            .map(|_| format!("{} set to {}", knob.label(), knob.format(*value))),
        Action::UpdateSetting(knob, None) => {
            client.patch_setting(knob.key(), None).await.map(|s| {
                let resolved = knob.get(&s);
                format!(
                    "{} reset to {} ({})",
                    knob.label(),
                    knob.format(resolved.value),
                    resolved.provenance.label()
                )
            })
        }
        Action::Refresh => Ok(String::new()),
    }
}

/// Spawn fetches for everything the current tab shows. `in_flight` dedupes
/// per resource so a slow server cannot cause request pile-up.
fn spawn_refresh(
    client: &ApiClient,
    tx: &mpsc::Sender<LoopMsg>,
    app: &App,
    in_flight: &mut HashSet<&'static str>,
) {
    if in_flight.insert("status") {
        let (client, tx) = (client.clone(), tx.clone());
        tokio::spawn(async move {
            drop(tx.send(LoopMsg::Status(client.system_status().await)).await);
        });
    }

    match app.tab {
        crate::app::Tab::Downloads => {
            if in_flight.insert("downloads") {
                let (client, tx) = (client.clone(), tx.clone());
                let filter = app.status_filter;
                tokio::spawn(async move {
                    drop(
                        tx.send(LoopMsg::Downloads(client.list_downloads(filter).await))
                            .await,
                    );
                });
            }
        }
        crate::app::Tab::Sources => {
            // The sources table joins profile names, so both are fetched.
            if in_flight.insert("sources") {
                let (client, tx) = (client.clone(), tx.clone());
                tokio::spawn(async move {
                    drop(tx.send(LoopMsg::Sources(client.list_sources().await)).await);
                });
            }
            if in_flight.insert("profiles") {
                let (client, tx) = (client.clone(), tx.clone());
                tokio::spawn(async move {
                    drop(
                        tx.send(LoopMsg::Profiles(client.list_profiles().await))
                            .await,
                    );
                });
            }
        }
        crate::app::Tab::Profiles => {
            if in_flight.insert("profiles") {
                let (client, tx) = (client.clone(), tx.clone());
                tokio::spawn(async move {
                    drop(
                        tx.send(LoopMsg::Profiles(client.list_profiles().await))
                            .await,
                    );
                });
            }
        }
        crate::app::Tab::Settings => {
            if in_flight.insert("settings") {
                let (client, tx) = (client.clone(), tx.clone());
                tokio::spawn(async move {
                    drop(
                        tx.send(LoopMsg::Settings(client.get_settings().await))
                            .await,
                    );
                });
            }
        }
        crate::app::Tab::Activity => {
            if in_flight.insert("activity") {
                let (client, tx) = (client.clone(), tx.clone());
                tokio::spawn(async move {
                    drop(
                        tx.send(LoopMsg::Activity(
                            client.list_activity(ACTIVITY_LIMIT).await,
                        ))
                        .await,
                    );
                });
            }
        }
    }
}

/// The main draw/select loop. Owns all mutable state.
async fn run_loop(terminal: &mut ratatui::DefaultTerminal, session: Session) -> Result<()> {
    let Session {
        client,
        access,
        notice,
    } = session;
    let mut app = App {
        access,
        ..App::default()
    };
    if let Some((text, is_error)) = notice {
        app.set_message(text, is_error);
    }
    let mut in_flight: HashSet<&'static str> = HashSet::new();

    let (loop_tx, mut loop_rx) = mpsc::channel::<LoopMsg>(64);
    let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressMsg>(256);

    // Reconnecting SSE progress stream (stopped by dropping the channel).
    tokio::spawn({
        let client = client.clone();
        async move { client.run_progress_stream(progress_tx).await }
    });

    // Initial data load before first draw. Later refreshes run off
    // `app.next_poll_at`, which the status result pushes out with backoff.
    spawn_refresh(&client, &loop_tx, &app, &mut in_flight);
    app.next_poll_at = Instant::now().checked_add(REFRESH_INTERVAL);

    let mut events = EventStream::new();
    // Redraw at least once a second so the retry countdowns keep moving
    // during a long backoff, when nothing else wakes the loop.
    let mut redraw = tokio::time::interval(Duration::from_secs(1));
    redraw.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        app.expire_message(Instant::now());
        terminal
            .draw(|frame| ui::draw(frame, &mut app))
            .context("failed to draw frame")?;

        let next_poll = app.next_poll_at.unwrap_or_else(Instant::now);
        tokio::select! {
            _ = redraw.tick() => {}
            () = tokio::time::sleep_until(next_poll.into()) => {
                spawn_refresh(&client, &loop_tx, &app, &mut in_flight);
                // Provisional; the status result reschedules with backoff.
                app.next_poll_at = Instant::now().checked_add(poll_delay(app.poll_failures));
            }
            maybe_event = events.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event
                    && key.kind == KeyEventKind::Press
                    && let Some(action) = app.handle_key(key)
                {
                    dispatch_action(&client, &loop_tx, &mut app, action, &mut in_flight);
                }
                // (Resize and paste events need no handling: we redraw every
                // loop iteration anyway.)
            }
            maybe_msg = loop_rx.recv() => {
                let Some(msg) = maybe_msg else { break };
                handle_loop_msg(&client, &loop_tx, &mut app, &mut in_flight, msg);
            }
            maybe_progress = progress_rx.recv() => {
                match maybe_progress {
                    Some(ProgressMsg::Progress(event)) => app.apply_progress(event),
                    Some(ProgressMsg::Connected(connected)) => {
                        // `sse_retry_at` is only set after a drop, so this is
                        // a reconnect, not the first connect.
                        if connected && app.sse_retry_at.is_some() {
                            // The server is back: stop waiting out the
                            // polling backoff and refresh now, which also
                            // clears a stale connection error on success.
                            app.poll_failures = 0;
                            app.next_poll_at = Some(Instant::now());
                        }
                        app.sse_connected = connected;
                        if connected {
                            app.sse_retry_at = None;
                        }
                    }
                    Some(ProgressMsg::Retrying(delay)) => {
                        app.sse_retry_at = Instant::now().checked_add(delay);
                    }
                    None => {} // SSE task ended (should not happen before quit)
                }
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

/// Run an action: API actions go to a background task; `Refresh` refetches
/// inline.
fn dispatch_action(
    client: &ApiClient,
    loop_tx: &mpsc::Sender<LoopMsg>,
    app: &mut App,
    action: Action,
    in_flight: &mut HashSet<&'static str>,
) {
    if action == Action::Refresh {
        spawn_refresh(client, loop_tx, app, in_flight);
        return;
    }
    if let Some(scope) = app.missing_scope(&action) {
        app.set_message(
            format!("this API key lacks the `{}` scope", scope.label()),
            true,
        );
        return;
    }
    let (client, loop_tx) = (client.clone(), loop_tx.clone());
    let currently_paused = app.pause_state().is_some_and(|p| p.any_paused());
    tokio::spawn(async move {
        let result = execute_action(&client, &action, currently_paused).await;
        drop(loop_tx.send(LoopMsg::Action(result)).await);
    });
}

/// Apply a background-task message to the app state.
fn handle_loop_msg(
    client: &ApiClient,
    loop_tx: &mpsc::Sender<LoopMsg>,
    app: &mut App,
    in_flight: &mut HashSet<&'static str>,
    msg: LoopMsg,
) {
    match msg {
        LoopMsg::Status(result) => {
            in_flight.remove("status");
            // The status fetch runs on every refresh regardless of tab, so
            // it alone decides the polling backoff.
            match result {
                Ok(status) => {
                    if app.clear_sticky() {
                        app.set_message("server reachable again".to_string(), false);
                    }
                    app.poll_failures = 0;
                    app.status = Some(status);
                }
                Err(e) => {
                    if is_outage(&e) {
                        app.poll_failures = app.poll_failures.saturating_add(1);
                    }
                    report_fetch_error(app, &e);
                }
            }
            app.next_poll_at = Instant::now().checked_add(poll_delay(app.poll_failures));
        }
        LoopMsg::Downloads(result) => {
            in_flight.remove("downloads");
            match result {
                Ok(downloads) => app.set_downloads(downloads),
                Err(e) => report_fetch_error(app, &e),
            }
        }
        LoopMsg::Sources(result) => {
            in_flight.remove("sources");
            match result {
                Ok(sources) => app.set_sources(sources),
                Err(e) => report_fetch_error(app, &e),
            }
        }
        LoopMsg::Profiles(result) => {
            in_flight.remove("profiles");
            match result {
                Ok(profiles) => app.set_profiles(profiles),
                Err(e) => report_fetch_error(app, &e),
            }
        }
        LoopMsg::Activity(result) => {
            in_flight.remove("activity");
            match result {
                Ok(activity) => app.set_activity(activity),
                Err(e) => report_fetch_error(app, &e),
            }
        }
        LoopMsg::Settings(result) => {
            in_flight.remove("settings");
            match result {
                Ok(settings) => app.set_settings(settings),
                Err(e) => report_fetch_error(app, &e),
            }
        }
        LoopMsg::Action(result) => {
            match result {
                Ok(text) => {
                    if !text.is_empty() {
                        app.set_message(text, false);
                    }
                }
                Err(e) => app.set_message(e.to_string(), true),
            }
            // Actions change server state; refetch so the UI reflects it.
            spawn_refresh(client, loop_tx, app, in_flight);
        }
    }
}
