//! skill_view tool - progressive disclosure of skill content.

use crate::skills::cache::SharedSkillsLoader;
use crate::skills::security::validate_skill_path;
use crate::tools::registry::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Input for skill_view tool
#[derive(Debug, Clone, Deserialize)]
pub struct SkillViewInput {
    pub name: String,
    #[serde(default)]
    pub file_path: Option<String>,
}

/// Linked files structure
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

/// Tier 1: Full skill view result
#[derive(Debug, Serialize)]
pub struct SkillViewResult {
    pub success: bool,
    pub name: String,
    pub description: String,
    pub content: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linked_files: Option<LinkedFiles>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Tier 2: Support file view result
#[derive(Debug, Serialize)]
pub struct SkillFileViewResult {
    pub success: bool,
    pub name: String,
    pub file: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_binary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct SkillViewTool {
    skills_dir: PathBuf,
    loader: SharedSkillsLoader,
}

impl SkillViewTool {
    pub fn new(skills_dir: PathBuf, loader: SharedSkillsLoader) -> Self {
        Self { skills_dir, loader }
    }
}

#[async_trait]
impl Tool for SkillViewTool {
    fn name(&self) -> &str {
        "skill_view"
    }

    fn description(&self) -> &str {
        "View skill content with progressive disclosure. Tier 1 (name only): returns full SKILL.md + linked files list. Tier 2 (name + file_path): returns specific support file content."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name to view"
                },
                "file_path": {
                    "type": "string",
                    "description": "Optional path to a specific support file: references/<file>, templates/<file>, scripts/<file>, assets/<file>. If omitted, returns full SKILL.md."
                }
            },
            "required": ["name"]
        })
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let args: SkillViewInput = serde_json::from_value(input)
            .map_err(|e| anyhow!("Invalid input: {}", e))?;

        let skill_dir = self.skills_dir.join(&args.name);

        if !skill_dir.exists() {
            let result = SkillFileViewResult {
                success: false,
                name: args.name.clone(),
                file: String::new(),
                content: String::new(),
                is_binary: None,
                error: Some(format!(
                    "Skill '{}' not found. Use skills_list() to see available skills.",
                    args.name
                )),
            };
            return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
        }

        match &args.file_path {
            Some(file_path) => self.view_support_file(&args.name, &skill_dir, file_path).await,
            None => self.view_full_skill(&args.name, &skill_dir).await,
        }
    }
}

impl SkillViewTool {
    async fn view_full_skill(&self, name: &str, skill_dir: &PathBuf) -> Result<String> {
        let skill_md = skill_dir.join("SKILL.md");

        if !skill_md.exists() {
            let result = SkillViewResult {
                success: false,
                name: name.to_string(),
                description: String::new(),
                content: String::new(),
                path: skill_dir.to_string_lossy().to_string(),
                linked_files: None,
                message: None,
                error: Some("SKILL.md not found".to_string()),
            };
            return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
        }

        let content = match fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(e) => {
                let result = SkillViewResult {
                    success: false,
                    name: name.to_string(),
                    description: String::new(),
                    content: String::new(),
                    path: skill_dir.to_string_lossy().to_string(),
                    linked_files: None,
                    message: None,
                    error: Some(format!("Failed to read SKILL.md: {}", e)),
                };
                return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
            }
        };

        // Parse description from frontmatter
        let description = extract_description_from_content(&content);

        // Collect linked files
        let linked_files = self.collect_linked_files(skill_dir);

        let path = skill_md
            .strip_prefix(&self.skills_dir)
            .unwrap_or(&skill_md)
            .to_string_lossy()
            .to_string();

        let result = SkillViewResult {
            success: true,
            name: name.to_string(),
            description,
            content,
            path,
            linked_files: Some(linked_files),
            message: Some("Use skill_view(name, file_path) to view support files".to_string()),
            error: None,
        };

        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    async fn view_support_file(
        &self,
        name: &str,
        skill_dir: &PathBuf,
        file_path: &str,
    ) -> Result<String> {
        // Validate path
        if let Err(e) = validate_skill_path(file_path) {
            let result = SkillFileViewResult {
                success: false,
                name: name.to_string(),
                file: file_path.to_string(),
                content: String::new(),
                is_binary: None,
                error: Some(format!("Invalid file path: {}", e)),
            };
            return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
        }

        let target = skill_dir.join(file_path);

        // Security: ensure target is within skill_dir
        let canonical_skill = match fs::canonicalize(skill_dir) {
            Ok(c) => c,
            Err(e) => {
                let result = SkillFileViewResult {
                    success: false,
                    name: name.to_string(),
                    file: file_path.to_string(),
                    content: String::new(),
                    is_binary: None,
                    error: Some(format!("Failed to resolve skill directory: {}", e)),
                };
                return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
            }
        };

        let canonical_target = match fs::canonicalize(&target) {
            Ok(c) => c,
            Err(_) => {
                // File might not exist - provide helpful error
                let available = self.list_available_files(skill_dir);
                let result = SkillFileViewResult {
                    success: false,
                    name: name.to_string(),
                    file: file_path.to_string(),
                    content: String::new(),
                    is_binary: None,
                    error: Some(format!("File '{}' not found in skill '{}'", file_path, name)),
                };
                return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
            }
        };

        if !canonical_target.starts_with(&canonical_skill) {
            let result = SkillFileViewResult {
                success: false,
                name: name.to_string(),
                file: file_path.to_string(),
                content: String::new(),
                is_binary: None,
                error: Some("Path escapes skill directory".to_string()),
            };
            return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
        }

        if !target.exists() {
            let result = SkillFileViewResult {
                success: false,
                name: name.to_string(),
                file: file_path.to_string(),
                content: String::new(),
                is_binary: None,
                error: Some(format!(
                    "File '{}' not found. Use skills_list() to see available skills.",
                    file_path
                )),
            };
            return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
        }

        // Try to read as text
        match fs::read_to_string(&target) {
            Ok(content) => {
                let result = SkillFileViewResult {
                    success: true,
                    name: name.to_string(),
                    file: file_path.to_string(),
                    content,
                    is_binary: Some(false),
                    error: None,
                };
                Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
            }
            Err(_) => {
                // Binary file
                let size = target.metadata().map(|m| m.len()).unwrap_or(0);
                let result = SkillFileViewResult {
                    success: true,
                    name: name.to_string(),
                    file: file_path.to_string(),
                    content: format!("[Binary file: {}, size: {} bytes]", target.file_name().unwrap_or_default().to_string_lossy(), size),
                    is_binary: Some(true),
                    error: None,
                };
                Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
            }
        }
    }

    fn collect_linked_files(&self, skill_dir: &PathBuf) -> LinkedFiles {
        let mut linked = LinkedFiles {
            references: None,
            templates: None,
            scripts: None,
            assets: None,
        };

        for subdir in &["references", "templates", "scripts", "assets"] {
            let dir = skill_dir.join(subdir);
            if !dir.exists() {
                continue;
            }

            let files: Vec<String> = match fs::read_dir(&dir) {
                Ok(entries) => entries
                    .flatten()
                    .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
                    .filter_map(|e| e.file_name().to_str().map(String::from))
                    .collect(),
                Err(_) => continue,
            };

            if !files.is_empty() {
                match *subdir {
                    "references" => linked.references = Some(files),
                    "templates" => linked.templates = Some(files),
                    "scripts" => linked.scripts = Some(files),
                    "assets" => linked.assets = Some(files),
                    _ => {}
                }
            }
        }

        linked
    }

    fn list_available_files(&self, skill_dir: &PathBuf) -> HashMap<String, Vec<String>> {
        let mut result: HashMap<String, Vec<String>> = HashMap::new();

        for subdir in &["references", "templates", "scripts", "assets"] {
            let dir = skill_dir.join(subdir);
            if !dir.exists() {
                continue;
            }

            let files: Vec<String> = match fs::read_dir(&dir) {
                Ok(entries) => entries
                    .flatten()
                    .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
                    .filter_map(|e| e.file_name().to_str().map(String::from))
                    .map(|f| format!("{}/{}", subdir, f))
                    .collect(),
                Err(_) => continue,
            };

            if !files.is_empty() {
                result.insert(subdir.to_string(), files);
            }
        }

        result
    }
}

fn extract_description_from_content(content: &str) -> String {
    let mut description = String::new();
    let mut in_frontmatter = false;
    let mut frontmatter_end = false;

    for line in content.lines() {
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
                break;
            }
        }
    }

    description
}
