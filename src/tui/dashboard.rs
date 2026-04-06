//! Live TUI dashboard for kew.

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::db::{self, Database};

/// Snapshot of system state for rendering.
struct DashboardState {
    tasks_pending: i64,
    tasks_running: i64,
    tasks_done: i64,
    tasks_failed: i64,
    recent_tasks: Vec<TaskRow>,
    context_count: usize,
    embedding_count: usize,
}

struct TaskRow {
    id: String,
    status: String,
    model: String,
    prompt: String,
    duration_ms: Option<i64>,
}

impl DashboardState {
    fn load(db: &Database) -> Self {
        let conn = db.conn();

        let counts = db::tasks::count_by_status(&conn).unwrap_or_default();
        let get = |s: &str| {
            counts
                .iter()
                .find(|(k, _)| k == s)
                .map(|(_, v)| *v)
                .unwrap_or(0)
        };

        let recent_tasks = db::tasks::list_tasks(&conn, None, 20)
            .unwrap_or_default()
            .into_iter()
            .map(|t| TaskRow {
                id: t.id[..12].to_string(), // truncate ULID for display
                status: format!("{:?}", t.status).to_lowercase(),
                model: t.model,
                prompt: if t.prompt.len() > 60 {
                    format!("{}...", &t.prompt[..57])
                } else {
                    t.prompt
                },
                duration_ms: t.duration_ms,
            })
            .collect();

        let context_count = db::context::list_context(&conn, None, 10000)
            .map(|v| v.len())
            .unwrap_or(0);

        let embedding_count = db::vectors::count_embeddings(&conn).unwrap_or(0);

        Self {
            tasks_pending: get("pending"),
            tasks_running: get("running"),
            tasks_done: get("done"),
            tasks_failed: get("failed"),
            recent_tasks,
            context_count,
            embedding_count,
        }
    }
}

/// Run the TUI dashboard. Blocks until the user presses 'q' or Esc.
pub fn run(db: &Database) -> io::Result<()> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    let tick_rate = Duration::from_secs(1);
    let mut last_tick = Instant::now();
    let mut state = DashboardState::load(db);

    loop {
        terminal.draw(|frame| render(frame, &state))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        _ => {}
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            state = DashboardState::load(db);
            last_tick = Instant::now();
        }
    }

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

fn render(frame: &mut Frame, state: &DashboardState) {
    let area = frame.area();

    // Split: top summary bar, main task table
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // Summary
            Constraint::Min(10),   // Task table
            Constraint::Length(1), // Footer
        ])
        .split(area);

    // --- Summary panel ---
    let summary = format!(
        " Tasks: {} pending | {} running | {} done | {} failed\n Context entries: {} | Embeddings: {}",
        state.tasks_pending,
        state.tasks_running,
        state.tasks_done,
        state.tasks_failed,
        state.context_count,
        state.embedding_count,
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

    // --- Task table ---
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
        Cell::from("Model").style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Cell::from("Duration").style(
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

    let rows: Vec<Row> = state
        .recent_tasks
        .iter()
        .map(|t| {
            let status_style = match t.status.as_str() {
                "done" => Style::default().fg(Color::Green),
                "running" => Style::default().fg(Color::Cyan),
                "pending" => Style::default().fg(Color::Yellow),
                "failed" => Style::default().fg(Color::Red),
                _ => Style::default(),
            };
            let duration = t
                .duration_ms
                .map(|d| format!("{}ms", d))
                .unwrap_or_else(|| "-".into());

            Row::new(vec![
                Cell::from(t.id.clone()),
                Cell::from(t.status.clone()).style(status_style),
                Cell::from(t.model.clone()),
                Cell::from(duration),
                Cell::from(t.prompt.clone()),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(15),
            Constraint::Length(10),
            Constraint::Min(20),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(" Recent Tasks ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );
    frame.render_widget(table, chunks[1]);

    // --- Footer ---
    let footer = Paragraph::new(" Press 'q' to quit | Refreshes every 1s")
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(footer, chunks[2]);
}
