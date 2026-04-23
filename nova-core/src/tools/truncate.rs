use crate::tools::constants::{
    BASH_TRUNCATION_WARNING, BASH_HEAD_CHARS, BASH_TAIL_CHARS, MAX_BASH_OUTPUT_CHARS,
    BROWSER_TRUNCATION_WARNING, BROWSER_HEAD_CHARS, BROWSER_TAIL_CHARS, MAX_BROWSER_OUTPUT_CHARS,
};

/// Truncate a string to max_chars by keeping head and tail, inserting a warning in the middle.
/// If output.len() <= max_chars, returns the output unchanged.
pub fn truncate_output(
    output: &str,
    max_chars: usize,
    head_chars: usize,
    tail_chars: usize,
    warning: &str,
) -> String {
    if output.len() <= max_chars {
        return output.to_string();
    }

    let mut head_idx = head_chars.min(output.len());
    while head_idx > 0 && !output.is_char_boundary(head_idx) {
        head_idx -= 1;
    }
    let head = &output[..head_idx];

    let mut tail_start = output.len().saturating_sub(tail_chars);
    while tail_start < output.len() && !output.is_char_boundary(tail_start) {
        tail_start += 1; // Move forward to start at a valid char
    }

    let tail = if tail_start > head_idx {
        &output[tail_start..]
    } else {
        let mut max_idx = max_chars.min(output.len());
        while max_idx > 0 && !output.is_char_boundary(max_idx) {
            max_idx -= 1;
        }
        return output[..max_idx].to_string();
    };

    let omitted = output.len() - head_idx - (output.len() - tail_start);
    let warning = warning.replace("{n}", &omitted.to_string());

    format!("{}{}{}", head, warning, tail)
}

/// Truncate bash output to MAX_BASH_OUTPUT_CHARS, preserving head and tail.
pub fn truncate_bash(output: &str) -> String {
    truncate_output(
        output,
        MAX_BASH_OUTPUT_CHARS,
        BASH_HEAD_CHARS,
        BASH_TAIL_CHARS,
        BASH_TRUNCATION_WARNING,
    )
}

/// Truncate browser output to MAX_BROWSER_OUTPUT_CHARS, preserving head and tail.
pub fn truncate_browser(output: &str) -> String {
    truncate_output(
        output,
        MAX_BROWSER_OUTPUT_CHARS,
        BROWSER_HEAD_CHARS,
        BROWSER_TAIL_CHARS,
        BROWSER_TRUNCATION_WARNING,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short_output_passes_through() {
        let input = "short output";
        let result = truncate_output(input, 20, 8, 8, "[...]");
        assert_eq!(result, "short output");
    }

    #[test]
    fn test_truncate_exact_boundary() {
        let input = "12345";
        let result = truncate_output(input, 5, 2, 2, "[...]");
        assert_eq!(result, "12345");
    }

    #[test]
    fn test_truncate_long_output_is_truncated() {
        let input = "a".repeat(100);
        let result = truncate_output(&input, 20, 5, 5, "[..{n}..]");
        assert!(result.len() < 100);
        assert!(result.starts_with("aaaaa"));
        assert!(result.ends_with("aaaaa"));
        // 100 - 5 - 5 = 90 omitted
        assert!(result.contains("..90.."));
    }

    #[test]
    fn test_truncate_warning_replaces_n() {
        let input = "x".repeat(100);
        let result = truncate_output(&input, 20, 5, 5, "[..{n}..]");
        // 100 - 5 - 5 = 90 omitted
        assert!(result.contains("..90.."));
    }

    #[test]
    fn test_truncate_overlapping_head_tail() {
        // When head + tail > max_chars, should just return first max_chars
        let input = "abcd";
        let result = truncate_output(input, 3, 3, 3, "[...]");
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_truncate_bash_wrapper() {
        let input = "x".repeat(30000);
        let result = truncate_bash(&input);
        assert!(result.len() < 30000);
        // 30000 - 8000 - 8000 = 14000 omitted
        assert!(result.contains("14000"));
    }

    #[test]
    fn test_truncate_browser_wrapper() {
        let input = "x".repeat(20000);
        let result = truncate_browser(&input);
        assert!(result.len() < 20000);
        // Browser warning text mentions truncation
        assert!(result.contains("页面内容因过长已省略"));
    }
}
