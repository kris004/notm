use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::rfc5322::{generate_message_id, render_message};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub name: Option<String>,
    pub email: String,
}

impl Identity {
    pub fn formatted(&self) -> String {
        match &self.name {
            Some(name) if !name.is_empty() => format!("{} <{}>", name, self.email),
            _ => self.email.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentInput {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
    pub source_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposedMessage {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<AttachmentInput>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub message_id: String,
}

impl ComposedMessage {
    pub fn new(from: String, to: Vec<String>, subject: String, body: String) -> Self {
        let domain = from
            .split('@')
            .nth(1)
            .map(|s| s.trim_matches('>').to_string());
        Self {
            from,
            to,
            cc: Vec::new(),
            bcc: Vec::new(),
            subject,
            body,
            attachments: Vec::new(),
            in_reply_to: None,
            references: Vec::new(),
            message_id: generate_message_id(domain.as_deref()),
        }
    }

    pub fn to_rfc5322(&self) -> String {
        render_message(self)
    }
}
