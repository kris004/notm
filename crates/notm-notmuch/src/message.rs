use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageSummary {
    pub message_id: String,
    pub thread_id: String,
    pub date: i64,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub tags: Vec<String>,
    pub filenames: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagMutation {
    pub add: Vec<String>,
    pub remove: Vec<String>,
    pub sync_maildir_flags: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageTagMutation {
    pub message_id: String,
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedTagChange {
    pub message_id: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

impl AppliedTagChange {
    pub fn inverse(&self) -> MessageTagMutation {
        MessageTagMutation {
            message_id: self.message_id.clone(),
            add: self.removed.clone(),
            remove: self.added.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagOperationReport {
    pub query: String,
    pub changed_messages: usize,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changes: Vec<AppliedTagChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadRangeTagReport {
    pub query: String,
    pub start: usize,
    pub end: usize,
    pub changed_threads: usize,
    pub changed_messages: usize,
    pub revision_before: u64,
    pub revision_after: u64,
    pub revision_uuid: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changes: Vec<AppliedTagChange>,
}
