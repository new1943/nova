//! Browser tool — High-level CDP via chromiumoxide
//!
//! 架构：
//! - 基于 chromiumoxide 库封装
//! - 自动关联已存在的 Chrome，复用用户目录避免 Anti-Bot 拦截
//! - 稳定的 DOM 等待与输入控制

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

use crate::tools::registry::Tool;
use crate::tools::truncate::truncate_browser;

// ──────────────────────────────────────────────────────────────────────────────
// Snapshot JS
// ──────────────────────────────────────────────────────────────────────────────

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
    // Just include a healthy chunk of the text. Truncation handles the rest if it's too huge.
    result.push(bodyText.substring(0, 8000));

    return result.join('\n');
})()
"#;

// ──────────────────────────────────────────────────────────────────────────────
// BrowserTool
// ──────────────────────────────────────────────────────────────────────────────

pub struct BrowserTool {
    chrome_path: Option<String>,
    user_data_dir: String,
    headless: bool,
    session: Arc<Mutex<Option<(Browser, Page, Option<tokio::process::Child>)>>>,
}

impl Drop for BrowserTool {
    fn drop(&mut self) {
        // Session is already being dropped (Arc<Mutex<Option<...>>>)
        // Child process will be killed when Option<Child> is dropped
        // Clean up profile directory for SubAgent instances
        if self.user_data_dir.contains("chrome-subagent") {
            if let Err(e) = std::fs::remove_dir_all(&self.user_data_dir) {
                tracing::debug!("Failed to remove SubAgent Chrome profile dir: {}", e);
            }
        }
    }
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
            headless,
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

    /// Launch Chrome with debug port.
    async fn launch_chrome(&self, chrome: &str, port: u16) -> Result<tokio::process::Child> {
        let mut cmd = Command::new(chrome);
        cmd.args([
            &format!("--remote-debugging-port={}", port),
            &format!("--user-data-dir={}", self.user_data_dir),
            "--no-first-run",
            "--no-default-browser-check",
            "--ignore-certificate-errors",
        ]);
        if self.headless {
            cmd.arg("--headless=new");
        }
        cmd.spawn().map_err(|e| anyhow!("Failed to launch Chrome: {}", e))
    }

    /// Start CDP session using chromiumoxide
    async fn start_session(&self) -> Result<(Browser, Page, Option<tokio::process::Child>)> {
        let chrome = self
            .chrome_path
            .clone()
            .or_else(Self::discover_chrome)
            .ok_or_else(|| anyhow!("Chrome not found"))?;

        std::fs::create_dir_all(&self.user_data_dir)?;

        let port = 9222u16;

        // Try to get browser websocket URL to see if it's already running
        let (ws_url, child) = match self.get_browser_ws_url(port).await {
            Ok(url) => {
                info!("Chrome already running, connecting to browser...");
                (url, None)
            }
            Err(_) => {
                info!("Chrome not running, launching...");
                let child = self.launch_chrome(&chrome, port).await?;
                // Wait up to 5 seconds for chrome to start
                let mut url = String::new();
                for _ in 0..50 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if let Ok(u) = self.get_browser_ws_url(port).await {
                        url = u;
                        break;
                    }
                }
                if url.is_empty() {
                    anyhow::bail!("Failed to get browser ws url after 5s");
                }
                (url, Some(child))
            }
        };

        // Connect using chromiumoxide
        let (browser, mut handler) = Browser::connect(&ws_url).await
            .map_err(|e| anyhow!("Failed to connect chromiumoxide: {}", e))?;

        // Must spawn the handler
        tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if h.is_err() {
                    break;
                }
            }
        });

        // Get or create page
        let pages = browser.pages().await.map_err(|e| anyhow!("Failed to list pages: {}", e))?;
        let page = if pages.is_empty() {
            browser.new_page("about:blank").await.map_err(|e| anyhow!("Failed to create page: {}", e))?
        } else {
            pages[0].clone()
        };

        Ok((browser, page, child))
    }

    async fn get_browser_ws_url(&self, port: u16) -> Result<String> {
        let url = format!("http://127.0.0.1:{}/json/version", port);
        let output = Command::new("curl")
            .args(["--max-time", "2", "-s", &url])
            .output()
            .await?;
        if !output.status.success() {
            anyhow::bail!("curl failed");
        }
        let body = String::from_utf8_lossy(&output.stdout);
        let info: serde_json::Value = serde_json::from_str(&body)?;
        
        if let Some(ws_url) = info.get("webSocketDebuggerUrl").and_then(|v| v.as_str()) {
            return Ok(ws_url.to_string());
        }
        anyhow::bail!("No browser webSocketDebuggerUrl found");
    }

    /// Resolve [@i:N] or [@link:N] to CSS selector.
    async fn resolve_selector(&self, page: &Page, selector: &str) -> Result<String> {
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
                var textInputs = Array.prototype.slice.call(document.querySelectorAll('input:not([type="hidden"]):not([type="button"]):not([type="submit"]):not([type="checkbox"]):not([type="radio"]), textarea, [contenteditable], [role="textbox"], [role="search"]'));
                var others = Array.prototype.slice.call(document.querySelectorAll('button, select, input[type="button"], input[type="submit"], input[type="checkbox"], input[type="radio"], [role="button"], [role="switch"], [role="checkbox"], [role="menuitem"]'));
                var els = textInputs.concat(others).filter(function(item, pos, self) {{ return self.indexOf(item) === pos; }});
                if ({n} >= els.length) return null;
                var el = els[{n}];
                if (!el) return null;

                var path = [];
                var current = el;
                while (current && current.nodeType === 1) {{
                    if (current.id) {{
                        path.unshift('#' + CSS.escape(current.id));
                        break;
                    }}
                    var tagName = current.tagName.toLowerCase();
                    var parent = current.parentElement;
                    if (!parent) {{
                        path.unshift(tagName);
                        break;
                    }}
                    var siblings = parent.children;
                    var sameTagSiblings = [];
                    for (var i = 0; i < siblings.length; i++) {{
                        if (siblings[i].tagName.toLowerCase() === tagName) {{
                            sameTagSiblings.push(siblings[i]);
                        }}
                    }}
                    if (sameTagSiblings.length > 1) {{
                        var idx = sameTagSiblings.indexOf(current);
                        path.unshift(tagName + ':nth-of-type(' + (idx + 1) + ')');
                    }} else {{
                        path.unshift(tagName);
                    }}
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
                    if (current.id) {{
                        path.unshift('#' + CSS.escape(current.id));
                        break;
                    }}
                    var tagName = current.tagName.toLowerCase();
                    var parent = current.parentElement;
                    if (!parent) {{
                        path.unshift(tagName);
                        break;
                    }}
                    var siblings = parent.children;
                    var sameTagSiblings = [];
                    for (var i = 0; i < siblings.length; i++) {{
                        if (siblings[i].tagName.toLowerCase() === tagName) {{
                            sameTagSiblings.push(siblings[i]);
                        }}
                    }}
                    if (sameTagSiblings.length > 1) {{
                        var idx = sameTagSiblings.indexOf(current);
                        path.unshift(tagName + ':nth-of-type(' + (idx + 1) + ')');
                    }} else {{
                        path.unshift(tagName);
                    }}
                    current = parent;
                }}
                return path.join(' > ');
            }})()"#, n = n)
        } else {
            return Ok(selector.to_string());
        };

        let result = page.evaluate(js).await?;
        let resolved = result.into_value::<Option<String>>()?.unwrap_or_default();

        if resolved.is_empty() || resolved == "null" {
            return Err(anyhow!("Element {} not found", selector));
        }
        Ok(resolved)
    }

    async fn get_snapshot(&self, page: &Page) -> Result<String> {
        let result = page.evaluate(SNAPSHOT_COMPACT_JS).await?;
        let snap = result.into_value::<String>()?;
        Ok(truncate_browser(&snap))
    }

    async fn count_interactives(&self, page: &Page) -> Result<usize> {
        let js = r#"(function() { return document.querySelectorAll('input,textarea,select,button,[role="button"],[contenteditable],[role="textbox"],[role="search"]').length; })()"#;
        let result = page.evaluate(js).await?;
        let count = result.into_value::<usize>()?;
        Ok(count)
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str { "browser" }

    fn description(&self) -> &str {
        "Search the internet, visit websites, and read web pages.\n\
        Controls Chrome directly via CDP.\n\n\
        WORKFLOW:\n\
        1) navigate to URL\n\
        2) interact (click/type/press)\n\
        3) CRITICAL: If your action (like pressing Enter or submitting) triggers a page load or AI streaming response, you MUST call `wait_idle` next instead of `snapshot`. `wait_idle` waits for the page to settle and returns the final snapshot automatically.\n\
        4) For normal static reads, call `snapshot`.\n\n\
        IMPORTANT:\n\
        - navigate automatically returns a snapshot.\n\
        - snapshot and wait_idle return [@i:N] index refs AND CSS selectors.\n\
        - Use either in click/type.\n\
        - Do NOT close unless done with browser.\n\n\
        ACTIONS:\n\
        - navigate: {action:'navigate', url:'https://...'}\n\
        - wait_idle: {action:'wait_idle'} (wait for page generation/load to finish and auto-snapshot)\n\
        - snapshot: {action:'snapshot'} (for static pages)\n\
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
                    "enum": ["navigate","snapshot","click","type","press","wait_idle",
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
            *guard = None; // Drop browser and child
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
        let (_, page, _) = guard.as_mut().ok_or_else(|| anyhow!("No session"))?;

        let result: Result<String> = match action {
            "navigate" => {
                let url = args.get("url").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("navigate requires 'url'"))?;

                page.goto(url).await?;
                page.wait_for_navigation().await?;

                let title_result = page.evaluate("document.title").await?;
                let title = title_result.into_value::<String>().unwrap_or_default();
                let url_result = page.evaluate("location.href").await?;
                let current_url = url_result.into_value::<String>().unwrap_or_default();
                
                let snap = self.get_snapshot(page).await?;
                let count = self.count_interactives(page).await?;

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
                    let result = page.evaluate("document.documentElement.outerHTML").await?;
                    let content = result.into_value::<String>().unwrap_or_default();
                    return Ok(format!("[full page]\n\n{}", truncate_browser(&content)));
                }
                let snap = self.get_snapshot(page).await?;
                let count = self.count_interactives(page).await?;
                Ok(format!("[{} interactive elements]\n\n{}", count, snap))
            }
            "click" => {
                let selector = args.get("selector").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("click requires 'selector'"))?;
                let resolved = self.resolve_selector(page, selector).await?;

                let el = page.find_element(&resolved).await?;
                el.click().await?;

                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok("Clicked".into())
            }
            "type" => {
                let selector = args.get("selector").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("type requires 'selector'"))?;
                let text = args.get("text").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("type requires 'text'"))?;

                let resolved = self.resolve_selector(page, selector).await?;

                let el = page.find_element(&resolved).await?;
                el.click().await?; // focus it first
                
                // [FIXBUG] type_str 不能很好支持中文字符（CJK），会报 Key not found
                // 改用 Input.insertText 模拟 IME 输入法，原生支持全部 Unicode
                use chromiumoxide::cdp::browser_protocol::input::InsertTextParams;
                let insert = InsertTextParams::new(text.to_string());
                page.execute(insert).await?;

                Ok(format!("Typed: {}", text))
            }
            "press" => {
                let key = args.get("key").and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow!("press requires 'key'"))?;
                
                use chromiumoxide::cdp::browser_protocol::input::{DispatchKeyEventParams, DispatchKeyEventType};
                
                // [FIXBUG] 修正特殊按键的 text 参数。Enter 键的 text 应该是 "\r" 而不是 "Enter"
                let text_val = match key {
                    "Enter" => "\r",
                    "Tab" => "\t",
                    "Backspace" | "Escape" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" => "",
                    _ => key,
                };
                
                let mut key_down_builder = DispatchKeyEventParams::builder()
                    .r#type(DispatchKeyEventType::KeyDown)
                    .key(key.to_string());
                if !text_val.is_empty() {
                    key_down_builder = key_down_builder.text(text_val.to_string());
                }
                page.execute(key_down_builder.build().unwrap()).await?;

                let mut key_up_builder = DispatchKeyEventParams::builder()
                    .r#type(DispatchKeyEventType::KeyUp)
                    .key(key.to_string());
                if !text_val.is_empty() {
                    key_up_builder = key_up_builder.text(text_val.to_string());
                }
                page.execute(key_up_builder.build().unwrap()).await?;

                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok(format!("Pressed: {}", key))
            }
            "scroll_down" => {
                page.evaluate("window.scrollBy(0, 600)").await?;
                tokio::time::sleep(Duration::from_millis(200)).await;

                let preview = page.evaluate(r#"(function() { return Array.prototype.slice.call(document.querySelectorAll('h1,h2,h3,h4,p,li,a,button,input,span')).slice(0,6).map(function(e) { return e.textContent.trim().substring(0,80); }).filter(function(t) { return t.length > 3; }).join('\n'); })()"#).await?.into_value::<String>().unwrap_or_default();
                Ok(format!("Scrolled down\n\nVisible:\n{}", preview))
            }
            "scroll_up" => {
                page.evaluate("window.scrollBy(0, -600)").await?;
                tokio::time::sleep(Duration::from_millis(200)).await;

                let preview = page.evaluate(r#"(function() { return Array.prototype.slice.call(document.querySelectorAll('h1,h2,h3,h4,p,li,a,button,input,span')).slice(0,6).map(function(e) { return e.textContent.trim().substring(0,80); }).filter(function(t) { return t.length > 3; }).join('\n'); })()"#).await?.into_value::<String>().unwrap_or_default();
                Ok(format!("Scrolled up\n\nVisible:\n{}", preview))
            }
            "screenshot" => {
                let dir = dirs::home_dir().unwrap_or_default().join(".nova/browser-screenshots");
                std::fs::create_dir_all(&dir)?;
                let path = dir.join(format!("{}.png", chrono::Utc::now().format("%Y%m%d_%H%M%S")));

                let data = page.pdf(chromiumoxide::cdp::browser_protocol::page::PrintToPdfParams::default()).await?;
                // Wait, chromiumoxide natively supports save_screenshot
                use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
                use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotParams;
                
                let screenshot_params = CaptureScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png)
                    .build();

                // It provides save_screenshot directly
                page.save_screenshot(screenshot_params, &path).await?;

                Ok(format!("Screenshot saved: {}", path.display()))
            }
            "wait_idle" => {
                let js = r#"
                    new Promise((resolve) => {
                        let timeout = null;
                        let maxTimeout = setTimeout(() => {
                            if (typeof observer !== 'undefined') observer.disconnect();
                            resolve('Max timeout reached (25s)');
                        }, 25000);

                        let observer = new MutationObserver(() => {
                            if (timeout) clearTimeout(timeout);
                            timeout = setTimeout(() => {
                                clearTimeout(maxTimeout);
                                observer.disconnect();
                                resolve('DOM idle');
                            }, 2000);
                        });

                        observer.observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true });

                        timeout = setTimeout(() => {
                            clearTimeout(maxTimeout);
                            observer.disconnect();
                            resolve('DOM idle (no initial mutations)');
                        }, 2000);
                    })
                "#;
                
                page.evaluate(js).await?;
                let snap = self.get_snapshot(page).await?;
                let count = self.count_interactives(page).await?;
                Ok(format!("Waited for page idle.\n[{} interactive elements]\n\n{}", count, snap))
            }
            "go_back" => {
                page.evaluate("window.history.back()").await?;
                tokio::time::sleep(Duration::from_millis(500)).await;
                
                let title = page.evaluate("document.title").await?.into_value::<String>().unwrap_or_default();
                let url = page.evaluate("location.href").await?.into_value::<String>().unwrap_or_default();
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
