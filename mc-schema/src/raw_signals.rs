use crate::pane_view::AgentStatus;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

// ─────────────────────────────────────────────────────────────────────
// Internal types (§6). These are what collectors emit.
// Clients never see them — they stop at the reducer boundary (R1).
// ─────────────────────────────────────────────────────────────────────

/// From the herdr collector — a point-in-time snapshot of one pane as
/// reported by `herdr pane list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HerdrPaneSnapshot {
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
    /// Agent kind, e.g. "pi"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// herdr's view of agent state
    pub agent_status: AgentStatus,
    /// Whether this pane has focus in the terminal
    pub focused: bool,
    /// Current working directory
    pub cwd: PathBuf,
    /// This is the bridge to PiSignals — the absolute path to the
    /// pi session .jsonl file, from herdr's AgentSessionInfo.value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_path: Option<PathBuf>,
    /// Custom status reported by the agent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_status: Option<String>,
    /// When this snapshot was captured
    pub captured_at: DateTime<Utc>,
}

// AgentStatus is imported from pane_view above, not duplicated here (R1).

// ─────────────────────────────────────────────────────────────────────

/// From the pi-session collector — cumulative + live-derived view of
/// one session's .jsonl.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PiSignals {
    pub session_id: Uuid,
    pub session_path: PathBuf,
    pub started_at: DateTime<Utc>,
    pub cwd: PathBuf,

    // ── cumulative over the whole session ──
    /// Model history, sorted by time; last = current
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model: Vec<ModelId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
    pub total_turns: u32,
    pub total_tool_calls: u32,
    pub total_cost_usd: f64,

    // ── live, derived view ──
    pub conversation_tree: ConversationTree,
    /// The last user message text (summary length)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_user_message: Option<String>,
    /// Where the agent is right now (deepest leaf since last user msg)
    pub deepest_leaf_since_last_user: LeafSummary,
    pub tool_calls_since_last_user: u32,
    pub cost_since_last_user: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_since_last_user: Option<ErrorSummary>,
    pub last_activity_at: DateTime<Utc>,
}

/// A model identifier — provider + modelId as recorded in pi's
/// `model_change` records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelId {
    pub provider: String,
    pub model_id: String,
}

/// Thinking level from pi's `thinking_level_change` records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    Xhigh,
    High,
    Medium,
    Low,
}

// ─────────────────────────────────────────────────────────────────────
// Conversation tree types — walked from parentId chains
// ─────────────────────────────────────────────────────────────────────

/// The DAG of messages in a session.
pub type ConversationTree = Vec<MessageNode>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageNode {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub role: MessageRole,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentBlock>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    #[serde(rename = "toolResult")]
    ToolResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentBlock {
    Text { text: String },
    Thinking { text: String },
    ToolCall {
        name: String,
        arguments: serde_json::Value,
    },
    ToolResult {
        content: String,
        #[serde(rename = "isError")]
        is_error: bool,
    },
}

/// Summary of the deepest assistant leaf since last user message.
/// This is "where the agent is right now."
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeafSummary {
    pub kind: LeafKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeafKind {
    /// Assistant text — the agent gave a final answer
    AssistantText,
    /// A tool call is in-flight
    ToolCall,
    /// A tool result was received and the agent is thinking/next-tooling
    ToolResult,
    /// The last message was from the user — agent hasn't responded yet
    UserPending,
}

/// Error summary for when a tool returns an error and the agent hasn't
/// produced follow-up assistant text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorSummary {
    pub tool_name: String,
    pub excerpt: String,
}