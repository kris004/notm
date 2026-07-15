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
    out.push_str(&format!(
        "Message-ID: {}\r\n",
        sanitize_header(&message.message_id)
    ));
    out.push_str(&format!("From: {}\r\n", sanitize_header(&message.from)));
    out.push_str(&format!(
        "To: {}\r\n",
        sanitize_header_values(&message.to, ", ")
    ));
    if !message.cc.is_empty() {
        out.push_str(&format!(
            "Cc: {}\r\n",
            sanitize_header_values(&message.cc, ", ")
        ));
    }
    if !message.bcc.is_empty() {
        out.push_str(&format!(
            "Bcc: {}\r\n",
            sanitize_header_values(&message.bcc, ", ")
        ));
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
        out.push_str(&format!(
            "References: {}\r\n",
            sanitize_header_values(&message.references, " ")
        ));
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
            let content_type_name = render_mime_parameter("name", &attachment.filename);
            let disposition_filename = render_mime_parameter("filename", &attachment.filename);
            out.push_str(&format!("--{}\r\n", boundary));
            out.push_str(&format!(
                "Content-Type: {}; {}\r\n",
                safe_attachment_content_type(&attachment.content_type),
                content_type_name
            ));
            out.push_str("Content-Transfer-Encoding: base64\r\n");
            out.push_str(&format!(
                "Content-Disposition: attachment; {}\r\n\r\n",
                disposition_filename
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

fn sanitize_header_values(values: &[String], separator: &str) -> String {
    values
        .iter()
        .map(|value| sanitize_header(value))
        .collect::<Vec<_>>()
        .join(separator)
}

fn safe_attachment_content_type(content_type: &str) -> &str {
    const FALLBACK: &str = "application/octet-stream";
    let Some((top_level, subtype)) = content_type.split_once('/') else {
        return FALLBACK;
    };
    if top_level.eq_ignore_ascii_case("multipart")
        || subtype.contains('/')
        || !is_mime_token(top_level)
        || !is_mime_token(subtype)
    {
        FALLBACK
    } else {
        content_type
    }
}

fn is_mime_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?=".contains(&byte))
}

fn render_mime_parameter(name: &str, value: &str) -> String {
    let sanitized = sanitize_header(value);
    // RFC 2231 keeps non-ASCII and delimiter-heavy filenames round-trippable.
    if sanitized.is_ascii()
        && !sanitized
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'"' | b'\\' | b';'))
    {
        format!("{name}=\"{sanitized}\"")
    } else {
        format!(
            "{name}*=utf-8''{}",
            percent_encode_mime_parameter(&sanitized)
        )
    }
}

fn percent_encode_mime_parameter(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
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
    use crate::compose::AttachmentInput;

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
    fn rendered_headers_do_not_allow_line_break_injection() {
        let mut message = test_message();
        message.message_id = "<safe@example.test>\r\nX-Injected-Id: yes".to_string();
        message.from = "Sender <sender@example.test>\r\nX-Injected-From: yes".to_string();
        message.to = vec!["visible@example.test\nX-Injected-To: yes".to_string()];
        message.cc = vec!["copy@example.test\rX-Injected-Cc: yes".to_string()];
        message.bcc = vec!["hidden@example.test\r\nX-Injected-Bcc: yes".to_string()];
        message.references = vec![
            "<first@example.test>".to_string(),
            "<second@example.test>\r\nX-Injected-References: yes".to_string(),
        ];
        message.attachments.push(AttachmentInput {
            filename: "note.txt\r\nX-Injected-Filename: yes".to_string(),
            content_type: "text/plain\r\nX-Injected-Content-Type: yes".to_string(),
            bytes: b"attachment".to_vec(),
            source_path: None,
        });

        let rendered = render_message(&message);
        let parsed = mailparse::parse_mail(rendered.as_bytes()).expect("parse rendered message");

        assert!(rendered.contains("Content-Type: application/octet-stream;"));
        assert!(
            parsed
                .headers
                .iter()
                .all(|header| !header.get_key().starts_with("X-Injected-"))
        );
        for injected_header in [
            "X-Injected-Id",
            "X-Injected-From",
            "X-Injected-To",
            "X-Injected-Cc",
            "X-Injected-Bcc",
            "X-Injected-References",
            "X-Injected-Filename",
            "X-Injected-Content-Type",
        ] {
            assert!(
                !rendered.contains(&format!("\r\n{injected_header}:")),
                "rendered an injected {injected_header} header"
            );
        }
    }

    #[test]
    fn attachment_content_type_accepts_only_a_leaf_type_and_subtype() {
        for content_type in [
            "text/plain",
            "application/vnd.example+json",
            "message/rfc822",
            "IMAGE/JPEG",
        ] {
            assert_eq!(safe_attachment_content_type(content_type), content_type);
        }
        for content_type in [
            "",
            "text",
            "/plain",
            "text/",
            " text/plain",
            "text/plain; charset=utf-8",
            "multipart/mixed",
            "text/plain/extra",
            "text/plain\r\nX-Injected: yes",
            "text/☕",
        ] {
            assert_eq!(
                safe_attachment_content_type(content_type),
                "application/octet-stream",
                "accepted unsafe attachment content type {content_type:?}"
            );
        }
    }

    #[test]
    fn rendered_attachment_content_type_cannot_add_mime_parameters_or_structure() {
        let mut message = test_message();
        message.attachments.push(AttachmentInput {
            filename: "payload.bin".to_string(),
            content_type: "multipart/mixed; boundary=attacker-boundary".to_string(),
            bytes: b"attachment".to_vec(),
            source_path: None,
        });

        let rendered = render_message(&message);
        let parsed = mailparse::parse_mail(rendered.as_bytes()).expect("parse rendered message");

        assert!(!rendered.contains("attacker-boundary"));
        assert_eq!(parsed.subparts.len(), 2);
        let attachment = &parsed.subparts[1];
        assert_eq!(attachment.ctype.mimetype, "application/octet-stream");
        assert!(attachment.subparts.is_empty());
    }

    #[test]
    fn rendered_attachment_encodes_special_filename_without_losing_unicode() {
        let mut message = test_message();
        let filename = "résumé \"final\" \\ draft.txt";
        message.attachments.push(AttachmentInput {
            filename: filename.to_string(),
            content_type: "text/plain".to_string(),
            bytes: b"attachment".to_vec(),
            source_path: None,
        });

        let rendered = render_message(&message);

        let encoded_filename = "r%C3%A9sum%C3%A9%20%22final%22%20%5C%20draft.txt";
        assert!(rendered.contains(&format!("name*=utf-8''{encoded_filename}\r\n")));
        assert!(rendered.contains(&format!("filename*=utf-8''{encoded_filename}\r\n")));
        let attachments = crate::mime::extract_attachments(rendered.as_bytes())
            .expect("extract rendered attachment");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].filename, filename);
        assert_eq!(attachments[0].content_type, "text/plain");
        assert_eq!(attachments[0].bytes, b"attachment");
    }

    #[test]
    fn rendered_attachment_preserves_an_ordinary_quoted_filename() {
        let mut message = test_message();
        message.attachments.push(AttachmentInput {
            filename: "meeting notes.txt".to_string(),
            content_type: "text/plain".to_string(),
            bytes: b"attachment".to_vec(),
            source_path: None,
        });

        let rendered = render_message(&message);

        assert!(rendered.contains("name=\"meeting notes.txt\"\r\n"));
        assert!(rendered.contains("filename=\"meeting notes.txt\"\r\n"));
        let attachments = crate::mime::extract_attachments(rendered.as_bytes())
            .expect("extract rendered attachment");
        assert_eq!(attachments[0].filename, "meeting notes.txt");
    }

    #[test]
    fn rendered_message_preserves_valid_address_and_reference_headers() {
        let mut message = test_message();
        message.cc = vec!["Copy <copy@example.test>".to_string()];
        message.bcc = vec!["Hidden <hidden@example.test>".to_string()];
        message.references = vec![
            "<first@example.test>".to_string(),
            "<second@example.test>".to_string(),
        ];

        let rendered = render_message(&message);

        assert!(rendered.contains("From: Sender <sender@example.test>\r\n"));
        assert!(rendered.contains("To: Visible <visible@example.test>\r\n"));
        assert!(rendered.contains("Cc: Copy <copy@example.test>\r\n"));
        assert!(rendered.contains("Bcc: Hidden <hidden@example.test>\r\n"));
        assert!(rendered.contains("References: <first@example.test> <second@example.test>\r\n"));
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
