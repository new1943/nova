use crate::skills::cache::SharedSkillsLoader;
use crate::skills::fuzzy::fuzzy_find_and_replace;
use crate::skills::security::{atomic_write, detect_injection, validate_frontmatter, validate_skill_path, validate_skill_name, MAX_SKILL_NAME_CHARS};
use crate::registry::{ToolHandler, ToolContext};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillAction { Create, Edit, Patch, Delete, WriteFile, RemoveFile }

#[derive(Debug, Clone, Deserialize)]
pub struct SkillManageInput {
    pub action: SkillAction,
    pub name: String,
    #[serde(default)] pub content: Option<String>,
    #[serde(default)] pub old_string: Option<String>,
    #[serde(default)] pub new_string: Option<String>,
    #[serde(default)] pub file_path: Option<String>,
    #[serde(default)] pub file_content: Option<String>,
    #[serde(default)] pub category: Option<String>,
    #[serde(default)] pub replace_all: bool,
}

#[derive(Debug, Serialize)]
struct SkillManageResult { success: bool, message: String }

impl SkillManageResult {
    fn ok(msg: impl Into<String>) -> String { serde_json::to_string(&Self { success: true, message: msg.into() }).unwrap_or_default() }
    fn err(msg: impl Into<String>) -> String { serde_json::to_string(&Self { success: false, message: msg.into() }).unwrap_or_default() }
}

pub struct SkillManageTool {
    skills_dir: PathBuf,
    loader: SharedSkillsLoader,
}

impl SkillManageTool {
    pub fn new(skills_dir: PathBuf, loader: SharedSkillsLoader) -> Self { Self { skills_dir, loader } }
}

#[async_trait]
impl ToolHandler for SkillManageTool {
    fn name(&self) -> &str { "skill_manage" }
    fn description(&self) -> &str { "Create, edit, patch, delete skills." }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "action": { "type": "string" }, "name": { "type": "string" }, "content": { "type": "string" }, "old_string": { "type": "string" }, "new_string": { "type": "string" }, "file_path": { "type": "string" }, "file_content": { "type": "string" }, "replace_all": { "type": "boolean" } }, "required": ["action", "name"] })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> Result<String> {
        let args: SkillManageInput = serde_json::from_value(input).map_err(|e| anyhow!("Invalid input: {}", e))?;
        if args.name.len() > MAX_SKILL_NAME_CHARS { return Ok(SkillManageResult::err("Name too long")); }
        if let Err(e) = validate_skill_name(&args.name) { return Ok(SkillManageResult::err(e.to_string())); }

        let skill_dir = match &args.category {
            Some(cat) => self.skills_dir.join(cat).join(&args.name),
            None => self.skills_dir.join(&args.name),
        };

        match args.action {
            SkillAction::Create => {
                let content = args.content.as_ref().ok_or_else(|| anyhow!("content required"))?;
                if let Err(e) = validate_frontmatter(content) { return Ok(SkillManageResult::err(format!("Invalid: {}", e))); }
                if let Err(e) = detect_injection(content) { return Ok(SkillManageResult::err(format!("Security: {}", e))); }
                if skill_dir.join("SKILL.md").exists() { return Ok(SkillManageResult::err("Already exists")); }
                fs::create_dir_all(&skill_dir)?;
                atomic_write(&skill_dir.join("SKILL.md"), content)?;
                let _ = crate::skills::cache::invalidate_skill_cache(&self.loader);
                Ok(SkillManageResult::ok(format!("Skill '{}' created", args.name)))
            }
            SkillAction::Edit => {
                let content = args.content.as_ref().ok_or_else(|| anyhow!("content required"))?;
                if !skill_dir.join("SKILL.md").exists() { return Ok(SkillManageResult::err("Not found")); }
                if let Err(e) = validate_frontmatter(content) { return Ok(SkillManageResult::err(format!("Invalid: {}", e))); }
                atomic_write(&skill_dir.join("SKILL.md"), content)?;
                let _ = crate::skills::cache::invalidate_skill_cache(&self.loader);
                Ok(SkillManageResult::ok(format!("Skill '{}' updated", args.name)))
            }
            SkillAction::Patch => {
                let old_string = args.old_string.as_ref().ok_or_else(|| anyhow!("old_string required"))?;
                let new_string = args.new_string.as_ref().ok_or_else(|| anyhow!("new_string required"))?;
                let skill_md = skill_dir.join("SKILL.md");
                if !skill_md.exists() { return Ok(SkillManageResult::err("Not found")); }
                let original = fs::read_to_string(&skill_md)?;
                let (result, count, _strategy) = match fuzzy_find_and_replace(&original, old_string, new_string, args.replace_all) {
                    Ok(r) => r,
                    Err(e) => return Ok(SkillManageResult::err(format!("Patch failed: {}", e))),
                };
                if count == 0 { return Ok(SkillManageResult::err("No matches")); }
                atomic_write(&skill_md, &result)?;
                let _ = crate::skills::cache::invalidate_skill_cache(&self.loader);
                Ok(SkillManageResult::ok(format!("Patched {} replacement(s)", count)))
            }
            SkillAction::Delete => {
                if !skill_dir.exists() { return Ok(SkillManageResult::err("Not found")); }
                fs::remove_dir_all(&skill_dir)?;
                let _ = crate::skills::cache::invalidate_skill_cache(&self.loader);
                Ok(SkillManageResult::ok(format!("Skill '{}' deleted", args.name)))
            }
            SkillAction::WriteFile => {
                let file_path = args.file_path.as_ref().ok_or_else(|| anyhow!("file_path required"))?;
                let file_content = args.file_content.as_ref().ok_or_else(|| anyhow!("file_content required"))?;
                if let Err(e) = validate_skill_path(file_path) { return Ok(SkillManageResult::err(format!("Invalid path: {}", e))); }
                let target = skill_dir.join(file_path);
                if let Some(parent) = target.parent() { fs::create_dir_all(parent)?; }
                atomic_write(&target, file_content)?;
                Ok(SkillManageResult::ok(format!("File '{}' written", file_path)))
            }
            SkillAction::RemoveFile => {
                let file_path = args.file_path.as_ref().ok_or_else(|| anyhow!("file_path required"))?;
                let target = skill_dir.join(file_path);
                if !target.exists() { return Ok(SkillManageResult::err("File not found")); }
                fs::remove_file(&target)?;
                Ok(SkillManageResult::ok(format!("File '{}' removed", file_path)))
            }
        }
    }
}
