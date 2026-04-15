//! Browser tool — Chrome DevTools Protocol (CDP) direct connection.
//!
//! 设计：
//! - 启动系统 Chrome（非 Playwright），完全无自动化痕迹
//! - 通过 CDP WebSocket 协议控制浏览器
//! - Chrome 进程 + WebSocket 连接在整个 connection 生命周期内持久化
//! - 反爬系统看到的就是一个正常的 Chrome 浏览器
//!
//! 前提：系统已安装 Chrome。

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
// (unused import removed)
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{info, warn};

use crate::tools::registry::Tool;

const CDP_PORT: u16 = 19222;

// ──────────────────────────────────────────────────────────────────────────────
// CDP Session — Chrome 进程 + WebSocket 连接
// ──────────────────────────────────────────────────────────────────────────────

struct CdpSession {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    /// Chrome 子进程（我们启动的才有，连接已有实例时为 None）
    _chrome: Option<tokio::process::Child>,
    next_id: u64,
}

impl CdpSession {
    /// Send a CDP command and wait for the response with matching id.
    async fn command(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let msg = json!({ "id": id, "method": method, "params": params });
        self.ws
            .send(WsMessage::Text(serde_json::to_string(&msg)?))
            .await?;

        // Read messages until we find the response with our id
        loop {
            match self.ws.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                            if let Some(error) = v.get("error") {
                                let msg = error
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("unknown CDP error");
                                anyhow::bail!("CDP: {msg}");
                            }
                            return Ok(v.get("result").cloned().unwrap_or(json!({})));
                        }
                        // Event or other response — skip
                    }
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => anyhow::bail!("CDP WebSocket error: {e}"),
                None => anyhow::bail!("CDP WebSocket closed"),
            }
        }
    }

    /// Wait for a specific CDP event (e.g. Page.loadEventFired).
    /// Returns Ok(()) on event or timeout (timeout is non-fatal).
    async fn wait_event(&mut self, event_name: &str, timeout_secs: u64) -> Result<()> {
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            match tokio::time::timeout_at(deadline, self.ws.next()).await {
                Ok(Some(Ok(WsMessage::Text(text)))) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if v.get("method").and_then(|m| m.as_str()) == Some(event_name) {
                            return Ok(());
                        }
                    }
                }
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(e))) => anyhow::bail!("WS error waiting for {event_name}: {e}"),
                Ok(None) => anyhow::bail!("WS closed waiting for {event_name}"),
                Err(_) => return Ok(()), // Timeout — continue anyway
            }
        }
    }

    /// Execute JavaScript in the page, return result as string.
    async fn eval_js(&mut self, expression: &str) -> Result<String> {
        let result = self
            .command(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;

        if let Some(exc) = result.get("exceptionDetails") {
            let text = exc
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
                .or_else(|| exc.get("text").and_then(|t| t.as_str()))
                .unwrap_or("JS error");
            anyhow::bail!("JavaScript: {text}");
        }

        let value = result
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(json!(null));

        match value {
            Value::String(s) => Ok(s),
            Value::Null => Ok("(no result)".into()),
            other => Ok(serde_json::to_string(&other)?),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// BrowserTool
// ──────────────────────────────────────────────────────────────────────────────

pub struct BrowserTool {
    chrome_executable: Option<String>,
    user_data_dir: String,
    headless: bool,
    port: u16,
    session: Arc<Mutex<Option<CdpSession>>>,
}

impl BrowserTool {
    pub fn new(
        chrome_executable: Option<String>,
        user_data_dir: Option<String>,
        headless: bool,
    ) -> Self {
        let nova_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".nova");

        let profile_dir = user_data_dir.unwrap_or_else(|| {
            nova_dir
                .join("browser-profile")
                .to_string_lossy()
                .to_string()
        });

        Self {
            chrome_executable,
            user_data_dir: profile_dir,
            headless,
            port: CDP_PORT,
            session: Arc::new(Mutex::new(None)),
        }
    }

    /// Auto-discover system Chrome path
    fn discover_chrome() -> Option<String> {
        let candidates = [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/usr/bin/google-chrome",
            "/usr/bin/chromium-browser",
            "/usr/bin/chromium",
        ];
        candidates
            .iter()
            .find(|p| std::path::Path::new(p).exists())
            .map(|s| s.to_string())
    }

    /// Launch Chrome and establish CDP WebSocket connection
    async fn spawn_chrome_session(&self) -> Result<CdpSession> {
        let chrome = self
            .chrome_executable
            .clone()
            .or_else(Self::discover_chrome)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Chrome not found. Install Chrome or set browser_chrome_path in ~/.nova/config"
                )
            })?;

        std::fs::create_dir_all(&self.user_data_dir)?;

        let mut child: Option<tokio::process::Child> = None;

        if TcpStream::connect(format!("127.0.0.1:{}", self.port))
            .await
            .is_ok()
        {
            info!("Found existing Chrome on port {}, reusing", self.port);
        } else {
            info!("Launching Chrome: {chrome}, profile: {}", self.user_data_dir);

            let mut args = vec![
                format!("--remote-debugging-port={}", self.port),
                format!("--user-data-dir={}", self.user_data_dir),
                "--no-first-run".to_string(),
                "--no-default-browser-check".to_string(),
                "--disable-default-apps".to_string(),
            ];

            if self.headless {
                args.push("--headless=new".to_string());
            }

            let c = Command::new(&chrome)
                .args(&args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| anyhow::anyhow!("Failed to launch Chrome: {e}"))?;
            child = Some(c);
        }

        // Wait for CDP port to be ready (up to 10s)
        let mut ready = false;
        for _ in 0..50 {
            if TcpStream::connect(format!("127.0.0.1:{}", self.port))
                .await
                .is_ok()
            {
                ready = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        if !ready {
            anyhow::bail!("Chrome CDP port {} not ready after 10s", self.port);
        }

        // Get WebSocket URL and connect
        let ws_url = get_ws_debugger_url(self.port).await?;
        info!("CDP connected: {ws_url}");

        let (ws, _) = connect_async(&ws_url)
            .await
            .map_err(|e| anyhow::anyhow!("CDP WebSocket connect failed: {e}"))?;

        let mut session = CdpSession {
            ws,
            _chrome: child,
            next_id: 1,
        };

        // Enable required CDP domains
        session.command("Page.enable", json!({})).await?;
        session.command("Runtime.enable", json!({})).await?;

        info!("CDP session ready");
        Ok(session)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Simple HTTP GET to localhost — get CDP WebSocket URL
// ──────────────────────────────────────────────────────────────────────────────

async fn get_ws_debugger_url(port: u16) -> Result<String> {
    let output = Command::new("curl")
        .args(["-s", "--max-time", "5", &format!("http://127.0.0.1:{port}/json")])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("curl failed to execute: {e}"))?;

    if !output.status.success() {
        anyhow::bail!("curl API check failed with status: {}", output.status);
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json_start = body
        .find('[')
        .ok_or_else(|| anyhow::anyhow!("No JSON in CDP /json response. Body: {}", body))?;

    let tabs: Vec<Value> = serde_json::from_str(&body[json_start..])?;

    let page = tabs
        .iter()
        .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        .or_else(|| tabs.first())
        .ok_or_else(|| anyhow::anyhow!("No browser tabs found"))?;

    page.get("webSocketDebuggerUrl")
        .and_then(|u| u.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("No webSocketDebuggerUrl in tab info"))
}

// ──────────────────────────────────────────────────────────────────────────────
// Snapshot JS — extract page content for LLM consumption
// ──────────────────────────────────────────────────────────────────────────────

const SNAPSHOT_JS: &str = r#"
(function() {
    let result = [];
    result.push('Title: ' + document.title);
    result.push('URL: ' + location.href);
    result.push('');

    // Visible text via TreeWalker
    const walker = document.createTreeWalker(
        document.body || document.documentElement,
        NodeFilter.SHOW_TEXT,
        { acceptNode: n => {
            const p = n.parentElement;
            if (!p) return NodeFilter.FILTER_REJECT;
            const tag = p.tagName;
            if (['SCRIPT','STYLE','NOSCRIPT','SVG'].includes(tag)) return NodeFilter.FILTER_REJECT;
            if (p.offsetHeight === 0) return NodeFilter.FILTER_REJECT;
            const text = n.textContent.trim();
            return text ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
        }}
    );
    let texts = [];
    while (walker.nextNode()) {
        const t = walker.currentNode.textContent.trim();
        if (t.length > 1) texts.push(t);
    }
    result.push('--- Page Content ---');
    result.push(texts.join('\n'));
    result.push('');

    // Links (max 50)
    const links = [...document.querySelectorAll('a[href]')].slice(0, 50);
    if (links.length) {
        result.push('--- Links ---');
        links.forEach((a, i) => {
            const text = a.textContent.trim().substring(0, 80);
            if (text) result.push('[link ' + i + '] "' + text + '" href=' + a.href);
        });
        result.push('');
    }

    // Interactive elements with CSS selectors
    const inputs = [...document.querySelectorAll('input,textarea,select,button,[role="button"],[contenteditable]')].slice(0, 40);
    if (inputs.length) {
        result.push('--- Interactive Elements ---');
        inputs.forEach((el, i) => {
            const tag = el.tagName.toLowerCase();
            const type = el.type || '';
            const name = el.name || '';
            const id = el.id || '';
            const ph = el.placeholder || '';
            const text = (el.textContent || '').trim().substring(0, 50);
            const val = (el.value || '').substring(0, 30);

            // Build a reliable CSS selector
            let sel;
            if (id) sel = '#' + CSS.escape(id);
            else if (name) sel = tag + '[name="' + name + '"]';
            else if (ph) sel = tag + '[placeholder="' + ph.substring(0, 40) + '"]';
            else {
                const parent = el.parentElement;
                const siblings = parent ? [...parent.querySelectorAll(':scope > ' + tag)] : [];
                const idx = siblings.indexOf(el);
                sel = tag + ':nth-child(' + (idx + 1) + ')';
            }

            let desc = '[' + tag + '] ';
            if (type) desc += 'type=' + type + ' ';
            if (name) desc += 'name="' + name + '" ';
            if (ph) desc += 'placeholder="' + ph + '" ';
            if (text) desc += 'text="' + text + '" ';
            if (val) desc += 'value="' + val + '" ';
            desc += '→ selector: ' + sel;
            result.push(desc);
        });
    }

    return result.join('\n');
})()
"#;

// ──────────────────────────────────────────────────────────────────────────────
// Tool trait
// ──────────────────────────────────────────────────────────────────────────────

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "Control a persistent Chrome browser via CDP (zero automation fingerprint).\n\n\
        WORKFLOW: 1) navigate to URL → 2) snapshot to read page → 3) interact (click/type/press) → 4) snapshot again.\n\n\
        IMPORTANT:\n\
        - Always 'snapshot' after 'navigate' to see the page.\n\
        - 'snapshot' returns page text + interactive elements with CSS selectors.\n\
        - Use the CSS selector from snapshot output in 'click' and 'type'.\n\
        - Session persists: cookies, login, current page preserved between calls.\n\
        - Do NOT 'close' unless completely done with the browser.\n\n\
        ACTIONS:\n\
        - navigate: Open URL. {action:'navigate', url:'https://...'}\n\
        - snapshot: Read page content + elements. {action:'snapshot'}\n\
        - click: Click element. {action:'click', selector:'#submit-btn'}\n\
        - type: Type text. {action:'type', selector:'input[name=q]', text:'query'}\n\
        - press: Key press. {action:'press', key:'Enter'}\n\
        - scroll_down / scroll_up: Scroll page.\n\
        - screenshot: Save screenshot to file.\n\
        - go_back: Navigate back.\n\
        - close: Close browser (session lost).\n\n\
        EXAMPLE — Google search:\n\
        1. browser({action:'navigate', url:'https://google.com'})\n\
        2. browser({action:'snapshot'}) → find search input selector\n\
        3. browser({action:'type', selector:'textarea[name=q]', text:'your query'})\n\
        4. browser({action:'press', key:'Enter'})\n\
        5. browser({action:'snapshot'}) → read results"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["navigate","snapshot","click","type","press",
                             "scroll_down","scroll_up","screenshot","go_back","close"]
                },
                "url": {
                    "type": "string",
                    "description": "URL to navigate to (required for 'navigate')"
                },
                "selector": {
                    "type": "string",
                    "description": "CSS selector from snapshot output (required for 'click'/'type')"
                },
                "text": {
                    "type": "string",
                    "description": "Text to type (required for 'type' action)"
                },
                "key": {
                    "type": "string",
                    "description": "Key to press: 'Enter', 'Tab', 'Escape', etc. (required for 'press')"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action'"))?;

        // Close doesn't need a session
        if action == "close" {
            let mut guard = self.session.lock().await;
            *guard = None; // Dropping Child kills Chrome
            return Ok("Browser closed".into());
        }

        let max_retries = 3;
        let mut last_error = String::new();

        for attempt in 0..max_retries {
            // Ensure session exists
            {
                let mut guard = self.session.lock().await;
                if guard.is_none() {
                    info!("Starting Chrome CDP session (attempt {})...", attempt + 1);
                    match self.spawn_chrome_session().await {
                        Ok(session) => *guard = Some(session),
                        Err(e) => {
                            let e_str = e.to_string();
                            warn!("Spawn failed: {}", e_str);
                            last_error = e_str;
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            continue;
                        }
                    }
                }
            }

            // Lock again to get the mutable session reference for the action
            let mut guard = self.session.lock().await;
            let session = guard.as_mut().unwrap();

            let result: Result<String> = match action {
                "navigate" => {
                    let url = args.get("url").and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("navigate requires 'url'"))?;
                    session.command("Page.navigate", json!({ "url": url })).await?;
                    session.wait_event("Page.loadEventFired", 15).await.ok();
                    Ok(format!("Navigated to {}", url))
                }
                "snapshot" => session.eval_js(SNAPSHOT_JS).await,
                "click" => {
                    let selector = args.get("selector").and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("click requires 'selector'"))?;
                    let sel_json = serde_json::to_string(selector)?;
                    let js = format!(r#"(function(){{const el=document.querySelector({});if(!el)return'Not found';el.scrollIntoView({{block:'center'}});el.click();return'Clicked';}})({})"#, sel_json, sel_json);
                    session.eval_js(&js).await
                }
                "type" => {
                    let selector = args.get("selector").and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("type requires 'selector'"))?;
                    let text = args.get("text").and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("type requires 'text'"))?;
                    let sel_json = serde_json::to_string(selector)?;
                    let text_json = serde_json::to_string(text)?;
                    let js = format!(r#"(function(){{const el=document.querySelector({});if(!el)return'Not found';el.focus();el.value={};el.dispatchEvent(new Event('input',{{bubbles:true}}));return'Typed';}})({},{})"#, sel_json, text_json, sel_json, text_json);
                    session.eval_js(&js).await
                }
                "press" => {
                    let key = args.get("key").and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("press requires 'key'"))?;
                    let (code, key_code) = match key {
                        "Enter" => ("Enter", 13),
                        "Tab" => ("Tab", 9),
                        "Escape" => ("Escape", 27),
                        "Backspace" => ("Backspace", 8),
                        "ArrowDown" => ("ArrowDown", 40),
                        "ArrowUp" => ("ArrowUp", 38),
                        "ArrowLeft" => ("ArrowLeft", 37),
                        "ArrowRight" => ("ArrowRight", 39),
                        " " => ("Space", 32),
                        _ => (key, 0),
                    };
                    session.command("Input.dispatchKeyEvent", json!({"type":"keyDown","key":key,"code":code,"windowsVirtualKeyCode":key_code})).await?;
                    session.command("Input.dispatchKeyEvent", json!({"type":"keyUp","key":key,"code":code,"windowsVirtualKeyCode":key_code})).await?;
                    Ok(format!("Pressed: {}", key))
                }
                "scroll_down" => session.eval_js("window.scrollBy(0,600);'Scrolled'").await,
                "scroll_up" => session.eval_js("window.scrollBy(0,-600);'Scrolled'").await,
                "screenshot" => {
                    let result = session.command("Page.captureScreenshot", json!({"format":"png"})).await?;
                    let data = result.get("data").and_then(|d| d.as_str())
                        .ok_or_else(|| anyhow::anyhow!("No screenshot data"))?;
                    let dir = dirs::home_dir().unwrap_or_default().join(".nova/browser-screenshots");
                    std::fs::create_dir_all(&dir)?;
                    let path = dir.join(format!("{}.png", chrono::Utc::now().format("%Y%m%d_%H%M%S")));
                    let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
                    std::fs::write(&path, bytes)?;
                    Ok(format!("Screenshot saved: {}", path.display()))
                }
                "go_back" => {
                    session.eval_js("history.back();'Back'").await?;
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    Ok("Went back".into())
                }
                other => Err(anyhow::anyhow!("Unknown browser action: '{other}'")),
            };

            // If success, return immediately
            if let Ok(res) = result {
                return Ok(res);
            }

            // On error, log and drop session. Next loop iteration will recreate it.
            let e_str = result.err().unwrap().to_string();
            warn!("CDP execution error: {}, resetting session and retrying...", e_str);
            last_error = e_str;
            
            // Drop our current guard before resetting the session
            drop(guard);
            
            let mut main_guard = self.session.lock().await;
            *main_guard = None;
        }

        Err(anyhow::anyhow!("Browser action failed after retries. Last error: {}", last_error))
    }
}
