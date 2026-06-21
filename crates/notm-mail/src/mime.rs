use std::{collections::BTreeMap, path::Path};

use mailparse::MailHeaderMap;
use serde::{Deserialize, Serialize};

use crate::html_sanitize::html_to_safe_text;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    pub filename: Option<String>,
    pub content_type: String,
    pub size: usize,
    pub content_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtractedAttachment {
    pub filename: String,
    pub content_type: String,
    pub content_id: Option<String>,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParsedMessage {
    pub headers: BTreeMap<String, String>,
    pub subject: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub reply_to: String,
    pub message_id: String,
    pub references: String,
    pub in_reply_to: String,
    pub text_body: String,
    pub html_body: Option<String>,
    pub safe_body: String,
    pub attachments: Vec<Attachment>,
    pub mime_tree: Vec<String>,
}

pub fn parse_rfc5322(bytes: &[u8]) -> anyhow::Result<ParsedMessage> {
    let parsed = mailparse::parse_mail(bytes)?;
    let headers = parsed
        .headers
        .iter()
        .map(|h| (h.get_key().to_string(), h.get_value()))
        .collect::<BTreeMap<_, _>>();
    let mut text_parts = Vec::new();
    let mut html_parts = Vec::new();
    let mut attachments = Vec::new();
    let mut tree = Vec::new();
    walk_part(
        &parsed,
        0,
        &mut text_parts,
        &mut html_parts,
        &mut attachments,
        &mut tree,
    );
    let text_body = text_parts.join("\n\n");
    let html_body = (!html_parts.is_empty()).then(|| html_parts.join("\n\n"));
    let safe_body = if !text_body.trim().is_empty() {
        text_body.clone()
    } else if let Some(html) = &html_body {
        html_to_safe_text(html)
    } else {
        String::new()
    };
    Ok(ParsedMessage {
        subject: parsed
            .headers
            .get_first_value("Subject")
            .unwrap_or_default(),
        from: parsed.headers.get_first_value("From").unwrap_or_default(),
        to: parsed.headers.get_first_value("To").unwrap_or_default(),
        cc: parsed.headers.get_first_value("Cc").unwrap_or_default(),
        reply_to: parsed
            .headers
            .get_first_value("Reply-To")
            .unwrap_or_default(),
        message_id: parsed
            .headers
            .get_first_value("Message-ID")
            .unwrap_or_default(),
        references: parsed
            .headers
            .get_first_value("References")
            .unwrap_or_default(),
        in_reply_to: parsed
            .headers
            .get_first_value("In-Reply-To")
            .unwrap_or_default(),
        headers,
        text_body,
        html_body,
        safe_body,
        attachments,
        mime_tree: tree,
    })
}

pub fn parse_file(path: impl AsRef<Path>) -> anyhow::Result<ParsedMessage> {
    let bytes = std::fs::read(path)?;
    parse_rfc5322(&bytes)
}

fn walk_part(
    part: &mailparse::ParsedMail<'_>,
    depth: usize,
    text_parts: &mut Vec<String>,
    html_parts: &mut Vec<String>,
    attachments: &mut Vec<Attachment>,
    tree: &mut Vec<String>,
) {
    let mimetype = part.ctype.mimetype.to_lowercase();
    tree.push(format!("{}{}", "  ".repeat(depth), mimetype));
    if !part.subparts.is_empty() {
        for subpart in &part.subparts {
            walk_part(
                subpart,
                depth + 1,
                text_parts,
                html_parts,
                attachments,
                tree,
            );
        }
        return;
    }

    let filename = part.ctype.params.get("name").cloned().or_else(|| {
        part.get_content_disposition()
            .params
            .get("filename")
            .cloned()
    });
    let content_id = part.headers.get_first_value("Content-ID");
    let is_attachment = filename.is_some()
        || part
            .headers
            .get_first_value("Content-Disposition")
            .map(|v| v.to_lowercase().contains("attachment"))
            .unwrap_or(false);

    if is_attachment || (!mimetype.starts_with("text/") && mimetype != "message/rfc822") {
        let size = part.get_body_raw().map(|b| b.len()).unwrap_or_default();
        attachments.push(Attachment {
            filename,
            content_type: mimetype,
            size,
            content_id,
        });
        return;
    }

    match mimetype.as_str() {
        "text/plain" => {
            if let Ok(body) = part.get_body() {
                text_parts.push(body);
            }
        }
        "text/html" => {
            if let Ok(body) = part.get_body() {
                html_parts.push(body);
            }
        }
        _ => {
            if let Ok(body) = part.get_body() {
                text_parts.push(body);
            }
        }
    }
}

pub fn extract_attachments_from_file(
    path: impl AsRef<Path>,
) -> anyhow::Result<Vec<ExtractedAttachment>> {
    let bytes = std::fs::read(path)?;
    extract_attachments(&bytes)
}

pub fn extract_attachments(bytes: &[u8]) -> anyhow::Result<Vec<ExtractedAttachment>> {
    let parsed = mailparse::parse_mail(bytes)?;
    let mut out = Vec::new();
    collect_attachment_data(&parsed, &mut out);
    Ok(out)
}

fn collect_attachment_data(part: &mailparse::ParsedMail<'_>, out: &mut Vec<ExtractedAttachment>) {
    if !part.subparts.is_empty() {
        for subpart in &part.subparts {
            collect_attachment_data(subpart, out);
        }
        return;
    }

    let mimetype = part.ctype.mimetype.to_lowercase();
    let filename = part.ctype.params.get("name").cloned().or_else(|| {
        part.get_content_disposition()
            .params
            .get("filename")
            .cloned()
    });
    let is_attachment = filename.is_some()
        || part
            .headers
            .get_first_value("Content-Disposition")
            .map(|v| v.to_lowercase().contains("attachment"))
            .unwrap_or(false);
    if !is_attachment {
        return;
    }
    let filename = filename.unwrap_or_else(|| "attachment.bin".to_string());
    let bytes = part.get_body_raw().unwrap_or_default();
    out.push(ExtractedAttachment {
        filename,
        content_type: mimetype,
        content_id: part.headers.get_first_value("Content-ID"),
        bytes,
    });
}
