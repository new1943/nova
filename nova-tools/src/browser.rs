//! Browser tool — High-level CDP via chromiumoxide
//!
//! Fixed issues:
//! 1. Navigate后重新获取Page（修复跨域Target替换导致的 "receiver is gone"）
//! 2. Handler JoinHandle纳入session管理，Drop时abort
//! 3. execute()中browser/page独立借用，navigate可修改page
//! 4. 端口可配置（默认9222）
//! 5. 所有CDP操作加timeout保护
//! 6. click/type后自动返回snapshot
//! 7. wait_idle加Rust侧timeout

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing::{info, warn};

use chromiumoxide::browser::Browser;
use chromiumoxide::page::Page;
use futures_util::StreamExt;

use crate::registry::{ToolHandler, ToolContext};
use crate::truncate::truncate_browser;

/// CDP 单次操作超时
const CDP_TIMEOUT: Duration = Duration::from_secs(15);
/// 导航超时（页面加载可能较慢）
const NAV_TIMEOUT: Duration = Duration::from_secs(30);
/// 导航后等待页面稳定（处理跨域 Target 切换）
const NAV_SETTLE_MS: u64 = 2000;
/// click 后等待
const CLICK_SETTLE_MS: u64 = 500;

const SNAPSHOT_COMPACT_JS: &str = r#"
(function() {
    var result = [];
    result.push('Title: ' + document.title);
    result.push('URL: ' + location.href);
    result.push('');

    var links = Array.prototype.slice.call(document.querySelectorAll('a[href]')).slice(0, 50);
    if (links.length) {
        result.push('--- Links ---');
        links.forEach(function(a, i) {
            var text = a.textContent.trim().substring(0, 80);
            if (text) result.push('[@link:' + i + '] [link] "' + text + '" href=' + a.href);
        });
        result.push('');
    }

    var textInputs = Array.prototype.slice.call(document.querySelectorAll(
        'input:not([type="hidden"]):not([type="button"]):not([type="submit"]):not([type="checkbox"]):not([type="radio"]), textarea, [contenteditable], [role="textbox"], [role="search"]'
    ));
    var otherInteractives = Array.prototype.slice.call(document.querySelectorAll(
        'button, select, input[type="button"], input[type="submit"], input[type="checkbox"], input[type="radio"], [role="button"], [role="switch"], [role="checkbox"], [role="menuitem"]'
    ));
    var inputs = textInputs.concat(otherInteractives).filter(function(item, pos, self) {
        return self.indexOf(item) === pos;
    }).slice(0, 80);
    if (inputs.length) {
        result.push('--- Interactive Elements ---');
        inputs.forEach(function(el, i) {
            var tag = el.tagName.toLowerCase();
            var type = el.type || '';
            var name = el.name || '';
            var id = el.id || '';
            var aria = el.getAttribute('aria-label') || el.title || '';
            var ph = el.placeholder || aria || '';
            var text = (el.textContent || '').trim().substring(0, 50);
            var val = (el.value || '').substring(0, 30);

            var sel;
            if (id) sel = '#' + CSS.escape(id);
            else if (name) sel = tag + '[name="' + name + '"]';
            else if (el.placeholder) sel = tag + '[placeholder="' + el.placeholder.substring(0, 40) + '"]';
            else {
                var parent = el.parentElement;
                var siblings = parent ? Array.prototype.slice.call(parent.querySelectorAll(':scope > ' + tag)) : [];
                var idx = siblings.indexOf(el);
                sel = tag + ':nth-child(' + (idx + 1) + ')';
            }

            var desc = '[@i:' + i + '] [' + tag + '] ';
            if (type) desc += 'type=' + type + ' ';
            if (name) desc += 'name="' + name + '" ';
            if (ph) desc += 'placeholder/aria="' + ph + '" ';
            if (text) desc += 'text="' + text + '" ';
            if (val) desc += 'value="' + val + '" ';
            desc += '\u2192 selector: ' + sel;
            result.push(desc);
        });
        result.push('');
    }

    result.push('--- Visible Text Content ---');
    var bodyText = document.body.innerText || '';
    result.push(bodyText.substring(0, 8000));

    return result.join('\n');
})()
"#;

/// 封装的 CDP 会话，Drop 时自动 abort handler task
struct CdpSession {
    browser: Browser,
    page: Page,
    #[allow(dead_code)]
    chrome_child: Option<tokio::process::Child>,
    handler_handle: tokio::task::JoinHandle<()>,
}

impl Drop for CdpSession {
    fn drop(&mut self) {
        self.handler_handle.abort();
    }
}

pub struct BrowserTool {
    chrome_path: Option<String>,
    user_data_dir: String,
    headless: bool,
    port: u16,
    session: Arc<Mutex<Option<CdpSession>>>,
}

impl Drop for BrowserTool {
    fn drop(&mut self) {
        if self.user_data_dir.contains("chrome-subagent") {
            if let Err(e) = std::fs::remove_dir_all(&self.user_data_dir) {
                tracing::debug!("Failed to remove SubAgent Chrome profile dir: {}", e);
            }
        }
    }
}

impl BrowserTool {
    pub fn new(chrome_path: Option<String>, user_data_dir: Option<String>, headless: bool) -> Self {
        let nova_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".nova");

        let profile_dir = user_data_dir.unwrap_or_else(|| {
            nova_dir.join("browser-profile").to_string_lossy().to_string()
        });

        // SubAgent 复用主 Agent 的 Chrome 实例（端口 9222），通过不同 Page/Tab 隔离
        // 注意：随机端口方案（find_available_port）会启动新 Chrome 实例，
        // 在 macOS 上导致 WebSocket 立即断开 → "receiver is gone"
        let port = 9222;

        Self {
            chrome_path,
            user_data_dir: profile_dir,
            headless,
            port,
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
        candidates.iter().find(|p| std::path::Path::new(p).exists()).map(|s| s.to_string())
    }

    async fn launch_chrome(&self, chrome: &str) -> Result<tokio::process::Child> {
        let mut cmd = Command::new(chrome);
        cmd.args([
            &format!("--remote-debugging-port={}", self.port),
            &format!("--user-data-dir={}", self.user_data_dir),
            "--no-first-run",
            "--no-default-browser-check",
            "--ignore-certificate-errors",
        ]);
        if self.headless { cmd.arg("--headless=new"); }
        cmd.spawn().map_err(|e| anyhow!("Failed to launch Chrome: {}", e))
    }

    async fn start_session(&self) -> Result<CdpSession> {
        let chrome = self.chrome_path.clone()
            .or_else(Self::discover_chrome)
            .ok_or_else(|| anyhow!("Chrome not found"))?;

        std::fs::create_dir_all(&self.user_data_dir)?;

        let (ws_url, child) = match self.get_browser_ws_url().await {
            Ok(url) => { info!("Chrome already running on port {}", self.port); (url, None) }
            Err(_) => {
                info!("Launching Chrome on port {}...", self.port);
                let child = self.launch_chrome(&chrome).await?;
                let mut url = String::new();
                for _ in 0..50 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if let Ok(u) = self.get_browser_ws_url().await { url = u; break; }
                }
                if url.is_empty() { anyhow::bail!("Failed to get browser ws url after 5s"); }
                (url, Some(child))
            }
        };

        let (browser, mut handler) = Browser::connect(&ws_url).await
            .map_err(|e| anyhow!("Failed to connect chromiumoxide: {}", e))?;

        // handler task 纳入管理，Drop 时自动 abort
        // 注意：不要在 Err 时 break！chromiumoxide 0.7 无法反序列化新版 Chrome 的某些 CDP 事件，
        // 但这些都是无害的未知消息。只有 stream 返回 None（WebSocket 真正断开）才应退出。
        let handler_handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if let Err(e) = h {
                    tracing::debug!("CDP handler: ignored non-fatal event: {}", e);
                }
            }
            tracing::warn!("CDP handler: WebSocket stream ended");
        });

        let pages = browser.pages().await.map_err(|e| anyhow!("Failed to list pages: {}", e))?;
        let page = if pages.is_empty() {
            browser.new_page("about:blank").await.map_err(|e| anyhow!("Failed to create page: {}", e))?
        } else {
            pages[0].clone()
        };

        Ok(CdpSession { browser, page, chrome_child: child, handler_handle })
    }

    async fn get_browser_ws_url(&self) -> Result<String> {
        let url = format!("http://127.0.0.1:{}/json/version", self.port);
        let output = Command::new("curl")
            .args(["--max-time", "2", "-s", &url])
            .output().await?;
        if !output.status.success() { anyhow::bail!("curl failed"); }
        let body = String::from_utf8_lossy(&output.stdout);
        let info: serde_json::Value = serde_json::from_str(&body)?;
        if let Some(ws_url) = info.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
            return Ok(ws_url.to_string());
        }
        anyhow::bail!("No browser webSocketDebuggerUrl found")
    }

    async fn resolve_selector(&self, page: &Page, selector: &str) -> Result<String> {
        if !selector.starts_with("[@") { return Ok(selector.to_string()); }

        let re = regex::Regex::new(r"^\[@(\w+):(\d+)\]$")?;
        let caps = re.captures(selector)
            .ok_or_else(|| anyhow!("Invalid index ref: {}", selector))?;

        let kind = caps.get(1).unwrap().as_str();
        let n: usize = caps.get(2).unwrap().as_str().parse()
            .map_err(|_| anyhow!("Invalid index: {}", selector))?;

        let js = if kind == "i" {
            format!(r#"(function() {{
                var textInputs = Array.prototype.slice.call(document.querySelectorAll('input:not([type="hidden"]):not([type="button"]):not([type="submit"]):not([type="checkbox"]):not([type="radio"]), textarea, [contenteditable], [role="textbox"], [role="search"]'));
                var others = Array.prototype.slice.call(document.querySelectorAll('button, select, input[type="button"], input[type="submit"], input[type="checkbox"], input[type="radio"], [role="button"], [role="switch"], [role="checkbox"], [role="menuitem"]'));
                var els = textInputs.concat(others).filter(function(item, pos, self) {{ return self.indexOf(item) === pos; }});
                if ({n} >= els.length) return null;
                var el = els[{n}];
                if (!el) return null;
                var path = [];
                var current = el;
                while (current && current.nodeType === 1) {{
                    if (current.id) {{ path.unshift('#' + CSS.escape(current.id)); break; }}
                    var tagName = current.tagName.toLowerCase();
                    var parent = current.parentElement;
                    if (!parent) {{ path.unshift(tagName); break; }}
                    var siblings = parent.children;
                    var sameTagSiblings = [];
                    for (var i = 0; i < siblings.length; i++) {{ if (siblings[i].tagName.toLowerCase() === tagName) {{ sameTagSiblings.push(siblings[i]); }} }}
                    if (sameTagSiblings.length > 1) {{ var idx = sameTagSiblings.indexOf(current); path.unshift(tagName + ':nth-of-type(' + (idx + 1) + ')'); }}
                    else {{ path.unshift(tagName); }}
                    current = parent;
                }}
                return path.join(' > ');
            }})()"#, n = n)
        } else if kind == "link" {
            format!(r#"(function() {{
                var links = Array.prototype.slice.call(document.querySelectorAll('a[href]'));
                if ({n} >= links.length) return null;
                var a = links[{n}];
                if (!a) return null;
                var path = [];
                var current = a;
                while (current && current.nodeType === 1) {{
                    if (current.id) {{ path.unshift('#' + CSS.escape(current.id)); break; }}
                    var tagName = current.tagName.toLowerCase();
                    var parent = current.parentElement;
                    if (!parent) {{ path.unshift(tagName); break; }}
                    var siblings = parent.children;
                    var sameTagSiblings = [];
                    for (var i = 0; i < siblings.length; i++) {{ if (siblings[i].tagName.toLowerCase() === tagName) {{ sameTagSiblings.push(siblings[i]); }} }}
                    if (sameTagSiblings.length > 1) {{ var idx = sameTagSiblings.indexOf(current); path.unshift(tagName + ':nth-of-type(' + (idx + 1) + ')'); }}
                    else {{ path.unshift(tagName); }}
                    current = parent;
                }}
                return path.join(' > ');
            }})()"#, n = n)
        } else {
            return Ok(selector.to_string());
        };

        let result = tokio::time::timeout(CDP_TIMEOUT, page.evaluate(js)).await
            .map_err(|_| anyhow!("resolve_selector timed out"))??;
        let resolved = result.into_value::<Option<String>>()?.unwrap_or_default();

        if resolved.is_empty() || resolved == "null" {
            return Err(anyhow!("Element {} not found", selector));
        }
        Ok(resolved)
    }

    async fn get_snapshot(&self, page: &Page) -> Result<String> {
        let result = tokio::time::timeout(CDP_TIMEOUT, page.evaluate(SNAPSHOT_COMPACT_JS)).await
            .map_err(|_| anyhow!("get_snapshot timed out"))??;
        let snap = result.into_value::<String>()?;
        Ok(truncate_browser(&snap))
    }

    async fn count_interactives(&self, page: &Page) -> Result<usize> {
        let js = r#"(function() { return document.querySelectorAll('input,textarea,select,button,[role="button"],[contenteditable],[role="textbox"],[role="search"]').length; })()"#;
        let result = tokio::time::timeout(CDP_TIMEOUT, page.evaluate(js)).await
            .map_err(|_| anyhow!("count_interactives timed out"))??;
        let count = result.into_value::<usize>()?;
        Ok(count)
    }
}

#[async_trait]
impl ToolHandler for BrowserTool {
    fn name(&self) -> &str { "browser" }

    fn description(&self) -> &str {
        "Search the internet, visit websites, and read web pages. Controls Chrome directly via CDP."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["navigate","snapshot","click","type","press","wait_idle","scroll_down","scroll_up","screenshot","go_back","close"] },
                "url": { "type": "string" },
                "selector": { "type": "string" },
                "text": { "type": "string" },
                "key": { "type": "string" },
                "full": { "type": "boolean" }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<String> {
        let action = args.get("action").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'action'"))?;

        if action == "close" {
            let mut guard = self.session.lock().await;
            *guard = None; // CdpSession::drop 会 abort handler task
            return Ok("Browser closed".into());
        }

        // 确保 session 存在且 handler 存活
        {
            let mut guard = self.session.lock().await;
            // 检查 handler 是否还活着，死了就清掉重建
            let needs_recreate = match guard.as_ref() {
                None => true,
                Some(s) => s.handler_handle.is_finished(),
            };
            if needs_recreate {
                if guard.is_some() {
                    warn!("CDP handler died, recreating session...");
                }
                *guard = None; // Drop 旧 session（abort handler）
                info!("Starting CDP session on port {}...", self.port);
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

                // 带超时的导航（不再调用 wait_for_navigation，避免跨域 Target 切换后 channel 断开）
                tokio::time::timeout(NAV_TIMEOUT, session.page.goto(url)).await
                    .map_err(|_| anyhow!("Navigation to {} timed out after {}s", url, NAV_TIMEOUT.as_secs()))??;

                // 等待页面稳定（替代 wait_for_navigation）
                tokio::time::sleep(Duration::from_millis(NAV_SETTLE_MS)).await;

                // ★ 关键修复：从 browser 重新获取 page，应对跨域 Target 替换
                let pages = tokio::time::timeout(CDP_TIMEOUT, session.browser.pages()).await
                    .map_err(|_| anyhow!("Failed to list pages (timeout)"))??;
                if let Some(new_page) = pages.into_iter().next() {
                    session.page = new_page;
                }

                let snap = self.get_snapshot(&session.page).await?;
                let count = self.count_interactives(&session.page).await?;
                Ok(format!("Navigated to {}\n[{} interactive elements]\n\n{}", url, count, snap))
            }
            "snapshot" => {
                let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);
                if full {
                    let result = tokio::time::timeout(CDP_TIMEOUT, session.page.evaluate("document.documentElement.outerHTML")).await
                        .map_err(|_| anyhow!("full snapshot timed out"))??;
                    let content = result.into_value::<String>().unwrap_or_default();
                    return Ok(format!("[full page]\n\n{}", truncate_browser(&content)));
                }
                let snap = self.get_snapshot(&session.page).await?;
                let count = self.count_interactives(&session.page).await?;
                Ok(format!("[{} interactive elements]\n\n{}", count, snap))
            }
            "click" => {
                let selector = args.get("selector").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("click requires 'selector'"))?;
                let resolved = self.resolve_selector(&session.page, selector).await?;
                let el = tokio::time::timeout(CDP_TIMEOUT, session.page.find_element(&resolved)).await
                    .map_err(|_| anyhow!("find_element timed out"))??;
                tokio::time::timeout(CDP_TIMEOUT, el.click()).await
                    .map_err(|_| anyhow!("click timed out"))??;
                tokio::time::sleep(Duration::from_millis(CLICK_SETTLE_MS)).await;
                // click 后自动返回 snapshot，AI 无需额外调用
                let snap = self.get_snapshot(&session.page).await?;
                Ok(format!("Clicked: {}\n\n{}", selector, snap))
            }
            "type" => {
                let selector = args.get("selector").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("type requires 'selector'"))?;
                let text = args.get("text").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("type requires 'text'"))?;
                let resolved = self.resolve_selector(&session.page, selector).await?;
                let el = tokio::time::timeout(CDP_TIMEOUT, session.page.find_element(&resolved)).await
                    .map_err(|_| anyhow!("find_element timed out"))??;
                tokio::time::timeout(CDP_TIMEOUT, el.click()).await
                    .map_err(|_| anyhow!("click before type timed out"))??;
                use chromiumoxide::cdp::browser_protocol::input::InsertTextParams;
                let insert = InsertTextParams::new(text.to_string());
                tokio::time::timeout(CDP_TIMEOUT, session.page.execute(insert)).await
                    .map_err(|_| anyhow!("InsertText timed out"))??;
                // type 后自动返回 snapshot
                let snap = self.get_snapshot(&session.page).await?;
                Ok(format!("Typed: {}\n\n{}", text, snap))
            }
            "press" => {
                let key = args.get("key").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("press requires 'key'"))?;
                use chromiumoxide::cdp::browser_protocol::input::{DispatchKeyEventParams, DispatchKeyEventType};
                let text_val = match key {
                    "Enter" => "\r", "Tab" => "\t",
                    "Backspace" | "Escape" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" => "",
                    _ => key,
                };
                let mut key_down = DispatchKeyEventParams::builder()
                    .r#type(DispatchKeyEventType::KeyDown).key(key.to_string());
                if !text_val.is_empty() { key_down = key_down.text(text_val.to_string()); }
                tokio::time::timeout(CDP_TIMEOUT, session.page.execute(key_down.build().unwrap())).await
                    .map_err(|_| anyhow!("key_down timed out"))??;
                let mut key_up = DispatchKeyEventParams::builder()
                    .r#type(DispatchKeyEventType::KeyUp).key(key.to_string());
                if !text_val.is_empty() { key_up = key_up.text(text_val.to_string()); }
                tokio::time::timeout(CDP_TIMEOUT, session.page.execute(key_up.build().unwrap())).await
                    .map_err(|_| anyhow!("key_up timed out"))??;
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(format!("Pressed: {}", key))
            }
            "scroll_down" => {
                tokio::time::timeout(CDP_TIMEOUT, session.page.evaluate("window.scrollBy(0, 600)")).await
                    .map_err(|_| anyhow!("scroll_down timed out"))??;
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok("Scrolled down".into())
            }
            "scroll_up" => {
                tokio::time::timeout(CDP_TIMEOUT, session.page.evaluate("window.scrollBy(0, -600)")).await
                    .map_err(|_| anyhow!("scroll_up timed out"))??;
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok("Scrolled up".into())
            }
            "screenshot" => {
                let dir = dirs::home_dir().unwrap_or_default().join(".nova/browser-screenshots");
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(format!("{}.png", chrono::Utc::now().format("%Y%m%d_%H%M%S")));
                use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
                use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotParams;
                let screenshot_params = CaptureScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png).build();
                tokio::time::timeout(CDP_TIMEOUT, session.page.save_screenshot(screenshot_params, &path)).await
                    .map_err(|_| anyhow!("screenshot timed out"))??;
                Ok(format!("Screenshot saved: {}", path.display()))
            }
            "wait_idle" => {
                let js = r#"new Promise((resolve) => { let timeout = null; let maxTimeout = setTimeout(() => { if (typeof observer !== 'undefined') observer.disconnect(); resolve('Max timeout'); }, 25000); let observer = new MutationObserver(() => { if (timeout) clearTimeout(timeout); timeout = setTimeout(() => { clearTimeout(maxTimeout); observer.disconnect(); resolve('DOM idle'); }, 2000); }); observer.observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true }); timeout = setTimeout(() => { clearTimeout(maxTimeout); observer.disconnect(); resolve('DOM idle (no mutations)'); }, 2000); })"#;
                // Rust 侧 30s 超时兜底，防止 JS Promise 永不 resolve
                let _ = tokio::time::timeout(Duration::from_secs(30), session.page.evaluate(js)).await;
                let snap = self.get_snapshot(&session.page).await?;
                let count = self.count_interactives(&session.page).await?;
                Ok(format!("Waited for page idle.\n[{} interactive elements]\n\n{}", count, snap))
            }
            "go_back" => {
                tokio::time::timeout(CDP_TIMEOUT, session.page.evaluate("window.history.back()")).await
                    .map_err(|_| anyhow!("go_back timed out"))??;
                tokio::time::sleep(Duration::from_millis(500)).await;

                // go_back 也可能触发跨域切换，重新获取 page
                let pages = tokio::time::timeout(CDP_TIMEOUT, session.browser.pages()).await
                    .map_err(|_| anyhow!("Failed to list pages after go_back"))??;
                if let Some(new_page) = pages.into_iter().next() {
                    session.page = new_page;
                }

                let title = tokio::time::timeout(CDP_TIMEOUT, session.page.evaluate("document.title")).await
                    .map_err(|_| anyhow!("get title timed out"))??
                    .into_value::<String>().unwrap_or_default();
                let url = tokio::time::timeout(CDP_TIMEOUT, session.page.evaluate("location.href")).await
                    .map_err(|_| anyhow!("get url timed out"))??
                    .into_value::<String>().unwrap_or_default();
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
                *g = None; // CdpSession::drop 会 abort handler
                Err(e)
            }
        }
    }
}
