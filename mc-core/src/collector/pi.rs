//! Pi session collector — tails `.jsonl` files and builds conversation trees.
//!
//! For each session path reported by herdr, this collector reads the `.jsonl`
//! file and constructs a `PiSignals` struct with:
//! - Cumulative totals (turns, tool calls, cost)
//! - The conversation DAG (via `parentId` chains)
//! - The "deepest leaf since last user message" (where the agent is right now)
//! - Error detection (tool errors with no follow-up assistant text)

use mc_schema::raw_signals::{
    ContentBlock, ErrorSummary, LeafKind, LeafSummary, MessageNode,
    MessageRole, ModelId, PiSignals, ThinkingLevel,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Read and parse a pi session `.jsonl` file into `PiSignals`.
pub fn parse_session(path: &Path) -> Result<PiSignals, Error> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| Error::Io(path.to_path_buf(), e))?;

    let raw_records: Vec<RawRecord> = contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<RawRecord>(l).ok())
        .collect();

    build_signals(path, &raw_records)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read {0}: {1}")]
    Io(std::path::PathBuf, std::io::Error),
    #[error("no session record found in {0}")]
    NoSessionRecord(String),
}

// ── Raw pi JSON records (deserialized from .jsonl) ──────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawRecord {
    Session {
        id: String,
        #[serde(default)]
        cwd: Option<String>,
        timestamp: String,
    },
    #[serde(rename = "model_change")]
    ModelChange {
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
        #[serde(default)]
        #[serde(rename = "parentId")]
        #[allow(dead_code)]
        parent_id: Option<String>,
    },
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange {
        #[serde(rename = "thinkingLevel")]
        thinking_level: String,
    },
    Message {
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        message: RawMessage,
    },
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    #[serde(default)]
    id: Option<String>,
    role: String,
    #[serde(default)]
    content: serde_json::Value,
}

// ── Build PiSignals from raw records ─────────────────────────────────

fn build_signals(session_path: &Path, records: &[RawRecord]) -> Result<PiSignals, Error> {
    let mut session_id = uuid::Uuid::nil();
    let mut started_at = chrono::Utc::now();
    let mut cwd = String::from(".");
    let mut model: Vec<ModelId> = Vec::new();
    let mut thinking_level_accum: Option<ThinkingLevel> = None;
    let mut nodes: Vec<MessageNode> = Vec::new();
    let mut total_tool_calls: u32 = 0;
    let total_cost_usd = 0.0_f64;
    let mut last_activity_at = chrono::Utc::now();

    let mut message_index = 0u64;

    for record in records {
        match record {
            RawRecord::Session {
                id,
                cwd: session_cwd,
                timestamp,
            } => {
                session_id = id.parse().unwrap_or(uuid::Uuid::nil());
                cwd = session_cwd.clone().unwrap_or_else(|| ".".into());
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(timestamp) {
                    started_at = ts.with_timezone(&chrono::Utc);
                }
            }
            RawRecord::ModelChange {
                provider,
                model_id,
                ..
            } => {
                model.push(ModelId {
                    provider: provider.clone(),
                    model_id: model_id.clone(),
                });
            }
            RawRecord::ThinkingLevelChange { thinking_level } => {
                thinking_level_accum = Some(match thinking_level.as_str() {
                    "xhigh" => ThinkingLevel::Xhigh,
                    "high" => ThinkingLevel::High,
                    "medium" => ThinkingLevel::Medium,
                    _ => ThinkingLevel::Low,
                });
            }
            RawRecord::Message {
                parent_id,
                message,
            } => {
                let node_id = message
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("mc-node-{}", message_index));
                message_index += 1;

                let role = match message.role.as_str() {
                    "user" => MessageRole::User,
                    "assistant" => MessageRole::Assistant,
                    "toolResult" => MessageRole::ToolResult,
                    _ => continue, // skip unknown roles
                };

                // Parse content blocks
                let content = parse_content(&message.content);

                // Count tool calls
                let tool_call_count = content
                    .iter()
                    .filter(|b| matches!(b, ContentBlock::ToolCall { .. }))
                    .count() as u32;
                total_tool_calls += tool_call_count;

                // Extract timestamp if available (use parent's or sequence)
                // pi records don't embed message timestamp in a simple way;
                // we use the record order as proxy.

                nodes.push(MessageNode {
                    id: node_id,
                    parent_id: parent_id.clone(),
                    role,
                    content,
                    timestamp: chrono::Utc::now(), // approximate — no per-message ts in pi format
                });
            }
        }
    }

    if session_id.is_nil() {
        return Err(Error::NoSessionRecord(
            session_path.display().to_string(),
        ));
    }

    // Update last activity time from newest node
    if let Some(last) = nodes.last() {
        last_activity_at = last.timestamp;
    }

    // Walk conversation tree
    let total_turns = nodes.iter().filter(|n| n.role == MessageRole::User).count() as u32;
    let last_user_message = find_last_user_message(&nodes);
    let deepest_leaf = walk_deepest_leaf(&nodes);
    let tool_calls_since_last_user = count_tools_since_last_user(&nodes);
    let error_since_last_user = detect_error_since_last_user(&nodes);

    Ok(PiSignals {
        session_id,
        session_path: session_path.to_path_buf(),
        started_at,
        cwd: Path::new(&cwd).to_path_buf(),
        model,
        thinking_level: thinking_level_accum,
        total_turns,
        total_tool_calls,
        total_cost_usd,
        conversation_tree: nodes,
        last_user_message,
        deepest_leaf_since_last_user: deepest_leaf,
        tool_calls_since_last_user,
        cost_since_last_user: 0.0, // TODO: compute from usage records
        error_since_last_user,
        last_activity_at,
    })
}

/// Parse pi's content array into ContentBlock enums.
/// Handles the camelCase `toolCall` key the PRD warns about.
fn parse_content(raw: &serde_json::Value) -> Vec<ContentBlock> {
    let arr = match raw {
        serde_json::Value::Array(a) => a,
        serde_json::Value::String(s) => {
            // Old-style string content — wrap as text
            return vec![ContentBlock::Text {
                text: s.clone(),
            }];
        }
        _ => return vec![],
    };

    arr.iter()
        .filter_map(|b| {
            let ty = b.get("type").and_then(|v| v.as_str())?;
            match ty {
                "text" => b.get("text").and_then(|v| v.as_str()).map(|t| {
                    ContentBlock::Text {
                        text: t.to_string(),
                    }
                }),
                "thinking" => b.get("text").and_then(|v| v.as_str()).map(|t| {
                    ContentBlock::Thinking {
                        text: t.to_string(),
                    }
                }),
                // PRD-verified: pi uses "toolCall" (camelCase), not "tool_use"
                "toolCall" => {
                    let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                    let arguments = b.get("arguments").cloned().unwrap_or(serde_json::Value::Null);
                    Some(ContentBlock::ToolCall { name, arguments })
                }
                "toolResult" => {
                    let content = extract_tool_result_content(b);
                    let is_error = b.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
                    Some(ContentBlock::ToolResult { content, is_error })
                }
                _ => None,
            }
        })
        .collect()
}

/// Extract readable text from a toolResult content block.
fn extract_tool_result_content(block: &serde_json::Value) -> String {
    block
        .get("content")
        .and_then(|c| {
            if let Some(s) = c.as_str() {
                Some(s.to_string())
            } else if let Some(arr) = c.as_array() {
                let parts: Vec<String> = arr
                    .iter()
                    .filter_map(|x| x.get("text").and_then(|t| t.as_str()).map(String::from))
                    .collect();
                Some(parts.join("\n"))
            } else {
                Some(c.to_string())
            }
        })
        .unwrap_or_default()
}

// ── Conversation tree helpers ────────────────────────────────────────

/// Find the last user message text.
fn find_last_user_message(nodes: &[MessageNode]) -> Option<String> {
    nodes
        .iter()
        .rev()
        .filter(|n| n.role == MessageRole::User)
        .find_map(|n| {
            n.content
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
                .next()
        })
        .map(|t| {
            if t.len() > 200 {
                format!("{}...", &t[..197])
            } else {
                t
            }
        })
}

/// Build a child map from parentId chains.
fn build_child_map<'a>(nodes: &'a [MessageNode]) -> HashMap<&'a str, Vec<&'a MessageNode>> {
    let mut children: HashMap<&str, Vec<&MessageNode>> = HashMap::new();
    for node in nodes {
        if let Some(ref pid) = node.parent_id {
            children.entry(pid.as_str()).or_default().push(node);
        }
    }
    children
}

/// Walk the conversation tree from each user message to find the deepest leaf.
/// This mirrors the Python prototype in /tmp/walk.py.
fn walk_deepest_leaf(nodes: &[MessageNode]) -> LeafSummary {
    let children = build_child_map(nodes);

    // Find the last user node
    let last_user = nodes.iter().rev().find(|n| n.role == MessageRole::User);
    let Some(user_node) = last_user else {
        return LeafSummary {
            kind: LeafKind::UserPending,
            tool_name: None,
            snippet: None,
        };
    };

    // Walk from the user node to the deepest descendant
    let deepest = walk_deepest(user_node, &children, &mut std::collections::HashSet::new());

    match deepest {
        Some(node) => {
            // Check what the deepest node contains
            let has_text = node.content.iter().any(|b| matches!(b, ContentBlock::Text { .. }));
            let has_tool_call = node.content.iter().any(|b| matches!(b, ContentBlock::ToolCall { .. }));
            let has_error = node.content.iter().any(|b| {
                matches!(b, ContentBlock::ToolResult { is_error: true, .. })
            });

            if has_error {
                let tool_name = node
                    .content
                    .iter()
                    .find_map(|b| {
                        if let ContentBlock::ToolResult { content: _, .. } = b {
                            // The tool name isn't stored in toolResult; non-critical
                            None
                        } else {
                            None
                        }
                    });
                let snippet = node.content.iter().find_map(|b| {
                    if let ContentBlock::ToolResult { content, .. } = b {
                        Some(truncate(content, 100))
                    } else {
                        None
                    }
                });
                LeafSummary {
                    kind: LeafKind::ToolResult,
                    tool_name,
                    snippet,
                }
            } else if has_tool_call && !has_text {
                let tool_name = node.content.iter().find_map(|b| {
                    if let ContentBlock::ToolCall { name, .. } = b {
                        Some(name.clone())
                    } else {
                        None
                    }
                });
                let snippet = node.content.iter().find_map(|b| {
                    if let ContentBlock::ToolCall {
                        name,
                        arguments,
                    } = b
                    {
                        let args_str = serde_json::to_string(arguments).unwrap_or_default();
                        Some(format!("{} {}", name, truncate(&args_str, 80)))
                    } else {
                        None
                    }
                });
                LeafSummary {
                    kind: LeafKind::ToolCall,
                    tool_name,
                    snippet,
                }
            } else if has_text {
                let snippet = node.content.iter().find_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(truncate(text, 200))
                    } else {
                        None
                    }
                });
                LeafSummary {
                    kind: LeafKind::AssistantText,
                    tool_name: None,
                    snippet,
                }
            } else {
                LeafSummary {
                    kind: LeafKind::ToolResult,
                    tool_name: None,
                    snippet: None,
                }
            }
        }
        None => LeafSummary {
            kind: LeafKind::UserPending,
            tool_name: None,
            snippet: None,
        },
    }
}

/// Walk from a node to its deepest descendant, avoiding cycles.
fn walk_deepest<'a>(
    node: &'a MessageNode,
    children: &HashMap<&str, Vec<&'a MessageNode>>,
    seen: &mut std::collections::HashSet<&'a str>,
) -> Option<&'a MessageNode> {
    if !seen.insert(node.id.as_str()) {
        return None; // cycle detected
    }

    let kids = children.get(node.id.as_str());
    match kids {
        Some(c) if c.is_empty() => Some(node),
        None => Some(node),
        Some(c) => {
            // Walk to the deepest descendant
            let mut deepest: Option<&MessageNode> = None;
            for child in &*c {
                let d = walk_deepest(child, children, seen);
                deepest = d.or(deepest);
            }
            deepest
        }
    }
}

/// Count tool calls that happened after the last user message.
fn count_tools_since_last_user(nodes: &[MessageNode]) -> u32 {
    let children = build_child_map(nodes);

    // Find the last user node
    let Some(user_node) = nodes.iter().rev().find(|n| n.role == MessageRole::User) else {
        return 0;
    };

    // Walk the subtree from the user node and count tool calls
    count_tools_in_subtree(user_node, &children, &mut std::collections::HashSet::new())
}

fn count_tools_in_subtree(
    node: &MessageNode,
    children: &HashMap<&str, Vec<&MessageNode>>,
    seen: &mut std::collections::HashSet<String>,
) -> u32 {
    if !seen.insert(node.id.clone()) {
        return 0;
    }

    let mut count: u32 = node
        .content
        .iter()
        .filter(|b| matches!(b, ContentBlock::ToolCall { .. }))
        .count() as u32;

    if let Some(kids) = children.get(node.id.as_str()) {
        for child in kids {
            count += count_tools_in_subtree(child, children, seen);
        }
    }

    count
}

/// Detect if there's a tool error with no follow-up assistant text.
fn detect_error_since_last_user(nodes: &[MessageNode]) -> Option<ErrorSummary> {
    let children = build_child_map(nodes);

    let Some(user_node) = nodes.iter().rev().find(|n| n.role == MessageRole::User) else {
        return None;
    };

    // Walk to the deepest leaf
    let deepest = {
        let mut seen = std::collections::HashSet::new();
        walk_deepest(user_node, &children, &mut seen)
    };

    let Some(leaf) = deepest else {
        return None;
    };

    // Check if the deepest leaf has a tool error
    let has_error = leaf.content.iter().any(|b| {
        matches!(b, ContentBlock::ToolResult { is_error: true, .. })
    });

    if !has_error {
        return None;
    }

    // Check that there's no assistant text after the error
    let has_assistant_text_after = leaf.content.iter().any(|b| {
        matches!(b, ContentBlock::Text { .. })
    });

    if has_assistant_text_after {
        return None;
    }

    // Build error summary from the error content
    let excerpt = leaf
        .content
        .iter()
        .find_map(|b| {
            if let ContentBlock::ToolResult {
                content,
                is_error: true,
            } = b
            {
                Some(truncate(content, 200))
            } else {
                None
            }
        })
        .unwrap_or_default();

    Some(ErrorSummary {
        tool_name: "unknown".to_string(),
        excerpt,
    })
}

fn truncate(s: &str, max_len: usize) -> String {
    let s = s.replace('\n', " ");
    if s.len() <= max_len {
        s
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_content_toolcall_camelcase() {
        let json = serde_json::json!([
            {"type": "toolCall", "name": "bash", "arguments": {"command": "ls"}},
            {"type": "toolResult", "content": [{"text": "file.txt"}], "isError": false},
            {"type": "text", "text": "Done!"},
        ]);
        let blocks = parse_content(&json);
        assert_eq!(blocks.len(), 3);
        assert!(matches!(blocks[0], ContentBlock::ToolCall { .. }));
        assert!(matches!(blocks[1], ContentBlock::ToolResult { .. }));
        assert!(matches!(blocks[2], ContentBlock::Text { .. }));
    }

    #[test]
    fn parse_content_string_fallback() {
        let json = serde_json::Value::String("hello world".into());
        let blocks = parse_content(&json);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0], ContentBlock::Text { .. }));
    }
}