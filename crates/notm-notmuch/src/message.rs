use std::fmt;

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
    /// The authoritative tags observed after applying this change.
    #[serde(default)]
    pub tags: Vec<String>,
    /// The authoritative current filenames, including Maildir renames.
    #[serde(default)]
    pub filenames: Vec<String>,
    /// Per-file old-to-current mappings for retained path-bearing models.
    #[serde(default)]
    pub filename_changes: Vec<MaildirFilenameChange>,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TagFailureStage {
    Lookup,
    Freeze,
    RemoveTag,
    AddTag,
    Thaw,
    MaildirFlagSync,
}

impl fmt::Display for TagFailureStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stage = match self {
            Self::Lookup => "lookup",
            Self::Freeze => "freeze",
            Self::RemoveTag => "remove_tag",
            Self::AddTag => "add_tag",
            Self::Thaw => "thaw",
            Self::MaildirFlagSync => "maildir_flag_sync",
        };
        f.write_str(stage)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaildirFlagSyncFailure {
    pub previous_filename: String,
    pub expected_filename: String,
    pub current_filename: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MaildirFilenameChange {
    pub previous_filename: String,
    pub current_filename: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageTagFailure {
    pub message_id: String,
    pub stage: TagFailureStage,
    pub detail: String,
    #[serde(default)]
    pub current_filenames: Vec<String>,
    #[serde(default)]
    pub file_failures: Vec<MaildirFlagSyncFailure>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagBatchReport {
    pub requested_messages: usize,
    pub changed_messages: usize,
    #[serde(default)]
    pub changes: Vec<AppliedTagChange>,
    #[serde(default)]
    pub failures: Vec<MessageTagFailure>,
    /// Errors ending the atomic section or durably closing the database.
    #[serde(default)]
    pub finalization_errors: Vec<String>,
}

impl TagBatchReport {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty() && self.finalization_errors.is_empty()
    }

    pub fn record_finalization_error(&mut self, error: impl fmt::Display) {
        self.finalization_errors.push(error.to_string());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TagOperationReport {
    pub query: String,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub batch: TagBatchReport,
}

impl TagOperationReport {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.batch.is_complete()
    }

    pub fn record_finalization_error(&mut self, error: impl fmt::Display) {
        self.batch.record_finalization_error(error);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadTagReport {
    /// Exact thread IDs supplied by the immutable result snapshot.
    pub thread_ids: Vec<String>,
    #[serde(default)]
    pub missing_thread_ids: Vec<String>,
    pub matched_threads: usize,
    pub changed_threads: usize,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub batch: TagBatchReport,
}

impl ThreadTagReport {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing_thread_ids.is_empty() && self.batch.is_complete()
    }

    pub fn record_finalization_error(&mut self, error: impl fmt::Display) {
        self.batch.record_finalization_error(error);
    }
}
