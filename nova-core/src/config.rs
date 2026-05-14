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
    /// Browser: path to system Chrome/Chromium executable (auto-detected if absent)
    pub browser_chrome_path: Option<String>,
    /// Browser: user data dir for profile persistence (default: ~/.nova/browser-profile)
    pub browser_profile_dir: Option<String>,
    /// Browser: run headless (default: true)
    pub browser_headless: Option<bool>,
    /// Discord Gateway: enabled flag (default: false)
    #[serde(default = "default_discord_enabled")]
    pub discord_enabled: bool,
    /// Discord: show tool approval buttons before executing sensitive tools (default: true)
    #[serde(default = "default_tool_approval_enabled")]
    pub tool_approval_enabled: bool,
    /// Discord Gateway: Bot Token (required if enabled)
    pub discord_token: Option<String>,
    /// Discord: Default channel ID for proactive pushes (e.g. ProjectCompleted).
    /// Used when the channel_id in the event is "coordinator".
    pub discord_channel_id: Option<u64>,
    /// Log level: "info" (default) or "debug"
    #[serde(default = "default_log_level")]
    pub log_level: String,
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
fn default_discord_enabled() -> bool { false }
fn default_tool_approval_enabled() -> bool { true }
fn default_log_level() -> String { "info".into() }

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
            browser_chrome_path: None,
            browser_profile_dir: None,
            browser_headless: None,
            discord_enabled: default_discord_enabled(),
            tool_approval_enabled: default_tool_approval_enabled(),
            discord_token: None,
            discord_channel_id: None,
            log_level: default_log_level(),
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
