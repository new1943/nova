use anyhow::Result;
use serenity::prelude::*;
use serenity::model::application::Interaction;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::async_trait;
use serenity::builder::{CreateMessage, EditMessage};
use tracing::{info, warn, error};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

use nova_agent::{QueryLoop, LoopEvent};
use nova_core::models::ShadowEvent;
use nova_memory::memory::consolidate::MemoryConsolidator;
use nova_memory::memory::daily::DailyNotes;
use nova_memory::memory::dream::DreamEngine;
use nova_memory::memory::recall::MemoryRecall;
use nova_memory::session::manager::SessionManager;
use nova_memory::session::AgenticSessionSearch;
use nova_memory::sidequery::SideQuery;
use nova_agent::workspace::BootstrapLoader;
use nova_tools::ToolContext;

use crate::HandleConfig;

/// Filter `<nova_os>...</nova_os>` blocks from content before sending to Discord.
/// This prevents internal reasoning tags from being exposed to users.
fn filter_nova_os(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut search_start = 0;

    while let Some(start) = content[search_start..].find("<nova_os>") {
        let absolute_start = search_start + start;
        result.push_str(&content[search_start..absolute_start]);

        if let Some(end) = content[absolute_start..].find("</nova_os>") {
            search_start = absolute_start + end + "</nova_os>".len();
        } else {
            break;
        }
    }

    result.push_str(&content[search_start..]);
    result
}


struct DiscordHandler {
    cfg: Arc<HandleConfig>,
    // Mutex to prevent concurrent processing in the same channel
    active_channels: Arc<Mutex<std::collections::HashSet<String>>>,
    // Application ID set after Ready event
    app_id: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

#[async_trait]
impl EventHandler for DiscordHandler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("Discord Gateway connected as {}", ready.user.name);

        // Register global commands after Ready (application_id is now available)
        let app_id = ready.application.id.get();
        self.app_id.store(app_id, std::sync::atomic::Ordering::SeqCst);

        let http = ctx.http.clone();
        let commands = register_global_commands();
        if let Err(e) = http.create_global_commands(&commands).await {
            warn!("Failed to register global commands: {}", e);
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        // Ignore bot messages
        if msg.author.bot { return; }
        
        let content = msg.content.trim().to_string();
        if content.is_empty() { return; }

        let session_id = msg.channel_id.to_string();

        let mut active = self.active_channels.lock().await;
        if active.contains(&session_id) {
            let builder = CreateMessage::new().content("⏳ Please wait for the previous turn to complete...");
            let _ = msg.channel_id.send_message(&ctx.http, builder).await;
            return;
        }
        active.insert(session_id.clone());
        drop(active);

        let cfg = self.cfg.clone();
        let ctx = ctx.clone();
        let msg = msg.clone();
        let active_channels = self.active_channels.clone();

        tokio::spawn(async move {
            if let Err(e) = process_discord_message(cfg, ctx.clone(), msg.clone(), session_id.clone(), content).await {
                error!("Discord process error: {}", e);
                let builder = CreateMessage::new().content(format!("❌ Error: {}", e));
                let _ = msg.channel_id.send_message(&ctx.http, builder).await;
            }
            let mut active = active_channels.lock().await;
            active.remove(&session_id);
        });
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(cmd) = interaction else { return; };

        let channel_id = cmd.channel_id;
        let session_id = channel_id.to_string();

        match cmd.data.name.as_str() {
            "new" => {
                let cfg = self.cfg.clone();
                let active_channels = self.active_channels.clone();

                let mut active = active_channels.lock().await;
                if active.contains(&session_id) {
                    let builder = CreateMessage::new().content("⏳ Please wait for the previous turn to complete...");
                    let _ = channel_id.send_message(&ctx.http, builder).await;
                    return;
                }
                active.insert(session_id.clone());
                drop(active);

                let _ = cmd.defer(&ctx.http).await;

                tokio::spawn(async move {
                    if let Err(e) = handle_new_command(cfg, ctx.clone(), channel_id).await {
                        error!("Error handling /new command: {}", e);
                    }
                    let mut active = active_channels.lock().await;
                    active.remove(&session_id);
                });
            }
            "reset" => {
                let cfg = self.cfg.clone();
                let active_channels = self.active_channels.clone();

                let mut active = active_channels.lock().await;
                if active.contains(&session_id) {
                    let builder = CreateMessage::new().content("⏳ Please wait for the previous turn to complete...");
                    let _ = channel_id.send_message(&ctx.http, builder).await;
                    return;
                }
                active.insert(session_id.clone());
                drop(active);

                let _ = cmd.defer(&ctx.http).await;

                tokio::spawn(async move {
                    if let Err(e) = handle_reset_command(cfg, ctx.clone(), channel_id).await {
                        error!("Error handling /reset command: {}", e);
                    }
                    let mut active = active_channels.lock().await;
                    active.remove(&session_id);
                });
            }
            _ => {
                let builder = serenity::builder::CreateInteractionResponseFollowup::new()
                    .content("Unknown command");
                let _ = cmd.create_followup(&ctx.http, builder).await;
            }
        }
    }
}

async fn process_discord_message(
    cfg: Arc<HandleConfig>,
    ctx: Context,
    msg: Message,
    session_id: String,
    content: String,
) -> Result<()> {
    let workspace_dir = cfg.workspace_dir.clone();
    let sessions_dir = cfg.sessions_dir.clone();
    let memories_dir = cfg.memories_dir.clone();
    let mut loop_config = cfg.loop_config.clone();
    let skills = cfg.skills.clone();
    let tools = cfg.tools.clone();
    let dispatcher_tx = cfg.dispatcher_tx.clone();
    let llm_backend = cfg.llm_backend.clone();

    let session_mgr = SessionManager::new(sessions_dir.clone());
    let bootstrap = Arc::new(Mutex::new(BootstrapLoader::new(workspace_dir.clone())));
    let tool_desc = crate::tool_factory::tool_descriptions(&tools);

    let daily_notes = DailyNotes::new(memories_dir.clone());
    let side_query = SideQuery::new(llm_backend.clone(), loop_config.model.clone());

    let recall_session_mgr = SessionManager::new(sessions_dir.clone());
    let memory_recall = MemoryRecall::new(memories_dir.clone(), side_query.clone(), recall_session_mgr);

    let dream_engine = Arc::new(DreamEngine::new(
        workspace_dir.clone(),
        memories_dir.clone(),
        SideQuery::new(llm_backend.clone(), loop_config.model.clone()),
    ));

    loop_config.memories_dir = Some(memories_dir.clone());

    let consolidator = Arc::new(MemoryConsolidator::new(
        workspace_dir.clone(),
        SideQuery::new(llm_backend.clone(), loop_config.model.clone()),
    ));

    // v2 Phase 1.5: TopicTracker, TensionTracker, ModeRouter, MemoryBoard
    let tension_tracker = std::sync::Arc::new(nova_memory::TensionTracker::new());
    let topic_tracker = std::sync::Arc::new(tokio::sync::RwLock::new(nova_memory::TopicTracker::new()));
    let memory_board = std::sync::Arc::new(tokio::sync::RwLock::new(nova_memory::MemoryBoard::new(
        workspace_dir.join("MEMORY.md"),
    )));

    let mut session = match session_mgr.resume_by_id(&session_id)? {
        Some(s) => s,
        None => {
            let mut s = session_mgr.create(loop_config.max_turns)?;
            s.session_id = session_id.clone();
            s
        }
    };

    // T23: Idle consolidation
    let idle_secs = (chrono::Utc::now() - session.updated_at).num_seconds();
    let has_unswept = session.last_memory_sweep_index < session.messages.len();
    if idle_secs > 900 && has_unswept {
        let cons = (*consolidator).clone();
        match cons.consolidate(&session.messages, session.last_memory_sweep_index, session.memory_updated_mutex).await {
            Ok(_) => {
                session.last_memory_sweep_index = session.messages.len();
                session.memory_updated_mutex = false;
                session_mgr.save_meta(&session)?;
            }
            Err(e) => warn!("T23: Idle consolidation failed: {}", e),
        }
    }

    let content = {
        let skills = skills.lock().unwrap();
        if content.starts_with('/') {
            let skill_name = content.trim_start_matches('/').split_whitespace().next().unwrap_or("");
            if let Some(skill) = skills.find_by_name(skill_name) {
                format!("{}\n\n{}", skill.prompt, content)
            } else {
                content.clone()
            }
        } else {
            let matched = skills.match_auto_trigger(&content);
            if let Some(skill) = matched.first() {
                format!("{}\n\n{}", skill.prompt, content)
            } else {
                content.clone()
            }
        }
    };

    if content.trim() == "/new" {
        if session.messages.len() > 2 {
            let summary_session = session.clone();
            let dn = daily_notes.clone();
            let sq = side_query.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::session_diary::write_session_diary(&dn, &sq, &summary_session).await {
                    warn!("Failed to write session diary: {}", e);
                }
            });
        }
        
        if let Err(e) = session_mgr.clear_session(&mut session) {
            let builder = CreateMessage::new().content(format!("❌ Failed to clear session: {}", e));
            let _ = msg.channel_id.send_message(&ctx.http, builder).await;
            return Ok(());
        }
        
        let builder = CreateMessage::new().content("✨ Started a new session. Context cleared.");
        let _ = msg.channel_id.send_message(&ctx.http, builder).await;
        return Ok(());
    }


    let user_msg = nova_core::message::Message::user(&content);
    session_mgr.append_message(&mut session, user_msg)?;

    crate::session_diary::record_memory_mtime(&mut session, &workspace_dir);

    let mut sp = bootstrap.lock().await.build_system_prompt(&tool_desc);


    // Context Search
    if content.chars().count() > 5 {
        let sq = SideQuery::new(
            llm_backend.clone(),
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

        if let Ok(Ok(results)) = search_res {
            if !results.is_empty() {
                let max_inject_chars = (loop_config.context_window / 20).max(1000);
                let per_session_chars = max_inject_chars / 3;
                let mut ctx_str = String::from("\n\n<relevant_history>\nExcerpts from previous conversations:\n");
                for (i, r) in results.iter().take(3).enumerate() {
                    let excerpt: String = r.transcript.chars().take(per_session_chars).collect();
                    ctx_str.push_str(&format!("\n--- Session {} ---\nUser: {}\nExcerpt: {}\n", i + 1, r.first_message, excerpt));
                }
                ctx_str.push_str("</relevant_history>");
                sp.push_str(&ctx_str);
            }
        }
        if let Ok(Ok(injection)) = recall_res {
            if !injection.is_empty() {
                sp.push_str(&injection);
            }
        }
    }

    let (event_tx, mut event_rx) = mpsc::channel::<LoopEvent>(64);
    // [V4 Task 3.2] Get shadow event sender for QueryLoop
    let shadow_tx = dispatcher_tx.channel();
    let lc = loop_config.clone();
    let dn = daily_notes.clone();
    let sq_loop = side_query.clone();
    let session_clone = session.clone();
    let tools_clone = tools.clone();
    let cons = consolidator.clone();
    let tt = topic_tracker.clone();
    let tens = tension_tracker.clone();
    let mb = memory_board.clone();
    let session_id_str = session.session_id.clone();
    let workspace_dir_clone = workspace_dir.clone();
    let backend_for_loop = llm_backend.clone();
    let skills_clone = Some(skills.clone());

    let loop_handle = tokio::spawn(async move {
        let hooks = crate::tool_factory::make_hooks();
        let cons_inner = Arc::try_unwrap(cons).unwrap_or_else(|arc| (*arc).clone());
        let tool_ctx = ToolContext::new(
            session_id_str.clone(),
            Some(workspace_dir_clone),
        );
        let ql = QueryLoop::new(backend_for_loop, tools_clone, hooks, lc, Some(dn), Some(sq_loop), Some(cons_inner), Some(tt), Some(tens), /* mr disabled */ Some(mb), Some(shadow_tx), skills_clone, tool_ctx);
        let mut s = session_clone;
        s.turn_count = 0;
        
        let result = ql.run_turn(s.clone(), &sp, event_tx).await;
        match result {
            Ok((updated_s, new_msgs, preflight)) => (updated_s, new_msgs, preflight),
            Err(_) => (s, vec![], None),
        }
    });

    let builder = CreateMessage::new().content("🤔 Thinking...");
    let mut reply_msg = msg.channel_id.send_message(&ctx.http, builder).await?;
    
    let mut text_buffer = String::new();
    let mut last_update = tokio::time::Instant::now();

    while let Some(event) = event_rx.recv().await {
        match event {
            LoopEvent::TextDelta(t) => {
                text_buffer.push_str(&t);
                if last_update.elapsed().as_secs() >= 2 && !text_buffer.is_empty() {
                    let display = filter_nova_os(&text_buffer);
                    let trunc_text: String = display.chars().take(1900).collect();
                    let builder = EditMessage::new().content(format!("{}...", trunc_text));
                    let _ = reply_msg.edit(&ctx.http, builder).await;
                    last_update = tokio::time::Instant::now();
                }
            }
            LoopEvent::ToolCallStart { name, .. } => {
                let display = filter_nova_os(&text_buffer);
                let trunc_text: String = display.chars().take(1850).collect();
                let builder = EditMessage::new().content(format!("{}...\n\n🛠️ Running tool: `{}`", trunc_text, name));
                let _ = reply_msg.edit(&ctx.http, builder).await;
            }
            LoopEvent::Error(e) => {
                let display = filter_nova_os(&text_buffer);
                let trunc_text: String = display.chars().take(1850).collect();
                let builder = EditMessage::new().content(format!("{}...\n\n❌ Error: {}", trunc_text, e));
                let _ = reply_msg.edit(&ctx.http, builder).await;
            }
            _ => {}
        }
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
        for m in &new_msgs {
            let _ = history.append(m);
        }

        let memory_written = new_msgs.iter().any(|m| {
            if let Some(tcs) = &m.tool_calls {
                tcs.iter().any(|tc| {
                    (tc.name == "write_file" || tc.name == "file_edit") && tc.arguments.to_string().contains("MEMORY.md")
                })
            } else { false }
        });
        if memory_written {
            session.memory_updated_mutex = true;
        }

        session_mgr.save_meta(&session)?;

        if dream_engine.should_dream() {
            let de = dream_engine.clone();
            let mtime = session.token_stats.memory_mtime;
            tokio::spawn(async move {
                let _ = de.dream(mtime).await;
            });
        }
    }

    if text_buffer.is_empty() {
        text_buffer = "Done.".into();
    }

    let display = filter_nova_os(&text_buffer);
    let chars: Vec<char> = display.chars().collect();
    
    if chars.is_empty() {
        let builder = EditMessage::new().content("Done.");
        let _ = reply_msg.edit(&ctx.http, builder).await;
    } else {
        let mut chunks = chars.chunks(1950);
        
        if let Some(first_chunk) = chunks.next() {
            let chunk_str: String = first_chunk.iter().collect();
            let builder = EditMessage::new().content(chunk_str);
            let _ = reply_msg.edit(&ctx.http, builder).await;
        }
        
        for chunk in chunks {
            let chunk_str: String = chunk.iter().collect();
            let builder = CreateMessage::new().content(chunk_str);
            let _ = msg.channel_id.send_message(&ctx.http, builder).await;
        }
    }

    Ok(())
}

pub async fn start(token: String, cfg: Arc<HandleConfig>) -> Result<()> {
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let handler = DiscordHandler {
        cfg,
        active_channels: Arc::new(Mutex::new(std::collections::HashSet::new())),
        app_id: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
    };

    let mut client = Client::builder(&token, intents)
        .event_handler(handler)
        .await?;

    info!("Starting Discord Gateway...");
    client.start().await?;
    Ok(())
}

fn register_global_commands() -> Vec<serenity::builder::CreateCommand> {
    vec![
        serenity::builder::CreateCommand::new("new")
            .description("Start a new session, clearing all conversation history"),
        serenity::builder::CreateCommand::new("reset")
            .description("Reset the current session to initial state"),
    ]
}

async fn handle_new_command(
    cfg: Arc<HandleConfig>,
    ctx: Context,
    channel_id: serenity::model::id::ChannelId,
) -> Result<()> {
    let sessions_dir = cfg.sessions_dir.clone();
    let session_mgr = SessionManager::new(sessions_dir);
    let session_id = channel_id.to_string();

    let mut session = match session_mgr.resume_by_id(&session_id)? {
        Some(s) => s,
        None => {
            let mut s = session_mgr.create(cfg.loop_config.max_turns)?;
            s.session_id = session_id.clone();
            s
        }
    };

    if session.messages.len() > 2 {
        let daily_notes = DailyNotes::new(cfg.memories_dir.clone());
        let side_query = SideQuery::new(
            cfg.llm_backend.clone(),
            cfg.loop_config.model.clone(),
        );
        let session_clone = session.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::session_diary::write_session_diary(&daily_notes, &side_query, &session_clone).await {
                warn!("Failed to write session diary: {}", e);
            }
        });
    }

    if let Err(e) = session_mgr.clear_session(&mut session) {
        let builder = CreateMessage::new().content(format!("❌ Failed to clear session: {}", e));
        let _ = channel_id.send_message(&ctx.http, builder).await;
        return Ok(());
    }

    let builder = CreateMessage::new().content("✨ Started a new session. Context cleared.");
    let _ = channel_id.send_message(&ctx.http, builder).await;
    Ok(())
}

async fn handle_reset_command(
    cfg: Arc<HandleConfig>,
    ctx: Context,
    channel_id: serenity::model::id::ChannelId,
) -> Result<()> {
    let sessions_dir = cfg.sessions_dir.clone();
    let session_mgr = SessionManager::new(sessions_dir);
    let session_id = channel_id.to_string();

    let mut session = match session_mgr.resume_by_id(&session_id)? {
        Some(s) => s,
        None => {
            let mut s = session_mgr.create(cfg.loop_config.max_turns)?;
            s.session_id = session_id.clone();
            s
        }
    };

    if let Err(e) = session_mgr.clear_session(&mut session) {
        let builder = CreateMessage::new().content(format!("❌ Failed to reset session: {}", e));
        let _ = channel_id.send_message(&ctx.http, builder).await;
        return Ok(());
    }

    let builder = CreateMessage::new().content("🔄 Session reset to initial state.");
    let _ = channel_id.send_message(&ctx.http, builder).await;
    Ok(())
}
