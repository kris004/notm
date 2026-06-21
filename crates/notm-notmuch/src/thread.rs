use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadSummary {
    pub thread_id: String,
    pub subject: String,
    pub authors: String,
    pub oldest_date: i64,
    pub newest_date: i64,
    pub matched_messages: i32,
    pub total_messages: i32,
    pub tags: Vec<String>,
    pub has_unread: bool,
    pub is_flagged: bool,
}
