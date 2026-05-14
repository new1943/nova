use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::platform::Platform;

/// 任务 ID
pub type TaskId = String;

/// 执行模式 — 由主 Agent 通过工具参数选择
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ExecMode {
    /// 边查边想，每步结果决定下一步。适合调试、调研、探索性任务。
    React,
    /// 固定步骤，顺序执行，无分支。适合流水线任务。
    Chain,
    /// 多个独立子任务同时执行。
    Parallel,
    /// 执行 + 自我审查。生成结果后换视角审视，发现错误则修正。
    WithReview,
    /// 复杂项目协调器。规划→执行→验收→循环，直到所有步骤完成。
    Project,
}

/// 任务状态
#[derive(Debug, Clone)]
pub enum TaskStatus {
    Pending,
    Running { started_at: std::time::Instant },
    Completed { output: String, duration: Duration },
    Failed { error: String, duration: Duration },
    TimedOut { duration: Duration },
    Cancelled,
}

/// 渠道上下文 — spawn 时从主 Agent 透传，通知时原样带回
#[derive(Debug, Clone)]
pub struct ChannelContext {
    pub platform: Platform,
    pub channel_id: String,
    pub reply_to: Option<String>,
}

/// 任务请求 — 传递给 Executor 的执行请求
#[derive(Debug)]
pub struct TaskRequest {
    pub id: TaskId,
    /// 任务名称（给用户看的摘要）
    pub name: String,
    /// 执行模式
    pub mode: ExecMode,
    /// 任务描述（完整 prompt，由主 Agent 生成）
    pub task_prompt: String,
    /// 领域专家系统提示词（由主 Agent 根据任务类型生成）
    pub domain_prompt: String,
    /// execute_chain 专用：步骤列表
    pub steps: Option<Vec<String>>,
    /// execute_with_review 专用：验证提示词（可选，不填用默认对抗性模板）
    pub review_prompt: Option<String>,
    /// execute_project 专用：研究提示词
    pub research_prompt: Option<String>,
    /// execute_project 专用：实现提示词
    pub implementation_prompt: Option<String>,
    /// 工具调用轮数上限（主要限制手段）
    pub max_turns: usize,
    /// 时间兜底（安全网，主要靠 max_turns）
    pub timeout: Duration,
    /// 子 Agent 可用工具列表
    pub tools: Vec<String>,
    /// 渠道上下文
    pub channel: ChannelContext,
}

/// 任务通知 — 注入主 Agent 对话
#[derive(Debug, Clone)]
pub struct Notification {
    pub id: TaskId,
    pub name: String,
    pub tool: String,
    pub status: TaskStatus,
    pub output: Option<String>,
    pub channel: ChannelContext,
    /// execute_project 专用：各阶段信息
    pub phases: Option<Vec<PhaseInfo>>,
}

/// 阶段信息 — execute_project 的四阶段记录
#[derive(Debug, Clone)]
pub struct PhaseInfo {
    pub name: String,
    pub worker_count: usize,
    pub duration: Duration,
    pub verdict: Option<String>,
}

impl Notification {
    /// 格式化为 XML 通知文本，注入主 Agent 对话
    pub fn to_xml(&self) -> String {
        let status_str = match &self.status {
            TaskStatus::Completed { .. } => "completed",
            TaskStatus::Failed { .. } => "failed",
            TaskStatus::TimedOut { .. } => "timed_out",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Pending => "pending",
            TaskStatus::Running { .. } => "running",
        };

        let duration_str = match &self.status {
            TaskStatus::Completed { duration, .. }
            | TaskStatus::Failed { duration, .. }
            | TaskStatus::TimedOut { duration } => {
                format!("<duration>{:.1}s</duration>", duration.as_secs_f64())
            }
            _ => String::new(),
        };

        let result_str = match &self.status {
            TaskStatus::Completed { output, .. } => {
                format!("<result>{}</result>", output)
            }
            TaskStatus::Failed { error, .. } => {
                format!("<result>{}</result>", error)
            }
            TaskStatus::TimedOut { .. } => {
                "<result>任务超时已终止</result>".to_string()
            }
            _ => String::new(),
        };

        let phases_str = if let Some(phases) = &self.phases {
            let inner: Vec<String> = phases
                .iter()
                .map(|p| {
                    let verdict_attr = p
                        .verdict
                        .as_ref()
                        .map(|v| format!(" verdict=\"{}\"", v))
                        .unwrap_or_default();
                    format!(
                        "  <phase name=\"{}\" workers=\"{}\" duration=\"{:.1}s\"{}/>",
                        p.name,
                        p.worker_count,
                        p.duration.as_secs_f64(),
                        verdict_attr
                    )
                })
                .collect();
            format!("<phases>\n{}\n</phases>", inner.join("\n"))
        } else {
            String::new()
        };

        format!(
            "<task-notification>\n\
             <tool>{}</tool>\n\
             <task-id>{}</task-id>\n\
             <name>{}</name>\n\
             <status>{}</status>\n\
             {}\n\
             {}\n\
             {}\n\
             </task-notification>",
            self.tool, self.id, self.name, status_str, duration_str, phases_str, result_str
        )
    }
}

/// 从 XML 通知文本中解析 task-id
pub fn parse_notification_task_id(xml: &str) -> Option<String> {
    let start = xml.find("<task-id>")? + "<task-id>".len();
    let end = xml.find("</task-id>")?;
    Some(xml[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_xml_completed() {
        let n = Notification {
            id: "abc123".into(),
            name: "测试任务".into(),
            tool: "execute_react".into(),
            status: TaskStatus::Completed {
                output: "完成".into(),
                duration: Duration::from_secs_f64(45.2),
            },
            output: Some("完成".into()),
            channel: ChannelContext {
                platform: Platform::Tui,
                channel_id: "test".into(),
                reply_to: None,
            },
            phases: None,
        };
        let xml = n.to_xml();
        assert!(xml.contains("<task-id>abc123</task-id>"));
        assert!(xml.contains("<status>completed</status>"));
        assert!(xml.contains("<duration>45.2s</duration>"));
    }

    #[test]
    fn notification_xml_with_phases() {
        let n = Notification {
            id: "def456".into(),
            name: "重构任务".into(),
            tool: "execute_project".into(),
            status: TaskStatus::Completed {
                output: "done".into(),
                duration: Duration::from_secs_f64(120.5),
            },
            output: Some("done".into()),
            channel: ChannelContext {
                platform: Platform::Discord,
                channel_id: "ch1".into(),
                reply_to: None,
            },
            phases: Some(vec![
                PhaseInfo {
                    name: "research".into(),
                    worker_count: 2,
                    duration: Duration::from_secs_f64(30.2),
                    verdict: None,
                },
                PhaseInfo {
                    name: "verification".into(),
                    worker_count: 1,
                    duration: Duration::from_secs_f64(12.3),
                    verdict: Some("PASS".into()),
                },
            ]),
        };
        let xml = n.to_xml();
        assert!(xml.contains("<phases>"));
        assert!(xml.contains("name=\"research\""));
        assert!(xml.contains("verdict=\"PASS\""));
    }

    #[test]
    fn parse_task_id() {
        let xml = "<task-notification>\n<task-id>abc123</task-id>\n</task-notification>";
        assert_eq!(parse_notification_task_id(xml), Some("abc123".into()));
    }
}
