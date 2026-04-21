//! skill_manage tool - create, edit, patch, delete skills.

use crate::skills::cache::SharedSkillsLoader;
use crate::skills::fuzzy::fuzzy_find_and_replace;
use crate::skills::security::{
    atomic_write, detect_injection, rollback_atomic, validate_frontmatter, validate_skill_path,
    validate_skill_name, MAX_SKILL_NAME_CHARS,
};
use crate::tools::registry::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

/// Skill management actions
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillAction {
    Create,
    Edit,
    Patch,
    Delete,
    WriteFile,
    RemoveFile,
}

/// Input for skill_manage tool
#[derive(Debug, Clone, Deserialize)]
pub struct SkillManageInput {
    pub action: SkillAction,
    pub name: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub old_string: Option<String>,
    #[serde(default)]
    pub new_string: Option<String>,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub file_content: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub replace_all: bool,
}

/// Result type for tool execution
#[derive(Debug, Serialize)]
pub struct SkillManageResult {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl SkillManageResult {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            path: None,
        }
    }

    pub fn ok_with_path(message: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            path: Some(path.into()),
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            path: None,
        }
    }
}

pub struct SkillManageTool {
    skills_dir: PathBuf,
    loader: SharedSkillsLoader,
}

impl SkillManageTool {
    pub fn new(skills_dir: PathBuf, loader: SharedSkillsLoader) -> Self {
        Self { skills_dir, loader }
    }
}

#[async_trait]
impl Tool for SkillManageTool {
    fn name(&self) -> &str {
        "skill_manage"
    }

    fn description(&self) -> &str {
        "Create, edit, patch, delete skills. Actions: create (name + content), edit (name + content), patch (name + old_string + new_string), delete (name), write_file (name + file_path + file_content), remove_file (name + file_path)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "edit", "patch", "delete", "write_file", "remove_file"],
                    "description": "The action to perform"
                },
                "name": {
                    "type": "string",
                    "description": "Skill name (max 64 chars, lowercase, hyphens/underscores)"
                },
                "content": {
                    "type": "string",
                    "description": "Full SKILL.md content with YAML frontmatter (for create/edit)"
                },
                "old_string": {
                    "type": "string",
                    "description": "Text to find and replace (for patch)"
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text (for patch)"
                },
                "file_path": {
                    "type": "string",
                    "description": "Path within skill dir: references/, templates/, scripts/, assets/ (for write_file/remove_file/patch)"
                },
                "file_content": {
                    "type": "string",
                    "description": "File content (for write_file)"
                },
                "category": {
                    "type": "string",
                    "description": "Category subdirectory (for create)"
                },
                "replace_all": {
                    "type": "boolean",
                    "default": false,
                    "description": "Replace all occurrences (for patch)"
                }
            },
            "required": ["action", "name"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let args: SkillManageInput = serde_json::from_value(input)
            .map_err(|e| anyhow!("Invalid input: {}", e))?;

        // Basic name validation
        if args.name.len() > MAX_SKILL_NAME_CHARS {
            return Ok(SkillManageResult::err(format!(
                "Skill name exceeds {} characters",
                MAX_SKILL_NAME_CHARS
            ))
            .to_json());
        }

        if let Err(e) = validate_skill_name(&args.name) {
            return Ok(SkillManageResult::err(e.to_string()).to_json());
        }

        let skill_dir = self.resolve_skill_dir(&args.name, &args.category);

        match args.action {
            SkillAction::Create => self.create_skill(&args, &skill_dir).await,
            SkillAction::Edit => self.edit_skill(&args, &skill_dir).await,
            SkillAction::Patch => self.patch_skill(&args, &skill_dir).await,
            SkillAction::Delete => self.delete_skill(&skill_dir).await,
            SkillAction::WriteFile => self.write_file(&args, &skill_dir).await,
            SkillAction::RemoveFile => self.remove_file(&args, &skill_dir).await,
        }
    }
}

impl SkillManageTool {
    fn resolve_skill_dir(&self, name: &str, category: &Option<String>) -> PathBuf {
        match category {
            Some(cat) => self.skills_dir.join(cat).join(name),
            None => self.skills_dir.join(name),
        }
    }

    fn skill_exists(&self, skill_dir: &PathBuf) -> bool {
        skill_dir.exists() && skill_dir.join("SKILL.md").exists()
    }

    async fn create_skill(&self, args: &SkillManageInput, skill_dir: &PathBuf) -> Result<String> {
        let content = args
            .content
            .as_ref()
            .ok_or_else(|| anyhow!("content is required for 'create'"))?;

        // Validate content
        if let Err(e) = validate_frontmatter(content) {
            return Ok(SkillManageResult::err(format!(
                "Invalid SKILL.md content: {}",
                e
            )).to_json());
        }

        if let Err(e) = detect_injection(content) {
            return Ok(SkillManageResult::err(format!(
                "Security scan failed: {}",
                e
            )).to_json());
        }

        // Check if already exists
        if self.skill_exists(skill_dir) {
            return Ok(SkillManageResult::err(format!(
                "Skill '{}' already exists",
                args.name
            )).to_json());
        }

        // Create directory and write file
        if let Err(e) = fs::create_dir_all(skill_dir) {
            return Ok(SkillManageResult::err(format!(
                "Failed to create skill directory: {}",
                e
            )).to_json());
        }

        let skill_md = skill_dir.join("SKILL.md");
        if let Err(e) = atomic_write(&skill_md, content) {
            // Clean up on failure
            let _ = fs::remove_dir_all(skill_dir);
            return Ok(SkillManageResult::err(format!(
                "Failed to write SKILL.md: {}",
                e
            )).to_json());
        }

        // Invalidate cache
        if let Err(e) = crate::skills::cache::invalidate_skill_cache(&self.loader) {
            tracing::warn!("Failed to invalidate skill cache: {}", e);
        }

        let relative = skill_dir
            .strip_prefix(&self.skills_dir)
            .unwrap_or(skill_dir)
            .to_string_lossy()
            .to_string();

        Ok(SkillManageResult::ok_with_path(
            format!("Skill '{}' created successfully", args.name),
            relative,
        ).to_json())
    }

    async fn edit_skill(&self, args: &SkillManageInput, skill_dir: &PathBuf) -> Result<String> {
        let content = args
            .content
            .as_ref()
            .ok_or_else(|| anyhow!("content is required for 'edit'"))?;

        if !self.skill_exists(skill_dir) {
            return Ok(SkillManageResult::err(format!(
                "Skill '{}' not found. Use 'create' first.",
                args.name
            )).to_json());
        }

        // Validate content
        if let Err(e) = validate_frontmatter(content) {
            return Ok(SkillManageResult::err(format!(
                "Invalid SKILL.md content: {}",
                e
            )).to_json());
        }

        if let Err(e) = detect_injection(content) {
            return Ok(SkillManageResult::err(format!(
                "Security scan failed: {}",
                e
            )).to_json());
        }

        let skill_md = skill_dir.join("SKILL.md");
        let backup = skill_md.with_extension("md.bak");

        // Backup original
        if let Err(e) = fs::copy(&skill_md, &backup) {
            return Ok(SkillManageResult::err(format!("Failed to backup: {}", e)).to_json());
        }

        // Write atomically with rollback
        if let Err(e) = atomic_write(&skill_md, content) {
            let _ = rollback_atomic(&skill_md, &backup);
            return Ok(SkillManageResult::err(format!(
                "Failed to write SKILL.md: {}",
                e
            )).to_json());
        }

        // Clean up backup
        let _ = fs::remove_file(&backup);

        // Invalidate cache
        if let Err(e) = crate::skills::cache::invalidate_skill_cache(&self.loader) {
            tracing::warn!("Failed to invalidate skill cache: {}", e);
        }

        Ok(SkillManageResult::ok(format!(
            "Skill '{}' updated successfully",
            args.name
        )).to_json())
    }

    async fn patch_skill(&self, args: &SkillManageInput, skill_dir: &PathBuf) -> Result<String> {
        let old_string = args
            .old_string
            .as_ref()
            .ok_or_else(|| anyhow!("old_string is required for 'patch'"))?;

        let new_string = args
            .new_string
            .as_ref()
            .ok_or_else(|| anyhow!("new_string is required for 'patch'"))?;

        if !self.skill_exists(skill_dir) {
            return Ok(SkillManageResult::err(format!(
                "Skill '{}' not found",
                args.name
            )).to_json());
        }

        let skill_md = skill_dir.join("SKILL.md");
        let original = match fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(e) => {
                return Ok(SkillManageResult::err(format!(
                    "Failed to read SKILL.md: {}",
                    e
                )).to_json());
            }
        };

        let (result, match_count, strategy) = match fuzzy_find_and_replace(
            &original,
            old_string,
            new_string,
            args.replace_all,
        ) {
            Ok(r) => r,
            Err(e) => {
                return Ok(SkillManageResult::err(format!(
                    "Patch failed: {}",
                    e
                )).to_json());
            }
        };

        if match_count == 0 {
            return Ok(SkillManageResult::err("No matches found".to_string()).to_json());
        }

        // Backup and write with rollback
        let backup = skill_md.with_extension("md.bak");
        let _ = fs::copy(&skill_md, &backup);

        if let Err(e) = atomic_write(&skill_md, &result) {
            let _ = rollback_atomic(&skill_md, &backup);
            return Ok(SkillManageResult::err(format!(
                "Failed to write SKILL.md: {}",
                e
            )).to_json());
        }

        let _ = fs::remove_file(&backup);

        // Validate patched content
        if let Err(e) = validate_frontmatter(&result) {
            // Rollback
            let _ = fs::copy(&backup, &skill_md);
            return Ok(SkillManageResult::err(format!(
                "Patch would break SKILL.md structure: {}",
                e
            )).to_json());
        }

        // Security scan
        if let Err(e) = detect_injection(&result) {
            // Rollback
            let _ = fs::copy(&backup, &skill_md);
            return Ok(SkillManageResult::err(format!(
                "Security scan failed: {}",
                e
            )).to_json());
        }

        // Invalidate cache
        if let Err(e) = crate::skills::cache::invalidate_skill_cache(&self.loader) {
            tracing::warn!("Failed to invalidate skill cache: {}", e);
        }

        let file_target = if let Some(ref fp) = args.file_path {
            format!("'{}' in ", fp)
        } else {
            "SKILL.md in ".to_string()
        };

        Ok(SkillManageResult::ok(format!(
            "Patched {}{}'{}' ({} replacement{}, strategy: {})",
            file_target,
            args.name,
            match_count,
            match_count,
            if match_count > 1 { "s" } else { "" },
            strategy
        )).to_json())
    }

    async fn delete_skill(&self, skill_dir: &PathBuf) -> Result<String> {
        if !skill_dir.exists() {
            return Ok(SkillManageResult::err("Skill directory does not exist".to_string()).to_json());
        }

        let name = skill_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        if let Err(e) = fs::remove_dir_all(skill_dir) {
            return Ok(SkillManageResult::err(format!(
                "Failed to delete skill: {}",
                e
            )).to_json());
        }

        // Invalidate cache
        if let Err(e) = crate::skills::cache::invalidate_skill_cache(&self.loader) {
            tracing::warn!("Failed to invalidate skill cache: {}", e);
        }

        Ok(SkillManageResult::ok(format!(
            "Skill '{}' deleted successfully",
            name
        )).to_json())
    }

    async fn write_file(&self, args: &SkillManageInput, skill_dir: &PathBuf) -> Result<String> {
        let file_path = args
            .file_path
            .as_ref()
            .ok_or_else(|| anyhow!("file_path is required for 'write_file'"))?;

        let file_content = args
            .file_content
            .as_ref()
            .ok_or_else(|| anyhow!("file_content is required for 'write_file'"))?;

        if !self.skill_exists(skill_dir) {
            return Ok(SkillManageResult::err(format!(
                "Skill '{}' not found",
                args.name
            )).to_json());
        }

        if let Err(e) = validate_skill_path(file_path) {
            return Ok(SkillManageResult::err(format!(
                "Invalid file path: {}",
                e
            )).to_json());
        }

        let target = skill_dir.join(file_path);

        // Security: ensure target is within skill_dir
        let canonical_skill = fs::canonicalize(skill_dir)?;
        let canonical_target = fs::canonicalize(&target)?;
        if !canonical_target.starts_with(&canonical_skill) {
            return Ok(SkillManageResult::err("Path escapes skill directory".to_string()).to_json());
        }

        // Ensure parent directory exists
        if let Some(parent) = target.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                return Ok(SkillManageResult::err(format!(
                    "Failed to create directory: {}",
                    e
                )).to_json());
            }
        }

        if let Err(e) = atomic_write(&target, file_content) {
            return Ok(SkillManageResult::err(format!(
                "Failed to write file: {}",
                e
            )).to_json());
        }

        Ok(SkillManageResult::ok_with_path(
            format!("File '{}' written to skill '{}'", file_path, args.name),
            target.to_string_lossy(),
        ).to_json())
    }

    async fn remove_file(&self, args: &SkillManageInput, skill_dir: &PathBuf) -> Result<String> {
        let file_path = args
            .file_path
            .as_ref()
            .ok_or_else(|| anyhow!("file_path is required for 'remove_file'"))?;

        if !self.skill_exists(skill_dir) {
            return Ok(SkillManageResult::err(format!(
                "Skill '{}' not found",
                args.name
            )).to_json());
        }

        if let Err(e) = validate_skill_path(file_path) {
            return Ok(SkillManageResult::err(format!(
                "Invalid file path: {}",
                e
            )).to_json());
        }

        let target = skill_dir.join(file_path);

        // Security check
        let canonical_skill = fs::canonicalize(skill_dir)?;
        let canonical_target = fs::canonicalize(&target)?;
        if !canonical_target.starts_with(&canonical_skill) {
            return Ok(SkillManageResult::err("Path escapes skill directory".to_string()).to_json());
        }

        if !target.exists() {
            return Ok(SkillManageResult::err(format!(
                "File '{}' not found in skill '{}'",
                file_path, args.name
            )).to_json());
        }

        if let Err(e) = fs::remove_file(&target) {
            return Ok(SkillManageResult::err(format!(
                "Failed to remove file: {}",
                e
            )).to_json());
        }

        Ok(SkillManageResult::ok(format!(
            "File '{}' removed from skill '{}'",
            file_path, args.name
        )).to_json())
    }
}

impl SkillManageResult {
    fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| r#"{"success":false,"message":"JSON serialization error"}"#.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_manage_result_ok() {
        let result = SkillManageResult::ok("test");
        assert!(result.success);
        assert_eq!(result.message, "test");
        assert!(result.path.is_none());
    }

    #[test]
    fn test_skill_manage_result_err() {
        let result = SkillManageResult::err("error");
        assert!(!result.success);
        assert_eq!(result.message, "error");
    }

    #[test]
    fn test_skill_manage_result_to_json() {
        let result = SkillManageResult::ok("test");
        let json = result.to_json();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"message\":\"test\""));
    }
}
