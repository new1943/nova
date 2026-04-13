use ratatui::style::Color;

/// Cyberpunk color theme
#[allow(dead_code)]
pub struct Theme;

#[allow(dead_code)]
impl Theme {
    pub const BG: Color = Color::Rgb(10, 10, 25);
    pub const PRIMARY: Color = Color::Rgb(0, 255, 255);      // Cyan
    pub const SECONDARY: Color = Color::Rgb(180, 0, 255);    // Purple
    pub const ACCENT: Color = Color::Rgb(255, 0, 128);       // Magenta/Pink
    pub const TEXT: Color = Color::Rgb(200, 200, 220);
    pub const DIM: Color = Color::Rgb(80, 80, 100);
    pub const USER_MSG: Color = Color::Rgb(0, 200, 200);
    pub const ASSISTANT_MSG: Color = Color::Rgb(180, 140, 255);
    pub const TOOL_MSG: Color = Color::Rgb(255, 180, 0);
    pub const ERROR: Color = Color::Rgb(255, 60, 60);
    pub const BORDER: Color = Color::Rgb(60, 0, 120);
    pub const STATUS_BG: Color = Color::Rgb(20, 0, 40);
}
