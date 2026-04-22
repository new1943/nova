//! Browser tool — Direct CDP WebSocket control of Chrome.
//!
//! 架构：
//! - 用 tokio-tungstenite 直接连 Chrome CDP WebSocket
//! - 不需要 Node.js / Playwright server
//! - Chrome 开着就直接连，没开就启动
//! - 使用正确的 CDP 命令，触发 React/Vue controlled components

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing::{info, warn};
use tokio_tungstenite::{connect_async, tungstenite::Message, WebSocketStream};

use crate::tools::registry::Tool;
use crate::tools::truncate::truncate_browser;

// ──────────────────────────────────────────────────────────────────────────────
// CDP JSON-RPC types
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct CdpRequest {
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct CdpResponse {
    id: u64,
    #[serde(rename = "result")]
    result: Option<Value>,
    #[serde(rename = "error")]
    error: Option<CdpError>,
}

#[derive(Debug, Deserialize)]
struct CdpError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
struct TargetInfo {
    id: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: Option<String>,
    #[serde(rename = "type")]
    target_type: String,
}

// ──────────────────────────────────────────────────────────────────────────────
// CDP Session
// ──────────────────────────────────────────────────────────────────────────────

struct CdpSession {
    ws: WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    _chrome_child: Option<tokio::process::Child>,
}

impl CdpSession {
    /// Send CDP command, return result.
    async fn send_cmd(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = rand_id();
        let req = CdpRequest {
            id,
            method: method.to_string(),
            params,
        };
        let msg = serde_json::to_string(&req)?;
        self.ws.send(Message::Text(msg)).await
            .map_err(|e| anyhow!("send error: {}", e))?;

        loop {
            let opt = tokio::time::timeout(Duration::from_secs(30), self.ws.next()).await;
            let msg = match opt {
                Ok(Some(Ok(m))) => m,
                Ok(Some(Err(e))) => anyhow::bail!("recv error: {}", e),
                Ok(None) => anyhow::bail!("stream ended"),
                Err(_) => anyhow::bail!("timeout waiting for CDP response"),
            };

            match msg {
                Message::Text(text) => {
                    if let Ok(resp) = serde_json::from_str::<CdpResponse>(&text) {
                        if resp.id == id {
                            if let Some(err) = resp.error {
                                anyhow::bail!("CDP error {}: {}", err.code, err.message);
                            }
                            return Ok(resp.result.unwrap_or(json!({})));
                        }
                    }
                }
                Message::Ping(data) => {
                    self.ws.send(Message::Pong(data)).await.ok();
                }
                _ => {}
            }
        }
    }

    async fn enable(&mut self, domain: &str) -> Result<()> {
        self.send_cmd(&format!("{}.enable", domain), None).await?;
        Ok(())
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Snapshot JS
// ──────────────────────────────────────────────────────────────────────────────

const SNAPSHOT_COMPACT_JS: &str = r#"
(function() {
    var result = [];
    result.push('Title: ' + document.title);
    result.push('URL: ' + location.href);
    result.push('');

    var links = Array.slice.call(document.querySelectorAll('a[href]')).slice(0, 50);
    if (links.length) {
        result.push('--- Links ---');
        links.forEach(function(a, i) {
            var text = a.textContent.trim().substring(0, 80);
            if (text) result.push('[@link:' + i + '] [link] "' + text + '" href=' + a.href);
        });
        result.push('');
    }

    var inputs = Array.slice.call(document.querySelectorAll(
        'input,textarea,select,button,[role="button"],[contenteditable]'
    )).slice(0, 40);
    if (inputs.length) {
        result.push('--- Interactive Elements ---');
        inputs.forEach(function(el, i) {
            var tag = el.tagName.toLowerCase();
            var type = el.type || '';
            var name = el.name || '';
            var id = el.id || '';
            var ph = el.placeholder || '';
            var text = (el.textContent || '').trim().substring(0, 50);
            var val = (el.value || '').substring(0, 30);

            var sel;
            if (id) sel = '#' + CSS.escape(id);
            else if (name) sel = tag + '[name="' + name + '"]';
            else if (ph) sel = tag + '[placeholder="' + ph.substring(0, 40) + '"]';
            else {
                var parent = el.parentElement;
                var siblings = parent ? Array.slice.call(parent.querySelectorAll(':scope > ' + tag)) : [];
                var idx = siblings.indexOf(el);
                sel = tag + ':nth-child(' + (idx + 1) + ')';
            }

            var desc = '[@i:' + i + '] [' + tag + '] ';
            if (type) desc += 'type=' + type + ' ';
            if (name) desc += 'name="' + name + '" ';
            if (ph) desc += 'placeholder="' + ph + '" ';
            if (text) desc += 'text="' + text + '" ';
            if (val) desc += 'value="' + val + '" ';
            desc += '\u2192 selector: ' + sel;
            result.push(desc);
        });
    }

    return result.join('\n');
})()
"#;

// ──────────────────────────────────────────────────────────────────────────────
// BrowserTool
// ──────────────────────────────────────────────────────────────────────────────

pub struct BrowserTool {
    chrome_path: Option<String>,
    user_data_dir: String,
    _headless: bool,
    session: Arc<Mutex<Option<CdpSession>>>,
}

fn rand_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::SeqCst)
}

impl BrowserTool {
    pub fn new(
        chrome_path: Option<String>,
        user_data_dir: Option<String>,
        headless: bool,
    ) -> Self {
        let nova_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".nova");

        let profile_dir = user_data_dir.unwrap_or_else(|| {
            nova_dir.join("browser-profile").to_string_lossy().to_string()
        });

        Self {
            chrome_path,
            user_data_dir: profile_dir,
            _headless: headless,
            session: Arc::new(Mutex::new(None)),
        }
    }

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

    /// Get page WebSocket URL from /json endpoint.
    async fn get_page_ws_url(&self, port: u16) -> Result<String> {
        let url = format!("http://127.0.0.1:{}/json", port);
        let output = Command::new("curl")
            .args(["--max-time", "2", &url])
            .output()
            .await?;
        if !output.status.success() {
            anyhow::bail!("curl failed");
        }
        let body = String::from_utf8_lossy(&output.stdout);
        if body.is_empty() {
            anyhow::bail!("empty response");
        }
        let targets: Vec<TargetInfo> = serde_json::from_str(&body)
            .map_err(|e| anyhow!("failed to parse: {}", e))?;

        for target in targets {
            if target.target_type == "page" {
                if let Some(ws_url) = target.web_socket_debugger_url {
                    return Ok(ws_url);
                }
            }
        }
        anyhow::bail!("No page target found");
    }

    /// Wait for Chrome CDP port to have a page target.
    async fn wait_for_page(&self, port: u16) -> Result<String> {
        for _ in 0..100 {
            if let Ok(url) = self.get_page_ws_url(port).await {
                return Ok(url);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("No page target after 10s")
    }

    /// Connect WebSocket with retries.
    async fn connect_ws(&self, url: &str) -> Result<WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>> {
        info!("WS connecting to: {}", url);
        for attempt in 0..5 {
            match connect_async(url).await {
                Ok((ws, _)) => {
                    info!("WS connected");
                    return Ok(ws);
                }
                Err(e) if attempt < 4 => {
                    info!("WS attempt {} failed: {}, retrying...", attempt + 1, e);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Err(e) => anyhow::bail!("WS connect failed after 5 attempts: {}", e),
            }
        }
        unreachable!()
    }

    /// Launch Chrome with debug port.
    async fn launch_chrome(&self, chrome: &str, port: u16) -> Result<tokio::process::Child> {
        Command::new(chrome)
            .args([
                &format!("--remote-debugging-port={}", port),
                &format!("--user-data-dir={}", self.user_data_dir),
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-default-apps",
                "--disable-extensions",
                "--disable-popup-blocking",
            ])
            .spawn()
            .map_err(|e| anyhow!("Failed to launch Chrome: {}", e))
    }

    /// Start CDP session.
    async fn start_session(&self) -> Result<CdpSession> {
        let chrome = self
            .chrome_path
            .clone()
            .or_else(Self::discover_chrome)
            .ok_or_else(|| anyhow!("Chrome not found"))?;

        std::fs::create_dir_all(&self.user_data_dir)?;

        let port = 9222u16;

        // Step 1: Try to get page WebSocket URL (Chrome already running)
        match self.get_page_ws_url(port).await {
            Ok(page_url) => {
                // Chrome is running, connect to existing page
                info!("Chrome already running, connecting to page...");
                let ws = self.connect_ws(&page_url).await?;
                let mut session = CdpSession { ws, _chrome_child: None };
                session.enable("Page").await?;
                session.enable("Runtime").await?;
                session.enable("DOM").await?;
                info!("CDP session ready (existing Chrome)");
                return Ok(session);
            }
            Err(_) => {
                // Chrome not running, launch it
                info!("Chrome not running, launching...");
                let child = self.launch_chrome(&chrome, port).await?;
                tokio::time::sleep(Duration::from_secs(2)).await;
                let page_url = self.wait_for_page(port).await?;
                let ws = self.connect_ws(&page_url).await?;
                let mut session = CdpSession { ws, _chrome_child: Some(child) };
                session.enable("Page").await?;
                session.enable("Runtime").await?;
                session.enable("DOM").await?;
                info!("CDP session ready (new Chrome)");
                return Ok(session);
            }
        }
    }

    /// Resolve [@i:N] or [@link:N] to CSS selector.
    async fn resolve_selector(&self, session: &mut CdpSession, selector: &str) -> Result<String> {
        if !selector.starts_with("[@") {
            return Ok(selector.to_string());
        }

        let re = regex::Regex::new(r"^\[@(\w+):(\d+)\]$")?;
        let caps = re.captures(selector)
            .ok_or_else(|| anyhow!("Invalid index ref: {}", selector))?;

        let kind = caps.get(1).unwrap().as_str();
        let n: usize = caps.get(2).unwrap().as_str().parse()
            .map_err(|_| anyhow!("Invalid index: {}", selector))?;

        let js = if kind == "i" {
            format!(r#"(function() {{
                var els = Array.slice.call(document.querySelectorAll(
                    'input,textarea,select,button,[role="button"],[contenteditable]'
                ));
                if ({n} >= els.length) return null;
                var el = els[{n}];
                if (!el) return null;
                if (el.id) return '#' + CSS.escape(el.id);
                if (el.name) return el.tagName.toLowerCase() + '[name="' + el.name + '"]';
                if (el.placeholder) return el.tagName.toLowerCase()
                    + '[placeholder="' + el.placeholder.substring(0,40) + '"]';
                var parent = el.parentElement;
                var siblings = parent ? Array.slice.call(
                    parent.querySelectorAll(':scope > ' + el.tagName.toLowerCase())
                ) : [];
                var idx = siblings.indexOf(el);
                return el.tagName.toLowerCase() + ':nth-child(' + (idx + 1) + ')';
            }})()"#, n = n)
        } else if kind == "link" {
            format!(r#"(function() {{
                var links = Array.slice.call(document.querySelectorAll('a[href]'));
                if ({n} >= links.length) return null;
                var a = links[{n}];
                if (!a) return null;
                if (a.id) return '#' + CSS.escape(a.id);
                var parent = a.parentElement;
                var siblings = parent ? Array.slice.call(
                    parent.querySelectorAll(':scope > a')
                ) : [];
                var idx = siblings.indexOf(a);
                return 'a:nth-child(' + (idx + 1) + ')';
            }})()"#, n = n)
        } else {
            return Ok(selector.to_string());
        };

        let result: Value = session.send_cmd("Runtime.evaluate", Some(json!({
            "expression": js,
            "returnByValue": true
        }))).await?;

        let resolved = result.get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if resolved.is_empty() || resolved == "null" {
            return Err(anyhow!("Element {} not found", selector));
        }
        Ok(resolved.to_string())
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str { "browser" }

    fn description(&self) -> &str {
        "Search the internet, visit websites, and read web pages.\n\
        Controls Chrome directly via CDP (Chrome DevTools Protocol).\n\n\
        WORKFLOW: 1) navigate to URL → 2) snapshot to read page → 3) interact → 4) snapshot.\n\n\
        IMPORTANT:\n\
        - Always snapshot after navigate.\n\
        - snapshot returns [@i:N] index refs AND CSS selectors.\n\
        - Use either in click/type.\n\
        - Do NOT close unless done with browser.\n\n\
        ACTIONS:\n\
        - navigate: {action:'navigate', url:'https://...'}\n\
        - snapshot: {action:'snapshot'}\n\
        - click: {action:'click', selector:'[@i:0]'}\n\
        - type: {action:'type', selector:'input', text:'text'}\n\
        - press: {action:'press', key:'Enter'}\n\
        - scroll_down / scroll_up: {action:'scroll_down'}\n\
        - screenshot: {action:'screenshot'}\n\
        - go_back: {action:'go_back'}\n\
        - close: {action:'close'}"
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
                "url": { "type": "string" },
                "selector": { "type": "string" },
                "text": { "type": "string" },
                "key": { "type": "string" },
                "full": { "type": "boolean" }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let action = args.get("action").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'action'"))?;

        if action == "close" {
            let mut guard = self.session.lock().await;
            *guard = None;
            return Ok("Browser closed".into());
        }

        {
            let mut guard = self.session.lock().await;
            if guard.is_none() {
                info!("Starting CDP session...");
                match self.start_session().await {
                    Ok(s) => *guard = Some(s),
                    Err(e) => return Err(e),
                }
            }
        }

        let mut guard = self.session.lock().await;
        let session = guard.as_mut().ok_or_else(|| anyhow!("No session"))?;

        let result: Result<String> = match action {
            "navigate" => {
                let url = args.get("url").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("navigate requires 'url'"))?;

                let _: Value = session.send_cmd("Page.navigate", Some(json!({ "url": url }))).await?;

                // Wait for load
                self.wait_for_load(session).await?;

                let (title, current_url) = self.get_page_info(session).await?;
                let snap = self.get_snapshot(session).await?;
                let count = self.count_interactives(session).await?;

                let blocked_patterns = [
                    "access denied", "blocked", "bot detected", "verification required",
                    "captcha", "cloudflare", "ddos protection",
                ];
                let is_blocked = blocked_patterns.iter().any(|p| title.to_lowercase().contains(p));

                let mut resp = format!(
                    "Navigated to {}\nTitle: {}\nURL: {}\n[{} interactive elements]\n\n{}",
                    url, title, current_url, count, snap
                );
                if is_blocked {
                    resp.push_str("\n⚠️ Bot detection likely");
                }
                Ok(resp)
            }
            "snapshot" => {
                let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
                if full {
                    let result: Value = session.send_cmd("Runtime.evaluate", Some(json!({
                        "expression": "document.documentElement.outerHTML",
                        "returnByValue": true
                    }))).await?;
                    let content = result.get("result")
                        .and_then(|r| r.get("value"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    return Ok(format!("[full page]\n\n{}", truncate_browser(content)));
                }
                let snap = self.get_snapshot(session).await?;
                let count = self.count_interactives(session).await?;
                Ok(format!("[{} interactive elements]\n\n{}", count, snap))
            }
            "click" => {
                let selector = args.get("selector").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("click requires 'selector'"))?;
                let resolved = self.resolve_selector(session, selector).await?;

                // Get element center
                let result: Value = session.send_cmd("Runtime.evaluate", Some(json!({
                    "expression": &format!(
                        "(function() {{ var el = document.querySelector('{}'); if (!el) return null; var r = el.getBoundingClientRect(); return {{ x: r.left + r.width/2, y: r.top + r.height/2 }}; }})()",
                        resolved.replace('\'', "\\'")
                    ),
                    "returnByValue": true
                }))).await?;

                let point = result.get("result")
                    .and_then(|r| r.get("value"))
                    .ok_or_else(|| anyhow!("Could not get element position"))?;
                let x = point.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let y = point.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);

                // Scroll into view
                session.send_cmd("Runtime.evaluate", Some(json!({
                    "expression": &format!(
                        "document.querySelector('{}').scrollIntoViewIfNeeded()",
                        resolved.replace('\'', "\\'")
                    )
                }))).await.ok();

                tokio::time::sleep(Duration::from_millis(200)).await;

                // Click
                session.send_cmd("Input.dispatchMouseEvent", Some(json!({
                    "type": "mousePressed",
                    "x": x, "y": y,
                    "button": "left", "clickCount": 1
                }))).await?;
                session.send_cmd("Input.dispatchMouseEvent", Some(json!({
                    "type": "mouseReleased",
                    "x": x, "y": y,
                    "button": "left", "clickCount": 1
                }))).await?;

                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok("Clicked".into())
            }
            "type" => {
                let selector = args.get("selector").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("type requires 'selector'"))?;
                let text = args.get("text").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("type requires 'text'"))?;

                let resolved = self.resolve_selector(session, selector).await?;

                // Focus then use native setter + dispatchEvent (correct for React/Vue)
                session.send_cmd("Runtime.evaluate", Some(json!({
                    "expression": &format!(
                        "(function() {{ var el = document.querySelector('{}'); if (!el) return; el.focus(); var setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value') || Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype, 'value') || Object.getOwnPropertyDescriptor(window.HTMLSelectElement.prototype, 'value'); if (setter) setter.set.call(el, '{}'); el.dispatchEvent(new Event('input', {{ bubbles: true }})); el.dispatchEvent(new Event('change', {{ bubbles: true }})); }})()",
                        resolved.replace('\'', "\\'"),
                        text.replace('\'', "\\'")
                    )
                }))).await?;

                Ok(format!("Typed: {}", text))
            }
            "press" => {
                let key = args.get("key").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("press requires 'key'"))?;

                session.send_cmd("Input.dispatchKeyEvent", Some(json!({
                    "type": "keyDown", "text": key, "key": key
                }))).await?;
                session.send_cmd("Input.dispatchKeyEvent", Some(json!({
                    "type": "keyUp", "text": key, "key": key
                }))).await?;

                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(format!("Pressed: {}", key))
            }
            "scroll_down" => {
                session.send_cmd("Runtime.evaluate", Some(json!({
                    "expression": "window.scrollBy(0, 600)"
                }))).await?;
                tokio::time::sleep(Duration::from_millis(200)).await;

                let preview: String = session.send_cmd("Runtime.evaluate", Some(json!({
                    "expression": r#"(function() { return Array.slice.call(document.querySelectorAll('h1,h2,h3,h4,p,li,a,button,input,span')).slice(0,6).map(function(e) { return e.textContent.trim().substring(0,80); }).filter(function(t) { return t.length > 3; }).join('\n'); })()"#,
                    "returnByValue": true
                }))).await?
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

                Ok(format!("Scrolled down\n\nVisible:\n{}", preview))
            }
            "scroll_up" => {
                session.send_cmd("Runtime.evaluate", Some(json!({
                    "expression": "window.scrollBy(0, -600)"
                }))).await?;
                tokio::time::sleep(Duration::from_millis(200)).await;

                let preview: String = session.send_cmd("Runtime.evaluate", Some(json!({
                    "expression": r#"(function() { return Array.slice.call(document.querySelectorAll('h1,h2,h3,h4,p,li,a,button,input,span')).slice(0,6).map(function(e) { return e.textContent.trim().substring(0,80); }).filter(function(t) { return t.length > 3; }).join('\n'); })()"#,
                    "returnByValue": true
                }))).await?
                .get("result")
                .and_then(|r| r.get("value"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

                Ok(format!("Scrolled up\n\nVisible:\n{}", preview))
            }
            "screenshot" => {
                let result: Value = session.send_cmd("Page.captureScreenshot", Some(json!({
                    "format": "png"
                }))).await?;

                let data = result.get("data")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("No screenshot data"))?;

                let dir = dirs::home_dir().unwrap_or_default().join(".nova/browser-screenshots");
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(format!("{}.png", chrono::Utc::now().format("%Y%m%d_%H%M%S")));

                let decoded = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    data
                ).map_err(|e| anyhow!("base64 error: {}", e))?;

                std::fs::write(&path, decoded)?;
                Ok(format!("Screenshot saved: {}", path.display()))
            }
            "go_back" => {
                session.send_cmd("Page.goBack", None).await.ok();
                tokio::time::sleep(Duration::from_millis(500)).await;
                let (title, url) = self.get_page_info(session).await?;
                Ok(format!("Went back to: {}\nTitle: {}", url, title))
            }
            other => Err(anyhow!("Unknown action: '{}'", other)),
        };

        match result {
            Ok(res) => Ok(res),
            Err(e) => {
                warn!("CDP error: {}, resetting session", e);
                drop(guard);
                let mut g = self.session.lock().await;
                *g = None;
                Err(e)
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

impl BrowserTool {
    async fn wait_for_load(&self, session: &mut CdpSession) -> Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("Load timeout");
            }
            let opt = tokio::time::timeout(Duration::from_secs(1), session.ws.next()).await;
            if let Ok(Some(Ok(Message::Text(text)))) = opt {
                if let Ok(evt) = serde_json::from_str::<serde_json::Value>(&text) {
                    if evt.get("method").and_then(|m| m.as_str()) == Some("Page.loadEventFired") {
                        return Ok(());
                    }
                }
            }
            // Also check if we got a navigation-related response
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn get_page_info(&self, session: &mut CdpSession) -> Result<(String, String)> {
        let result: Value = session.send_cmd("Runtime.evaluate", Some(json!({
            "expression": "JSON.stringify({title: document.title, url: location.href})",
            "returnByValue": true
        }))).await?;

        let s = result.get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or(r#"{"title":"","url":""}"#);

        let info: serde_json::Value = serde_json::from_str(s)
            .unwrap_or_else(|_| json!({"title": "", "url": ""}));
        let title = info.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let url = info.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
        Ok((title, url))
    }

    async fn get_snapshot(&self, session: &mut CdpSession) -> Result<String> {
        let result: Value = session.send_cmd("Runtime.evaluate", Some(json!({
            "expression": SNAPSHOT_COMPACT_JS,
            "returnByValue": true
        }))).await?;

        let snap = result.get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        Ok(truncate_browser(snap))
    }

    async fn count_interactives(&self, session: &mut CdpSession) -> Result<usize> {
        let result: Value = session.send_cmd("Runtime.evaluate", Some(json!({
            "expression": "String(document.querySelectorAll('input,textarea,select,button,[role=\"button\"],[contenteditable],a[href]').length)",
            "returnByValue": true
        }))).await?;

        let s = result.get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("0");
        Ok(s.trim().parse().unwrap_or(0))
    }
}
