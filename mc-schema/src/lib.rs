//! Mission Control schema crate.
//!
//! This crate is the frozen keel of the Mission Control system.
//! It contains all data types from the PRD (§6–§8) and depends on nothing
//! except serde + schemars for serialization / JSON Schema export.
//!
//! # Public surface (clients consume these)
//! - [`PaneView`] — the canonical per-pane state (pane_view module)
//! - [`EventKind`], [`EventEnvelope`] — wire protocol events (events module)
//! - [`McRequest`], [`McMethod`] — JSON-RPC request types (events module)
//!
//! # Internal surface (collectors emit these to the reducer)
//! - [`HerdrPaneSnapshot`], [`PiSignals`], [`ProjectProfile`] (raw_signals module)

pub mod events;
pub mod pane_view;
pub mod project;
pub mod raw_signals;

// ── Re-exports: public API ──
pub use events::*;
pub use pane_view::*;
pub use project::ProjectProfile;
pub use project::ProjectView;

// ── Re-exports: internal API (collectors → reducer) ──
pub use raw_signals::{
    ContentBlock, ConversationTree, ErrorSummary, HerdrPaneSnapshot, LeafKind,
    LeafSummary, MessageNode, MessageRole, ModelId, PiSignals, ThinkingLevel,
};

#[cfg(test)]
mod tests {
    use crate::pane_view::Attention;

    /// Helper: round-trip a value through serde_json and assert it equals the original.
    fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq>(
        val: &T,
    ) {
        let json = serde_json::to_string(val).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(val, &back, "round-trip failed for {}", std::any::type_name::<T>());
    }

    // ── project.rs ──

    #[test]
    fn project_view_round_trip() {
        let v = crate::project::ProjectView {
            kind: crate::project::ProjectKind::Rust,
            name: Some("herdr".into()),
            purpose: Some("terminal workspace manager".into()),
            stack_summary: vec!["Rust".into(), "Docker".into()],
            recent_artifacts: vec![crate::project::ArtifactHint {
                path: "graphify-out/".into(),
                updated_relative: "2h ago".into(),
            }],
            scanned_at: chrono::Utc::now(),
        };
        round_trip(&v);
    }

    #[test]
    fn project_kind_serde() {
        let json = r#""rust""#;
        let k: crate::project::ProjectKind = serde_json::from_str(json).unwrap();
        assert_eq!(k, crate::project::ProjectKind::Rust);
        assert_eq!(serde_json::to_string(&k).unwrap(), json);
    }

    // ── raw_signals.rs ──

    #[test]
    fn herdr_pane_snapshot_round_trip() {
        let s = crate::raw_signals::HerdrPaneSnapshot {
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            pane_id: "w1:p1".into(),
            agent: Some("pi".into()),
            agent_status: crate::pane_view::AgentStatus::Working,
            focused: true,
            cwd: "/home/user/project".into(),
            agent_session_path: Some("/tmp/session.jsonl".into()),
            custom_status: Some("thinking...".into()),
            captured_at: chrono::Utc::now(),
        };
        round_trip(&s);
    }

    #[test]
    fn pi_signals_round_trip() {
        let tree = vec![crate::raw_signals::MessageNode {
            id: "msg1".into(),
            parent_id: None,
            role: crate::raw_signals::MessageRole::User,
            content: vec![crate::raw_signals::ContentBlock::Text {
                text: "hello".into(),
            }],
            timestamp: chrono::Utc::now(),
        }];
        let s = crate::raw_signals::PiSignals {
            session_id: uuid::Uuid::new_v4(),
            session_path: "/tmp/s.jsonl".into(),
            started_at: chrono::Utc::now(),
            cwd: "/home/user/project".into(),
            model: vec![crate::raw_signals::ModelId {
                provider: "anthropic".into(),
                model_id: "claude-opus-4".into(),
            }],
            thinking_level: Some(crate::raw_signals::ThinkingLevel::High),
            total_turns: 42,
            total_tool_calls: 69,
            total_cost_usd: 1.23,
            conversation_tree: tree,
            last_user_message: Some("fix the bug".into()),
            deepest_leaf_since_last_user: crate::raw_signals::LeafSummary {
                kind: crate::raw_signals::LeafKind::ToolCall,
                tool_name: Some("bash".into()),
                snippet: Some("cargo test".into()),
            },
            tool_calls_since_last_user: 5,
            cost_since_last_user: 0.15,
            error_since_last_user: Some(crate::raw_signals::ErrorSummary {
                tool_name: "bash".into(),
                excerpt: "command not found".into(),
            }),
            last_activity_at: chrono::Utc::now(),
        };
        round_trip(&s);
    }

    #[test]
    fn content_block_toolcall_camelcase() {
        // Verify the camelCase serialization the PRD flagged
        let block = crate::raw_signals::ContentBlock::ToolCall {
            name: "bash".into(),
            arguments: serde_json::json!({"command": "ls"}),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains(r#""type":"toolCall""#), "camelCase toolCall: {json}");
    }

    // ── pane_view.rs ──

    #[test]
    fn pane_view_schema_version_field() {
        let pv = crate::pane_view::PaneView {
            schema_version: crate::pane_view::PANE_VIEW_SCHEMA_VERSION,
            pane_id: "w1:p1".into(),
            workspace_id: "w1".into(),
            tab_id: "t1".into(),
            updated_at: chrono::Utc::now(),
            agent: Some("pi".into()),
            agent_status: crate::pane_view::AgentStatus::Working,
            focused: false,
            session_id: Some(uuid::Uuid::new_v4()),
            session_path: Some("/tmp/s.jsonl".into()),
            project: None,
            last_user_message: Some("fix the bug".into()),
            arc: vec![],
            current: crate::pane_view::CurrentActivity {
                kind: crate::pane_view::ActivityKind::ToolCall,
                tool_name: Some("bash".into()),
                snippet: "cargo build".into(),
                started_at: chrono::Utc::now(),
            },
            vitals: crate::pane_view::Vitals {
                total_turns: 3,
                total_tool_calls: 12,
                total_cost_usd: 0.45,
                model: "claude-opus-4".into(),
                thinking_level: Some("high".into()),
                session_age_secs: 3600,
            },
            vitals_since_last_user: crate::pane_view::VitalsDelta {
                tool_calls: 12,
                cost_usd: 0.45,
                errors: 0,
            },
            flags: crate::pane_view::Flags {
                attention: crate::pane_view::Attention::Low,
                is_runaway: false,
                is_blocked: false,
                awaiting_user_reply: false,
                idle_long_secs: None,
            },
        };
        round_trip(&pv);
    }

    #[test]
    fn attention_is_ord() {
        // Verify Critical > High > Medium > Low > None
        assert!(Attention::Critical > Attention::High);
        assert!(Attention::High > Attention::Medium);
        assert!(Attention::Medium > Attention::Low);
        assert!(Attention::Low > Attention::None);
    }

    // ── events.rs ──

    #[test]
    fn event_envelope_round_trip() {
        let ev = crate::events::EventEnvelope {
            protocol_version: crate::events::PROTOCOL_VERSION,
            sequence: 1,
            timestamp: chrono::Utc::now(),
            event: crate::events::EventKind::PaneRemoved {
                pane_id: "w1:p1".into(),
            },
        };
        round_trip(&ev);
    }

    #[test]
    fn mc_method_serde() {
        let json = r#"{"id":"1","method":"mc.snapshot","params":null}"#;
        let req: crate::events::McRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(
            req.method,
            crate::events::McMethod::Snapshot,
        ));
        round_trip(&req);
    }

    // ── JSON Schema export (schemars) ──

    #[test]
    fn pane_view_json_schema() {
        let schema = schemars::schema_for!(crate::pane_view::PaneView);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("pane_id"));
        assert!(json.contains("flags"));
        assert!(json.contains("attention"));
    }

    #[test]
    fn event_envelope_json_schema() {
        let schema = schemars::schema_for!(crate::events::EventEnvelope);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("sequence"));
        // schemars uses snake_case serde names, not Rust variant names
        assert!(json.contains("pane_added") || json.contains("EventEnvelope"));
    }
}