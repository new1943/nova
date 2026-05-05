/// 扫描结果
#[derive(Debug, Clone)]
pub struct ScanResult {
    /// 是否检测到威胁
    pub has_threat: bool,
    /// 威胁类型列表
    pub threat_types: Vec<ThreatType>,
    /// 处理后的安全内容
    pub sanitized_content: String,
}

/// 威胁类型
#[derive(Debug, Clone, PartialEq)]
pub enum ThreatType {
    InvisibleUnicode,
    ThreatPattern(String),
}

/// Prompt Injection 扫描器
pub struct InjectionScanner;

impl InjectionScanner {
    /// 扫描注入内容，检测并替换危险部分
    pub fn scan(content: &str) -> ScanResult {
        let mut sanitized = content.to_string();
        let mut threat_types = Vec::new();

        // 1. 检测不可见 Unicode 字符
        if Self::has_invisible_unicode(&sanitized) {
            threat_types.push(ThreatType::InvisibleUnicode);
            sanitized = Self::remove_invisible_unicode(&sanitized);
        }

        // 2. 检测威胁模式文本（不区分大小写）
        let patterns = Self::threat_patterns();
        for pattern in &patterns {
            if Self::contains_pattern_ci(&sanitized, pattern) {
                threat_types.push(ThreatType::ThreatPattern(pattern.clone()));
                sanitized = Self::replace_pattern_ci(&sanitized, pattern, "[BLOCKED]");
            }
        }

        ScanResult {
            has_threat: !threat_types.is_empty(),
            threat_types,
            sanitized_content: sanitized,
        }
    }

    /// 不可见 Unicode 字符集
    fn invisible_chars() -> &'static [char] {
        &[
            '\u{200B}', // Zero Width Space
            '\u{200C}', // Zero Width Non-Joiner
            '\u{200D}', // Zero Width Joiner
            '\u{FEFF}', // BOM / Zero Width No-Break Space
            '\u{00AD}', // Soft Hyphen
            '\u{2060}', // Word Joiner
            '\u{2061}', // Function Application
            '\u{2062}', // Invisible Times
            '\u{2063}', // Invisible Separator
            '\u{2064}', // Invisible Plus
        ]
    }

    fn has_invisible_unicode(s: &str) -> bool {
        s.chars().any(|c| Self::invisible_chars().contains(&c))
    }

    fn remove_invisible_unicode(s: &str) -> String {
        s.chars()
            .filter(|c| !Self::invisible_chars().contains(c))
            .collect()
    }

    /// 威胁模式列表
    fn threat_patterns() -> Vec<String> {
        vec![
            "ignore previous instructions".into(),
            "ignore all previous".into(),
            "disregard previous".into(),
            "disregard all previous".into(),
            "you are now".into(),
            "override your".into(),
            "forget your instructions".into(),
            "new instructions".into(),
            "system prompt".into(),
            "act as".into(),
            "pretend to be".into(),
            "jailbreak".into(),
        ]
    }

    fn contains_pattern_ci(text: &str, pattern: &str) -> bool {
        text.to_lowercase().contains(&pattern.to_lowercase())
    }

    fn replace_pattern_ci(text: &str, pattern: &str, replacement: &str) -> String {
        let lower = text.to_lowercase();
        let pattern_lower = pattern.to_lowercase();
        let mut result = String::with_capacity(text.len());
        let mut search_start = 0;

        while let Some(pos) = lower[search_start..].find(&pattern_lower) {
            let abs_pos = search_start + pos;
            result.push_str(&text[search_start..abs_pos]);
            result.push_str(replacement);
            search_start = abs_pos + pattern.len();
        }
        result.push_str(&text[search_start..]);
        result
    }
}
