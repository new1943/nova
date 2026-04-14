/// Which panel has scroll focus
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Chat,
    Commands,
}

/// Role for display in the chat panel
#[derive(Debug, Clone)]
pub enum DisplayRole {
    User,
    Assistant,
    System,
}

/// A message for display in the chat panel
#[derive(Debug, Clone)]
pub struct DisplayMessage {
    pub role: DisplayRole,
    pub content: String,
}

/// A tool invocation for display in the commands panel
#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub name: String,
    pub args_summary: String,
    pub result: Option<String>,
    pub collapsed: bool,
}

/// TUI application state
pub struct App {
    // Chat panel
    pub messages: Vec<DisplayMessage>,
    pub scroll_offset: usize,
    /// Cached total wrapped line count for the chat panel (updated on render)
    pub chat_total_lines: usize,

    // Commands panel
    pub commands: Vec<CommandEntry>,
    pub cmd_scroll_offset: usize,

    // Focus
    pub focus: Focus,

    // Input
    pub input: String,
    pub cursor_pos: usize,
    /// Input history for up/down navigation
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,

    // Status
    pub token_input: u32,
    pub token_output: u32,
    pub budget_pct: f32,
    pub connected: bool,
    pub session_id: Option<String>,
    pub should_quit: bool,
    pub status_text: String,

    /// Whether we are currently streaming (assistant is generating)
    pub streaming: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            scroll_offset: 0,
            chat_total_lines: 0,
            commands: Vec::new(),
            cmd_scroll_offset: 0,
            focus: Focus::Chat,
            input: String::new(),
            cursor_pos: 0,
            input_history: Vec::new(),
            history_index: None,
            token_input: 0,
            token_output: 0,
            budget_pct: 0.0,
            connected: false,
            session_id: None,
            should_quit: false,
            status_text: "Connecting...".into(),
            streaming: false,
        }
    }

    pub fn push_message(&mut self, role: DisplayRole, content: String) {
        self.messages.push(DisplayMessage { role, content });
        // Auto-scroll to bottom when new message arrives
        self.scroll_offset = 0;
    }

    /// Append text to the last assistant message, or create one.
    /// This is the streaming path — text arrives incrementally.
    pub fn append_assistant_text(&mut self, text: &str) {
        if let Some(last) = self.messages.last_mut() {
            if matches!(last.role, DisplayRole::Assistant) {
                last.content.push_str(text);
                // Keep scrolled to bottom during streaming
                self.scroll_offset = 0;
                return;
            }
        }
        self.push_message(DisplayRole::Assistant, text.to_string());
    }

    /// Push a new tool call into the commands panel
    pub fn push_command(&mut self, name: String, args_summary: String) {
        self.commands.push(CommandEntry {
            name,
            args_summary,
            result: None,
            collapsed: false,
        });
        self.cmd_scroll_offset = 0; // auto-scroll
    }

    /// Set the result on the last command
    pub fn set_last_command_result(&mut self, content: String) {
        if let Some(last) = self.commands.last_mut() {
            last.result = Some(content);
        }
        self.cmd_scroll_offset = 0;
    }

    pub fn take_input(&mut self) -> String {
        let input = self.input.clone();
        // Save to history
        if !input.trim().is_empty() {
            self.input_history.push(input.clone());
        }
        self.input.clear();
        self.cursor_pos = 0;
        self.history_index = None;
        input
    }

    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    pub fn delete_char(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.input[..self.cursor_pos]
                .chars()
                .last()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            self.cursor_pos -= prev;
            self.input.remove(self.cursor_pos);
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.input[..self.cursor_pos]
                .chars()
                .last()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            self.cursor_pos -= prev;
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor_pos < self.input.len() {
            let next = self.input[self.cursor_pos..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            self.cursor_pos += next;
        }
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor_pos = self.input.len();
    }

    /// Navigate input history (up arrow)
    pub fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let idx = match self.history_index {
            None => self.input_history.len().saturating_sub(1),
            Some(i) => i.saturating_sub(1),
        };
        self.history_index = Some(idx);
        self.input = self.input_history[idx].clone();
        self.cursor_pos = self.input.len();
    }

    /// Navigate input history (down arrow)
    pub fn history_next(&mut self) {
        match self.history_index {
            None => {}
            Some(i) => {
                if i + 1 < self.input_history.len() {
                    let idx = i + 1;
                    self.history_index = Some(idx);
                    self.input = self.input_history[idx].clone();
                    self.cursor_pos = self.input.len();
                } else {
                    // Past the end — clear input
                    self.history_index = None;
                    self.input.clear();
                    self.cursor_pos = 0;
                }
            }
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Chat => Focus::Commands,
            Focus::Commands => Focus::Chat,
        };
    }

    pub fn scroll_up(&mut self) {
        match self.focus {
            Focus::Chat => self.scroll_offset += 3,
            Focus::Commands => self.cmd_scroll_offset += 3,
        }
    }

    pub fn scroll_down(&mut self) {
        match self.focus {
            Focus::Chat => self.scroll_offset = self.scroll_offset.saturating_sub(3),
            Focus::Commands => self.cmd_scroll_offset = self.cmd_scroll_offset.saturating_sub(3),
        }
    }
}
