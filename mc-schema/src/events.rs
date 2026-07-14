use crate::pane_view::{Attention, PaneView, Totals};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use schemars::JsonSchema;

/// Protocol version for the wire protocol (§8).
/// Bump when adding/removing methods or event kinds.
pub const PROTOCOL_VERSION: u32 = 1;

// ── Event kinds ───────────────────────────────────────────────────────

/// Per-pane deltas, not full snapshots, for efficiency.
/// Designed so 10-pane live updates stay under 1 KB/event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// A new pane appeared (herdr tab/pane was created)
    PaneAdded(PaneView),
    /// A pane was removed (herdr pane closed)
    PaneRemoved {
        pane_id: String,
    },
    /// Field-level change to an existing PaneView.
    /// Clients can merge the patch into their cached PaneView.
    PaneViewPatch {
        pane_id: String,
        patch: PaneViewPatch,
    },
    /// Attention level changed — the "needs-you" lane should re-sort.
    AttentionChanged {
        pane_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Attention>,
        to: Attention,
    },
    /// Cross-pane totals changed.
    TotalsChanged { totals: Totals },
}

/// Field-level patch for PaneView.
/// All fields optional — only set fields carry changed values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PaneViewPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_status: Option<crate::pane_view::AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<crate::pane_view::CurrentActivity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vitals_since_last_user: Option<crate::pane_view::VitalsDelta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flags: Option<crate::pane_view::Flags>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_user_message: Option<Option<String>>,
}

// ── Wire envelopes ────────────────────────────────────────────────────

/// Every event on the wire is wrapped in this envelope.
/// Mirrors herdr's EventEnvelope pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EventEnvelope {
    /// Protocol version at the time this event was emitted
    pub protocol_version: u32,
    /// Monotonic sequence number (no gaps).
    /// Clients use `events_after(seq)` to catch up.
    pub sequence: u64,
    /// When the event was emitted
    pub timestamp: DateTime<Utc>,
    /// The event payload
    #[serde(flatten)]
    pub event: EventKind,
}

// ── Request/response types ────────────────────────────────────────────

/// Request envelope for the mc JSON-RPC protocol (§8.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McRequest {
    pub id: String,
    #[serde(flatten)]
    pub method: McMethod,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", content = "params")]
pub enum McMethod {
    /// Full snapshot of all panes + totals.
    #[serde(rename = "mc.snapshot")]
    Snapshot,
    /// Get a single pane by id.
    #[serde(rename = "mc.pane.get")]
    PaneGet { pane_id: String },
    /// Subset of panes where Flags.attention != None, sorted desc.
    #[serde(rename = "mc.needs_attention")]
    NeedsAttention,
    /// Cross-pane aggregates.
    #[serde(rename = "mc.totals")]
    Totals,
    /// Subscribe to events, optionally starting after a sequence number
    /// and filtered by event kinds.
    #[serde(rename = "events.subscribe")]
    EventsSubscribe {
        #[serde(default)]
        after_seq: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        kinds: Vec<String>,
    },
    /// Get the current (latest) sequence number.
    #[serde(rename = "events.current_sequence")]
    EventsCurrentSequence,
}

/// Success response for mc.snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotResponse {
    pub panes: Vec<PaneView>,
    pub totals: Totals,
    pub sequence: u64,
}

/// Success response for mc.needs_attention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NeedsAttentionResponse {
    pub panes: Vec<PaneView>,
}

/// Success response for mc.pane.get.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PaneGetResponse {
    pub pane: PaneView,
}

/// Success response for mc.totals.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TotalsResponse {
    pub totals: Totals,
}

/// Success response for events.current_sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CurrentSequenceResponse {
    pub sequence: u64,
}

/// Error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct McError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}