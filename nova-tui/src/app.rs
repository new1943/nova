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
}

/// TUI application state
pub struct App {
    // Chat panel
    pub messages: Vec<DisplayMessage>,
    pub scroll_offset: usize,

    // Commands panel
    pub commands: Vec<CommandEntry>,
    pub cmd_scroll_offset: usize,

    // Focus
    pub focus: Focus,

    // Input
    pub input: String,
    pub cursor_pos: usize,

    // Status
    pub token_input: u32,
    pub token_output: u32,
    pub budget_pct: f32,
    pub connected: bool,
    pub session_id: Option<String>,
    pub should_quit: bool,
    pub status_text: String,
}

impl App {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            scroll_offset: 0,
            commands: Vec::new(),
            cmd_scroll_offset: 0,
            focus: Focus::Chat,
            input: String::new(),
            cursor_pos: 0,
            token_input: 0,
            token_output: 0,
            budget_pct: 0.0,
            connected: false,
            session_id: None,
            should_quit: false,
            status_text: "Connecting...".into(),
        }
    }

    pub fn push_message(&mut self, role: DisplayRole, content: String) {
        self.messages.push(DisplayMessage { role, content });
        self.scroll_offset = 0;
    }

    /// Append text to the last assistant message, or create one
    pub fn append_assistant_text(&mut self, text: &str) {
        if let Some(last) = self.messages.last_mut() {
            if matches!(last.role, DisplayRole::Assistant) {
                last.content.push_str(text);
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
        });
        self.cmd_scroll_offset = 0; // auto-scroll
    }

    /// Set the result on the last command with matching id-like name
    pub fn set_last_command_result(&mut self, content: String) {
        if let Some(last) = self.commands.last_mut() {
            last.result = Some(content);
        }
        self.cmd_scroll_offset = 0;
    }

    pub fn take_input(&mut self) -> String {
        let input = self.input.clone();
        self.input.clear();
        self.cursor_pos = 0;
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
