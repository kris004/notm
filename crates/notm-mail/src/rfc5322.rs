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
    if !message.bcc.is_empty() {
        out.push_str(&format!("Bcc: {}\r\n", message.bcc.join(", ")));
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
    let text_body = rendered_text_body(message);
    let html_body = rendered_html_body(message);
    if message.attachments.is_empty() && html_body.is_none() {
        out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
        out.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
        out.push_str(&normalize_body(&text_body));
    } else if message.attachments.is_empty() {
        let boundary = format!("notm-alt-{}", Uuid::new_v4());
        out.push_str(&format!(
            "Content-Type: multipart/alternative; boundary=\"{}\"\r\n\r\n",
            boundary
        ));
        render_alternative_parts(&mut out, &boundary, &text_body, html_body.as_deref());
    } else {
        let boundary = format!("notm-{}", Uuid::new_v4());
        out.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"{}\"\r\n\r\n",
            boundary
        ));
        out.push_str(&format!("--{}\r\n", boundary));
        if html_body.is_some() {
            let alt_boundary = format!("notm-alt-{}", Uuid::new_v4());
            out.push_str(&format!(
                "Content-Type: multipart/alternative; boundary=\"{}\"\r\n\r\n",
                alt_boundary
            ));
            render_alternative_parts(&mut out, &alt_boundary, &text_body, html_body.as_deref());
        } else {
            out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
            out.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
            out.push_str(&normalize_body(&text_body));
            out.push_str("\r\n");
        }
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

fn rendered_text_body(message: &ComposedMessage) -> String {
    match &message.text_reply_quote {
        Some(quote) => format!("{}{}", message.body, quote),
        None => message.body.clone(),
    }
}

fn rendered_html_body(message: &ComposedMessage) -> Option<String> {
    if let Some(html) = &message.html_body {
        return Some(html.clone());
    }
    message
        .html_reply_quote
        .as_ref()
        .map(|quote| format!("{}{}", plain_text_to_html_fragment(&message.body), quote))
}

fn render_alternative_parts(
    out: &mut String,
    boundary: &str,
    text_body: &str,
    html_body: Option<&str>,
) {
    out.push_str(&format!("--{}\r\n", boundary));
    out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    out.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
    out.push_str(&normalize_body(text_body));
    out.push_str("\r\n");
    if let Some(html) = html_body {
        out.push_str(&format!("--{}\r\n", boundary));
        out.push_str("Content-Type: text/html; charset=utf-8\r\n");
        out.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
        out.push_str(&normalize_body(html));
        out.push_str("\r\n");
    }
    out.push_str(&format!("--{}--\r\n", boundary));
}

fn sanitize_header(value: &str) -> String {
    value.replace(['\r', '\n'], " ")
}

fn normalize_body(body: &str) -> String {
    let mut normalized = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    let _ = chars.next();
                }
                normalized.push_str("\r\n");
            }
            '\n' => normalized.push_str("\r\n"),
            _ => normalized.push(ch),
        }
    }
    normalized
}

fn plain_text_to_html_fragment(body: &str) -> String {
    let mut out = String::from("<div>");
    for (index, line) in body.lines().enumerate() {
        if index > 0 {
            out.push_str("<br>");
        }
        out.push_str(&escape_html(line));
    }
    out.push_str("</div>");
    out
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_message() -> ComposedMessage {
        ComposedMessage::new(
            "Sender <sender@example.test>".to_string(),
            vec!["Visible <visible@example.test>".to_string()],
            "Bcc contract".to_string(),
            "Body".to_string(),
        )
    }

    #[test]
    fn rendered_message_includes_bcc_recipients_for_submission() {
        let mut message = test_message();
        message.bcc = vec![
            "Hidden <hidden@example.test>".to_string(),
            "second@example.test".to_string(),
        ];

        let rendered = render_message(&message);

        assert_eq!(
            rendered
                .matches("Bcc: Hidden <hidden@example.test>, second@example.test\r\n")
                .count(),
            1
        );
    }

    #[test]
    fn rendered_message_omits_empty_bcc_header() {
        let rendered = render_message(&test_message());

        assert!(!rendered.contains("\r\nBcc:"));
    }

    #[test]
    fn normalizes_all_body_line_endings_to_crlf() {
        for (case, input, expected) in [
            ("lf", "first\nsecond\n", "first\r\nsecond\r\n"),
            ("crlf", "first\r\nsecond\r\n", "first\r\nsecond\r\n"),
            ("cr", "first\rsecond\r", "first\r\nsecond\r\n"),
            (
                "mixed",
                "first\r\nsecond\nthird\rfourth",
                "first\r\nsecond\r\nthird\r\nfourth",
            ),
        ] {
            let normalized = normalize_body(input);
            assert_eq!(normalized, expected, "unexpected {case} normalization");
            assert_eq!(
                normalize_body(&normalized),
                normalized,
                "{case} normalization was not idempotent"
            );
        }
    }

    #[test]
    fn rendered_plain_body_does_not_double_existing_crlf() {
        let mut message = test_message();
        message.body = "first\r\nsecond".to_string();

        let rendered = render_message(&message);

        assert!(rendered.ends_with("\r\n\r\nfirst\r\nsecond"));
        assert!(!rendered.contains("\r\r\n"));
    }
}
