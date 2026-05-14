use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, broadcast, Mutex};
use tracing::{info, error, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use nova_agent::{QueryLoop, QueryLoopConfig, LoopEvent};
use nova_core::config::NovaConfig;
use nova_core::llm_backend::LlmBackend;

use nova_llm::client::ApiClient;
use nova_memory::memory::consolidate::MemoryConsolidator;
use nova_memory::memory::daily::DailyNotes;
use nova_memory::session::manager::SessionManager;
use nova_memory::session::AgenticSessionSearch;
use nova_memory::sidequery::SideQuery;
use nova_tools::skills::{create_shared_loader, SharedSkillsLoader};
use nova_agent::heartbeat::{HeartbeatScheduler};
use nova_agent::heartbeat::scheduler::HeartbeatEvent;
use nova_tools::{ToolRegistry, ToolContext, create_shared_tracker};
use nova_agent::workspace::BootstrapLoader;
use nova_ipc::{IpcServer, Event, Request};

const SOCKET_PATH: &str = "/tmp/nova.sock";
const PID_FILE: &str = "/tmp/nova.pid";

mod agentic_search;
mod discord;
mod discord_adapter;
mod tool_factory;
mod session_diary;

// Re-use library crate modules
use nova_daemon::dispatcher;
use nova_daemon::DiscordPush;

pub struct HandleConfig {
    workspace_dir: PathBuf,
    sessions_dir: PathBuf,
    memories_dir: PathBuf,
    loop_config: QueryLoopConfig,
    skills: SharedSkillsLoader,
    tools: Arc<ToolRegistry>,
    heartbeat_interval_secs: u64,
    /// Shared LLM backend for all QueryLoop construction
    llm_backend: Arc<dyn LlmBackend>,
    /// [V4 Task 6.2] Optional channel for Discord proactive push.
    #[allow(dead_code)]
    discord_push_tx: Option<std::sync::Arc<tokio::sync::mpsc::Sender<DiscordPush>>>,
    /// [V4 Fix] Broadcast sender for IPC push events to TUI
    ipc_push_tx: Option<std::sync::Arc<tokio::sync::broadcast::Sender<nova_ipc::Event>>>,
    /// Whether to show Discord tool approval buttons (default: true)
    tool_approval_enabled: bool,
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
        .add_directive("chromiumoxide=error".parse().unwrap())
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

    // [V2 Task System] Sweep orphaned [Running] tasks from previous crash
    if let Err(e) = nova_tools::task::TaskLogger::sweep_orphans(&config.workspace).await {
        warn!("[V2 Task] sweep_orphans failed: {}", e);
    } else {
        info!("[V2 Task] sweep_orphans completed");
    }

    // [V5] 检测旧 AGENTS.md 并提示迁移
    let old_agents_path = config.workspace.join("AGENTS.md");
    if old_agents_path.exists() {
        info!("检测到旧的 AGENTS.md 文件（位于 ~/.nova/AGENTS.md），该文件已被内置版本替代，可安全删除");
    }

    let run_mode = config.mode.clone();

    let skills = create_shared_loader(config.workspace.join("skills"))?;

    // Create shared LLM backend — single Arc used by all QueryLoop/SideQuery construction
    let llm_backend: Arc<dyn LlmBackend> = Arc::new(ApiClient::new(
        config.api_key.clone(),
        config.api_base_url.clone(),
    ));

    let loop_config = QueryLoopConfig {
        max_turns: config.max_turns,
        tool_timeout: std::time::Duration::from_secs(config.tool_timeout_secs),
        model: config.model.clone(),
        max_tokens: 8192,
        context_window: config.context_window,
        budget_trigger_pct: config.budget_trigger_pct,
        compact_target_pct: config.compact_target_pct,
        memories_dir: None,
    };

    let server = IpcServer::bind(SOCKET_PATH.as_ref()).await?;
    info!("Listening on {}", SOCKET_PATH);

    // Create shared file tracker for read-first safety
    let file_tracker = create_shared_tracker();

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

    // Spawn the ShadowEvent dispatcher loop (with optionally Discord push)
    {
        let mut d = dispatcher::Dispatcher::new(config.workspace.clone());
        if let Some(ref tx) = discord_push_tx {
            d = d.with_discord_push_tx(tx.clone());
        }
        d = d.with_ipc_push_tx(Arc::new(ipc_push_tx.clone()));
        d.spawn();
    }
    info!("ShadowEvent dispatcher spawned");

    if config.discord_enabled {
        if let Some(token) = config.discord_token.clone() {
            let mut discord_push_rx = discord_push_rx.unwrap(); // Safe: we checked config.discord_enabled

            let mut tools_dc = tool_factory::make_tools(
                &run_mode,
                config.browser_chrome_path.clone(),
                config.browser_profile_dir.clone(),
                config.browser_headless.unwrap_or(true),
                file_tracker.clone(),
                Some(config.workspace.clone()),
                Some(config.workspace.join("teams")),
                config.workspace.join("skills"),
                skills.clone(),
            );
            // Register agentic search tool (needs SideQuery from daemon)
            let sq_dc = nova_memory::sidequery::SideQuery::new(llm_backend.clone(), loop_config.model.clone());
            let sm_dc = SessionManager::new(config.workspace.join("sessions"));
            tools_dc.register_builtin(Box::new(tool_factory::make_agentic_search_tool(sq_dc, sm_dc)));
            let tools_dc = Arc::new(tools_dc);
            let dc_cfg = Arc::new(HandleConfig {
                workspace_dir: config.workspace.clone(),
                sessions_dir: config.workspace.join("sessions"),
                memories_dir: config.workspace.clone(),
                loop_config: loop_config.clone(),
                skills: skills.clone(),
                tools: tools_dc,
                heartbeat_interval_secs: config.heartbeat_interval_secs,
                llm_backend: llm_backend.clone(),
                discord_push_tx: discord_push_tx.clone(),
                ipc_push_tx: None, // Discord doesn't use IPC push
                tool_approval_enabled: config.tool_approval_enabled,
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
                        let chars: Vec<char> = push.content.chars().collect();
                        for chunk in chars.chunks(1950) {
                            let chunk_str: String = chunk.iter().collect();
                            let builder = serenity::builder::CreateMessage::new().content(chunk_str);
                            match channel_id.send_message(&http, builder).await {
                                Ok(_) => info!("[V4] Discord push chunk sent to channel {}", channel_id),
                                Err(e) => {
                                    warn!("[V4] Discord push chunk failed: {}", e);
                                    break;
                                }
                            }
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

                let mut tools = tool_factory::make_tools(
                    &run_mode,
                    config.browser_chrome_path.clone(),
                    config.browser_profile_dir.clone(),
                    config.browser_headless.unwrap_or(true),
                    file_tracker.clone(),
                    Some(workspace_dir.clone()),
                    Some(config.workspace.join("teams")),
                    config.workspace.join("skills"),
                    sk.clone(),
                );
                // Register agentic search tool (needs SideQuery from daemon)
                let sq_ipc = nova_memory::sidequery::SideQuery::new(llm_backend.clone(), lc.model.clone());
                let sm_ipc = SessionManager::new(sd.clone());
                tools.register_builtin(Box::new(tool_factory::make_agentic_search_tool(sq_ipc, sm_ipc)));
                let tools = Arc::new(tools);
                let ipc_push_tx_clone = ipc_push_tx.clone();
                let backend_clone = llm_backend.clone();
                tokio::spawn(async move {
                    let cfg = HandleConfig {
                        workspace_dir,
                        sessions_dir: sd,
                        memories_dir: md,
                        loop_config: lc,
                        skills: sk,
                        tools,
                        heartbeat_interval_secs: config.heartbeat_interval_secs,
                        llm_backend: backend_clone,
                        discord_push_tx: None, // IPC connections don't use Discord push
                        ipc_push_tx: Some(Arc::new(ipc_push_tx_clone)), // [V4 Fix] Broadcast to TUI
                        tool_approval_enabled: false, // IPC/TUI has no Discord button UI
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
    let tools = cfg.tools;
    let ipc_push_tx = cfg.ipc_push_tx.clone();
    let llm_backend = cfg.llm_backend.clone();
    let session_mgr = SessionManager::new(sessions_dir.clone());

    // Task registry (global singleton shared with Discord path)
    let task_registry = nova_core::executor::registry::TaskRegistry::global();

    // BootstrapLoader: hot-reloads workspace files with mtime caching
    let bootstrap = Arc::new(Mutex::new(BootstrapLoader::new(workspace_dir.clone())));
    let tool_desc = tool_factory::tool_descriptions(&tools);

    // DailyNotes: Layer 2 episodic memory (T21.1)
    let daily_notes = DailyNotes::new(memories_dir.clone());

    // SideQuery for diary generation and session search
    let side_query = SideQuery::new(llm_backend.clone(), loop_config.model.clone());

    // Pass memories_dir to QueryLoop for diary writing
    loop_config.memories_dir = Some(memories_dir.clone());

    // T23: MemoryConsolidator for Layer 1 idle-time dual-write mutex
    let consolidator = Arc::new(MemoryConsolidator::new(
        workspace_dir.clone(),
        SideQuery::new(llm_backend.clone(), loop_config.model.clone()),
    ));

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
    let ipc_push_rx = ipc_push_tx.as_ref().map(|tx| tx.subscribe());

    // [V6 Fix] Channel to inject background completion events into the main session loop
    let (inject_tx, mut inject_rx) = tokio::sync::mpsc::channel::<Event>(32);

    if let Some(mut rx) = ipc_push_rx {
        let writer = conn.clone_writer();
        tokio::spawn(async move {
            info!("[IPC Push] Forwarder started, waiting for ProjectCompleted events");
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        info!("[IPC Push] Forwarding event to TUI: {:?}", event);
                        
                        // 1. Notify main loop to inject into Session and Auto-Trigger AI
                        let _ = inject_tx.send(event.clone()).await;

                        // 2. Send to TUI via socket (Skip ProjectCompleted so the TUI doesn't render the raw System block)
                        if !matches!(event, Event::ProjectCompleted { .. }) {
                            let mut data = serde_json::to_string(&event).unwrap();
                            data.push('\n');
                            let mut w = writer.lock().await;
                            if w.write_all(data.as_bytes()).await.is_err() {
                                warn!("[IPC Push] Write failed, connection closed");
                                break; // Connection closed
                            }
                            w.flush().await.ok();
                        }
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

    loop {
        #[allow(unused_assignments)]
        let mut req_to_process = None;
        tokio::select! {
            req_res = conn.recv_request() => {
                match req_res {
                    Ok(Some(req)) => req_to_process = Some(req),
                    Ok(None) | Err(_) => break, // Connection closed
                }
            }
            Some(ipc_event) = inject_rx.recv() => {
                if let Event::ProjectCompleted { report, project_id } = ipc_event {
                    let msg_content = format!("<system_notification>\n后台任务 {} 执行完毕。以下是执行结果报告：\n\n{}\n\n请立刻以你的名义，用自然语言向我简述/汇报上述结果（如果报告已经排版得很好，可以直接原样输出，但要以你的口吻开头）。\n</system_notification>", project_id, report);
                    req_to_process = Some(Request::UserMessage { content: msg_content });
                    info!("Auto-triggering AI to report background task {}", project_id);
                } else {
                    continue;
                }
            }
        }

        match req_to_process.unwrap() {
            Request::UserMessage { content } => {
                info!("Received UserMessage ({} chars)", content.len());
                tracing::debug!("UserMessage content: {}", content);
                // T23: Idle-time consolidation — if >15min since last activity
                // and there are unswept messages, consolidate before processing new input.
                // This captures the "implicit context switch" when user returns after a break.
                {
                    let idle_secs = (chrono::Utc::now() - session.updated_at).num_seconds();

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
                session_diary::record_memory_mtime(&mut session, &workspace_dir);

                // Hot-reload system prompt from workspace files (mtime cached)
                let mut sp = bootstrap.lock().await.build_system_prompt(&tool_desc);


                // Auto-search: inject relevant history session context
                if content.chars().count() > 5 {
                    let sq = SideQuery::new(
                        llm_backend.clone(),
                        loop_config.model.clone(),
                    );
                    let search_mgr = SessionManager::new(sessions_dir.clone());
                    let searcher = AgenticSessionSearch::new(sq, search_mgr);

                    let search_fut = searcher.search(&content);

                    match tokio::time::timeout(std::time::Duration::from_secs(15), search_fut).await {
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
                }

                let (event_tx, mut event_rx) = mpsc::channel::<LoopEvent>(64);
                let lc = loop_config.clone();
                let ctx_window = lc.context_window;
                let dn = daily_notes.clone();
                let sq_loop = side_query.clone();
                let session_clone = session.clone();
                let tools_clone = tools.clone();
                let cons = consolidator.clone();
                let session_id_str = session.session_id.clone();
                let workspace_dir_clone = workspace_dir.clone();
                let backend_for_loop = llm_backend.clone();

                let loop_handle = tokio::spawn(async move {
                    let hooks = tool_factory::make_hooks(workspace_dir_clone.clone());
                    let cons_inner = Arc::try_unwrap(cons)
                        .unwrap_or_else(|arc| (*arc).clone());
                    let tool_ctx = ToolContext::new(
                        session_id_str.clone(),
                        Some(workspace_dir_clone),
                    );
                    let ql = QueryLoop::new(
                        backend_for_loop, tools_clone, hooks, lc, Some(dn), Some(sq_loop),
                        Some(cons_inner),
                        None, // notify_tx — no executor notifications from daemon yet
                        tool_ctx,
                    );
                    let mut s = session_clone;
                    s.turn_count = 0;

                    let result = ql.run_turn(s.clone(), &sp, event_tx).await;
                    match result {
                        Ok((updated_s, new_msgs)) => (updated_s, new_msgs),
                        Err(_) => (s, vec![]),
                    }
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
                        LoopEvent::FileOutput { .. } | LoopEvent::Embed { .. } => continue,
                    };
                    conn.send_event(&ipc_event).await.ok();
                }

                if let Ok((updated, new_msgs)) = loop_handle.await {
                    session = updated;

                    let history = session_mgr.history_for(&session);
                    for msg in &new_msgs {
                        let _ = history.append(msg);
                    }

                    // T23: Detect if any tool call wrote to MEMORY.md
                    let memory_written = new_msgs.iter().any(|m| {
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
                }
            }
            Request::NewSession => {
                // T21.1: Write session summary diary before ending current session
                if session.messages.len() > 2 {
                    let summary_session = session.clone();
                    let dn = daily_notes.clone();
                    let sq = side_query.clone();
                    tokio::spawn(async move {
                        if let Err(e) = session_diary::write_session_diary(&dn, &sq, &summary_session).await {
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
                    llm_backend.clone(),
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
            Request::Stop => {
                let count = task_registry.stop_all().await;
                info!("/stop — aborted {} background tasks", count);
                let msg = if count == 0 {
                    "⏹ No background tasks running.".to_string()
                } else {
                    format!("⏹ Stopped {} background task(s).", count)
                };
                conn.send_event(&Event::Notification { message: msg }).await?;
            }
            Request::ListTasks => {
                let running = task_registry.list().await;
                let turn = session.turn_count;
                let msgs = session.messages.len();
                let mut info_text = format!(
                    "📋 Session Status\n\
                     - Session: {}\n\
                     - Messages: {}\n\
                     - Turn: {} / {}\n\
                     - Model: {} | Context: {} tokens\n",
                    session.session_id, msgs, turn,
                    loop_config.max_turns, loop_config.model, loop_config.context_window,
                );
                if running.is_empty() {
                    info_text.push_str("\nNo background tasks running.");
                } else {
                    info_text.push_str(&format!("\n🔧 Background Tasks ({}):\n", running.len()));
                    for t in &running {
                        let elapsed = t.started_at.elapsed().as_secs();
                        info_text.push_str(&format!(
                            "  - [{}] {} ({}) — {}s ago\n",
                            t.id, t.name, t.tool, elapsed
                        ));
                    }
                }
                conn.send_event(&Event::Notification { message: info_text }).await?;
            }
            Request::Orchestrate { task } => {
                // v3: Orchestration is now handled by the LLM via execute_project tool.
                // This IPC path is kept for backwards compatibility but simply asks the LLM.
                info!("Orchestration requested (v3: delegating to LLM): {}", task);
                let msg = nova_core::message::Message::user(format!(
                    "Please orchestrate and complete this task: {}",
                    task
                ));
                session_mgr.append_message(&mut session, msg)?;
                // The next UserMessage cycle will handle this through the normal query loop
                conn.send_event(&Event::Notification {
                    message: "Task queued for next interaction".into(),
                }).await?;
            }
            Request::Shutdown => {
                // T21.1: Write session diary before shutdown
                if session.messages.len() > 2 {
                    let summary_session = session.clone();
                    let dn = daily_notes.clone();
                    let sq = side_query.clone();
                    tokio::spawn(async move {
                        if let Err(e) = session_diary::write_session_diary(&dn, &sq, &summary_session).await {
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
            if let Err(e) = session_diary::write_session_diary(&dn, &sq, &summary_session).await {
                warn!("Failed to write session diary on disconnect: {}", e);
            }
        });
    }

    session_mgr.save_meta(&session)?;
    Ok(())
}

