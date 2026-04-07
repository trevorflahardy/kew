//! Live TUI dashboard for kew with interactive task selection and detail view.
//!
//! ## Navigation
//! - List view: ↑/↓ to move, Enter to open a task, q/Esc to quit
//! - Detail view: ↑/↓ to scroll, c to cancel, Esc/q to go back to list

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};

use crate::db::{self, Database};

/// Which screen is currently active.
enum Screen {
    List,
    Detail,
}

struct TaskRow {
    /// Full ULID (used for navigation).
    id: String,
    /// 12-char display prefix.
    id_short: String,
    status: String,
    agent: Option<String>,
    prompt: String,
    duration_ms: Option<i64>,
}

/// Full task info shown in the detail view.
struct DetailInfo {
    id: String,
    status: String,
    model: String,
    agent: Option<String>,
    prompt: String,
    result: Option<String>,
    error: Option<String>,
    duration_ms: Option<i64>,
    prompt_tokens: Option<i32>,
    completion_tokens: Option<i32>,
    log_chunks: Vec<String>,
}

struct App {
    screen: Screen,
    // ── List ──────────────────────────────────────────────
    list_state: TableState,
    tasks: Vec<TaskRow>,
    tasks_pending: i64,
    tasks_running: i64,
    tasks_done: i64,
    tasks_failed: i64,
    context_count: usize,
    embedding_count: usize,
    // ── Detail ────────────────────────────────────────────
    selected_task_id: Option<String>,
    detail: Option<DetailInfo>,
    /// Lines scrolled from the top of the log area.
    log_scroll: usize,
    /// When true, log_scroll tracks the bottom automatically.
    auto_scroll: bool,
}

impl App {
    fn new(db: &Database) -> Self {
        let mut app = Self {
            screen: Screen::List,
            list_state: TableState::default(),
            tasks: vec![],
            tasks_pending: 0,
            tasks_running: 0,
            tasks_done: 0,
            tasks_failed: 0,
            context_count: 0,
            embedding_count: 0,
            selected_task_id: None,
            detail: None,
            log_scroll: 0,
            auto_scroll: true,
        };
        app.refresh_list(db);
        if !app.tasks.is_empty() {
            app.list_state.select(Some(0));
        }
        app
    }

    fn refresh_list(&mut self, db: &Database) {
        let conn = db.conn();
        let counts = db::tasks::count_by_status(&conn).unwrap_or_default();
        let get = |s: &str| {
            counts
                .iter()
                .find(|(k, _)| k == s)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        };
        self.tasks_pending = get("pending");
        self.tasks_running = get("running");
        self.tasks_done = get("done");
        self.tasks_failed = get("failed");

        self.tasks = db::tasks::list_tasks(&conn, None, 50)
            .unwrap_or_default()
            .into_iter()
            .map(|t| {
                let id_short = t.id[..12.min(t.id.len())].to_string();
                TaskRow {
                    id_short,
                    id: t.id,
                    status: format!("{:?}", t.status).to_lowercase(),
                    agent: t.agent,
                    prompt: if t.prompt.len() > 52 {
                        format!("{}…", &t.prompt[..52])
                    } else {
                        t.prompt
                    },
                    duration_ms: t.duration_ms,
                }
            })
            .collect();

        self.context_count = db::context::list_context(&conn, None, 10000)
            .map(|v| v.len())
            .unwrap_or(0);
        self.embedding_count = db::vectors::count_embeddings(&conn).unwrap_or(0);

        // Keep selection in bounds after refresh.
        let n = self.tasks.len();
        if n == 0 {
            self.list_state.select(None);
        } else if let Some(sel) = self.list_state.selected() {
            if sel >= n {
                self.list_state.select(Some(n - 1));
            }
        }
    }

    fn refresh_detail(&mut self, db: &Database) {
        let task_id = match &self.selected_task_id {
            Some(id) => id.clone(),
            None => return,
        };
        let conn = db.conn();
        let task = match db::tasks::get_task(&conn, &task_id).ok().flatten() {
            Some(t) => t,
            None => return,
        };
        let log_chunks = db::task_logs::get_chunks(&conn, &task_id).unwrap_or_default();
        self.detail = Some(DetailInfo {
            id: task.id,
            status: format!("{:?}", task.status).to_lowercase(),
            model: task.model,
            agent: task.agent,
            prompt: task.prompt,
            result: task.result,
            error: task.error,
            duration_ms: task.duration_ms,
            prompt_tokens: task.prompt_tokens,
            completion_tokens: task.completion_tokens,
            log_chunks,
        });
        // Auto-scroll: bump scroll to a large value; ratatui clamps it to content height.
        if self.auto_scroll {
            self.log_scroll = usize::MAX / 2;
        }
    }

    fn open_detail(&mut self, db: &Database) {
        if let Some(sel) = self.list_state.selected() {
            if let Some(row) = self.tasks.get(sel) {
                self.selected_task_id = Some(row.id.clone());
                self.log_scroll = usize::MAX / 2;
                self.auto_scroll = true;
                self.detail = None;
                self.screen = Screen::Detail;
                self.refresh_detail(db);
            }
        }
    }

    fn cancel_selected(&self, db: &Database) {
        if let Some(ref task_id) = self.selected_task_id {
            let conn = db.conn();
            let _ = db::tasks::cancel_task(&conn, task_id);
        }
    }

    /// Returns true if the selected task is cancellable.
    fn can_cancel(&self) -> bool {
        self.detail
            .as_ref()
            .is_some_and(|d| matches!(d.status.as_str(), "pending" | "assigned" | "running"))
    }

    fn move_up(&mut self) {
        match self.screen {
            Screen::List => {
                let n = self.tasks.len();
                if n == 0 {
                    return;
                }
                let next = match self.list_state.selected() {
                    Some(0) | None => 0,
                    Some(i) => i - 1,
                };
                self.list_state.select(Some(next));
            }
            Screen::Detail => {
                self.auto_scroll = false;
                self.log_scroll = self.log_scroll.saturating_sub(1);
            }
        }
    }

    fn move_down(&mut self) {
        match self.screen {
            Screen::List => {
                let n = self.tasks.len();
                if n == 0 {
                    return;
                }
                let next = match self.list_state.selected() {
                    None => 0,
                    Some(i) => (i + 1).min(n - 1),
                };
                self.list_state.select(Some(next));
            }
            Screen::Detail => {
                self.auto_scroll = false;
                self.log_scroll = self.log_scroll.saturating_add(1);
            }
        }
    }
}

/// Run the TUI dashboard. Blocks until the user quits.
pub fn run(db: &Database) -> io::Result<()> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let tick_rate = Duration::from_millis(500);
    let mut last_tick = Instant::now();
    let mut app = App::new(db);

    loop {
        terminal.draw(|frame| match app.screen {
            Screen::List => render_list(frame, &mut app),
            Screen::Detail => render_detail(frame, &mut app),
        })?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match app.screen {
                        Screen::List => match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Up => app.move_up(),
                            KeyCode::Down => app.move_down(),
                            KeyCode::Enter => app.open_detail(db),
                            _ => {}
                        },
                        Screen::Detail => match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => {
                                app.screen = Screen::List;
                                app.detail = None;
                            }
                            KeyCode::Up => app.move_up(),
                            KeyCode::Down => app.move_down(),
                            KeyCode::Char('c') | KeyCode::Char('C') => {
                                if app.can_cancel() {
                                    app.cancel_selected(db);
                                    app.refresh_detail(db);
                                }
                            }
                            _ => {}
                        },
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            match app.screen {
                Screen::List => app.refresh_list(db),
                Screen::Detail => app.refresh_detail(db),
            }
            last_tick = Instant::now();
        }
    }

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

// ── List view ────────────────────────────────────────────────────────────────

fn render_list(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .split(area);

    // Summary bar
    let summary = format!(
        " {} pending  {} running  {} done  {} failed   context: {}  embeddings: {}",
        app.tasks_pending,
        app.tasks_running,
        app.tasks_done,
        app.tasks_failed,
        app.context_count,
        app.embedding_count,
    );
    let summary_widget = Paragraph::new(summary)
        .block(
            Block::default()
                .title(" kew status ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .style(Style::default().fg(Color::White));
    frame.render_widget(summary_widget, chunks[0]);

    // Task table
    let header = Row::new(vec![
        Cell::from("ID").style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Status").style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Agent").style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Dur").style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Prompt").style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let rows: Vec<Row> = app
        .tasks
        .iter()
        .map(|t| {
            let status_style = match t.status.as_str() {
                "done" => Style::default().fg(Color::Green),
                "running" | "assigned" => Style::default().fg(Color::Cyan),
                "pending" => Style::default().fg(Color::Yellow),
                "failed" => Style::default().fg(Color::Red),
                "cancelled" => Style::default().fg(Color::DarkGray),
                _ => Style::default(),
            };
            let duration = t
                .duration_ms
                .map(|d| {
                    if d >= 60_000 {
                        format!("{}m{}s", d / 60_000, (d % 60_000) / 1000)
                    } else if d >= 1000 {
                        format!("{:.1}s", d as f64 / 1000.0)
                    } else {
                        format!("{}ms", d)
                    }
                })
                .unwrap_or_else(|| "-".into());
            let agent = t.agent.as_deref().unwrap_or("-").to_string();

            Row::new(vec![
                Cell::from(t.id_short.clone()),
                Cell::from(t.status.clone()).style(status_style),
                Cell::from(agent),
                Cell::from(duration),
                Cell::from(t.prompt.clone()),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Length(8),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(" Tasks (↑/↓ select, Enter to open) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("► ");
    frame.render_stateful_widget(table, chunks[1], &mut app.list_state);

    // Footer
    let footer = Paragraph::new(" ↑/↓ navigate  Enter open  q quit  │  refreshes every 0.5s")
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, chunks[2]);
}

// ── Detail view ──────────────────────────────────────────────────────────────

fn render_detail(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // task header
            Constraint::Min(5),    // log / output
            Constraint::Length(1), // footer
        ])
        .split(area);

    let Some(ref detail) = app.detail else {
        let msg = Paragraph::new("Loading…").block(Block::default().borders(Borders::ALL));
        frame.render_widget(msg, area);
        return;
    };

    // ── Header ──────────────────────────────────────────
    let status_color = match detail.status.as_str() {
        "done" => Color::Green,
        "running" | "assigned" => Color::Cyan,
        "pending" => Color::Yellow,
        "failed" => Color::Red,
        _ => Color::DarkGray,
    };
    let duration_str = detail
        .duration_ms
        .map(|d| {
            if d >= 60_000 {
                format!("{}m{}s", d / 60_000, (d % 60_000) / 1000)
            } else if d >= 1000 {
                format!("{:.1}s", d as f64 / 1000.0)
            } else {
                format!("{}ms", d)
            }
        })
        .unwrap_or_else(|| "in progress".into());

    let tokens_per_sec_str = match (
        detail.duration_ms,
        detail.prompt_tokens,
        detail.completion_tokens,
    ) {
        (Some(d), Some(p), Some(c)) if d > 0 => {
            let total = (p + c) as f64;
            let secs = d as f64 / 1000.0;
            Some(format!("{:.0} tok/s ({p}+{c})", total / secs))
        }
        _ => None,
    };

    let header_text = vec![
        Line::from(vec![
            Span::styled("  ID:     ", Style::default().fg(Color::DarkGray)),
            Span::raw(&detail.id),
        ]),
        Line::from({
            let mut spans = vec![
                Span::styled("  Status: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    &detail.status,
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("   Model: ", Style::default().fg(Color::DarkGray)),
                Span::raw(&detail.model),
                Span::styled("   Agent: ", Style::default().fg(Color::DarkGray)),
                Span::raw(detail.agent.as_deref().unwrap_or("—")),
                Span::styled("   Duration: ", Style::default().fg(Color::DarkGray)),
                Span::raw(&duration_str),
            ];
            if let Some(ref tps) = tokens_per_sec_str {
                spans.push(Span::styled(
                    "   Speed: ",
                    Style::default().fg(Color::DarkGray),
                ));
                spans.push(Span::styled(tps.as_str(), Style::default().fg(Color::Cyan)));
            }
            spans
        }),
        Line::from(vec![
            Span::styled("  Prompt: ", Style::default().fg(Color::DarkGray)),
            Span::raw(
                if detail.prompt.len() > (area.width as usize).saturating_sub(12) {
                    &detail.prompt[..area.width as usize - 12]
                } else {
                    &detail.prompt
                },
            ),
        ]),
    ];

    let header_widget = Paragraph::new(header_text).block(
        Block::default()
            .title(" Task Detail ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(header_widget, chunks[0]);

    // ── Log / output area ───────────────────────────────
    let mut lines: Vec<Line> = Vec::new();

    if detail.log_chunks.is_empty() && detail.result.is_none() && detail.error.is_none() {
        lines.push(Line::from(Span::styled(
            "  Waiting for output…",
            Style::default().fg(Color::DarkGray),
        )));
    }

    for chunk in &detail.log_chunks {
        let style = if chunk.starts_with("    ←") {
            Style::default().fg(Color::DarkGray)
        } else if chunk.starts_with('[') {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(chunk.as_str(), style)));
    }

    if let Some(ref result) = detail.result {
        if !detail.log_chunks.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "── result ──",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::DIM),
            )));
        }
        for l in result.lines() {
            lines.push(Line::from(l.to_string()));
        }
    }

    if let Some(ref error) = detail.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "── error ──",
            Style::default().fg(Color::Red).add_modifier(Modifier::DIM),
        )));
        for l in error.lines() {
            lines.push(Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(Color::Red),
            )));
        }
    }

    let total_lines = lines.len();
    let visible_height = chunks[1].height.saturating_sub(2) as usize; // subtract borders
    let max_scroll = total_lines.saturating_sub(visible_height);
    let scroll = app.log_scroll.min(max_scroll) as u16;
    // Sync app state back (clamped value)
    if !app.auto_scroll {
        app.log_scroll = scroll as usize;
    }

    let auto_indicator = if app.auto_scroll {
        " [follow]"
    } else {
        " [↑/↓ scroll]"
    };
    let log_title = format!(" Output{auto_indicator} ");

    let log_widget = Paragraph::new(lines)
        .block(
            Block::default()
                .title(log_title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(log_widget, chunks[1]);

    // ── Footer ──────────────────────────────────────────
    let cancel_hint = if app.can_cancel() {
        "  c cancel  │"
    } else {
        ""
    };
    let footer_text = format!(" Esc back  ↑/↓ scroll{cancel_hint}  refreshes every 0.5s");
    let footer = Paragraph::new(footer_text).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, chunks[2]);
}
