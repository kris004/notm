use chrono::Utc;
use uuid::Uuid;

use crate::compose::ComposedMessage;

pub fn normalize_subject_for_reply(subject: &str) -> String {
    let trimmed = subject.trim();
    if trimmed.to_lowercase().starts_with("re:") {
        trimmed.to_string()
    } else {
        format!("Re: {trimmed}")
    }
}

pub fn normalize_subject_for_forward(subject: &str) -> String {
    let trimmed = subject.trim();
    if trimmed.to_lowercase().starts_with("fwd:") {
        trimmed.to_string()
    } else {
        format!("Fwd: {trimmed}")
    }
}

pub fn generate_message_id(domain: Option<&str>) -> String {
    let domain = domain.unwrap_or("notm.local");
    format!("<{}@{}>", Uuid::new_v4(), domain)
}

pub fn render_message(message: &ComposedMessage) -> String {
    let mut out = String::new();
    out.push_str(&format!("Date: {}\r\n", Utc::now().to_rfc2822()));
    out.push_str(&format!("Message-ID: {}\r\n", message.message_id));
    out.push_str(&format!("From: {}\r\n", message.from));
    out.push_str(&format!("To: {}\r\n", message.to.join(", ")));
    if !message.cc.is_empty() {
        out.push_str(&format!("Cc: {}\r\n", message.cc.join(", ")));
    }
    out.push_str(&format!(
        "Subject: {}\r\n",
        sanitize_header(&message.subject)
    ));
    if let Some(in_reply_to) = &message.in_reply_to {
        out.push_str(&format!(
            "In-Reply-To: {}\r\n",
            sanitize_header(in_reply_to)
        ));
    }
    if !message.references.is_empty() {
        out.push_str(&format!("References: {}\r\n", message.references.join(" ")));
    }
    out.push_str("MIME-Version: 1.0\r\n");
    if message.attachments.is_empty() {
        out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
        out.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
        out.push_str(&normalize_body(&message.body));
    } else {
        let boundary = format!("notm-{}", Uuid::new_v4());
        out.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"{}\"\r\n\r\n",
            boundary
        ));
        out.push_str(&format!("--{}\r\n", boundary));
        out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
        out.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
        out.push_str(&normalize_body(&message.body));
        out.push_str("\r\n");
        for attachment in &message.attachments {
            out.push_str(&format!("--{}\r\n", boundary));
            out.push_str(&format!(
                "Content-Type: {}; name=\"{}\"\r\n",
                attachment.content_type,
                sanitize_header(&attachment.filename)
            ));
            out.push_str("Content-Transfer-Encoding: base64\r\n");
            out.push_str(&format!(
                "Content-Disposition: attachment; filename=\"{}\"\r\n\r\n",
                sanitize_header(&attachment.filename)
            ));
            use base64::Engine as _;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&attachment.bytes);
            for chunk in encoded.as_bytes().chunks(76) {
                out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
                out.push_str("\r\n");
            }
        }
        out.push_str(&format!("--{}--\r\n", boundary));
    }
    out
}

fn sanitize_header(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

fn normalize_body(body: &str) -> String {
    body.replace('\n', "\r\n")
}
