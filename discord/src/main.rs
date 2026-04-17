use anyhow::Result;
use serenity::all::{Context, EventHandler, GatewayIntents, Message};
use serenity::async_trait;
use std::env;
use tracing::{error, info};
use nova_ipc::{IpcClient, Event, Request};

struct DiscordBot;

#[async_trait]
impl EventHandler for DiscordBot {
    async fn message(&self, ctx: Context, msg: Message) {
        // 忽略自己的消息
        if msg.author.id == ctx.cache.current_user().id {
            return;
        }

        // 类型过滤
        if !matches!(msg.kind, serenity::all::MessageType::Regular | serenity::all::MessageType::Reply) {
            return;
        }

        // 获取配置
        let allowed_users: Vec<u64> = env::var("DISCORD_ALLOWED_USERS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();

        let allowed_channels: Vec<u64> = env::var("DISCORD_ALLOWED_CHANNELS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();

        let require_mention = env::var("DISCORD_REQUIRE_MENTION")
            .unwrap_or_else(|_| "true".to_string())
            .to_lowercase()
            != "false";

        // 白名单检查
        if !allowed_users.is_empty() && !allowed_users.contains(&msg.author.id.get()) {
            return;
        }

        if !allowed_channels.is_empty() && !allowed_channels.contains(&msg.channel_id.get()) {
            return;
        }

        // 服务器频道需要 @mention
        let content = if msg.is_dm() {
            msg.content.clone()
        } else {
            if require_mention && !msg.mentions_me(&ctx).await.unwrap_or(false) {
                return;
            }
            // 去掉 mention 前缀
            let bot_id = ctx.cache.current_user().id.get().to_string();
            msg.content
                .replace(&format!("<@{}>", bot_id), "")
                .replace(&format!("<@!{}>", bot_id), "")
                .trim()
                .to_string()
        };

        if content.is_empty() {
            return;
        }

        info!("Discord 收到消息: {}", content);

        // 发送到 Nova
        match send_to_nova(&content).await {
            Ok(response) => {
                if let Err(e) = msg.reply(&ctx.http, &response).await {
                    error!("发送回复失败: {}", e);
                }
            }
            Err(e) => {
                error!("Nova IPC 错误: {}", e);
                let _ = msg.reply(&ctx.http, "抱歉，无法连接到 Nova。").await;
            }
        }
    }
}

async fn send_to_nova(content: &str) -> Result<String> {
    let socket_path = env::var("NOVA_IPC_SOCKET")
        .unwrap_or_else(|_| "/tmp/nova.sock".to_string());

    let mut client = IpcClient::connect(std::path::Path::new(&socket_path)).await?;

    // 发送请求
    let request = Request::UserMessage {
        content: content.to_string(),
    };
    client.send_request(&request).await?;

    // 收集响应
    let mut response = String::new();

    while let Some(event) = client.recv_event().await? {
        match event {
            Event::TextDelta { content } => response.push_str(&content),
            Event::Notification { message } => {
                if !message.is_empty() {
                    response.push_str(&message);
                }
            }
            Event::TurnEnd => break,
            Event::Error { message } => {
                response = format!("错误: {}", message);
                break;
            }
            _ => {}
        }
    }

    if response.is_empty() {
        response = "抱歉，Nova 没有返回任何内容。".to_string();
    }

    Ok(response)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 加载 .env 文件
    dotenvy::dotenv().ok();

    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    // 获取 Token
    let token = env::var("DISCORD_BOT_TOKEN").expect("DISCORD_BOT_TOKEN 未设置");

    info!("启动 Nova Discord Bot...");

    // 构建 Intent
    let intents = GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES;

    // 启动 Bot
    let mut client = serenity::Client::builder(&token, intents)
        .event_handler(DiscordBot)
        .await?;

    client.start().await?;

    Ok(())
}
