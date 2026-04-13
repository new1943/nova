use anyhow::Result;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use tokio::sync::mpsc;

mod app;
mod input;
mod theme;
mod ui;

use app::{App, DisplayRole};
use input::InputAction;
use nova_ipc::{Event, IpcClient, Request};

const SOCKET_PATH: &str = "/tmp/nova.sock";

#[tokio::main]
async fn main() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }
    Ok(())
}

async fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = App::new();

    // Connect to daemon
    let mut client = match IpcClient::connect(SOCKET_PATH.as_ref()).await {
        Ok(c) => {
            app.connected = true;
            app.status_text = "Connected".into();
            c
        }
        Err(e) => {
            app.status_text = format!("Cannot connect: {} — is daemon running?", e);
            terminal.draw(|f| ui::render(f, &app))?;
            // Wait for quit
            let (tx, mut rx) = mpsc::channel::<InputAction>(16);
            std::thread::spawn(move || {
                loop {
                    let a = input::poll_input();
                    if tx.blocking_send(a).is_err() { break; }
                }
            });
            loop {
                if let Ok(InputAction::Quit) = rx.try_recv() {
                    return Ok(());
                }
                terminal.draw(|f| ui::render(f, &app))?;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    };

    client.send_request(&Request::ResumeSession).await?;

    // IPC channels (large buffer to avoid back-pressure)
    let (event_tx, mut event_rx) = mpsc::channel::<Event>(512);
    let (req_tx, mut req_rx) = mpsc::channel::<Request>(16);

    // IPC reader task
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(req) = req_rx.recv() => {
                    if client.send_request(&req).await.is_err() { break; }
                }
                result = client.recv_event() => {
                    match result {
                        Ok(Some(event)) => {
                            if event_tx.send(event).await.is_err() { break; }
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
            }
        }
    });

    // Keyboard input on a dedicated OS thread
    let (input_tx, mut input_rx) = mpsc::channel::<InputAction>(16);
    std::thread::spawn(move || {
        loop {
            let action = input::poll_input();
            if input_tx.blocking_send(action).is_err() { break; }
        }
    });

    // Pending text buffer for typewriter effect
    let mut pending_text: Vec<String> = Vec::new();

    // Main render loop
    loop {
        // Drain IPC events — buffer TextDeltas for typewriter effect
        while let Ok(event) = event_rx.try_recv() {
            match event {
                Event::TextDelta { content } => {
                    pending_text.push(content);
                }
                other => handle_ipc_event(&mut app, other),
            }
        }

        // Typewriter: feed 1-2 characters per frame for visible streaming effect
        if !pending_text.is_empty() {
            let chunk = &mut pending_text[0];
            // Take 1 char at a time (2 for ASCII to keep it snappy)
            let take = {
                let first = chunk.chars().next();
                match first {
                    Some(c) if c.is_ascii() => 2.min(chunk.chars().count()),
                    _ => 1.min(chunk.chars().count()),
                }
            };
            let emit: String = chunk.chars().take(take).collect();
            let rest: String = chunk.chars().skip(take).collect();
            app.append_assistant_text(&emit);
            if rest.is_empty() {
                pending_text.remove(0);
            } else {
                pending_text[0] = rest;
            }
        }

        // Drain input actions
        while let Ok(action) = input_rx.try_recv() {
            match action {
                InputAction::Submit => {
                    if !app.input.is_empty() {
                        let text = app.take_input();
                        match text.as_str() {
                            "/quit" | "/exit" => app.should_quit = true,
                            "/new" => {
                                app.messages.clear();
                                app.commands.clear();
                                app.status_text = "New session...".into();
                                let _ = req_tx.send(Request::NewSession).await;
                            }
                            _ if text.starts_with("/search ") => {
                                let query = text.trim_start_matches("/search ").to_string();
                                app.push_message(DisplayRole::System, format!("Searching: {}...", query));
                                app.status_text = "Searching sessions...".into();
                                let _ = req_tx.send(Request::SearchSessions { query }).await;
                            }
                            _ => {
                                app.push_message(DisplayRole::User, text.clone());
                                app.status_text = "Thinking...".into();
                                let _ = req_tx.send(Request::UserMessage { content: text }).await;
                            }
                        }
                    }
                }
                InputAction::Quit => app.should_quit = true,
                InputAction::Char(c) => app.insert_char(c),
                InputAction::Backspace => app.delete_char(),
                InputAction::Left => app.move_cursor_left(),
                InputAction::Right => app.move_cursor_right(),
                InputAction::ScrollUp => app.scroll_up(),
                InputAction::ScrollDown => app.scroll_down(),
                InputAction::PageUp => { for _ in 0..10 { app.scroll_up(); } }
                InputAction::PageDown => { for _ in 0..10 { app.scroll_down(); } }
                InputAction::ToggleFocus => app.toggle_focus(),
                InputAction::None => {}
            }
        }

        if app.should_quit { break; }

        terminal.draw(|f| ui::render(f, &app))?;

        // Faster refresh when streaming text for typewriter effect
        // 20ms per char ≈ 50 chars/sec, feels like natural typing
        let delay = if pending_text.is_empty() { 16 } else { 20 };
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
    }

    Ok(())
}

fn handle_ipc_event(app: &mut App, event: Event) {
    match event {
        Event::TextDelta { content } => {
            app.append_assistant_text(&content);
        }
        Event::ToolCallStart { name, id: _ } => {
            app.push_command(name, String::new());
        }
        Event::ToolCallResult { content, .. } => {
            app.set_last_command_result(content);
        }
        Event::TurnEnd => {
            app.status_text = "Ready".into();
        }
        Event::TokenUsage { input, output, budget_pct } => {
            app.token_input = input;
            app.token_output = output;
            app.budget_pct = budget_pct;
        }
        Event::Error { message } => {
            app.push_message(DisplayRole::System, format!("Error: {}", message));
        }
        Event::SessionRestored { session_id, message_count } => {
            app.session_id = Some(session_id);
            app.status_text = format!("Restored ({} msgs)", message_count);
        }
        Event::SessionCreated { session_id } => {
            app.session_id = Some(session_id);
            app.status_text = "New session".into();
        }
        Event::Notification { message } => {
            app.push_message(DisplayRole::System, message);
        }
        Event::SearchResults { results } => {
            if results.is_empty() {
                app.push_message(DisplayRole::System, "No matching sessions found.".into());
            } else {
                let mut text = format!("Found {} sessions:\n", results.len());
                for (i, r) in results.iter().enumerate() {
                    let sid_short: String = r.session_id.chars().take(8).collect();
                    text.push_str(&format!("  {}. [{}] {} ({} msgs)\n", i + 1, sid_short, r.title, r.message_count));
                }
                app.push_message(DisplayRole::System, text);
            }
            app.status_text = "Ready".into();
        }
    }
}
