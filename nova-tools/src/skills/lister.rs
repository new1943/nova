use crate::skills::cache::SharedSkillsLoader;
use crate::registry::{ToolHandler, ToolContext};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

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
    pub fn new(loader: SharedSkillsLoader) -> Self { Self { loader } }
}

#[async_trait]
impl ToolHandler for SkillsListTool {
    fn name(&self) -> &str { "skills_list" }
    fn description(&self) -> &str { "List all available skills with minimal metadata." }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "category": { "type": "string" } } })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
        let category = input.get("category").and_then(|v| v.as_str()).map(|s| s.to_string());
        let loader = match self.loader.lock() {
            Ok(l) => l,
            Err(e) => return Ok(serde_json::to_string(&SkillsListResult { success: false, skills: vec![], categories: vec![], message: Some(format!("Lock error: {}", e)) }).unwrap_or_default()),
        };
        let mut skills: Vec<SkillMeta> = loader.skills().iter().filter_map(|skill| {
            extract_skill_meta(&skill.name, &skill.prompt)
        }).collect();
        if let Some(ref cat) = category {
            skills.retain(|s| s.category.as_ref().map(|c| c == cat).unwrap_or(false));
        }
        let mut categories: Vec<String> = skills.iter().filter_map(|s| s.category.clone()).collect();
        categories.sort(); categories.dedup();
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        let result = SkillsListResult { success: true, skills, categories, message: None };
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }
}

fn extract_skill_meta(name: &str, prompt: &str) -> Option<SkillMeta> {
    let mut description = String::new();
    let mut category = None;
    let mut in_frontmatter = false;
    let mut frontmatter_end = false;
    for line in prompt.lines() {
        if line == "---" {
            if !in_frontmatter { in_frontmatter = true; } else if !frontmatter_end { frontmatter_end = true; continue; }
            continue;
        }
        if in_frontmatter && !frontmatter_end {
            if let Some(val) = line.strip_prefix("description:") { description = val.trim().to_string(); }
            else if let Some(val) = line.strip_prefix("category:") { let cat = val.trim().to_string(); if !cat.is_empty() { category = Some(cat); } }
        }
    }
    Some(SkillMeta { name: name.to_string(), description, category })
}
