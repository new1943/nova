use anyhow::Result;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, broadcast, Mutex};
use tracing::{info, error, warn};

use nova_agent::{QueryLoop, QueryLoopConfig, LoopEvent};
use nova_core::llm_backend::LlmBackend;
use nova_agent::memory::{EpisodicMemory, ConsolidationMemory};
use nova_memory::memory::consolidate::MemoryConsolidator;
use nova_memory::memory::daily::DailyNotes;
use nova_memory::memory::tension_tracker::TensionTracker;
use nova_memory::memory::mode_router::ModeRouter;
use nova_memory::memory::topic_state::TopicTracker;
use nova_memory::memory::memory_board::MemoryBoard;
use nova_memory::memory::recall::MemoryRecall;
use nova_memory::session::manager::SessionManager;
use nova_memory::session::AgenticSessionSearch;
use nova_memory::sidequery::SideQuery;
use nova_tools::skills::SharedSkillsLoader;
use nova_agent::heartbeat::{HeartbeatScheduler};
use nova_agent::heartbeat::scheduler::HeartbeatEvent;
use nova_tools::{ToolRegistry, ToolContext};
use nova_agent::workspace::BootstrapLoader;
use nova_ipc::{Event, Request};

use crate::tool_factory;
use crate::session_diary;

pub struct HandleConfig {
    pub workspace_dir: std::path::PathBuf,
    pub sessions_dir: std::path::PathBuf,
    pub memories_dir: std::path::PathBuf,
    pub loop_config: QueryLoopConfig,
    pub skills: SharedSkillsLoader,
    pub tools: Arc<ToolRegistry>,
    pub heartbeat_interval_secs: u64,
    pub llm_backend: Arc<dyn LlmBackend>,
    #[allow(dead_code)]
    pub discord_push_tx: Option<std::sync::Arc<tokio::sync::mpsc::Sender<crate::DiscordPush>>>,
    pub ipc_push_tx: Option<std::sync::Arc<tokio::sync::broadcast::Sender<nova_ipc::Event>>>,
    pub exec_event_tx: tokio::sync::broadcast::Sender<nova_ipc::Event>,
    pub tool_approval_enabled: bool,
    pub discord_channel_id: Option<u64>,
}

fn budget_pct_calc(input_tokens: usize, context_window: usize) -> f32 {
    if context_window == 0 {
        return 0.0;
    }
    input_tokens as f32 / context_window as f32
}

pub async fn handle_connection(
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

    let task_registry = nova_core::executor::registry::TaskRegistry::global();

    let bootstrap = Arc::new(Mutex::new(BootstrapLoader::new(workspace_dir.clone())));
    let tool_desc = tool_factory::tool_descriptions(&tools);

    let daily_notes = DailyNotes::new(memories_dir.clone());
    let episodic = EpisodicMemory::new(daily_notes.clone());
    let side_query = SideQuery::new(llm_backend.clone(), loop_config.model.clone());

    loop_config.memories_dir = Some(memories_dir.clone());

    let raw_consolidator = MemoryConsolidator::new(
        workspace_dir.clone(),
        SideQuery::new(llm_backend.clone(), loop_config.model.clone()),
    );
    let consolidation = ConsolidationMemory::new(raw_consolidator.clone());
    let consolidator = Arc::new(raw_consolidator);

    // Memory subsystems
    let tension_tracker = Arc::new(TensionTracker::new());
    let mode_router = Arc::new(ModeRouter::new(tension_tracker.clone()));
    let topic_tracker = Arc::new(TopicTracker::new());
    let memory_board = Arc::new(MemoryBoard::new(workspace_dir.join("MEMORY.md")));
    let _ = memory_board.load().await; // Load existing MEMORY.md
    let memory_recall = Arc::new(MemoryRecall::new(
        memories_dir.clone(),
        SideQuery::new(llm_backend.clone(), loop_config.model.clone()),
        SessionManager::new(sessions_dir.clone()),
    ));

    let mut session = match session_mgr.resume_latest()? {
        Some(s) => {
            conn.send_event(&Event::SessionRestored {
                session_id: s.session_id.clone(),
                message_count: s.messages.len(),
            })
            .await?;
            s
        }
        None => {
            let s = session_mgr.create(loop_config.max_turns)?;
            conn.send_event(&Event::SessionCreated {
                session_id: s.session_id.clone(),
            })
            .await?;
            s
        }
    };

    let ipc_push_rx = ipc_push_tx.as_ref().map(|tx| tx.subscribe());
    let (inject_tx, mut inject_rx) = tokio::sync::mpsc::channel::<Event>(32);

    // IPC push forwarder
    if let Some(mut rx) = ipc_push_rx {
        let writer = conn.clone_writer();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if matches!(event, Event::ProjectCompleted { .. }) {
                            continue;
                        }
                        let mut data = serde_json::to_string(&event).unwrap();
                        data.push('\n');
                        let mut w = writer.lock().await;
                        if w.write_all(data.as_bytes()).await.is_err() {
                            break;
                        }
                        w.flush().await.ok();
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("[IPC Push] Lagged {} messages", n);
                    }
                }
            }
        });
    }

    // Exec event forwarder
    {
        let mut exec_rx = cfg.exec_event_tx.subscribe();
        let inject = inject_tx.clone();
        tokio::spawn(async move {
            info!("[ExecEvent] Forwarder started");
            loop {
                match exec_rx.recv().await {
                    Ok(event) => {
                        if let Err(e) = inject.send(event).await {
                            error!("[ExecEvent] Failed to inject: {}", e);
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("[ExecEvent] Lagged {} messages", n);
                    }
                }
            }
        });
    }

    // Heartbeat
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
                    Ok(None) | Err(_) => break,
                }
            }
            Some(ipc_event) = inject_rx.recv() => {
                if let Event::ProjectCompleted { report, project_id } = ipc_event {
                    info!("[MainLoop] ProjectCompleted for task {}", project_id);
                    let msg_content = format!(
                        "<system_notification>\n后台任务 {} 执行完毕。以下是执行结果报告：\n\n{}\n\n请立刻以你的名义，用自然语言向我简述/汇报上述结果。\n</system_notification>",
                        project_id, report
                    );
                    req_to_process = Some(Request::UserMessage { content: msg_content });
                } else {
                    continue;
                }
            }
        }

        match req_to_process.unwrap() {
            Request::UserMessage { content } => {
                info!("Received UserMessage ({} chars)", content.len());

                // T23: Idle-time consolidation
                {
                    let idle_secs = (chrono::Utc::now() - session.updated_at).num_seconds();
                    let has_unswept = session.last_memory_sweep_index < session.messages.len();
                    if idle_secs > 900 && has_unswept {
                        info!("T23: Idle detected ({}s), consolidating", idle_secs);
                        let cons = (*consolidator).clone();
                        match cons
                            .consolidate(
                                &session.messages,
                                session.last_memory_sweep_index,
                                session.memory_updated_mutex,
                            )
                            .await
                        {
                            Ok(_) => {
                                session.last_memory_sweep_index = session.messages.len();
                                session.memory_updated_mutex = false;
                                session_mgr.save_meta(&session)?;
                            }
                            Err(e) => warn!("T23: Consolidation failed: {}", e),
                        }
                    }
                }

                // Skill injection
                let content = if let Ok(skills_guard) = skills.lock() {
                    if content.starts_with('/') {
                        let skill_name = content
                            .trim_start_matches('/')
                            .split_whitespace()
                            .next()
                            .unwrap_or("");
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

                session_diary::record_memory_mtime(&mut session, &workspace_dir);

                // Update memory subsystems
                let _session_state = tension_tracker.update_from_message(&content).await;
                let _mode = mode_router.process(&content).await;
                let topic_transition = topic_tracker.on_user_message(&content).await;
                if matches!(topic_transition, nova_memory::memory::topic_state::TopicTransition::Archive) {
                    if let Some(topic) = topic_tracker.current_topic().await {
                        let _ = memory_board.archive_topic(&topic.name).await;
                    }
                }
                tension_tracker.record_interaction().await;

                let mut sp = bootstrap.lock().await.build_system_prompt(&tool_desc);

                // Inject mode hints from ModeRouter
                let hints = mode_router.get_mode_hints().await;
                sp.push_str(&format!(
                    "\n\n<mode_hints>\n当前模式: {}\n语气: {}\n回复长度: {}\n张力值: {}\n</mode_hints>",
                    hints.description, hints.tone, hints.response_length, hints.tension
                ));

                // Inject relevant diary recall from MemoryRecall
                if content.chars().count() > 10 {
                    match memory_recall.recall(&content, 2).await {
                        Ok(recall) if !recall.is_empty() => {
                            sp.push_str(&recall);
                            info!("MemoryRecall injected diary excerpts");
                        }
                        Ok(_) => {}
                        Err(e) => info!("MemoryRecall failed: {}", e),
                    }
                }

                // Auto-search
                if content.chars().count() > 5 {
                    let sq = SideQuery::new(llm_backend.clone(), loop_config.model.clone());
                    let search_mgr = SessionManager::new(sessions_dir.clone());
                    let searcher = AgenticSessionSearch::new(sq, search_mgr);

                    match tokio::time::timeout(
                        std::time::Duration::from_secs(15),
                        searcher.search(&content),
                    )
                    .await
                    {
                        Ok(Ok(results)) if !results.is_empty() => {
                            let max_chars = (loop_config.context_window / 20).max(1000);
                            let per_session = max_chars / 3;
                            let mut ctx = String::from(
                                "\n\n<relevant_history>\nExcerpts from previous conversations:\n",
                            );
                            for (i, r) in results.iter().take(3).enumerate() {
                                let excerpt: String =
                                    r.transcript.chars().take(per_session).collect();
                                ctx.push_str(&format!(
                                    "\n--- Session {} ---\nUser: {}\nExcerpt: {}\n",
                                    i + 1,
                                    r.first_message,
                                    excerpt
                                ));
                            }
                            ctx.push_str("</relevant_history>");
                            sp.push_str(&ctx);
                            info!("Auto-search injected {} sessions", results.len().min(3));
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => info!("Auto-search failed: {}", e),
                        Err(_) => info!("Auto-search timed out"),
                    }
                }

                let (event_tx, mut event_rx) = mpsc::channel::<LoopEvent>(64);
                let lc = loop_config.clone();
                let ctx_window = lc.context_window;
                let sq_loop = side_query.clone();
                let session_clone = session.clone();
                let tools_clone = tools.clone();
                let ep = episodic.clone();
                let cons_mem = consolidation.clone();
                let session_id_str = session.session_id.clone();
                let workspace_dir_clone = workspace_dir.clone();
                let backend_for_loop = llm_backend.clone();

                let loop_handle = tokio::spawn(async move {
                    let hooks = tool_factory::make_hooks(workspace_dir_clone.clone());
                    let tool_ctx =
                        ToolContext::new(session_id_str.clone(), Some(workspace_dir_clone));
                    let ql = QueryLoop::new(
                        backend_for_loop,
                        tools_clone,
                        hooks,
                        lc,
                        Some(ep),
                        Some(sq_loop),
                        Some(cons_mem),
                        None,
                        tool_ctx,
                    );
                    let mut s = session_clone;
                    s.turn_count = 0;

                    match ql.run_turn(s.clone(), &sp, event_tx).await {
                        Ok((updated_s, new_msgs)) => (updated_s, new_msgs),
                        Err(_) => (s, vec![]),
                    }
                });

                while let Some(event) = event_rx.recv().await {
                    let ipc_event = match event {
                        LoopEvent::TextDelta(t) => Event::TextDelta { content: t },
                        LoopEvent::ToolCallStart { id, name } => {
                            Event::ToolCallStart { id, name }
                        }
                        LoopEvent::ToolCallResult { id, content } => {
                            Event::ToolCallResult { id, content }
                        }
                        LoopEvent::TurnEnd => Event::TurnEnd,
                        LoopEvent::TokenUsage { input, output } => Event::TokenUsage {
                            input,
                            output,
                            budget_pct: budget_pct_calc(input as usize, ctx_window),
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
                        info!("T23: MEMORY.md write detected, mutex set");
                    }

                    session_mgr.save_meta(&session)?;
                }
            }
            Request::NewSession => {
                if session.messages.len() > 2 {
                    let summary_session = session.clone();
                    let dn = daily_notes.clone();
                    let sq = side_query.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            session_diary::write_session_diary(&dn, &sq, &summary_session).await
                        {
                            warn!("Failed to write session diary: {}", e);
                        }
                    });
                }
                session = session_mgr.create(session.max_turns)?;
                conn.send_event(&Event::SessionCreated {
                    session_id: session.session_id.clone(),
                })
                .await?;
            }
            Request::SearchSessions { query } => {
                let sq = SideQuery::new(llm_backend.clone(), loop_config.model.clone());
                let search_mgr = SessionManager::new(sessions_dir.clone());
                let searcher = AgenticSessionSearch::new(sq, search_mgr);
                match searcher.search(&query).await {
                    Ok(results) => {
                        let entries: Vec<nova_ipc::protocol::SearchResultEntry> = results
                            .iter()
                            .map(|r| nova_ipc::protocol::SearchResultEntry {
                                session_id: r.session_id.clone(),
                                title: r.first_message.chars().take(80).collect(),
                                message_count: r.message_count,
                            })
                            .collect();
                        conn.send_event(&Event::SearchResults { results: entries })
                            .await?;
                    }
                    Err(e) => {
                        conn.send_event(&Event::Error {
                            message: format!("Search failed: {}", e),
                        })
                        .await?;
                    }
                }
            }
            Request::ResumeSession => {
                if let Some(s) = session_mgr.resume_latest()? {
                    conn.send_event(&Event::SessionRestored {
                        session_id: s.session_id.clone(),
                        message_count: s.messages.len(),
                    })
                    .await?;
                    session = s;
                }
            }
            Request::Stop => {
                let count = task_registry.stop_all().await;
                info!("/stop — aborted {} tasks", count);
                let msg = if count == 0 {
                    "No background tasks running.".to_string()
                } else {
                    format!("Stopped {} background task(s).", count)
                };
                conn.send_event(&Event::Notification { message: msg })
                    .await?;
            }
            Request::ListTasks => {
                let running = task_registry.list().await;
                let turn = session.turn_count;
                let msgs = session.messages.len();
                let mut info_text = format!(
                    "Session Status\n\
                     - Session: {}\n\
                     - Messages: {}\n\
                     - Turn: {} / {}\n\
                     - Model: {} | Context: {} tokens\n",
                    session.session_id,
                    msgs,
                    turn,
                    loop_config.max_turns,
                    loop_config.model,
                    loop_config.context_window,
                );
                if running.is_empty() {
                    info_text.push_str("\nNo background tasks running.");
                } else {
                    info_text.push_str(&format!("\nBackground Tasks ({}):\n", running.len()));
                    for t in &running {
                        let elapsed = t.started_at.elapsed().as_secs();
                        info_text.push_str(&format!(
                            "  - [{}] {} ({}) — {}s ago\n",
                            t.id, t.name, t.tool, elapsed
                        ));
                    }
                }
                conn.send_event(&Event::Notification { message: info_text })
                    .await?;
            }
            Request::Orchestrate { task } => {
                info!("Orchestration requested: {}", task);
                let msg = nova_core::message::Message::user(format!(
                    "Please orchestrate and complete this task: {}",
                    task
                ));
                session_mgr.append_message(&mut session, msg)?;
                conn.send_event(&Event::Notification {
                    message: "Task queued for next interaction".into(),
                })
                .await?;
            }
            Request::Shutdown => {
                if session.messages.len() > 2 {
                    let summary_session = session.clone();
                    let dn = daily_notes.clone();
                    let sq = side_query.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            session_diary::write_session_diary(&dn, &sq, &summary_session).await
                        {
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

    // Write session diary on disconnect
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
