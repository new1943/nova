use crate::skills::cache::SharedSkillsLoader;
use crate::skills::security::validate_skill_path;
use crate::registry::{ToolHandler, ToolContext};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize)]
pub struct LinkedFiles {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub references: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub templates: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scripts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct SkillViewResult {
    success: bool,
    name: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub struct SkillViewTool {
    skills_dir: PathBuf,
    #[allow(dead_code)]
    loader: SharedSkillsLoader,
}

impl SkillViewTool {
    pub fn new(skills_dir: PathBuf, loader: SharedSkillsLoader) -> Self {
        Self { skills_dir, loader }
    }
}

#[async_trait]
impl ToolHandler for SkillViewTool {
    fn name(&self) -> &str { "skill_view" }
    fn description(&self) -> &str { "View skill content with progressive disclosure." }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "name": { "type": "string" }, "file_path": { "type": "string" } }, "required": ["name"] })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
        let name = input.get("name").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing 'name'"))?;
        let file_path = input.get("file_path").and_then(|v| v.as_str());

        let skill_dir = self.skills_dir.join(name);
        if !skill_dir.exists() {
            return Ok(serde_json::to_string(&SkillViewResult { success: false, name: name.to_string(), content: String::new(), error: Some(format!("Skill '{}' not found", name)) }).unwrap_or_default());
        }

        match file_path {
            Some(fp) => {
                if let Err(e) = validate_skill_path(fp) {
                    return Ok(serde_json::to_string(&SkillViewResult { success: false, name: name.to_string(), content: String::new(), error: Some(format!("Invalid path: {}", e)) }).unwrap_or_default());
                }
                let target = skill_dir.join(fp);
                match fs::read_to_string(&target) {
                    Ok(content) => Ok(serde_json::to_string(&SkillViewResult { success: true, name: name.to_string(), content, error: None }).unwrap_or_default()),
                    Err(e) => Ok(serde_json::to_string(&SkillViewResult { success: false, name: name.to_string(), content: String::new(), error: Some(format!("Read error: {}", e)) }).unwrap_or_default()),
                }
            }
            None => {
                let skill_md = skill_dir.join("SKILL.md");
                match fs::read_to_string(&skill_md) {
                    Ok(content) => Ok(serde_json::to_string(&SkillViewResult { success: true, name: name.to_string(), content, error: None }).unwrap_or_default()),
                    Err(e) => Ok(serde_json::to_string(&SkillViewResult { success: false, name: name.to_string(), content: String::new(), error: Some(format!("Read error: {}", e)) }).unwrap_or_default()),
                }
            }
        }
    }
}
