//! Application state and input handling.
//!
//! The event loop in `main.rs` owns an `App`, feeds it crossterm key events
//! ([`App::handle_key`] returns the [`Action`] to execute, if any), async
//! fetch results, and SSE progress events. Rendering lives in `ui.rs`.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

use crate::types::{
    ActivityEventResponse, ActivityEventType, ActivitySeverity, ApiKeyScope, OutputPreset,
    PauseModule, PauseSummaryResponse, ProfileResponse, ProgressEvent, Provenance, Quality,
    ResolvedValue, SettingsResponse, SourceResponse, SourceType, SystemStatusResponse,
    VideoResponse, VideoStatus, WhoAmIResponse,
};

/// Top-level tabs, left to right.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Downloads,
    Sources,
    Profiles,
    Activity,
    Settings,
}

impl Tab {
    pub const ALL: [Self; 5] = [
        Self::Downloads,
        Self::Sources,
        Self::Profiles,
        Self::Activity,
        Self::Settings,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Downloads => "Downloads",
            Self::Sources => "Sources",
            Self::Profiles => "Profiles",
            Self::Activity => "Activity",
            Self::Settings => "Settings",
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Downloads => 0,
            Self::Sources => 1,
            Self::Profiles => 2,
            Self::Activity => 3,
            Self::Settings => 4,
        }
    }

    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Downloads => Self::Sources,
            Self::Sources => Self::Profiles,
            Self::Profiles => Self::Activity,
            Self::Activity => Self::Settings,
            Self::Settings => Self::Downloads,
        }
    }

    #[must_use]
    pub const fn prev(self) -> Self {
        match self {
            Self::Downloads => Self::Settings,
            Self::Sources => Self::Downloads,
            Self::Profiles => Self::Sources,
            Self::Activity => Self::Profiles,
            Self::Settings => Self::Activity,
        }
    }
}

/// A runtime setting editable through `PATCH /api/v1/system/settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Knob {
    MaxConcurrentDownloads,
    MaxIndexersPerTick,
    RateLimitDelay,
    CheckInterval,
    CleanupInterval,
    DrainTimeout,
}

impl Knob {
    /// JSON field name in the settings request/response.
    pub const fn key(self) -> &'static str {
        match self {
            Self::MaxConcurrentDownloads => "max_concurrent_downloads",
            Self::MaxIndexersPerTick => "max_indexers_per_tick",
            Self::RateLimitDelay => "rate_limit_delay_secs",
            Self::CheckInterval => "check_interval_secs",
            Self::CleanupInterval => "cleanup_interval_secs",
            Self::DrainTimeout => "drain_timeout_secs",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::MaxConcurrentDownloads => "Max concurrent downloads",
            Self::MaxIndexersPerTick => "Max indexers per tick",
            Self::RateLimitDelay => "Rate-limit delay",
            Self::CheckInterval => "Scheduler check interval",
            Self::CleanupInterval => "Cleanup interval",
            Self::DrainTimeout => "Shutdown drain timeout",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::MaxConcurrentDownloads => "Downloads running at the same time",
            Self::MaxIndexersPerTick => "Sources indexed per scheduler tick",
            Self::RateLimitDelay => "Pause between requests to a platform",
            Self::CheckInterval => "How often the scheduler looks for due sources",
            Self::CleanupInterval => "How often retention cleanup runs",
            Self::DrainTimeout => "Grace period for in-flight work on shutdown",
        }
    }

    /// Whether the value is a duration in seconds (vs. a plain count).
    pub const fn is_duration(self) -> bool {
        !matches!(
            self,
            Self::MaxConcurrentDownloads | Self::MaxIndexersPerTick
        )
    }

    /// Lower bound enforced by the server (and the database `CHECK`).
    pub const fn min(self) -> u64 {
        match self {
            Self::RateLimitDelay => 0,
            _ => 1,
        }
    }

    pub const fn get(self, settings: &SettingsResponse) -> ResolvedValue {
        match self {
            Self::MaxConcurrentDownloads => settings.max_concurrent_downloads,
            Self::MaxIndexersPerTick => settings.max_indexers_per_tick,
            Self::RateLimitDelay => settings.rate_limit_delay_secs,
            Self::CheckInterval => settings.check_interval_secs,
            Self::CleanupInterval => settings.cleanup_interval_secs,
            Self::DrainTimeout => settings.drain_timeout_secs,
        }
    }

    /// Value as shown in the table and prefilled in the editor.
    pub fn format(self, value: u64) -> String {
        if self.is_duration() {
            human_secs(value)
        } else {
            value.to_string()
        }
    }

    /// Parse editor input: a plain number, or for durations a number with an
    /// `s`/`m`/`h`/`d` suffix, or several such parts (`90`, `5m`, `1h 30m`).
    /// Checks the server's bounds so a typo is caught before the round trip.
    ///
    /// # Errors
    ///
    /// A message for the edit popup.
    pub fn parse(self, input: &str) -> Result<u64, String> {
        let input = input.trim();
        let invalid = || {
            if self.is_duration() {
                format!("not a duration: `{input}` (try 90, 5m, 1h 30m, 1d)")
            } else {
                format!("not a whole number: `{input}`")
            }
        };
        let too_large = || format!("too large (max {})", i32::MAX);

        let value = if self.is_duration() {
            let mut total: u64 = 0;
            let mut parts = input.split_whitespace().peekable();
            if parts.peek().is_none() {
                return Err(invalid());
            }
            for part in parts {
                let (digits, multiplier) = match part.char_indices().last() {
                    Some((i, 's')) => (part.get(..i).unwrap_or_default(), 1),
                    Some((i, 'm')) => (part.get(..i).unwrap_or_default(), 60),
                    Some((i, 'h')) => (part.get(..i).unwrap_or_default(), 3600),
                    Some((i, 'd')) => (part.get(..i).unwrap_or_default(), 86_400),
                    _ => (part, 1),
                };
                let n: u64 = digits.parse().map_err(|_| invalid())?;
                total = n
                    .checked_mul(multiplier)
                    .and_then(|v| total.checked_add(v))
                    .ok_or_else(too_large)?;
            }
            total
        } else {
            input.parse().map_err(|_| invalid())?
        };

        if i32::try_from(value).is_err() {
            return Err(too_large());
        }
        if value < self.min() {
            return Err(format!("must be at least {}", self.min()));
        }
        Ok(value)
    }
}

/// One row of the settings table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsRow {
    Knob(Knob),
    /// Pause toggle for one module.
    Pause(PauseModule),
}

pub const SETTINGS_ROWS: [SettingsRow; 8] = [
    SettingsRow::Pause(PauseModule::Indexing),
    SettingsRow::Pause(PauseModule::Downloads),
    SettingsRow::Knob(Knob::MaxConcurrentDownloads),
    SettingsRow::Knob(Knob::MaxIndexersPerTick),
    SettingsRow::Knob(Knob::RateLimitDelay),
    SettingsRow::Knob(Knob::CheckInterval),
    SettingsRow::Knob(Knob::CleanupInterval),
    SettingsRow::Knob(Knob::DrainTimeout),
];

/// Seconds as a compact duration: `45s`, `5m`, `1h 30m`, `2d`. Round-trips
/// through [`Knob::parse`].
pub fn human_secs(secs: u64) -> String {
    if secs == 0 {
        return "0s".to_string();
    }
    let mut rest = secs;
    let mut parts = Vec::new();
    for (size, unit) in [(86_400, "d"), (3600, "h"), (60, "m"), (1, "s")] {
        let n = rest / size;
        if n > 0 {
            parts.push(format!("{n}{unit}"));
            rest %= size;
        }
    }
    parts.join(" ")
}

/// A unit of work for the event loop to execute against the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    RetryDownload(String),
    CancelDownload(String),
    DeleteDownload(String),
    TriggerIndex(String),
    DeleteSource(String),
    DeleteProfile(String),
    /// Pause or resume all modules, depending on current state.
    TogglePause,
    /// Pause (`true`) or resume (`false`) one module.
    SetPause(PauseModule, bool),
    /// Set a knob; `None` resets it to its env/default value.
    UpdateSetting(Knob, Option<u64>),
    /// Refetch the current tab's data (filter changed, tab switched, F5).
    Refresh,
}

impl Action {
    /// Scope the server checks for this action (see `hof-api` routes).
    pub const fn required_scope(&self) -> ApiKeyScope {
        match self {
            Self::RetryDownload(_)
            | Self::CancelDownload(_)
            | Self::TriggerIndex(_)
            | Self::TogglePause
            | Self::SetPause(..)
            | Self::UpdateSetting(..) => ApiKeyScope::Write,
            Self::DeleteDownload(_) | Self::DeleteSource(_) | Self::DeleteProfile(_) => {
                ApiKeyScope::Delete
            }
            Self::Refresh => ApiKeyScope::Read,
        }
    }
}

/// A destructive action awaiting confirmation.
#[derive(Debug, Clone)]
pub struct Confirm {
    pub prompt: String,
    pub action: Action,
}

/// Modal popup state.
#[derive(Debug, Clone)]
pub enum Popup {
    Confirm(Confirm),
    /// Read-only detail view for the selected row.
    Detail {
        title: String,
        body: String,
    },
    /// Value editor for a settings knob.
    Edit(EditSetting),
}

/// State of the settings value editor.
#[derive(Debug, Clone)]
pub struct EditSetting {
    pub knob: Knob,
    pub input: String,
    /// Validation error for the current input.
    pub error: Option<String>,
}

/// One line in the status-message area.
#[derive(Debug, Clone)]
pub struct StatusMessage {
    pub text: String,
    pub is_error: bool,
    /// When the message disappears on its own. `None` for a connection
    /// error, which stays until a refresh succeeds.
    pub expires_at: Option<std::time::Instant>,
}

/// How long one-off feedback (action results, validation errors) stays up.
pub const MESSAGE_TTL: std::time::Duration = std::time::Duration::from_secs(8);

/// Downloads-tab status filter, cycled with `f`.
pub const STATUS_FILTER_CYCLE: [Option<VideoStatus>; 6] = [
    None,
    Some(VideoStatus::Downloading),
    Some(VideoStatus::Pending),
    Some(VideoStatus::Failed),
    Some(VideoStatus::PermanentlyFailed),
    Some(VideoStatus::Completed),
];

/// Mutable application state.
#[derive(Debug, Default)]
pub struct App {
    pub tab: Tab,
    pub should_quit: bool,
    pub sse_connected: bool,
    /// When the next SSE reconnect attempt starts, while disconnected.
    pub sse_retry_at: Option<std::time::Instant>,
    /// Consecutive refreshes that found the server down; drives the polling
    /// backoff in `run.rs`.
    pub poll_failures: u32,
    /// When the next scheduled refresh runs.
    pub next_poll_at: Option<std::time::Instant>,

    pub downloads: Vec<VideoResponse>,
    pub sources: Vec<SourceResponse>,
    pub profiles: Vec<ProfileResponse>,
    pub activity: Vec<ActivityEventResponse>,
    pub status: Option<SystemStatusResponse>,
    pub settings: Option<SettingsResponse>,

    /// Latest progress event per video ID (from the SSE stream).
    pub progress: HashMap<String, ProgressEvent>,

    pub downloads_state: TableState,
    pub sources_state: TableState,
    pub profiles_state: TableState,
    pub activity_state: TableState,
    pub settings_state: TableState,

    /// Current status filter for the downloads list.
    pub status_filter: Option<VideoStatus>,

    /// The API key's scopes from `whoami`; `None` when the server is too old
    /// to report them.
    pub access: Option<WhoAmIResponse>,

    pub message: Option<StatusMessage>,
    pub popup: Option<Popup>,
}

impl App {
    /// The scope `action` needs that the key is known to lack. Unknown
    /// access (old server) never blocks: the server still enforces it.
    #[must_use]
    pub fn missing_scope(&self, action: &Action) -> Option<ApiKeyScope> {
        let scope = action.required_scope();
        self.access
            .as_ref()
            .is_some_and(|access| !access.has(scope))
            .then_some(scope)
    }

    // ------------------------------------------------------------------
    // Selection helpers
    // ------------------------------------------------------------------

    /// The table state of the current tab.
    const fn current_state_mut(&mut self) -> &mut TableState {
        match self.tab {
            Tab::Downloads => &mut self.downloads_state,
            Tab::Sources => &mut self.sources_state,
            Tab::Profiles => &mut self.profiles_state,
            Tab::Activity => &mut self.activity_state,
            Tab::Settings => &mut self.settings_state,
        }
    }

    /// Row count of the current tab.
    const fn current_len(&self) -> usize {
        match self.tab {
            Tab::Downloads => self.downloads.len(),
            Tab::Sources => self.sources.len(),
            Tab::Profiles => self.profiles.len(),
            Tab::Activity => self.activity.len(),
            Tab::Settings => {
                if self.settings.is_some() {
                    SETTINGS_ROWS.len()
                } else {
                    0
                }
            }
        }
    }

    const fn selected(&self) -> Option<usize> {
        match self.tab {
            Tab::Downloads => self.downloads_state.selected(),
            Tab::Sources => self.sources_state.selected(),
            Tab::Profiles => self.profiles_state.selected(),
            Tab::Activity => self.activity_state.selected(),
            Tab::Settings => self.settings_state.selected(),
        }
    }

    fn select_wrapping(&mut self, delta: isize) {
        let len = self.current_len();
        if len == 0 {
            self.current_state_mut().select(None);
            return;
        }
        let cur = self.selected().unwrap_or(0);
        let cur = isize::try_from(cur).unwrap_or(0);
        let len = isize::try_from(len).unwrap_or(isize::MAX);
        let next = cur.saturating_add(delta).rem_euclid(len);
        let next = usize::try_from(next).unwrap_or(0);
        self.current_state_mut().select(Some(next));
    }

    /// Keep the selection in range after a list refresh replaced its rows.
    const fn clamp_selection(state: &mut TableState, len: usize) {
        match (state.selected(), len) {
            (None, 0) => {}
            (None, _) => state.select(Some(0)),
            (Some(_), 0) => state.select(None),
            (Some(i), n) if i >= n => state.select(Some(n.saturating_sub(1))),
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // Data ingress
    // ------------------------------------------------------------------

    pub fn set_downloads(&mut self, downloads: Vec<VideoResponse>) {
        self.downloads = downloads;
        Self::clamp_selection(&mut self.downloads_state, self.downloads.len());
        // Drop progress entries for videos no longer listed (e.g. deleted).
        let ids: std::collections::HashSet<&str> =
            self.downloads.iter().map(|v| v.id.as_str()).collect();
        self.progress.retain(|id, _| ids.contains(id.as_str()));
    }

    pub fn set_sources(&mut self, sources: Vec<SourceResponse>) {
        self.sources = sources;
        Self::clamp_selection(&mut self.sources_state, self.sources.len());
    }

    pub fn set_settings(&mut self, settings: SettingsResponse) {
        self.settings = Some(settings);
        Self::clamp_selection(&mut self.settings_state, SETTINGS_ROWS.len());
    }

    pub fn set_profiles(&mut self, profiles: Vec<ProfileResponse>) {
        self.profiles = profiles;
        Self::clamp_selection(&mut self.profiles_state, self.profiles.len());
    }

    pub fn set_activity(&mut self, resp: crate::types::ActivityListResponse) {
        self.activity = resp.events;
        Self::clamp_selection(&mut self.activity_state, self.activity.len());
    }

    pub fn apply_progress(&mut self, event: ProgressEvent) {
        self.progress.insert(event.video_id.clone(), event);
    }

    /// Show one-off feedback that fades after [`MESSAGE_TTL`].
    pub fn set_message(&mut self, text: String, is_error: bool) {
        self.message = Some(StatusMessage {
            text,
            is_error,
            expires_at: std::time::Instant::now().checked_add(MESSAGE_TTL),
        });
    }

    /// Show a connection error that stays until [`Self::clear_sticky`].
    pub fn set_sticky_error(&mut self, text: String) {
        self.message = Some(StatusMessage {
            text,
            is_error: true,
            expires_at: None,
        });
    }

    /// Clear a sticky connection error; returns whether one was showing.
    pub fn clear_sticky(&mut self) -> bool {
        let sticky = self
            .message
            .as_ref()
            .is_some_and(|m| m.expires_at.is_none());
        if sticky {
            self.message = None;
        }
        sticky
    }

    /// Drop a one-off message whose time is up.
    pub fn expire_message(&mut self, now: std::time::Instant) {
        if self
            .message
            .as_ref()
            .and_then(|m| m.expires_at)
            .is_some_and(|at| at <= now)
        {
            self.message = None;
        }
    }

    /// Profile name for a source row, resolved from the profiles list.
    pub fn profile_name(&self, profile_id: &str) -> Option<&str> {
        self.profiles
            .iter()
            .find(|p| p.id == profile_id)
            .map(|p| p.name.as_str())
    }

    pub fn pause_state(&self) -> Option<PauseSummaryResponse> {
        self.status.as_ref().and_then(|s| s.pause)
    }

    // ------------------------------------------------------------------
    // Input handling
    // ------------------------------------------------------------------

    /// Handle a key press. Returns the action the event loop should execute
    /// asynchronously, if any.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.popup.is_some() {
            return self.handle_popup_key(key);
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
                None
            }
            KeyCode::Char('q') => {
                self.should_quit = true;
                None
            }
            KeyCode::Tab | KeyCode::Right => {
                self.tab = self.tab.next();
                Some(Action::Refresh)
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.tab = self.tab.prev();
                Some(Action::Refresh)
            }
            KeyCode::Char('1') => self.switch_to(Tab::Downloads),
            KeyCode::Char('2') => self.switch_to(Tab::Sources),
            KeyCode::Char('3') => self.switch_to(Tab::Profiles),
            KeyCode::Char('4') => self.switch_to(Tab::Activity),
            KeyCode::Char('5') => self.switch_to(Tab::Settings),
            KeyCode::Char('j') | KeyCode::Down => {
                self.select_wrapping(1);
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_wrapping(-1);
                None
            }
            KeyCode::Char('g') | KeyCode::Home => {
                if self.current_len() > 0 {
                    self.current_state_mut().select(Some(0));
                }
                None
            }
            KeyCode::Char('G') | KeyCode::End => {
                let len = self.current_len();
                if len > 0 {
                    self.current_state_mut().select(Some(len - 1));
                }
                None
            }
            KeyCode::Char('p') => Some(Action::TogglePause),
            KeyCode::F(5) => Some(Action::Refresh),
            KeyCode::Enter if self.tab == Tab::Settings => self.activate_setting(),
            KeyCode::Enter => {
                self.open_detail();
                None
            }
            _ => self.handle_tab_key(key),
        }
    }

    fn switch_to(&mut self, tab: Tab) -> Option<Action> {
        if self.tab == tab {
            None
        } else {
            self.tab = tab;
            Some(Action::Refresh)
        }
    }

    /// Tab-specific keys (actions on the selected row).
    fn handle_tab_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.tab {
            Tab::Downloads => match key.code {
                KeyCode::Char('f') => {
                    let pos = STATUS_FILTER_CYCLE
                        .iter()
                        .position(|s| *s == self.status_filter)
                        .unwrap_or(0);
                    self.status_filter = STATUS_FILTER_CYCLE
                        .get(pos.saturating_add(1))
                        .or_else(|| STATUS_FILTER_CYCLE.first())
                        .copied()
                        .flatten();
                    Some(Action::Refresh)
                }
                KeyCode::Char('r') => self
                    .selected_download()
                    .filter(|v| {
                        matches!(
                            v.status,
                            VideoStatus::Failed | VideoStatus::PermanentlyFailed
                        )
                    })
                    .map(|v| Action::RetryDownload(v.id.clone())),
                KeyCode::Char('c') => {
                    let target = self
                        .selected_download()
                        .filter(|v| {
                            matches!(v.status, VideoStatus::Pending | VideoStatus::Downloading)
                        })
                        .map(|v| (v.id.clone(), truncate(&v.title, 40)));
                    if let Some((id, title)) = target {
                        self.popup = Some(Popup::Confirm(Confirm {
                            prompt: format!("Cancel download \"{title}\"?"),
                            action: Action::CancelDownload(id),
                        }));
                    }
                    None
                }
                KeyCode::Char('d') => {
                    let target = self
                        .selected_download()
                        .map(|v| (v.id.clone(), truncate(&v.title, 40)));
                    if let Some((id, title)) = target {
                        self.popup = Some(Popup::Confirm(Confirm {
                            prompt: format!("Delete video \"{title}\"?"),
                            action: Action::DeleteDownload(id),
                        }));
                    }
                    None
                }
                _ => None,
            },
            Tab::Sources => match key.code {
                KeyCode::Char('i') => self
                    .selected_source()
                    .map(|s| Action::TriggerIndex(s.id.clone())),
                KeyCode::Char('d') => {
                    let target = self
                        .selected_source()
                        .map(|s| (s.id.clone(), truncate(&source_name(s), 40)));
                    if let Some((id, name)) = target {
                        self.popup = Some(Popup::Confirm(Confirm {
                            prompt: format!("Delete source \"{name}\"?"),
                            action: Action::DeleteSource(id),
                        }));
                    }
                    None
                }
                _ => None,
            },
            Tab::Profiles => match key.code {
                KeyCode::Char('d') => {
                    let target = self
                        .selected_profile()
                        .map(|p| (p.id.clone(), truncate(&p.name, 40)));
                    if let Some((id, name)) = target {
                        self.popup = Some(Popup::Confirm(Confirm {
                            prompt: format!("Delete profile \"{name}\"?"),
                            action: Action::DeleteProfile(id),
                        }));
                    }
                    None
                }
                _ => None,
            },
            Tab::Activity => None,
            Tab::Settings => match key.code {
                KeyCode::Char('e' | ' ') => self.activate_setting(),
                KeyCode::Char('r') => {
                    self.confirm_reset_setting();
                    None
                }
                _ => None,
            },
        }
    }

    pub fn selected_setting(&self) -> Option<SettingsRow> {
        self.settings.as_ref()?;
        self.settings_state
            .selected()
            .and_then(|i| SETTINGS_ROWS.get(i))
            .copied()
    }

    /// `Enter`/`e`/`Space` on the settings tab: toggle a pause row, or open
    /// the editor on a knob row.
    fn activate_setting(&mut self) -> Option<Action> {
        let row = self.selected_setting()?;
        let settings = self.settings.as_ref()?;
        let action = match row {
            SettingsRow::Pause(module) => {
                let paused = match module {
                    PauseModule::Indexing => settings.pause.indexing.paused,
                    PauseModule::Downloads | PauseModule::All => settings.pause.downloads.paused,
                };
                Action::SetPause(module, !paused)
            }
            SettingsRow::Knob(knob) => Action::UpdateSetting(knob, None),
        };
        // Refuse before opening an editor the key could never save.
        if let Some(scope) = self.missing_scope(&action) {
            self.set_message(
                format!("this API key lacks the `{}` scope", scope.label()),
                true,
            );
            return None;
        }
        match row {
            SettingsRow::Pause(_) => Some(action),
            SettingsRow::Knob(knob) => {
                self.popup = Some(Popup::Edit(EditSetting {
                    knob,
                    input: knob.format(knob.get(settings).value),
                    error: None,
                }));
                None
            }
        }
    }

    /// `r` on a knob row: drop the database override after confirmation.
    fn confirm_reset_setting(&mut self) {
        let Some(SettingsRow::Knob(knob)) = self.selected_setting() else {
            return;
        };
        let Some(settings) = &self.settings else {
            return;
        };
        if knob.get(settings).provenance != Provenance::Database {
            self.set_message(
                format!("{} has no database override to reset", knob.label()),
                false,
            );
            return;
        }
        self.popup = Some(Popup::Confirm(Confirm {
            prompt: format!("Reset \"{}\" to its env/default value?", knob.label()),
            action: Action::UpdateSetting(knob, None),
        }));
    }

    /// While a popup is open: `y`/`Enter` confirms a destructive action, any
    /// other key dismisses the popup without acting.
    fn handle_popup_key(&mut self, key: KeyEvent) -> Option<Action> {
        let popup = self.popup.take()?;
        match popup {
            Popup::Confirm(confirm) => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => Some(confirm.action),
                _ => None,
            },
            Popup::Detail { .. } => None,
            Popup::Edit(mut edit) => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc => return None,
                    KeyCode::Enter => match edit.knob.parse(&edit.input) {
                        Ok(value) => return Some(Action::UpdateSetting(edit.knob, Some(value))),
                        Err(e) => edit.error = Some(e),
                    },
                    KeyCode::Char('u') if ctrl => {
                        edit.input.clear();
                        edit.error = None;
                    }
                    KeyCode::Backspace => {
                        edit.input.pop();
                        edit.error = None;
                    }
                    KeyCode::Char(c) if !ctrl => {
                        edit.input.push(c);
                        edit.error = None;
                    }
                    _ => {}
                }
                self.popup = Some(Popup::Edit(edit));
                None
            }
        }
    }

    fn selected_download(&self) -> Option<&VideoResponse> {
        self.downloads_state
            .selected()
            .and_then(|i| self.downloads.get(i))
    }

    fn selected_source(&self) -> Option<&SourceResponse> {
        self.sources_state
            .selected()
            .and_then(|i| self.sources.get(i))
    }

    fn selected_profile(&self) -> Option<&ProfileResponse> {
        self.profiles_state
            .selected()
            .and_then(|i| self.profiles.get(i))
    }

    fn selected_activity(&self) -> Option<&ActivityEventResponse> {
        self.activity_state
            .selected()
            .and_then(|i| self.activity.get(i))
    }

    /// Open the read-only detail popup for the selected row.
    fn open_detail(&mut self) {
        let popup = match self.tab {
            Tab::Downloads => self.selected_download().map(download_detail),
            Tab::Sources => self
                .selected_source()
                .map(|s| source_detail(s, self.profile_name(&s.profile_id))),
            Tab::Profiles => self.selected_profile().map(profile_detail),
            Tab::Activity => self.selected_activity().map(activity_detail),
            Tab::Settings => None,
        };
        if let Some(popup) = popup {
            self.popup = Some(popup);
        }
    }
}

// ----------------------------------------------------------------------
// Row helpers shared by ui.rs and the detail popups
// ----------------------------------------------------------------------

/// Display name of a source: custom name, falling back to the URL.
pub fn source_name(s: &SourceResponse) -> String {
    s.custom_name.clone().unwrap_or_else(|| s.url.clone())
}

/// Truncate a string to at most `max` chars, adding an ellipsis when cut.
pub fn truncate(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let taken: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{taken}…")
    } else {
        taken
    }
}

/// Short codec name: `av01.0.12M.08` → `av01`, `avc1.64002a` → `avc1`.
pub fn short_codec(codec: &str) -> &str {
    codec.split('.').next().unwrap_or(codec)
}

/// Human-readable byte size.
#[allow(clippy::cast_precision_loss, clippy::as_conversions)]
pub fn human_bytes(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 0 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0_usize;
    while value >= 1024.0 && unit < UNITS.len().saturating_sub(1) {
        value /= 1024.0;
        unit = unit.saturating_add(1);
    }
    let label = UNITS.get(unit).unwrap_or(&"B");
    if unit == 0 {
        format!("{bytes} {label}")
    } else {
        format!("{value:.1} {label}")
    }
}

/// Format a timestamp for table cells (UTC, seconds precision).
pub fn fmt_time(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// In-memory text gauge for table cells: `█████░░░░░ 42%`.
///
/// Casts are display-only: the bar width is a handful of terminal cells and
/// `percent` comes from the API as `f64`, so truncation/precision are moot.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::as_conversions
)]
pub fn progress_bar(percent: f64, width: usize) -> String {
    let clamped = percent.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    let empty = width.saturating_sub(filled);
    format!(
        "{}{} {:>3.0}%",
        "█".repeat(filled),
        "░".repeat(empty),
        clamped
    )
}

/// Human-readable duration for table cells: `1h23m`, `45m`, `12s`.
pub fn human_duration(secs: i64) -> String {
    if secs < 0 {
        return "?".to_string();
    }
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    if hours > 0 {
        format!("{hours}h{mins:02}m")
    } else if mins > 0 {
        format!("{mins}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

// ----------------------------------------------------------------------
// Detail popup bodies
// ----------------------------------------------------------------------

fn download_detail(v: &VideoResponse) -> Popup {
    let mut lines = vec![
        format!("ID:       {}", v.id),
        format!("Platform: {} ({})", v.platform, v.platform_video_id),
        format!("Status:   {} (attempts: {})", v.status.label(), v.attempts),
    ];
    if let Some(name) = &v.source_display_name {
        lines.push(format!("Source:   {name}"));
    }
    if let (Some(q), Some(p)) = (v.profile_quality, v.profile_output_preset) {
        lines.push(format!(
            "Profile:  {} ({} / {})",
            v.profile_name.as_deref().unwrap_or("?"),
            q.label(),
            p.label()
        ));
    }
    if let Some(h) = v.video_height {
        let codec = v.video_codec.as_deref().map_or("?", short_codec);
        lines.push(format!("Delivered: {h}p {codec}"));
    }
    if let Some(size) = v.file_size_bytes {
        lines.push(format!("Size:     {}", human_bytes(size)));
    }
    if let Some(path) = &v.file_path {
        lines.push(format!("File:     {path}"));
    }
    if let Some(next) = v.next_retry {
        lines.push(format!("Retry at: {}", fmt_time(next)));
    }
    if let Some(err) = &v.last_error {
        lines.push(format!("\n--- last error ---\n{err}"));
    }
    if let Some(desc) = &v.description {
        lines.push(format!("\n--- description ---\n{desc}"));
    }
    Popup::Detail {
        title: v.title.clone(),
        body: lines.join("\n"),
    }
}

fn source_detail(s: &SourceResponse, profile_name: Option<&str>) -> Popup {
    let body = format!(
        "ID:        {}\nURL:       {}\nType:      {}\nProfile:   {}\nEnabled:   {}\nCutoff:    {}\nFrequency: {}s\nRetention: {}\nOrder:     {}\nIndexed:   {}\nCleanup-exempt: {}",
        s.id,
        s.url,
        s.source_type.map_or("?", SourceType::label),
        profile_name.unwrap_or(&s.profile_id),
        s.enabled,
        s.cutoff_date,
        s.index_frequency_secs,
        s.retention_days
            .map_or_else(|| "profile".to_string(), |d| format!("{d}d")),
        s.entry_order.label(),
        s.last_indexed_at
            .map_or_else(|| "never".to_string(), fmt_time),
        s.exclude_from_cleanup,
    );
    Popup::Detail {
        title: source_name(s),
        body,
    }
}

fn profile_detail(p: &ProfileResponse) -> Popup {
    let body = format!(
        "ID:        {}\nQuality:   {}\nPreset:    {}\nTemplate:  {}\nOutput:    {}\nQuota:     {}\nRetention: {}\nShorts:    {}\nLivestreams: {}",
        p.id,
        p.quality.map_or("?", Quality::label),
        p.output_preset.map_or("?", OutputPreset::label),
        p.naming_template,
        p.output_dir,
        human_bytes(p.storage_quota_bytes),
        p.retention_days
            .map_or_else(|| "none".to_string(), |d| format!("{d}d")),
        p.include_shorts,
        p.include_livestreams,
    );
    Popup::Detail {
        title: p.name.clone(),
        body,
    }
}

fn activity_detail(e: &ActivityEventResponse) -> Popup {
    let body = format!(
        "Time:     {}\nSeverity: {}\nType:     {}\n\n{}",
        e.created_at.map_or_else(|| "?".to_string(), fmt_time),
        e.severity.map_or("?", ActivitySeverity::label),
        e.event_type.map_or("?", ActivityEventType::label),
        e.message,
    );
    Popup::Detail {
        title: "Activity event".to_string(),
        body,
    }
}
