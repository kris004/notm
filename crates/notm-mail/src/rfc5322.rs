use anyhow::{Context, ensure};
use base64::Engine as _;
use uuid::Uuid;

use crate::{
    address::{parse_one_checked, quote_name},
    compose::{AttachmentInput, ComposedMessage},
};

const RECOMMENDED_HEADER_LINE_LENGTH: usize = 78;
const MAX_WIRE_LINE_LENGTH: usize = 998;
// The RFC 2047 wrapper plus Base64 expansion makes 39 input bytes an encoded
// word no longer than 64 characters. This keeps even the first Subject line
// within RFC 2047's stricter 76-character limit for encoded-word headers.
const ENCODED_WORD_INPUT_BYTES: usize = 39;
const MIME_PARAMETER_SEGMENT_LENGTH: usize = 42;

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

/// Render a complete RFC 5322 message suitable for submission to a transport.
///
/// Rendering is deliberately fallible: user-controlled header values are
/// parsed or rejected rather than repaired, so invalid input cannot become a
/// different message or smuggle an additional header onto the wire.
pub fn render_message(message: &ComposedMessage) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();

    write_token_header(&mut out, "Date", &[message.render_date.to_rfc2822()])?;
    let message_ids = message_id_tokens("Message-ID", std::slice::from_ref(&message.message_id))?;
    ensure!(
        message_ids.len() == 1,
        "Message-ID must contain exactly one message identifier"
    );
    write_token_header(&mut out, "Message-ID", &message_ids)?;
    write_address_header(&mut out, "From", std::slice::from_ref(&message.from), false)?;
    if !message.to.is_empty() {
        write_address_header(&mut out, "To", &message.to, true)?;
    }
    if !message.cc.is_empty() {
        write_address_header(&mut out, "Cc", &message.cc, true)?;
    }
    // This is the intentional pre-helper contract: a sendmail-style helper
    // reads Bcc for envelope recipients and strips it before final delivery.
    if !message.bcc.is_empty() {
        write_address_header(&mut out, "Bcc", &message.bcc, true)?;
    }
    write_subject_header(&mut out, &message.subject)?;
    if let Some(in_reply_to) = &message.in_reply_to {
        let ids = message_id_tokens("In-Reply-To", std::slice::from_ref(in_reply_to))?;
        ensure!(
            !ids.is_empty(),
            "In-Reply-To must contain a message identifier"
        );
        write_token_header(&mut out, "In-Reply-To", &ids)?;
    }
    if !message.references.is_empty() {
        let ids = message_id_tokens("References", &message.references)?;
        ensure!(
            !ids.is_empty(),
            "References must contain a message identifier"
        );
        write_token_header(&mut out, "References", &ids)?;
    }
    write_literal_header(&mut out, "MIME-Version", "1.0")?;

    let text_body = rendered_text_body(message);
    let html_body = rendered_html_body(message);
    if message.attachments.is_empty() && html_body.is_none() {
        write_text_part_headers(&mut out, "text/plain")?;
        out.extend_from_slice(b"\r\n");
        write_quoted_printable_body(&mut out, &text_body);
    } else if message.attachments.is_empty() {
        let boundary = alternative_boundary(message);
        write_multipart_header(&mut out, "multipart/alternative", &boundary)?;
        out.extend_from_slice(b"\r\n");
        render_alternative_parts(&mut out, &boundary, &text_body, html_body.as_deref())?;
    } else {
        let boundary = mixed_boundary(message);
        write_multipart_header(&mut out, "multipart/mixed", &boundary)?;
        out.extend_from_slice(b"\r\n");
        write_boundary(&mut out, &boundary, false);
        if html_body.is_some() {
            let alternative = alternative_boundary(message);
            write_multipart_header(&mut out, "multipart/alternative", &alternative)?;
            out.extend_from_slice(b"\r\n");
            render_alternative_parts(&mut out, &alternative, &text_body, html_body.as_deref())?;
        } else {
            write_text_part_headers(&mut out, "text/plain")?;
            out.extend_from_slice(b"\r\n");
            write_quoted_printable_body(&mut out, &text_body);
        }
        for attachment in &message.attachments {
            write_boundary(&mut out, &boundary, false);
            render_attachment(&mut out, attachment)?;
        }
        write_boundary(&mut out, &boundary, true);
    }

    validate_wire(&out)?;
    Ok(out)
}

fn write_subject_header(out: &mut Vec<u8>, subject: &str) -> anyhow::Result<()> {
    validate_header_text("Subject", subject)?;
    let tokens = if !subject.is_ascii()
        || "Subject: ".len().saturating_add(subject.len()) > RECOMMENDED_HEADER_LINE_LENGTH
    {
        encode_words(subject)
    } else {
        vec![subject.to_string()]
    };
    write_token_header(out, "Subject", &tokens)
}

fn write_address_header(
    out: &mut Vec<u8>,
    field: &str,
    values: &[String],
    allow_empty: bool,
) -> anyhow::Result<()> {
    ensure!(
        allow_empty || values.len() == 1,
        "{field} requires one mailbox"
    );
    let mut tokens = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let mut mailbox = render_mailbox_tokens(field, value)
            .with_context(|| format!("invalid {field} mailbox {}", index + 1))?;
        if index + 1 != values.len() {
            mailbox
                .last_mut()
                .expect("a rendered mailbox always has a token")
                .push(',');
        }
        tokens.extend(mailbox);
    }
    write_token_header(out, field, &tokens)
}

fn render_mailbox_tokens(field: &str, input: &str) -> anyhow::Result<Vec<String>> {
    validate_header_text(field, input)?;
    let address = parse_one_checked(input)?;
    validate_header_text(field, &address.email)?;
    let mut tokens = Vec::new();
    if let Some(name) = address.name.as_deref().filter(|name| !name.is_empty()) {
        validate_header_text(field, name)?;
        if !name.is_ascii() || name.len() > ENCODED_WORD_INPUT_BYTES {
            tokens.extend(encode_words(name));
        } else {
            tokens.push(quote_name(name));
        }
        tokens.push(format!("<{}>", address.email));
    } else {
        tokens.push(address.email);
    }
    Ok(tokens)
}

fn encode_words(value: &str) -> Vec<String> {
    if value.is_empty() {
        return vec![String::new()];
    }
    utf8_chunks(value, ENCODED_WORD_INPUT_BYTES)
        .into_iter()
        .map(|chunk| {
            format!(
                "=?UTF-8?B?{}?=",
                base64::engine::general_purpose::STANDARD.encode(chunk.as_bytes())
            )
        })
        .collect()
}

fn utf8_chunks(value: &str, max_bytes: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut bytes = 0;
    for (offset, character) in value.char_indices() {
        let character_bytes = character.len_utf8();
        if bytes != 0 && bytes + character_bytes > max_bytes {
            chunks.push(&value[start..offset]);
            start = offset;
            bytes = 0;
        }
        bytes += character_bytes;
    }
    chunks.push(&value[start..]);
    chunks
}

fn message_id_tokens(field: &str, values: &[String]) -> anyhow::Result<Vec<String>> {
    let mut tokens = Vec::new();
    let allow_obsolete_threading_syntax = matches!(field, "In-Reply-To" | "References");
    for value in values {
        validate_header_text(field, value)?;
        tokens.extend(parse_message_id_sequence(
            field,
            value,
            allow_obsolete_threading_syntax,
        )?);
    }
    Ok(tokens)
}

fn parse_message_id_sequence(
    field: &str,
    value: &str,
    allow_obsolete_threading_syntax: bool,
) -> anyhow::Result<Vec<String>> {
    let bytes = value.as_bytes();
    let mut index = 0;
    let mut ids = Vec::new();
    while index < bytes.len() {
        index = skip_message_id_cfws(field, value, index)?;
        if index == bytes.len() {
            break;
        }
        if bytes[index] != b'<' {
            ensure!(
                allow_obsolete_threading_syntax,
                "{field} contains text outside a message identifier near {:?}",
                &value[index..]
            );
            // RFC 5322's obsolete In-Reply-To and References syntax is
            // *(phrase / msg-id). Phrases are semantically ignored, but
            // still need to be parsed deliberately so arbitrary punctuation
            // cannot be mistaken for harmless legacy text.
            index = parse_obsolete_threading_phrase(field, value, index)?;
            continue;
        }
        let end = find_message_id_end(field, value, index + 1)?;
        let id = &value[index..=end];
        ids.push(canonical_message_id_core(
            field,
            id,
            allow_obsolete_threading_syntax,
        )?);
        index = end + 1;
    }
    Ok(ids)
}

fn parse_obsolete_threading_phrase(
    field: &str,
    value: &str,
    mut index: usize,
) -> anyhow::Result<usize> {
    let bytes = value.as_bytes();
    index = parse_obsolete_threading_word(field, value, index)?;
    loop {
        index = skip_message_id_cfws(field, value, index)?;
        let Some(&byte) = bytes.get(index) else {
            return Ok(index);
        };
        if byte == b'<' {
            return Ok(index);
        }
        if byte == b'.' {
            index += 1;
            continue;
        }
        index = parse_obsolete_threading_word(field, value, index)?;
    }
}

fn parse_obsolete_threading_word(field: &str, value: &str, index: usize) -> anyhow::Result<usize> {
    let bytes = value.as_bytes();
    if bytes.get(index..index + 2) == Some(b"=?") {
        return parse_obsolete_threading_encoded_word(field, value, index);
    }
    if bytes.get(index) == Some(&b'"') {
        return parse_obsolete_threading_quoted_word(value, index).ok_or_else(|| {
            anyhow::anyhow!(
                "{field} contains an invalid or unterminated quoted phrase near {:?}",
                &value[index..]
            )
        });
    }
    parse_obsolete_threading_atom(value, index).ok_or_else(|| {
        anyhow::anyhow!(
            "{field} contains an invalid obsolete phrase token near {:?}",
            &value[index..]
        )
    })
}

fn parse_obsolete_threading_encoded_word(
    field: &str,
    value: &str,
    index: usize,
) -> anyhow::Result<usize> {
    let bytes = value.as_bytes();
    ensure!(
        index == 0
            || bytes[..index]
                .last()
                .is_some_and(|byte| matches!(byte, b' ' | b'\t')),
        "{field} contains text adjacent to an RFC 2047 encoded-word near {:?}",
        &value[index..]
    );
    let encoded = &bytes[index..];
    let charset_end = encoded[2..]
        .iter()
        .position(|byte| *byte == b'?')
        .map(|offset| index + 2 + offset)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{field} contains an unterminated RFC 2047 encoded-word near {:?}",
                &value[index..]
            )
        })?;
    let charset = &bytes[index + 2..charset_end];
    ensure!(
        valid_rfc2047_token(charset),
        "{field} contains an invalid RFC 2047 charset near {:?}",
        &value[index..]
    );

    let encoding_start = charset_end + 1;
    let encoding_end = bytes[encoding_start..]
        .iter()
        .position(|byte| *byte == b'?')
        .map(|offset| encoding_start + offset)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{field} contains an incomplete RFC 2047 encoded-word near {:?}",
                &value[index..]
            )
        })?;
    let encoding = &bytes[encoding_start..encoding_end];
    ensure!(
        valid_rfc2047_token(encoding),
        "{field} contains an invalid RFC 2047 encoding near {:?}",
        &value[index..]
    );

    let text_start = encoding_end + 1;
    let text_end = bytes[text_start..]
        .iter()
        .position(|byte| *byte == b'?')
        .map(|offset| text_start + offset)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{field} contains an unterminated RFC 2047 encoded-word near {:?}",
                &value[index..]
            )
        })?;
    ensure!(
        bytes.get(text_end + 1) == Some(&b'=') && text_end != text_start,
        "{field} contains an invalid RFC 2047 encoded-text near {:?}",
        &value[index..]
    );
    let text = &bytes[text_start..text_end];
    ensure!(
        text.iter()
            .all(|byte| byte.is_ascii_graphic() && *byte != b'?'),
        "{field} contains an invalid RFC 2047 encoded-text near {:?}",
        &value[index..]
    );
    let end = text_end + 2;
    ensure!(
        end - index <= 75,
        "{field} contains an RFC 2047 encoded-word longer than 75 characters"
    );

    // RFC 2047 sections 6.1 and 6.3 tell readers to recognize the section 2
    // encoded-word syntax before decoding and not to block handling merely
    // because its B/Q payload is malformed. We never decode phrase words
    // here, so syntax-valid payloads stay opaque even when their encoding is
    // unsupported, incorrectly formed, or violates composer-side placement
    // restrictions.
    if let Some(&next) = bytes.get(end) {
        ensure!(
            matches!(next, b' ' | b'\t'),
            "{field} contains text adjacent to an RFC 2047 encoded-word near {:?}",
            &value[index..]
        );
    }
    Ok(end)
}

fn valid_rfc2047_token(value: &[u8]) -> bool {
    !value.is_empty()
        && value
            .iter()
            .all(|byte| byte.is_ascii_graphic() && !b"()<>@,;:\"/[]?.=".contains(byte))
}

fn parse_obsolete_threading_atom(value: &str, index: usize) -> Option<usize> {
    // Keep IDs on the ASCII atext parser. Raw RFC 6532 Unicode is accepted
    // only in ignored legacy phrase words; RFC 2047 encoded-words take the
    // dedicated opaque path above and are never decoded into syntax.
    let mut end = index;
    for (offset, character) in value[index..].char_indices() {
        if character.is_ascii() {
            if !is_atext(character as u8) {
                break;
            }
        } else if character.is_control() {
            return None;
        }
        end = index + offset + character.len_utf8();
    }
    (end != index).then_some(end)
}

fn parse_obsolete_threading_quoted_word(value: &str, mut index: usize) -> Option<usize> {
    index += 1;
    while let Some(&byte) = value.as_bytes().get(index) {
        match byte {
            b'\\' => {
                index += 1;
                let &escaped = value.as_bytes().get(index)?;
                if !matches!(escaped, b' ' | b'\t') && !escaped.is_ascii_graphic() {
                    return None;
                }
                index += 1;
            }
            b'"' => return Some(index + 1),
            b' ' | b'\t' | b'!' | b'#'..=b'[' | b']'..=b'~' => index += 1,
            byte if byte.is_ascii() => return None,
            _ => {
                // RFC 6532 permits raw Unicode header text. It remains
                // semantically ignored in a legacy phrase; IDs are ASCII-only.
                let character = value[index..].chars().next()?;
                if character.is_control() {
                    return None;
                }
                index += character.len_utf8();
            }
        }
    }
    None
}

fn skip_message_id_cfws(field: &str, value: &str, mut index: usize) -> anyhow::Result<usize> {
    let bytes = value.as_bytes();
    loop {
        while bytes
            .get(index)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            index += 1;
        }
        if bytes.get(index) != Some(&b'(') {
            return Ok(index);
        }
        let mut depth = 1usize;
        index += 1;
        while depth != 0 {
            let Some(&byte) = bytes.get(index) else {
                anyhow::bail!("{field} contains an unterminated comment");
            };
            ensure!(
                byte.is_ascii() && (matches!(byte, b' ' | b'\t') || byte.is_ascii_graphic()),
                "{field} contains an invalid character in a comment"
            );
            match byte {
                b'\\' => {
                    index += 1;
                    let Some(&escaped) = bytes.get(index) else {
                        anyhow::bail!("{field} contains an incomplete quoted pair in a comment");
                    };
                    ensure!(
                        matches!(escaped, b' ' | b'\t') || escaped.is_ascii_graphic(),
                        "{field} contains an invalid quoted pair in a comment"
                    );
                }
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            index += 1;
        }
    }
}

fn find_message_id_end(field: &str, value: &str, mut index: usize) -> anyhow::Result<usize> {
    let bytes = value.as_bytes();
    let mut quoted = false;
    let mut literal = false;
    let mut escaped = false;
    let mut comment_depth = 0usize;
    while let Some(&byte) = bytes.get(index) {
        if comment_depth != 0 {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'(' {
                comment_depth += 1;
            } else if byte == b')' {
                comment_depth -= 1;
            }
        } else if escaped {
            escaped = false;
        } else if (quoted || literal) && byte == b'\\' {
            escaped = true;
        } else if !literal && byte == b'"' {
            quoted = !quoted;
        } else if !quoted && byte == b'[' {
            literal = true;
        } else if literal && byte == b']' {
            literal = false;
        } else if !quoted && !literal && byte == b'(' {
            comment_depth = 1;
        } else if !quoted && !literal && byte == b'>' {
            return Ok(index);
        }
        index += 1;
    }
    anyhow::bail!("{field} contains an unterminated message identifier")
}

fn canonical_message_id_core(
    field: &str,
    value: &str,
    allow_obsolete_internal_cfws: bool,
) -> anyhow::Result<String> {
    ensure!(
        value.is_ascii() && value.starts_with('<') && value.ends_with('>'),
        "{field} contains a non-ASCII or unbracketed message identifier: {value:?}"
    );
    let source_inner = &value.as_bytes()[1..value.len() - 1];
    let source_separator = message_id_separator(source_inner).ok_or_else(|| {
        anyhow::anyhow!(
            "{field} contains a message identifier without one valid @ separator: {value:?}"
        )
    })?;
    let (source_left, source_right_with_at) = source_inner.split_at(source_separator);
    let source_right = &source_right_with_at[1..];

    // notm-generated Message-ID values use the current RFC 5322 grammar:
    // dot-atom-text on the left and no internal CFWS. Threading values can
    // originate in older mail, for which conforming readers must also accept
    // obs-id-left/local-part and obs-id-right/domain. Validate that obsolete
    // grammar before removing its semantically empty CFWS so invalid adjacent
    // words cannot be silently repaired into a different identifier.
    let inner = if allow_obsolete_internal_cfws {
        ensure!(
            valid_obs_id_left_with_cfws(field, source_left)?,
            "{field} contains an invalid obsolete id-left in {value:?}"
        );
        ensure!(
            valid_obs_id_right_with_cfws(field, source_right)?,
            "{field} contains an invalid obsolete id-right in {value:?}"
        );
        strip_obsolete_message_id_cfws(field, source_inner)?
    } else {
        ensure!(
            valid_dot_atom(source_left),
            "{field} contains an invalid id-left in {value:?}"
        );
        ensure!(
            valid_id_right(source_right),
            "{field} contains an invalid id-right in {value:?}"
        );
        source_inner.to_vec()
    };
    let separator = message_id_separator(&inner).ok_or_else(|| {
        anyhow::anyhow!(
            "{field} contains a message identifier without one valid @ separator: {value:?}"
        )
    })?;
    let (left, right_with_at) = inner.split_at(separator);
    let right = &right_with_at[1..];
    ensure!(
        valid_obs_id_left(left),
        "{field} contains an invalid id-left in {value:?}"
    );
    ensure!(
        valid_id_right(right),
        "{field} contains an invalid id-right in {value:?}"
    );
    Ok(format!(
        "<{}>",
        std::str::from_utf8(&inner).expect("validated Message-ID is ASCII")
    ))
}

fn strip_obsolete_message_id_cfws(field: &str, value: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut canonical = Vec::with_capacity(value.len());
    let mut index = 0;
    let mut quoted = false;
    let mut literal = false;
    let mut escaped = false;
    while index < value.len() {
        let byte = value[index];
        if escaped {
            canonical.push(byte);
            escaped = false;
            index += 1;
            continue;
        }
        if (quoted || literal) && byte == b'\\' {
            canonical.push(byte);
            escaped = true;
            index += 1;
            continue;
        }
        if !literal && byte == b'"' {
            quoted = !quoted;
            canonical.push(byte);
            index += 1;
            continue;
        }
        if !quoted && byte == b'[' {
            literal = true;
            canonical.push(byte);
            index += 1;
            continue;
        }
        if literal && byte == b']' {
            literal = false;
            canonical.push(byte);
            index += 1;
            continue;
        }
        if !quoted && matches!(byte, b' ' | b'\t') {
            index += 1;
            continue;
        }
        if !quoted && !literal && byte == b'(' {
            index = skip_obsolete_message_id_comment(field, value, index)?;
            continue;
        }
        canonical.push(byte);
        index += 1;
    }
    ensure!(
        !quoted && !literal && !escaped,
        "{field} contains an unterminated quoted component"
    );
    Ok(canonical)
}

fn skip_obsolete_message_id_comment(
    field: &str,
    value: &[u8],
    mut index: usize,
) -> anyhow::Result<usize> {
    let mut depth = 1usize;
    index += 1;
    while depth != 0 {
        let Some(&byte) = value.get(index) else {
            anyhow::bail!("{field} contains an unterminated comment");
        };
        match byte {
            b'\\' => {
                index += 1;
                ensure!(
                    value.get(index).is_some_and(|escaped| {
                        matches!(escaped, b' ' | b'\t') || escaped.is_ascii_graphic()
                    }),
                    "{field} contains an invalid quoted pair in a comment"
                );
            }
            b'(' => depth += 1,
            b')' => depth -= 1,
            b' ' | b'\t' | b'!'..=b'~' => {}
            _ => anyhow::bail!("{field} contains an invalid character in a comment"),
        }
        index += 1;
    }
    Ok(index)
}

fn message_id_separator(inner: &[u8]) -> Option<usize> {
    let mut separator = None;
    let mut quoted = false;
    let mut literal = false;
    let mut escaped = false;
    let mut comment_depth = 0usize;
    for (index, byte) in inner.iter().copied().enumerate() {
        if comment_depth != 0 {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'(' {
                comment_depth += 1;
            } else if byte == b')' {
                comment_depth -= 1;
            }
        } else if escaped {
            escaped = false;
        } else if (quoted || literal) && byte == b'\\' {
            escaped = true;
        } else if !literal && byte == b'"' {
            quoted = !quoted;
        } else if !quoted && byte == b'[' {
            literal = true;
        } else if literal && byte == b']' {
            literal = false;
        } else if !quoted && !literal && byte == b'(' {
            comment_depth = 1;
        } else if !quoted && !literal && byte == b'@' && separator.replace(index).is_some() {
            return None;
        }
    }
    (!quoted && !literal && !escaped && comment_depth == 0)
        .then_some(separator)
        .flatten()
}

fn valid_obs_id_left_with_cfws(field: &str, value: &[u8]) -> anyhow::Result<bool> {
    if value.is_empty() {
        return Ok(false);
    }
    let mut index = 0;
    loop {
        let Some(next) = parse_obs_id_left_word(field, value, index)? else {
            return Ok(false);
        };
        index = next;
        if index == value.len() {
            return Ok(true);
        }
        if value[index] != b'.' {
            return Ok(false);
        }
        index += 1;
        if index == value.len() {
            return Ok(false);
        }
    }
}

fn parse_obs_id_left_word(
    field: &str,
    value: &[u8],
    index: usize,
) -> anyhow::Result<Option<usize>> {
    let index = skip_obsolete_message_id_cfws(field, value, index)?;
    let Some(&first) = value.get(index) else {
        return Ok(None);
    };
    let next = if first == b'"' {
        parse_quoted_id_word(value, index)
    } else {
        parse_id_left_atom(value, index)
    };
    next.map(|next| skip_obsolete_message_id_cfws(field, value, next))
        .transpose()
}

fn parse_id_left_atom(value: &[u8], index: usize) -> Option<usize> {
    let mut end = index;
    while value.get(end).is_some_and(|byte| is_atext(*byte)) {
        end += 1;
    }
    (end != index).then_some(end)
}

fn valid_obs_id_right_with_cfws(field: &str, value: &[u8]) -> anyhow::Result<bool> {
    if value.is_empty() {
        return Ok(false);
    }
    let start = skip_obsolete_message_id_cfws(field, value, 0)?;
    if value.get(start) == Some(&b'[') {
        return valid_obs_domain_literal(field, value, start);
    }

    let mut index = 0;
    loop {
        let Some(next) = parse_obs_domain_atom(field, value, index)? else {
            return Ok(false);
        };
        index = next;
        if index == value.len() {
            return Ok(true);
        }
        if value[index] != b'.' {
            return Ok(false);
        }
        index += 1;
        if index == value.len() {
            return Ok(false);
        }
    }
}

fn parse_obs_domain_atom(field: &str, value: &[u8], index: usize) -> anyhow::Result<Option<usize>> {
    let index = skip_obsolete_message_id_cfws(field, value, index)?;
    let Some(next) = parse_id_left_atom(value, index) else {
        return Ok(None);
    };
    Ok(Some(skip_obsolete_message_id_cfws(field, value, next)?))
}

fn valid_obs_domain_literal(field: &str, value: &[u8], mut index: usize) -> anyhow::Result<bool> {
    index += 1;
    let mut has_content = false;
    while let Some(&byte) = value.get(index) {
        match byte {
            b']' => {
                if !has_content {
                    return Ok(false);
                }
                index = skip_obsolete_message_id_cfws(field, value, index + 1)?;
                return Ok(index == value.len());
            }
            b' ' | b'\t' => index += 1,
            b'\\' => {
                let Some(&escaped) = value.get(index + 1) else {
                    return Ok(false);
                };
                if !matches!(escaped, b' ' | b'\t') && !escaped.is_ascii_graphic() {
                    return Ok(false);
                }
                has_content = true;
                index += 2;
            }
            b'!'..=b'Z' | b'^'..=b'~' => {
                has_content = true;
                index += 1;
            }
            _ => return Ok(false),
        }
    }
    Ok(false)
}

fn skip_obsolete_message_id_cfws(
    field: &str,
    value: &[u8],
    mut index: usize,
) -> anyhow::Result<usize> {
    loop {
        while value
            .get(index)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            index += 1;
        }
        if value.get(index) != Some(&b'(') {
            return Ok(index);
        }
        index = skip_obsolete_message_id_comment(field, value, index)?;
    }
}

fn valid_obs_id_left(value: &[u8]) -> bool {
    // RFC 5322 obs-id-left -> local-part -> obs-local-part, whose exact
    // grammar is word *("." word); word is atom / quoted-string. Multiple
    // quoted or mixed dot-separated words are therefore deliberately legal
    // when canonicalizing legacy threading identifiers.
    if value.is_empty() {
        return false;
    }
    let mut index = 0;
    loop {
        let Some(next) = parse_id_left_word(value, index) else {
            return false;
        };
        index = next;
        if index == value.len() {
            return true;
        }
        if value[index] != b'.' {
            return false;
        }
        index += 1;
        if index == value.len() {
            return false;
        }
    }
}

fn parse_id_left_word(value: &[u8], index: usize) -> Option<usize> {
    if value.get(index) == Some(&b'"') {
        return parse_quoted_id_word(value, index);
    }
    parse_id_left_atom(value, index)
}

fn parse_quoted_id_word(value: &[u8], mut index: usize) -> Option<usize> {
    index += 1;
    let mut escaped = false;
    while let Some(&byte) = value.get(index) {
        if escaped {
            if !matches!(byte, b' ' | b'\t') && !byte.is_ascii_graphic() {
                return None;
            }
            escaped = false;
        } else {
            match byte {
                b'\\' => escaped = true,
                // An empty quoted-string is valid in the obsolete local-part
                // grammar. The strict Message-ID path still requires a
                // dot-atom and therefore continues to reject quoted forms.
                b'"' => return Some(index + 1),
                b' ' | b'\t' | b'!' | b'#'..=b'[' | b']'..=b'~' => {}
                _ => return None,
            }
        }
        index += 1;
    }
    None
}

fn valid_id_right(value: &[u8]) -> bool {
    if value.starts_with(b"[") {
        return valid_no_fold_literal(value);
    }
    valid_dot_atom(value)
}

fn valid_no_fold_literal(value: &[u8]) -> bool {
    if value.len() < 3 || !value.starts_with(b"[") || !value.ends_with(b"]") {
        return false;
    }
    let content = &value[1..value.len() - 1];
    let mut index = 0;
    while index < content.len() {
        match content[index] {
            b'\\' => {
                index += 1;
                if !content
                    .get(index)
                    .is_some_and(|byte| matches!(byte, b' ' | b'\t') || byte.is_ascii_graphic())
                {
                    return false;
                }
            }
            b'!'..=b'Z' | b'^'..=b'~' => {}
            _ => return false,
        }
        index += 1;
    }
    true
}

fn valid_dot_atom(value: &[u8]) -> bool {
    !value.is_empty()
        && value[0] != b'.'
        && value[value.len() - 1] != b'.'
        && !value.windows(2).any(|window| window == b"..")
        && value.iter().all(|byte| *byte == b'.' || is_atext(*byte))
}

fn is_atext(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~".contains(&byte)
}

fn validate_header_text(field: &str, value: &str) -> anyhow::Result<()> {
    // HTAB is WSP in RFC 5322 and is therefore valid folding whitespace.
    // CR, LF, DEL, and every other control remain forbidden so a caller
    // cannot introduce a new field or an invalid wire line.
    if let Some(character) = value
        .chars()
        .find(|character| *character != '\t' && character.is_control())
    {
        anyhow::bail!(
            "{field} contains a forbidden control character U+{:04X}",
            u32::from(character)
        );
    }
    Ok(())
}

fn write_token_header(out: &mut Vec<u8>, name: &str, tokens: &[String]) -> anyhow::Result<()> {
    out.extend_from_slice(name.as_bytes());
    out.push(b':');
    let mut line_length = name.len() + 1;
    if tokens.is_empty() {
        out.extend_from_slice(b"\r\n");
        return Ok(());
    }
    for token in tokens {
        validate_header_text(name, token)?;
        ensure!(
            token.len() < MAX_WIRE_LINE_LENGTH,
            "{name} contains a token longer than the RFC 5322 line limit"
        );
        if line_length + 1 + token.len() <= RECOMMENDED_HEADER_LINE_LENGTH {
            out.push(b' ');
            line_length += 1;
        } else {
            out.extend_from_slice(b"\r\n ");
            line_length = 1;
        }
        out.extend_from_slice(token.as_bytes());
        line_length += token.len();
    }
    out.extend_from_slice(b"\r\n");
    Ok(())
}

fn write_literal_header(out: &mut Vec<u8>, name: &str, value: &str) -> anyhow::Result<()> {
    validate_header_text(name, value)?;
    ensure!(
        name.len() + 2 + value.len() <= MAX_WIRE_LINE_LENGTH,
        "{name} exceeds the RFC 5322 line limit"
    );
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b": ");
    out.extend_from_slice(value.as_bytes());
    out.extend_from_slice(b"\r\n");
    Ok(())
}

fn mixed_boundary(message: &ComposedMessage) -> String {
    format!("notm-mixed-{}", message.mime_boundary_id)
}

fn alternative_boundary(message: &ComposedMessage) -> String {
    format!("notm-alt-{}", message.mime_boundary_id)
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

fn write_text_part_headers(out: &mut Vec<u8>, content_type: &str) -> anyhow::Result<()> {
    write_literal_header(
        out,
        "Content-Type",
        &format!("{content_type}; charset=utf-8"),
    )?;
    write_literal_header(out, "Content-Transfer-Encoding", "quoted-printable")
}

fn write_multipart_header(
    out: &mut Vec<u8>,
    content_type: &str,
    boundary: &str,
) -> anyhow::Result<()> {
    write_literal_header(out, "Content-Type", &format!("{content_type};"))?;
    out.extend_from_slice(b" boundary=\"");
    out.extend_from_slice(boundary.as_bytes());
    out.extend_from_slice(b"\"\r\n");
    Ok(())
}

fn render_alternative_parts(
    out: &mut Vec<u8>,
    boundary: &str,
    text_body: &str,
    html_body: Option<&str>,
) -> anyhow::Result<()> {
    write_boundary(out, boundary, false);
    write_text_part_headers(out, "text/plain")?;
    out.extend_from_slice(b"\r\n");
    write_quoted_printable_body(out, text_body);
    if let Some(html) = html_body {
        write_boundary(out, boundary, false);
        write_text_part_headers(out, "text/html")?;
        out.extend_from_slice(b"\r\n");
        write_quoted_printable_body(out, html);
    }
    write_boundary(out, boundary, true);
    Ok(())
}

fn write_quoted_printable_body(out: &mut Vec<u8>, body: &str) {
    let normalized = normalize_body(body);
    out.extend_from_slice(&quoted_printable::encode(normalized.as_bytes()));
    ensure_crlf_terminated(out);
}

fn render_attachment(out: &mut Vec<u8>, attachment: &AttachmentInput) -> anyhow::Result<()> {
    validate_header_text("attachment filename", &attachment.filename)?;
    validate_header_text("attachment content type", &attachment.content_type)?;
    let mut content_type = safe_attachment_content_type(&attachment.content_type);
    // Composite message subtypes have subtype-specific transfer-encoding
    // rules. Only message/rfc822 is currently constructed deliberately; treat
    // unknown message subtypes as opaque bytes rather than guessing illegally.
    if content_type
        .split_once('/')
        .is_some_and(|(top_level, _)| top_level.eq_ignore_ascii_case("message"))
        && !content_type.eq_ignore_ascii_case("message/rfc822")
    {
        content_type = "application/octet-stream";
    }
    ensure!(
        content_type.len() <= 200,
        "attachment content type is unreasonably long"
    );
    write_parameterized_header(
        out,
        "Content-Type",
        content_type,
        "name",
        &attachment.filename,
    )?;
    let is_message = content_type.eq_ignore_ascii_case("message/rfc822");
    write_literal_header(
        out,
        "Content-Transfer-Encoding",
        if is_message { "8bit" } else { "base64" },
    )?;
    write_parameterized_header(
        out,
        "Content-Disposition",
        "attachment",
        "filename",
        &attachment.filename,
    )?;
    out.extend_from_slice(b"\r\n");
    if is_message {
        let embedded = normalize_embedded_message(&attachment.bytes, content_type)?;
        out.extend_from_slice(&embedded);
        ensure_crlf_terminated(out);
    } else {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&attachment.bytes);
        for chunk in encoded.as_bytes().chunks(76) {
            out.extend_from_slice(chunk);
            out.extend_from_slice(b"\r\n");
        }
        if encoded.is_empty() {
            out.extend_from_slice(b"\r\n");
        }
    }
    Ok(())
}

fn normalize_embedded_message(bytes: &[u8], content_type: &str) -> anyhow::Result<Vec<u8>> {
    ensure!(
        !bytes.contains(&0),
        "{content_type} attachment contains a NUL byte that cannot be sent with 8bit encoding"
    );
    let mut normalized = normalize_line_endings(bytes);
    ensure_crlf_terminated(&mut normalized);
    if content_type.eq_ignore_ascii_case("message/rfc822") {
        ensure!(
            normalized.windows(4).any(|window| window == b"\r\n\r\n"),
            "message/rfc822 attachment does not contain a header/body separator"
        );
        let parsed = mailparse::parse_mail(&normalized)
            .context("message/rfc822 attachment is not a parseable RFC 5322 message")?;
        ensure!(
            !parsed.headers.is_empty(),
            "message/rfc822 attachment does not contain any message headers"
        );
    }
    validate_wire(&normalized)
        .context("message attachment does not satisfy RFC 5322 wire line limits")?;
    Ok(normalized)
}

fn write_parameterized_header(
    out: &mut Vec<u8>,
    name: &str,
    value: &str,
    parameter_name: &str,
    parameter_value: &str,
) -> anyhow::Result<()> {
    write_literal_header(out, name, &format!("{value};"))?;
    let parameters = render_mime_parameter(parameter_name, parameter_value);
    for (index, parameter) in parameters.iter().enumerate() {
        ensure!(
            1 + parameter.len() + usize::from(index + 1 != parameters.len())
                <= RECOMMENDED_HEADER_LINE_LENGTH,
            "{name} MIME parameter segment exceeds the recommended line length"
        );
        out.push(b' ');
        out.extend_from_slice(parameter.as_bytes());
        if index + 1 != parameters.len() {
            out.push(b';');
        }
        out.extend_from_slice(b"\r\n");
    }
    Ok(())
}

fn render_mime_parameter(name: &str, value: &str) -> Vec<String> {
    if value.is_ascii()
        && value.len() <= MIME_PARAMETER_SEGMENT_LENGTH
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'"' | b'\\' | b';'))
    {
        return vec![format!("{name}=\"{value}\"")];
    }
    let atoms = percent_encoded_character_atoms(value);
    let mut segments = Vec::new();
    let mut current = String::new();
    for atom in atoms {
        if !current.is_empty() && current.len() + atom.len() > MIME_PARAMETER_SEGMENT_LENGTH {
            segments.push(current);
            current = String::new();
        }
        current.push_str(&atom);
    }
    segments.push(current);
    segments
        .into_iter()
        .enumerate()
        .map(|(index, segment)| {
            if index == 0 {
                format!("{name}*{index}*=utf-8''{segment}")
            } else {
                format!("{name}*{index}*={segment}")
            }
        })
        .collect()
}

fn percent_encoded_character_atoms(value: &str) -> Vec<String> {
    value
        .chars()
        .map(|character| percent_encode_mime_parameter(&character.to_string()))
        .collect()
}

fn percent_encode_mime_parameter(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::new();
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

fn write_boundary(out: &mut Vec<u8>, boundary: &str, closing: bool) {
    out.extend_from_slice(b"--");
    out.extend_from_slice(boundary.as_bytes());
    if closing {
        out.extend_from_slice(b"--");
    }
    out.extend_from_slice(b"\r\n");
}

fn ensure_crlf_terminated(out: &mut Vec<u8>) {
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
}

fn normalize_body(body: &str) -> String {
    String::from_utf8(normalize_line_endings(body.as_bytes()))
        .expect("normalizing line endings preserves UTF-8")
}

fn normalize_line_endings(input: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        match input[index] {
            b'\r' => {
                if input.get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
                normalized.extend_from_slice(b"\r\n");
            }
            b'\n' => normalized.extend_from_slice(b"\r\n"),
            byte => normalized.push(byte),
        }
        index += 1;
    }
    normalized
}

fn validate_wire(bytes: &[u8]) -> anyhow::Result<()> {
    let mut line_start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                ensure!(
                    bytes.get(index + 1) == Some(&b'\n'),
                    "rendered message contains a bare carriage return"
                );
                ensure!(
                    index - line_start <= MAX_WIRE_LINE_LENGTH,
                    "rendered message contains a line longer than {MAX_WIRE_LINE_LENGTH} bytes"
                );
                index += 2;
                line_start = index;
            }
            b'\n' => anyhow::bail!("rendered message contains a bare line feed"),
            _ => index += 1,
        }
    }
    ensure!(
        line_start == bytes.len(),
        "rendered message does not end with CRLF"
    );
    Ok(())
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
    use mailparse::MailHeaderMap;

    use super::*;

    fn test_message() -> ComposedMessage {
        ComposedMessage::new(
            "Sender <sender@example.test>".to_string(),
            vec!["Visible <visible@example.test>".to_string()],
            "Bcc contract".to_string(),
            "Body".to_string(),
        )
    }

    fn render(message: &ComposedMessage) -> Vec<u8> {
        render_message(message).expect("render valid test message")
    }

    #[test]
    fn long_unicode_headers_are_encoded_folded_and_round_trip() {
        let display_name = "長い表示名 Café 🚀 ".repeat(12);
        let subject = "Résumé status 世界 🚀 — ".repeat(24);
        let mut message = ComposedMessage::new(
            format!("{display_name} <sender@example.test>"),
            (0..32)
                .map(|index| format!("Recipient {index:02} <person{index:02}@example.test>"))
                .collect(),
            subject.clone(),
            "Body".to_string(),
        );
        message.cc = vec!["受信者 チーム <copy@example.test>".to_string()];

        let raw = render(&message);
        let parsed = mailparse::parse_mail(&raw).expect("independent parser accepts output");
        assert_eq!(
            parsed.headers.get_first_value("Subject").as_deref(),
            Some(subject.as_str())
        );
        assert!(
            parsed
                .headers
                .get_first_value("From")
                .expect("From header")
                .contains(&display_name)
        );
        assert_eq!(
            mailparse::addrparse_header(parsed.headers.get_first_header("To").expect("To header"))
                .expect("parse folded recipients")
                .count_addrs(),
            32
        );
        let text = String::from_utf8(raw).expect("header fixture is UTF-8");
        for encoded_word in text
            .split_ascii_whitespace()
            .filter(|word| word.starts_with("=?UTF-8?B?"))
        {
            assert!(
                encoded_word.len() <= 75,
                "encoded-word too long: {encoded_word}"
            );
        }
        for line in text
            .split("\r\n\r\n")
            .next()
            .expect("message headers")
            .split("\r\n")
            .filter(|line| line.contains("=?UTF-8?B?") || line.starts_with(' '))
        {
            assert!(line.len() <= 76, "encoded header line too long: {line}");
        }
        assert_wire_limits(text.as_bytes());
    }

    #[test]
    fn long_ascii_subject_is_encoded_without_changing_its_value() {
        let subject = "an intentionally long ASCII subject with exact spacing ".repeat(30);
        let mut message = test_message();
        message.subject = subject.clone();
        let raw = render(&message);
        let parsed = mailparse::parse_mail(&raw).expect("parse rendered message");
        assert_eq!(
            parsed.headers.get_first_value("Subject").as_deref(),
            Some(subject.as_str())
        );
        assert!(raw.windows(10).any(|part| part == b"=?UTF-8?B?"));
    }

    #[test]
    fn unusual_valid_mailboxes_preserve_address_semantics() {
        let mut message = ComposedMessage::new(
            r#""Ops, East" <"Abc@def"@example.test>"#.to_string(),
            vec![
                r#""Quoted local" <"john..doe"@example.test>"#.to_string(),
                "user+mailbox/department=shipping@example.test".to_string(),
                "literal@[192.0.2.1]".to_string(),
            ],
            "Addresses".to_string(),
            "Body".to_string(),
        );
        message.bcc = vec!["hidden+tag@example.test".to_string()];
        let raw = render(&message);
        let parsed = mailparse::parse_mail(&raw).expect("parse rendered message");
        let to =
            mailparse::addrparse_header(parsed.headers.get_first_header("To").expect("To header"))
                .expect("parse To header");
        let mailboxes = to
            .iter()
            .flat_map(|address| match address {
                mailparse::MailAddr::Single(single) => std::slice::from_ref(single),
                mailparse::MailAddr::Group(group) => group.addrs.as_slice(),
            })
            .map(|single| single.addr.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            mailboxes,
            [
                r#""john..doe"@example.test"#,
                "user+mailbox/department=shipping@example.test",
                "literal@[192.0.2.1]",
            ]
        );
    }

    #[test]
    fn header_injection_and_controls_are_rejected_instead_of_repaired() {
        for value in [
            "safe\r\nX-Injected: yes",
            "safe\nBcc: attacker@example.test",
            "safe\u{7f}bad",
        ] {
            let mut message = test_message();
            message.subject = value.to_string();
            assert!(
                render_message(&message)
                    .expect_err("control must be rejected")
                    .to_string()
                    .contains("control character")
            );
        }
        let mut address = test_message();
        address.to = vec!["visible@example.test\r\nBcc: attacker@example.test".to_string()];
        assert!(render_message(&address).is_err());
        let mut filename = test_message();
        filename.attachments.push(AttachmentInput {
            filename: "safe.txt\r\nX-Injected: yes".to_string(),
            content_type: "text/plain".to_string(),
            bytes: b"attachment".to_vec(),
            source_path: None,
        });
        assert!(render_message(&filename).is_err());
    }

    #[test]
    fn horizontal_tabs_remain_valid_folding_whitespace() {
        let mut message = test_message();
        message.subject = "Tabbed\tsubject".to_string();
        message.to = vec!["\"Tabbed\tName\" <tabbed@example.test>".to_string()];
        message.in_reply_to =
            Some("\t(legacy\tcomment) <\told\t@\t[\tIPv6:2001:db8::1\t]\t>\t".to_string());
        message.references = vec!["\t<root\t@\texample.test>\t".to_string()];

        let raw = render_message(&message).expect("HTAB is valid RFC 5322 whitespace");
        let parsed = mailparse::parse_mail(&raw).expect("parse tabbed message");

        assert!(
            raw.windows(b"Subject: Tabbed\tsubject".len())
                .any(|window| { window == b"Subject: Tabbed\tsubject" })
        );
        assert_eq!(
            parsed.headers.get_first_value("In-Reply-To").as_deref(),
            Some("<old@[IPv6:2001:db8::1]>")
        );
        assert_eq!(
            parsed.headers.get_first_value("References").as_deref(),
            Some("<root@example.test>")
        );
        let to = mailparse::addrparse_header(parsed.headers.get_first_header("To").expect("To"))
            .expect("parse tabbed address");
        assert_eq!(
            to.extract_single_info().expect("single mailbox").addr,
            "tabbed@example.test"
        );
    }

    #[test]
    fn bcc_submission_contract_does_not_expose_hidden_addresses_elsewhere() {
        let mut message = test_message();
        message.bcc = vec![
            "Hidden <hidden@example.test>".to_string(),
            "second@example.test".to_string(),
        ];
        let raw = render(&message);
        let parsed = mailparse::parse_mail(&raw).expect("parse rendered message");
        assert_eq!(
            parsed.headers.get_first_value("Bcc").as_deref(),
            Some("Hidden <hidden@example.test>, second@example.test")
        );
        assert!(
            !parsed
                .headers
                .get_first_value("To")
                .unwrap()
                .contains("hidden@")
        );
        assert!(!parsed.get_body().expect("decode body").contains("hidden@"));
        assert_eq!(
            parsed
                .headers
                .iter()
                .filter(|header| header.get_key_ref().eq_ignore_ascii_case("Bcc"))
                .count(),
            1
        );
    }

    #[test]
    fn bcc_only_message_omits_invalid_empty_destination_fields() {
        let mut message = test_message();
        message.to.clear();
        message.bcc = vec!["Only Hidden <hidden@example.test>".to_string()];

        let raw = render(&message);
        let parsed = mailparse::parse_mail(&raw).expect("parse Bcc-only message");
        assert!(parsed.headers.get_first_header("To").is_none());
        assert!(parsed.headers.get_first_header("Cc").is_none());
        assert_eq!(
            parsed.headers.get_first_value("Bcc").as_deref(),
            Some("Only Hidden <hidden@example.test>")
        );
        assert_wire_limits(&raw);
    }

    #[test]
    fn long_text_html_and_binary_attachment_lines_are_transfer_encoded() {
        let mut message = test_message();
        let text_body = format!("{}\nUnicode: {}", "x".repeat(4_000), "世界".repeat(1_000));
        let html_body = format!("<p>{}</p>", "🚀".repeat(2_000));
        let attachment_bytes = (0..20_000).map(|value| value as u8).collect::<Vec<_>>();
        message.body = text_body.clone();
        message.html_body = Some(html_body.clone());
        message.attachments.push(AttachmentInput {
            filename: "payload.bin".to_string(),
            content_type: "application/octet-stream".to_string(),
            bytes: attachment_bytes.clone(),
            source_path: None,
        });
        let raw = render(&message);
        assert_wire_limits(&raw);
        let parsed = mailparse::parse_mail(&raw).expect("parse long multipart message");
        assert_eq!(parsed.ctype.mimetype, "multipart/mixed");
        assert_eq!(
            parsed.subparts[0].subparts[0]
                .get_body()
                .unwrap()
                .replace("\r\n", "\n"),
            text_body
        );
        assert_eq!(
            parsed.subparts[0].subparts[1].get_body().unwrap(),
            html_body
        );
        assert_eq!(parsed.subparts[1].get_body_raw().unwrap(), attachment_bytes);
    }

    #[test]
    fn unicode_and_long_filenames_use_rfc2231_continuations_and_round_trip() {
        let filename = format!(
            "{}-{}.txt",
            "非常に長い添付ファイル名".repeat(8),
            "x".repeat(90)
        );
        let mut message = test_message();
        message.attachments.push(AttachmentInput {
            filename: filename.clone(),
            content_type: "text/plain".to_string(),
            bytes: b"attachment".to_vec(),
            source_path: None,
        });
        let raw = render(&message);
        let text = String::from_utf8(raw.clone()).expect("rendered fixture is UTF-8");
        assert!(text.contains("filename*0*=utf-8''"));
        assert!(text.contains("filename*1*="));
        assert!(text.contains("name*0*=utf-8''"));
        assert_wire_limits(&raw);
        let attachments = crate::mime::extract_attachments(&raw).expect("extract attachment");
        assert_eq!(attachments[0].filename, filename);
        assert_eq!(attachments[0].bytes, b"attachment");
    }

    #[test]
    fn message_rfc822_attachment_uses_normalized_8bit_not_base64() {
        let embedded = b"From: Original <original@example.test>\nTo: receiver@example.test\nSubject: Caf\xC3\xA9\n\nNested body\n";
        let mut message = test_message();
        message.attachments.push(AttachmentInput {
            filename: "forwarded.eml".to_string(),
            content_type: "message/rfc822".to_string(),
            bytes: embedded.to_vec(),
            source_path: None,
        });
        let raw = render(&message);
        let text = String::from_utf8(raw.clone()).expect("fixture is UTF-8");
        assert!(text.contains(
            "Content-Type: message/rfc822;\r\n name=\"forwarded.eml\"\r\nContent-Transfer-Encoding: 8bit\r\n"
        ));
        assert!(text.contains("Subject: Caf\u{e9}\r\n\r\nNested body\r\n"));
        assert_wire_limits(&raw);
        mailparse::parse_mail(&raw).expect("independent parser accepts forwarded message");
    }

    #[test]
    fn malformed_or_overlong_message_rfc822_attachment_is_rejected() {
        for bytes in [
            b"not a message".to_vec(),
            format!("From: sender@example.test\r\n\r\n{}\r\n", "x".repeat(999)).into_bytes(),
        ] {
            let mut message = test_message();
            message.attachments.push(AttachmentInput {
                filename: "forwarded.eml".to_string(),
                content_type: "message/rfc822".to_string(),
                bytes,
                source_path: None,
            });
            assert!(render_message(&message).is_err());
        }
    }

    #[test]
    fn threading_headers_fold_and_round_trip() {
        let mut message = test_message();
        message.in_reply_to =
            Some("(parent) < (old) \"quoted id\" (left) @ [ IPv6:2001:db8::1 ] >".to_string());
        message.references = vec![
            "<first.\"obsolete word\"@example.test>".to_string(),
            "<\"quoted\".\"words\"@example.test>".to_string(),
            "<literal@[192.0.2.1]>".to_string(),
        ];
        message
            .references
            .extend((0..80).map(|index| format!("<thread-{index:03}@example.test>")));
        let raw = render(&message);
        let parsed = mailparse::parse_mail(&raw).expect("parse threaded message");
        assert_eq!(
            parsed.headers.get_first_value("In-Reply-To").as_deref(),
            Some("<\"quoted id\"@[IPv6:2001:db8::1]>")
        );
        assert_eq!(
            parsed
                .headers
                .get_first_value("References")
                .unwrap()
                .matches('<')
                .count(),
            83
        );
        assert_wire_limits(&raw);
    }

    #[test]
    fn generated_and_legacy_message_id_grammars_are_deliberately_distinct() {
        assert_eq!(
            message_id_tokens(
                "Message-ID",
                &["(outside) <strict.id@[IPv6:2001:db8::1]> (outside)".to_string()],
            )
            .expect("strict identifier with surrounding CFWS"),
            ["<strict.id@[IPv6:2001:db8::1]>"]
        );

        for invalid in [
            "<\"quoted\"@example.test>",
            "<first.\"quoted\"@example.test>",
            "<strict(comment)@example.test>",
            "<strict @ example.test>",
        ] {
            assert!(
                message_id_tokens("Message-ID", &[invalid.to_string()]).is_err(),
                "strict Message-ID accepted obsolete syntax {invalid:?}"
            );
        }

        for (legacy, canonical) in [
            (
                "(outside) < (old) \"quoted id\" (left) @ [ IPv6:2001:db8::1 ] > (outside)",
                "<\"quoted id\"@[IPv6:2001:db8::1]>",
            ),
            (
                "< (left) first (after) . (before) \"second word\" (right) @ \
                 (domain) example (after) . (before) test (right) >",
                "<first.\"second word\"@example.test>",
            ),
            (
                "<\"quoted@left\".\"words\"@example.test>",
                "<\"quoted@left\".\"words\"@example.test>",
            ),
            (
                "<atom(comment@ignored)@example.test>",
                "<atom@example.test>",
            ),
        ] {
            for field in ["In-Reply-To", "References"] {
                assert_eq!(
                    message_id_tokens(field, &[legacy.to_string()])
                        .unwrap_or_else(|error| panic!("{field} rejected {legacy:?}: {error:#}")),
                    [canonical],
                    "unexpected {field} canonicalization for {legacy:?}"
                );
            }
        }

        let sequence = "(lead) <first@example.test> (between) \
                        < (old) \"second id\" (left) @ example.test > (tail)";
        assert_eq!(
            message_id_tokens("References", &[sequence.to_string()])
                .expect("legacy References list"),
            ["<first@example.test>", "<\"second id\"@example.test>"]
        );

        for invalid in [
            "<a(comment)b@example.test>",
            "<a b@example.test>",
            "<a.\"b\" \"c\"@example.test>",
            "<a@example(comment)test>",
            "<a@example test>",
            "<a@\"quoted\".example.test>",
            "<a@[ ]>",
            "<a@@example.test>",
            "<x\"\"@example.test>",
            "<\"\"x@example.test>",
        ] {
            for field in ["In-Reply-To", "References"] {
                assert!(
                    message_id_tokens(field, &[invalid.to_string()]).is_err(),
                    "{field} accepted invalid obsolete syntax {invalid:?}"
                );
            }
        }
    }

    #[test]
    fn obsolete_threading_phrases_are_ignored_while_message_ids_are_preserved() {
        let sequence = "Leading Café atom . initials \"日本語 <text>\" (before) \
                        <first@example.test> between . words (middle) \
                        <second@example.test> trailing . phrase (after)";
        for field in ["In-Reply-To", "References"] {
            assert_eq!(
                message_id_tokens(field, &[sequence.to_string()])
                    .unwrap_or_else(|error| panic!("{field} rejected obsolete phrases: {error:#}")),
                ["<first@example.test>", "<second@example.test>"]
            );
        }
    }

    #[test]
    fn encoded_threading_phrase_words_are_opaque_and_validated() {
        let sequence = "=?US-ASCII?Q?=3Cfake=40example=2Etest=3E?= \
                        =?US-ASCII?Q?=2C_=3Cother=40example=2Etest=3E?= \
                        <real@example.test>";
        for field in ["In-Reply-To", "References"] {
            assert_eq!(
                message_id_tokens(field, &[sequence.to_string()])
                    .unwrap_or_else(|error| panic!("{field} rejected encoded phrase: {error:#}")),
                ["<real@example.test>"]
            );
        }
        assert!(
            message_id_tokens("Message-ID", &[sequence.to_string()]).is_err(),
            "strict Message-ID accepted an encoded phrase"
        );

        for tolerated in [
            "=?UTF-8?Q?bad=ZZ?= <real@example.test>",
            "=?UTF-8?B?not-base64!?= <real@example.test>",
            "=?UTF-8?X?value?= <real@example.test>",
            "=?US-ASCII?Q?=3Cfake@example.test=3E?= <real@example.test>",
            "=?US-ASCII?Q?phrase,with,commas?= <real@example.test>",
        ] {
            for field in ["In-Reply-To", "References"] {
                assert_eq!(
                    message_id_tokens(field, &[tolerated.to_string()]).unwrap_or_else(|error| {
                        panic!("{field} rejected opaque malformed payload {tolerated:?}: {error:#}")
                    }),
                    ["<real@example.test>"]
                );
            }
        }

        for invalid in [
            "=?UTF.8?Q?value?= <real@example.test>",
            "=?UTF-8?Q??= <real@example.test>",
            "=?UTF-8?Q?unterminated <real@example.test>",
            "=?UTF-8?Q?valid?=adjacent <real@example.test>",
        ] {
            for field in ["In-Reply-To", "References"] {
                assert!(
                    message_id_tokens(field, &[invalid.to_string()]).is_err(),
                    "{field} accepted malformed encoded-word {invalid:?}"
                );
            }
        }
    }

    #[test]
    fn obsolete_threading_phrases_are_validated_not_treated_as_free_text() {
        for invalid in [
            "leading, phrase <valid@example.test>",
            "leading: phrase <valid@example.test>",
            "leading @ phrase <valid@example.test>",
            "\"unterminated <valid@example.test>",
            "(unterminated <valid@example.test>",
            "<valid@example.test> trailing,",
            "<valid@example.test> \"unterminated",
            ". leading <valid@example.test>",
            "leading <missing-at>",
        ] {
            for field in ["In-Reply-To", "References"] {
                assert!(
                    message_id_tokens(field, &[invalid.to_string()]).is_err(),
                    "{field} accepted malformed obsolete phrase {invalid:?}"
                );
            }
        }

        let phrase_only = "legacy . phrase \"with <quoted angles>\" (comment)";
        assert_eq!(
            message_id_tokens("References", &[phrase_only.to_string()])
                .expect("valid phrase grammar can be parsed"),
            Vec::<String>::new()
        );
        for field in ["In-Reply-To", "References"] {
            let mut message = test_message();
            if field == "In-Reply-To" {
                message.in_reply_to = Some(phrase_only.to_string());
            } else {
                message.references = vec![phrase_only.to_string()];
            }
            let error = render_message(&message)
                .expect_err("a threading field containing no msg-id must be rejected");
            assert!(
                error.to_string().contains(field),
                "unexpected {field} error: {error:#}"
            );
        }
    }

    #[test]
    fn empty_quoted_legacy_id_left_is_accepted_only_for_threading() {
        for field in ["In-Reply-To", "References"] {
            assert_eq!(
                message_id_tokens(field, &["<\"\"@example.test>".to_string()]).unwrap_or_else(
                    |error| panic!("{field} rejected empty quoted id-left: {error:#}")
                ),
                ["<\"\"@example.test>"]
            );
        }
        assert!(
            message_id_tokens("Message-ID", &["<\"\"@example.test>".to_string()]).is_err(),
            "strict Message-ID accepted an obsolete quoted id-left"
        );

        let mut message = test_message();
        message.in_reply_to = Some("legacy phrase <\"\"@example.test> tail".to_string());
        message.references = vec!["root <root@example.test> <\"\"@example.test>".to_string()];
        let raw = render(&message);
        let parsed = mailparse::parse_mail(&raw).expect("parse rendered message");
        assert_eq!(
            parsed.headers.get_first_value("In-Reply-To").as_deref(),
            Some("<\"\"@example.test>")
        );
        assert_eq!(
            parsed.headers.get_first_value("References").as_deref(),
            Some("<root@example.test> <\"\"@example.test>")
        );
    }

    #[test]
    fn invalid_threading_header_is_rejected() {
        let mut message = test_message();
        message.in_reply_to =
            Some("<parent@example.test>\r\nBcc: attacker@example.test".to_string());
        assert!(render_message(&message).is_err());
        let mut malformed = test_message();
        malformed.references = vec!["not-a-message-id".to_string()];
        assert!(render_message(&malformed).is_err());

        for field in ["Message-ID", "In-Reply-To", "References"] {
            let mut message = test_message();
            match field {
                "Message-ID" => message.message_id = "<a@@example.test>".to_string(),
                "In-Reply-To" => {
                    message.in_reply_to = Some("<a@@example.test>".to_string());
                }
                "References" => message.references = vec!["<a@@example.test>".to_string()],
                _ => unreachable!(),
            }
            let error = render_message(&message)
                .expect_err("invalid grammar must be rejected in every message-ID field");
            assert!(
                error.to_string().contains(field),
                "unexpected {field} error: {error:#}"
            );
        }

        for invalid in [
            "<a@@example.test>",
            "<a@b@c>",
            "<a,b@example.test>",
            "<@example.test>",
            "<a@>",
            "<.a@example.test>",
            "<a.@example.test>",
            "<a..b@example.test>",
            "<a@.example.test>",
            "<a@example..test>",
            "<a@example.test.>",
            "<\"\"@example.test>",
            "<a@[]>",
            "<a(comment)@example.test>",
            "<a @example.test>",
            "<a@ example.test>",
        ] {
            let mut message = test_message();
            message.message_id = invalid.to_string();
            let error =
                render_message(&message).expect_err("invalid Message-ID grammar must be rejected");
            assert!(
                error.to_string().contains("Message-ID"),
                "unexpected error for {invalid:?}: {error:#}"
            );
        }

        let mut invalid_list = test_message();
        invalid_list.references =
            vec!["<valid@example.test> <a@@example.test> <later@example.test>".to_string()];
        assert!(
            render_message(&invalid_list).is_err(),
            "one malformed identifier must reject the complete References list"
        );
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
            "text/\u{2615}",
        ] {
            assert_eq!(
                safe_attachment_content_type(content_type),
                "application/octet-stream",
                "accepted unsafe attachment content type {content_type:?}"
            );
        }
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
            assert_eq!(normalize_body(&normalized), normalized);
        }
    }

    #[test]
    fn repeated_multipart_rendering_is_byte_identical_across_clones() {
        let mut message = test_message();
        message.body = "Plain body".to_string();
        message.html_body = Some("<p>HTML body</p>".to_string());
        message.attachments.push(AttachmentInput {
            filename: "report.txt".to_string(),
            content_type: "text/plain".to_string(),
            bytes: b"attachment contents\n".to_vec(),
            source_path: None,
        });
        let persistence_copy = message.clone();
        let submitted = message.to_rfc5322().expect("render message");
        let submitted_again = message.to_rfc5322().expect("render message again");
        let persisted = persistence_copy.to_rfc5322().expect("render clone");
        assert_eq!(submitted, submitted_again);
        assert_eq!(submitted, persisted);
        let parsed = mailparse::parse_mail(&submitted).expect("parse rendered message");
        assert_eq!(parsed.ctype.mimetype, "multipart/mixed");
        assert_eq!(parsed.subparts.len(), 2);
        assert_eq!(parsed.subparts[0].ctype.mimetype, "multipart/alternative");
        assert_eq!(parsed.subparts[0].subparts.len(), 2);
        assert_eq!(parsed.subparts[1].ctype.mimetype, "text/plain");
    }

    fn assert_wire_limits(raw: &[u8]) {
        validate_wire(raw).expect("valid CRLF and line lengths");
        for line in raw.split(|byte| *byte == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            assert!(line.len() <= MAX_WIRE_LINE_LENGTH);
        }
    }
}
