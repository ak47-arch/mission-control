//! Unix-socket JSON-RPC server transport (§8.1).
//!
//! Binds to a Unix domain socket, serves `mc.snapshot`, `mc.pane.get`,
//! `mc.needs_attention`, `mc.totals`, `events.subscribe`, and
//! `events.current_sequence`. Identical wire pattern to herdr's API.

use crate::state::StateStore;
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, ListenerOptions};
use interprocess::TryClone;
use mc_schema::events::{
    CurrentSequenceResponse, McMethod, McRequest, NeedsAttentionResponse,
    PaneGetResponse, SnapshotResponse, TotalsResponse,
};
use mc_schema::pane_view::Attention;
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::path::{PathBuf};
use std::sync::Arc;

/// Start the JSON-RPC server on the given socket path.
/// Blocks the calling thread — run in a spawned task.
pub fn serve(
    state: Arc<StateStore>,
    panes: Arc<std::sync::Mutex<Vec<mc_schema::pane_view::PaneView>>>,
    totals: Arc<std::sync::Mutex<mc_schema::pane_view::Totals>>,
    socket_path: PathBuf,
) -> std::io::Result<()> {
    let path_str = socket_path.display().to_string();
    let _ = std::fs::remove_file(&socket_path);

    let name = socket_path.to_fs_name::<GenericFilePath>()?;
    let listener = ListenerOptions::new()
        .name(name)
        .create_sync()?;

    eprintln!("mc: daemon listening on {path_str}");

    for stream in listener.incoming() {
        match stream {
            Ok(mut conn) => {
                let state = Arc::clone(&state);
                let panes = Arc::clone(&panes);
                let totals = Arc::clone(&totals);

                std::thread::spawn(move || {
                    handle_connection(&mut conn, &state, &panes, &totals);
                });
            }
            Err(e) => {
                eprintln!("mc: connection error: {e}");
            }
        }
    }

    Ok(())
}

/// Handle one client connection — reads newline-delimited JSON requests,
/// writes JSON responses.
fn handle_connection(
    conn: &mut interprocess::local_socket::Stream,
    state: &StateStore,
    panes: &Arc<std::sync::Mutex<Vec<mc_schema::pane_view::PaneView>>>,
    totals: &Arc<std::sync::Mutex<mc_schema::pane_view::Totals>>,
) {
    // Clone the stream so we can read independently.
    let read_stream = match conn.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = BufReader::new(read_stream);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {
                let response = handle_request(&line, state, panes, totals);
                let _ = conn.write_all(response.as_bytes());
                let _ = conn.write_all(b"\n");
                let _ = conn.flush();
            }
            Err(_) => return,
        }
    }
}

fn handle_request(
    line: &str,
    state: &StateStore,
    panes: &Arc<std::sync::Mutex<Vec<mc_schema::pane_view::PaneView>>>,
    totals: &Arc<std::sync::Mutex<mc_schema::pane_view::Totals>>,
) -> String {
    let request: McRequest = match serde_json::from_str(line.trim()) {
        Ok(r) => r,
        Err(e) => {
            return json_error(&request_id_from_line(line), -32700, &format!("Parse error: {e}"));
        }
    };

    let id = request.id.clone();

    match request.method {
        McMethod::Snapshot => {
            let ps = panes.lock().unwrap().clone();
            let ts = totals.lock().unwrap();
            let seq = state.current_sequence();
            let resp = SnapshotResponse {
                panes: ps,
                totals: *ts,
                sequence: seq,
            };
            json_response(&id, &resp)
        }
        McMethod::PaneGet { pane_id } => {
            let ps = panes.lock().unwrap();
            match ps.iter().find(|p| p.pane_id == pane_id) {
                Some(pane) => {
                    json_response(&id, &PaneGetResponse { pane: pane.clone() })
                }
                None => json_error(&id, -32000, &format!("pane not found: {pane_id}")),
            }
        }
        McMethod::NeedsAttention => {
            let ps = panes.lock().unwrap();
            let mut results: Vec<_> = ps
                .iter()
                .filter(|p| p.flags.attention != Attention::None)
                .cloned()
                .collect();
            results.sort_by(|a, b| b.flags.attention.cmp(&a.flags.attention));
            json_response(&id, &NeedsAttentionResponse { panes: results })
        }
        McMethod::Totals => {
            let ts = totals.lock().unwrap();
            json_response(&id, &TotalsResponse { totals: *ts })
        }
        McMethod::EventsSubscribe { after_seq, kinds } => {
            // For TUI simplicity, return a snapshot-like response and then
            // immediately close. The TUI polls rather than streaming.
            // Full SSE/streaming support is Phase 3.
            let events = state.events_after(after_seq);
            let filtered: Vec<_> = if kinds.is_empty() {
                events
            } else {
                events
                    .into_iter()
                    .filter(|(_, ev)| {
                        let kind_str = match &ev.event {
                            mc_schema::events::EventKind::PaneAdded(_) => "pane_added",
                            mc_schema::events::EventKind::PaneRemoved { .. } => "pane_removed",
                            mc_schema::events::EventKind::PaneViewPatch { .. } => "pane_view_patch",
                            mc_schema::events::EventKind::AttentionChanged { .. } => "attention_changed",
                            mc_schema::events::EventKind::TotalsChanged { .. } => "totals_changed",
                        };
                        kinds.iter().any(|k| k == kind_str)
                    })
                    .collect()
            };
            json_response(&id, &filtered)
        }
        McMethod::EventsCurrentSequence => {
            let seq = state.current_sequence();
            json_response(&id, &CurrentSequenceResponse { sequence: seq })
        }
    }
}

fn request_id_from_line(line: &str) -> String {
    serde_json::from_str::<serde_json::Value>(line.trim())
        .ok()
        .and_then(|v| v.get("id").and_then(|id| id.as_str()).map(String::from))
        .unwrap_or_else(|| "unknown".to_string())
}

fn json_response(id: &str, result: &impl Serialize) -> String {
    serde_json::json!({
        "id": id,
        "result": result,
    })
    .to_string()
}

fn json_error(id: &str, code: i32, message: &str) -> String {
    serde_json::json!({
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}