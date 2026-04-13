use anyhow::Result;
use std::path::PathBuf;
use tracing::warn;

/// A loaded skill from ~/.nova/skills/<name>/SKILL.md
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub prompt: String,
    pub auto_trigger: Option<AutoTrigger>,
}

/// Auto-trigger rules: activate skill when context matches
#[derive(Debug, Clone)]
pub struct AutoTrigger {
    pub path_patterns: Vec<String>,
    pub keywords: Vec<String>,
}

/// Skills loader — scans skills directory and loads SKILL.md files
pub struct SkillsLoader {
    skills_dir: PathBuf,
    skills: Vec<Skill>,
}

impl SkillsLoader {
    pub fn new(skills_dir: PathBuf) -> Self {
        Self {
            skills_dir,
            skills: Vec::new(),
        }
    }

    /// Load all skills from the skills directory.
    /// Each subdirectory with a SKILL.md is a skill.
    pub fn load_all(&mut self) -> Result<()> {
        self.skills.clear();

        if !self.skills_dir.exists() {
            return Ok(());
        }

        for entry in std::fs::read_dir(&self.skills_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let skill_md = entry.path().join("SKILL.md");
            if !skill_md.exists() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();
            match std::fs::read_to_string(&skill_md) {
                Ok(content) => {
                    let auto_trigger = parse_auto_trigger(&content);
                    self.skills.push(Skill {
                        name,
                        prompt: content,
                        auto_trigger,
                    });
                }
                Err(e) => {
                    warn!("Failed to load skill '{}': {}", name, e);
                }
            }
        }

        Ok(())
    }

    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    /// Find a skill by `/name` command
    pub fn find_by_name(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Find skills whose auto-trigger matches the given context text
    pub fn match_auto_trigger(&self, context: &str) -> Vec<&Skill> {
        self.skills.iter().filter(|s| {
            if let Some(ref trigger) = s.auto_trigger {
                trigger.keywords.iter().any(|kw| context.contains(kw))
            } else {
                false
            }
        }).collect()
    }
}

/// Parse auto-trigger config from SKILL.md frontmatter.
/// Looks for lines like:
///   auto_trigger_keywords: keyword1, keyword2
///   auto_trigger_paths: *.rs, src/**
fn parse_auto_trigger(content: &str) -> Option<AutoTrigger> {
    let mut keywords = Vec::new();
    let mut paths = Vec::new();

    for line in content.lines() {
        if let Some(kw) = line.strip_prefix("auto_trigger_keywords:") {
            keywords = kw.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        }
        if let Some(p) = line.strip_prefix("auto_trigger_paths:") {
            paths = p.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        }
    }

    if keywords.is_empty() && paths.is_empty() {
        None
    } else {
        Some(AutoTrigger {
            path_patterns: paths,
            keywords,
        })
    }
}
