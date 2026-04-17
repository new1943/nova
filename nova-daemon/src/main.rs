use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, error, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use nova_core::agent::{QueryLoop, QueryLoopConfig, LoopEvent};
use nova_core::config::NovaConfig;
use nova_core::hooks::HookManager;
use nova_core::memory::consolidate::MemoryConsolidator;
use nova_core::memory::daily::DailyNotes;
use nova_core::memory::dream::DreamEngine;
use nova_core::memory::recall::MemoryRecall;
use nova_core::session::manager::SessionManager;
use nova_core::session::search::AgenticSessionSearch;
use nova_core::sidequery::SideQuery;
use nova_core::skills::SkillsLoader;
use nova_core::tools::{ToolRegistry, ReadFileTool, WriteFileTool, FileEditTool, GlobTool, GrepTool, BrowserTool, AgenticSearchTool};
use nova_core::tools::bash::{BashTool, BashMode};
use nova_core::workspace::BootstrapLoader;
use nova_ipc::{IpcServer, Event, Request};

const SOCKET_PATH: &str = "/tmp/nova.sock";
const PID_FILE: &str = "/tmp/nova.pid";

mod discord;

pub struct HandleConfig {
    workspace_dir: PathBuf,
    sessions_dir: PathBuf,
    memories_dir: PathBuf,
    loop_config: QueryLoopConfig,
    skills: Arc<SkillsLoader>,
    run_mode: String,
    tools: Arc<ToolRegistry>,
}

fn budget_pct_calc(input_tokens: usize, context_window: usize) -> f32 {
    if context_window == 0 { return 0.0; }
    input_tokens as f32 / context_window as f32
}

#[tokio::main]
async fn main() -> Result<()> {
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

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("run");

    match cmd {
        "run" => run_daemon().await,
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

fn make_tools(
    mode: &str,
    browser_chrome_path: Option<String>,
    browser_profile_dir: Option<String>,
    browser_headless: bool,
    side_query: SideQuery,
    session_manager: SessionManager,
) -> ToolRegistry {
    let bash_mode = match mode {
        "sandbox" => BashMode::Sandbox,
        _ => BashMode::Open,
    };
    let mut tools = ToolRegistry::new();
    tools.register_builtin(Box::new(BashTool::new(bash_mode)));
    tools.register_builtin(Box::new(ReadFileTool));
    tools.register_builtin(Box::new(WriteFileTool));
    tools.register_builtin(Box::new(FileEditTool));
    tools.register_builtin(Box::new(GlobTool));
    tools.register_builtin(Box::new(GrepTool));

    // Browser tool — 通过 @playwright/mcp 子进程驱动
    tools.register_builtin(Box::new(BrowserTool::new(
        browser_chrome_path,
        browser_profile_dir,
        browser_headless,
    )));

    tools.register_builtin(Box::new(AgenticSearchTool::new(side_query, session_manager)));

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

async fn run_daemon() -> Result<()> {
    if check_pid_file() {
        anyhow::bail!("Daemon already running (PID file: {})", PID_FILE);
    }
    write_pid_file();

    struct PidGuard;
    impl Drop for PidGuard {
        fn drop(&mut self) { remove_pid_file(); }
    }
    let _guard = PidGuard;

    let config = NovaConfig::load_default()?;
    info!("NOVA daemon starting, workspace: {:?}, mode: {}", config.workspace, config.mode);

    let run_mode = config.mode.clone();

    let mut skills_loader = SkillsLoader::new(config.workspace.join("skills"));
    let _ = skills_loader.load_all();
    let skills = Arc::new(skills_loader);

    let loop_config = QueryLoopConfig {
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
    };

    let server = IpcServer::bind(SOCKET_PATH.as_ref()).await?;
    info!("Listening on {}", SOCKET_PATH);

    if config.discord_enabled {
        if let Some(token) = config.discord_token.clone() {
            let tools_dc = Arc::new(make_tools(
                &run_mode,
                config.browser_chrome_path.clone(),
                config.browser_profile_dir.clone(),
                config.browser_headless.unwrap_or(true),
                SideQuery::new(loop_config.api_key.clone(), loop_config.api_base_url.clone(), loop_config.model.clone()),
                SessionManager::new(config.workspace.join("sessions")),
            ));
            let dc_cfg = Arc::new(HandleConfig {
                workspace_dir: config.workspace.clone(),
                sessions_dir: config.workspace.join("sessions"),
                memories_dir: config.workspace.clone(),
                loop_config: loop_config.clone(),
                skills: skills.clone(),
                run_mode: run_mode.clone(),
                tools: tools_dc,
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

                let tools = Arc::new(make_tools(
                    &run_mode,
                    config.browser_chrome_path.clone(),
                    config.browser_profile_dir.clone(),
                    config.browser_headless.unwrap_or(true),
                    sq_for_tools,
                    sm_for_tools,
                ));
                tokio::spawn(async move {
                    let cfg = HandleConfig {
                        workspace_dir,
                        sessions_dir: sd,
                        memories_dir: md,
                        loop_config: lc,
                        skills: sk,
                        run_mode: rm,
                        tools,
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
                let content = if content.starts_with('/') {
                    let skill_name = content.trim_start_matches('/').split_whitespace().next().unwrap_or("");
                    if let Some(skill) = skills.find_by_name(skill_name) {
                        format!("{}\n\n{}", skill.prompt, content)
                    } else {
                        content
                    }
                } else {
                    let matched = skills.match_auto_trigger(&content);
                    if let Some(skill) = matched.first() {
                        format!("{}\n\n{}", skill.prompt, content)
                    } else {
                        content
                    }
                };

                let msg = nova_core::message::Message::user(&content);
                session_mgr.append_message(&mut session, msg)?;

                // T21.4: Record MEMORY.md mtime at turn start (for Dream dual-write mutex)
                record_memory_mtime(&mut session, &workspace_dir);

                // Hot-reload system prompt from workspace files (mtime cached)
                let mut sp = bootstrap.lock().await.build_system_prompt(&tool_desc);

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
                let lc = loop_config.clone();
                let ctx_window = lc.context_window;
                let dn = daily_notes.clone();
                let sq_loop = side_query.clone();
                let session_clone = session.clone();
                let tools_clone = tools.clone();
                let cons = consolidator.clone();

                let loop_handle = tokio::spawn(async move {
                    let hooks = make_hooks();
                    let cons_inner = Arc::try_unwrap(cons)
                        .unwrap_or_else(|arc| (*arc).clone());
                    let ql = QueryLoop::new(
                        tools_clone, hooks, lc, Some(dn), Some(sq_loop),
                        Some(cons_inner),
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
                    };
                    conn.send_event(&ipc_event).await.ok();
                }

                if let Ok((updated, new_msgs)) = loop_handle.await {
                    session = updated;
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
