//! `mc status` — inline collectors + reducer, prints the "needs-you" table.
//!
//! Phase 1 implementation: no daemon, no transport. Collects from herdr, pi
//! sessions, and project scans; reduces; prints a table sorted by attention.

use mc_core::collector::herdr;
use mc_core::collector::pi;
use mc_core::collector::project;
use mc_core::config::Config;
use mc_core::reducer;
use mc_schema::pane_view::{Attention, PaneView, Totals};
use mc_schema::raw_signals::{HerdrPaneSnapshot, PiSignals};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

pub fn run() {
    let config = Config::load();
    let herdr_socket = herdr::herdr_socket_path().unwrap_or_else(|e| {
        eprintln!("mc: {e}");
        std::process::exit(1);
    });

    // 1. Collect herdr panes
    eprintln!("Connecting to herdr at {} ...", herdr_socket.display());
    let herdr_panes = herdr::fetch_panes(&herdr_socket, Duration::from_secs(5))
        .unwrap_or_else(|e| {
            eprintln!("mc: failed to fetch panes from herdr: {e}");
            std::process::exit(1);
        });

    // 2. Collect pi session signals (join by agent_session_path)
    let pi_signals = collect_pi_signals(&herdr_panes);

    // 3. Collect project profiles (join by cwd)
    let projects = collect_projects(&herdr_panes);

    // 4. Reduce
    let (mut panes, totals) = reducer::reduce(&herdr_panes, &pi_signals, &projects, &config);

    // Sort by attention desc (Critical > High > Medium > Low > None), then pane_id
    panes.sort_by(|a, b| {
        b.flags
            .attention
            .cmp(&a.flags.attention)
            .then_with(|| a.pane_id.cmp(&b.pane_id))
    });

    // 5. Render
    render(&panes, &totals);
}

fn collect_pi_signals(herdr_panes: &[HerdrPaneSnapshot]) -> HashMap<PathBuf, PiSignals> {
    let mut signals = HashMap::new();

    for pane in herdr_panes {
        let Some(ref session_path) = pane.agent_session_path else {
            continue;
        };

        if signals.contains_key(session_path) {
            continue; // already parsed (shared session)
        }

        if !session_path.exists() {
            continue;
        }

        match pi::parse_session(session_path) {
            Ok(ps) => {
                signals.insert(session_path.clone(), ps);
            }
            Err(e) => {
                eprintln!(
                    "mc: warning: failed to parse session {}: {e}",
                    session_path.display()
                );
            }
        }
    }

    signals
}

fn collect_projects(herdr_panes: &[HerdrPaneSnapshot]) -> HashMap<PathBuf, mc_schema::project::ProjectProfile> {
    let mut profiles = HashMap::new();

    for pane in herdr_panes {
        if profiles.contains_key(&pane.cwd) {
            continue; // already scanned (shared cwd)
        }

        if pane.cwd.as_os_str().is_empty() || !pane.cwd.exists() {
            continue;
        }

        let profile = project::scan(&pane.cwd);
        profiles.insert(pane.cwd.clone(), profile);
    }

    profiles
}

// ── Terminal table rendering ──────────────────────────────────────────

fn render(panes: &[PaneView], totals: &Totals) {
    println!();
    println!(
        "┌─ Mission Control ────────────────────────────────────────────────────────────────┐"
    );
    println!(
        "│ {} panes  │  {} working  │  {} idle  │  {} blocked  │  ${:.2} total cost  │",
        totals.pane_count, totals.working_count, totals.idle_count,
        totals.blocked_count, totals.total_cost_usd
    );
    println!(
        "└───────────────────────────────────────────────────────────────────────────────────┘"
    );
    println!();

    if panes.is_empty() {
        println!("(no panes found)");
        return;
    }

    for pane in panes {
        render_pane(pane);
    }
}

fn render_pane(p: &PaneView) {
    let attention_icon = match p.flags.attention {
        Attention::Critical => "🔴 CRIT",
        Attention::High => "🟠 HIGH",
        Attention::Medium => "🟡 MED ",
        Attention::Low => "🟢 LOW ",
        Attention::None => "⚪     ",
    };

    let status = match p.agent_status {
        mc_schema::pane_view::AgentStatus::Idle => "💤 idle",
        mc_schema::pane_view::AgentStatus::Working => "⚙️  working",
        mc_schema::pane_view::AgentStatus::Blocked => "🚫 blocked",
        mc_schema::pane_view::AgentStatus::Done => "✅ done",
        mc_schema::pane_view::AgentStatus::Unknown => "❓ unknown",
    };

    let focus = if p.focused { "👁 " } else { "   " };

    let mut flags: Vec<String> = Vec::new();
    if p.flags.is_blocked {
        flags.push("BLOCKED".to_string());
    }
    if p.flags.is_runaway {
        flags.push("RUNAWAY".to_string());
    }
    if p.flags.awaiting_user_reply {
        flags.push("AWAITS-REPLY".to_string());
    }
    if let Some(idle_secs) = p.flags.idle_long_secs {
        flags.push(format!("IDLE-{}m", idle_secs / 60));
    }

    let project_name = p
        .project
        .as_ref()
        .and_then(|proj| proj.name.as_deref())
        .unwrap_or("-");

    let last_ask = p
        .last_user_message
        .as_deref()
        .map(|m| if m.len() > 60 { m[..57].to_string() + "..." } else { m.to_string() })
        .unwrap_or_else(|| "-".to_string());

    let current = &p.current.snippet;
    let current_display = if current.is_empty() {
        "-".to_string()
    } else if current.len() > 50 {
        current[..47].to_string() + "..."
    } else {
        current.clone()
    };

    let model = if p.vitals.model.is_empty() {
        "-"
    } else {
        &p.vitals.model
    };

    let tools_delta = if p.vitals_since_last_user.tool_calls > 0 {
        format!("+{} tools", p.vitals_since_last_user.tool_calls)
    } else {
        String::from("-")
    };

    // Pane header line
    println!(
        "{focus}{attention_icon}  {pane_id:<20} {status:<12}  {project_name:<16}  {model:<20}  ${cost:.4}  {tools}",
        pane_id = p.pane_id,
        project_name = project_name,
        model = model,
        cost = p.vitals.total_cost_usd,
        tools = tools_delta,
    );

    // Activity line
    println!(
        "      {activity:<60}",
        activity = format!("💬 {}  │  🏃 {}", last_ask, current_display),
    );

    // Flags line (only if flags are non-empty)
    if !flags.is_empty() {
        println!("      🚩 {}", flags.join(" | "));
    }

    println!();
}