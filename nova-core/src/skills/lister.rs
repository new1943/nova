//! skills_list tool - list all available skills with minimal metadata.

use crate::skills::cache::SharedSkillsLoader;
use crate::tools::registry::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Skill metadata for listing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

/// Input for skills_list tool
#[derive(Debug, Clone, Deserialize)]
pub struct SkillsListInput {
    #[serde(default)]
    pub category: Option<String>,
}

/// Result wrapper
#[derive(Debug, Serialize)]
pub struct SkillsListResult {
    pub success: bool,
    pub skills: Vec<SkillMeta>,
    pub categories: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub struct SkillsListTool {
    loader: SharedSkillsLoader,
}

impl SkillsListTool {
    pub fn new(loader: SharedSkillsLoader) -> Self {
        Self { loader }
    }
}

#[async_trait]
impl Tool for SkillsListTool {
    fn name(&self) -> &str {
        "skills_list"
    }

    fn description(&self) -> &str {
        "List all available skills with minimal metadata (name, description, category). Use skill_view(name) to see full content."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "description": "Optional category filter (e.g., 'mlops', 'devops')"
                }
            },
            "properties": {}
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let args: SkillsListInput = serde_json::from_value(input).unwrap_or(SkillsListInput {
            category: None,
        });

        let loader = match self.loader.lock() {
            Ok(l) => l,
            Err(e) => {
                return Ok(serde_json::to_string(&SkillsListResult {
                    success: false,
                    skills: vec![],
                    categories: vec![],
                    message: Some(format!("Failed to acquire skills lock: {}", e)),
                })
                .unwrap_or_default());
            }
        };

        let mut skills: Vec<SkillMeta> = loader
            .skills()
            .iter()
            .filter_map(|skill| extract_skill_meta(&skill.name, &skill.prompt))
            .collect();

        // Filter by category if specified
        if let Some(ref cat) = args.category {
            skills.retain(|s| s.category.as_ref().map(|c| c == cat).unwrap_or(false));
        }

        // Collect unique categories
        let mut categories: Vec<String> = skills
            .iter()
            .filter_map(|s| s.category.clone())
            .collect();
        categories.sort();
        categories.dedup();

        // Sort by category then name
        skills.sort_by(|a, b| {
            let cat_cmp = a.category.cmp(&b.category);
            if cat_cmp == std::cmp::Ordering::Equal {
                a.name.cmp(&b.name)
            } else {
                cat_cmp
            }
        });

        let result = SkillsListResult {
            success: true,
            skills,
            categories,
            message: None,
        };

        Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| r#"{"success":false,"message":"JSON serialization error"}"#.to_string()))
    }
}

/// Extract metadata from skill prompt content
fn extract_skill_meta(name: &str, prompt: &str) -> Option<SkillMeta> {
    let mut description = String::new();
    let mut category = None;

    // Parse frontmatter
    let mut in_frontmatter = false;
    let mut frontmatter_end = false;
    let mut found_description = false;

    for line in prompt.lines() {
        if line == "---" {
            if !in_frontmatter {
                in_frontmatter = true;
            } else if !frontmatter_end {
                frontmatter_end = true;
                continue;
            }
            continue;
        }

        if in_frontmatter && !frontmatter_end {
            if let Some(val) = line.strip_prefix("description:") {
                description = val.trim().to_string();
                found_description = true;
            } else if let Some(val) = line.strip_prefix("category:") {
                let cat = val.trim().to_string();
                if !cat.is_empty() {
                    category = Some(cat);
                }
            }
        } else if !found_description && !line.starts_with('#') && !line.trim().is_empty() {
            // First non-header, non-empty line as description fallback
            description = line.trim().to_string();
            break;
        }
    }

    if description.len() > 1024 {
        description = format!("{}...", &description[..1021]);
    }

    Some(SkillMeta {
        name: name.to_string(),
        description,
        category,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_skill_meta() {
        let prompt = r#"---
name: test-skill
description: A test skill for unit testing
category: testing
---

# Test Skill

Content here.
"#;
        let meta = extract_skill_meta("test-skill", prompt);
        assert!(meta.is_some());
        let meta = meta.unwrap();
        assert_eq!(meta.name, "test-skill");
        assert_eq!(meta.description, "A test skill for unit testing");
        assert_eq!(meta.category, Some("testing".to_string()));
    }

    #[test]
    fn test_extract_skill_meta_no_category() {
        let prompt = r#"---
name: test
description: Test
---

# Test
"#;
        let meta = extract_skill_meta("test", prompt);
        assert!(meta.is_some());
        let meta = meta.unwrap();
        assert_eq!(meta.category, None);
    }
}
