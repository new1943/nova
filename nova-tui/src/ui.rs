use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, DisplayRole, Focus};
use crate::theme::Theme;

/// Render the full TUI layout:
/// ┌──────────── Status Bar ─────────────┐
/// ├──── Chat (2/3) ──┬── Commands (1/3) ┤
/// ├──────────── Input (full) ───────────┤
/// └─────────────────────────────────────┘
pub fn render(f: &mut Frame, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // status bar
            Constraint::Min(5),    // main area (chat + commands)
            Constraint::Length(3), // input box
        ])
        .split(f.area());

    render_status_bar(f, app, rows[0]);

    // Split main area: left 2/3 chat, right 1/3 commands
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(66),
            Constraint::Percentage(34),
        ])
        .split(rows[1]);

    render_chat_panel(f, app, cols[0]);
    render_commands_panel(f, app, cols[1]);
    render_input_box(f, app, rows[2]);
}

// ── Status Bar ──────────────────────────────────────────

fn render_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let text = if app.connected {
        let sid = app.session_id.as_deref().unwrap_or("—");
        let sid_short: String = sid.chars().take(8).collect();
        let bar = budget_bar(app.budget_pct, 10);
        format!(
            " [●] KIKO v0.1 │ {} │ {}↑ {}↓ │ {} {:.0}%  {}",
            sid_short,
            app.token_input,
            app.token_output,
            bar,
            app.budget_pct * 100.0,
            app.status_text,
        )
    } else {
        format!(" [○] KIKO v0.1 │ {}", app.status_text)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::BORDER))
        .style(Style::default().bg(Theme::STATUS_BG));

    let paragraph = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().fg(Theme::PRIMARY).add_modifier(Modifier::BOLD),
    )))
    .block(block);

    f.render_widget(paragraph, area);
}

fn budget_bar(pct: f32, width: usize) -> String {
    let filled = ((pct * width as f32).round() as usize).min(width);
    let empty = width.saturating_sub(filled);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

// ── Chat Panel (left 2/3) ───────────────────────────────

fn render_chat_panel(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in &app.messages {
        let (prefix, color) = match &msg.role {
            DisplayRole::User => ("▶ You", Theme::USER_MSG),
            DisplayRole::Assistant => ("◆ Kiko", Theme::ASSISTANT_MSG),
            DisplayRole::System => ("◇ System", Theme::DIM),
        };

        lines.push(Line::from(Span::styled(
            prefix,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )));

        for line in msg.content.lines() {
            lines.push(Line::from(Span::styled(
                format!("  {}", line),
                Style::default().fg(Theme::TEXT),
            )));
        }

        lines.push(Line::from(""));
    }

    let scroll = calc_scroll(&lines, area, app.scroll_offset);

    let focused = app.focus == Focus::Chat;
    let border_color = if focused { Theme::PRIMARY } else { Theme::BORDER };

    let block = Block::default()
        .title(Span::styled(
            " ◈ NOVA ",
            Style::default().fg(Theme::ACCENT).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(Theme::BG));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    f.render_widget(paragraph, area);
}

// ── Commands Panel (right 1/3) ──────────────────────────

fn render_commands_panel(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for cmd in &app.commands {
        // Header: $ tool_name [args_summary]
        let header = if cmd.args_summary.is_empty() {
            format!("$ {}", cmd.name)
        } else {
            format!("$ {}: {}", cmd.name, cmd.args_summary)
        };
        lines.push(Line::from(Span::styled(
            header,
            Style::default().fg(Theme::TOOL_MSG).add_modifier(Modifier::BOLD),
        )));

        // Result lines (truncated)
        if let Some(ref result) = cmd.result {
            let max_lines = 12;
            let result_lines: Vec<&str> = result.lines().collect();
            let total = result_lines.len();
            let show = result_lines.iter().take(max_lines);
            for line in show {
                lines.push(Line::from(Span::styled(
                    format!("  {}", line),
                    Style::default().fg(Theme::DIM),
                )));
            }
            if total > max_lines {
                lines.push(Line::from(Span::styled(
                    format!("  ... ({} more lines)", total - max_lines),
                    Style::default().fg(Theme::DIM),
                )));
            }
        } else {
            lines.push(Line::from(Span::styled(
                "  ⏳ running...",
                Style::default().fg(Theme::DIM),
            )));
        }

        lines.push(Line::from(""));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No commands yet",
            Style::default().fg(Theme::DIM),
        )));
    }

    let scroll = calc_scroll(&lines, area, app.cmd_scroll_offset);

    let focused = app.focus == Focus::Commands;
    let border_color = if focused { Theme::PRIMARY } else { Theme::BORDER };

    let block = Block::default()
        .title(Span::styled(
            " ⚙ Commands ",
            Style::default().fg(Theme::TOOL_MSG).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(Theme::BG));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    f.render_widget(paragraph, area);
}

// ── Input Box (full width) ──────────────────────────────

fn render_input_box(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(Span::styled(
            " ⌨ Input ",
            Style::default().fg(Theme::SECONDARY),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::BORDER))
        .style(Style::default().bg(Theme::BG));

    let input_text = if app.input.is_empty() {
        Span::styled("Type a message... (/new /quit)", Style::default().fg(Theme::DIM))
    } else {
        Span::styled(&app.input, Style::default().fg(Theme::TEXT))
    };

    let paragraph = Paragraph::new(Line::from(input_text)).block(block);
    f.render_widget(paragraph, area);

    // Cursor — clamp to input box bounds
    let display_pos = UnicodeWidthStr::width(&app.input[..app.cursor_pos]);
    let max_x = area.x.saturating_add(area.width).saturating_sub(2);
    let cursor_x = (area.x + 1 + display_pos as u16).min(max_x);
    let cursor_y = area.y + 1;
    f.set_cursor_position((cursor_x, cursor_y));
}

// ── Helpers ─────────────────────────────────────────────

/// Calculate scroll offset for a paragraph with wrap, auto-scrolling to bottom.
fn calc_scroll(lines: &[Line], area: Rect, manual_offset: usize) -> u16 {
    let inner_width = area.width.saturating_sub(2) as usize;
    let visible_height = area.height.saturating_sub(2) as usize;
    if inner_width == 0 || visible_height == 0 {
        return 0;
    }
    // Estimate wrapped line count.
    // Use unicode display width for accurate CJK handling.
    let total: usize = lines.iter().map(|l| {
        let w = l.width();
        if w == 0 { 1 } else { (w + inner_width - 1) / inner_width }
    }).sum();

    if total <= visible_height {
        return 0; // Everything fits, no scroll needed
    }

    let max_scroll = total.saturating_sub(visible_height);
    max_scroll.saturating_sub(manual_offset).min(u16::MAX as usize) as u16
}
