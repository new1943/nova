use anyhow::Result;
use serenity::prelude::*;
use serenity::model::application::Interaction;
use serenity::model::channel::{Message, ReactionType};
use serenity::model::gateway::Ready;
use serenity::async_trait;
use serenity::builder::{CreateAttachment, CreateButton, CreateActionRow, CreateMessage, EditMessage};
use serenity::model::application::ButtonStyle;
use nova_core::approval::{ApprovalDecision, ApprovalHandler};
use nova_core::message::MessageAttachment;
use tracing::{info, warn, error};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, broadcast};

use nova_agent::{QueryLoop, LoopEvent};
use nova_memory::memory::consolidate::MemoryConsolidator;
use nova_memory::memory::daily::DailyNotes;
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


/// Pending approval requests, keyed by approval ID
pub type PendingApprovals = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<ApprovalDecision>>>>;

struct DiscordApprovalHandler {
    ctx: Context,
    channel_id: serenity::model::id::ChannelId,
    pending: PendingApprovals,
}

#[async_trait]
impl ApprovalHandler for DiscordApprovalHandler {
    async fn request_approval(&self, tool_name: &str, arguments: &serde_json::Value) -> ApprovalDecision {
        let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let args_preview = {
            let s = serde_json::to_string_pretty(arguments).unwrap_or_default();
            if s.len() > 300 {
                let end = s.char_indices().map(|(i, _)| i).filter(|&i| i <= 300).last().unwrap_or(0);
                format!("{}...", &s[..end])
            } else { s }
        };

        let buttons = vec![
            CreateButton::new(format!("approve:{}", id)).label("Allow").style(ButtonStyle::Success),
            CreateButton::new(format!("session:{}", id)).label("Allow Session").style(ButtonStyle::Primary),
            CreateButton::new(format!("deny:{}", id)).label("Deny").style(ButtonStyle::Danger),
        ];
        let action_row = CreateActionRow::Buttons(buttons);
        let builder = CreateMessage::new()
            .content(format!("🔧 Tool `{}` requests execution\n```\n{}\n```", tool_name, args_preview))
            .components(vec![action_row]);

        let mut msg = match self.channel_id.send_message(&self.ctx.http, builder).await {
            Ok(m) => m,
            Err(e) => {
                warn!("Failed to send approval request: {}", e);
                return ApprovalDecision::Allow;
            }
        };

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);

        // Wait for response with 5-minute timeout
        match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
            Ok(Ok(decision)) => {
                let _ = msg.edit(&self.ctx.http, EditMessage::new().content(format!("✅ Tool `{}` — {:?}", tool_name, decision)).components(vec![])).await;
                decision
            }
            _ => {
                warn!("Approval request timed out for tool `{}`", tool_name);
                self.pending.lock().await.remove(&id);
                let _ = msg.edit(&self.ctx.http, EditMessage::new().content(format!("⏰ Tool `{}` — timed out, denying", tool_name)).components(vec![])).await;
                ApprovalDecision::Deny
            }
        }
    }
}

struct DiscordHandler {
    cfg: Arc<HandleConfig>,
    // Mutex to prevent concurrent processing in the same channel
    active_channels: Arc<Mutex<std::collections::HashSet<String>>>,
    // Application ID set after Ready event
    app_id: std::sync::Arc<std::sync::atomic::AtomicU64>,
    // Pending approval requests
    pending_approvals: PendingApprovals,
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

        // Spawn background listener for executor task completion notifications.
        // When a task completes, the Agent processes the result and sends a summary to Discord.
        let cfg = self.cfg.clone();
        let ctx_clone = ctx.clone();
        tokio::spawn(async move {
            let mut rx = cfg.exec_event_tx.subscribe();
            info!("[Discord Notify] Background listener started for task completions");
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if let nova_ipc::Event::ProjectCompleted { project_id, report } = event {
                            info!("[Discord Notify] Task {} completed, running Agent to summarize", project_id);
                            // Run a standalone Agent turn to summarize the result
                            let msg_content = format!(
                                "<system_notification>\n后台任务 {} 执行完毕。以下是执行结果报告：\n\n{}\n\n请立刻以你的名义，用自然语言向我简述/汇报上述结果。\n</system_notification>",
                                project_id, report
                            );
                            let tool_desc = crate::tool_factory::tool_descriptions(&cfg.tools);
                            let sp = {
                                let mut bootstrap = nova_agent::BootstrapLoader::new(cfg.workspace_dir.clone());
                                bootstrap.build_system_prompt(&tool_desc)
                            };
                            let hooks = crate::tool_factory::make_hooks(cfg.workspace_dir.clone());
                            let tool_ctx = nova_tools::ToolContext::new(
                                format!("discord-notify-{}", project_id),
                                Some(cfg.workspace_dir.clone()),
                            );
                            let ql = QueryLoop::new(
                                cfg.llm_backend.clone(),
                                cfg.tools.clone(),
                                hooks,
                                cfg.loop_config.clone(),
                                None, None, None, None,
                                tool_ctx,
                            );
                            let session = nova_memory::session::manager::Session {
                                session_id: format!("discord-notify-{}", project_id),
                                messages: vec![nova_core::message::Message::user(&msg_content)],
                                turn_count: 0,
                                max_turns: cfg.loop_config.max_turns,
                                token_stats: Default::default(),
                                created_at: chrono::Utc::now(),
                                updated_at: chrono::Utc::now(),
                                last_memory_sweep_index: 0,
                                memory_updated_mutex: false,
                            };
                            let (event_tx, mut event_rx) = mpsc::channel::<LoopEvent>(64);
                            let handle = tokio::spawn(async move {
                                ql.run_turn(session, &sp, event_tx).await
                            });
                            // Collect response text
                            let mut response = String::new();
                            while let Some(ev) = event_rx.recv().await {
                                if let LoopEvent::TextDelta(t) = ev {
                                    response.push_str(&t);
                                }
                            }
                            let _ = handle.await;
                            // Send to Discord
                            if !response.is_empty() {
                                let filtered = filter_nova_os(&response);
                                if let Some(ch_id) = cfg.discord_channel_id {
                                    let channel_id = serenity::model::id::ChannelId::new(ch_id);
                                    let chars: Vec<char> = filtered.chars().collect();
                                    for chunk in chars.chunks(1950) {
                                        let chunk_str: String = chunk.iter().collect();
                                        let builder = CreateMessage::new().content(chunk_str);
                                        if let Err(e) = channel_id.send_message(&ctx_clone.http, builder).await {
                                            warn!("[Discord Notify] Failed to send: {}", e);
                                            break;
                                        }
                                    }
                                    info!("[Discord Notify] Task {} summary sent to Discord", project_id);
                                } else {
                                    warn!("[Discord Notify] No discord_channel_id configured, cannot send task {} result", project_id);
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("[Discord Notify] Lagged {} messages", n);
                    }
                }
            }
        });
    }

    async fn message(&self, ctx: Context, msg: Message) {
        // Ignore bot messages
        if msg.author.bot { return; }
        
        let content = msg.content.trim().to_string();
        if content.is_empty() && msg.attachments.is_empty() { return; }

        let session_id = msg.channel_id.to_string();

        let mut active = self.active_channels.lock().await;
        if active.contains(&session_id) {
            let builder = CreateMessage::new().content("⏳ Please wait for the previous turn to complete...");
            let _ = msg.channel_id.send_message(&ctx.http, builder).await;
            return;
        }
        active.insert(session_id.clone());
        drop(active);

        // Add eyes reaction to indicate processing
        let _ = msg.react(&ctx.http, ReactionType::Unicode("👀".to_string())).await;

        let cfg = self.cfg.clone();
        let ctx = ctx.clone();
        let msg = msg.clone();
        let active_channels = self.active_channels.clone();
        let pending = self.pending_approvals.clone();

        tokio::spawn(async move {
            let result = process_discord_message(cfg, ctx.clone(), msg.clone(), session_id.clone(), content, pending).await;
            // Remove eyes reaction
            let _ = msg.delete_reaction_emoji(&ctx.http, ReactionType::Unicode("👀".to_string())).await;
            match result {
                Ok(_) => {
                    let _ = msg.react(&ctx.http, ReactionType::Unicode("✅".to_string())).await;
                }
                Err(e) => {
                    error!("Discord process error: {}", e);
                    let _ = msg.react(&ctx.http, ReactionType::Unicode("❌".to_string())).await;
                    let builder = CreateMessage::new().content(format!("❌ Error: {}", e));
                    let _ = msg.channel_id.send_message(&ctx.http, builder).await;
                }
            }
            let mut active = active_channels.lock().await;
            active.remove(&session_id);
        });
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(cmd) => {
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
            "stop" => {
                let count = nova_core::executor::registry::TaskRegistry::global().stop_all().await;
                let msg = if count == 0 {
                    "⏹ No background tasks running.".to_string()
                } else {
                    format!("⏹ Stopped {} background task(s).", count)
                };
                let response = serenity::builder::CreateInteractionResponse::Message(
                    serenity::builder::CreateInteractionResponseMessage::new().content(msg),
                );
                let _ = cmd.create_response(&ctx.http, response).await;
            }
            "tasks" => {
                let running = nova_core::executor::registry::TaskRegistry::global().list().await;
                let mut text = if running.is_empty() {
                    "📋 No background tasks running.".to_string()
                } else {
                    let mut s = format!("🔧 Background Tasks ({}):\n", running.len());
                    for t in &running {
                        let elapsed = t.started_at.elapsed().as_secs();
                        s.push_str(&format!("  - [{}] {} ({}) — {}s ago\n", t.id, t.name, t.tool, elapsed));
                    }
                    s
                };
                // Session status
                let sessions_dir = self.cfg.sessions_dir.clone();
                let session_mgr = SessionManager::new(sessions_dir);
                let sid = channel_id.to_string();
                if let Ok(Some(s)) = session_mgr.resume_by_id(&sid) {
                    text.push_str(&format!(
                        "\n📋 Session: {} | Messages: {} | Turn: {}/{}",
                        s.session_id, s.messages.len(), s.turn_count, self.cfg.loop_config.max_turns,
                    ));
                }
                let response = serenity::builder::CreateInteractionResponse::Message(
                    serenity::builder::CreateInteractionResponseMessage::new().content(text),
                );
                let _ = cmd.create_response(&ctx.http, response).await;
            }
            _ => {
                let builder = serenity::builder::CreateInteractionResponseFollowup::new()
                    .content("Unknown command");
                let _ = cmd.create_followup(&ctx.http, builder).await;
            }
            }
            }
            Interaction::Component(component) => {
                let custom_id = &component.data.custom_id;
                let pending = self.pending_approvals.clone();

                let (decision, label) = if custom_id.starts_with("approve:") {
                    (ApprovalDecision::Allow, "Approved")
                } else if custom_id.starts_with("session:") {
                    (ApprovalDecision::AllowSession, "Approved (session)")
                } else if custom_id.starts_with("deny:") {
                    (ApprovalDecision::Deny, "Denied")
                } else {
                    return;
                };

                // Extract the ID from the custom_id
                let id = custom_id.split(':').nth(1).unwrap_or("").to_string();

                if let Some(tx) = pending.lock().await.remove(&id) {
                    let _ = tx.send(decision);
                }

                let response = serenity::builder::CreateInteractionResponse::UpdateMessage(
                    serenity::builder::CreateInteractionResponseMessage::new()
                        .content(format!("🔧 {}", label))
                        .components(vec![]),
                );
                let _ = component.create_response(&ctx.http, response).await;
            }
            _ => {}
        }
    }
}

async fn process_discord_message(
    cfg: Arc<HandleConfig>,
    ctx: Context,
    msg: Message,
    session_id: String,
    content: String,
    pending_approvals: PendingApprovals,
) -> Result<()> {
    let workspace_dir = cfg.workspace_dir.clone();
    let sessions_dir = cfg.sessions_dir.clone();
    let memories_dir = cfg.memories_dir.clone();
    let mut loop_config = cfg.loop_config.clone();
    let skills = cfg.skills.clone();
    let tools = cfg.tools.clone();
    let llm_backend = cfg.llm_backend.clone();

    let session_mgr = SessionManager::new(sessions_dir.clone());
    let bootstrap = Arc::new(Mutex::new(BootstrapLoader::new(workspace_dir.clone())));
    let tool_desc = crate::tool_factory::tool_descriptions(&tools);

    let daily_notes = DailyNotes::new(memories_dir.clone());
    let episodic = nova_agent::memory::EpisodicMemory::new(daily_notes.clone());
    let side_query = SideQuery::new(llm_backend.clone(), loop_config.model.clone());

    loop_config.memories_dir = Some(memories_dir.clone());

    let raw_consolidator = MemoryConsolidator::new(
        workspace_dir.clone(),
        SideQuery::new(llm_backend.clone(), loop_config.model.clone()),
    );
    let consolidation = nova_agent::memory::ConsolidationMemory::new(raw_consolidator.clone());
    let consolidator = Arc::new(raw_consolidator);

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

    if content.trim() == "/stop" {
        let count = nova_core::executor::registry::TaskRegistry::global().stop_all().await;
        let reply = if count == 0 {
            "⏹ No background tasks running.".to_string()
        } else {
            format!("⏹ Stopped {} background task(s).", count)
        };
        let builder = CreateMessage::new().content(reply);
        let _ = msg.channel_id.send_message(&ctx.http, builder).await;
        return Ok(());
    }

    if content.trim() == "/tasks" {
        let running = nova_core::executor::registry::TaskRegistry::global().list().await;
        let mut text = if running.is_empty() {
            "📋 No background tasks running.".to_string()
        } else {
            let mut s = format!("🔧 Background Tasks ({}):\n", running.len());
            for t in &running {
                let elapsed = t.started_at.elapsed().as_secs();
                s.push_str(&format!("  - [{}] {} ({}) — {}s ago\n", t.id, t.name, t.tool, elapsed));
            }
            s
        };
        text.push_str(&format!(
            "\n📋 Session: {} | Messages: {} | Turn: {}/{}",
            session.session_id, session.messages.len(), session.turn_count, loop_config.max_turns,
        ));
        let builder = CreateMessage::new().content(text);
        let _ = msg.channel_id.send_message(&ctx.http, builder).await;
        return Ok(());
    }


    // Download attachments from Discord message
    let attachments: Vec<MessageAttachment> = if !msg.attachments.is_empty() {
        let mut result = Vec::new();
        for att in &msg.attachments {
            if att.size > 20 * 1024 * 1024 {
                warn!("Skipping attachment {} ({} bytes exceeds 20MB limit)", att.filename, att.size);
                continue;
            }
            match att.download().await {
                Ok(bytes) => {
                    let media_type = att.content_type.clone()
                        .unwrap_or_else(|| "application/octet-stream".to_string());
                    info!("Downloaded attachment: {} ({} bytes, {})", att.filename, bytes.len(), media_type);
                    result.push(MessageAttachment {
                        filename: att.filename.clone(),
                        media_type,
                        data: bytes,
                    });
                }
                Err(e) => {
                    warn!("Failed to download attachment {}: {}", att.filename, e);
                }
            }
        }
        result
    } else {
        Vec::new()
    };

    let user_msg = if attachments.is_empty() {
        nova_core::message::Message::user(&content)
    } else {
        nova_core::message::Message::user_with_attachments(&content, attachments)
    };
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

        if let Ok(Ok(results)) = tokio::time::timeout(std::time::Duration::from_secs(15), search_fut).await {
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
    }

    let (event_tx, mut event_rx) = mpsc::channel::<LoopEvent>(64);
    let lc = loop_config.clone();
    let ep = episodic.clone();
    let sq_loop = side_query.clone();
    let session_clone = session.clone();
    let tools_clone = tools.clone();
    let cons_mem = consolidation.clone();
    let session_id_str = session.session_id.clone();
    let workspace_dir_clone = workspace_dir.clone();
    let backend_for_loop = llm_backend.clone();
    let ctx_for_approval = ctx.clone();
    let approval_channel_id = msg.channel_id;
    let pending_approvals_clone = pending_approvals;

    let loop_handle = tokio::spawn(async move {
        let hooks = crate::tool_factory::make_hooks(workspace_dir_clone.clone());
        let tool_ctx = ToolContext::new(
            session_id_str.clone(),
            Some(workspace_dir_clone),
        );
        let mut ql = QueryLoop::new(
            backend_for_loop, tools_clone, hooks, lc, Some(ep), Some(sq_loop),
            Some(cons_mem),
            None, // notify_tx
            tool_ctx,
        );
        if cfg.tool_approval_enabled {
            ql = ql.with_approval_handler(Arc::new(DiscordApprovalHandler {
                ctx: ctx_for_approval,
                channel_id: approval_channel_id,
                pending: pending_approvals_clone,
            }));
        }
        let mut s = session_clone;
        s.turn_count = 0;

        let result = ql.run_turn(s.clone(), &sp, event_tx).await;
        match result {
            Ok((updated_s, new_msgs)) => (updated_s, new_msgs),
            Err(_) => (s, vec![]),
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
            LoopEvent::FileOutput { path, filename } => {
                send_file_to_discord(&ctx, &msg, &path, &filename).await;
            }
            LoopEvent::Embed { title, description, color, fields, image_url, footer } => {
                let mut embed = serenity::builder::CreateEmbed::new();
                if let Some(t) = title { embed = embed.title(t); }
                if let Some(d) = description { embed = embed.description(d); }
                if let Some(c) = color { embed = embed.colour(c); }
                if let Some(url) = image_url { embed = embed.image(url); }
                if let Some(f) = footer { embed = embed.footer(serenity::builder::CreateEmbedFooter::new(f)); }
                for field in &fields {
                    embed = embed.field(&field.name, &field.value, field.inline);
                }
                let builder = CreateMessage::new().add_embed(embed);
                let _ = msg.channel_id.send_message(&ctx.http, builder).await;
            }
            _ => {}
        }
    }

    if let Ok((updated, new_msgs)) = loop_handle.await {
        session = updated;

        // Detect and send file outputs from tool results
        for m in &new_msgs {
            if m.role == nova_core::message::Role::Tool {
                if let Some(ref content) = m.content {
                    for line in content.lines() {
                        // Screenshot pattern
                        if let Some(rest) = line.strip_prefix("Screenshot saved: ") {
                            let path = rest.trim();
                            let filename = std::path::Path::new(path)
                                .file_name().and_then(|n| n.to_str()).unwrap_or("screenshot.png").to_string();
                            send_file_to_discord(&ctx, &msg, path, &filename).await;
                        }
                        // Generic file output pattern: [FILE:path]
                        if line.starts_with("[FILE:") && line.ends_with(']') {
                            let path = &line[6..line.len()-1];
                            let filename = std::path::Path::new(path)
                                .file_name().and_then(|n| n.to_str()).unwrap_or("file").to_string();
                            send_file_to_discord(&ctx, &msg, path, &filename).await;
                        }
                    }
                }
            }
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
        pending_approvals: Arc::new(Mutex::new(HashMap::new())),
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
        serenity::builder::CreateCommand::new("stop")
            .description("Stop all running background executor tasks"),
        serenity::builder::CreateCommand::new("tasks")
            .description("List running background tasks and session status"),
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

async fn send_file_to_discord(ctx: &Context, msg: &Message, path: &str, filename: &str) {
    let file_path = std::path::Path::new(path);
    if !file_path.exists() {
        warn!("FileOutput path does not exist: {}", path);
        return;
    }
    match CreateAttachment::path(file_path).await {
        Ok(attachment) => {
            let builder = CreateMessage::new()
                .content(format!("📎 {}", filename))
                .add_file(attachment);
            let _ = msg.channel_id.send_message(&ctx.http, builder).await;
        }
        Err(e) => warn!("Failed to send file {}: {}", path, e),
    }
}
