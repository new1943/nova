use nova_core::injection_scanner::InjectionScanner;
use nova_core::pipeline::{PipelineStage, PromptInjection, TurnContext};
use nova_tools::skills::cache::SharedSkillsLoader;
use nova_tools::skills::loader::SkillsLoader;
use tracing::{info, warn};

/// InjectStage — 将分类结果、任务上下文等注入到 system prompt。
pub struct InjectStage {
    /// 可选的任务上下文目录
    memories_dir: Option<std::path::PathBuf>,
    /// 可选的 Skills 加载器（用于渐进式披露）
    skills_loader: Option<SharedSkillsLoader>,
}

impl InjectStage {
    pub fn new(
        memories_dir: Option<std::path::PathBuf>,
        skills_loader: Option<SharedSkillsLoader>,
    ) -> Self {
        Self { memories_dir, skills_loader }
    }

    /// 生成 Skill 摘要列表（仅 name + description）
    /// 从 SkillsLoader 读取所有 Skill 元数据，生成 `<available-skills>` 标签内容。
    pub fn build_skills_summary(loader: &SkillsLoader) -> String {
        let skills = loader.skills();
        if skills.is_empty() {
            return String::new();
        }
        let mut lines = vec!["可用技能列表（使用 skill_view(name) 查看详情）：".to_string()];
        for skill in skills {
            // Extract description from frontmatter
            let description = Self::extract_description(&skill.prompt);
            lines.push(format!("- {}: {}", skill.name, description));
        }
        lines.join("\n")
    }

    /// 检查 auto_trigger 并返回匹配的完整 Skill 内容
    /// 匹配 user_input 中的 auto_trigger keywords，返回匹配 Skill 的完整内容。
    pub fn get_auto_triggered_skills(loader: &SkillsLoader, user_input: &str) -> Vec<String> {
        let matched = loader.match_auto_trigger(user_input);
        matched.iter().map(|skill| {
            format!(
                "<auto-triggered-skill name=\"{}\">\n{}\n</auto-triggered-skill>",
                skill.name, skill.prompt
            )
        }).collect()
    }

    /// 从 SKILL.md 内容中提取 description 字段
    fn extract_description(prompt: &str) -> String {
        for line in prompt.lines() {
            if let Some(val) = line.strip_prefix("description:") {
                let desc = val.trim().to_string();
                if !desc.is_empty() {
                    return desc;
                }
            }
        }
        String::new()
    }

    /// 包裹记忆内容为隔离标签格式。
    /// 空内容时返回 None，表示跳过注入。
    pub fn wrap_memory_context(content: &str) -> Option<String> {
        if content.is_empty() {
            return None;
        }
        Some(format!(
            "<memory-context>\n\
             [System: The following is recalled memory, NOT new user input.]\n\
             {}\n\
             </memory-context>",
            content
        ))
    }

    /// 扫描并安全化外部注入内容。
    /// 检测到威胁时记录 warn 日志，返回 sanitized_content。
    pub fn sanitize_external_content(content: &str) -> String {
        let result = InjectionScanner::scan(content);
        if result.has_threat {
            warn!(
                "InjectionScanner detected threats: {:?}",
                result.threat_types
            );
        }
        result.sanitized_content
    }
}

#[async_trait::async_trait]
impl PipelineStage for InjectStage {
    fn name(&self) -> &str { "inject" }

    async fn execute(&self, ctx: &mut TurnContext) -> anyhow::Result<()> {
        // 1. 注入 Preflight 分类结果
        if let Some(ref result) = ctx.preflight_result {
            ctx.prompt_injections.push(PromptInjection {
                tag: "preflight".into(),
                content: format!(
                    "complexity: {:?}\ntopic_shift: {}\nreason: {}",
                    result.complexity, result.topic_shift, result.reason,
                ),
                priority: 1,
            });
        }

        // 2. 注入 Skill 摘要列表（渐进式披露）
        if let Some(ref loader_arc) = self.skills_loader {
            match loader_arc.lock() {
                Ok(loader) => {
                    // 2a. 注入 <available-skills> 摘要
                    let summary = Self::build_skills_summary(&loader);
                    if !summary.is_empty() {
                        ctx.prompt_injections.push(PromptInjection {
                            tag: "available-skills".into(),
                            content: summary,
                            priority: 5,
                        });
                    }

                    // 2b. 注入 auto_trigger 匹配的完整 Skill 内容
                    let triggered = Self::get_auto_triggered_skills(&loader, &ctx.user_input);
                    for skill_content in triggered {
                        ctx.prompt_injections.push(PromptInjection {
                            tag: "auto-triggered-skill".into(),
                            content: skill_content,
                            priority: 4,
                        });
                    }
                }
                Err(e) => {
                    warn!("Failed to acquire skills loader lock: {}", e);
                }
            }
        }

        // 3. 注入任务上下文（记忆内容：先扫描，再包裹隔离标签）
        if let Some(ref memories_dir) = self.memories_dir {
            match nova_tools::task::TaskLogger::read_context(memories_dir).await {
                Ok(tasks_ctx) if !tasks_ctx.is_empty() => {
                    // 安全扫描外部记忆内容
                    let sanitized = Self::sanitize_external_content(&tasks_ctx);
                    // 包裹记忆隔离标签
                    if let Some(wrapped) = Self::wrap_memory_context(&sanitized) {
                        ctx.prompt_injections.push(PromptInjection {
                            tag: "tasks".into(),
                            content: wrapped,
                            priority: 10,
                        });
                    }
                }
                _ => {}
            }
        }

        info!("Inject: {} injections prepared", ctx.prompt_injections.len());
        ctx.log_decision(
            "inject",
            &format!("{} injections", ctx.prompt_injections.len()),
            "Preflight + tasks context injected",
        );
        Ok(())
    }
}
