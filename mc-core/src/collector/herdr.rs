//! Herdr collector — polls herdr's JSON-RPC API over a Unix socket.
//!
//! Connects to herdr via `$HERDR_SOCKET_PATH`, calls `pane.list`,
//! and emits `HerdrPaneSnapshot` structs for the reducer.

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::GenericFilePath;
use mc_schema::pane_view::AgentStatus;
use mc_schema::raw_signals::HerdrPaneSnapshot;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Error from the herdr collector.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("socket path not set (set $HERDR_SOCKET_PATH or config)")]
    NoSocketPath,
    #[error("herdr I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("herdr JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("herdr returned error: {0}")]
    ApiError(String),
    #[error("unexpected herdr response shape")]
    UnexpectedResponse,
}

/// Raw response from herdr's `pane.list` method.
/// herdr wraps the result in `{"id":"...","result":{"panes":[...]}}`.
#[derive(Debug, Deserialize)]
struct WorkspaceListResponse {
    result: WorkspaceListResult,
}

#[derive(Debug, Deserialize)]
struct WorkspaceListResult {
    workspaces: Vec<WorkspaceInfo>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceInfo {
    workspace_id: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TabListResponse {
    result: TabListResult,
}

#[derive(Debug, Deserialize)]
struct TabListResult {
    tabs: Vec<TabInfo>,
}

#[derive(Debug, Deserialize)]
struct TabInfo {
    tab_id: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HerdrResponse {
    result: PaneListResult,
}

#[derive(Debug, Deserialize)]
struct PaneListResult {
    panes: Vec<HerdrPaneInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
struct HerdrPaneInfo {
    pane_id: String,
    workspace_id: String,
    tab_id: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    agent_status: HerdrAgentStatus,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    agent_session: Option<HerdrAgentSession>,
    #[serde(default)]
    custom_status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HerdrAgentSession {
    value: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum HerdrAgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    #[default]
    Unknown,
}

impl From<HerdrAgentStatus> for AgentStatus {
    fn from(s: HerdrAgentStatus) -> Self {
        match s {
            HerdrAgentStatus::Idle => AgentStatus::Idle,
            HerdrAgentStatus::Working => AgentStatus::Working,
            HerdrAgentStatus::Blocked => AgentStatus::Blocked,
            HerdrAgentStatus::Done => AgentStatus::Done,
            HerdrAgentStatus::Unknown => AgentStatus::Unknown,
        }
    }
}

/// Resolve the herdr socket path from environment or config.
pub fn herdr_socket_path() -> Result<PathBuf, Error> {
    // 1. Check $HERDR_SOCKET_PATH (set by herdr in every pane)
    if let Ok(path) = std::env::var("HERDR_SOCKET_PATH") {
        return Ok(PathBuf::from(path));
    }
    // 2. Check config override
    let config = crate::config::Config::load();
    if let Some(path) = config.herdr_socket {
        return Ok(path);
    }
    // 3. Fallback default
    Ok(dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("herdr")
        .join("herdr.sock"))
}

/// Fetch all panes from herdr, enriched with workspace/tab labels.
pub fn fetch_panes(socket_path: &Path, timeout: Duration) -> Result<Vec<HerdrPaneSnapshot>, Error> {
    let mut stream = connect(&socket_path, timeout)?;

    // Fetch workspace labels
    send_request(&mut stream, "mc-ws", "workspace.list")?;
    let ws_response: WorkspaceListResponse = read_response(&mut stream)?;
    let ws_labels: HashMap<String, String> = ws_response
        .result
        .workspaces
        .into_iter()
        .filter_map(|ws| ws.label.map(|l| (ws.workspace_id, l)))
        .collect();

    // Fetch tab labels
    let mut stream = connect(&socket_path, timeout)?;
    send_request(&mut stream, "mc-tab", "tab.list")?;
    let tab_response: TabListResponse = read_response(&mut stream)?;
    let tab_labels: HashMap<String, String> = tab_response
        .result
        .tabs
        .into_iter()
        .filter_map(|t| t.label.map(|l| (t.tab_id, l)))
        .collect();

    // Fetch panes
    let mut stream = connect(&socket_path, timeout)?;
    send_request(&mut stream, "mc-status", "pane.list")?;
    let response: HerdrResponse = read_response(&mut stream)?;

    let now = chrono::Utc::now();
    let panes = response
        .result
        .panes
        .into_iter()
        .map(|p| {
            let cwd = p.cwd.unwrap_or_else(|| ".".into());
            let agent_session_path = p.agent_session.map(|s| PathBuf::from(s.value));

            // Fallback: if pi is running but herdr lost the session path,
            // scan the session directory by cwd and pick the newest file.
            let agent_session_path = if agent_session_path.is_none()
                && p.agent.as_deref() == Some("pi")
                && cwd != "."
            {
                find_orphaned_session_path(&cwd)
                    .or_else(|| find_orphaned_session_path_by_header(&cwd))
            } else {
                agent_session_path
            };

            HerdrPaneSnapshot {
                workspace_id: p.workspace_id.clone(),
                workspace_label: ws_labels.get(&p.workspace_id).cloned(),
                tab_id: p.tab_id.clone(),
                tab_label: tab_labels.get(&p.tab_id).cloned(),
                pane_id: p.pane_id,
                agent: p.agent,
                agent_status: p.agent_status.into(),
                focused: p.focused,
                cwd: cwd.into(),
                agent_session_path,
                custom_status: p.custom_status,
                captured_at: now,
            }
        })
        .collect();

    Ok(panes)
}

/// Connect to herdr's Unix socket with a timeout.
fn connect(path: &Path, timeout: Duration) -> std::io::Result<interprocess::local_socket::Stream> {
    let name = path.to_fs_name::<GenericFilePath>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let stream = interprocess::local_socket::Stream::connect(name)?;
    stream.set_send_timeout(Some(timeout))?;
    stream.set_recv_timeout(Some(timeout))?;
    Ok(stream)
}

/// Send a JSON-RPC request to herdr (newline-delimited JSON).
fn send_request(stream: &mut interprocess::local_socket::Stream, id: &str, method: &str) -> Result<(), Error> {
    let request = serde_json::json!({
        "id": id,
        "method": method,
        "params": {},
    });
    stream.write_all(serde_json::to_string(&request)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

/// Read a single newline-delimited JSON response from herdr.
fn read_response<T: serde::de::DeserializeOwned>(
    stream: &mut interprocess::local_socket::Stream,
) -> Result<T, Error> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = reader.read_line(&mut line)?;
    if read == 0 || line.trim().is_empty() {
        return Err(Error::UnexpectedResponse);
    }
    serde_json::from_str(&line).map_err(Error::Json)
}

// ── Pi session directory helpers ──────────────────────────────────────────

/// Pi's session directory (default: ~/.pi/agent/sessions/).
fn pi_sessions_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".pi")
        .join("agent")
        .join("sessions")
}

/// Convert a cwd path to pi's session directory slug.
/// E.g. `/home/anupam/Desktop/workspace` → `--home-anupam-Desktop-workspace--`
fn cwd_to_slug(cwd: &str) -> String {
    let slug = cwd.trim_start_matches('/').replace('/', "-");
    if slug.is_empty() {
        return "--root--".to_string();
    }
    format!("--{}--", slug)
}

/// Find the newest `.jsonl` session file in the directory for the given cwd.
/// Matches by directory slug derived from the cwd path, then verifies the
/// session header `cwd` field matches.
pub fn find_orphaned_session_path(cwd: &str) -> Option<PathBuf> {
    let session_dir = pi_sessions_dir().join(cwd_to_slug(cwd));
    let candidate = find_newest_jsonl(&session_dir)?;

    // Verify the session header cwd matches
    let content = fs::read_to_string(&candidate).ok()?;
    let first_line = content.lines().next()?;
    let val: Value = serde_json::from_str(first_line).ok()?;
    if val.get("cwd").and_then(|v| v.as_str()) == Some(cwd) {
        Some(candidate)
    } else {
        None
    }
}

/// Fallback: scan ALL session directories for a session whose header `cwd`
/// matches the given cwd. This catches cases where the slug algorithm doesn't
/// exactly match pi's internal slug generation.
pub fn find_orphaned_session_path_by_header(cwd: &str) -> Option<PathBuf> {
    let sessions_dir = pi_sessions_dir();
    let dir = match fs::read_dir(&sessions_dir) {
        Ok(d) => d,
        Err(_) => return None,
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in dir.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(newest) = find_newest_jsonl(&path) {
            // Read the first line to check the cwd header
            if let Ok(content) = fs::read_to_string(&newest) {
                if let Some(first_line) = content.lines().next() {
                    if let Ok(val) = serde_json::from_str::<Value>(first_line) {
                        if val.get("cwd").and_then(|v| v.as_str()) == Some(cwd) {
                            candidates.push(newest);
                        }
                    }
                }
            }
        }
    }

    // Pick the newest by modification time
    candidates.sort_by(|a, b| {
        let mta = fs::metadata(a).and_then(|m| m.modified()).ok();
        let mtb = fs::metadata(b).and_then(|m| m.modified()).ok();
        mta.cmp(&mtb).reverse()
    });

    candidates.into_iter().next()
}

/// Find the newest `.jsonl` file in a directory.
fn find_newest_jsonl(dir: &Path) -> Option<PathBuf> {
    let read_dir = match fs::read_dir(dir) {
        Ok(d) => d,
        Err(_) => return None,
    };

    let mut newest: Option<PathBuf> = None;
    let mut newest_mtime: Option<std::time::SystemTime> = None;

    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(meta) = fs::metadata(&path) {
            if meta.len() == 0 {
                continue; // skip files being written to
            }
            if let Ok(mtime) = meta.modified() {
                if newest_mtime.map_or(true, |old| mtime > old) {
                    newest = Some(path);
                    newest_mtime = Some(mtime);
                }
            }
        }
    }

    newest
}