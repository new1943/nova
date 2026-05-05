use anyhow::Result;
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste},
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
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableBracketedPaste)?;
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
            terminal.draw(|f| ui::render(f, &mut app))?;
            // Wait for quit
            let (tx, mut rx) = mpsc::channel::<InputAction>(16);
            std::thread::spawn(move || loop {
                let a = input::poll_input();
                if tx.blocking_send(a).is_err() {
                    break;
                }
            });
            loop {
                if let Ok(InputAction::Quit) = rx.try_recv() {
                    return Ok(());
                }
                terminal.draw(|f| ui::render(f, &mut app))?;
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    };

    client.send_request(&Request::ResumeSession).await?;

    // IPC channels
    let (event_tx, mut event_rx) = mpsc::channel::<Event>(512);
    let (req_tx, mut req_rx) = mpsc::channel::<Request>(16);

    // IPC reader task
    let event_tx_clone = event_tx.clone();
    tokio::spawn(async move {
        let mut current_client = client;
        loop {
            let disconnected = tokio::select! {
                Some(req) = req_rx.recv() => {
                    current_client.send_request(&req).await.is_err()
                }
                result = current_client.recv_event() => {
                    match result {
                        Ok(Some(event)) => {
                            if event_tx_clone.send(event).await.is_err() { break; }
                            false
                        }
                        Ok(None) | Err(_) => true,
                    }
                }
            };

            if disconnected {
                let _ = event_tx_clone.send(Event::Error { message: "Daemon disconnected. Trying to reconnect...".into() }).await;
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    if let Ok(c) = IpcClient::connect(SOCKET_PATH.as_ref()).await {
                        current_client = c;
                        let _ = event_tx_clone.send(Event::Notification { message: "Reconnected to daemon.".into() }).await;
                        let _ = current_client.send_request(&Request::ResumeSession).await;
                        break;
                    }
                }
            }
        }
    });

    // Keyboard input on a dedicated OS thread
    let (input_tx, mut input_rx) = mpsc::channel::<InputAction>(16);
    std::thread::spawn(move || loop {
        let action = input::poll_input();
        if input_tx.blocking_send(action).is_err() {
            break;
        }
    });

    // Main render loop
    loop {
        // Drain ALL IPC events immediately — no artificial typewriter delay.
        // The key insight from Claude Code and OpenClaw: render the full
        // streamed text immediately. Users want to see output as fast as
        // possible, not watch a fake typing animation.
        while let Ok(event) = event_rx.try_recv() {
            handle_ipc_event(&mut app, event);
        }

        // Drain input actions
        while let Ok(action) = input_rx.try_recv() {
            match action {
                InputAction::Submit => {
                    if !app.input.is_empty() {
                        let text = app.take_input();
                        match text.trim() {
                            "/quit" | "/exit" => app.should_quit = true,
                            "/new" => {
                                app.messages.clear();
                                app.commands.clear();
                                app.status_text = "New session...".into();
                                let _ = req_tx.try_send(Request::NewSession);
                            }
                            _ if text.starts_with("/search ") => {
                                let query = text.trim_start_matches("/search ").to_string();
                                app.push_message(
                                    DisplayRole::System,
                                    format!("Searching: {}...", query),
                                );
                                app.status_text = "Searching sessions...".into();
                                let _ = req_tx.try_send(Request::SearchSessions { query });
                            }
                            _ => {
                                // Show user message immediately in chat
                                app.push_message(DisplayRole::User, text.clone());
                                app.status_text = "Thinking...".into();
                                app.streaming = true;
                                let _ = req_tx.try_send(Request::UserMessage { content: text });
                            }
                        }
                    }
                }
                InputAction::Quit => app.should_quit = true,
                InputAction::Char(c) => app.insert_char(c),
                InputAction::Backspace => app.delete_char(),
                InputAction::Delete => {
                    // Delete char at cursor (forward delete)
                    if app.cursor_pos < app.input.len() {
                        app.input.remove(app.cursor_pos);
                    }
                }
                InputAction::Left => app.move_cursor_left(),
                InputAction::Right => app.move_cursor_right(),
                InputAction::Home => app.move_cursor_home(),
                InputAction::End => app.move_cursor_end(),
                InputAction::HistoryPrev => {
                    // In chat focus mode, up/down scroll; otherwise history
                    if app.focus == app::Focus::Chat {
                        app.scroll_up();
                    } else {
                        app.history_prev();
                    }
                }
                InputAction::HistoryNext => {
                    if app.focus == app::Focus::Chat {
                        app.scroll_down();
                    } else {
                        app.history_next();
                    }
                }
                InputAction::ScrollUp => app.scroll_up(),
                InputAction::ScrollDown => app.scroll_down(),
                InputAction::PageUp => {
                    for _ in 0..10 {
                        app.scroll_up();
                    }
                }
                InputAction::PageDown => {
                    for _ in 0..10 {
                        app.scroll_down();
                    }
                }
                InputAction::ToggleFocus => app.toggle_focus(),
                InputAction::Paste(text) => app.insert_text(&text),
                InputAction::None => {}
            }
        }

        if app.should_quit {
            break;
        }

        terminal.draw(|f| ui::render(f, &mut app))?;

        // 16ms ≈ 60fps — fast enough for smooth streaming display
        tokio::time::sleep(std::time::Duration::from_millis(16)).await;
    }

    Ok(())
}

fn handle_ipc_event(app: &mut App, event: Event) {
    match event {
        Event::TextDelta { content } => {
            // Append directly — no buffering, no typewriter effect.
            // This is how both Claude Code and OpenClaw handle streaming:
            // render the text as soon as it arrives.
            app.append_assistant_text(&content);
            app.streaming = true;
        }
        Event::ToolCallStart { name, id: _ } => {
            app.push_command(name, String::new());
        }
        Event::ToolCallResult { content, .. } => {
            app.set_last_command_result(content);
        }
        Event::TurnEnd => {
            app.status_text = "Ready".into();
            app.streaming = false;
        }
        Event::TokenUsage {
            input,
            output,
            budget_pct,
        } => {
            app.token_input = input;
            app.token_output = output;
            app.budget_pct = budget_pct;
        }
        Event::Error { message } => {
            app.push_message(DisplayRole::System, format!("Error: {}", message));
            app.streaming = false;
        }
        Event::SessionRestored {
            session_id,
            message_count,
        } => {
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
        Event::Heartbeat { task_name, message } => {
            app.status_text = format!("[Heartbeat] {}: {}", task_name, message);
        }
        Event::SearchResults { results } => {
            if results.is_empty() {
                app.push_message(DisplayRole::System, "No matching sessions found.".into());
            } else {
                let mut text = format!("Found {} sessions:\n", results.len());
                for (i, r) in results.iter().enumerate() {
                    let sid_short: String = r.session_id.chars().take(8).collect();
                    text.push_str(&format!(
                        "  {}. [{}] {} ({} msgs)\n",
                        i + 1,
                        sid_short,
                        r.title,
                        r.message_count
                    ));
                }
                app.push_message(DisplayRole::System, text);
            }
            app.status_text = "Ready".into();
        }
        // [V4 Fix] Handle ProjectCompleted notification from Coordinator
        Event::ProjectCompleted { project_id, report } => {
            app.push_message(DisplayRole::System, format!("✅ Project Completed (ID: {})\n\n{}", project_id, report));
            app.status_text = "Project completed".into();
        }
    }
}
