//! Fuzzy matching engine for skill content patching.
//!
//! Implements 9 strategies in priority order for robust find-and-replace
//! that handles whitespace, indentation, and Unicode variations.

use anyhow::{Result, anyhow};
use strsim::normalized_damerau_levenshtein;

/// Fuzzy matching result
#[derive(Debug)]
pub struct FuzzyMatch {
    pub content: String,
    pub match_count: usize,
    pub strategy: &'static str,
}

/// Find and replace using fuzzy matching strategies
///
/// Returns (modified_content, match_count, strategy_used)
pub fn fuzzy_find_and_replace(
    original: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<(String, usize, &'static str)> {
    if old_string.is_empty() {
        anyhow::bail!("old_string cannot be empty");
    }

    if old_string == new_string {
        return Ok((original.to_string(), 0, "identical"));
    }

    // Try strategies in order
    let strategies: Vec<(&str, fn(&str, &str) -> Option<Vec<(usize, usize)>>)> = vec![
        ("exact", strategy_exact),
        ("line_trimmed", strategy_line_trimmed),
        ("whitespace_normalized", strategy_whitespace_normalized),
        ("indentation_flexible", strategy_indentation_flexible),
        ("escape_normalized", strategy_escape_normalized),
        ("trimmed_boundary", strategy_trimmed_boundary),
        ("unicode_normalized", strategy_unicode_normalized),
        ("block_anchor", strategy_block_anchor),
    ];

    for (name, strategy) in strategies {
        if let Some(matches) = strategy(original, old_string) {
            if matches.is_empty() {
                continue;
            }

            // Check uniqueness if not replace_all
            if matches.len() > 1 && !replace_all {
                return Err(anyhow!(
                    "Found {} matches. Provide more context to make it unique, or use replace_all=true.",
                    matches.len()
                ));
            }

            let match_count = matches.len();
            let modified = apply_replacements(original, &matches, new_string);
            return Ok((modified, match_count, name));
        }
    }

    Err(anyhow!(
        "Could not find '{}' in content. Check for typos or try providing more context.",
        truncate(old_string, 50)
    ))
}

/// Apply replacements at given positions (from end to start to preserve positions)
fn apply_replacements(original: &str, matches: &[(usize, usize)], new_string: &str) -> String {
    let mut result = original.to_string();

    // Sort by position descending
    let mut sorted_matches: Vec<_> = matches.iter().collect();
    sorted_matches.sort_by(|a, b| b.0.cmp(&a.0));

    for &(start, end) in sorted_matches {
        result = format!("{}{}{}", &result[..start], new_string, &result[end..]);
    }

    result
}

/// Truncate string for display
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

/// Calculate line positions for character index calculation
fn get_line_offsets(content: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (i, c) in content.char_indices() {
        if c == '\n' {
            offsets.push(i + 1);
        }
    }
    offsets
}

/// Convert line+col to character position
fn position_to_offset(line_offsets: &[usize], line: usize, _col: usize) -> usize {
    if line >= line_offsets.len() {
        line_offsets.last().copied().unwrap_or(0)
    } else {
        line_offsets[line]
    }
}

/// Strategy 1: Exact match
fn strategy_exact(content: &str, pattern: &str) -> Option<Vec<(usize, usize)>> {
    let mut matches = Vec::new();
    let mut start = 0;

    while let Some(pos) = content[start..].find(pattern) {
        let abs_pos = start + pos;
        matches.push((abs_pos, abs_pos + pattern.len()));
        start = abs_pos + 1;
        if matches.len() > 1000 {
            break; // Safety limit
        }
    }

    if matches.is_empty() {
        None
    } else {
        Some(matches)
    }
}

/// Strategy 2: Line-trimmed match
fn strategy_line_trimmed(content: &str, pattern: &str) -> Option<Vec<(usize, usize)>> {
    let pattern_lines: Vec<&str> = pattern.lines().collect();
    let pattern_normalized: String = pattern_lines
        .iter()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n");

    let content_lines: Vec<&str> = content.lines().collect();
    let content_normalized: Vec<&str> = content_lines.iter().map(|l| l.trim()).collect();

    let n_pattern = pattern_normalized.lines().count();
    if n_pattern == 0 || content_normalized.len() < n_pattern {
        return None;
    }

    let normalized_join: String = content_normalized.join("\n");
    let mut matches = Vec::new();
    let mut search_start = 0;

    while let Some(pos) = normalized_join[search_start..].find(&pattern_normalized) {
        let abs_pos = search_start + pos;
        // Find corresponding position in original
        let line_start = normalized_join[..abs_pos].matches('\n').count();
        let char_start = position_to_offset(&get_line_offsets(content), line_start, 0);

        // Find line end for this match
        let match_len = pattern_normalized.len();
        let normalized_before = &normalized_join[..abs_pos + match_len];
        let line_end = normalized_before.matches('\n').count();
        let char_end = if line_end < content_lines.len() {
            let offset = get_line_offsets(content);
            if line_end + 1 < offset.len() {
                offset[line_end + 1]
            } else {
                content.len()
            }
        } else {
            content.len()
        };

        matches.push((char_start, char_end));
        search_start = abs_pos + 1;

        if matches.len() > 1000 {
            break;
        }
    }

    if matches.is_empty() {
        None
    } else {
        Some(matches)
    }
}

/// Strategy 3: Whitespace normalized (collapse multiple spaces/tabs)
fn strategy_whitespace_normalized(content: &str, pattern: &str) -> Option<Vec<(usize, usize)>> {
    let normalized_content = collapse_whitespace(content);
    let normalized_pattern = collapse_whitespace(pattern);

    let matches = strategy_exact(&normalized_content, &normalized_pattern)?;
    let line_offsets = get_line_offsets(content);

    // Map normalized positions back to original (approximate)
    let result: Vec<(usize, usize)> = matches
        .iter()
        .map(|(start, end)| {
            let orig_start = map_normalized_to_original(content, *start);
            let orig_end = map_normalized_to_original(content, *end);
            (orig_start, orig_end)
        })
        .collect();

    Some(result)
}

/// Collapse multiple whitespace to single space
fn collapse_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_was_space = false;

    for c in s.chars() {
        if c == ' ' || c == '\t' {
            if !prev_was_space {
                result.push(' ');
                prev_was_space = true;
            }
        } else {
            result.push(c);
            prev_was_space = false;
        }
    }
    result
}

/// Map normalized position back to original (best effort)
fn map_normalized_to_original(content: &str, norm_pos: usize) -> usize {
    let mut orig_pos = 0;
    let mut norm_count = 0;
    let mut prev_was_space = false;

    for c in content.chars() {
        if norm_count >= norm_pos {
            break;
        }

        if c == ' ' || c == '\t' {
            if !prev_was_space {
                norm_count += 1;
                if norm_count >= norm_pos {
                    break;
                }
            }
        } else {
            norm_count += 1;
        }

        orig_pos += c.len_utf8();
        prev_was_space = c == ' ' || c == '\t';
    }

    orig_pos
}

/// Strategy 4: Indentation flexible (ignore all leading whitespace per line)
fn strategy_indentation_flexible(content: &str, pattern: &str) -> Option<Vec<(usize, usize)>> {
    let content_lines: Vec<&str> = content.lines().collect();
    let pattern_lines: Vec<&str> = pattern.lines().collect();

    if pattern_lines.is_empty() || content_lines.len() < pattern_lines.len() {
        return None;
    }

    let pattern_trimmed: Vec<&str> = pattern_lines.iter().map(|l| l.trim()).collect();
    let mut matches = Vec::new();

    for start_line in 0..=(content_lines.len() - pattern_lines.len()) {
        let mut all_match = true;

        for (i, pat) in pattern_trimmed.iter().enumerate() {
            let content_line = content_lines[start_line + i].trim();
            if content_line != *pat {
                all_match = false;
                break;
            }
        }

        if all_match {
            // Calculate character positions
            let line_offsets = get_line_offsets(content);
            let start = position_to_offset(&line_offsets, start_line, 0);
            let end = if start_line + pattern_lines.len() < content_lines.len() {
                position_to_offset(&line_offsets, start_line + pattern_lines.len(), 0)
            } else {
                content.len()
            };
            matches.push((start, end));
        }

        if matches.len() > 1000 {
            break;
        }
    }

    if matches.is_empty() {
        None
    } else {
        Some(matches)
    }
}

/// Strategy 5: Escape normalized (\n -> actual newline)
fn strategy_escape_normalized(content: &str, pattern: &str) -> Option<Vec<(usize, usize)>> {
    // Unescape the pattern
    let unescaped = pattern.replace("\\n", "\n").replace("\\t", "\t").replace("\\r", "\r");

    if unescaped == pattern {
        return None; // No escapes to convert
    }

    strategy_exact(content, &unescaped)
}

/// Strategy 6: Trimmed boundary (trim only first/last lines)
fn strategy_trimmed_boundary(content: &str, pattern: &str) -> Option<Vec<(usize, usize)>> {
    let pattern_lines: Vec<&str> = pattern.lines().collect();
    let content_lines: Vec<&str> = content.lines().collect();

    if pattern_lines.is_empty() || content_lines.len() < pattern_lines.len() {
        return None;
    }

    let mut matches = Vec::new();

    for start_line in 0..=(content_lines.len() - pattern_lines.len()) {
        // Trim only first and last lines
        let check_pattern: Vec<&str> = if pattern_lines.len() == 1 {
            vec![pattern_lines[0].trim()]
        } else {
            let mut check: Vec<&str> = pattern_lines.to_vec();
            check[0] = check[0].trim();
            let last_idx = check.len() - 1;
            let trimmed_last = check[last_idx].trim();
            check[last_idx] = trimmed_last;
            check
        };

        let block = &content_lines[start_line..start_line + pattern_lines.len()];
        let check_block: Vec<&str> = if block.len() == 1 {
            vec![block[0].trim()]
        } else {
            let mut check: Vec<&str> = block.to_vec();
            check[0] = check[0].trim();
            let last_idx = check.len() - 1;
            let trimmed_last = check[last_idx].trim();
            check[last_idx] = trimmed_last;
            check
        };

        if check_pattern == check_block {
            let line_offsets = get_line_offsets(content);
            let start = position_to_offset(&line_offsets, start_line, 0);
            let end = if start_line + pattern_lines.len() < content_lines.len() {
                position_to_offset(&line_offsets, start_line + pattern_lines.len(), 0)
            } else {
                content.len()
            };
            matches.push((start, end));
        }

        if matches.len() > 1000 {
            break;
        }
    }

    if matches.is_empty() {
        None
    } else {
        Some(matches)
    }
}

/// Unicode normalization map
const UNICODE_MAP: &[(char, &str)] = &[
    ('\u{2018}', "'"),  // smart single quotes
    ('\u{2019}', "'"),
    ('\u{201C}', "\""), // smart double quotes
    ('\u{201D}', "\""),
    ('\u{2013}', "-"),  // en dash
    ('\u{2014}', "-"),  // em dash
    ('\u{2026}', "..."), // ellipsis
    ('\u{00A0}', " "),  // non-breaking space
];

/// Strategy 7: Unicode normalized
fn strategy_unicode_normalized(content: &str, pattern: &str) -> Option<Vec<(usize, usize)>> {
    let norm_content = normalize_unicode(content);
    let norm_pattern = normalize_unicode(pattern);

    if norm_content == content && norm_pattern == pattern {
        return None; // No unicode to normalize
    }

    // Try exact first, then line_trimmed
    if let matches @ Some(_) = strategy_exact(&norm_content, &norm_pattern) {
        return matches;
    }

    strategy_line_trimmed(&norm_content, &norm_pattern)
}

/// Normalize unicode characters to ASCII equivalents
fn normalize_unicode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        let mut found = false;
        for (from, to) in UNICODE_MAP {
            if c == *from {
                result.push_str(to);
                found = true;
                break;
            }
        }
        if !found {
            result.push(c);
        }
    }
    result
}

/// Strategy 8: Block anchor (match first+last lines exactly, similarity in middle)
fn strategy_block_anchor(content: &str, pattern: &str) -> Option<Vec<(usize, usize)>> {
    let content_lines: Vec<&str> = content.lines().collect();
    let pattern_lines: Vec<&str> = pattern.lines().collect();

    if pattern_lines.len() < 2 || content_lines.len() < pattern_lines.len() {
        return None;
    }

    // Normalize for comparison
    let pattern_first = pattern_lines.first().unwrap().trim();
    let pattern_last = pattern_lines.last().unwrap().trim();

    let mut matches = Vec::new();

    for start_line in 0..=(content_lines.len() - pattern_lines.len()) {
        let content_first = content_lines[start_line].trim();
        let content_last = content_lines[start_line + pattern_lines.len() - 1].trim();

        // First and last lines must match exactly
        if content_first != pattern_first || content_last != pattern_last {
            continue;
        }

        // If only 2 lines, it's a match
        if pattern_lines.len() == 2 {
            let line_offsets = get_line_offsets(content);
            let start = position_to_offset(&line_offsets, start_line, 0);
            let end = position_to_offset(&line_offsets, start_line + 2, 0);
            matches.push((start, end));
            continue;
        }

        // Check middle similarity
        let content_middle: String = content_lines[start_line + 1..start_line + pattern_lines.len() - 1]
            .join("\n");
        let pattern_middle: String = pattern_lines[1..pattern_lines.len() - 1].join("\n");

        let sim = normalized_damerau_levenshtein(&content_middle, &pattern_middle);

        // Threshold: 0.50 for unique, 0.70 for multiple
        if sim >= 0.50 {
            let line_offsets = get_line_offsets(content);
            let start = position_to_offset(&line_offsets, start_line, 0);
            let end = if start_line + pattern_lines.len() < content_lines.len() {
                position_to_offset(&line_offsets, start_line + pattern_lines.len(), 0)
            } else {
                content.len()
            };
            matches.push((start, end));
        }
    }

    if matches.is_empty() {
        None
    } else {
        Some(matches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let content = "Hello world\nThis is a test";
        let matches = strategy_exact(content, "Hello");
        assert!(matches.is_some());
        assert_eq!(matches.unwrap().len(), 1);
    }

    #[test]
    fn test_exact_no_match() {
        let content = "Hello world";
        assert!(strategy_exact(content, "Goodbye").is_none());
    }

    #[test]
    fn test_fuzzy_find_and_replace_exact() {
        let (result, count, strategy) =
            fuzzy_find_and_replace("Hello world", "Hello", "Hi there", false).unwrap();
        assert_eq!(result, "Hi there world");
        assert_eq!(count, 1);
        assert_eq!(strategy, "exact");
    }

    #[test]
    fn test_fuzzy_find_and_replace_line_trimmed() {
        let content = "  Hello world  \n  Another line  ";
        let (result, _, _) = fuzzy_find_and_replace(content, "Hello world", "Hi", false).unwrap();
        assert!(result.contains("Hi"));
    }

    #[test]
    fn test_fuzzy_find_and_replace_error() {
        let result = fuzzy_find_and_replace("Hello", "NotFound", " replacement", false);
        assert!(result.is_err());
    }

    #[test]
    fn test_collapse_whitespace() {
        assert_eq!(collapse_whitespace("a    b"), "a b");
        assert_eq!(collapse_whitespace("a\t\tb"), "a b");
        assert_eq!(collapse_whitespace("a  \t  b"), "a b");
    }

    #[test]
    fn test_normalize_unicode() {
        assert_eq!(normalize_unicode("Hello\u{201C}world\u{201D}"), "Hello\"world\"");
        assert_eq!(normalize_unicode("test\u{2014}case"), "test-case");
    }

    #[test]
    fn test_replace_all() {
        let content = "foo bar foo baz foo";
        let (result, count, _) =
            fuzzy_find_and_replace(content, "foo", "qux", true).unwrap();
        assert_eq!(result, "qux bar qux baz qux");
        assert_eq!(count, 3);
    }
}
