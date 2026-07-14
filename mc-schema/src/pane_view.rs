use crate::project::ProjectView;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

use schemars::JsonSchema;

/// Schema version for PaneView. Bump when adding/removing fields.
pub const PANE_VIEW_SCHEMA_VERSION: u32 = 1;

/// The canonical per-pane state struct (§7).
/// This is the ONLY thing a client needs to understand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PaneView {
    pub schema_version: u32,
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub updated_at: DateTime<Utc>,

    // ── identity & location ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub agent_status: AgentStatus,
    pub focused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_path: Option<PathBuf>,

    // ── project (static-ish) ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectView>,

    // ── conversation arc (semantic) ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_user_message: Option<String>,
    /// Last N user turns + their ending shape
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arc: Vec<TurnSummary>,
    /// Where the agent is right now
    pub current: CurrentActivity,

    // ── numeric vitals ──
    pub vitals: Vitals,
    pub vitals_since_last_user: VitalsDelta,

    // ── computed flags (R4 — derived once in reducer) ──
    pub flags: Flags,
}

/// herdr agent-status (public-facing, same as the raw-signal version
/// but independent per R1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

// ── Conversation arc ─────────────────────────────────────────────────

/// One user turn and what came of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TurnSummary {
    /// Abbreviated user message text
    pub user: String,
    /// How many turns ago (0 = current)
    pub turns_ago: u32,
    /// How the turn ended
    pub ended: TurnEnd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnEnd {
    /// Assistant gave a final text answer
    Answered {
        final_text_excerpt: String,
    },
    /// Still active — the agent is working on it
    Active {
        current_activity: CurrentActivity,
        tools_so_far: u32,
    },
    /// The turn hit an error
    Errored {
        last_error_excerpt: String,
    },
}

/// What the agent is doing right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CurrentActivity {
    pub kind: ActivityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    pub snippet: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Thinking,
    ToolCall,
    ToolResult,
    UserPending,
}

// ── Vitals ───────────────────────────────────────────────────────────

/// Session-wide cumulative numbers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Vitals {
    pub total_turns: u32,
    pub total_tool_calls: u32,
    pub total_cost_usd: f64,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    /// How long this session has been running
    pub session_age_secs: u64,
}

/// Deltas since the last user message — "is this turn expensive/long?"
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VitalsDelta {
    pub tool_calls: u32,
    pub cost_usd: f64,
    pub errors: u32,
}

// ── Computed flags ───────────────────────────────────────────────────

/// Flags are computed by the reducer from PiSignals + HerdrPaneSnapshot.
/// This is the single most important client affordance — clients that
/// only want "what needs me?" deserialize just pane_id + flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Flags {
    /// The kernel of the "needs-you" lane
    pub attention: Attention,
    /// tools_since_last_user >= runaway_threshold (default 25)
    pub is_runaway: bool,
    /// toolResult.isError && no assistant text after
    pub is_blocked: bool,
    /// agent_status Idle && has unanswered final text
    pub awaiting_user_reply: bool,
    /// Some(Duration) when last_activity older than idle threshold
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_long_secs: Option<u64>,
}

/// Discrete urgency level. Foundation of the "needs-you" lane.
///
/// Variants are ordered most-urgent-first: Critical > High > Medium > Low > None.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    /// Nothing to report
    None,
    /// Busy working — all good
    Low,
    /// Awaiting user reply
    Medium,
    /// Runaway agent — may need intervention
    High,
    /// Blocked on an error — needs immediate attention
    Critical,
}

// ── Cross-pane aggregates ───────────────────────────────────────────

/// Aggregates across all panes, returned by mc.totals and the
/// TotalsChanged event.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Totals {
    pub pane_count: usize,
    pub working_count: usize,
    pub idle_count: usize,
    pub blocked_count: usize,
    pub total_cost_usd: f64,
    pub total_tool_calls: u32,
}