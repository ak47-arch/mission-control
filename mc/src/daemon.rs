//! `mc serve` — the Mission Control daemon.
//!
//! Long-running background process: polls herdr, tails pi sessions,
//! scans projects, reduces signals into PaneViews, and serves the
//! JSON-RPC API over a Unix domain socket.

use mc_core::collector;
use mc_core::config::Config;
use mc_core::reducer;
use mc_core::state::StateStore;
use mc_core::transport::unix_socket;
use mc_schema::events::{EventEnvelope, EventKind};
use mc_schema::pane_view::{PaneView, Totals};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Default socket path for the daemon.
fn daemon_socket_path() -> PathBuf {
    dirs::runtime_dir()
        .or_else(|| dirs::cache_dir())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("mc.sock")
}

pub fn run() {
    let config = Config::load();
    let herdr_socket = collector::herdr::herdr_socket_path().unwrap_or_else(|e| {
        eprintln!("mc: {e}");
        std::process::exit(1);
    });

    let socket_path = daemon_socket_path();
    eprintln!("mc: starting daemon...");
    eprintln!("mc: herdr at {}", herdr_socket.display());
    eprintln!("mc: api socket at {}", socket_path.display());

    // Shared state
    let state = Arc::new(StateStore::new());
    let panes: Arc<std::sync::Mutex<Vec<PaneView>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let totals: Arc<std::sync::Mutex<Totals>> =
        Arc::new(std::sync::Mutex::new(Totals {
            pane_count: 0,
            working_count: 0,
            idle_count: 0,
            blocked_count: 0,
            total_cost_usd: 0.0,
            total_tool_calls: 0,
        }));

    // Spawn the collector + reducer loop
    {
        let panes = Arc::clone(&panes);
        let totals = Arc::clone(&totals);
        let state = Arc::clone(&state);

        std::thread::spawn(move || {
            collector_loop(herdr_socket, config, panes, totals, state);
        });
    }

    // Start the JSON-RPC server (blocks this thread)
    if let Err(e) = unix_socket::serve(state, panes, totals, socket_path) {
        eprintln!("mc: server error: {e}");
        std::process::exit(1);
    }
}

/// Poll-collect-reduce loop. Runs indefinitely.
fn collector_loop(
    herdr_socket: PathBuf,
    config: Config,
    panes: Arc<std::sync::Mutex<Vec<PaneView>>>,
    totals: Arc<std::sync::Mutex<Totals>>,
    state: Arc<StateStore>,
) {
    let poll_interval = Duration::from_secs(1);
    let mut prev_panes: Vec<PaneView> = Vec::new();

    loop {
        // 1. Collect herdr snapshots
        let herdr_panes = match collector::herdr::fetch_panes(&herdr_socket, Duration::from_secs(5)) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("mc: herdr poll error: {e}");
                std::thread::sleep(poll_interval);
                continue;
            }
        };

        // 2. Collect pi signals
        let pi_signals = collect_pi_signals(&herdr_panes);

        // 3. Collect project profiles
        let projects = collect_projects(&herdr_panes);

        // 4. Reduce
        let (new_panes, new_totals) =
            reducer::reduce(&herdr_panes, &pi_signals, &projects, &config);

        // 5. Diff & emit events
        emit_events(&prev_panes, &new_panes, new_totals, &state);
        prev_panes = new_panes.clone();

        // 6. Update shared state
        {
            let mut ps = panes.lock().unwrap();
            *ps = new_panes;
        }
        {
            let mut ts = totals.lock().unwrap();
            *ts = new_totals;
        }

        std::thread::sleep(poll_interval);
    }
}

fn collect_pi_signals(
    herdr_panes: &[mc_schema::raw_signals::HerdrPaneSnapshot],
) -> HashMap<PathBuf, mc_schema::raw_signals::PiSignals> {
    let mut signals = HashMap::new();
    for pane in herdr_panes {
        let Some(ref session_path) = pane.agent_session_path else {
            continue;
        };
        if signals.contains_key(session_path) || !session_path.exists() {
            continue;
        }
        if let Ok(ps) = collector::pi::parse_session(session_path) {
            signals.insert(session_path.clone(), ps);
        }
    }
    signals
}

fn collect_projects(
    herdr_panes: &[mc_schema::raw_signals::HerdrPaneSnapshot],
) -> HashMap<PathBuf, mc_schema::project::ProjectProfile> {
    let mut profiles = HashMap::new();
    for pane in herdr_panes {
        if profiles.contains_key(&pane.cwd) || !pane.cwd.exists() {
            continue;
        }
        let profile = collector::project::scan(&pane.cwd);
        profiles.insert(pane.cwd.clone(), profile);
    }
    profiles
}

/// Compare old and new PaneViews, emit appropriate events.
fn emit_events(
    prev: &[PaneView],
    next: &[PaneView],
    new_totals: Totals,
    state: &StateStore,
) {
    let now = chrono::Utc::now();

    // Detect added/removed panes
    let prev_ids: std::collections::HashSet<_> = prev.iter().map(|p| &p.pane_id).collect();
    let next_ids: std::collections::HashSet<_> = next.iter().map(|p| &p.pane_id).collect();

    for p in next {
        if !prev_ids.contains(&p.pane_id) {
            state.push(EventEnvelope {
                protocol_version: mc_schema::events::PROTOCOL_VERSION,
                sequence: 0,
                timestamp: now,
                event: EventKind::PaneAdded(p.clone()),
            });
        }
    }
    for p in prev {
        if !next_ids.contains(&p.pane_id) {
            state.push(EventEnvelope {
                protocol_version: mc_schema::events::PROTOCOL_VERSION,
                sequence: 0,
                timestamp: now,
                event: EventKind::PaneRemoved {
                    pane_id: p.pane_id.clone(),
                },
            });
        }
    }

    // Detect status/flag changes
    let prev_map: HashMap<&str, &PaneView> = prev.iter().map(|p| (p.pane_id.as_str(), p)).collect();
    for p in next {
        if let Some(old) = prev_map.get(p.pane_id.as_str()) {
            if old.flags.attention != p.flags.attention {
                state.push(EventEnvelope {
                    protocol_version: mc_schema::events::PROTOCOL_VERSION,
                    sequence: 0,
                    timestamp: now,
                    event: EventKind::AttentionChanged {
                        pane_id: p.pane_id.clone(),
                        from: Some(old.flags.attention),
                        to: p.flags.attention,
                    },
                });
            }
        }
    }

    // Always emit totals
    state.push(EventEnvelope {
        protocol_version: mc_schema::events::PROTOCOL_VERSION,
        sequence: 0,
        timestamp: now,
        event: EventKind::TotalsChanged {
            totals: new_totals,
        },
    });
}