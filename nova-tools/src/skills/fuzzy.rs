//! Fuzzy matching engine for skill content patching.

use anyhow::{Result, anyhow};

pub fn fuzzy_find_and_replace(
    original: &str, old_string: &str, new_string: &str, replace_all: bool,
) -> Result<(String, usize, &'static str)> {
    if old_string.is_empty() { anyhow::bail!("old_string cannot be empty"); }
    if old_string == new_string { return Ok((original.to_string(), 0, "identical")); }

    #[allow(clippy::type_complexity)]
    let strategies: Vec<(&str, fn(&str, &str) -> Option<Vec<(usize, usize)>>)> = vec![
        ("exact", strategy_exact),
        ("line_trimmed", strategy_line_trimmed),
        ("indentation_flexible", strategy_indentation_flexible),
    ];

    for (name, strategy) in strategies {
        if let Some(matches) = strategy(original, old_string) {
            if matches.is_empty() { continue; }
            if matches.len() > 1 && !replace_all {
                return Err(anyhow!("Found {} matches. Use replace_all=true.", matches.len()));
            }
            let match_count = matches.len();
            let modified = apply_replacements(original, &matches, new_string);
            return Ok((modified, match_count, name));
        }
    }

    Err(anyhow!("Could not find '{}' in content.", truncate(old_string, 50)))
}

fn apply_replacements(original: &str, matches: &[(usize, usize)], new_string: &str) -> String {
    let mut result = original.to_string();
    let mut sorted_matches: Vec<_> = matches.iter().collect();
    sorted_matches.sort_by(|a, b| b.0.cmp(&a.0));
    for &(start, end) in sorted_matches {
        result = format!("{}{}{}", &result[..start], new_string, &result[end..]);
    }
    result
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len { s.to_string() } else { format!("{}...", &s[..max_len]) }
}

fn get_line_offsets(content: &str) -> Vec<usize> {
    let mut offsets = vec![0];
    for (i, c) in content.char_indices() {
        if c == '\n' { offsets.push(i + 1); }
    }
    offsets
}

fn position_to_offset(line_offsets: &[usize], line: usize, _col: usize) -> usize {
    if line >= line_offsets.len() { line_offsets.last().copied().unwrap_or(0) }
    else { line_offsets[line] }
}

fn strategy_exact(content: &str, pattern: &str) -> Option<Vec<(usize, usize)>> {
    let mut matches = Vec::new();
    let mut start = 0;
    while let Some(pos) = content[start..].find(pattern) {
        let abs_pos = start + pos;
        matches.push((abs_pos, abs_pos + pattern.len()));
        start = abs_pos + 1;
        if matches.len() > 1000 { break; }
    }
    if matches.is_empty() { None } else { Some(matches) }
}

fn strategy_line_trimmed(content: &str, pattern: &str) -> Option<Vec<(usize, usize)>> {
    let pattern_lines: Vec<&str> = pattern.lines().collect();
    let content_lines: Vec<&str> = content.lines().collect();
    if pattern_lines.is_empty() || content_lines.len() < pattern_lines.len() { return None; }

    let pattern_trimmed: Vec<&str> = pattern_lines.iter().map(|l| l.trim()).collect();
    let mut matches = Vec::new();

    for start_line in 0..=(content_lines.len() - pattern_lines.len()) {
        let mut all_match = true;
        for (i, pat) in pattern_trimmed.iter().enumerate() {
            if content_lines[start_line + i].trim() != *pat { all_match = false; break; }
        }
        if all_match {
            let line_offsets = get_line_offsets(content);
            let start = position_to_offset(&line_offsets, start_line, 0);
            let end = if start_line + pattern_lines.len() < content_lines.len() {
                position_to_offset(&line_offsets, start_line + pattern_lines.len(), 0)
            } else { content.len() };
            matches.push((start, end));
        }
        if matches.len() > 1000 { break; }
    }

    if matches.is_empty() { None } else { Some(matches) }
}

fn strategy_indentation_flexible(content: &str, pattern: &str) -> Option<Vec<(usize, usize)>> {
    // Same as line_trimmed for now
    strategy_line_trimmed(content, pattern)
}
