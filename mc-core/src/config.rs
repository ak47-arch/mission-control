use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Mission Control configuration, read from `~/.config/mc/config.toml`.
/// All fields have sensible defaults — mc runs with zero config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Tools since last user message >= this → runaway flag
    #[serde(default = "default_runaway_threshold")]
    pub runaway_threshold: u32,

    /// Seconds since last activity > this → idle_long flag
    #[serde(default = "default_idle_threshold_secs")]
    pub idle_threshold_secs: u64,

    /// Number of user turns to include in the conversation arc
    #[serde(default = "default_arc_turns")]
    pub arc_turns: usize,

    /// Override for herdr socket path. Normally read from $HERDR_SOCKET_PATH.
    #[serde(default)]
    pub herdr_socket: Option<PathBuf>,
}

fn default_runaway_threshold() -> u32 { 25 }
fn default_idle_threshold_secs() -> u64 { 900 } // 15 minutes
fn default_arc_turns() -> usize { 5 }

impl Default for Config {
    fn default() -> Self {
        Self {
            runaway_threshold: default_runaway_threshold(),
            idle_threshold_secs: default_idle_threshold_secs(),
            arc_turns: default_arc_turns(),
            herdr_socket: None,
        }
    }
}

impl Config {
    /// Load config from `~/.config/mc/config.toml`. Returns default if file
    /// doesn't exist or can't be parsed.
    pub fn load() -> Self {
        let config_path = Self::default_path();
        match std::fs::read_to_string(&config_path) {
            Ok(contents) => {
                toml::from_str(&contents).unwrap_or_else(|e| {
                    eprintln!("mc: failed to parse config at {}: {e}", config_path.display());
                    Self::default()
                })
            }
            Err(_) => Self::default(),
        }
    }

    /// Default config path: `~/.config/mc/config.toml`
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("mc")
            .join("config.toml")
    }
}