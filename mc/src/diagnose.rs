//! `mc diagnose` — inspect session-to-pane mapping and orphaned sessions.
//!
//! Connects to the daemon, fetches the pane list, scans the pi session
//! directory, and shows a report of what's mapped vs orphaned.

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::GenericFilePath;
use mc_schema::events::SnapshotResponse;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default socket path (same as daemon.rs)
fn daemon_socket_path() -> PathBuf {
    dirs::runtime_dir()
        .or_else(|| dirs::cache_dir())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("mc.sock")
}

/// Pi session directory
fn pi_session_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".pi")
        .join("agent")
        .join("sessions")
}

pub fn run() {
    let socket_path = daemon_socket_path();
    let sessions_dir = pi_session_dir();

    if !socket_path.exists() {
        eprintln!("mc: daemon socket not found at {}", socket_path.display());
        eprintln!("mc: is `mc serve` running?");
        std::process::exit(1);
    }

    // Fetch snapshot from daemon
    let snapshot = match daemon_call(&socket_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mc: daemon error: {e}");
            std::process::exit(1);
        }
    };

    // Collect all session files from session dirs
    let mut all_sessions: Vec<SessionEntry> = Vec::new();
    scan_session_dirs(&sessions_dir, &mut all_sessions);

    // Collect mapped session paths from panes
    let mut mapped_paths: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut panes_with_sessions: Vec<&PaneInfo> = Vec::new();
    let mut panes_without_sessions: Vec<&PaneInfo> = Vec::new();

    let panes: Vec<PaneInfo> = snapshot
        .panes
        .iter()
        .map(|p| PaneInfo {
            pane_id: p.pane_id.clone(),
            workspace_name: p.workspace_name.clone().unwrap_or_default(),
            tab_name: p.tab_name.clone().unwrap_or_default(),
            agent: p.agent.clone().unwrap_or_default(),
            agent_status: format!("{:?}", p.agent_status).to_lowercase(),
            session_path: p.session_path.clone(),
            total_cost: p.vitals.total_cost_usd,
            total_tool_calls: p.vitals.total_tool_calls,
            arc_turns: p.arc.len(),
        })
        .collect();

    for pane in &panes {
        if let Some(ref path) = pane.session_path {
            mapped_paths.insert(path.clone());
            panes_with_sessions.push(pane);
        } else {
            panes_without_sessions.push(pane);
        }
    }

    // Separate orphaned and mapped sessions
    let mut orphaned: Vec<&SessionEntry> = Vec::new();
    let mut mapped_sessions: Vec<&SessionEntry> = Vec::new();

    for session in &all_sessions {
        if mapped_paths.contains(&session.path) {
            mapped_sessions.push(session);
        } else {
            orphaned.push(session);
        }
    }

    // Sort orphaned by mtime descending
    orphaned.sort_by(|a, b| b.mtime.cmp(&a.mtime));

    // ── Report ────────────────────────────────────────────────

    println!("═══ mc diagnose — session mapping report ═══");
    println!();

    // Summary
    println!("Summary:");
    println!("  Panes:             {:3}", panes.len());
    println!("  With session:      {:3}", panes_with_sessions.len());
    println!("  Without session:   {:3}", panes_without_sessions.len());
    println!(
        "  Session files:     {:3}",
        all_sessions.len()
    );
    println!("  Mapped to panes:   {:3}", mapped_sessions.len());
    println!("  Orphaned on disk:  {:3}", orphaned.len());
    println!(
        "  Total cost:        ${:.2}",
        panes.iter().map(|p| p.total_cost).sum::<f64>()
    );
    println!();

    // Panes with sessions
    println!("═══ Panes WITH session ──────────────────────────────");
    println!(
        "{:<40} {:<22}  {:>8}  {:>5}  {:>5}",
        "Pane", "Session", "Cost", "Turns", "Tools"
    );
    for p in &panes_with_sessions {
        let label = if !p.workspace_name.is_empty() && !p.tab_name.is_empty() {
            format!("{} / {}", p.workspace_name, p.tab_name)
        } else {
            p.pane_id.clone()
        };
        let session_id = p
            .session_path
            .as_ref()
            .and_then(|sp| sp.file_stem())
            .and_then(|s| s.to_str())
            .map(|s| truncate_mid(s, 22))
            .unwrap_or_default();
        println!(
            "{:<40} {:<22}  ${:>7.2}  {:>5}  {:>5}",
            label, session_id, p.total_cost, p.arc_turns, p.total_tool_calls
        );
    }
    println!();

    // Panes without sessions
    if !panes_without_sessions.is_empty() {
        println!("═══ Panes WITHOUT session ────────────────────────────");
        for p in &panes_without_sessions {
            let label = if !p.workspace_name.is_empty() && !p.tab_name.is_empty() {
                format!("{} / {}", p.workspace_name, p.tab_name)
            } else {
                p.pane_id.clone()
            };
            println!("  {}  (status: {})", label, p.agent_status);
        }
        println!();
    }

    // Orphaned sessions (show recent ones that might match the 4 panes)
    if !orphaned.is_empty() {
        println!("═══ Orphaned session files (top 20 by recency) ────────");
        println!(
            "{:<30} {:>10}  {:>12}  {:<20}",
            "Session ID", "Size", "Last Modified", "CWD"
        );
        for s in orphaned.iter().take(20) {
            println!(
                "{:<30} {:>8}KB  {:>12}  {:<20}",
                truncate(s.session_id(), 28),
                s.size_kb,
                s.mtime_str(),
                truncate(&s.cwd(), 18)
            );
        }
        if orphaned.len() > 20 {
            println!("  ... and {} more", orphaned.len() - 20);
        }
        println!();
    }

    // Recommendations
    println!("═══ Recommendations ──────────────────────────────────");
    if panes_without_sessions.len() > 0 {
        println!(
            "  • {} panes have no session associated.",
            panes_without_sessions.len()
        );
        println!("    The collector needs a fallback scan of the orphaned session files");
        println!("    matching by cwd + recency to recover their data.");
    }
    if orphaned.len() > 0 {
        println!(
            "  • {} orphaned session files on disk are not linked to any pane.",
            orphaned.len()
        );
        println!("    Add orphaned-session fallback in mc-core/src/collector/herdr.rs:");
        println!("    when agent_session_path is None, scan session files matching cwd.");
    }
    println!(
        "  • Total tracked cost: ${:.2} across {} panes.",
        panes.iter().map(|p| p.total_cost).sum::<f64>(),
        panes.len()
    );
    println!("  • Total orphaned sessions could add more cost data if recovered.");
    println!();
}

// ── Helpers ────────────────────────────────────────────────────────

struct PaneInfo {
    pane_id: String,
    workspace_name: String,
    tab_name: String,
    agent: String,
    agent_status: String,
    session_path: Option<PathBuf>,
    total_cost: f64,
    total_tool_calls: u32,
    arc_turns: usize,
}

struct SessionEntry {
    path: PathBuf,
    size_kb: u64,
    mtime: i64,
}

impl SessionEntry {
    fn session_id(&self) -> &str {
        self.path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
    }

    fn cwd(&self) -> String {
        // Read first line from session file to get cwd
        if let Ok(content) = std::fs::read_to_string(&self.path) {
            if let Some(line) = content.lines().next() {
                if let Ok(header) = serde_json::from_str::<serde_json::Value>(line) {
                    if let Some(cwd) = header.get("cwd").and_then(|c| c.as_str()) {
                        return cwd
                            .rsplit('/')
                            .next()
                            .unwrap_or(cwd)
                            .to_string();
                    }
                }
            }
        }
        String::new()
    }

    fn mtime_str(&self) -> String {
        let dt = chrono::DateTime::from_timestamp(self.mtime, 0).unwrap_or_default();
        dt.format("%b %d %H:%M").to_string()
    }
}

fn scan_session_dirs(dir: &Path, out: &mut Vec<SessionEntry>) {
    let dir_entries = match std::fs::read_dir(dir) {
        Ok(d) => d,
        Err(_) => return,
    };

    for entry in dir_entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Recurse into per-cwd session subdirectories
            scan_session_dirs(&path, out);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            _ => continue,
        };
        out.push(SessionEntry {
            size_kb: (meta.len() + 512) / 1024,
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            path,
        });
    }
}

fn daemon_call(socket_path: &Path) -> Result<SnapshotResponse, String> {
    let mut stream = interprocess::local_socket::Stream::connect(
        socket_path
            .to_fs_name::<GenericFilePath>()
            .map_err(|e| format!("socket error: {e}"))?,
    )
    .map_err(|e| format!("connect error: {e}"))?;

    stream
        .set_send_timeout(Some(Duration::from_secs(3)))
        .unwrap_or(());
    stream
        .set_recv_timeout(Some(Duration::from_secs(3)))
        .unwrap_or(());

    let request = serde_json::json!({
        "id": "diagnose",
        "method": "mc.snapshot",
        "params": null,
    });

    stream
        .write_all(serde_json::to_string(&request).unwrap().as_bytes())
        .map_err(|e| format!("write error: {e}"))?;
    stream
        .write_all(b"\n")
        .map_err(|e| format!("write error: {e}"))?;
    stream
        .flush()
        .map_err(|e| format!("flush error: {e}"))?;

    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("read error: {e}"))?;

    let resp: serde_json::Value =
        serde_json::from_str(&line).map_err(|e| format!("parse error: {e}"))?;

    let result = resp
        .get("result")
        .ok_or_else(|| "no result in response".to_string())?;

    serde_json::from_value(result.clone()).map_err(|e| format!("snapshot parse: {e}"))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

fn truncate_mid(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max < 5 {
        s[..max].to_string()
    } else {
        let left = (max - 3) / 2;
        let right = max - 3 - left;
        format!("{}…{}", &s[..left], &s[s.len() - right..])
    }
}