# Nova Discord 接入方案

> KISS - Keep It Simple, Stupid

## 架构

```
Discord -> nova-discord -> nova-ipc (Unix Socket) -> nova-daemon -> LLM
```

## Rust 实现

```toml
# Cargo.toml
[dependencies]
serenity = "0.12"           # Discord 库
tokio = { version = "1", features = ["full"] }
nova-ipc = { path = "../nova-ipc" }
serde_json = "1"
tracing = "0.1"
dotenvy = "0.15"
```

```rust
// src/main.rs
use serenity::prelude::*;
use serenity::model::channel::Message;
use nova_ipc::{IpcClient, Event, Request};

struct DiscordBot;

#[serenity::async_trait]
impl EventHandler for DiscordBot {
    async fn message(&self, ctx: Context, msg: Message) {
        // 忽略自己
        if msg.author.id == ctx.cache.current_user_id() {
            return;
        }

        // 需要 @mention（DM 除外）
        if !msg.is_dm() && !msg.mentions_me(&ctx).await.unwrap_or(false) {
            return;
        }

        // IPC 通信
        let mut client = IpcClient::connect("/tmp/nova.sock").await.unwrap();
        client.send_request(&Request::UserMessage { content: msg.content.clone() }).await;

        // 收集响应
        let mut response = String::new();
        while let Ok(Some(event)) = client.recv_event().await {
            match event {
                Event::TextDelta { content } => response.push_str(&content),
                Event::TurnEnd | Event::Notification { message } => {
                    if !message.is_empty() { response.push_str(&message); }
                    break;
                }
                _ => {}
            }
        }

        // 发送回复（分片）
        msg.reply(&ctx.http, &response).await;
    }
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    
    let token = std::env::var("DISCORD_BOT_TOKEN").expect("DISCORD_BOT_TOKEN");
    let intents = GatewayIntents::MESSAGE_CONTENT | GatewayIntents::GUILD_MESSAGES | GatewayIntents::DIRECT_MESSAGES;
    
    Client::builder(&token, intents)
        .event_handler(DiscordBot)
        .await
        .unwrap()
        .start()
        .await;
}
```

## 添加到 Workspace

```toml
# Cargo.toml
[workspace]
members = [
    "nova-core",
    "nova-api",
    "nova-ipc",
    "nova-daemon",
    "nova-tui",
    "nova-discord",  # 新增
]
```

## 依赖

```bash
# serenity 需要编译 OpenSSL
brew install openssl@3
```

## 配置

| 环境变量 | 说明 |
|----------|------|
| DISCORD_BOT_TOKEN | Bot Token（必填） |
| DISCORD_ALLOWED_USERS | 用户白名单（可选） |
| DISCORD_REQUIRE_MENTION | 是否需要 @mention（默认 true） |
| NOVA_IPC_SOCKET | Socket 路径（默认 /tmp/nova.sock） |

## 已知限制

- 无语音支持
- 无斜杠命令
- 无审批
- 无线程绑定
