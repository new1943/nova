use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailMessage {
    pub from: String,
    pub to: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

pub struct Mailbox {
    inboxes: HashMap<String, VecDeque<MailMessage>>,
}

impl Default for Mailbox {
    fn default() -> Self { Self::new() }
}

impl Mailbox {
    pub fn new() -> Self { Self { inboxes: HashMap::new() } }

    pub fn send(&mut self, msg: MailMessage) {
        self.inboxes.entry(msg.to.clone()).or_default().push_back(msg);
    }

    pub fn recv(&mut self, agent_name: &str) -> Option<MailMessage> {
        self.inboxes.get_mut(agent_name)?.pop_front()
    }

    pub fn pending_count(&self, agent_name: &str) -> usize {
        self.inboxes.get(agent_name).map_or(0, |q| q.len())
    }

    pub fn drain(&mut self, agent_name: &str) -> Vec<MailMessage> {
        self.inboxes.get_mut(agent_name).map(|q| q.drain(..).collect()).unwrap_or_default()
    }
}
