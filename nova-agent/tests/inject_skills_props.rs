// Feature: r4-self-evolution, Property 5: InjectStage Skill 摘要注入格式
// Feature: r4-self-evolution, Property 6: InjectStage auto_trigger 完整注入

use proptest::prelude::*;
use std::sync::{Arc, Mutex};

use nova_agent::stages::inject::InjectStage;
use nova_tools::skills::loader::SkillsLoader;

/// Helper: create a temp directory with skill files and return a loaded SkillsLoader
fn create_loader_from_skills(skills: &[(String, String, Option<Vec<String>>)]) -> SkillsLoader {
    let temp_dir = std::env::temp_dir().join(format!("nova-test-skills-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).unwrap();

    for (name, description, keywords) in skills {
        let skill_dir = temp_dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();

        let mut content = String::new();
        content.push_str("---\n");
        content.push_str(&format!("description: {}\n", description));
        if let Some(kws) = keywords {
            content.push_str(&format!("auto_trigger_keywords: {}\n", kws.join(", ")));
        }
        content.push_str("---\n");
        content.push_str(&format!("# Skill: {}\n\nThis is the full content of {}.", name, name));

        std::fs::write(skill_dir.join("SKILL.md"), &content).unwrap();
    }

    let mut loader = SkillsLoader::new(temp_dir.clone());
    loader.load_all().unwrap();

    // Cleanup will happen when tests finish (temp dir)
    loader
}

/// Strategy: generate a valid skill name (alphanumeric + hyphens, non-empty)
fn skill_name_strategy() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9\\-]{0,15}".prop_map(|s| s)
}

/// Strategy: generate a skill description (non-empty, no newlines)
fn skill_description_strategy() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ]{1,50}".prop_map(|s| s)
}

/// Strategy: generate optional keywords
fn keywords_strategy() -> impl Strategy<Value = Option<Vec<String>>> {
    prop_oneof![
        Just(None),
        prop::collection::vec("[a-z]{3,10}", 1..=3).prop_map(Some),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Property 5: For any non-empty Skill set, InjectStage's injected Skill content
    /// SHALL use `<available-skills>` tag and only contain name and description summaries,
    /// not full prompt content.
    ///
    /// **Validates: Requirements 5.1, 5.2, 5.4**
    #[test]
    fn prop_inject_skills_summary_format(
        skills in prop::collection::vec(
            (skill_name_strategy(), skill_description_strategy(), keywords_strategy()),
            1..=5
        )
    ) {
        // Deduplicate skill names
        let mut seen = std::collections::HashSet::new();
        let unique_skills: Vec<_> = skills.into_iter()
            .filter(|(name, _, _)| seen.insert(name.clone()))
            .collect();

        if unique_skills.is_empty() {
            return Ok(());
        }

        let loader = create_loader_from_skills(&unique_skills);
        let summary = InjectStage::build_skills_summary(&loader);

        // Summary should not be empty for non-empty skill set
        prop_assert!(!summary.is_empty(), "Summary should not be empty for non-empty skill set");

        // Summary should contain the header text
        prop_assert!(
            summary.contains("可用技能列表（使用 skill_view(name) 查看详情）："),
            "Summary should contain the header text"
        );

        // For each skill, summary should contain name and description (trimmed)
        for (name, description, _) in &unique_skills {
            prop_assert!(
                summary.contains(name),
                "Summary should contain skill name '{}', got: {}",
                name, summary
            );
            // Description is trimmed when extracted from frontmatter
            let trimmed_desc = description.trim();
            if !trimmed_desc.is_empty() {
                prop_assert!(
                    summary.contains(trimmed_desc),
                    "Summary should contain skill description '{}', got: {}",
                    trimmed_desc, summary
                );
            }
        }

        // Summary should NOT contain full prompt content markers
        // (full content has "# Skill:" header from our test data)
        prop_assert!(
            !summary.contains("# Skill:"),
            "Summary should NOT contain full prompt content, got: {}",
            summary
        );
        prop_assert!(
            !summary.contains("This is the full content"),
            "Summary should NOT contain full prompt body, got: {}",
            summary
        );

        // Now test the full injection via execute() to verify <available-skills> tag usage
        let shared_loader = Arc::new(Mutex::new(loader));
        let stage = InjectStage::new(None, Some(shared_loader));
        let mut ctx = nova_core::pipeline::TurnContext::new(
            "hello".to_string(),
            vec![],
        );

        // Run execute synchronously via tokio runtime
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use nova_core::pipeline::PipelineStage;
            stage.execute(&mut ctx).await.unwrap();
        });

        // Find the available-skills injection
        let skills_injection = ctx.prompt_injections.iter()
            .find(|p| p.tag == "available-skills");
        prop_assert!(
            skills_injection.is_some(),
            "Should have an 'available-skills' injection"
        );

        let injection_content = &skills_injection.unwrap().content;
        // Verify it contains summaries but not full content
        for (name, description, _) in &unique_skills {
            prop_assert!(
                injection_content.contains(name),
                "Injection should contain skill name '{}'",
                name
            );
            let trimmed_desc = description.trim();
            if !trimmed_desc.is_empty() {
                prop_assert!(
                    injection_content.contains(trimmed_desc),
                    "Injection should contain skill description '{}'",
                    trimmed_desc
                );
            }
        }
        prop_assert!(
            !injection_content.contains("# Skill:"),
            "Injection should NOT contain full prompt content"
        );

        // Cleanup temp dir
        let loader_for_cleanup = ctx.prompt_injections.len(); // just to use ctx
        let _ = loader_for_cleanup;
    }

    /// Property 6: For any Skill with auto_trigger keywords, when user message contains
    /// matching keywords, InjectStage SHALL inject that Skill's full content.
    ///
    /// **Validates: Requirements 5.3**
    #[test]
    fn prop_inject_auto_triggered_skills(
        skill_name in skill_name_strategy(),
        description in skill_description_strategy(),
        keyword in "[a-z]{3,10}",
        prefix in "[a-zA-Z0-9 ]{0,20}",
        suffix in "[a-zA-Z0-9 ]{0,20}",
    ) {
        // Create a skill with auto_trigger keyword
        let skills = vec![
            (skill_name.clone(), description.clone(), Some(vec![keyword.clone()])),
        ];
        let loader = create_loader_from_skills(&skills);

        // User input contains the keyword
        let user_input = format!("{} {} {}", prefix, keyword, suffix);

        // Test get_auto_triggered_skills
        let triggered = InjectStage::get_auto_triggered_skills(&loader, &user_input);

        // Should have exactly one triggered skill
        prop_assert_eq!(
            triggered.len(), 1,
            "Should trigger exactly one skill for keyword '{}' in input '{}', got {} triggers",
            keyword, user_input, triggered.len()
        );

        // The triggered content should contain the full skill content
        let triggered_content = &triggered[0];
        prop_assert!(
            triggered_content.contains(&format!("<auto-triggered-skill name=\"{}\">", skill_name)),
            "Triggered content should have correct opening tag with skill name"
        );
        prop_assert!(
            triggered_content.contains("</auto-triggered-skill>"),
            "Triggered content should have closing tag"
        );
        // Full content includes the "# Skill:" header
        prop_assert!(
            triggered_content.contains("# Skill:"),
            "Triggered content should contain full prompt content (# Skill: header)"
        );
        prop_assert!(
            triggered_content.contains("This is the full content"),
            "Triggered content should contain full prompt body"
        );

        // Now test via execute() to verify injection into TurnContext
        let shared_loader = Arc::new(Mutex::new(loader));
        let stage = InjectStage::new(None, Some(shared_loader));
        let mut ctx = nova_core::pipeline::TurnContext::new(
            user_input.clone(),
            vec![],
        );

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use nova_core::pipeline::PipelineStage;
            stage.execute(&mut ctx).await.unwrap();
        });

        // Should have auto-triggered-skill injection
        let auto_injection = ctx.prompt_injections.iter()
            .find(|p| p.tag == "auto-triggered-skill");
        prop_assert!(
            auto_injection.is_some(),
            "Should have an 'auto-triggered-skill' injection for keyword '{}' in input '{}'",
            keyword, user_input
        );

        let auto_content = &auto_injection.unwrap().content;
        prop_assert!(
            auto_content.contains(&skill_name),
            "Auto-triggered injection should reference skill name '{}'",
            skill_name
        );
        prop_assert!(
            auto_content.contains("# Skill:"),
            "Auto-triggered injection should contain full prompt content"
        );
    }

    /// Property 6 (negative case): When user message does NOT contain any keyword,
    /// no auto-triggered skill should be injected.
    #[test]
    fn prop_no_auto_trigger_without_keyword(
        skill_name in skill_name_strategy(),
        description in skill_description_strategy(),
        keyword in "[a-z]{5,10}",
        user_input in "[A-Z0-9 ]{1,30}",  // uppercase only, won't match lowercase keywords
    ) {
        let skills = vec![
            (skill_name.clone(), description.clone(), Some(vec![keyword.clone()])),
        ];
        let loader = create_loader_from_skills(&skills);

        // Ensure user_input doesn't accidentally contain the keyword
        if user_input.to_lowercase().contains(&keyword) {
            return Ok(());  // Skip this case
        }

        let triggered = InjectStage::get_auto_triggered_skills(&loader, &user_input);
        prop_assert_eq!(
            triggered.len(), 0,
            "Should NOT trigger any skill when keyword '{}' is not in input '{}'",
            keyword, user_input
        );
    }
}
