use anyhow::Result;
use serenity::prelude::*;
use serenity::model::channel::Message;
use serenity::model::gateway::Ready;
use serenity::async_trait;
use serenity::builder::{CreateMessage, EditMessage};
use tracing::{info, warn, error, debug};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

use nova_core::agent::{QueryLoop, LoopEvent};
use nova_core::memory::consolidate::MemoryConsolidator;
use nova_core::memory::daily::DailyNotes;
use nova_core::memory::dream::DreamEngine;
use nova_core::memory::recall::MemoryRecall;
use nova_core::session::manager::SessionManager;
use nova_core::session::search::AgenticSessionSearch;
use nova_core::sidequery::SideQuery;
use nova_core::workspace::BootstrapLoader;

use crate::{HandleConfig, tool_descriptions, make_hooks, record_memory_mtime};

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

/// v2 Phase 2: Build <nova_os> thinking pipe section for Discord system prompt.
async fn discord_build_nova_os_section(
    mode_router: Arc<tokio::sync::RwLock<nova_core::memory::ModeRouter>>,
    tension_tracker: Arc<nova_core::memory::TensionTracker>,
    topic_tracker: Arc<tokio::sync::RwLock<nova_core::memory::TopicTracker>>,
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

struct DiscordHandler {
    cfg: Arc<HandleConfig>,
    // Mutex to prevent concurrent processing in the same channel
    active_channels: Arc<Mutex<std::collections::HashSet<String>>>,
}

#[async_trait]
impl EventHandler for DiscordHandler {
    async fn ready(&self, _: Context, ready: Ready) {
        info!("Discord Gateway connected as {}", ready.user.name);
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
    
    let session_mgr = SessionManager::new(sessions_dir.clone());
    let bootstrap = Arc::new(Mutex::new(BootstrapLoader::new(workspace_dir.clone())));
    let tool_desc = tool_descriptions(&tools);

    let daily_notes = DailyNotes::new(memories_dir.clone());
    let side_query = SideQuery::new(
        loop_config.api_key.clone(),
        loop_config.api_base_url.clone(),
        loop_config.model.clone(),
    );

    let recall_session_mgr = SessionManager::new(sessions_dir.clone());
    let memory_recall = MemoryRecall::new(memories_dir.clone(), side_query.clone(), recall_session_mgr);

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

    loop_config.memories_dir = Some(memories_dir.clone());

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
        let skills_guard = skills.lock().map_err(|e| anyhow::anyhow!("skills lock poisoned: {}", e))?;
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
    };

    let user_msg = nova_core::message::Message::user(&content);
    session_mgr.append_message(&mut session, user_msg)?;

    record_memory_mtime(&mut session, &workspace_dir);

    let mut sp = bootstrap.lock().await.build_system_prompt(&tool_desc);

    // v2 Phase 2: Inject <nova_os> thinking pipe hints
    let nova_os = discord_build_nova_os_section(
        mode_router.clone(),
        tension_tracker.clone(),
        topic_tracker.clone(),
    ).await;
    if !nova_os.is_empty() {
        sp.push_str("\n\n---\n\n");
        sp.push_str(&nova_os);
    }

    // Context Search
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

    let (event_tx, _event_rx) = mpsc::channel::<LoopEvent>(64);
    let lc = loop_config.clone();
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
        let cons_inner = Arc::try_unwrap(cons).unwrap_or_else(|arc| (*arc).clone());
        let ql = QueryLoop::new(tools_clone, hooks, lc, Some(dn), Some(sq_loop), Some(cons_inner), Some(tt), Some(tens), Some(mr), Some(mb));
        let mut s = session_clone;
        s.turn_count = 0;
        let result = ql.run_turn(s.clone(), &sp, event_tx).await;
        match result {
            Ok((updated_s, new_msgs)) => (updated_s, new_msgs),
            Err(_) => (s, vec![]),
        }
    });

    // IMPORTANT: await loop_handle BEFORE processing events.
    // event_tx is moved into run_turn inside loop_handle. If we process events
    // first and event_rx returns None (channel closed because event_tx dropped),
    // we'd exit the while loop while run_turn is still executing tools.
    // By awaiting first, we ensure run_turn fully completes (all tools executed)
    // before we start consuming events.
    let (updated, new_msgs) = match loop_handle.await {
        Ok(result) => result,
        Err(e) => {
            error!("Query loop task panicked: {}", e);
            return Ok(());
        }
    };
    session = updated;

    let builder = CreateMessage::new().content("🤔 Thinking...");
    let mut reply_msg = msg.channel_id.send_message(&ctx.http, builder).await?;

    let history = session_mgr.history_for(&session);
    for m in &new_msgs {
        let _ = history.append(m);
    }

    let mut text_buffer = String::new();
    for m in &new_msgs {
        // Only show content (assistant text or tool result), not tool call tracking
        if let Some(ref content) = m.content {
            text_buffer.push_str(content);
        }
    }

    debug!("Discord event loop ended, text_buffer_len={}", text_buffer.len());

    if text_buffer.is_empty() {
        text_buffer = "Done.".into();
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
        debug!("Discord: dream triggered, spawning background task");
        let de = dream_engine.clone();
        let mtime = session.token_stats.memory_mtime;
        tokio::spawn(async move {
            let _ = de.dream(mtime).await;
        });
    } else {
        debug!("Discord: dream skipped (conditions not met)");
    }

    let display = filter_nova_os(&text_buffer);
    let display_len = display.chars().count();
    debug!("Final edit: text_buffer_len={}, display_len={}", text_buffer.len(), display_len);
    if display_len > 1950 {
        let trunc_text = display.chars().take(1950).collect::<String>() + "\n...(truncated)";
        let builder = EditMessage::new().content(trunc_text);
        let _ = reply_msg.edit(&ctx.http, builder).await;
    } else {
        let builder = EditMessage::new().content(&display);
        let _ = reply_msg.edit(&ctx.http, builder).await;
    }

    Ok(())
}

pub async fn start(token: String, cfg: Arc<HandleConfig>) -> Result<()> {
    let intents = GatewayIntents::GUILD_MESSAGES | GatewayIntents::DIRECT_MESSAGES | GatewayIntents::MESSAGE_CONTENT;
    
    let handler = DiscordHandler {
        cfg,
        active_channels: Arc::new(Mutex::new(std::collections::HashSet::new())),
    };

    let mut client = Client::builder(&token, intents)
        .event_handler(handler)
        .await?;

    info!("Starting Discord Gateway...");
    client.start().await?;
    Ok(())
}
