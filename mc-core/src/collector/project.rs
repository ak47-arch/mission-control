//! Project collector — scans a pane's cwd to identify the project.
//!
//! Cheap, cacheable: checks for well-known marker files, reads descriptions,
//! and notes recently-modified artifact directories.

use mc_schema::project::{ArtifactHint, ProjectKind, ProjectProfile};
use std::path::Path;
use std::time::Duration;

/// Scan the given cwd and return a ProjectProfile.
/// Result is cached in memory by the caller (keyed by cwd).
pub fn scan(cwd: &Path) -> ProjectProfile {
    let kind = detect_kind(cwd);
    let name = detect_name(cwd, &kind);
    let purpose = detect_purpose(cwd, &kind);
    let stack_summary = detect_stack(cwd);
    let recent_artifacts = detect_artifacts(cwd);
    let now = chrono::Utc::now();

    // Use mtime of the directory itself as cache key.
    let cwd_mtime = std::fs::metadata(cwd)
        .and_then(|m| m.modified())
        .map(|t| {
            // Convert SystemTime → DateTime<Utc> via duration since epoch
            let dur = t
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or(Duration::ZERO);
            chrono::DateTime::from_timestamp(dur.as_secs() as i64, dur.subsec_nanos())
                .unwrap_or(now)
        })
        .unwrap_or(now);

    ProjectProfile {
        cwd: cwd.to_path_buf(),
        kind,
        name,
        purpose,
        stack_summary,
        recent_artifacts,
        scanned_at: now,
        cwd_mtime,
    }
}

/// Detect project kind from marker files.
fn detect_kind(cwd: &Path) -> ProjectKind {
    let markers: &[(ProjectKind, &[&str])] = &[
        (ProjectKind::Rust, &["Cargo.toml"]),
        (ProjectKind::Node, &["package.json", "yarn.lock", "pnpm-lock.yaml"]),
        (ProjectKind::Python, &["pyproject.toml", "setup.py", "requirements.txt", "Pipfile"]),
    ];

    let mut hits = Vec::new();
    for (kind, files) in markers {
        for file in *files {
            if cwd.join(file).exists() {
                hits.push(*kind);
                break;
            }
        }
    }

    if hits.is_empty() {
        ProjectKind::Unknown
    } else if hits.len() == 1 {
        hits[0]
    } else {
        ProjectKind::Mixed
    }
}

/// Detect project name from common description files.
fn detect_name(cwd: &Path, kind: &ProjectKind) -> Option<String> {
    match kind {
        ProjectKind::Rust => {
            let cargo = cwd.join("Cargo.toml");
            if let Ok(contents) = std::fs::read_to_string(&cargo) {
                for line in contents.lines().take(20) {
                    if let Some(name) = line.strip_prefix("name = ").or_else(|| line.strip_prefix("name =" )) {
                        let name = name.trim().trim_matches('"').trim_matches('\'');
                        if !name.is_empty() {
                            return Some(name.to_string());
                        }
                    }
                }
            }
            None
        }
        ProjectKind::Node => {
            let pkg = cwd.join("package.json");
            if let Ok(contents) = std::fs::read_to_string(&pkg) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&contents) {
                    return val.get("name").and_then(|v| v.as_str()).map(String::from);
                }
            }
            None
        }
        ProjectKind::Python => {
            let pyproject = cwd.join("pyproject.toml");
            if let Ok(contents) = std::fs::read_to_string(&pyproject) {
                for line in contents.lines() {
                    if let Some(name) = line.strip_prefix("name = ") {
                        let name = name.trim().trim_matches('"').trim_matches('\'');
                        if !name.is_empty() {
                            return Some(name.to_string());
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Extract a one-line purpose from README or description field.
fn detect_purpose(cwd: &Path, kind: &ProjectKind) -> Option<String> {
    // Try README first line that isn't a header or empty
    for readme_name in &["README.md", "readme.md", "README", "README.txt"] {
        let path = cwd.join(readme_name);
        if let Ok(contents) = std::fs::read_to_string(&path) {
            for line in contents.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('[') {
                    continue;
                }
                let purpose = if trimmed.len() > 120 {
                    format!("{}...", &trimmed[..117])
                } else {
                    trimmed.to_string()
                };
                return Some(purpose);
            }
        }
    }
    // Fall back to Cargo.toml / package.json description
    match kind {
        ProjectKind::Rust => {
            let cargo = cwd.join("Cargo.toml");
            if let Ok(contents) = std::fs::read_to_string(&cargo) {
                for line in contents.lines() {
                    if let Some(desc) = line.strip_prefix("description = ")
                        .or_else(|| line.strip_prefix("description =")) {
                        return Some(desc.trim().trim_matches('"').trim_matches('\'').to_string());
                    }
                }
            }
            None
        }
        ProjectKind::Node => {
            let pkg = cwd.join("package.json");
            if let Ok(contents) = std::fs::read_to_string(&pkg) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&contents) {
                    return val.get("description").and_then(|v| v.as_str()).map(String::from);
                }
            }
            None
        }
        _ => None,
    }
}

/// Build a stack summary from marker files.
fn detect_stack(cwd: &Path) -> Vec<String> {
    let mut stack = Vec::new();

    let checks: &[(&str, &str)] = &[
        ("Rust", "Cargo.toml"),
        ("Node", "package.json"),
        ("Python", "pyproject.toml"),
        ("Docker", "Dockerfile"),
        ("docker-compose", "docker-compose.yml"),
        ("Nix", "flake.nix"),
        ("GraphQL", "schema.graphql"),
        ("Terraform", "main.tf"),
    ];

    for (label, file) in checks {
        if cwd.join(file).exists() {
            stack.push(label.to_string());
        }
    }

    // Also check Rust edition from Cargo.toml
    if let Some(rust_pos) = stack.iter().position(|s| s == "Rust") {
        if let Ok(contents) = std::fs::read_to_string(cwd.join("Cargo.toml")) {
            for line in contents.lines() {
                if let Some(ed) = line.strip_prefix("edition = ") {
                    let ed = ed.trim().trim_matches('"').trim_matches('\'');
                    stack[rust_pos] = format!("Rust ({ed})");
                }
            }
        }
    }

    stack
}

/// Detect recently-modified artifact directories as "open threads" hints.
fn detect_artifacts(cwd: &Path) -> Vec<ArtifactHint> {
    let interesting: &[(&str, &str)] = &[
        ("graphify-out", "graphify-out/"),
        ("openwiki", "openwiki/"),
        ("headroom-out", "headroom-out/"),
        ("target", "target/"),
        ("node_modules", "node_modules/"),
        ("dist", "dist/"),
        ("build", "build/"),
        (".git", ".git/"),
    ];

    let now = std::time::SystemTime::now();
    let mut hints = Vec::new();

    for (dir, path_str) in interesting {
        let full = cwd.join(dir);
        if !full.is_dir() {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(&full) {
            if let Ok(mtime) = meta.modified() {
                {
                    let age = now
                        .duration_since(mtime)
                        .unwrap_or(Duration::ZERO);
                    let desc = if age < Duration::from_secs(60) {
                        "just now".to_string()
                    } else if age < Duration::from_secs(3600) {
                        format!("{}m ago", age.as_secs() / 60)
                    } else if age < Duration::from_secs(86400) {
                        format!("{}h ago", age.as_secs() / 3600)
                    } else {
                        format!("{}d ago", age.as_secs() / 86400)
                    };
                    hints.push(ArtifactHint {
                        path: path_str.to_string(),
                        updated_relative: desc,
                    });
                }
            }
        }
    }

    hints
}