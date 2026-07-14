//! `mc tui` — the Mission Control TUI client.
//!
//! Connects to the daemon via Unix socket, polls `mc.snapshot`,
//! and renders a ratatui dashboard with the "needs-you" lane.

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use mc_schema::pane_view::{Attention, PaneView, Totals};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame,
};
use interprocess::local_socket::prelude::*;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Thin JSON-RPC client to the mc daemon.
struct McClient {
    socket_path: PathBuf,
}

impl McClient {
    fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// Call a method on the daemon and get the raw JSON value response.
    fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let mut stream = interprocess::local_socket::Stream::connect(
            self.socket_path.clone()
                .to_fs_name::<interprocess::local_socket::GenericFilePath>()
                .map_err(|e| format!("socket error: {e}"))?,
        )
        .map_err(|e| format!("connect error: {e}"))?;

        stream
            .set_send_timeout(Some(Duration::from_secs(3)))
            .unwrap_or(());
        stream
            .set_recv_timeout(Some(Duration::from_secs(3)))
            .unwrap_or(());

        // Send null for unit variants, the value for others
        let params_value = if params.is_null() {
            serde_json::Value::Null
        } else {
            params
        };

        let request = serde_json::json!({
            "id": "tui",
            "method": method,
            "params": params_value,
        });

        stream
            .write_all(serde_json::to_string(&request).unwrap().as_bytes())
            .map_err(|e| format!("write error: {e}"))?;
        stream.write_all(b"\n").unwrap_or(());
        stream.flush().unwrap_or(());

        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| format!("read error: {e}"))?;

        if line.trim().is_empty() {
            return Err("empty response".to_string());
        }

        let val: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| format!("json error: {e}"))?;

        if let Some(error) = val.get("error") {
            return Err(format!("api error: {error}"));
        }

        Ok(val.get("result").cloned().unwrap_or(serde_json::Value::Null))
    }

    /// Get the current snapshot of all panes + totals.
    fn snapshot(&self) -> Result<(Vec<PaneView>, Totals), String> {
        let result = self.call("mc.snapshot", serde_json::Value::Null)?;
        let panes: Vec<PaneView> = serde_json::from_value(
            result.get("panes").cloned().unwrap_or(serde_json::Value::Array(vec![])),
        )
        .map_err(|e| format!("parse panes: {e}"))?;
        let totals: Totals = serde_json::from_value(
            result.get("totals").cloned().unwrap_or(serde_json::Value::Null),
        )
        .map_err(|e| format!("parse totals: {e}"))?;
        Ok((panes, totals))
    }
}

/// Run the TUI.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let socket_path = daemon_socket_path();
    let client = McClient::new(socket_path.clone());

    // Test connection
    match client.snapshot() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("mc: cannot connect to daemon at {}: {e}", socket_path.display());
            eprintln!("mc: is `mc serve` running?");
            std::process::exit(1);
        }
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut last_update = Instant::now();
    let mut panes: Vec<PaneView> = Vec::new();
    let mut totals = Totals {
        pane_count: 0,
        working_count: 0,
        idle_count: 0,
        blocked_count: 0,
        total_cost_usd: 0.0,
        total_tool_calls: 0,
    };
    let mut error_msg: Option<String> = None;
    let poll_interval = Duration::from_secs(1);

    loop {
        // Poll for keyboard input
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    _ => {}
                }
            }
        }

        // Poll daemon for updates
        if last_update.elapsed() >= poll_interval {
            match client.snapshot() {
                Ok((new_panes, new_totals)) => {
                    panes = new_panes;
                    totals = new_totals;
                    error_msg = None;
                }
                Err(e) => {
                    error_msg = Some(format!("daemon error: {e}"));
                }
            }
            last_update = Instant::now();
        }

        terminal.draw(|f| {
            render(f, &panes, &totals, &error_msg);
        })?;
    }

    // Cleanup
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

fn daemon_socket_path() -> PathBuf {
    dirs::runtime_dir()
        .or_else(|| dirs::cache_dir())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("mc.sock")
}

// ── Rendering ──────────────────────────────────────────────────────────

fn render(f: &mut Frame, panes: &[PaneView], totals: &Totals, error_msg: &Option<String>) {
    let size = f.area();

    // Header
    let header_text = format!(
        "Mission Control — {} panes | {} working | {} idle | {} blocked | ${:.4} total",
        totals.pane_count, totals.working_count, totals.idle_count,
        totals.blocked_count, totals.total_cost_usd
    );

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)].as_ref())
        .split(size);

    let header = Paragraph::new(header_text)
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("mc"));
    f.render_widget(header, chunks[0]);

    // Error banner
    if let Some(msg) = error_msg {
        let error = Paragraph::new(msg.as_str())
            .style(Style::default().fg(Color::Red));
        f.render_widget(error, chunks[1]);
        return;
    }

    // Pane list
    let items: Vec<ListItem> = panes
        .iter()
        .map(|p| pane_list_item(p))
        .collect();

    let pane_list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Panes (q to quit)"),
    );
    f.render_widget(pane_list, chunks[1]);
}

fn pane_list_item(p: &PaneView) -> ListItem<'static> {
    let attention_str = match p.flags.attention {
        Attention::Critical => "🔴 CRIT",
        Attention::High => "🟠 HIGH",
        Attention::Medium => "🟡 MED ",
        Attention::Low => "🟢 LOW ",
        Attention::None => "      ",
    };

    let status_str = match p.agent_status {
        mc_schema::pane_view::AgentStatus::Idle => "IDLE",
        mc_schema::pane_view::AgentStatus::Working => " WORK",
        mc_schema::pane_view::AgentStatus::Blocked => "BLOCKED",
        mc_schema::pane_view::AgentStatus::Done => "DONE",
        mc_schema::pane_view::AgentStatus::Unknown => "UNKNOWN",
    };

    let focus_str = if p.focused { "👁" } else { " " };

    let project = p
        .project
        .as_ref()
        .and_then(|proj| proj.name.as_deref())
        .unwrap_or("-");

    let model = if p.vitals.model.is_empty() {
        "-"
    } else {
        &p.vitals.model
    };

    let last_ask = p
        .last_user_message
        .as_deref()
        .unwrap_or("-");
    let last_ask_short = if last_ask.len() > 80 {
        &last_ask[..77]
    } else {
        last_ask
    };

    let tools_delta = p.vitals_since_last_user.tool_calls;
    let delta_str = if tools_delta > 0 {
        format!("+{}t", tools_delta)
    } else {
        String::from("-")
    };

    let line = format!(
        "{focus}{attention} {pane_id:<18} {status:<7} {proj:<14} {model:<18} ${cost:<8.4} {delta:>5}  {ask}",
        focus = focus_str,
        attention = attention_str,
        pane_id = p.pane_id,
        status = status_str,
        proj = project,
        model = model,
        cost = p.vitals.total_cost_usd,
        delta = delta_str,
        ask = last_ask_short,
    );

    ListItem::new(Line::from(line))
}