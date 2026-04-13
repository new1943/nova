use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, error};

use nova_core::agent::{QueryLoop, QueryLoopConfig, LoopEvent};
use nova_core::config::NovaConfig;
use nova_core::hooks::HookManager;
use nova_core::hooks::post_sampling::MemoryExtractHook;
use nova_core::hooks::stop::MemoryExtractStopHook;
use nova_core::memory::DualWriteMemory;
use nova_core::session::manager::SessionManager;
use nova_core::session::search::AgenticSessionSearch;
use nova_core::sidequery::SideQuery;
use nova_core::skills::SkillsLoader;
use nova_core::tools::{ToolRegistry, ReadFileTool, WriteFileTool, GlobTool, GrepTool};
use nova_core::tools::bash::{BashTool, BashMode};
use nova_core::workspace::BootstrapLoader;
use nova_ipc::{IpcServer, Event, Request};

const SOCKET_PATH: &str = "/tmp/nova.sock";
const PID_FILE: &str = "/tmp/nova.pid";

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
    tracing_subscriber::fmt()
        .with_writer(std::sync::Mutex::new(log_file))
        .with_max_level(tracing::Level::INFO)
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

fn make_tools(mode: &str) -> ToolRegistry {
    let bash_mode = match mode {
        "sandbox" => BashMode::Sandbox,
        _ => BashMode::Open,
    };
    let mut tools = ToolRegistry::new();
    tools.register_builtin(Box::new(BashTool::new(bash_mode)));
    tools.register_builtin(Box::new(ReadFileTool));
    tools.register_builtin(Box::new(WriteFileTool));
    tools.register_builtin(Box::new(GlobTool));
    tools.register_builtin(Box::new(GrepTool));
    tools
}

fn make_hooks(dual_write: Arc<Mutex<DualWriteMemory>>) -> HookManager {
    let mut hooks = HookManager::new();
    hooks.register_post_sampling(Box::new(MemoryExtractHook::new(dual_write.clone())));
    hooks.register_stop(Box::new(MemoryExtractStopHook::new(dual_write)));
    hooks
}

/// Generate tool descriptions string (used by BootstrapLoader)
fn tool_descriptions(mode: &str) -> String {
    let tools = make_tools(mode);
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
    };

    let server = IpcServer::bind(SOCKET_PATH.as_ref()).await?;
    info!("Listening on {}", SOCKET_PATH);

    loop {
        match server.accept().await {
            Ok(conn) => {
                let workspace_dir = config.workspace.clone();
                let sd = config.workspace.join("sessions");
                let md = config.workspace.join("memories");
                let lc = loop_config.clone();
                let sk = skills.clone();
                let rm = run_mode.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(conn, workspace_dir, sd, md, lc, sk, rm).await {
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
    workspace_dir: PathBuf,
    sessions_dir: PathBuf,
    memories_dir: PathBuf,
    loop_config: QueryLoopConfig,
    skills: Arc<SkillsLoader>,
    run_mode: String,
) -> Result<()> {
    let session_mgr = SessionManager::new(sessions_dir.clone());
    let dual_write = Arc::new(Mutex::new(DualWriteMemory::new(memories_dir)));

    // BootstrapLoader: hot-reloads workspace files with mtime caching
    let bootstrap = Arc::new(Mutex::new(BootstrapLoader::new(workspace_dir)));
    let tool_desc = tool_descriptions(&run_mode);

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
                dual_write.lock().await.clear_marker(&session.session_id);

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

                    match tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        searcher.search(&content),
                    ).await {
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
                let dw = dual_write.clone();
                let session_clone = session.clone();
                let rm = run_mode.clone();

                let loop_handle = tokio::spawn(async move {
                    let tools = make_tools(&rm);
                    let hooks = make_hooks(dw);
                    let ql = QueryLoop::new(tools, hooks, lc);
                    let mut s = session_clone;
                    s.turn_count = 0;
                    let _ = ql.run(&mut s, &sp, event_tx).await;
                    s
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
                    conn.send_event(&ipc_event).await?;
                }

                if let Ok(updated) = loop_handle.await {
                    let old_len = session.messages.len();
                    session = updated;
                    let history = session_mgr.history_for(&session);
                    for msg in &session.messages[old_len..] {
                        let _ = history.append(msg);
                    }
                    session_mgr.save_meta(&session)?;
                }
            }
            Request::NewSession => {
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
                session_mgr.save_meta(&session)?;
                info!("Shutdown requested");
                std::process::exit(0);
            }
        }
    }

    session_mgr.save_meta(&session)?;
    Ok(())
}
