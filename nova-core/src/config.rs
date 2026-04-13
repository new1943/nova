use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct NovaConfig {
    #[serde(default = "default_api_key")]
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_api_base_url")]
    pub api_base_url: String,
    #[serde(default = "default_context_window")]
    pub context_window: usize,
    #[serde(default = "default_char_delay_ms")]
    pub char_delay_ms: u64,
    #[serde(default = "default_workspace")]
    pub workspace: PathBuf,
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_secs: u64,
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    #[serde(default = "default_tool_timeout")]
    pub tool_timeout_secs: u64,
    #[serde(default = "default_compact_target")]
    pub compact_target_pct: f32,
    #[serde(default = "default_budget_trigger")]
    pub budget_trigger_pct: f32,
    /// Run mode: "open" = no restrictions, "sandbox" = whitelist + path restrictions
    #[serde(default = "default_mode")]
    pub mode: String,
}

fn default_api_key() -> String { String::new() }
fn default_model() -> String { "MiniMax-M2.7".into() }
fn default_api_base_url() -> String { "https://api.minimaxi.com/anthropic".into() }
fn default_context_window() -> usize { 200_000 }
fn default_char_delay_ms() -> u64 { 5 }
fn default_workspace() -> PathBuf { dirs::home_dir().unwrap_or_default().join(".nova") }
fn default_heartbeat_interval() -> u64 { 300 }
fn default_max_turns() -> usize { 20 }
fn default_tool_timeout() -> u64 { 60 }
fn default_compact_target() -> f32 { 0.6 }
fn default_budget_trigger() -> f32 { 0.9 }
fn default_mode() -> String { "open".into() }

impl Default for NovaConfig {
    fn default() -> Self {
        Self {
            api_key: default_api_key(),
            model: default_model(),
            api_base_url: default_api_base_url(),
            context_window: default_context_window(),
            char_delay_ms: default_char_delay_ms(),
            workspace: default_workspace(),
            heartbeat_interval_secs: default_heartbeat_interval(),
            max_turns: default_max_turns(),
            tool_timeout_secs: default_tool_timeout(),
            compact_target_pct: default_compact_target(),
            budget_trigger_pct: default_budget_trigger(),
            mode: default_mode(),
        }
    }
}

impl NovaConfig {
    /// Load config from file, falling back to defaults for missing fields.
    /// If file doesn't exist, returns all defaults.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    /// Load from default path (~/.nova/config)
    pub fn load_default() -> Result<Self> {
        let path = default_workspace().join("config");
        Self::load(&path)
    }
}
