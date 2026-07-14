//! Reducer — pure function merging RawSignals into PaneViews (§9).
//!
//! Takes the three signal maps, joins them per pane, computes Flags via
//! the rules module, and emits `Vec<PaneView>` + `Totals`.

use mc_schema::pane_view::{
    ActivityKind, AgentStatus, Attention, CurrentActivity, Flags, PaneView, Totals, TurnEnd,
    TurnSummary, Vitals, VitalsDelta,
};
use mc_schema::project::ProjectProfile;
use mc_schema::raw_signals::{HerdrPaneSnapshot, LeafKind, LeafSummary, PiSignals};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::Config;

/// Reduce signals into PaneViews.
pub fn reduce(
    herdr_panes: &[HerdrPaneSnapshot],
    pi_signals: &HashMap<PathBuf, PiSignals>,
    projects: &HashMap<PathBuf, ProjectProfile>,
    config: &Config,
) -> (Vec<PaneView>, Totals) {
    let now = chrono::Utc::now();

    let panes: Vec<PaneView> = herdr_panes
        .iter()
        .map(|h| build_pane_view(h, pi_signals, projects, config, now))
        .collect();

    let totals = compute_totals(&panes);

    (panes, totals)
}

fn build_pane_view(
    h: &HerdrPaneSnapshot,
    pi_signals: &HashMap<PathBuf, PiSignals>,
    projects: &HashMap<PathBuf, ProjectProfile>,
    config: &Config,
    now: chrono::DateTime<chrono::Utc>,
) -> PaneView {
    // Join with pi signals by session path
    let pi = h
        .agent_session_path
        .as_ref()
        .and_then(|sp| pi_signals.get(sp));

    // Join with project by cwd
    let project = projects.get(&h.cwd).map(|p| p.to_view());

    // Build the current activity from pi signals
    let current = pi
        .map(|p| current_activity(&p.deepest_leaf_since_last_user))
        .unwrap_or(CurrentActivity {
            kind: ActivityKind::UserPending,
            tool_name: None,
            snippet: String::new(),
            started_at: now,
        });

    // Build vitals
    let vitals = pi
        .map(|p| Vitals {
            total_turns: p.total_turns,
            total_tool_calls: p.total_tool_calls,
            total_cost_usd: p.total_cost_usd,
            model: p
                .model
                .last()
                .map(|m| m.model_id.clone())
                .unwrap_or_default(),
            thinking_level: p.thinking_level.map(|tl| format!("{tl:?}").to_lowercase()),
            session_age_secs: (now - p.started_at).num_seconds().max(0) as u64,
        })
        .unwrap_or(Vitals {
            total_turns: 0,
            total_tool_calls: 0,
            total_cost_usd: 0.0,
            model: String::new(),
            thinking_level: None,
            session_age_secs: 0,
        });

    let vitals_delta = pi
        .map(|p| VitalsDelta {
            tool_calls: p.tool_calls_since_last_user,
            cost_usd: p.cost_since_last_user,
            errors: p.error_since_last_user.is_some() as u32,
        })
        .unwrap_or(VitalsDelta {
            tool_calls: 0,
            cost_usd: 0.0,
            errors: 0,
        });

    // Build conversation arc
    let arc = pi
        .map(|p| build_arc(p, config.arc_turns))
        .unwrap_or_default();

    // Compute flags
    let flags = crate::reducer::compute_flags(h, pi, config, now);

    PaneView {
        schema_version: mc_schema::pane_view::PANE_VIEW_SCHEMA_VERSION,
        pane_id: h.pane_id.clone(),
        workspace_id: h.workspace_id.clone(),
        tab_id: h.tab_id.clone(),
        updated_at: now,
        agent: h.agent.clone(),
        agent_status: h.agent_status,
        focused: h.focused,
        session_id: pi.map(|p| p.session_id),
        session_path: pi.map(|p| p.session_path.clone()),
        project,
        last_user_message: pi.and_then(|p| p.last_user_message.clone()),
        arc,
        current,
        vitals,
        vitals_since_last_user: vitals_delta,
        flags,
    }
}

fn current_activity(leaf: &LeafSummary) -> CurrentActivity {
    CurrentActivity {
        kind: match leaf.kind {
            LeafKind::AssistantText => ActivityKind::Thinking,
            LeafKind::ToolCall => ActivityKind::ToolCall,
            LeafKind::ToolResult => ActivityKind::ToolResult,
            LeafKind::UserPending => ActivityKind::UserPending,
        },
        tool_name: leaf.tool_name.clone(),
        snippet: leaf.snippet.clone().unwrap_or_default(),
        started_at: chrono::Utc::now(),
    }
}

fn build_arc(pi: &PiSignals, max_turns: usize) -> Vec<TurnSummary> {
    // Find user nodes and for each, determine the TurnEnd
    let mut turns = Vec::new();
    let user_nodes: Vec<&mc_schema::raw_signals::MessageNode> = pi
        .conversation_tree
        .iter()
        .filter(|n| n.role == mc_schema::raw_signals::MessageRole::User)
        .collect();

    let count = user_nodes.len().min(max_turns);
    for (i, node) in user_nodes.iter().rev().take(count).enumerate() {
        let user_text = node
            .content
            .iter()
            .filter_map(|b| {
                if let mc_schema::raw_signals::ContentBlock::Text { text } = b {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .next()
            .unwrap_or_default();

        // For simplicity in v1, mark all historical turns as "active" with
        // the current activity. TODO: walk each subtree independently.
        let ended = TurnEnd::Active {
            current_activity: CurrentActivity {
                kind: ActivityKind::Thinking,
                tool_name: None,
                snippet: String::new(),
                started_at: chrono::Utc::now(),
            },
            tools_so_far: 0,
        };

        turns.push(TurnSummary {
            user: if user_text.len() > 100 {
                format!("{}...", &user_text[..97])
            } else {
                user_text
            },
            turns_ago: i as u32,
            ended,
        });
    }

    turns
}

// ── Flag computation ──────────────────────────────────────────────────

/// Compute flags from herdr status + pi signals.
pub fn compute_flags(
    h: &HerdrPaneSnapshot,
    pi: Option<&PiSignals>,
    config: &Config,
    now: chrono::DateTime<chrono::Utc>,
) -> Flags {
    let is_blocked = pi
        .map(|p| {
            p.error_since_last_user.is_some()
                && !matches!(p.deepest_leaf_since_last_user.kind, LeafKind::AssistantText)
        })
        .unwrap_or(false);

    let is_runaway = pi
        .map(|p| p.tool_calls_since_last_user >= config.runaway_threshold)
        .unwrap_or(false);

    let awaiting_user_reply = h.agent_status == AgentStatus::Idle
        && pi
            .map(|p| matches!(p.deepest_leaf_since_last_user.kind, LeafKind::AssistantText))
            .unwrap_or(false)
        && !h.focused; // not the currently focused pane

    let idle_long_secs = pi
        .and_then(|p| {
            let age = (now - p.last_activity_at).num_seconds().max(0) as u64;
            if age > config.idle_threshold_secs {
                Some(age)
            } else {
                None
            }
        });

    let attention = if is_blocked {
        Attention::Critical
    } else if is_runaway {
        Attention::High
    } else if awaiting_user_reply {
        Attention::Medium
    } else if h.agent_status == AgentStatus::Working {
        Attention::Low
    } else {
        Attention::None
    };

    Flags {
        attention,
        is_runaway,
        is_blocked,
        awaiting_user_reply,
        idle_long_secs,
    }
}

// ── Totals ────────────────────────────────────────────────────────────

fn compute_totals(panes: &[PaneView]) -> Totals {
    Totals {
        pane_count: panes.len(),
        working_count: panes
            .iter()
            .filter(|p| p.agent_status == AgentStatus::Working)
            .count(),
        idle_count: panes
            .iter()
            .filter(|p| p.agent_status == AgentStatus::Idle)
            .count(),
        blocked_count: panes
            .iter()
            .filter(|p| p.flags.is_blocked)
            .count(),
        total_cost_usd: panes.iter().map(|p| p.vitals.total_cost_usd).sum(),
        total_tool_calls: panes.iter().map(|p| p.vitals.total_tool_calls).sum(),
    }
}