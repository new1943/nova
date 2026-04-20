/// I/O Shield truncation constants for bash output
pub const MAX_BASH_OUTPUT_CHARS: usize = 20_000;
pub const BASH_HEAD_CHARS: usize = 8_000;
pub const BASH_TAIL_CHARS: usize = 8_000;

/// I/O Shield truncation constants for browser output
pub const MAX_BROWSER_OUTPUT_CHARS: usize = 15_000;
pub const BROWSER_HEAD_CHARS: usize = 5_000;
pub const BROWSER_TAIL_CHARS: usize = 5_000;

/// Warning text template for bash truncation
pub const BASH_TRUNCATION_WARNING: &str =
    "[... 约 {n} 字符因过长已省略。如需查看完整输出，请使用 grep 搜索指定行号，或用 read_file 的 start_line/end_line 参数读取特定范围。]";

/// Warning text template for browser truncation
pub const BROWSER_TRUNCATION_WARNING: &str =
    "[... 页面内容因过长已省略。如需查看特定区域，请使用 click 点击目标元素，或用 navigate 直接访问相关 URL。]";
