//! `mc web` — HTTP + SSE bridge for the browser dashboard.
//!
//! A thin adapter that connects to the daemon's Unix socket, polls
//! `mc.snapshot`, and serves JSON + SSE to browsers. The adapter is
//! the only component that knows HTTP exists (PRD R5).

use axum::{
    extract::State,
    response::{
        sse::{Event, Sse},
        Html, Json,
    },
    routing::get,
    Router,
};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::GenericFilePath;
use mc_schema::events::SnapshotResponse;
use mc_schema::pane_view::PaneView;
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

const INDEX_HTML: &str = include_str!("../../mc-web/index.html");

/// Daemon socket path (same as daemon.rs and tui.rs)
fn daemon_socket_path() -> PathBuf {
    dirs::runtime_dir()
        .or_else(|| dirs::cache_dir())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("mc.sock")
}

/// The shared state for the web server.
struct WebState {
    panes: tokio::sync::RwLock<Vec<PaneView>>,
    totals: tokio::sync::RwLock<mc_schema::pane_view::Totals>,
    sequence: tokio::sync::RwLock<u64>,
    tx: broadcast::Sender<String>,
}

pub async fn run(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let socket_path = daemon_socket_path();

    // Test connection to daemon
    match poll_daemon(&socket_path).await {
        Ok(_) => eprintln!("mc: connected to daemon at {}", socket_path.display()),
        Err(e) => {
            eprintln!("mc: cannot connect to daemon at {}: {e}", socket_path.display());
            eprintln!("mc: is `mc serve` running?");
            std::process::exit(1);
        }
    }

    let (tx, _) = broadcast::channel(64);

    let state = Arc::new(WebState {
        panes: tokio::sync::RwLock::new(Vec::new()),
        totals: tokio::sync::RwLock::new(mc_schema::pane_view::Totals {
            pane_count: 0,
            working_count: 0,
            idle_count: 0,
            blocked_count: 0,
            total_cost_usd: 0.0,
            total_tool_calls: 0,
        }),
        sequence: tokio::sync::RwLock::new(0),
        tx,
    });

    // Spawn poll loop
    {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            poll_loop(socket_path, state).await;
        });
    }

    // Build axum router
    let app_state = Arc::clone(&state);
    let app = Router::new()
        .route("/", get(index_html))
        .route("/api/snapshot", get(api_snapshot))
        .route("/api/pane/{pane_id}", get(api_pane_get))
        .route("/api/needs-attention", get(api_needs_attention))
        .route("/api/totals", get(api_totals))
        .route("/api/events", get(api_events))
        .layer(CorsLayer::permissive())
        .with_state(app_state);

    let addr = format!("0.0.0.0:{port}");
    eprintln!("mc: web dashboard at http://localhost:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

// ── Routes ───────────────────────────────────────────────────────────

async fn index_html() -> Html<&'static str> {
    Html(INDEX_HTML)
}

#[derive(Serialize)]
struct SnapshotJson {
    panes: Vec<PaneView>,
    totals: mc_schema::pane_view::Totals,
    sequence: u64,
}

async fn api_snapshot(State(state): State<Arc<WebState>>) -> Json<SnapshotJson> {
    let panes = state.panes.read().await.clone();
    let totals = *state.totals.read().await;
    let seq = *state.sequence.read().await;
    Json(SnapshotJson {
        panes,
        totals,
        sequence: seq,
    })
}

#[derive(Serialize)]
struct PaneJson {
    pane: PaneView,
}

async fn api_pane_get(
    State(state): State<Arc<WebState>>,
    axum::extract::Path(pane_id): axum::extract::Path<String>,
) -> Result<Json<PaneJson>, (axum::http::StatusCode, String)> {
    let panes = state.panes.read().await;
    panes
        .iter()
        .find(|p| p.pane_id == pane_id)
        .cloned()
        .map(|pane| Json(PaneJson { pane }))
        .ok_or_else(|| {
            (
                axum::http::StatusCode::NOT_FOUND,
                format!("pane not found: {pane_id}"),
            )
        })
}

async fn api_needs_attention(State(state): State<Arc<WebState>>) -> Json<Vec<PaneView>> {
    use mc_schema::pane_view::Attention;
    let mut panes: Vec<PaneView> = state
        .panes
        .read()
        .await
        .iter()
        .filter(|p| p.flags.attention != Attention::None)
        .cloned()
        .collect();
    panes.sort_by(|a, b| b.flags.attention.cmp(&a.flags.attention));
    Json(panes)
}

#[derive(Serialize)]
struct TotalsJson {
    totals: mc_schema::pane_view::Totals,
}

async fn api_totals(State(state): State<Arc<WebState>>) -> Json<TotalsJson> {
    let totals = *state.totals.read().await;
    Json(TotalsJson { totals })
}

async fn api_events(
    State(state): State<Arc<WebState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let mut rx = state.tx.subscribe();
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(msg) => yield Ok(Event::default().data(msg)),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream)
}

// ── Daemon poll ───────────────────────────────────────────────────────

async fn poll_daemon(socket_path: &PathBuf) -> Result<SnapshotResponse, String> {
    let result = daemon_call(socket_path, "mc.snapshot", serde_json::Value::Null)?;
    serde_json::from_value(result).map_err(|e| format!("parse snapshot: {e}"))
}

async fn poll_loop(socket_path: PathBuf, state: Arc<WebState>) {
    let interval = Duration::from_secs(1);
    loop {
        match poll_daemon(&socket_path).await {
            Ok(snapshot) => {
                {
                    let mut ps = state.panes.write().await;
                    *ps = snapshot.panes;
                }
                {
                    let mut ts = state.totals.write().await;
                    *ts = snapshot.totals;
                }
                {
                    let mut seq = state.sequence.write().await;
                    *seq = snapshot.sequence;
                }
                // Notify SSE listeners
                let _ = state.tx.send("refresh".to_string());
            }
            Err(e) => {
                eprintln!("mc: daemon poll error: {e}");
            }
        }
        tokio::time::sleep(interval).await;
    }
}

fn daemon_call(
    socket_path: &PathBuf,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut stream = interprocess::local_socket::Stream::connect(
        socket_path
            .clone()
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

    let params_value = if params.is_null() {
        serde_json::Value::Null
    } else {
        params
    };

    let request = serde_json::json!({
        "id": "web",
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