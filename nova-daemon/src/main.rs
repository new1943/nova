use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, broadcast, Mutex};
use tracing::{info, error, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use nova_core::agent::{QueryLoop, QueryLoopConfig, LoopEvent};
use nova_core::agent::preflight::PreFlightChecker;
use nova_core::config::NovaConfig;
use nova_core::models::ShadowEvent;
use nova_core::hooks::HookManager;
use nova_core::memory::consolidate::MemoryConsolidator;
use nova_core::memory::daily::DailyNotes;
use nova_core::memory::dream::DreamEngine;
use nova_core::memory::recall::MemoryRecall;
use nova_core::session::manager::SessionManager;
use nova_core::session::search::AgenticSessionSearch;
use nova_core::sidequery::{SideQuery, MemoryKeeper};
use nova_core::skills::{create_shared_loader, SharedSkillsLoader, SkillManageTool, SkillsListTool, SkillViewTool};
use nova_core::heartbeat::{HeartbeatScheduler, scheduler::HeartbeatEvent};
use nova_core::coordinator::Coordinator;
use nova_core::tools::{ToolRegistry, ReadFileTool, WriteFileTool, FileEditTool, GlobTool, GrepTool, BrowserTool, AgenticSearchTool, WorktreeTool, AgentTool, TeamTool, create_shared_tracker};
use nova_core::tools::bash::{BashTool, BashMode};
use nova_core::workspace::BootstrapLoader;
use nova_ipc::{IpcServer, Event, Request};

const SOCKET_PATH: &str = "/tmp/nova.sock";
const PID_FILE: &str = "/tmp/nova.pid";

mod discord;
mod dispatcher;
mod task_manager;

/// [V4 Task 6.2] Discord proactive push message
#[derive(Clone)]
struct DiscordPush {
    channel_id: String,
    content: String,
}

pub struct HandleConfig {
    workspace_dir: PathBuf,
    sessions_dir: PathBuf,
    memories_dir: PathBuf,
    loop_config: QueryLoopConfig,
    skills: SharedSkillsLoader,
    run_mode: String,
    tools: Arc<ToolRegistry>,
    heartbeat_interval_secs: u64,
    /// Arc-wrapped dispatcher sender so it can be shared across HandleConfigs
    /// without move semantics. Clone of Arc<DispatcherSender> is cheap (just ref count).
    dispatcher_tx: Arc<dispatcher::DispatcherSender>,
    /// [V4 Task 6.2] Optional channel for Discord proactive push.
    /// When Some, DiscordHandler listens and sends messages to specified channels.
    discord_push_tx: Option<std::sync::Arc<tokio::sync::mpsc::Sender<DiscordPush>>>,
    /// [V4 Fix] Broadcast sender for IPC push events to TUI
    ipc_push_tx: Option<std::sync::Arc<tokio::sync::broadcast::Sender<nova_ipc::Event>>>,
}

fn budget_pct_calc(input_tokens: usize, context_window: usize) -> f32 {
    if context_window == 0 { return 0.0; }
    input_tokens as f32 / context_window as f32
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load config first to get log_level
    let config = NovaConfig::load_default()?;

    let log_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".nova")
        .join("daemon.log");
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .unwrap_or_else(|_| std::fs::File::open("/dev/null").expect("cannot open /dev/null"));

    // 简洁日志格式: [LEVEL] HH:MM:SS message
    let fmt_layer = fmt::layer()
        .with_writer(std::sync::Mutex::new(log_file))
        .with_ansi(false)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .compact();

    // Use config.log_level, but allow env override
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.log_level))
        // Suppress verbose CDP/WebSocket parsing errors from chromiumoxide
        .add_directive("chromiumoxide=warn".parse().unwrap())
        .add_directive("tungstenite=warn".parse().unwrap());

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("run");

    match cmd {
        "run" => run_daemon(config).await,
        "stop" => {
            let mut client = nova_ipc::IpcClient::connect(SOCKET_PATH.as_ref()).await?;
            client.send_request(&Request::Shutdown).await?;
            info!("Shutdown signal sent");
            Ok(())
        }
        _ => {
            eprintln!("Usage: nova [run|stop]");
            Ok(())
        }
    }
}

fn write_pid_file() {
    let _ = std::fs::write(PID_FILE, std::process::id().to_string());
}
fn remove_pid_file() {
    let _ = std::fs::remove_file(PID_FILE);
}
fn check_pid_file() -> bool {
    if let Ok(pid_str) = std::fs::read_to_string(PID_FILE) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        } else { false }
    } else { false }
}

/// [V4 Fix] Create a ToolRegistry for Coordinator's SubAgents.
/// Contains ONLY subagent-appropriate tools: bash, read_file, write_file, file_edit, glob, grep, browser.
/// Does NOT include delegate_complex_project (avoids circular dependency).
///
/// [V5] SubAgent BrowserTool 使用独立 Chrome Profile，避免与主 Agent 的 CDP session 冲突
fn make_subagent_tools(
    browser_chrome_path: Option<String>,
    _browser_profile_dir: Option<String>, // 忽略，主 Agent 的 profile 不适用于 SubAgent
    browser_headless: bool,
    file_tracker: nova_core::tools::SharedFileReadTracker,
) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.register_builtin(Box::new(BashTool::new(BashMode::Open)));
    tools.register_builtin(Box::new(ReadFileTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(WriteFileTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(FileEditTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(GlobTool));
    tools.register_builtin(Box::new(GrepTool));
    // [V5] SubAgent 使用独立 Chrome Profile
    let subagent_profile = format!(
        "{}/.nova/chrome-subagent-{}",
        dirs::home_dir().unwrap().display(),
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    tools.register_builtin(Box::new(BrowserTool::new(
        browser_chrome_path,
        Some(subagent_profile),
        browser_headless,
    )));
    tools
}

fn make_tools(
    mode: &str,
    browser_chrome_path: Option<String>,
    browser_profile_dir: Option<String>,
    browser_headless: bool,
    side_query: SideQuery,
    session_manager: SessionManager,
    file_tracker: nova_core::tools::SharedFileReadTracker,
    repo_root: Option<PathBuf>,
    teams_dir: Option<PathBuf>,
    api_key: String,
    api_base_url: String,
    model: String,
    skills_dir: PathBuf,
    skills: SharedSkillsLoader,
    dispatcher_tx: Arc<dispatcher::DispatcherSender>,
    shadow_tx: tokio::sync::mpsc::Sender<nova_core::models::ShadowEvent>,
    subagent_tools: Option<Arc<ToolRegistry>>,
) -> ToolRegistry {
    let bash_mode = match mode {
        "sandbox" => BashMode::Sandbox,
        _ => BashMode::Open,
    };
    let mut tools = ToolRegistry::new();
    tools.register_builtin(Box::new(BashTool::new(bash_mode)));
    tools.register_builtin(Box::new(ReadFileTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(WriteFileTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(FileEditTool::new(file_tracker.clone())));
    tools.register_builtin(Box::new(GlobTool));
    tools.register_builtin(Box::new(GrepTool));

    // Browser tool — 通过 @playwright/mcp 子进程驱动
    tools.register_builtin(Box::new(BrowserTool::new(
        browser_chrome_path,
        browser_profile_dir,
        browser_headless,
    )));

    tools.register_builtin(Box::new(AgenticSearchTool::new(side_query, session_manager)));

    // Agent tool — spawn subagents for parallel/background tasks
    // [V4 Fix] Pass shadow_tx so SubAgents can emit TaskProgress events
    tools.register_builtin(Box::new(AgentTool::new(api_key.clone(), api_base_url.clone(), model.clone()).with_shadow_tx(shadow_tx.clone())));

    // Worktree tool — git worktree isolation per session
    if let Some(root) = repo_root {
        tools.register_builtin(Box::new(WorktreeTool::new(root)));
    }

    // Team tool — team/member/task management
    if let Some(dir) = teams_dir {
        tools.register_builtin(Box::new(TeamTool::new(dir)));
    }

    // Skill tools — skill management (create/edit/patch/delete/list/view)
    tools.register_builtin(Box::new(SkillManageTool::new(skills_dir.clone(), skills.clone())));
    tools.register_builtin(Box::new(SkillsListTool::new(skills.clone())));
    tools.register_builtin(Box::new(SkillViewTool::new(skills_dir, skills)));

    // [V4 Phase 4.2] delegate_complex_project — for complex project delegation
    // [V4 Fix] Pass shadow_tx so Coordinator's SubAgents emit TaskProgress events
    // Also inject subagent_tools so Coordinator's SubAgents can execute tools
    let delegate_tool = nova_core::tools::DelegateComplexProjectTool::new(
        dispatcher_tx.clone(),
        shadow_tx.clone(),
        api_key.clone(),
        api_base_url.clone(),
        model.clone(),
    );
    let delegate_tool = if let Some(ref st) = subagent_tools {
        delegate_tool.with_tools(st.clone())
    } else {
        delegate_tool
    };
    tools.register_builtin(Box::new(delegate_tool));
    tools.register_builtin(Box::new(nova_core::tools::CancelDelegatedProjectTool::new()));

    // [FIXBUG-002] delegate_task — for Medium complexity single-task delegation
    let delegate_task_tool = nova_core::tools::DelegateTaskTool::new(
        dispatcher_tx.clone(),
        shadow_tx.clone(),
        api_key.clone(),
        api_base_url.clone(),
        model.clone(),
    );
    let delegate_task_tool = if let Some(st) = subagent_tools {
        delegate_task_tool.with_tools(st)
    } else {
        delegate_task_tool
    };
    tools.register_builtin(Box::new(delegate_task_tool));

    tools
}


pub fn make_hooks() -> HookManager {
    // T21.1: Old DualWriteMemory hooks disabled — replaced by T21 layered memory system
    HookManager::new()
}

/// Generate tool descriptions string (used by BootstrapLoader)
pub fn tool_descriptions(tools: &ToolRegistry) -> String {
    tools.describe_all()
}

async fn run_daemon(config: NovaConfig) -> Result<()> {
    if check_pid_file() {
        anyhow::bail!("Daemon already running (PID file: {})", PID_FILE);
    }
    write_pid_file();

    struct PidGuard;
    impl Drop for PidGuard {
        fn drop(&mut self) { remove_pid_file(); }
    }
    let _guard = PidGuard;

    info!("NOVA daemon starting, workspace: {:?}, mode: {}, log_level: {}",
        config.workspace, config.mode, config.log_level);

    // [V5] 检测旧 AGENTS.md 并提示迁移
    let old_agents_path = config.workspace.join("AGENTS.md");
    if old_agents_path.exists() {
        info!("检测到旧的 AGENTS.md 文件（位于 ~/.nova/AGENTS.md），该文件已被内置版本替代，可安全删除");
    }

    let run_mode = config.mode.clone();

    let skills = create_shared_loader(config.workspace.join("skills"))?;

    let mut loop_config = QueryLoopConfig {
        max_turns: config.max_turns,
        tool_timeout: std::time::Duration::from_secs(config.tool_timeout_secs),
        model: config.model.clone(),
        max_tokens: 8192,
        api_key: config.api_key.clone(),
        api_base_url: config.api_base_url.clone(),
        context_window: config.context_window,
        budget_trigger_pct: config.budget_trigger_pct,
        compact_target_pct: config.compact_target_pct,
        memories_dir: None,
        preflight_checker: None, // [V4 Phase 3.1] Set up after Dispatcher creation
    };

    let server = IpcServer::bind(SOCKET_PATH.as_ref()).await?;
    info!("Listening on {}", SOCKET_PATH);

    // Create shared file tracker for read-first safety
    let file_tracker = create_shared_tracker();

    // [V4 Phase 2.4] Create MemoryKeeper for async memory extraction
    let memory_keeper = Arc::new(MemoryKeeper::new(
        config.workspace.clone(),
        SideQuery::new(
            loop_config.api_key.clone(),
            loop_config.api_base_url.clone(),
            loop_config.model.clone(),
        ),
    ));

    // [V4 Task 6.2] Create Discord push channel (before Dispatcher so it can be passed in)
    // This channel forwards ProjectCompleted events to Discord push listener
    let (discord_push_tx, discord_push_rx) = if config.discord_enabled {
        let (tx, rx) = tokio::sync::mpsc::channel::<DiscordPush>(32);
        (Some(std::sync::Arc::new(tx)), Some(rx))
    } else {
        (None, None)
    };

    // [V4 Fix] Create IPC push channel for ProjectCompleted → TUI
    // Using broadcast channel so all TUI connections receive the notification
    let ipc_push_tx = tokio::sync::broadcast::channel::<nova_ipc::Event>(32).0;

    // [V4 Phase 2.2] Spawn the ShadowEvent dispatcher loop (with MemoryKeeper and optionally Discord push)
    let dispatcher_tx = {
        let mut d = dispatcher::Dispatcher::new(config.workspace.clone())
            .with_memory_keeper(memory_keeper);
        if let Some(ref tx) = discord_push_tx {
            d = d.with_discord_push_tx(tx.clone());
        }
        d = d.with_ipc_push_tx(Arc::new(ipc_push_tx.clone()));
        d.spawn()
    };
    info!("ShadowEvent dispatcher spawned with MemoryKeeper");

    // [V4 Phase 3.1] Set up PreFlightChecker now that we have api credentials
    // (was deferred until after Dispatcher creation per comment above)
    loop_config.preflight_checker = Some(PreFlightChecker::new(
        loop_config.api_key.clone(),
        loop_config.api_base_url.clone(),
        loop_config.model.clone(),
    ));
    info!("[V4] PreFlightChecker initialized");

    if config.discord_enabled {
        if let Some(token) = config.discord_token.clone() {
            let mut discord_push_rx = discord_push_rx.unwrap(); // Safe: we checked config.discord_enabled

            // [V4 Fix] Create subagent tools for Coordinator's SubAgents
            let tools_subagent = Arc::new(make_subagent_tools(
                config.browser_chrome_path.clone(),
                config.browser_profile_dir.clone(),
                config.browser_headless.unwrap_or(true),
                file_tracker.clone(),
            ));

            let tools_dc = Arc::new(make_tools(
                &run_mode,
                config.browser_chrome_path.clone(),
                config.browser_profile_dir.clone(),
                config.browser_headless.unwrap_or(true),
                SideQuery::new(loop_config.api_key.clone(), loop_config.api_base_url.clone(), loop_config.model.clone()),
                SessionManager::new(config.workspace.join("sessions")),
                file_tracker.clone(),
                Some(config.workspace.clone()),
                Some(config.workspace.join("teams")),
                loop_config.api_key.clone(),
                loop_config.api_base_url.clone(),
                loop_config.model.clone(),
                config.workspace.join("skills"),
                skills.clone(),
                dispatcher_tx.clone(),
                dispatcher_tx.channel(),
                Some(tools_subagent),
            ));
            let dc_cfg = Arc::new(HandleConfig {
                workspace_dir: config.workspace.clone(),
                sessions_dir: config.workspace.join("sessions"),
                memories_dir: config.workspace.clone(),
                loop_config: loop_config.clone(),
                skills: skills.clone(),
                run_mode: run_mode.clone(),
                tools: tools_dc,
                heartbeat_interval_secs: config.heartbeat_interval_secs,
                dispatcher_tx: dispatcher_tx.clone(),
                discord_push_tx: discord_push_tx.clone(),
                ipc_push_tx: None, // Discord doesn't use IPC push
            });

            // [V4 Task 6.2] Spawn Discord proactive push listener
            // Uses a standalone Http client to send messages to Discord channels
            let http = serenity::http::Http::new(&token);
            // [V4 GAP Fix] Map "coordinator" symbolic channel to configured discord_channel_id
            let discord_channel_id = config.discord_channel_id;
            tokio::spawn(async move {
                while let Some(push) = discord_push_rx.recv().await {
                    // [V4 GAP Fix] Resolve symbolic channel "coordinator" to configured channel ID
                    let resolved_channel_id = if push.channel_id == "coordinator" {
                        discord_channel_id
                    } else {
                        push.channel_id.parse::<u64>().ok()
                    };

                    if let Some(channel_id_val) = resolved_channel_id {
                        let channel_id = serenity::model::id::ChannelId::new(channel_id_val);
                        let builder = serenity::builder::CreateMessage::new()
                            .content(push.content);
                        match channel_id.send_message(&http, builder).await {
                            Ok(_) => info!("[V4] Discord push sent to channel {}", channel_id),
                            Err(e) => warn!("[V4] Discord push failed: {}", e),
                        }
                    } else {
                        warn!("[V4] Invalid Discord channel ID: {} (tried 'coordinator' mapping)", push.channel_id);
                    }
                }
            });

            tokio::spawn(async move {
                if let Err(e) = discord::start(token, dc_cfg).await {
                    error!("Discord gateway crashed: {}", e);
                }
            });
        } else {
            warn!("Discord enabled but no token provided");
        }
    }

    loop {
        match server.accept().await {
        Ok(conn) => {
                let workspace_dir = config.workspace.clone();
                let sd = config.workspace.join("sessions");
                let md = config.workspace.clone();
                let lc = loop_config.clone();
                let sk = skills.clone();
                let rm = run_mode.clone();
                let sq_for_tools = SideQuery::new(
                    lc.api_key.clone(),
                    lc.api_base_url.clone(),
                    lc.model.clone(),
                );
                let sm_for_tools = SessionManager::new(sd.clone());

                // [V4 Fix] Create subagent tools for Coordinator's SubAgents
                let tools_subagent = Arc::new(make_subagent_tools(
                    config.browser_chrome_path.clone(),
                    config.browser_profile_dir.clone(),
                    config.browser_headless.unwrap_or(true),
                    file_tracker.clone(),
                ));

                let tools = Arc::new(make_tools(
                    &run_mode,
                    config.browser_chrome_path.clone(),
                    config.browser_profile_dir.clone(),
                    config.browser_headless.unwrap_or(true),
                    sq_for_tools,
                    sm_for_tools,
                    file_tracker.clone(),
                    Some(workspace_dir.clone()),
                    Some(config.workspace.join("teams")),
                    lc.api_key.clone(),
                    lc.api_base_url.clone(),
                    lc.model.clone(),
                    config.workspace.join("skills"),
                    sk.clone(),
                    dispatcher_tx.clone(),
                    dispatcher_tx.channel(),
                    Some(tools_subagent),
                ));
                // Shadow dispatcher_tx for each connection so it can be moved into the async block
                let dispatcher_tx = dispatcher_tx.clone();
                let ipc_push_tx_clone = ipc_push_tx.clone();
                tokio::spawn(async move {
                    let cfg = HandleConfig {
                        workspace_dir,
                        sessions_dir: sd,
                        memories_dir: md,
                        loop_config: lc,
                        skills: sk,
                        run_mode: rm,
                        tools,
                        heartbeat_interval_secs: config.heartbeat_interval_secs,
                        dispatcher_tx: dispatcher_tx.clone(),
                        discord_push_tx: None, // IPC connections don't use Discord push
                        ipc_push_tx: Some(Arc::new(ipc_push_tx_clone)), // [V4 Fix] Broadcast to TUI
                    };
                    if let Err(e) = handle_connection(conn, cfg).await {
                        error!("Connection error: {}", e);
                    }
                });
            }
            Err(e) => error!("Accept error: {}", e),
        }
    }
}

async fn handle_connection(
    mut conn: nova_ipc::IpcConnection,
    cfg: HandleConfig,
) -> Result<()> {
    let workspace_dir = cfg.workspace_dir;
    let sessions_dir = cfg.sessions_dir;
    let memories_dir = cfg.memories_dir;
    let mut loop_config = cfg.loop_config;
    let skills = cfg.skills;
    let _run_mode = cfg.run_mode;
    let tools = cfg.tools;
    let dispatcher_tx = cfg.dispatcher_tx.clone();
    let ipc_push_tx = cfg.ipc_push_tx.clone();
    let session_mgr = SessionManager::new(sessions_dir.clone());

    // BootstrapLoader: hot-reloads workspace files with mtime caching
    let bootstrap = Arc::new(Mutex::new(BootstrapLoader::new(workspace_dir.clone())));
    let tool_desc = tool_descriptions(&tools);

    // DailyNotes: Layer 2 episodic memory (T21.1)
    let daily_notes = DailyNotes::new(memories_dir.clone());

    // SideQuery for diary generation and recall (T21.1 + T21.2)
    let side_query = SideQuery::new(
        loop_config.api_key.clone(),
        loop_config.api_base_url.clone(),
        loop_config.model.clone(),
    );

    // MemoryRecall for diary recall (T21.2)
    let recall_session_mgr = SessionManager::new(sessions_dir.clone());
    let memory_recall = MemoryRecall::new(memories_dir.clone(), side_query.clone(), recall_session_mgr);

    // DreamEngine for periodic memory consolidation (T21.4)
    let dream_sq = SideQuery::new(
        loop_config.api_key.clone(),
        loop_config.api_base_url.clone(),
        loop_config.model.clone(),
    );
    let dream_engine = Arc::new(DreamEngine::new(
        workspace_dir.clone(),
        memories_dir.clone(),
        dream_sq,
    ));

    // Pass memories_dir to QueryLoop for diary writing
    loop_config.memories_dir = Some(memories_dir.clone());

    // T23: MemoryConsolidator for Layer 1 idle-time dual-write mutex
    let consolidate_sq = SideQuery::new(
        loop_config.api_key.clone(),
        loop_config.api_base_url.clone(),
        loop_config.model.clone(),
    );
    let consolidator = Arc::new(MemoryConsolidator::new(
        workspace_dir.clone(),
        consolidate_sq,
    ));

    // v2 Phase 1.5: TopicTracker, TensionTracker, ModeRouter, MemoryBoard
    let tension_tracker = std::sync::Arc::new(nova_core::memory::TensionTracker::new());
    let topic_tracker = std::sync::Arc::new(tokio::sync::RwLock::new(nova_core::memory::TopicTracker::new()));
    let mode_router = std::sync::Arc::new(tokio::sync::RwLock::new(nova_core::memory::ModeRouter::new(
        tension_tracker.clone(),
    )));
    let memory_board = std::sync::Arc::new(tokio::sync::RwLock::new(nova_core::memory::MemoryBoard::new(
        workspace_dir.join("MEMORY.md"),
    )));

    let mut session = match session_mgr.resume_latest()? {
        Some(s) => {
            conn.send_event(&Event::SessionRestored {
                session_id: s.session_id.clone(),
                message_count: s.messages.len(),
            }).await?;
            s
        }
        None => {
            let s = session_mgr.create(loop_config.max_turns)?;
            conn.send_event(&Event::SessionCreated {
                session_id: s.session_id.clone(),
            }).await?;
            s
        }
    };

    // [V4 Fix] Create IPC push receiver for ProjectCompleted notifications
    let mut ipc_push_rx = if let Some(ref tx) = ipc_push_tx {
        Some(tx.subscribe())
    } else {
        None
    };

    // [V4 Fix] Spawn IPC push forwarder: listens to broadcast and sends to conn
    let writer = conn.clone_writer();
    if let Some(mut rx) = ipc_push_rx {
        tokio::spawn(async move {
            info!("[IPC Push] Forwarder started, waiting for ProjectCompleted events");
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        info!("[IPC Push] Forwarding event to TUI: {:?}", event);
                        let mut data = serde_json::to_string(&event).unwrap();
                        data.push('\n');
                        let mut w = writer.lock().await;
                        if w.write_all(data.as_bytes()).await.is_err() {
                            warn!("[IPC Push] Write failed, connection closed");
                            break; // Connection closed
                        }
                        w.flush().await.ok();
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("[IPC Push] Broadcast channel closed, forwarder stopping");
                        break;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("[IPC Push] Forwarder lagged {} messages", n);
                    }
                }
            }
        });
    }

    // Heartbeat: load HEARTBEAT.md and start the scheduler (background, detached).
    // Events are logged only — TUI forwarding requires future tokio::select! refactor.
    if let Ok(content) = std::fs::read_to_string(workspace_dir.join("HEARTBEAT.md")) {
        let interval = std::time::Duration::from_secs(cfg.heartbeat_interval_secs);
        let scheduler = HeartbeatScheduler::from_config(&content, interval);
        if !scheduler.tasks().is_empty() {
            let (tx, mut rx) = mpsc::channel::<HeartbeatEvent>(8);
            let _handle = scheduler.start(tx);
            tokio::spawn(async move {
                while let Some(hb) = rx.recv().await {
                    info!("[Heartbeat] {} — {}", hb.task_name, hb.prompt);
                }
            });
        }
    }

    while let Some(req) = conn.recv_request().await? {
        match req {
            Request::UserMessage { content } => {
                info!("Received UserMessage ({} chars)", content.len());
                tracing::debug!("UserMessage content: {}", content);
                // T23: Idle-time consolidation — if >15min since last activity
                // and there are unswept messages, consolidate before processing new input.
                // This captures the "implicit context switch" when user returns after a break.
                {
                    let idle_secs = (chrono::Utc::now() - session.updated_at).num_seconds();

                    // [V4 Task 6.1] SystemIdle — emit when idle >= 60 seconds
                    if idle_secs >= 60 {
                        info!("[V4] SystemIdle detected ({}s idle), emitting event", idle_secs);
                        dispatcher_tx.emit(ShadowEvent::SystemIdle {
                            duration_secs: idle_secs as u64,
                            transcript: session.messages.clone(),
                        });
                    }

                    let has_unswept = session.last_memory_sweep_index < session.messages.len();
                    if idle_secs > 900 && has_unswept {
                        info!(
                            "T23: Idle detected ({}s), running consolidation on {} unswept messages",
                            idle_secs,
                            session.messages.len() - session.last_memory_sweep_index,
                        );
                        let cons = (*consolidator).clone();
                        match cons.consolidate(
                            &session.messages,
                            session.last_memory_sweep_index,
                            session.memory_updated_mutex,
                        ).await {
                            Ok(_) => {
                                session.last_memory_sweep_index = session.messages.len();
                                session.memory_updated_mutex = false;
                                session_mgr.save_meta(&session)?;
                            }
                            Err(e) => warn!("T23: Idle consolidation failed: {}", e),
                        }
                    }
                }

                // Skill injection
                let content = if let Ok(skills_guard) = skills.lock() {
                    if content.starts_with('/') {
                        let skill_name = content.trim_start_matches('/').split_whitespace().next().unwrap_or("");
                        if let Some(skill) = skills_guard.find_by_name(skill_name) {
                            format!("{}\n\n{}", skill.prompt, content)
                        } else {
                            content.clone()
                        }
                    } else {
                        let matched = skills_guard.match_auto_trigger(&content);
                        if let Some(skill) = matched.first() {
                            format!("{}\n\n{}", skill.prompt, content)
                        } else {
                            content.clone()
                        }
                    }
                } else {
                    warn!("skills lock poisoned");
                    content.clone()
                };

                let msg = nova_core::message::Message::user(&content);
                session_mgr.append_message(&mut session, msg)?;

                // T21.4: Record MEMORY.md mtime at turn start (for Dream dual-write mutex)
                record_memory_mtime(&mut session, &workspace_dir);

                // Hot-reload system prompt from workspace files (mtime cached)
                let mut sp = bootstrap.lock().await.build_system_prompt(&tool_desc);

                // [V4 DEPRECATED] v2 Phase 2: Inject <nova_os> thinking pipe hints into system prompt
                // <nova_os> is deprecated - state machine interception now handles this in Rust side
                // let nova_os = build_nova_os_section(
                //     mode_router.clone(),
                //     tension_tracker.clone(),
                //     topic_tracker.clone(),
                // ).await;
                // if !nova_os.is_empty() {
                //     sp.push_str("\n\n---\n\n");
                //     sp.push_str(&nova_os);
                // }

                // Auto-search: inject relevant history session context
                if content.chars().count() > 5 {
                    let sq = SideQuery::new(
                        loop_config.api_key.clone(),
                        loop_config.api_base_url.clone(),
                        loop_config.model.clone(),
                    );
                    let search_mgr = SessionManager::new(sessions_dir.clone());
                    let searcher = AgenticSessionSearch::new(sq, search_mgr);

                    let search_fut = searcher.search(&content);
                    let recall_fut = memory_recall.recall(&content, 3);

                    let (search_res, recall_res) = tokio::join!(
                        tokio::time::timeout(std::time::Duration::from_secs(15), search_fut),
                        tokio::time::timeout(std::time::Duration::from_secs(15), recall_fut),
                    );

                    match search_res {
                        Ok(Ok(results)) if !results.is_empty() => {
                            // Dynamic injection: use up to 5% of context window for history
                            let max_inject_chars = (loop_config.context_window / 20).max(1000);
                            let per_session_chars = max_inject_chars / 3;

                            let mut ctx = String::from("\n\n<relevant_history>\nExcerpts from previous conversations:\n");
                            for (i, r) in results.iter().take(3).enumerate() {
                                let excerpt: String = r.transcript.chars().take(per_session_chars).collect();
                                ctx.push_str(&format!(
                                    "\n--- Session {} ---\nUser: {}\nExcerpt: {}\n",
                                    i + 1, r.first_message, excerpt
                                ));
                            }
                            ctx.push_str("</relevant_history>");
                            sp.push_str(&ctx);
                            info!("Auto-search injected {} session contexts (~{} chars)", results.len().min(3), ctx.len());
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => info!("Auto-search failed (non-fatal): {}", e),
                        Err(_) => info!("Auto-search timed out, skipping"),
                    }

                    // T21.2: Memory recall — inject relevant daily diary entries
                    match recall_res {
                        Ok(Ok(injection)) if !injection.is_empty() => {
                            sp.push_str(&injection);
                            info!("Memory recall injected diary context (~{} chars)", injection.len());
                        }
                        Ok(Ok(_)) | Ok(Err(_)) => {}
                        Err(_) => info!("Memory recall timed out, skipping"),
                    }
                }

                let (event_tx, mut event_rx) = mpsc::channel::<LoopEvent>(64);
                // [V4 Task 3.2] Get shadow event sender for QueryLoop
                let shadow_tx = dispatcher_tx.channel();
                let lc = loop_config.clone();
                let ctx_window = lc.context_window;
                let dn = daily_notes.clone();
                let sq_loop = side_query.clone();
                let session_clone = session.clone();
                let tools_clone = tools.clone();
                let cons = consolidator.clone();
                let tt = topic_tracker.clone();
                let tens = tension_tracker.clone();
                let mr = mode_router.clone();
                let mb = memory_board.clone();

                let loop_handle = tokio::spawn(async move {
                    let hooks = make_hooks();
                    let cons_inner = Arc::try_unwrap(cons)
                        .unwrap_or_else(|arc| (*arc).clone());
                    let ql = QueryLoop::new(
                        tools_clone, hooks, lc, Some(dn), Some(sq_loop),
                        Some(cons_inner),
                        Some(tt), Some(tens), /* mr disabled */ Some(mb),
                        Some(shadow_tx),
                    );
                    let mut s = session_clone;
                    s.turn_count = 0;
                    
                    let session_id_str = s.session_id.clone();
                    nova_core::tools::CURRENT_CHANNEL_ID.scope(session_id_str, async move {
                        let result = ql.run_turn(s.clone(), &sp, event_tx).await;
                        match result {
                            Ok((updated_s, new_msgs, preflight)) => (updated_s, new_msgs, preflight),
                            Err(_) => (s, vec![], None),
                        }
                    }).await
                });

                while let Some(event) = event_rx.recv().await {
                    let ipc_event = match event {
                        LoopEvent::TextDelta(t) => Event::TextDelta { content: t },
                        LoopEvent::ToolCallStart { id, name } => Event::ToolCallStart { id, name },
                        LoopEvent::ToolCallResult { id, content } => Event::ToolCallResult { id, content },
                        LoopEvent::TurnEnd => Event::TurnEnd,
                        LoopEvent::TokenUsage { input, output } => Event::TokenUsage {
                            input, output, budget_pct: budget_pct_calc(input as usize, ctx_window),
                        },
                        LoopEvent::CompactTriggered => Event::Notification {
                            message: "Compacting conversation...".into(),
                        },
                        LoopEvent::Error(e) => Event::Error { message: e },
                    };
                    conn.send_event(&ipc_event).await.ok();
                }

                if let Ok((updated, new_msgs, preflight_result)) = loop_handle.await {
                    session = updated;

                    // T3.2: TopicShift → ShadowEvent::TopicArchived
                    if preflight_result.as_ref().map(|p| p.topic_shift).unwrap_or(false) {
                        info!("Topic shift detected, emitting TopicArchived event");
                        dispatcher_tx.emit(ShadowEvent::TopicArchived {
                            transcript: session.messages.clone(),
                        });
                    }

                    let history = session_mgr.history_for(&session);
                    // Append ONLY newly added messages instead of relying on `[old_len..]`
                    // since compaction might reduce the total length in memory!
                    for msg in &new_msgs {
                        let _ = history.append(msg);
                    }

                    // T23: Detect if any tool call wrote to MEMORY.md
                    // Scan new_msgs for tool results that indicate MEMORY.md was modified
                    let memory_written = new_msgs.iter().any(|m| {
                        // Check assistant tool_calls for write_file/file_edit targeting MEMORY.md
                        if let Some(tcs) = &m.tool_calls {
                            tcs.iter().any(|tc| {
                                (tc.name == "write_file" || tc.name == "file_edit")
                                    && tc.arguments.to_string().contains("MEMORY.md")
                            })
                        } else {
                            false
                        }
                    });
                    if memory_written {
                        session.memory_updated_mutex = true;
                        info!("T23: Detected MEMORY.md write by LLM, mutex set");
                    }

                    session_mgr.save_meta(&session)?;

                    // T21.4: Check dream trigger after each turn
                    if dream_engine.should_dream() {
                        let de = dream_engine.clone();
                        let mtime = session.token_stats.memory_mtime;
                        tokio::spawn(async move {
                            if let Err(e) = de.dream(mtime).await {
                                warn!("Dream consolidation failed: {}", e);
                            } else {
                                info!("Dream consolidation completed");
                            }
                        });
                    }
                }
            }
            Request::NewSession => {
                // T21.1: Write session summary diary before ending current session
                if session.messages.len() > 2 {
                    let summary_session = session.clone();
                    let dn = daily_notes.clone();
                    let sq = side_query.clone();
                    tokio::spawn(async move {
                        if let Err(e) = write_session_diary(&dn, &sq, &summary_session).await {
                            warn!("Failed to write session diary: {}", e);
                        }
                    });
                }
                session = session_mgr.create(session.max_turns)?;
                conn.send_event(&Event::SessionCreated {
                    session_id: session.session_id.clone(),
                }).await?;
            }
            Request::SearchSessions { query } => {
                let sq = SideQuery::new(
                    loop_config.api_key.clone(),
                    loop_config.api_base_url.clone(),
                    loop_config.model.clone(),
                );
                let search_mgr = SessionManager::new(sessions_dir.clone());
                let searcher = AgenticSessionSearch::new(sq, search_mgr);
                match searcher.search(&query).await {
                    Ok(results) => {
                        let entries: Vec<nova_ipc::protocol::SearchResultEntry> = results.iter()
                            .map(|r| nova_ipc::protocol::SearchResultEntry {
                                session_id: r.session_id.clone(),
                                title: r.first_message.chars().take(80).collect(),
                                message_count: r.message_count,
                            })
                            .collect();
                        conn.send_event(&Event::SearchResults { results: entries }).await?;
                    }
                    Err(e) => {
                        conn.send_event(&Event::Error {
                            message: format!("Search failed: {}", e),
                        }).await?;
                    }
                }
            }
            Request::ResumeSession => {
                if let Some(s) = session_mgr.resume_latest()? {
                    conn.send_event(&Event::SessionRestored {
                        session_id: s.session_id.clone(),
                        message_count: s.messages.len(),
                    }).await?;
                    session = s;
                }
            }
            Request::Orchestrate { task } => {
                info!("Orchestration requested: {}", task);
                // [V4 Fix] Pass shadow_tx so Coordinator's SubAgents emit TaskProgress events
                // Note: tools=None here since this is a direct API path, not through DelegateComplexProjectTool
                let coordinator = Coordinator::new(
                    loop_config.api_key.clone(),
                    loop_config.api_base_url.clone(),
                    loop_config.model.clone(),
                    String::new(), // system_prompt empty
                    Some(dispatcher_tx.channel()),
                    None, // [V4 Fix] No tools for direct Orchestrate API
                );
                match coordinator.orchestrate(&task).await {
                    Ok(result) => {
                        conn.send_event(&Event::Notification {
                            message: format!("[Coordinator] Output:\n{}", result.output),
                        }).await?;
                    }
                    Err(e) => {
                        conn.send_event(&Event::Error {
                            message: format!("Orchestration failed: {}", e),
                        }).await?;
                    }
                }
            }
            Request::Shutdown => {
                // T21.1: Write session diary before shutdown
                if session.messages.len() > 2 {
                    let summary_session = session.clone();
                    let dn = daily_notes.clone();
                    let sq = side_query.clone();
                    tokio::spawn(async move {
                        if let Err(e) = write_session_diary(&dn, &sq, &summary_session).await {
                            warn!("Failed to write session diary on shutdown: {}", e);
                        }
                    });
                }
                session_mgr.save_meta(&session)?;
                info!("Shutdown requested");
                std::process::exit(0);
            }
        }
    }

    // T21.1: Write session diary on TUI disconnect (connection closed)
    if session.messages.len() > 2 {
        let summary_session = session.clone();
        let dn = daily_notes.clone();
        let sq = side_query.clone();
        tokio::spawn(async move {
            if let Err(e) = write_session_diary(&dn, &sq, &summary_session).await {
                warn!("Failed to write session diary on disconnect: {}", e);
            }
        });
    }

    session_mgr.save_meta(&session)?;
    Ok(())
}

// T21.4: Record MEMORY.md mtime at the start of each turn.
/// Used by Dream to detect if LLM modified MEMORY.md during this turn.
pub fn record_memory_mtime(session: &mut nova_core::session::manager::Session, workspace_dir: &Path) {
    let memory_path = workspace_dir.join("MEMORY.md");
    if let Ok(meta) = std::fs::metadata(&memory_path) {
        if let Ok(mtime) = meta.modified() {
            session.token_stats.memory_mtime = Some(mtime);
        }
    }
}

const MAP_CHUNK_SIZE: usize = 180_000; // ~180K chars per Map batch

/// Preprocess session: keep user original + assistant decisions, strip tool details.
fn preprocess_session(messages: &[nova_core::message::Message]) -> String {
    let mut lines = Vec::new();
    for m in messages {
        match m.role {
            nova_core::message::Role::User => {
                if let Some(c) = &m.content {
                    let c = c.trim();
                    if !c.is_empty() {
                        lines.push(format!("User: {}", c));
                    }
                }
            }
            nova_core::message::Role::Assistant => {
                // Keep content (decisions/reasoning), skip tool_calls
                if let Some(c) = &m.content {
                    let c = c.trim();
                    if !c.is_empty() {
                        lines.push(format!("Assistant: {}", c));
                    }
                }
            }
            _ => {}
        }
    }
    lines.join("\n")
}

/// Map phase: extract key points from one chunk by dimension.
async fn map_chunk(sq: &SideQuery, chunk: &str) -> anyhow::Result<String> {
    let system = "You are a key-point extractor. Given a conversation chunk, \
extract important information along these dimensions:
- 事件 (events that happened)
- 反馈 (user feedback / opinions)
- 用户偏好 (user preferences)
- 项目状态 (project status / decisions)
- 重要决策 (key decisions made)
- 参考资料 (references, links, configurations)

Output a concise list of key points, one per line, in Chinese. \
If a dimension has no information, skip it. Do not add explanatory text.";

    sq.query_await(system, chunk).await
}

/// Reduce phase: combine all map results into ~200 char final summary.
async fn reduce_summaries(sq: &SideQuery, map_results: &[String]) -> anyhow::Result<String> {
    let combined = map_results.join("\n\n");
    let system = "你是一名会话记录员。根据以下会话要点，写一段 100-200 字的中文总结，要有头有尾，连贯自然。

重点记录：
- 发生了什么（事件）
- 用户说了什么、反馈如何
- 项目进展或重要决策
- 用户的偏好或习惯

要求：
- 用完整的句子叙述，不是罗列要点
- 一口气说完，不要分段
- 100-200 字为宜
- 只输出中文总结，不加标签不加格式";

    sq.query_await(system, &combined).await
}

pub async fn write_session_diary(
    daily: &DailyNotes,
    sq: &SideQuery,
    session: &nova_core::session::manager::Session,
) -> anyhow::Result<()> {
    // Step 1: preprocess — keep user + assistant decisions, strip tool calls
    let preprocessed = preprocess_session(&session.messages);
    if preprocessed.len() < 10 {
        return Ok(());
    }

    // Step 2: map phase — split into ~180K chunks
    let mut map_results = Vec::new();
    for chunk in preprocessed.chars().collect::<Vec<_>>().chunks(MAP_CHUNK_SIZE) {
        let chunk_str: String = chunk.iter().collect();
        match map_chunk(sq, &chunk_str).await {
            Ok(result) if !result.trim().is_empty() => {
                map_results.push(result);
            }
            Ok(_) => {}
            Err(e) => {
                warn!("Map chunk failed: {}", e);
            }
        }
    }

    if map_results.is_empty() {
        return Ok(());
    }

    // Step 3: reduce phase — combine all map results into final ~200 char summary
    let final_summary = reduce_summaries(sq, &map_results).await?;
    if final_summary.trim().is_empty() {
        return Ok(());
    }

    // Step 4: write to diary
    daily.append_session(&final_summary)?;
    info!("Session diary written: {} chars ({} map chunks)", final_summary.len(), map_results.len());
    Ok(())
}

/// v2 Phase 2: Build <nova_os> thinking pipe section for system prompt injection.
/// Reads current mode and tension from trackers and formats them as <nova_os> XML block.
async fn build_nova_os_section(
    mode_router: std::sync::Arc<tokio::sync::RwLock<nova_core::memory::ModeRouter>>,
    tension_tracker: std::sync::Arc<nova_core::memory::TensionTracker>,
    topic_tracker: std::sync::Arc<tokio::sync::RwLock<nova_core::memory::TopicTracker>>,
) -> String {
    use nova_core::memory::Mode;

    let mode = mode_router.read().await.current_mode().await;
    let tension = tension_tracker.current_tension().await;
    let current_topic = topic_tracker.read().await.current_topic().await;

    let topic_name = current_topic
        .as_ref()
        .map(|t| t.name.clone())
        .unwrap_or_else(|| "（无进行中话题）".to_string());

    let topic_status = current_topic
        .as_ref()
        .map(|t| match t.status {
            nova_core::memory::TopicStatus::Started => "开始",
            nova_core::memory::TopicStatus::Active => "活跃",
            nova_core::memory::TopicStatus::Suspended => "挂起",
            nova_core::memory::TopicStatus::Archived => "归档",
        })
        .unwrap_or("无");

    let mode_str = match mode {
        Mode::Normal => "Normal",
        Mode::SoftIntimate => "SoftIntimate",
        Mode::HighIntimate => "HighIntimate",
        Mode::Cooling => "Cooling",
    };

    format!(r#"<nova_os>
## 话题生命周期
当前话题：{} [{}]

## 用户状态
张力值：{}/100
模式：{}

## 响应策略
根据上述状态，决定：
1. 回复长度（短句/中句/长句）
2. 语气风格（简洁/温和/关怀）
3. 是否需要触发主动机制
</nova_os>"#, topic_name, topic_status, tension, mode_str)
}
