use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    address::{MailAddress, format_address, parse_one},
    rfc5322::{generate_message_id, render_message},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub name: Option<String>,
    pub email: String,
}

impl Identity {
    pub fn formatted(&self) -> String {
        format_address(&MailAddress {
            name: self.name.clone(),
            email: self.email.clone(),
        })
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
    pub html_body: Option<String>,
    pub text_reply_quote: Option<String>,
    pub html_reply_quote: Option<String>,
    pub attachments: Vec<AttachmentInput>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub message_id: String,
    // These values are generated once with the message rather than during
    // rendering. That makes the bytes submitted to a transport identical to
    // the bytes later persisted in Sent, even when the message is cloned
    // before either operation.
    #[serde(default = "default_render_date")]
    pub(crate) render_date: DateTime<Utc>,
    #[serde(default = "default_mime_boundary_id")]
    pub(crate) mime_boundary_id: Uuid,
}

impl ComposedMessage {
    pub fn new(from: String, to: Vec<String>, subject: String, body: String) -> Self {
        let domain = message_id_domain(&from);
        Self {
            from,
            to,
            cc: Vec::new(),
            bcc: Vec::new(),
            subject,
            body,
            html_body: None,
            text_reply_quote: None,
            html_reply_quote: None,
            attachments: Vec::new(),
            in_reply_to: None,
            references: Vec::new(),
            message_id: generate_message_id(domain.as_deref()),
            render_date: default_render_date(),
            mime_boundary_id: default_mime_boundary_id(),
        }
    }

    pub fn to_rfc5322(&self) -> anyhow::Result<Vec<u8>> {
        render_message(self)
    }
}

fn default_render_date() -> DateTime<Utc> {
    Utc::now()
}

fn default_mime_boundary_id() -> Uuid {
    Uuid::new_v4()
}

fn message_id_domain(from: &str) -> Option<String> {
    let sender = parse_one(from)?;
    sender
        .email
        .rsplit_once('@')
        .map(|(_, domain)| domain.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_id_domain_comes_from_parsed_sender_mailbox() {
        let message = ComposedMessage::new(
            r#""Doe, Jane @ Sales" <jane@example.test>"#.to_string(),
            vec!["recipient@example.test".to_string()],
            "Subject".to_string(),
            "Body".to_string(),
        );

        assert!(message.message_id.ends_with("@example.test>"));
        assert_eq!(message.message_id.matches('@').count(), 1);
    }

    #[test]
    fn invalid_sender_uses_local_message_id_domain() {
        let message = ComposedMessage::new(
            "not an address".to_string(),
            Vec::new(),
            "Subject".to_string(),
            "Body".to_string(),
        );

        assert!(message.message_id.ends_with("@notm.local>"));
    }

    #[test]
    fn identity_quotes_display_names_that_contain_address_delimiters() {
        let identity = Identity {
            name: Some("Doe, Alice".to_string()),
            email: "alice@example.test".to_string(),
        };

        assert_eq!(identity.formatted(), r#""Doe, Alice" <alice@example.test>"#);
    }
}
