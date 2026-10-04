//! Startup screen asking for server URL and API token.
//!
//! Shown when no token comes from `--token`, `HOF_API_TOKEN`, or the config
//! file. The event loop in
//! [`crate::run`] owns a [`Setup`], feeds it key and paste events, and on
//! [`SetupOutcome::Submit`] tries to connect; failures land back here in
//! [`Setup::error`] so the user can correct the input. When no config file
//! exists yet, Ctrl-S marks the entered values to be saved there once the
//! connection succeeds.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

/// Input field with keyboard focus.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Field {
    Url,
    #[default]
    Token,
}

impl Field {
    const fn toggle(self) -> Self {
        match self {
            Self::Url => Self::Token,
            Self::Token => Self::Url,
        }
    }
}

/// What the event loop should do after a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupOutcome {
    Submit,
    Quit,
}

/// Setup form state.
#[derive(Debug, Default)]
pub struct Setup {
    pub url: String,
    pub token: String,
    pub focus: Field,
    /// Show the token in clear text (toggled with Ctrl-R).
    pub reveal: bool,
    /// Last validation or connection error.
    pub error: Option<String>,
    /// A connection attempt is in progress.
    pub connecting: bool,
    /// Config file the values can be saved to; `None` hides the option.
    pub save_path: Option<PathBuf>,
    /// Save URL and token to [`Self::save_path`] after connecting (Ctrl-S).
    pub save: bool,
}

impl Setup {
    /// Start with the URL prefilled (from `--api-url`, `HOF_API_URL`, or the
    /// default) and focus on the empty token field. Saving is opt-in: it
    /// writes a secret to disk.
    #[must_use]
    pub fn new(url: String, save_path: Option<PathBuf>) -> Self {
        Self {
            url,
            save_path,
            ..Self::default()
        }
    }

    const fn focused_mut(&mut self) -> &mut String {
        match self.focus {
            Field::Url => &mut self.url,
            Field::Token => &mut self.token,
        }
    }

    /// Insert pasted text into the focused field. Line breaks are dropped:
    /// both fields are single-line, and a copied token often carries a
    /// trailing newline.
    pub fn handle_paste(&mut self, text: &str) {
        self.focused_mut()
            .extend(text.chars().filter(|c| !c.is_control()));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<SetupOutcome> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => Some(SetupOutcome::Quit),
            KeyCode::Esc => Some(SetupOutcome::Quit),
            KeyCode::Char('r') if ctrl => {
                self.reveal = !self.reveal;
                None
            }
            KeyCode::Char('u') if ctrl => {
                self.focused_mut().clear();
                None
            }
            KeyCode::Char('s') if ctrl => {
                self.save = !self.save && self.save_path.is_some();
                None
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                self.focus = self.focus.toggle();
                None
            }
            KeyCode::Enter => {
                // Enter on the URL field moves on unless a token is already
                // there, so the natural top-to-bottom flow works.
                if self.focus == Field::Url && self.token.trim().is_empty() {
                    self.focus = Field::Token;
                    None
                } else {
                    Some(SetupOutcome::Submit)
                }
            }
            KeyCode::Backspace => {
                self.focused_mut().pop();
                None
            }
            KeyCode::Char(c) if !ctrl => {
                self.focused_mut().push(c);
                None
            }
            _ => None,
        }
    }
}

/// Field text as displayed: the token is masked unless revealed.
fn display_value(setup: &Setup, field: Field) -> String {
    match field {
        Field::Url => setup.url.clone(),
        Field::Token if setup.reveal => setup.token.clone(),
        Field::Token => "•".repeat(setup.token.chars().count()),
    }
}

/// Draw the setup form centered on the screen.
pub fn draw(frame: &mut Frame, setup: &Setup) {
    let area = centered(frame.area(), 72, 14);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Hofvarpnir — connect ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [intro, url, token, save, status, hints] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    frame.render_widget(
        Paragraph::new(
            "No API token given (--token, HOF_API_TOKEN, or config file). Create one in the web UI under Settings.",
        )
        .style(Style::default().fg(Color::DarkGray))
        .wrap(Wrap { trim: true }),
        intro,
    );
    draw_field(frame, setup, Field::Url, " Server URL ", url);
    draw_field(frame, setup, Field::Token, " API token (hof_sk_…) ", token);

    if let Some(path) = &setup.save_path {
        let (mark, style) = if setup.save {
            ("[x]", Style::default().fg(Color::Cyan))
        } else {
            ("[ ]", Style::default().fg(Color::DarkGray))
        };
        frame.render_widget(
            Paragraph::new(format!("{mark} [Ctrl-S] save to {}", path.display())).style(style),
            save,
        );
    }

    let status_line = if setup.connecting {
        Line::from(Span::styled(
            "connecting…",
            Style::default().fg(Color::Yellow),
        ))
    } else if let Some(err) = &setup.error {
        Line::from(Span::styled(err.clone(), Style::default().fg(Color::Red)))
    } else {
        Line::default()
    };
    frame.render_widget(
        Paragraph::new(status_line).wrap(Wrap { trim: true }),
        status,
    );

    frame.render_widget(
        Paragraph::new(
            "[Enter] connect  [Tab] switch field  [Ctrl-R] show token  [Ctrl-U] clear  [Esc] quit",
        )
        .style(Style::default().fg(Color::DarkGray)),
        hints,
    );
}

fn draw_field(frame: &mut Frame, setup: &Setup, field: Field, title: &str, area: Rect) {
    let focused = setup.focus == field;
    let border = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let value = display_value(setup, field);
    let widget = Paragraph::new(value.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(title),
    );
    frame.render_widget(widget, area);

    if focused && !setup.connecting {
        // Keep the cursor visible at the end of the text; long values scroll
        // off to the right, which is acceptable for a one-shot form.
        let len = u16::try_from(value.chars().count()).unwrap_or(u16::MAX);
        let max_x = area.right().saturating_sub(2);
        let x = area.x.saturating_add(1).saturating_add(len).min(max_x);
        frame.set_cursor_position((x, area.y.saturating_add(1)));
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x.saturating_add(area.width.saturating_sub(width) / 2),
        y: area
            .y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    }
}
