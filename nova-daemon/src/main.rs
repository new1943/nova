use anyhow::Result;
use std::sync::Arc;
use tracing::{info, error, warn};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use nova_agent::QueryLoopConfig;
use nova_core::config::NovaConfig;
use nova_core::llm_backend::LlmBackend;
use nova_llm::client::ApiClient;
use nova_memory::session::manager::SessionManager;
use nova_memory::sidequery::SideQuery;
use nova_tools::skills::create_shared_loader;
use nova_tools::create_shared_tracker;
use nova_ipc::IpcServer;

mod agentic_search;
mod discord;
mod discord_adapter;
mod lifecycle;
mod session_handler;
mod tool_factory;
mod session_diary;

use nova_daemon::dispatcher;
use nova_daemon::DiscordPush;
use session_handler::HandleConfig;

#[tokio::main]
async fn main() -> Result<()> {
    let config = NovaConfig::load_default()?;

    // Logging
    let log_path = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
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

    let fmt_layer = fmt::layer()
        .with_writer(std::sync::Mutex::new(log_file))
        .with_ansi(false)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .compact();

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.log_level))
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
            let mut client = nova_ipc::IpcClient::connect(lifecycle::SOCKET_PATH.as_ref()).await?;
            client.send_request(&nova_ipc::Request::Shutdown).await?;
            info!("Shutdown signal sent");
            Ok(())
        }
        _ => {
            eprintln!("Usage: nova [run|stop]");
            Ok(())
        }
    }
}

async fn run_daemon(config: NovaConfig) -> Result<()> {
    let _guard = lifecycle::PidGuard::acquire()?;

    info!(
        "NOVA daemon starting, workspace: {:?}, mode: {}, log_level: {}",
        config.workspace, config.mode, config.log_level
    );

    if let Err(e) = nova_tools::task::TaskLogger::sweep_orphans(&config.workspace).await {
        warn!("[Task] sweep_orphans failed: {}", e);
    }

    let old_agents_path = config.workspace.join("AGENTS.md");
    if old_agents_path.exists() {
        info!("Detected legacy AGENTS.md, safe to remove");
    }

    let run_mode = config.mode.clone();
    let skills = create_shared_loader(config.workspace.join("skills"))?;

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

    let server = IpcServer::bind(lifecycle::SOCKET_PATH.as_ref()).await?;
    info!("Listening on {}", lifecycle::SOCKET_PATH);

    let file_tracker = create_shared_tracker();

    // Discord push channel
    let (discord_push_tx, discord_push_rx) = if config.discord_enabled {
        let (tx, rx) = tokio::sync::mpsc::channel::<DiscordPush>(32);
        (Some(Arc::new(tx)), Some(rx))
    } else {
        (None, None)
    };

    // IPC push channel
    let ipc_push_tx = tokio::sync::broadcast::channel::<nova_ipc::Event>(32).0;

    // ShadowEvent dispatcher
    {
        let mut d = dispatcher::Dispatcher::new(config.workspace.clone());
        if let Some(ref tx) = discord_push_tx {
            d = d.with_discord_push_tx(tx.clone());
        }
        d = d.with_ipc_push_tx(Arc::new(ipc_push_tx.clone()));
        d.spawn();
    }
    info!("ShadowEvent dispatcher spawned");

    // Executor notification channel
    let (exec_notify_tx, mut exec_notify_rx) =
        tokio::sync::mpsc::channel::<nova_core::executor::types::Notification>(32);
    let (exec_event_tx, _) = tokio::sync::broadcast::channel::<nova_ipc::Event>(16);

    // Notification forwarder
    {
        let exec_bcast = exec_event_tx.clone();
        tokio::spawn(async move {
            info!("[Executor] Notification forwarder started");
            while let Some(notification) = exec_notify_rx.recv().await {
                let report = notification.to_xml();
                let event = nova_ipc::Event::ProjectCompleted {
                    project_id: notification.id.clone(),
                    report,
                };
                let receivers = exec_bcast.send(event).unwrap_or(0);
                info!(
                    "[Executor] Task {} ({}) — {} receivers",
                    notification.id, notification.name, receivers
                );
            }
        });
    }

    // Discord gateway
    if config.discord_enabled {
        if let Some(token) = config.discord_token.clone() {
            let mut discord_push_rx = discord_push_rx.unwrap();

            let tools_dc = tool_factory::make_tools(
                &run_mode,
                config.browser_chrome_path.clone(),
                config.browser_profile_dir.clone(),
                config.browser_headless.unwrap_or(true),
                file_tracker.clone(),
                Some(config.workspace.clone()),
                config.workspace.join("skills"),
                skills.clone(),
                llm_backend.clone(),
                exec_notify_tx.clone(),
            );
            let sq_dc = SideQuery::new(llm_backend.clone(), loop_config.model.clone());
            let sm_dc = SessionManager::new(config.workspace.join("sessions"));
            tools_dc.register_builtin(Box::new(tool_factory::make_agentic_search_tool(
                sq_dc, sm_dc,
            )));

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
                ipc_push_tx: None,
                exec_event_tx: exec_event_tx.clone(),
                tool_approval_enabled: config.tool_approval_enabled,
                discord_channel_id: config.discord_channel_id,
            });

            // Discord push listener
            let http = serenity::http::Http::new(&token);
            let discord_channel_id = config.discord_channel_id;
            tokio::spawn(async move {
                while let Some(push) = discord_push_rx.recv().await {
                    let resolved = if push.channel_id == "coordinator" {
                        discord_channel_id
                    } else {
                        push.channel_id.parse::<u64>().ok()
                    };
                    if let Some(ch_id) = resolved {
                        let channel = serenity::model::id::ChannelId::new(ch_id);
                        let chars: Vec<char> = push.content.chars().collect();
                        for chunk in chars.chunks(1950) {
                            let s: String = chunk.iter().collect();
                            let builder =
                                serenity::builder::CreateMessage::new().content(s);
                            if let Err(e) = channel.send_message(&http, builder).await {
                                warn!("[Discord] Push failed: {}", e);
                                break;
                            }
                        }
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

    // IPC server loop
    loop {
        match server.accept().await {
            Ok(conn) => {
                let workspace_dir = config.workspace.clone();
                let sd = config.workspace.join("sessions");
                let md = config.workspace.clone();
                let lc = loop_config.clone();
                let sk = skills.clone();

                let tools = tool_factory::make_tools(
                    &run_mode,
                    config.browser_chrome_path.clone(),
                    config.browser_profile_dir.clone(),
                    config.browser_headless.unwrap_or(true),
                    file_tracker.clone(),
                    Some(workspace_dir.clone()),
                    config.workspace.join("skills"),
                    sk.clone(),
                    llm_backend.clone(),
                    exec_notify_tx.clone(),
                );
                let sq_ipc = SideQuery::new(llm_backend.clone(), lc.model.clone());
                let sm_ipc = SessionManager::new(sd.clone());
                tools.register_builtin(Box::new(tool_factory::make_agentic_search_tool(
                    sq_ipc, sm_ipc,
                )));

                let ipc_push_tx_clone = ipc_push_tx.clone();
                let backend_clone = llm_backend.clone();
                let exec_event_tx_clone = exec_event_tx.clone();
                let heartbeat_secs = config.heartbeat_interval_secs;
                let discord_ch = config.discord_channel_id;

                tokio::spawn(async move {
                    let cfg = HandleConfig {
                        workspace_dir,
                        sessions_dir: sd,
                        memories_dir: md,
                        loop_config: lc,
                        skills: sk,
                        tools,
                        heartbeat_interval_secs: heartbeat_secs,
                        llm_backend: backend_clone,
                        discord_push_tx: None,
                        ipc_push_tx: Some(Arc::new(ipc_push_tx_clone)),
                        exec_event_tx: exec_event_tx_clone,
                        tool_approval_enabled: false,
                        discord_channel_id: discord_ch,
                    };
                    if let Err(e) = session_handler::handle_connection(conn, cfg).await {
                        error!("Connection error: {}", e);
                    }
                });
            }
            Err(e) => error!("Accept error: {}", e),
        }
    }
}
