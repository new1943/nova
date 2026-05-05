use nova_core::pipeline::{PipelineStage, TurnContext};
use nova_memory::{TopicTracker, TensionTracker, TopicTransition};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// TrackStage — 更新话题和张力追踪器。
pub struct TrackStage {
    topic_tracker: Arc<RwLock<TopicTracker>>,
    tension_tracker: Arc<TensionTracker>,
}

impl TrackStage {
    pub fn new(
        topic_tracker: Arc<RwLock<TopicTracker>>,
        tension_tracker: Arc<TensionTracker>,
    ) -> Self {
        Self { topic_tracker, tension_tracker }
    }
}

#[async_trait::async_trait]
impl PipelineStage for TrackStage {
    fn name(&self) -> &str { "track" }

    async fn execute(&self, ctx: &mut TurnContext) -> anyhow::Result<()> {
        let transition = self.topic_tracker.write().await
            .on_user_message(&ctx.user_input).await;

        match transition {
            TopicTransition::Archive => {
                info!("Track: topic archived via user signal");
                ctx.log_decision("track", "topic_archived", "User signal word triggered archive");
            }
            TopicTransition::NewTopic => {
                info!("Track: new topic started via user signal");
                ctx.log_decision("track", "new_topic", "User signal word triggered new topic");
            }
            _ => {}
        }

        // 更新张力值
        self.tension_tracker.update_from_message(&ctx.user_input).await;
        ctx.tension = self.tension_tracker.current_tension().await;
        ctx.log_decision("track", &format!("tension={}", ctx.tension), "Updated from message");

        Ok(())
    }
}
