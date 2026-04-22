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
use crate::tools::truncate::truncate_browser;

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
            if (text) result.push('[@i:' + i + '] [link] "' + text + '" href=' + a.href);
        });
        result.push('');
    }

    // Interactive elements with CSS selectors and index refs
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

            let desc = '[@i:' + i + '] [' + tag + '] ';
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

// Compact snapshot — interactive elements + links only (no full page text)
const SNAPSHOT_COMPACT_JS: &str = r#"
(function() {
    let result = [];
    result.push('Title: ' + document.title);
    result.push('URL: ' + location.href);
    result.push('');

    // Links (max 50)
    const links = [...document.querySelectorAll('a[href]')].slice(0, 50);
    if (links.length) {
        result.push('--- Links ---');
        links.forEach((a, i) => {
            const text = a.textContent.trim().substring(0, 80);
            if (text) result.push('[@i:' + i + '] [link] "' + text + '" href=' + a.href);
        });
        result.push('');
    }

    // Interactive elements with CSS selectors and index refs
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

            let desc = '[@i:' + i + '] [' + tag + '] ';
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
        "Search the internet, visit websites, and read web pages.\n\
        Use this tool especially when the user asks questions like 'Search the web for...',\n\
        'Look up...', 'Open this website...', or needs real-time information from the internet.\n\n\
        It controls a persistent Chrome browser via CDP (zero automation fingerprint) to browse like a human.\n\n\
        WORKFLOW: 1) navigate to URL → 2) snapshot to read page → 3) interact (click/type/press) → 4) snapshot again.\n\n\
        IMPORTANT:\n\
        - Always 'snapshot' after 'navigate' to see the page.\n\
        - 'snapshot' returns interactive elements with [@i:N] index refs AND CSS selectors.\n\
        - Use either [@i:N] index refs (more precise) or CSS selectors in 'click' and 'type'.\n\
        - Session persists: cookies, login, current page preserved between calls.\n\
        - Do NOT 'close' unless completely done with the browser.\n\n\
        ACTIONS:\n\
        - navigate: Open URL. Returns title + URL. {action:'navigate', url:'https://...'}\n\
        - snapshot: Read page (compact=interactive only, full=page text too). {action:'snapshot', full:false}\n\
        - click: Click element by [@i:N] index or CSS selector. {action:'click', selector:'[@i:0]'} or {action:'click', selector:'button'}\n\
        - type: Type text. {action:'type', selector:'input[name=q]', text:'query'}\n\
        - press: Key press. {action:'press', key:'Enter'}\n\
        - scroll_down / scroll_up: Returns visible content preview.\n\
        - screenshot: Save screenshot to file.\n\
        - go_back: Navigate back. Returns new URL.\n\
        - close: Close browser (session lost).\n\n\
        EXAMPLE — Google search:\n\
        1. browser({action:'navigate', url:'https://google.com'})\n\
        2. browser({action:'snapshot'}) → find search input [@i:N] or selector\n\
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
                    "description": "CSS selector OR [@i:N] index ref from snapshot (required for 'click'/'type')"
                },
                "text": {
                    "type": "string",
                    "description": "Text to type (required for 'type' action)"
                },
                "key": {
                    "type": "string",
                    "description": "Key to press: 'Enter', 'Tab', 'Escape', etc. (required for 'press')"
                },
                "full": {
                    "type": "boolean",
                    "description": "If true, return full page content. If false (default), return only interactive elements."
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

                    // 获取 page title 和最终 URL
                    let title: String = session.eval_js("document.title").await?;
                    let current_url: String = session.eval_js("location.href").await?;

                    // Bot 检测
                    let blocked_patterns = [
                        "access denied", "blocked", "bot detected", "verification required",
                        "captcha", "cloudflare", "ddos protection", "checking your browser",
                        "just a moment", "attention required",
                    ];
                    let is_blocked = blocked_patterns.iter()
                        .any(|p| title.to_lowercase().contains(p));

                    // Auto-snapshot (同 Hermes)，让 LLM 立即看到页面内容
                    let snapshot_js = SNAPSHOT_COMPACT_JS;
                    let snap_result = session.eval_js(snapshot_js).await?;
                    let truncated = truncate_browser(&snap_result);
                    let count_js = r#"(function(){
                        return document.querySelectorAll('input,textarea,select,button,[role="button"],[contenteditable],a[href]').length;
                    })()"#;
                    let count: usize = session.eval_js(count_js).await?
                        .parse().unwrap_or(0);

                    let mut resp = format!("Navigated to {}\nTitle: {}\nURL: {}\n[{} interactive elements]\n\n{}",
                        url, title, current_url, count, truncated);
                    if is_blocked {
                        resp.push_str("\n⚠️ Bot detection likely — page may be blocked");
                    }
                    Ok(resp)
                }
                "snapshot" => {
                    let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
                    let js = if full { SNAPSHOT_JS } else { SNAPSHOT_COMPACT_JS };
                    let result = session.eval_js(js).await?;
                    // I/O Shield: truncate超长页面内容
                    let truncated = truncate_browser(&result);
                    if truncated.len() < result.len() {
                        tracing::warn!("browser snapshot truncated: {} -> {} chars", result.len(), truncated.len());
                    }
                    // Count interactable elements for LLM awareness
                    let count_js = r#"(function(){
                        return document.querySelectorAll('input,textarea,select,button,[role="button"],[contenteditable],a[href]').length;
                    })()"#;
                    let count: usize = session.eval_js(count_js).await?
                        .parse().unwrap_or(0);
                    Ok(format!("[{} interactive elements]\n\n{}", count, truncated))
                }
                "click" => {
                    let selector = args.get("selector").and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("click requires 'selector'"))?;
                    let sel_json = serde_json::to_string(selector)?;

                    // Record DOM fingerprint before click (for SPA detection)
                    let before_url: String = session.eval_js("location.href").await?;
                    let before_hash: String = session.eval_js(
                        "String(document.body.innerText.length)"
                    ).await?;

                    // Handle [@i:N] index refs, CSS selectors, and [@link:N] for links
                    let js = format!(r#"(function(){{
const sel = {};
// Parse index-ref formats: [@i:N] = interactive element N, [@link:N] = link N
const idxMatch = sel.match(/^\[@(\w+):(\d+)\]$/);
let el = null;
if (idxMatch) {{
    const [, type, idx] = idxMatch;
    const n = parseInt(idx, 10);
    if (type === 'i') {{
        const inputs = [...document.querySelectorAll('input,textarea,select,button,[role="button"],[contenteditable]')];
        el = inputs[n];
    }} else if (type === 'link') {{
        const links = [...document.querySelectorAll('a[href]')];
        el = links[n];
    }}
}} else {{
    el = document.querySelector(sel);
}}
if (!el) return 'Not found: '+sel;
el.scrollIntoView({{block:'center'}});
el.click();
return 'Clicked: '+el.textContent.trim().substring(0,50);
}})({})"#, sel_json, sel_json);
                    session.eval_js(&js).await?;

                    // Wait for DOM change: check if URL changed (traditional nav)
                    // or body content changed (SPA nav), up to 8s
                    let dominated = tokio::time::timeout(
                        std::time::Duration::from_secs(8),
                        async {
                            let mut attempts = 0;
                            loop {
                                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                                let url_now = session.eval_js("location.href").await?;
                                let hash_now = session.eval_js(
                                    "String(document.body.innerText.length)"
                                ).await?;
                                attempts += 1;
                                // URL changed (traditional nav) OR DOM changed (SPA nav)
                                if (url_now != before_url || hash_now != before_hash) && attempts > 1 {
                                    break;
                                }
                                if attempts >= 20 {
                                    break; // 8s hard cap
                                }
                            }
                            Ok::<(), anyhow::Error>(())
                        }
                    ).await;
                    let changed = dominated.is_ok();

                    if changed {
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        let snapshot_js = SNAPSHOT_COMPACT_JS;
                        let result = session.eval_js(snapshot_js).await?;
                        let truncated = truncate_browser(&result);
                        let title: String = session.eval_js("document.title").await?;
                        let url: String = session.eval_js("location.href").await?;
                        let count_js = r#"(function(){
                            return document.querySelectorAll('input,textarea,select,button,[role="button"],[contenteditable],a[href]').length;
                        })()"#;
                        let count: usize = session.eval_js(count_js).await?
                            .parse().unwrap_or(0);
                        return Ok(format!(
                            "Clicked → navigated to: {}\nTitle: {}\n[{} interactive elements]\n\n{}",
                            url, title, count, truncated
                        ));
                    }

                    Ok("Clicked".into())
                }
                "type" => {
                    let selector = args.get("selector").and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("type requires 'selector'"))?;
                    let text = args.get("text").and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow::anyhow!("type requires 'text'"))?;
                    let sel_js = serde_json::to_string(selector)?;
                    let text_js = serde_json::to_string(text)?;

                    // React/Vue/Angular controlled components: clear field first (select-all +
                    // Backspace), then set value via native setter, then fire input+change events.
                    let js = format!(r#"(function(){{
const sel = {};
// Parse [@i:N] index ref format (for type, we only support interactive elements)
const idxMatch = sel.match(/^\[@i:(\d+)\]$/);
let el = null;
if (idxMatch) {{
    const inputs = [...document.querySelectorAll('input,textarea,select,button,[role="button"],[contenteditable]')];
    el = inputs[parseInt(idxMatch[1], 10)];
}} else {{
    el = document.querySelector(sel);
}}
if (!el) return 'Not found: '+sel;
el.scrollIntoView({{block:'center'}});
el.focus();

// Clear: select all + Backspace
try {{
    const r = document.createRange();
    r.selectNodeContents(el);
    const s = window.getSelection();
    s.removeAllRanges();
    s.addRange(r);
    el.dispatchEvent(new KeyboardEvent('keydown',{{key:'Backspace',bubbles:true}}));
    el.dispatchEvent(new KeyboardEvent('keyup',{{key:'Backspace',bubbles:true}}));
}} catch(e) {{}}

// Native setter clear (bypasses framework value override)
const tag = el.tagName;
if (tag === 'INPUT' || tag === 'TEXTAREA') {{
    const proto = tag === 'INPUT' ? window.HTMLInputElement.prototype : window.HTMLTextAreaElement.prototype;
    const s = Object.getOwnPropertyDescriptor(proto,'value')?.set;
    if (s) s.call(el,'');
}}

// Character-by-character typing to trigger framework key handlers
const chars = {1}.split('');
for (const ch of chars) {{
    el.dispatchEvent(new KeyboardEvent('keydown',{{key:ch,code:'Key'+ch.toUpperCase(),bubbles:true,cancelable:true}}));
    el.dispatchEvent(new InputEvent('beforeinput',{{inputType:'insertText',data:ch,bubbles:true}}));
}}

// Final value set via native setter
if (tag === 'INPUT' || tag === 'TEXTAREA') {{
    const proto = tag === 'INPUT' ? window.HTMLInputElement.prototype : window.HTMLTextAreaElement.prototype;
    const s = Object.getOwnPropertyDescriptor(proto,'value')?.set;
    if (s) {{ s.call(el,{1}); }} else {{ el.value = {1}; }}
}} else {{
    el.textContent = {1};
}}

el.dispatchEvent(new Event('input',{{bubbles:true}}));
el.dispatchEvent(new Event('change',{{bubbles:true}}));
return 'Typed: '+{1};
}})({0},{1})"#, sel_js, text_js);
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

                    // For Enter: record DOM fingerprint before sending key
                    let dom_fingerprint_before: Option<String> = if key == "Enter" {
                        Some(session.eval_js("String(document.body.innerText.length)").await?)
                    } else {
                        None
                    };

                    // Focus the element (all keys)
                    session.eval_js(
                        &format!(r#"(function(){{const el=document.activeElement;if(el&&el!==document.body)el.scrollIntoView({{block:'center'}});return el?'focused':'none';}})()"#)
                    ).await?;

                    // Send keyDown + keyUp
                    session.command("Input.dispatchKeyEvent", json!({"type":"keyDown","key":key,"code":code,"windowsVirtualKeyCode":key_code})).await?;
                    session.command("Input.dispatchKeyEvent", json!({"type":"keyUp","key":key,"code":code,"windowsVirtualKeyCode":key_code})).await?;

                    if key == "Enter" {
                        // SPA note: Page.loadEventFired does NOT fire for fetch/XHR form submissions.
                        // Poll the DOM until content changes (up to 8s) — works for both traditional
                        // page navigation AND SPA fetch-based updates.
                        let dominated = tokio::time::timeout(
                            std::time::Duration::from_secs(8),
                            async {
                                let before = dom_fingerprint_before.as_deref().unwrap_or("0");
                                let mut attempts = 0;
                                loop {
                                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                                    let hash = session.eval_js(
                                        "String(document.body.innerText.length)"
                                    ).await?;
                                    attempts += 1;
                                    // DOM changed → search results or new content loaded
                                    if hash != before && attempts > 1 {
                                        break;
                                    }
                                    if attempts >= 20 {
                                        break; // 8s hard cap
                                    }
                                }
                                Ok::<(), anyhow::Error>(())
                            }
                        ).await;
                        let changed = dominated.is_ok();

                        // Settle time for DOM to fully render
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

                        // Auto-snapshot
                        let snapshot_js = SNAPSHOT_COMPACT_JS;
                        let result = session.eval_js(snapshot_js).await?;
                        let truncated = truncate_browser(&result);
                        let title: String = session.eval_js("document.title").await?;
                        let url: String = session.eval_js("location.href").await?;
                        let count_js = r#"(function(){
                            return document.querySelectorAll('input,textarea,select,button,[role="button"],[contenteditable],a[href]').length;
                        })()"#;
                        let count: usize = session.eval_js(count_js).await?
                            .parse().unwrap_or(0);
                        return Ok(format!(
                            "Pressed Enter → {}\nTitle: {}\n[{} interactive elements]\n\n{}",
                            if changed { "results loaded" } else { "no DOM change detected" },
                            title, count, truncated
                        ));
                    }

                    Ok(format!("Pressed: {}", key))
                }
                "scroll_down" => {
                    session.eval_js("window.scrollBy(0,600);'Scrolled'").await?;
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    // 返回滚动后的可见内容预览
                    let preview: String = session.eval_js(
                        r#"(function(){
                            const els = document.querySelectorAll('h1,h2,h3,h4,p,li,a,button,input,span');
                            return Array.from(els).slice(0,6)
                                .map(e=>e.textContent.trim().substring(0,80))
                                .filter(t=>t.length>3)
                                .join('\n');
                        })()"#
                    ).await?;
                    Ok(format!("Scrolled down\n\nVisible:\n{}", preview))
                }
                "scroll_up" => {
                    session.eval_js("window.scrollBy(0,-600);'Scrolled'").await?;
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    let preview: String = session.eval_js(
                        r#"(function(){
                            const els = document.querySelectorAll('h1,h2,h3,h4,p,li,a,button,input,span');
                            return Array.from(els).slice(0,6)
                                .map(e=>e.textContent.trim().substring(0,80))
                                .filter(t=>t.length>3)
                                .join('\n');
                        })()"#
                    ).await?;
                    Ok(format!("Scrolled up\n\nVisible:\n{}", preview))
                }
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
                    let before_hash: String = session.eval_js(
                        "String(document.body.innerText.length)"
                    ).await?;
                    session.eval_js("history.back();'Back'").await?;
                    // Poll DOM until it changes (SPA popstate or traditional back nav), up to 8s
                    let dominated = tokio::time::timeout(
                        std::time::Duration::from_secs(8),
                        async {
                            let mut attempts = 0;
                            loop {
                                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                                let hash_now = session.eval_js(
                                    "String(document.body.innerText.length)"
                                ).await?;
                                attempts += 1;
                                if hash_now != before_hash && attempts > 1 {
                                    break;
                                }
                                if attempts >= 20 {
                                    break;
                                }
                            }
                            Ok::<(), anyhow::Error>(())
                        }
                    ).await;
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    let new_url: String = session.eval_js("location.href").await?;
                    let title: String = session.eval_js("document.title").await?;
                    Ok(format!(
                        "Went back to: {}\nTitle: {}",
                        new_url, title
                    ))
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
