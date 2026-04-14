use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, DisplayRole, Focus};
use crate::theme::Theme;

pub fn render(f: &mut Frame, app: &mut App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // status bar (with border)
            Constraint::Min(5),    // main area
            Constraint::Length(3), // input box
        ])
        .split(f.area());

    render_status_bar(f, app, rows[0]);

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
            " [●] NOVA v1.0 │ {} │ {}↑ {}↓ │ {} {:.0}%  {}",
            sid_short, app.token_input, app.token_output,
            bar, app.budget_pct * 100.0, app.status_text,
        )
    } else {
        format!(" [○] NOVA v1.0 │ {}", app.status_text)
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

// ── Chat Panel ──────────────────────────────────────────
//
// The key insight: ratatui's Paragraph with Wrap does its own internal
// word-wrapping that we CANNOT accurately predict from the outside.
// Our previous approach of estimating wrapped line counts and using
// Paragraph::scroll() was fundamentally broken — the estimate was always
// off, causing content to be cut off at the bottom.
//
// New approach: we do our OWN wrapping into physical screen lines,
// then display them WITHOUT Paragraph::scroll or Wrap. This gives us
// 100% accurate control over what's visible.

fn render_chat_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let visible_height = area.height.saturating_sub(2) as usize;

    if inner_width == 0 || visible_height == 0 {
        let block = chat_block(app, false);
        f.render_widget(block, area);
        return;
    }

    // Step 1: Build all physical (wrapped) lines ourselves
    let mut physical_lines: Vec<PhysicalLine> = Vec::new();

    for msg in &app.messages {
        let (prefix, color) = match &msg.role {
            DisplayRole::User => ("▶ You", Theme::USER_MSG),
            DisplayRole::Assistant => ("◆ AI", Theme::ASSISTANT_MSG),
            DisplayRole::System => ("◇ System", Theme::DIM),
        };

        physical_lines.push(PhysicalLine::styled(prefix, color, true));

        if msg.content.is_empty() {
            if matches!(msg.role, DisplayRole::Assistant) {
                physical_lines.push(PhysicalLine::styled("  ▍", Theme::ASSISTANT_MSG, false));
            }
        } else {
            for line in msg.content.lines() {
                let indented = format!("  {}", line);
                // Wrap this line manually into inner_width chunks
                wrap_line_into(&indented, inner_width, Theme::TEXT, &mut physical_lines);
            }
        }

        // Blank separator between messages
        physical_lines.push(PhysicalLine::empty());
    }

    // Streaming cursor
    if app.streaming {
        if let Some(last) = physical_lines.last_mut() {
            if last.text.is_empty() {
                *last = PhysicalLine::styled("  ▍", Theme::ASSISTANT_MSG, false);
            }
        }
    }

    let total = physical_lines.len();
    app.chat_total_lines = total;

    // Step 2: Slice the visible window
    // scroll_offset=0 means "show the bottom", higher means scrolled up
    let end = total.saturating_sub(app.scroll_offset);
    let start = end.saturating_sub(visible_height);

    // Clamp scroll_offset so user can't scroll past the top
    if app.scroll_offset > total.saturating_sub(visible_height) {
        app.scroll_offset = total.saturating_sub(visible_height);
    }

    let visible_slice = &physical_lines[start..end];

    // Step 3: Convert to ratatui Lines (no wrapping needed — already wrapped)
    let display_lines: Vec<Line> = visible_slice
        .iter()
        .map(|pl| pl.to_line())
        .collect();

    let focused = app.focus == Focus::Chat;
    let at_bottom = app.scroll_offset == 0;

    let title = if !at_bottom {
        let lines_above = start;
        let lines_below = total.saturating_sub(end);
        format!(" ◈ NOVA [↑{} ↓{}] ", lines_above, lines_below)
    } else {
        " ◈ NOVA ".to_string()
    };

    let border_color = if focused { Theme::PRIMARY } else { Theme::BORDER };
    let block = Block::default()
        .title(Span::styled(title, Style::default().fg(Theme::ACCENT).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(Theme::BG));

    // NO Wrap, NO scroll — we already did both manually
    let paragraph = Paragraph::new(display_lines).block(block);
    f.render_widget(paragraph, area);
}

fn chat_block(_app: &App, _focused: bool) -> Block<'static> {
    Block::default()
        .title(Span::styled(" ◈ Chat ", Style::default().fg(Theme::ACCENT).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::BORDER))
        .style(Style::default().bg(Theme::BG))
}

// ── Commands Panel ──────────────────────────────────────

fn render_commands_panel(f: &mut Frame, app: &App, area: Rect) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let visible_height = area.height.saturating_sub(2) as usize;

    let mut physical_lines: Vec<PhysicalLine> = Vec::new();

    for cmd in &app.commands {
        let header = if cmd.args_summary.is_empty() {
            format!("$ {}", cmd.name)
        } else {
            format!("$ {}: {}", cmd.name, cmd.args_summary)
        };
        physical_lines.push(PhysicalLine::styled(&header, Theme::TOOL_MSG, true));

        if let Some(ref result) = cmd.result {
            let max_lines = 12;
            let result_lines: Vec<&str> = result.lines().collect();
            let total = result_lines.len();
            for line in result_lines.iter().take(max_lines) {
                let indented = format!("  {}", line);
                wrap_line_into(&indented, inner_width.max(1), Theme::DIM, &mut physical_lines);
            }
            if total > max_lines {
                physical_lines.push(PhysicalLine::styled(
                    &format!("  ... ({} more lines)", total - max_lines),
                    Theme::DIM, false,
                ));
            }
        } else {
            physical_lines.push(PhysicalLine::styled("  ⏳ running...", Theme::DIM, false));
        }
        physical_lines.push(PhysicalLine::empty());
    }

    if physical_lines.is_empty() {
        physical_lines.push(PhysicalLine::styled("  No commands yet", Theme::DIM, false));
    }

    let total = physical_lines.len();
    let end = total.saturating_sub(app.cmd_scroll_offset);
    let start = end.saturating_sub(visible_height);

    let visible_slice = &physical_lines[start..end];
    let display_lines: Vec<Line> = visible_slice.iter().map(|pl| pl.to_line()).collect();

    let focused = app.focus == Focus::Commands;
    let border_color = if focused { Theme::PRIMARY } else { Theme::BORDER };

    let block = Block::default()
        .title(Span::styled(" ⚙ Commands ", Style::default().fg(Theme::TOOL_MSG).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .style(Style::default().bg(Theme::BG));

    let paragraph = Paragraph::new(display_lines).block(block);
    f.render_widget(paragraph, area);
}

// ── Input Box ───────────────────────────────────────────

fn render_input_box(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(Span::styled(" ⌨ Input ", Style::default().fg(Theme::SECONDARY)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Theme::BORDER))
        .style(Style::default().bg(Theme::BG));

    let input_text = if app.input.is_empty() {
        Span::styled(
            "Type a message... (/new /quit) [Tab: panel] [Shift+↑↓: scroll]",
            Style::default().fg(Theme::DIM),
        )
    } else {
        Span::styled(&app.input, Style::default().fg(Theme::TEXT))
    };

    let paragraph = Paragraph::new(Line::from(input_text)).block(block);
    f.render_widget(paragraph, area);

    let display_pos = UnicodeWidthStr::width(&app.input[..app.cursor_pos]);
    let max_x = area.x.saturating_add(area.width).saturating_sub(2);
    let cursor_x = (area.x + 1 + display_pos as u16).min(max_x);
    let cursor_y = area.y + 1;
    f.set_cursor_position((cursor_x, cursor_y));
}

// ── Physical Line: a single screen row after wrapping ───

use ratatui::style::Color;

struct PhysicalLine {
    text: String,
    color: Color,
    bold: bool,
}

impl PhysicalLine {
    fn styled(text: &str, color: Color, bold: bool) -> Self {
        Self { text: text.to_string(), color, bold }
    }

    fn empty() -> Self {
        Self { text: String::new(), color: Theme::TEXT, bold: false }
    }

    fn to_line(&self) -> Line<'static> {
        let mut style = Style::default().fg(self.color);
        if self.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        Line::from(Span::styled(self.text.clone(), style))
    }
}

/// Wrap a single logical line into multiple physical lines that fit within
/// `max_width` display columns. This handles CJK characters correctly by
/// using unicode display width.
fn wrap_line_into(text: &str, max_width: usize, color: Color, out: &mut Vec<PhysicalLine>) {
    if max_width == 0 {
        out.push(PhysicalLine::styled(text, color, false));
        return;
    }

    let text_width = UnicodeWidthStr::width(text);
    if text_width <= max_width {
        out.push(PhysicalLine::styled(text, color, false));
        return;
    }

    // Need to wrap: walk char by char, tracking display width
    let mut current_line = String::new();
    let mut current_width: usize = 0;

    for ch in text.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);

        if current_width + ch_width > max_width && !current_line.is_empty() {
            // Emit current line, start new one
            out.push(PhysicalLine::styled(&current_line, color, false));
            current_line.clear();
            current_width = 0;
        }

        current_line.push(ch);
        current_width += ch_width;
    }

    if !current_line.is_empty() {
        out.push(PhysicalLine::styled(&current_line, color, false));
    }
}
