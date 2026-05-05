use nova_core::injection_scanner::{InjectionScanner, ScanResult, ThreatType};
use proptest::prelude::*;

/// The set of invisible Unicode characters that InjectionScanner should remove.
const INVISIBLE_CHARS: &[char] = &[
    '\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}', '\u{00AD}',
    '\u{2060}', '\u{2061}', '\u{2062}', '\u{2063}', '\u{2064}',
];

/// Strategy to generate a visible (non-invisible) character.
fn visible_char_strategy() -> impl Strategy<Value = char> {
    any::<char>().prop_filter("must not be invisible", |c| !INVISIBLE_CHARS.contains(c))
}

/// Strategy to generate one of the invisible Unicode characters.
fn invisible_char_strategy() -> impl Strategy<Value = char> {
    prop::sample::select(INVISIBLE_CHARS)
}

/// Strategy to generate a string that contains at least one invisible Unicode character
/// interspersed with visible characters.
fn string_with_invisible_chars() -> impl Strategy<Value = (String, Vec<char>)> {
    // Generate a vector of visible chars and a vector of (position, invisible_char) insertions
    let visible_part = prop::collection::vec(visible_char_strategy(), 0..50);
    let invisible_insertions = prop::collection::vec(invisible_char_strategy(), 1..10);

    (visible_part, invisible_insertions).prop_map(|(visible, invisibles)| {
        // Build a string by interleaving visible and invisible chars
        let mut result = String::new();
        let mut visible_order = Vec::new();

        for (i, vc) in visible.iter().enumerate() {
            // Insert an invisible char before some visible chars
            if i < invisibles.len() {
                result.push(invisibles[i]);
            }
            result.push(*vc);
            visible_order.push(*vc);
        }
        // Append remaining invisible chars at the end
        for ic in invisibles.iter().skip(visible.len()) {
            result.push(*ic);
        }

        (result, visible_order)
    })
}

/// The list of threat patterns used by InjectionScanner.
const THREAT_PATTERNS: &[&str] = &[
    "ignore previous instructions",
    "ignore all previous",
    "disregard previous",
    "disregard all previous",
    "you are now",
    "override your",
    "forget your instructions",
    "new instructions",
    "system prompt",
    "act as",
    "pretend to be",
    "jailbreak",
];

/// Strategy to generate a random case variant of a string.
fn random_case(s: &str) -> impl Strategy<Value = String> {
    let len = s.len();
    let s_owned = s.to_string();
    prop::collection::vec(prop::bool::ANY, len).prop_map(move |bools| {
        s_owned
            .chars()
            .zip(bools.iter().cycle())
            .map(|(c, &upper)| {
                if upper {
                    c.to_uppercase().to_string()
                } else {
                    c.to_lowercase().to_string()
                }
            })
            .collect::<String>()
    })
}

/// Strategy to pick a random threat pattern and generate a random case variant.
fn random_case_threat_pattern() -> impl Strategy<Value = (String, String)> {
    prop::sample::select(THREAT_PATTERNS).prop_flat_map(|pattern| {
        let pattern_owned = pattern.to_string();
        random_case(pattern).prop_map(move |case_variant| {
            (pattern_owned.clone(), case_variant)
        })
    })
}

proptest! {
    /// Feature: r3-open-connectivity, Property 4: Invisible Unicode character removal
    ///
    /// **Validates: Requirements 7.1**
    ///
    /// For any string containing invisible Unicode characters, after scan:
    /// - sanitized_content contains no invisible Unicode characters
    /// - visible characters order is preserved
    #[test]
    fn prop_invisible_unicode_removal((input, visible_order) in string_with_invisible_chars()) {
        let result: ScanResult = InjectionScanner::scan(&input);

        // Should detect invisible unicode threat
        prop_assert!(result.has_threat, "Expected has_threat=true for input with invisible chars");
        prop_assert!(
            result.threat_types.contains(&ThreatType::InvisibleUnicode),
            "Expected ThreatType::InvisibleUnicode in threat_types"
        );

        // Sanitized content should contain no invisible chars
        for c in result.sanitized_content.chars() {
            prop_assert!(
                !INVISIBLE_CHARS.contains(&c),
                "Sanitized content still contains invisible char: {:?}",
                c
            );
        }

        // Visible characters order should be preserved
        let sanitized_visible: Vec<char> = result
            .sanitized_content
            .chars()
            .filter(|c| !INVISIBLE_CHARS.contains(c))
            .collect();

        // The visible chars from the original input should appear in the same order
        // Note: sanitized_content may also have [BLOCKED] replacements if threat patterns
        // happen to be generated, so we compare against the original visible_order
        // by checking that visible_order chars appear as a subsequence
        let mut order_iter = visible_order.iter();
        for sc in &sanitized_visible {
            if let Some(expected) = order_iter.next() {
                prop_assert_eq!(sc, expected, "Visible char order mismatch");
            } else {
                // More chars in sanitized than expected - this shouldn't happen
                // unless [BLOCKED] added chars, which are all ASCII visible
                break;
            }
        }
    }

    /// Feature: r3-open-connectivity, Property 5: Threat pattern detection and replacement
    ///
    /// **Validates: Requirements 7.2, 7.3, 7.6**
    ///
    /// For any string containing a threat pattern (in any case), after scan:
    /// - the pattern is replaced with [BLOCKED]
    /// - non-matching parts are unchanged
    #[test]
    fn prop_threat_pattern_detection_and_replacement(
        prefix in "[a-zA-Z0-9 ]{0,20}",
        (_original_pattern, case_variant) in random_case_threat_pattern(),
        suffix in "[a-zA-Z0-9 ]{0,20}",
    ) {
        let input = format!("{}{}{}", prefix, case_variant, suffix);
        let result: ScanResult = InjectionScanner::scan(&input);

        // Should detect threat
        prop_assert!(result.has_threat, "Expected has_threat=true for input containing threat pattern");

        // Should contain the ThreatPattern type
        let has_pattern_threat = result.threat_types.iter().any(|t| {
            matches!(t, ThreatType::ThreatPattern(_))
        });
        prop_assert!(has_pattern_threat, "Expected ThreatType::ThreatPattern in threat_types");

        // The matched part should be replaced with [BLOCKED]
        let expected_sanitized = format!("{}[BLOCKED]{}", prefix, suffix);
        prop_assert_eq!(
            result.sanitized_content,
            expected_sanitized,
            "Expected pattern to be replaced with [BLOCKED], preserving prefix and suffix"
        );
    }
}
