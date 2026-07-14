use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use schemars::JsonSchema;

/// The kind of project detected in a pane's cwd.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    Rust,
    Node,
    Python,
    Mixed,
    Unknown,
}

/// A hint about a recently-modified artifact directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactHint {
    /// Relative path under cwd, e.g. "graphify-out/"
    pub path: String,
    /// Human-readable description of when it was last touched
    pub updated_relative: String,
}

/// Project metadata derived from the pane's cwd.
/// Many panes can share one ProjectProfile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectView {
    pub kind: ProjectKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// One-liner from README / Cargo.toml description / package.json description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Stack summary, e.g. ["Rust", "Docker", "Nix"]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stack_summary: Vec<String>,
    /// Recently-modified artifact dirs, surfaced as "open threads" hints
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_artifacts: Vec<ArtifactHint>,
    /// When this project was last scanned
    pub scanned_at: DateTime<Utc>,
}

// ── Raw-signal types (§6) used by the project collector ──

/// Internal-only: what the ProjectCollector emits.
/// Not part of the public client API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectProfile {
    pub cwd: PathBuf,
    pub kind: ProjectKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stack_summary: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_artifacts: Vec<ArtifactHint>,
    pub scanned_at: DateTime<Utc>,
    /// Last modification time of cwd, for cache invalidation
    pub cwd_mtime: DateTime<Utc>,
}

impl ProjectProfile {
    /// Convert into the public ProjectView.
    pub fn to_view(&self) -> ProjectView {
        ProjectView {
            kind: self.kind.clone(),
            name: self.name.clone(),
            purpose: self.purpose.clone(),
            stack_summary: self.stack_summary.clone(),
            recent_artifacts: self.recent_artifacts.clone(),
            scanned_at: self.scanned_at,
        }
    }
}