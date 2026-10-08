//! Rendering for the TUI: header tabs, status bar, per-tab tables, message
//! line, key hints, and modal popups. All drawing is a pure function of
//! [`App`] — no I/O happens here.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Tabs, Wrap};

use crate::app::{
    App, EditSetting, InputMode, Paging, Popup, SETTINGS_ROWS, SettingsRow, Tab, fmt_time,
    human_bytes, human_duration, pause_label, progress_bar, short_codec, source_name, truncate,
};
use crate::types::{
    ActivitySeverity, ApiKeyScope, AuthMethod, PauseModule, Provenance, VideoStatus,
};

/// Accent color for the selected tab / selected row highlight.
const ACCENT: Color = Color::Cyan;

/// Draw the whole UI for the current frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, status, body, message, hints] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_tabs(frame, app, header);
    draw_status(frame, app, status);
    match app.tab {
        Tab::Downloads => draw_downloads(frame, app, body),
        Tab::Sources => draw_sources(frame, app, body),
        Tab::Profiles => draw_profiles(frame, app, body),
        Tab::Activity => draw_activity(frame, app, body),
        Tab::Settings => draw_settings(frame, app, body),
    }
    draw_message(frame, app, message);
    draw_hints(frame, app, hints);

    if let Some(popup) = &app.popup {
        draw_popup(frame, popup);
    }
}

// ----------------------------------------------------------------------
// Header / status / footer
// ----------------------------------------------------------------------

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| {
            Line::from(vec![
                Span::styled(
                    format!(" {} ", t.index().saturating_add(1)),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(t.title()),
            ])
        })
        .collect();

    let sse = match (app.sse_connected, app.sse_retry_at) {
        (true, _) => "live".to_string(),
        (false, Some(at)) => {
            let secs = at
                .saturating_duration_since(std::time::Instant::now())
                .as_secs();
            format!("offline, retry in {secs}s")
        }
        (false, None) => "offline".to_string(),
    };
    let sse_style = if app.sse_connected {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Red)
    };

    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Hofvarpnir ")
                .title_bottom(access_badge(app))
                .title_bottom(
                    Line::from(Span::styled(format!(" sse:{sse} "), sse_style)).right_aligned(),
                ),
        )
        .select(app.tab.index())
        .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
        .divider("|");
    frame.render_widget(tabs, area);
}

/// What the API key may do, colored by its most powerful scope.
fn access_badge(app: &App) -> Line<'static> {
    let (text, color) = match &app.access {
        None => ("key: unknown".to_string(), Color::DarkGray),
        Some(access) if access.auth_method == AuthMethod::Session => {
            ("session: full access".to_string(), Color::Red)
        }
        Some(access) => {
            let color = if access.has(ApiKeyScope::Delete) {
                Color::Red
            } else if access.has(ApiKeyScope::Write) {
                Color::Yellow
            } else {
                Color::Green
            };
            (format!("key: {}", access.label()), color)
        }
    };
    Line::from(Span::styled(
        format!(" {text} "),
        Style::default().fg(color),
    ))
    .left_aligned()
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();

    match &app.status {
        None => spans.push(Span::styled(
            "connecting…".to_string(),
            Style::default().fg(Color::Yellow),
        )),
        Some(status) => {
            if let Some(dl) = &status.downloads {
                if dl.supervisor_reachable {
                    let active = dl
                        .active_downloads
                        .map_or_else(|| "?".to_string(), |n| n.to_string());
                    let dispatching = dl
                        .dispatching
                        .map_or_else(|| "?".to_string(), |n| n.to_string());
                    spans.push(Span::raw(format!(
                        " active:{active}+{dispatching}/{} ",
                        dl.max_concurrent_downloads
                    )));
                } else {
                    spans.push(Span::styled(
                        " supervisor:DOWN ".to_string(),
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ));
                }
                if dl.db_backoff_until.is_some() {
                    spans.push(Span::styled(
                        " db-backoff ".to_string(),
                        Style::default().fg(Color::Red),
                    ));
                }
            }
            if let Some(sched) = &status.scheduler {
                spans.push(Span::raw(format!(" indexers:{} ", sched.active_indexers)));
                if !sched.running {
                    spans.push(Span::styled(
                        " scheduler:DOWN ".to_string(),
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ));
                }
            }
            if let Some(stats) = &status.statistics {
                spans.push(Span::raw(format!(
                    "| pending:{} dl:{} done:{} failed:{} perm:{}",
                    stats.pending_downloads,
                    stats.downloading,
                    stats.completed,
                    stats.failed,
                    stats.permanently_failed,
                )));
            }
        }
    }

    if let Some(pause) = app.pause_state()
        && pause.any_paused()
    {
        let mut parts: Vec<&str> = Vec::new();
        if pause.indexing.paused {
            parts.push("indexing");
        }
        if pause.downloads.paused {
            parts.push("downloads");
        }
        spans.push(Span::styled(
            format!(" PAUSED:{} ", parts.join("+")),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let bar = Paragraph::new(Line::from(spans))
        .block(Block::default().borders(Borders::ALL).title(" Status "));
    frame.render_widget(bar, area);
}

fn draw_message(frame: &mut Frame, app: &App, area: Rect) {
    let line = match &app.message {
        Some(msg) => {
            let style = if msg.is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Green)
            };
            let mut spans = vec![Span::styled(msg.text.clone(), style)];
            if msg.is_error
                && app.poll_failures > 0
                && let Some(at) = app.next_poll_at
            {
                let secs = at
                    .saturating_duration_since(std::time::Instant::now())
                    .as_secs();
                spans.push(Span::styled(
                    format!("  (retrying in {secs}s)"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Line::from(spans)
        }
        None => Line::default(),
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_hints(frame: &mut Frame, app: &App, area: Rect) {
    if app.input == InputMode::Search {
        draw_search_prompt(frame, app, area);
        return;
    }
    let tab_hints = match app.tab {
        Tab::Downloads => "r retry | c cancel | d delete | f filter | Enter detail",
        Tab::Sources => "i index now | d delete | Enter detail",
        Tab::Profiles => "d delete | Enter detail",
        Tab::Activity => "Enter detail",
        Tab::Settings => "Enter/e edit or toggle | r reset to env/default",
    };
    let filter = if app.tab == Tab::Downloads {
        format!(
            " | filter: {}",
            app.status_filter.map_or("all", VideoStatus::label)
        )
    } else {
        String::new()
    };
    let search = if app.query(app.tab).is_empty() {
        "/ search"
    } else {
        "/ edit search | Esc clear search"
    };
    let text = format!(
        " q quit | 1-5/Tab tabs | j/k/↑/↓ move | p pause/resume | F5 refresh | {search} | {tab_hints}{filter}"
    );
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

/// The `/` prompt in place of the key hints, with the terminal cursor at
/// the end of the query.
fn draw_search_prompt(frame: &mut Frame, app: &App, area: Rect) {
    let query = app.query(app.tab);
    let apply = if app.tab == Tab::Activity {
        "Enter search server (error/warn/info/ok filter severity)"
    } else {
        "Enter keep"
    };
    let line = Line::from(vec![
        Span::styled(
            "/",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(query.to_string()),
        Span::styled(
            format!("   {apply} | Esc clear | ↑/↓ move"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
    let len = u16::try_from(query.chars().count()).unwrap_or(u16::MAX);
    let x = area
        .x
        .saturating_add(1)
        .saturating_add(len)
        .min(area.right().saturating_sub(1));
    frame.set_cursor_position((x, area.y));
}

// ----------------------------------------------------------------------
// Tables
// ----------------------------------------------------------------------

/// Table title: ` Name (count) `, or ` Name (shown/loaded) /query ` while
/// a search narrows the rows.
fn table_title(app: &App, tab: Tab, shown: usize, loaded: usize) -> String {
    let query = app.query(tab);
    if query.is_empty() {
        format!(" {} ({loaded}) ", tab.title())
    } else {
        format!(" {} ({shown}/{loaded}) /{query} ", tab.title())
    }
}

fn header_style() -> Style {
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

fn selected_style() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

fn status_style(status: VideoStatus) -> Style {
    let color = match status {
        VideoStatus::Pending => Color::Yellow,
        VideoStatus::Downloading => Color::Cyan,
        VideoStatus::Completed => Color::Green,
        VideoStatus::Failed => Color::Red,
        VideoStatus::PermanentlyFailed => Color::Magenta,
        VideoStatus::Skipped | VideoStatus::Cleaned => Color::DarkGray,
    };
    Style::default().fg(color)
}

fn draw_downloads(frame: &mut Frame, app: &mut App, area: Rect) {
    let header =
        Row::new(["STATUS", "TITLE", "SOURCE", "PROGRESS", "SIZE", "INFO"]).style(header_style());

    let visible = app.visible(Tab::Downloads);
    let rows: Vec<Row> = visible
        .iter()
        .filter_map(|&i| app.downloads.get(i))
        .map(|v| {
            let progress_cell = if v.status == VideoStatus::Downloading {
                app.progress.get(&v.id).map_or_else(
                    || "starting…".to_string(),
                    |p| {
                        let mut text = progress_bar(p.percent, 10);
                        if let Some(speed) = &p.speed {
                            text.push(' ');
                            text.push_str(speed);
                        }
                        text
                    },
                )
            } else {
                String::new()
            };

            let info = if let Some(err) = &v.last_error {
                truncate(err, 60)
            } else if let Some(next) = v.next_retry {
                format!("retry {}", fmt_time(next))
            } else if let Some(h) = v.video_height {
                let codec = v.video_codec.as_deref().map_or("", short_codec);
                format!("{h}p {codec}")
            } else if let Some(d) = v.duration_secs {
                human_duration(d)
            } else {
                String::new()
            };

            Row::new(vec![
                Cell::from(v.status.label()).style(status_style(v.status)),
                Cell::from(truncate(&v.title, 60)),
                Cell::from(truncate(
                    v.source_display_name.as_deref().unwrap_or("-"),
                    24,
                )),
                Cell::from(progress_cell),
                Cell::from(v.file_size_bytes.map_or_else(String::new, human_bytes)),
                Cell::from(info),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(12),
        Constraint::Min(30),
        Constraint::Length(24),
        Constraint::Length(24),
        Constraint::Length(10),
        Constraint::Min(16),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(table_title(
            app,
            Tab::Downloads,
            visible.len(),
            app.downloads.len(),
        )))
        .row_highlight_style(selected_style())
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(table, area, &mut app.downloads_state);
}

fn draw_sources(frame: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new([
        "EN",
        "NAME",
        "TYPE",
        "PROFILE",
        "LAST INDEXED",
        "ORDER",
        "RET",
        "URL",
    ])
    .style(header_style());

    let visible = app.visible(Tab::Sources);
    let rows: Vec<Row> = visible
        .iter()
        .filter_map(|&i| app.sources.get(i))
        .map(|s| {
            let enabled = if s.enabled {
                Cell::from("●").style(Style::default().fg(Color::Green))
            } else {
                Cell::from("○").style(Style::default().fg(Color::DarkGray))
            };
            Row::new(vec![
                enabled,
                Cell::from(truncate(&source_name(s), 30)),
                Cell::from(s.source_type.map_or("?", crate::types::SourceType::label)),
                Cell::from(truncate(
                    app.profile_name(&s.profile_id).unwrap_or(&s.profile_id),
                    16,
                )),
                Cell::from(
                    s.last_indexed_at
                        .map_or_else(|| "never".to_string(), fmt_time),
                ),
                Cell::from(s.entry_order.label()),
                Cell::from(
                    s.retention_days
                        .map_or_else(|| "-".to_string(), |d| format!("{d}d")),
                ),
                Cell::from(truncate(&s.url, 40)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(2),
        Constraint::Min(20),
        Constraint::Length(8),
        Constraint::Length(16),
        Constraint::Length(19),
        Constraint::Length(9),
        Constraint::Length(5),
        Constraint::Min(24),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(table_title(
            app,
            Tab::Sources,
            visible.len(),
            app.sources.len(),
        )))
        .row_highlight_style(selected_style())
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(table, area, &mut app.sources_state);
}

fn draw_profiles(frame: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new([
        "NAME",
        "QUALITY",
        "PRESET",
        "OUTPUT DIR",
        "QUOTA",
        "RET",
        "FLAGS",
    ])
    .style(header_style());

    let visible = app.visible(Tab::Profiles);
    let rows: Vec<Row> = visible
        .iter()
        .filter_map(|&i| app.profiles.get(i))
        .map(|p| {
            let mut flags = String::new();
            if p.include_shorts {
                flags.push_str("shorts ");
            }
            if p.include_livestreams {
                flags.push_str("live");
            }
            Row::new(vec![
                Cell::from(truncate(&p.name, 24)),
                Cell::from(p.quality.map_or("?", crate::types::Quality::label)),
                Cell::from(
                    p.output_preset
                        .map_or("?", crate::types::OutputPreset::label),
                ),
                Cell::from(truncate(&p.output_dir, 40)),
                Cell::from(human_bytes(p.storage_quota_bytes)),
                Cell::from(
                    p.retention_days
                        .map_or_else(|| "-".to_string(), |d| format!("{d}d")),
                ),
                Cell::from(flags.trim_end().to_string()),
            ])
        })
        .collect();

    let widths = [
        Constraint::Min(20),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Min(24),
        Constraint::Length(10),
        Constraint::Length(5),
        Constraint::Length(11),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(table_title(
            app,
            Tab::Profiles,
            visible.len(),
            app.profiles.len(),
        )))
        .row_highlight_style(selected_style())
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(table, area, &mut app.profiles_state);
}

fn draw_activity(frame: &mut Frame, app: &mut App, area: Rect) {
    let header = Row::new(["TIME", "SEV", "TYPE", "MESSAGE"]).style(header_style());

    let rows: Vec<Row> = app
        .activity
        .iter()
        .map(|e| {
            let sev_style = match e.severity {
                Some(ActivitySeverity::Error) => Style::default().fg(Color::Red),
                Some(ActivitySeverity::Warning) => Style::default().fg(Color::Yellow),
                Some(ActivitySeverity::Success) => Style::default().fg(Color::Green),
                _ => Style::default(),
            };
            Row::new(vec![
                Cell::from(e.created_at.map_or_else(|| "?".to_string(), fmt_time)),
                Cell::from(e.severity.map_or("?", ActivitySeverity::label)).style(sev_style),
                Cell::from(
                    e.event_type
                        .map_or("?", crate::types::ActivityEventType::label),
                ),
                Cell::from(truncate(&e.message, 120)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(19),
        Constraint::Length(6),
        Constraint::Length(16),
        Constraint::Min(40),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(activity_title(app)),
        )
        .row_highlight_style(selected_style())
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(table, area, &mut app.activity_state);
}

/// ` Activity (loaded of total) `, the search sent to the server, and
/// whether the next page is on its way.
fn activity_title(app: &App) -> String {
    let mut title = format!(
        " Activity ({} of {}) ",
        app.activity.len(),
        app.activity_total
    );
    // The applied filter, not the prompt text, which is not sent until Enter.
    if !app.activity_filter.is_empty() {
        title.push('/');
        title.push_str(&app.activity_filter.describe());
        title.push(' ');
    }
    if app.activity_paging == Paging::Loading {
        title.push_str("loading more… ");
    } else if app.activity_has_more() {
        title.push_str("↓ for more ");
    }
    title
}

fn pause_style(state: &crate::types::PauseStateResponse) -> Style {
    if state.paused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::Green)
    }
}

fn draw_settings(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(settings) = &app.settings else {
        frame.render_widget(
            Paragraph::new("loading settings…")
                .block(Block::default().borders(Borders::ALL).title(" Settings ")),
            area,
        );
        return;
    };

    let header = Row::new(["SETTING", "VALUE", "SOURCE", "DESCRIPTION"]).style(header_style());
    let visible = app.visible(Tab::Settings);
    let rows: Vec<Row> = visible
        .iter()
        .filter_map(|&i| SETTINGS_ROWS.get(i))
        .map(|row| match *row {
            SettingsRow::Pause(module) => {
                let state = match module {
                    PauseModule::Indexing => &settings.pause.indexing,
                    PauseModule::Downloads | PauseModule::All => &settings.pause.downloads,
                };
                Row::new(vec![
                    Cell::from(row.label()),
                    Cell::from(pause_label(state)).style(pause_style(state)),
                    Cell::from("-").style(Style::default().fg(Color::DarkGray)),
                    Cell::from("Enter toggles pause / resume"),
                ])
            }
            SettingsRow::Knob(knob) => {
                let resolved = knob.get(settings);
                let source_style = match resolved.provenance {
                    Provenance::Database => Style::default().fg(ACCENT),
                    Provenance::Env => Style::default().fg(Color::Magenta),
                    Provenance::Default => Style::default().fg(Color::DarkGray),
                };
                Row::new(vec![
                    Cell::from(knob.label()),
                    Cell::from(knob.format(resolved.value)),
                    Cell::from(resolved.provenance.label()).style(source_style),
                    Cell::from(knob.description()),
                ])
            }
        })
        .collect();

    let audit = match (&settings.updated_at, &settings.updated_by) {
        (Some(at), Some(by)) => format!(" last change {} by {} ", fmt_time(*at), truncate(by, 26)),
        (Some(at), None) => format!(" last change {} ", fmt_time(*at)),
        _ => String::new(),
    };

    let widths = [
        Constraint::Length(26),
        Constraint::Length(30),
        Constraint::Length(9),
        Constraint::Min(20),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(table_title(
                    app,
                    Tab::Settings,
                    visible.len(),
                    SETTINGS_ROWS.len(),
                ))
                .title_bottom(
                    Line::from(Span::styled(
                        " database overrides env overrides default ",
                        Style::default().fg(Color::DarkGray),
                    ))
                    .left_aligned(),
                )
                .title_bottom(
                    Line::from(Span::styled(audit, Style::default().fg(Color::DarkGray)))
                        .right_aligned(),
                ),
        )
        .row_highlight_style(selected_style())
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(table, area, &mut app.settings_state);
}

// ----------------------------------------------------------------------
// Popups
// ----------------------------------------------------------------------

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let [_, mid, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(area);
    let height = height.min(mid.height);
    let [_, out, _] = Layout::vertical([
        Constraint::Length(mid.height.saturating_sub(height) / 2),
        Constraint::Length(height),
        Constraint::Min(0),
    ])
    .areas(mid);
    out
}

fn draw_popup(frame: &mut Frame, popup: &Popup) {
    match popup {
        Popup::Confirm(confirm) => {
            let area = centered_rect(50, 5, frame.area());
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(" Confirm ");
            let text = Paragraph::new(format!(
                "{}\n\n[y] yes   [any other key] no",
                confirm.prompt
            ))
            .block(block)
            .wrap(Wrap { trim: true });
            frame.render_widget(Clear, area);
            frame.render_widget(text, area);
        }
        Popup::Detail { title, body } => {
            let lines = u16::try_from(body.lines().count()).unwrap_or(u16::MAX);
            let area = centered_rect(
                70,
                lines
                    .saturating_add(2)
                    .min(frame.area().height.saturating_sub(2))
                    .max(7),
                frame.area(),
            );
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .title(format!(" {} ", truncate(title, 60)));
            let text = Paragraph::new(body.clone())
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(Clear, area);
            frame.render_widget(text, area);
        }
        Popup::Edit(edit) => draw_edit(frame, edit),
    }
}

fn draw_edit(frame: &mut Frame, edit: &EditSetting) {
    let area = centered_rect(50, 9, frame.area());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(format!(" {} ", edit.knob.label()));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let [desc, input, status, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    let unit = if edit.knob.is_duration() {
        "e.g. 90, 5m, 1h 30m, 1d"
    } else {
        "whole number"
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{} ({unit}, min {})",
            edit.knob.description(),
            edit.knob.min()
        ))
        .style(Style::default().fg(Color::DarkGray)),
        desc,
    );
    frame.render_widget(
        Paragraph::new(edit.input.as_str()).block(Block::default().borders(Borders::ALL)),
        input,
    );
    let len = u16::try_from(edit.input.chars().count()).unwrap_or(u16::MAX);
    let x = input
        .x
        .saturating_add(1)
        .saturating_add(len)
        .min(input.right().saturating_sub(2));
    frame.set_cursor_position((x, input.y.saturating_add(1)));

    if let Some(err) = &edit.error {
        frame.render_widget(
            Paragraph::new(err.as_str()).style(Style::default().fg(Color::Red)),
            status,
        );
    }
    frame.render_widget(
        Paragraph::new("[Enter] save  [Esc] cancel  [Ctrl-U] clear")
            .style(Style::default().fg(Color::DarkGray)),
        hints,
    );
}
