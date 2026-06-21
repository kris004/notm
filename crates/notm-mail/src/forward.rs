use crate::{
    compose::{AttachmentInput, ComposedMessage, Identity},
    mime::ParsedMessage,
    rfc5322::normalize_subject_for_forward,
};

pub fn build_inline_forward(original: &ParsedMessage, identity: &Identity) -> ComposedMessage {
    let body = format!(
        "\n\n---------- Forwarded message ---------\nFrom: {}\nTo: {}\nSubject: {}\n\n{}",
        original.from, original.to, original.subject, original.safe_body
    );
    ComposedMessage::new(
        identity.formatted(),
        Vec::new(),
        normalize_subject_for_forward(&original.subject),
        body,
    )
}

pub fn build_attachment_forward(
    original: &ParsedMessage,
    identity: &Identity,
    raw_message: Vec<u8>,
) -> ComposedMessage {
    let mut message = ComposedMessage::new(
        identity.formatted(),
        Vec::new(),
        normalize_subject_for_forward(&original.subject),
        format!(
            "\n\nThe original message is attached.\n\nFrom: {}\nTo: {}\nSubject: {}\n",
            original.from, original.to, original.subject
        ),
    );
    message.attachments.push(AttachmentInput {
        filename: forwarded_message_filename(&original.subject),
        content_type: "message/rfc822".to_string(),
        bytes: raw_message,
        source_path: None,
    });
    message
}

fn forwarded_message_filename(subject: &str) -> String {
    let mut slug = subject
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "forwarded-message.eml".to_string()
    } else {
        format!(
            "forwarded-{}.eml",
            slug.chars().take(48).collect::<String>()
        )
    }
}
