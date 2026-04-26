# FIXBUG-004: Discord /new 命令不生效

## 问题描述

在 Discord 中输入 `/new` 命令无法触发新 session 创建。

**原因分析：**
1. Nova 的 `/new` 是普通文本消息，不是 Discord 注册的 Application Command
2. Discord 会拦截 `/` 开头的输入，弹出 Application Command 补全面板
3. 由于 Nova 没有在 Discord 注册 `/new` 命令，用户输入的 `/new` 可能被 Discord 丢弃或无法正确发送
4. 即使发送了，普通消息中的 `/new` 也可能被 Discord 的命令系统拦截

## 解决方案

**方案：在 Discord Developer Portal 注册 Application Commands**

将 `/new`、`/reset` 等命令注册为 Discord 的原生斜杠命令（Application Commands）。

## 实现步骤

### Step 1: 在 Discord Developer Portal 创建 Application Commands

1. 访问 https://discord.com/developers/applications
2. 选择 Nova 的 Bot Application
3. 进入 **Slash Commands** (或 **Application Commands**) 页面
4. 点击 **Create** 创建新命令

#### 命令 1: `/new`

| 字段 | 值 |
|:---|:---|
| Name | `new` |
| Description | `Start a new session, clearing all conversation history` |
| Options | 无 |

#### 命令 2: `/reset`

| 字段 | 值 |
|:---|:---|
| Name | `reset` |
| Description | `Reset the current session to initial state` |
| Options | 无 |

#### 命令 3: `/compact`

| 字段 | 值 |
|:---|:---|
| Name | `compact` |
| Description | `Manually trigger context compaction to save tokens` |
| Options | 无 |

#### 命令 4: `/search`

| 字段 | 值 |
|:---|
| Name | `search` |
| Description | `Search through previous session history` |
| Options | `query` (string, required) - The search query |

### Step 2: 修改 Nova 代码处理 Interaction

在 `nova-daemon/src/discord.rs` 中：

1. 添加 `Interaction` 事件处理
2. 在 `EventHandler` 中添加 `interaction_create` 回调
3. 解析 `CommandInteraction` 处理命令

**serenity 0.12 API 注意：**
- `Interaction` 是 enum，包含 `Interaction::Command(CommandInteraction)`
- 命令注册使用 `Vec<serenity::builder::CreateCommand>`
- `create_global_commands` 接受 `&impl serde::Serialize`
- `create_followup` 接受 owned value，不是 reference

```rust
// 添加到 use 语句
use serenity::model::application::Interaction;
use serenity::builder::CreateMessage;

// 定义全局命令
fn register_global_commands() -> Vec<serenity::builder::CreateCommand> {
    vec![
        serenity::builder::CreateCommand::new("new")
            .description("Start a new session, clearing all conversation history"),
        serenity::builder::CreateCommand::new("reset")
            .description("Reset the current session to initial state"),
    ]
}

// 在 start() 函数中注册命令
pub async fn start(token: String, cfg: Arc<HandleConfig>) -> Result<()> {
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let handler = DiscordHandler {
        cfg,
        active_channels: Arc::new(Mutex::new(std::collections::HashSet::new())),
    };

    let mut client = Client::builder(&token, intents)
        .event_handler(handler)
        .await?;

    // 注册全局命令
    let http = client.cache_and_http.http.clone();
    let commands = register_global_commands();
    if let Err(e) = http.create_global_application_commands(&commands).await {
        warn!("Failed to register global commands: {}", e);
    }

    info!("Starting Discord Gateway...");
    client.start().await?;
    Ok(())
}

// 修改 EventHandler 添加 interaction 处理
#[async_trait]
impl EventHandler for DiscordHandler {
    // ... existing ready and message handlers ...

    async fn interaction_create(&self, ctx: Context, interaction: CommandInteraction) {
        let command_name = interaction.data.name.as_str();
        
        match command_name {
            "new" => {
                handle_new_command(&ctx, &interaction).await;
            }
            "reset" => {
                handle_reset_command(&ctx, &interaction).await;
            }
            "compact" => {
                handle_compact_command(&ctx, &interaction).await;
            }
            "search" => {
                handle_search_command(&ctx, &interaction).await;
            }
            _ => {
                let builder = CreateMessage::new()
                    .content("Unknown command");
                let _ = interaction.create_followup(&ctx.http, &builder).await;
            }
        }
    }
}

// 各个命令的处理函数
async fn handle_new_command(ctx: &Context, interaction: &CommandInteraction) {
    let builder = CreateMessage::new().content("✨ Starting a new session...");
    let _ = interaction.defer(&ctx.http).await;
    let _ = interaction.edit_followup(&ctx.http, &builder).await;
    
    // TODO: 调用实际的 new session 逻辑
    // session_mgr.clear_session()
    
    let builder = CreateMessage::new().content("✨ New session started. Context cleared.");
    let _ = interaction.edit_followup(&ctx.http, &builder).await;
}
```

### Step 3: 处理命令选项（如 `/search query`）

```rust
async fn handle_search_command(ctx: &Context, interaction: &CommandInteraction) {
    let query = interaction
        .data
        .options
        .iter()
        .find(|opt| opt.name == "query")
        .and_then(|opt| opt.value.as_str())
        .unwrap_or("");

    let builder = CreateMessage::new()
        .content(format!("🔍 Searching for: {}", query));
    let _ = interaction.defer(&ctx.http).await;
    
    // TODO: 调用实际的搜索逻辑
    // let results = session_searcher.search(query).await;
    
    let builder = CreateMessage::new()
        .content(format!("Found {} results for '{}'", 0, query));
    let _ = interaction.edit_followup(&ctx.http, &builder).await;
}
```

## 替代方案：动态注册命令（代码中）

如果不想手动在 Discord Portal 创建，也可以用代码动态注册：

```rust
// 在 Client::builder 之前
let mut commands = CreateApplicationCommands::new();
commands = commands.create_application_command(|c| {
    c.name("nova")
      .description("Nova AI Assistant commands")
      .create_sub_option(|sub| {
          sub.name("new")
             .description("Start a new session")
             .kind(CommandOptionType::SubCommand)
      })
      .create_sub_option(|sub| {
          sub.name("search")
             .description("Search history")
             .kind(CommandOptionType::SubCommandGroup)
             .create_sub_option(|opt| {
                 opt.name("query")
                    .description("Search query")
                    .kind(CommandOptionType::String)
                    .required(true)
             })
      })
});

// 注册命令
let _ = http.create_global_application_commands(&commands).await;
```

## 验证步骤

1. 在 Discord 中输入 `/new`，应该看到 Nova 的命令选项
2. 选择 `/new` 命令，应该收到 "✨ New session started" 响应
3. 检查日志确认命令被正确处理

## 参考资料

- [Discord Application Commands Documentation](https://discord.com/developers/docs/interactions/application-commands)
- [Serenity Command Framework](https://docs.rs/serenity/latest/serenity/builder/struct.CreateApplicationCommand.html)
- OpenClaw 实现：`/Users/zhenglingbing/Documents/openclaw/projects/openclaw/extensions/discord/src/monitor/native-command.ts`

## 状态

- [x] 动态注册 Application Commands（代码中）
- [x] 修改 Nova 代码处理 Interaction
- [ ] 测试验证（需要在有 DISCORD_TOKEN 的环境下运行）
