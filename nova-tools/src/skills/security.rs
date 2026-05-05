use anyhow::Result;
use regex::Regex;
use std::fs;
use std::io::Write;
use std::path::Path;

const ALLOWED_SUPPORT_DIRS: &[&str] = &["references", "templates", "scripts", "assets"];

pub const MAX_SKILL_NAME_CHARS: usize = 64;
pub const MAX_SKILL_DESCRIPTION_CHARS: usize = 1024;
pub const MAX_SKILL_CONTENT_CHARS: usize = 100_000;
pub const MAX_SUPPORT_FILE_CHARS: usize = 1_048_576;

const VALID_NAME_RE: &str = r"^[a-z0-9][a-z0-9._-]*$";

pub fn has_path_traversal(path: &str) -> bool {
    Path::new(path).components().any(|c| c.as_os_str() == "..")
}

pub fn validate_skill_path(file_path: &str) -> Result<()> {
    if has_path_traversal(file_path) { anyhow::bail!("Path traversal not allowed: {}", file_path); }
    let clean_path = file_path.trim_start_matches("./");
    let components: Vec<&str> = clean_path.split('/').collect();
    if components.len() > 1 {
        let first_dir = components[0];
        if !ALLOWED_SUPPORT_DIRS.contains(&first_dir) {
            anyhow::bail!("File path must be in allowed directories: {:?}, got '{}'", ALLOWED_SUPPORT_DIRS, first_dir);
        }
    } else if components.len() == 1 && !components[0].is_empty() {
        anyhow::bail!("File path must be under a subdirectory: references/, templates/, scripts/, or assets/");
    }
    Ok(())
}

pub fn validate_skill_name(name: &str) -> Result<()> {
    if name.is_empty() { anyhow::bail!("Skill name is required"); }
    if name.len() > MAX_SKILL_NAME_CHARS { anyhow::bail!("Skill name exceeds {} characters", MAX_SKILL_NAME_CHARS); }
    let re = Regex::new(VALID_NAME_RE).unwrap();
    if !re.is_match(name) { anyhow::bail!("Invalid skill name '{}'", name); }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub platforms: Option<Vec<String>>,
}

pub fn validate_frontmatter(content: &str) -> Result<SkillFrontmatter> {
    if !content.starts_with("---") { anyhow::bail!("SKILL.md must start with YAML frontmatter (---)"); }
    if content.len() > MAX_SKILL_CONTENT_CHARS { anyhow::bail!("SKILL.md content exceeds {} chars", MAX_SKILL_CONTENT_CHARS); }
    let frontmatter = extract_frontmatter(content)?;
    validate_skill_name(&frontmatter.name)?;
    if frontmatter.description.len() > MAX_SKILL_DESCRIPTION_CHARS {
        anyhow::bail!("Description exceeds {} chars", MAX_SKILL_DESCRIPTION_CHARS);
    }
    Ok(frontmatter)
}

fn extract_frontmatter(content: &str) -> Result<SkillFrontmatter> {
    let mut lines = content.lines();
    if lines.next() != Some("---") { anyhow::bail!("Missing opening ---"); }
    let mut name = String::new();
    let mut description = String::new();
    let mut version = None;
    let mut platforms = None;
    for line in lines {
        if line == "---" { break; }
        if let Some(val) = line.strip_prefix("name:") { name = val.trim().to_string(); }
        else if let Some(val) = line.strip_prefix("description:") { description = val.trim().to_string(); }
        else if let Some(val) = line.strip_prefix("version:") { version = Some(val.trim().to_string()); }
        else if let Some(val) = line.strip_prefix("platforms:") {
            let val = val.trim();
            if val.starts_with('[') && val.ends_with(']') {
                let inner = &val[1..val.len() - 1];
                platforms = Some(inner.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());
            }
        }
    }
    if name.is_empty() { anyhow::bail!("Frontmatter must have 'name' field"); }
    if description.is_empty() { anyhow::bail!("Frontmatter must have 'description' field"); }
    Ok(SkillFrontmatter { name, description, version, platforms })
}

pub fn detect_injection(content: &str) -> Result<()> {
    let patterns: Vec<(Regex, &str)> = vec![
        (Regex::new(r"(?i)ignore\s+(previous|all)\s+instructions").unwrap(), "Ignore previous instructions"),
        (Regex::new(r"(?i)you\s+are\s+now\s+").unwrap(), "Role override"),
        (Regex::new(r"<system>").unwrap(), "System tag injection"),
        (Regex::new(r"]]>").unwrap(), "XML EOF injection"),
        (Regex::new(r"(?i)disregard\s+(all|your)\s+(previous|prior)").unwrap(), "Disregard instructions"),
        (Regex::new(r"(?i)new\s+instructions?:").unwrap(), "New instructions override"),
        (Regex::new(r"(?i)forget\s+your\s+instructions").unwrap(), "Forget instructions"),
    ];
    let content_lower = content.to_lowercase();
    for (re, description) in patterns {
        if re.is_match(&content_lower) {
            anyhow::bail!("Potential injection detected: {}", description);
        }
    }
    Ok(())
}

pub fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let temp_path = parent.join(format!(".{}.tmp", path.file_name().unwrap_or_default().to_string_lossy()));
    { let mut file = fs::File::create(&temp_path)?; file.write_all(content.as_bytes())?; }
    fs::rename(&temp_path, path).inspect_err(|_e| { let _ = fs::remove_file(&temp_path); })?;
    Ok(())
}

pub fn rollback_atomic(path: &Path, backup: &Path) -> Result<()> {
    if backup.exists() { fs::copy(backup, path)?; fs::remove_file(backup)?; }
    Ok(())
}
