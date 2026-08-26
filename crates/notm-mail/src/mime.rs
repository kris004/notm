use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

use anyhow::Context;
use mailparse::{DispositionType, MailHeaderMap, body::Body};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    html_sanitize::html_to_safe_text,
    message_io::{MESSAGE_BYTES_LIMIT, read_message_bytes_with_limit},
};

pub const MIME_DEPTH_LIMIT: usize = 32;
pub const MIME_PARTS_LIMIT: usize = 2048;
pub const MIME_DECODED_PART_BYTES_LIMIT: usize = 32 * 1024 * 1024;
pub const MIME_TOTAL_DECODED_BYTES_LIMIT: usize = 64 * 1024 * 1024;

/// Resource limits applied before and during MIME parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MimeLimits {
    pub max_message_bytes: usize,
    pub max_depth: usize,
    pub max_parts: usize,
    pub max_decoded_part_bytes: usize,
    pub max_total_decoded_bytes: usize,
}

impl Default for MimeLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: MESSAGE_BYTES_LIMIT,
            max_depth: MIME_DEPTH_LIMIT,
            max_parts: MIME_PARTS_LIMIT,
            max_decoded_part_bytes: MIME_DECODED_PART_BYTES_LIMIT,
            max_total_decoded_bytes: MIME_TOTAL_DECODED_BYTES_LIMIT,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum MimeLimitError {
    #[error("message has MIME nesting depth {actual}, exceeding the limit of {limit}")]
    Depth { actual: usize, limit: usize },
    #[error("message has more than the allowed {limit} MIME parts")]
    Parts { limit: usize },
    #[error("{part} may decode to more than the {limit}-byte per-part limit")]
    DecodedPart { part: String, limit: usize },
    #[error("decoded MIME content exceeds the {limit}-byte aggregate limit")]
    TotalDecoded { limit: usize },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CryptoPartKind {
    EncryptedContainer,
    SignedContainer,
    OpenPgpEncrypted,
    OpenPgpSignature,
    OpenPgpKey,
    SmimeEncrypted,
    SmimeSigned,
    SmimeSignature,
    SmimeOther,
}

impl CryptoPartKind {
    pub fn is_encrypted(&self) -> bool {
        matches!(
            self,
            Self::EncryptedContainer | Self::OpenPgpEncrypted | Self::SmimeEncrypted
        )
    }

    pub fn is_signed(&self) -> bool {
        matches!(
            self,
            Self::SignedContainer
                | Self::OpenPgpSignature
                | Self::SmimeSigned
                | Self::SmimeSignature
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CryptoPart {
    /// Child indexes from the message root to this MIME part.
    pub path: Vec<usize>,
    pub kind: CryptoPartKind,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalendarPart {
    /// Child indexes from the message root to this MIME part.
    pub path: Vec<usize>,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
}

impl CalendarPart {
    pub fn is_invitation(&self) -> bool {
        self.method
            .as_deref()
            .is_some_and(|method| matches!(method, "PUBLISH" | "REQUEST" | "ADD"))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MimeClassification {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calendar_parts: Vec<CalendarPart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crypto_parts: Vec<CryptoPart>,
}

impl MimeClassification {
    pub fn has_calendar(&self) -> bool {
        !self.calendar_parts.is_empty()
    }

    pub fn has_invitation(&self) -> bool {
        self.calendar_parts.iter().any(CalendarPart::is_invitation)
    }

    pub fn has_encrypted(&self) -> bool {
        self.crypto_parts
            .iter()
            .any(|part| part.kind.is_encrypted())
    }

    pub fn has_signed(&self) -> bool {
        self.crypto_parts.iter().any(|part| part.kind.is_signed())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    /// Stable depth-first ordinal among attachment-like MIME parts in this message.
    #[serde(default)]
    pub part_index: usize,
    pub filename: Option<String>,
    pub content_type: String,
    pub size: usize,
    pub content_id: Option<String>,
    /// Non-fatal transfer-encoding problems encountered while decoding this part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decode_warnings: Vec<String>,
    /// A fatal transfer-encoding problem. Metadata remains available when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decode_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtractedAttachment {
    /// Stable depth-first ordinal among attachment MIME parts in this message.
    #[serde(default)]
    pub part_index: usize,
    pub filename: String,
    pub content_type: String,
    pub content_id: Option<String>,
    pub bytes: Vec<u8>,
    /// Non-fatal transfer-encoding problems encountered while decoding this part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decode_warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentDecodeFailure {
    /// Stable depth-first ordinal among attachment MIME parts in this message.
    pub part_index: usize,
    pub filename: String,
    pub content_type: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttachmentExtractionReport {
    pub attachments: Vec<ExtractedAttachment>,
    pub failures: Vec<AttachmentDecodeFailure>,
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
    /// Transfer-encoding problems encountered while preserving readable parts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decode_warnings: Vec<String>,
    pub attachments: Vec<Attachment>,
    pub mime_tree: Vec<String>,
    #[serde(default)]
    pub classification: MimeClassification,
}

pub fn parse_rfc5322(bytes: &[u8]) -> anyhow::Result<ParsedMessage> {
    parse_rfc5322_with_limits(bytes, MimeLimits::default())
}

pub fn parse_rfc5322_with_limits(
    bytes: &[u8],
    limits: MimeLimits,
) -> anyhow::Result<ParsedMessage> {
    anyhow::ensure!(
        bytes.len() <= limits.max_message_bytes,
        "message exceeds the {}-byte safety limit",
        limits.max_message_bytes
    );
    preflight_mime_structure(bytes, limits)?;
    let parsed = mailparse::parse_mail(bytes)?;
    let headers = parsed
        .headers
        .iter()
        .map(|h| (h.get_key().to_string(), h.get_value()))
        .collect::<BTreeMap<_, _>>();
    let mut state = ParseState::new(limits);
    let selection = walk_part(&parsed, 0, &mut Vec::new(), false, &mut state)?;
    let text_body = selection.text.unwrap_or_default();
    let html_body = selection.html;
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
        decode_warnings: state.decode_warnings,
        attachments: state.attachments,
        mime_tree: state.tree,
        classification: state.classification,
    })
}

pub fn parse_file(path: impl AsRef<Path>) -> anyhow::Result<ParsedMessage> {
    parse_file_with_limits(path, MimeLimits::default())
}

pub fn parse_file_with_limits(
    path: impl AsRef<Path>,
    limits: MimeLimits,
) -> anyhow::Result<ParsedMessage> {
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("opening message {}", path.display()))?;
    parse_reader_with_limits(file, limits)
}

pub fn parse_reader(reader: impl Read) -> anyhow::Result<ParsedMessage> {
    parse_reader_with_limits(reader, MimeLimits::default())
}

pub fn parse_reader_with_limits(
    reader: impl Read,
    limits: MimeLimits,
) -> anyhow::Result<ParsedMessage> {
    let bytes = read_message_bytes_with_limit(reader, limits.max_message_bytes)?;
    parse_rfc5322_with_limits(&bytes, limits)
}

#[derive(Debug, Default)]
struct BodySelection {
    text: Option<String>,
    html: Option<String>,
}

impl BodySelection {
    fn append(&mut self, other: Self) {
        append_body(&mut self.text, other.text);
        append_body(&mut self.html, other.html);
    }

    fn replace_supported(&mut self, other: Self) {
        if other.text.is_some() {
            self.text = other.text;
        }
        if other.html.is_some() {
            self.html = other.html;
        }
    }
}

fn append_body(destination: &mut Option<String>, source: Option<String>) {
    let Some(source) = source.filter(|body| !body.trim().is_empty()) else {
        return;
    };
    match destination {
        Some(destination) if !destination.trim().is_empty() => {
            destination.push_str("\n\n");
            destination.push_str(&source);
        }
        _ => *destination = Some(source),
    }
}

struct ParseState {
    limits: MimeLimits,
    total_decoded_bytes: usize,
    next_attachment_index: usize,
    attachments: Vec<Attachment>,
    tree: Vec<String>,
    decode_warnings: Vec<String>,
    classification: MimeClassification,
}

impl ParseState {
    fn new(limits: MimeLimits) -> Self {
        Self {
            limits,
            total_decoded_bytes: 0,
            next_attachment_index: 0,
            attachments: Vec::new(),
            tree: Vec::new(),
            decode_warnings: Vec::new(),
            classification: MimeClassification::default(),
        }
    }

    fn reserve_decoded(&mut self, part: &str, bytes: usize) -> anyhow::Result<()> {
        if bytes > self.limits.max_decoded_part_bytes {
            return Err(MimeLimitError::DecodedPart {
                part: part.to_string(),
                limit: self.limits.max_decoded_part_bytes,
            }
            .into());
        }
        let total =
            self.total_decoded_bytes
                .checked_add(bytes)
                .ok_or(MimeLimitError::TotalDecoded {
                    limit: self.limits.max_total_decoded_bytes,
                })?;
        if total > self.limits.max_total_decoded_bytes {
            return Err(MimeLimitError::TotalDecoded {
                limit: self.limits.max_total_decoded_bytes,
            }
            .into());
        }
        self.total_decoded_bytes = total;
        Ok(())
    }
}

fn walk_part(
    part: &mailparse::ParsedMail<'_>,
    depth: usize,
    path: &mut Vec<usize>,
    named_body_allowed: bool,
    state: &mut ParseState,
) -> anyhow::Result<BodySelection> {
    let mimetype = part.ctype.mimetype.to_lowercase();
    state
        .tree
        .push(format!("{}{}", "  ".repeat(depth), mimetype));
    let mut filename = part_filename(part);
    let explicitly_attached =
        part_is_explicit_attachment(part, filename.as_deref(), named_body_allowed);
    classify_part(part, path, filename.as_deref(), &mut state.classification);

    if !part.subparts.is_empty() {
        let mut selections = Vec::with_capacity(part.subparts.len());
        let child_named_body_allowed = named_body_allowed || mimetype == "multipart/alternative";
        for (index, subpart) in part.subparts.iter().enumerate() {
            path.push(index);
            let selection = walk_part(subpart, depth + 1, path, child_named_body_allowed, state)?;
            path.pop();
            selections.push(selection);
        }
        if explicitly_attached {
            return Ok(BodySelection::default());
        }
        return Ok(select_multipart_body(part, &mimetype, selections));
    }

    let content_id = part.headers.get_first_value("Content-ID");
    let crypto_kind = crypto_part_kind(part);
    let attachment_like =
        part_is_attachment_like(&mimetype, explicitly_attached, crypto_kind.is_some());
    if attachment_like && filename.is_none() {
        filename = Some(fallback_attachment_filename(part));
    }
    let description = part_description(&mimetype, filename.as_deref(), attachment_like);
    if attachment_like {
        ensure_decode_may_fit(part, &description, state.limits.max_decoded_part_bytes)?;
        let part_index = state.next_attachment_index;
        state.next_attachment_index += 1;
        match decode_part_bytes(part) {
            Ok(decoded) => {
                state.reserve_decoded(&description, decoded.bytes.len())?;
                for warning in &decoded.warnings {
                    state
                        .decode_warnings
                        .push(format!("Decoded non-conformant {description}: {warning}"));
                }
                state.attachments.push(Attachment {
                    part_index,
                    filename,
                    content_type: mimetype,
                    size: decoded.bytes.len(),
                    content_id,
                    decode_warnings: decoded.warnings,
                    decode_error: None,
                });
            }
            Err(err) => {
                let error = format!("{err:#}");
                state
                    .decode_warnings
                    .push(format!("Could not decode {description}: {error}"));
                state.attachments.push(Attachment {
                    part_index,
                    filename,
                    content_type: mimetype,
                    size: 0,
                    content_id,
                    decode_warnings: Vec::new(),
                    decode_error: Some(error),
                });
            }
        }
        return Ok(BodySelection::default());
    }
    if crypto_kind.is_some() {
        return Ok(BodySelection::default());
    }

    match validate_text_transfer_encoding(part) {
        Ok(warnings) => {
            for warning in warnings {
                state
                    .decode_warnings
                    .push(format!("Decoded non-conformant {description}: {warning}"));
            }
        }
        Err(err) => {
            state
                .decode_warnings
                .push(format!("Could not decode {description}: {err:#}"));
            return Ok(BodySelection::default());
        }
    }
    ensure_decode_may_fit(part, &description, state.limits.max_decoded_part_bytes)?;
    match part.get_body() {
        Ok(body) => {
            state.reserve_decoded(&description, body.len())?;
            Ok(match mimetype.as_str() {
                "text/plain" => BodySelection {
                    text: Some(body),
                    html: None,
                },
                "text/html" => BodySelection {
                    text: None,
                    html: Some(body),
                },
                _ => BodySelection::default(),
            })
        }
        Err(err) => {
            state
                .decode_warnings
                .push(format!("Could not decode {description}: {err}"));
            Ok(BodySelection::default())
        }
    }
}

fn select_multipart_body(
    part: &mailparse::ParsedMail<'_>,
    mimetype: &str,
    selections: Vec<BodySelection>,
) -> BodySelection {
    match mimetype {
        "multipart/alternative" => {
            // RFC 2046 orders alternatives from least to most faithful. Preserve
            // the last supported representation of each exact body media type.
            let mut selected = BodySelection::default();
            for selection in selections {
                selected.replace_supported(selection);
            }
            selected
        }
        "multipart/related" => {
            let root_index =
                part.ctype
                    .params
                    .get("start")
                    .and_then(|start| {
                        let start = normalize_content_id(start);
                        part.subparts.iter().position(|subpart| {
                            subpart.headers.get_first_value("Content-ID").is_some_and(
                                |content_id| normalize_content_id(&content_id) == start,
                            )
                        })
                    })
                    .unwrap_or(0);
            selections.into_iter().nth(root_index).unwrap_or_default()
        }
        "multipart/signed" => selections.into_iter().next().unwrap_or_default(),
        "multipart/encrypted" => BodySelection::default(),
        _ => {
            let mut selected = BodySelection::default();
            for selection in selections {
                selected.append(selection);
            }
            selected
        }
    }
}

fn normalize_content_id(value: &str) -> &str {
    value.trim().trim_start_matches('<').trim_end_matches('>')
}

fn part_filename(part: &mailparse::ParsedMail<'_>) -> Option<String> {
    let disposition = part.get_content_disposition();
    disposition
        .params
        .get("filename")
        .cloned()
        .or_else(|| part.ctype.params.get("name").cloned())
}

fn part_is_explicit_attachment(
    part: &mailparse::ParsedMail<'_>,
    filename: Option<&str>,
    named_body_allowed: bool,
) -> bool {
    matches!(
        part.get_content_disposition().disposition,
        DispositionType::Attachment
    ) || (filename.is_some() && !named_body_allowed)
}

fn part_is_attachment_like(
    mimetype: &str,
    explicitly_attached: bool,
    crypto_related: bool,
) -> bool {
    explicitly_attached
        || mimetype == "text/calendar"
        || (!crypto_related && !matches!(mimetype, "text/plain" | "text/html"))
}

fn fallback_attachment_filename(part: &mailparse::ParsedMail<'_>) -> String {
    match part.ctype.mimetype.to_ascii_lowercase().as_str() {
        "text/calendar" => {
            let method = calendar_method(part);
            if method
                .as_deref()
                .is_some_and(|method| matches!(method, "PUBLISH" | "REQUEST" | "ADD"))
            {
                "invitation.ics".to_string()
            } else {
                "calendar.ics".to_string()
            }
        }
        "message/rfc822" => "attached-message.eml".to_string(),
        _ => "attachment.bin".to_string(),
    }
}

fn calendar_method(part: &mailparse::ParsedMail<'_>) -> Option<String> {
    part.ctype
        .params
        .get("method")
        .map(|method| method.trim().to_ascii_uppercase())
        .filter(|method| !method.is_empty())
}

fn classify_part(
    part: &mailparse::ParsedMail<'_>,
    path: &[usize],
    filename: Option<&str>,
    classification: &mut MimeClassification,
) {
    let content_type = part.ctype.mimetype.to_lowercase();
    if content_type == "text/calendar" {
        classification.calendar_parts.push(CalendarPart {
            path: path.to_vec(),
            content_type: content_type.clone(),
            method: calendar_method(part),
            filename: filename.map(ToOwned::to_owned),
        });
    }
    if let Some(kind) = crypto_part_kind(part) {
        classification.crypto_parts.push(CryptoPart {
            path: path.to_vec(),
            kind,
            content_type,
            protocol: part.ctype.params.get("protocol").cloned(),
            smime_type: part.ctype.params.get("smime-type").cloned(),
        });
    }
}

fn crypto_part_kind(part: &mailparse::ParsedMail<'_>) -> Option<CryptoPartKind> {
    match part.ctype.mimetype.to_ascii_lowercase().as_str() {
        "multipart/encrypted" => Some(CryptoPartKind::EncryptedContainer),
        "multipart/signed" => Some(CryptoPartKind::SignedContainer),
        "application/pgp-encrypted" => Some(CryptoPartKind::OpenPgpEncrypted),
        "application/pgp-signature" => Some(CryptoPartKind::OpenPgpSignature),
        "application/pgp-keys" => Some(CryptoPartKind::OpenPgpKey),
        "application/pkcs7-signature" | "application/x-pkcs7-signature" => {
            Some(CryptoPartKind::SmimeSignature)
        }
        "application/pkcs7-mime" | "application/x-pkcs7-mime" => match part
            .ctype
            .params
            .get("smime-type")
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("enveloped-data" | "authenveloped-data") => Some(CryptoPartKind::SmimeEncrypted),
            Some("signed-data") => Some(CryptoPartKind::SmimeSigned),
            _ => Some(CryptoPartKind::SmimeOther),
        },
        _ => None,
    }
}

fn ensure_decode_may_fit(
    part: &mailparse::ParsedMail<'_>,
    description: &str,
    limit: usize,
) -> anyhow::Result<()> {
    let raw = encoded_body_bytes(part);
    let Ok(encoding) = transfer_encoding(part) else {
        // The decoder will preserve this as a per-part error or warning. An
        // unknown encoding does not justify guessing a decoded-size expansion.
        return Ok(());
    };
    let upper_bound = match encoding {
        TransferEncoding::Identity | TransferEncoding::QuotedPrintable => raw.len(),
        TransferEncoding::Base64 => {
            raw.iter()
                .filter(|byte| !byte.is_ascii_whitespace())
                .count()
                .saturating_add(3)
                / 4
                * 3
        }
    };
    if upper_bound > limit {
        return Err(MimeLimitError::DecodedPart {
            part: description.to_string(),
            limit,
        }
        .into());
    }
    Ok(())
}

fn encoded_body_bytes<'a>(part: &'a mailparse::ParsedMail<'a>) -> &'a [u8] {
    match part.get_body_encoded() {
        Body::Base64(body) | Body::QuotedPrintable(body) => body.get_raw(),
        Body::SevenBit(body) | Body::EightBit(body) => body.get_raw(),
        Body::Binary(body) => body.get_raw(),
    }
}

fn preflight_mime_structure(bytes: &[u8], limits: MimeLimits) -> anyhow::Result<()> {
    let mut parts = 0_usize;
    preflight_part(bytes, 0, limits, &mut parts)
}

fn preflight_part(
    raw: &[u8],
    depth: usize,
    limits: MimeLimits,
    parts: &mut usize,
) -> anyhow::Result<()> {
    if depth > limits.max_depth {
        return Err(MimeLimitError::Depth {
            actual: depth,
            limit: limits.max_depth,
        }
        .into());
    }
    *parts = parts.saturating_add(1);
    if *parts > limits.max_parts {
        return Err(MimeLimitError::Parts {
            limit: limits.max_parts,
        }
        .into());
    }

    let (headers, body_start) = mailparse::parse_headers(raw)?;
    let Some(content_type) = headers.get_first_value("Content-Type") else {
        return Ok(());
    };
    let content_type = mailparse::parse_content_type(&content_type);
    if !content_type.mimetype.starts_with("multipart/") || raw.len() <= body_start {
        return Ok(());
    }
    let Some(boundary) = content_type.params.get("boundary") else {
        return Ok(());
    };
    let mut delimiter = Vec::with_capacity(boundary.len().saturating_add(2));
    delimiter.extend_from_slice(b"--");
    delimiter.extend_from_slice(boundary.as_bytes());
    let Some(first_boundary) = find_line_prefix(raw, body_start, &delimiter) else {
        return Ok(());
    };

    let mut boundary_end = first_boundary + delimiter.len();
    while let Some(part_start) = find_subslice(raw, boundary_end, b"\n").map(|index| index + 1) {
        let next_boundary = find_line_prefix(raw, part_start, &delimiter);
        let part_end = next_boundary
            .map(|index| strip_trailing_line_ending(raw, part_start, index))
            .unwrap_or(raw.len());
        preflight_part(&raw[part_start..part_end], depth + 1, limits, parts)?;
        boundary_end = next_boundary
            .map(|index| index + delimiter.len())
            .unwrap_or(raw.len());
        if boundary_end + 2 > raw.len()
            || (raw[boundary_end] == b'-' && raw[boundary_end + 1] == b'-')
        {
            break;
        }
    }
    Ok(())
}

fn strip_trailing_line_ending(raw: &[u8], start: usize, mut end: usize) -> usize {
    if end > start && raw[end - 1] == b'\n' {
        end -= 1;
        if end > start && raw[end - 1] == b'\r' {
            end -= 1;
        }
    }
    end
}

fn find_subslice(haystack: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || start > haystack.len() || needle.len() > haystack.len() {
        return None;
    }
    haystack[start..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|index| start + index)
}

// Match mailparse's multipart delimiter recognition: the delimiter must be at
// the beginning of the searched body or immediately follow LF.
fn find_line_prefix(haystack: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    let mut search_from = start;
    while let Some(index) = find_subslice(haystack, search_from, needle) {
        if index == start || haystack[index - 1] == b'\n' {
            return Some(index);
        }
        search_from = index + 1;
    }
    None
}

fn part_description(mimetype: &str, filename: Option<&str>, is_attachment: bool) -> String {
    match filename {
        Some(filename) => format!("attachment {filename:?} ({mimetype})"),
        None if is_attachment => format!("attachment ({mimetype})"),
        None => format!("{mimetype} MIME part"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferEncoding {
    Identity,
    Base64,
    QuotedPrintable,
}

fn transfer_encoding(part: &mailparse::ParsedMail<'_>) -> anyhow::Result<TransferEncoding> {
    let Some(raw) = part.headers.get_first_value("Content-Transfer-Encoding") else {
        return Ok(TransferEncoding::Identity);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "7bit" | "8bit" | "binary" => Ok(TransferEncoding::Identity),
        "base64" => Ok(TransferEncoding::Base64),
        "quoted-printable" => Ok(TransferEncoding::QuotedPrintable),
        _ => anyhow::bail!("unsupported Content-Transfer-Encoding {raw:?}"),
    }
}

fn validate_text_transfer_encoding(
    part: &mailparse::ParsedMail<'_>,
) -> anyhow::Result<Vec<String>> {
    match transfer_encoding(part)? {
        TransferEncoding::QuotedPrintable => {
            let Body::QuotedPrintable(body) = part.get_body_encoded() else {
                anyhow::bail!("quoted-printable body was not recognized by the MIME parser");
            };
            validate_quoted_printable(body.get_raw())
        }
        TransferEncoding::Identity | TransferEncoding::Base64 => Ok(Vec::new()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DecodedPart {
    bytes: Vec<u8>,
    warnings: Vec<String>,
}

fn decode_part_bytes(part: &mailparse::ParsedMail<'_>) -> anyhow::Result<DecodedPart> {
    match transfer_encoding(part)? {
        TransferEncoding::QuotedPrintable => {
            let Body::QuotedPrintable(body) = part.get_body_encoded() else {
                anyhow::bail!("quoted-printable body was not recognized by the MIME parser");
            };
            let warnings = validate_quoted_printable(body.get_raw())?;
            let bytes =
                quoted_printable::decode(body.get_raw(), quoted_printable::ParseMode::Robust)
                    .context("decoding quoted-printable transfer encoding")?;
            Ok(DecodedPart { bytes, warnings })
        }
        TransferEncoding::Identity | TransferEncoding::Base64 => Ok(DecodedPart {
            bytes: part.get_body_raw()?,
            warnings: Vec::new(),
        }),
    }
}

fn validate_quoted_printable(raw: &[u8]) -> anyhow::Result<Vec<String>> {
    if let Some((offset, byte)) = raw
        .iter()
        .copied()
        .enumerate()
        .find(|(_, byte)| !matches!(byte, b'\t' | b'\r' | b'\n' | b' '..=b'~'))
    {
        anyhow::bail!(
            "malformed quoted-printable transfer encoding: invalid byte 0x{byte:02X} at offset {offset}"
        );
    }

    let non_crlf_line_endings = raw.iter().enumerate().any(|(index, byte)| match byte {
        b'\n' => index == 0 || raw[index - 1] != b'\r',
        b'\r' => raw.get(index + 1) != Some(&b'\n'),
        _ => false,
    });

    let mut line_length = 0usize;
    let mut overlong_line = false;
    let mut index = 0usize;
    while index < raw.len() {
        match raw[index] {
            b'\r' => {
                overlong_line |= line_length > 76;
                line_length = 0;
                if raw.get(index + 1) == Some(&b'\n') {
                    index += 2;
                } else {
                    index += 1;
                }
            }
            b'\n' => {
                overlong_line |= line_length > 76;
                line_length = 0;
                index += 1;
            }
            _ => {
                line_length += 1;
                index += 1;
            }
        }
    }
    overlong_line |= line_length > 76;

    let mut lowercase_hex = false;
    let mut index = 0usize;
    while let Some(&byte) = raw.get(index) {
        if byte != b'=' {
            index += 1;
            continue;
        }
        match (raw.get(index + 1), raw.get(index + 2)) {
            (Some(b'\r'), Some(b'\n')) => index += 3,
            (Some(b'\n'), _) => index += 2,
            (Some(&upper), Some(&lower))
                if upper.is_ascii_hexdigit() && lower.is_ascii_hexdigit() =>
            {
                lowercase_hex |= matches!(upper, b'a'..=b'f') || matches!(lower, b'a'..=b'f');
                index += 3;
            }
            (None, _) | (Some(_), None) => anyhow::bail!(
                "malformed quoted-printable transfer encoding: incomplete escape at offset {index}"
            ),
            _ => anyhow::bail!(
                "malformed quoted-printable transfer encoding: invalid escape at offset {index}"
            ),
        }
    }

    let mut warnings = Vec::new();
    if lowercase_hex {
        warnings.push("quoted-printable hex escape uses lowercase digits".to_string());
    }
    if non_crlf_line_endings {
        warnings.push("quoted-printable body uses non-CRLF line endings".to_string());
    }
    if overlong_line {
        warnings.push("quoted-printable line exceeds 76 bytes".to_string());
    }
    Ok(warnings)
}

/// Extract every attachment, returning an error if any attachment cannot be decoded.
///
/// Use [`extract_attachments_detailed`] when partial results are useful. The strict
/// contract prevents callers from mistaking a partially decoded message for a
/// complete extraction.
pub fn extract_attachments_from_file(
    path: impl AsRef<Path>,
) -> anyhow::Result<Vec<ExtractedAttachment>> {
    extract_attachments_from_file_with_limits(path, MimeLimits::default())
}

pub fn extract_attachments_from_file_with_limits(
    path: impl AsRef<Path>,
    limits: MimeLimits,
) -> anyhow::Result<Vec<ExtractedAttachment>> {
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("opening message {}", path.display()))?;
    extract_attachments_from_reader_with_limits(file, limits)
}

/// Extract every attachment from an RFC 5322 message, returning an error if any
/// attachment cannot be decoded.
pub fn extract_attachments(bytes: &[u8]) -> anyhow::Result<Vec<ExtractedAttachment>> {
    extract_attachments_with_limits(bytes, MimeLimits::default())
}

pub fn extract_attachments_with_limits(
    bytes: &[u8],
    limits: MimeLimits,
) -> anyhow::Result<Vec<ExtractedAttachment>> {
    let report = extract_attachments_detailed_with_limits(bytes, limits)?;
    if let Some(failure) = report.failures.first() {
        anyhow::bail!("{}", failure.error);
    }
    Ok(report.attachments)
}

pub fn extract_attachments_from_reader(
    reader: impl Read,
) -> anyhow::Result<Vec<ExtractedAttachment>> {
    extract_attachments_from_reader_with_limits(reader, MimeLimits::default())
}

pub fn extract_attachments_from_reader_with_limits(
    reader: impl Read,
    limits: MimeLimits,
) -> anyhow::Result<Vec<ExtractedAttachment>> {
    let report = extract_attachments_from_reader_detailed_with_limits(reader, limits)?;
    if let Some(failure) = report.failures.first() {
        anyhow::bail!("{}", failure.error);
    }
    Ok(report.attachments)
}

/// Extract decodable attachments and report failures for individual MIME parts.
///
/// Each success and failure carries a stable depth-first attachment-part index, so
/// callers can display and later retrieve a good sibling even when an earlier part
/// is malformed.
pub fn extract_attachments_from_file_detailed(
    path: impl AsRef<Path>,
) -> anyhow::Result<AttachmentExtractionReport> {
    extract_attachments_from_file_detailed_with_limits(path, MimeLimits::default())
}

pub fn extract_attachments_from_file_detailed_with_limits(
    path: impl AsRef<Path>,
    limits: MimeLimits,
) -> anyhow::Result<AttachmentExtractionReport> {
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("opening message {}", path.display()))?;
    extract_attachments_from_reader_detailed_with_limits(file, limits)
}

/// Extract decodable attachments from an RFC 5322 message and report failures
/// for individual MIME parts.
pub fn extract_attachments_detailed(bytes: &[u8]) -> anyhow::Result<AttachmentExtractionReport> {
    extract_attachments_detailed_with_limits(bytes, MimeLimits::default())
}

pub fn extract_attachments_detailed_with_limits(
    bytes: &[u8],
    limits: MimeLimits,
) -> anyhow::Result<AttachmentExtractionReport> {
    anyhow::ensure!(
        bytes.len() <= limits.max_message_bytes,
        "message exceeds the {}-byte safety limit",
        limits.max_message_bytes
    );
    preflight_mime_structure(bytes, limits)?;
    let parsed = mailparse::parse_mail(bytes)?;
    let mut report = AttachmentExtractionReport::default();
    let mut next_part_index = 0usize;
    let mut total_decoded_bytes = 0usize;
    collect_attachment_data(
        &parsed,
        limits,
        false,
        &mut total_decoded_bytes,
        &mut next_part_index,
        &mut report,
    )?;
    Ok(report)
}

pub fn extract_attachments_from_reader_detailed(
    reader: impl Read,
) -> anyhow::Result<AttachmentExtractionReport> {
    extract_attachments_from_reader_detailed_with_limits(reader, MimeLimits::default())
}

pub fn extract_attachments_from_reader_detailed_with_limits(
    reader: impl Read,
    limits: MimeLimits,
) -> anyhow::Result<AttachmentExtractionReport> {
    let bytes = read_message_bytes_with_limit(reader, limits.max_message_bytes)?;
    extract_attachments_detailed_with_limits(&bytes, limits)
}

fn collect_attachment_data(
    part: &mailparse::ParsedMail<'_>,
    limits: MimeLimits,
    named_body_allowed: bool,
    total_decoded_bytes: &mut usize,
    next_part_index: &mut usize,
    report: &mut AttachmentExtractionReport,
) -> anyhow::Result<()> {
    if !part.subparts.is_empty() {
        let child_named_body_allowed =
            named_body_allowed || part.ctype.mimetype == "multipart/alternative";
        for subpart in &part.subparts {
            collect_attachment_data(
                subpart,
                limits,
                child_named_body_allowed,
                total_decoded_bytes,
                next_part_index,
                report,
            )?;
        }
        return Ok(());
    }

    let mimetype = part.ctype.mimetype.to_lowercase();
    let filename = part_filename(part);
    let explicitly_attached =
        part_is_explicit_attachment(part, filename.as_deref(), named_body_allowed);
    if !part_is_attachment_like(
        &mimetype,
        explicitly_attached,
        crypto_part_kind(part).is_some(),
    ) {
        return Ok(());
    }
    let part_index = *next_part_index;
    *next_part_index += 1;
    let filename = filename.unwrap_or_else(|| fallback_attachment_filename(part));
    let description = part_description(&mimetype, Some(&filename), true);
    ensure_decode_may_fit(part, &description, limits.max_decoded_part_bytes)?;
    match decode_part_bytes(part).with_context(|| format!("decoding {description}")) {
        Ok(decoded) => {
            if decoded.bytes.len() > limits.max_decoded_part_bytes {
                return Err(MimeLimitError::DecodedPart {
                    part: description,
                    limit: limits.max_decoded_part_bytes,
                }
                .into());
            }
            let total = total_decoded_bytes.checked_add(decoded.bytes.len()).ok_or(
                MimeLimitError::TotalDecoded {
                    limit: limits.max_total_decoded_bytes,
                },
            )?;
            if total > limits.max_total_decoded_bytes {
                return Err(MimeLimitError::TotalDecoded {
                    limit: limits.max_total_decoded_bytes,
                }
                .into());
            }
            *total_decoded_bytes = total;
            report.attachments.push(ExtractedAttachment {
                part_index,
                filename,
                content_type: mimetype,
                content_id: part.headers.get_first_value("Content-ID"),
                bytes: decoded.bytes,
                decode_warnings: decoded.warnings,
            });
        }
        Err(err) => report.failures.push(AttachmentDecodeFailure {
            part_index,
            filename,
            content_type: mimetype,
            error: format!("{err:#}"),
        }),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment_message(encoding: &str, payload: &str) -> Vec<u8> {
        format!(
            "MIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=x\r\n\r\n\
             --x\r\n\
             Content-Type: application/octet-stream; name=broken.bin\r\n\
             Content-Disposition: attachment; filename=broken.bin\r\n\
             Content-Transfer-Encoding: {encoding}\r\n\r\n\
             {payload}\r\n\
             --x--\r\n"
        )
        .into_bytes()
    }

    fn sibling_attachment_message() -> Vec<u8> {
        b"MIME-Version: 1.0\r\n\
          Content-Type: multipart/mixed; boundary=x\r\n\r\n\
          --x\r\n\
          Content-Type: application/octet-stream; name=broken.bin\r\n\
          Content-Disposition: attachment; filename=broken.bin\r\n\
          Content-Transfer-Encoding: base64\r\n\r\n\
          !!!!\r\n\
          --x\r\n\
          Content-Type: text/plain; name=good.txt\r\n\
          Content-Disposition: attachment; filename=good.txt\r\n\
          Content-Transfer-Encoding: base64\r\n\r\n\
          Z29vZCBzaWJsaW5n\r\n\
          --x--\r\n"
            .to_vec()
    }

    #[test]
    fn malformed_base64_text_records_a_decode_warning() {
        let raw = b"Content-Type: text/plain; charset=utf-8\r\n\
                    Content-Transfer-Encoding: base64\r\n\r\n!!!!";

        let parsed = parse_rfc5322(raw).expect("parse message structure");

        assert!(parsed.safe_body.is_empty());
        assert_eq!(parsed.decode_warnings.len(), 1);
        assert!(parsed.decode_warnings[0].contains("text/plain MIME part"));
        assert!(parsed.decode_warnings[0].contains("Base64 decode error"));
    }

    #[test]
    fn unsupported_transfer_encoding_records_a_warning_instead_of_using_raw_bytes() {
        let raw = b"Content-Type: text/plain; charset=utf-8\r\n\
                    Content-Transfer-Encoding: bas64\r\n\r\nraw encoded bytes";

        let parsed = parse_rfc5322(raw).expect("parse message structure");

        assert!(parsed.safe_body.is_empty());
        assert_eq!(parsed.decode_warnings.len(), 1);
        assert!(parsed.decode_warnings[0].contains("unsupported Content-Transfer-Encoding"));
        assert!(parsed.decode_warnings[0].contains("bas64"));
    }

    #[test]
    fn malformed_quoted_printable_text_is_not_rendered_ambiguously() {
        let raw = b"Content-Type: text/plain; charset=utf-8\r\n\
                    Content-Transfer-Encoding: quoted-printable\r\n\r\nvisible=ZZtext";

        let parsed = parse_rfc5322(raw).expect("parse message structure");

        assert!(parsed.safe_body.is_empty());
        assert_eq!(parsed.decode_warnings.len(), 1);
        assert!(parsed.decode_warnings[0].contains("Could not decode text/plain MIME part"));
        assert!(parsed.decode_warnings[0].contains("malformed quoted-printable transfer encoding"));
    }

    #[test]
    fn recoverable_quoted_printable_variants_decode_with_warnings() {
        let cases = [
            (
                "caf=c3=a9".to_string(),
                "café".to_string(),
                "lowercase digits",
            ),
            (
                "first\nsecond".to_string(),
                "first\r\nsecond".to_string(),
                "non-CRLF line endings",
            ),
            ("x".repeat(77), "x".repeat(77), "exceeds 76 bytes"),
        ];

        for (payload, expected, warning) in cases {
            let raw = format!(
                "Content-Type: text/plain; charset=utf-8\r\n\
                 Content-Transfer-Encoding: quoted-printable\r\n\r\n{payload}"
            );
            let parsed = parse_rfc5322(raw.as_bytes()).expect("parse recoverable message");

            assert_eq!(parsed.safe_body, expected, "{payload:?}");
            assert_eq!(parsed.decode_warnings.len(), 1, "{payload:?}");
            assert!(parsed.decode_warnings[0].contains(warning), "{parsed:?}");
        }
    }

    #[test]
    fn ambiguous_quoted_printable_input_is_rejected() {
        for raw in [
            b"bad=ZZ".as_slice(),
            b"bad=".as_slice(),
            &[b'b', b'a', b'd', 0x80],
        ] {
            let error = validate_quoted_printable(raw)
                .expect_err("ambiguous quoted-printable data must be rejected")
                .to_string();
            assert!(
                error.contains("malformed quoted-printable transfer encoding"),
                "{error}"
            );
        }
    }

    #[test]
    fn malformed_attachment_transfer_encodings_return_errors() {
        for (encoding, payload, expected_error) in [
            ("base64", "!!!!", "Base64 decode error"),
            (
                "quoted-printable",
                "broken=ZZ",
                "malformed quoted-printable transfer encoding",
            ),
            ("bas64", "aGVsbG8=", "unsupported Content-Transfer-Encoding"),
        ] {
            let err = extract_attachments(&attachment_message(encoding, payload))
                .expect_err("malformed attachment must not become empty data");
            let error = format!("{err:#}");
            assert!(
                error.contains("attachment \"broken.bin\""),
                "missing attachment context for {encoding}: {error}"
            );
            assert!(
                error.contains(expected_error),
                "unexpected {encoding} error: {error}"
            );
        }
    }

    #[test]
    fn valid_attachment_transfer_encodings_still_decode() {
        for (encoding, payload, expected) in [
            ("base64", "aGVs bG8=\r\n", b"hello".as_slice()),
            (
                "quoted-printable",
                "hello=20world",
                b"hello world".as_slice(),
            ),
        ] {
            let attachments = extract_attachments(&attachment_message(encoding, payload))
                .expect("decode valid attachment");
            assert_eq!(attachments.len(), 1);
            assert_eq!(attachments[0].bytes, expected, "{encoding}");
        }
    }

    #[test]
    fn detailed_extraction_keeps_good_siblings_and_stable_part_indexes() {
        let raw = sibling_attachment_message();

        let report = extract_attachments_detailed(&raw).expect("extract attachment report");

        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].part_index, 0);
        assert_eq!(report.failures[0].filename, "broken.bin");
        assert!(report.failures[0].error.contains("Base64 decode error"));
        assert_eq!(report.attachments.len(), 1);
        assert_eq!(report.attachments[0].part_index, 1);
        assert_eq!(report.attachments[0].filename, "good.txt");
        assert_eq!(report.attachments[0].bytes, b"good sibling");

        let strict_error = extract_attachments(&raw)
            .expect_err("strict extraction must reject any corrupt attachment");
        assert!(format!("{strict_error:#}").contains("broken.bin"));
    }

    #[test]
    fn malformed_attachment_metadata_remains_available() {
        let parsed = parse_rfc5322(&sibling_attachment_message()).expect("parse message");

        assert_eq!(parsed.attachments.len(), 2);
        assert_eq!(
            parsed.attachments[0].filename.as_deref(),
            Some("broken.bin")
        );
        assert!(parsed.attachments[0].decode_error.is_some());
        assert_eq!(parsed.attachments[1].filename.as_deref(), Some("good.txt"));
        assert!(parsed.attachments[1].decode_error.is_none());
    }

    #[test]
    fn invalid_utf8_headers_and_text_body_remain_readable() {
        let raw = b"Subject: before \xff after\r\n\
                    Content-Type: text/plain; charset=utf-8\r\n\r\n\
                    body before \xff body after";

        let parsed = parse_rfc5322(raw).expect("parse binary-tolerant message");

        assert!(parsed.subject.starts_with("before "));
        assert!(parsed.subject.ends_with(" after"));
        assert!(parsed.safe_body.starts_with("body before "));
        assert!(parsed.safe_body.ends_with(" body after"));
    }

    #[test]
    fn multipart_alternative_selects_by_media_type_in_either_order() {
        for parts in [
            "--alt\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>HTML body</p>\r\n\
             --alt\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nPlain body\r\n",
            "--alt\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nPlain body\r\n\
             --alt\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>HTML body</p>\r\n",
        ] {
            let raw = format!(
                "MIME-Version: 1.0\r\n\
                 Content-Type: multipart/alternative; boundary=alt\r\n\r\n\
                 {parts}--alt--\r\n"
            );
            let parsed = parse_rfc5322(raw.as_bytes()).expect("parse alternative message");

            assert_eq!(parsed.text_body, "Plain body");
            assert_eq!(parsed.safe_body, "Plain body");
            assert_eq!(parsed.html_body.as_deref(), Some("<p>HTML body</p>"));
        }
    }

    #[test]
    fn multipart_alternative_uses_last_supported_duplicate() {
        let raw = b"MIME-Version: 1.0\r\n\
                    Content-Type: multipart/alternative; boundary=alt\r\n\r\n\
                    --alt\r\nContent-Type: text/plain\r\n\r\nOlder plain\r\n\
                    --alt\r\nContent-Type: text/plain\r\n\r\nPreferred plain\r\n\
                    --alt--\r\n";

        let parsed = parse_rfc5322(raw).expect("parse duplicate alternatives");

        assert_eq!(parsed.text_body, "Preferred plain");
        assert_eq!(parsed.safe_body, "Preferred plain");
    }

    #[test]
    fn named_plain_and_html_alternatives_remain_body_representations() {
        let raw = b"MIME-Version: 1.0\r\n\
                    Content-Type: multipart/alternative; boundary=alt\r\n\r\n\
                    --alt\r\n\
                    Content-Type: text/plain; charset=utf-8; name=body.txt\r\n\
                    Content-Disposition: inline; filename=body.txt\r\n\r\n\
                    Named plain body\r\n\
                    --alt\r\n\
                    Content-Type: text/html; charset=utf-8; name=body.html\r\n\r\n\
                    <p>Named HTML body</p>\r\n\
                    --alt--\r\n";

        let parsed = parse_rfc5322(raw).expect("parse named alternatives");

        assert_eq!(parsed.text_body, "Named plain body");
        assert_eq!(parsed.safe_body, "Named plain body");
        assert_eq!(parsed.html_body.as_deref(), Some("<p>Named HTML body</p>"));
        assert!(parsed.attachments.is_empty());
        assert!(
            extract_attachments(raw)
                .expect("extract named alternatives")
                .is_empty()
        );
    }

    #[test]
    fn genuinely_attached_text_parts_stay_attachments() {
        let mixed = b"MIME-Version: 1.0\r\n\
                      Content-Type: multipart/mixed; boundary=mixed\r\n\r\n\
                      --mixed\r\nContent-Type: text/plain\r\n\r\nVisible body\r\n\
                      --mixed\r\nContent-Type: text/plain; name=notes.txt\r\n\r\nAttached notes\r\n\
                      --mixed--\r\n";
        let parsed = parse_rfc5322(mixed).expect("parse named mixed part");
        assert_eq!(parsed.safe_body, "Visible body");
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].filename.as_deref(), Some("notes.txt"));

        let alternative_attachment = b"MIME-Version: 1.0\r\n\
                                       Content-Type: multipart/alternative; boundary=alt\r\n\r\n\
                                       --alt\r\n\
                                       Content-Type: text/plain; name=notes.txt\r\n\
                                       Content-Disposition: attachment; filename=notes.txt\r\n\r\n\
                                       Attached notes\r\n\
                                       --alt\r\nContent-Type: text/html\r\n\r\n<p>Visible HTML</p>\r\n\
                                       --alt--\r\n";
        let parsed =
            parse_rfc5322(alternative_attachment).expect("parse attached alternative part");
        assert!(parsed.text_body.is_empty());
        assert_eq!(parsed.html_body.as_deref(), Some("<p>Visible HTML</p>"));
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(parsed.attachments[0].filename.as_deref(), Some("notes.txt"));
    }

    #[test]
    fn multipart_related_uses_the_declared_root_or_first_part_fallback() {
        let declared = b"MIME-Version: 1.0\r\n\
                         Content-Type: multipart/related; boundary=rel; start=\"<body>\"\r\n\r\n\
                         --rel\r\nContent-Type: image/png\r\nContent-ID: <image>\r\n\r\npng\r\n\
                         --rel\r\nContent-Type: text/html\r\nContent-ID: <body>\r\n\r\n<p>root</p>\r\n\
                         --rel--\r\n";
        let parsed = parse_rfc5322(declared).expect("parse related start root");
        assert_eq!(parsed.html_body.as_deref(), Some("<p>root</p>"));

        let fallback = b"MIME-Version: 1.0\r\n\
                         Content-Type: multipart/related; boundary=rel\r\n\r\n\
                         --rel\r\nContent-Type: text/plain\r\n\r\nfirst root\r\n\
                         --rel\r\nContent-Type: text/html\r\n\r\n<p>not root</p>\r\n\
                         --rel--\r\n";
        let parsed = parse_rfc5322(fallback).expect("parse related first-part fallback");
        assert_eq!(parsed.safe_body, "first root");
        assert!(parsed.html_body.is_none());
    }

    #[test]
    fn filename_less_calendar_is_classified_and_extractable_not_message_body() {
        let raw = b"MIME-Version: 1.0\r\n\
                    Content-Type: multipart/mixed; boundary=mixed\r\n\r\n\
                    --mixed\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nMeeting details\r\n\
                    --mixed\r\nContent-Type: text/calendar; method=REQUEST; charset=utf-8\r\n\r\n\
                    BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n\
                    --mixed--\r\n";

        let parsed = parse_rfc5322(raw).expect("parse calendar message");

        assert_eq!(parsed.safe_body, "Meeting details");
        assert!(parsed.classification.has_calendar());
        assert!(parsed.classification.has_invitation());
        assert_eq!(parsed.classification.calendar_parts.len(), 1);
        assert_eq!(
            parsed.classification.calendar_parts[0].method.as_deref(),
            Some("REQUEST")
        );
        assert_eq!(parsed.classification.calendar_parts[0].filename, None);
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(
            parsed.attachments[0].filename.as_deref(),
            Some("invitation.ics")
        );

        let extracted = extract_attachments(raw).expect("extract filename-less calendar");
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0].filename, "invitation.ics");
        assert_eq!(extracted[0].content_type, "text/calendar");
        assert!(extracted[0].bytes.starts_with(b"BEGIN:VCALENDAR"));

        let misleading_filename = parse_rfc5322(
            b"Content-Type: text/plain; name=invite.ics\r\n\
              Content-Disposition: attachment; filename=invite.ics\r\n\r\nnot a calendar",
        )
        .expect("parse misleading filename");
        assert!(!misleading_filename.classification.has_calendar());
        assert!(!misleading_filename.classification.has_invitation());
    }

    #[test]
    fn uppercase_filename_less_media_types_use_specific_fallback_names() {
        let calendar = b"Content-Type: TEXT/CALENDAR; METHOD=REQUEST\r\n\r\n\
                         BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n";
        let parsed = parse_rfc5322(calendar).expect("parse uppercase calendar media type");
        assert!(parsed.classification.has_invitation());
        assert_eq!(
            parsed.attachments[0].filename.as_deref(),
            Some("invitation.ics")
        );
        assert_eq!(
            extract_attachments(calendar)
                .expect("extract uppercase calendar")
                .remove(0)
                .filename,
            "invitation.ics"
        );

        let attached_message = b"Content-Type: MESSAGE/RFC822\r\n\r\n\
                                 From: sender@example.test\r\n\
                                 To: recipient@example.test\r\n\
                                 Subject: nested\r\n\r\nbody\r\n";
        let parsed =
            parse_rfc5322(attached_message).expect("parse uppercase attached-message media type");
        assert_eq!(
            parsed.attachments[0].filename.as_deref(),
            Some("attached-message.eml")
        );
        assert_eq!(
            extract_attachments(attached_message)
                .expect("extract uppercase attached message")
                .remove(0)
                .filename,
            "attached-message.eml"
        );
    }

    #[test]
    fn crypto_classification_uses_exact_media_types_and_parameters() {
        let false_positive = parse_rfc5322(
            b"Content-Type: text/plain\r\n\r\n\
              strings multipart/encrypted application/pgp-signature and smime.p7m",
        )
        .expect("parse ordinary text");
        assert!(!false_positive.classification.has_encrypted());
        assert!(!false_positive.classification.has_signed());

        let encrypted = parse_rfc5322(
            b"MIME-Version: 1.0\r\n\
              Content-Type: multipart/encrypted; boundary=enc; protocol=\"application/pgp-encrypted\"\r\n\r\n\
              --enc\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n\
              --enc\r\nContent-Type: application/octet-stream\r\n\r\nCiphertext\r\n\
              --enc--\r\n",
        )
        .expect("parse encrypted structure");
        assert!(encrypted.classification.has_encrypted());
        assert!(!encrypted.classification.has_signed());
        assert_eq!(
            encrypted.classification.crypto_parts[0].path,
            Vec::<usize>::new()
        );
        assert_eq!(
            encrypted.classification.crypto_parts[0].protocol.as_deref(),
            Some("application/pgp-encrypted")
        );

        let signed = parse_rfc5322(
            b"MIME-Version: 1.0\r\n\
              Content-Type: multipart/signed; boundary=sig; protocol=\"application/pgp-signature\"\r\n\r\n\
              --sig\r\nContent-Type: text/plain\r\n\r\nSigned body\r\n\
              --sig\r\nContent-Type: application/pgp-signature\r\n\r\nSignature\r\n\
              --sig--\r\n",
        )
        .expect("parse signed structure");
        assert!(signed.classification.has_signed());
        assert_eq!(signed.safe_body, "Signed body");
        assert!(signed.attachments.is_empty());

        let smime = parse_rfc5322(
            b"Content-Type: application/pkcs7-mime; smime-type=Enveloped-Data\r\n\r\nopaque",
        )
        .expect("parse S/MIME classification");
        assert!(smime.classification.has_encrypted());
        assert_eq!(
            smime.classification.crypto_parts[0].kind,
            CryptoPartKind::SmimeEncrypted
        );

        let near_match =
            parse_rfc5322(b"Content-Type: application/not-pkcs7-signature\r\n\r\nopaque")
                .expect("parse near-match media type");
        assert!(!near_match.classification.has_signed());
        assert!(near_match.classification.crypto_parts.is_empty());
    }

    fn nested_multipart(depth: usize) -> Vec<u8> {
        let mut message = "Content-Type: text/plain\r\n\r\nleaf".to_string();
        for index in 0..depth {
            let boundary = format!("nested-{index}");
            message = format!(
                "MIME-Version: 1.0\r\n\
                 Content-Type: multipart/mixed; boundary={boundary}\r\n\r\n\
                 --{boundary}\r\n{message}\r\n--{boundary}--\r\n"
            );
        }
        message.into_bytes()
    }

    #[test]
    fn mime_depth_is_rejected_before_recursive_mailparse() {
        let limits = MimeLimits {
            max_depth: 2,
            ..MimeLimits::default()
        };

        parse_rfc5322_with_limits(&nested_multipart(2), limits).expect("depth at limit");
        let error =
            parse_rfc5322_with_limits(&nested_multipart(3), limits).expect_err("depth over limit");
        let limit = error
            .downcast_ref::<MimeLimitError>()
            .expect("typed depth limit");
        assert_eq!(
            limit,
            &MimeLimitError::Depth {
                actual: 3,
                limit: 2
            }
        );
    }

    #[test]
    fn mime_part_count_is_bounded() {
        let raw = b"MIME-Version: 1.0\r\n\
                    Content-Type: multipart/mixed; boundary=x\r\n\r\n\
                    --x\r\nContent-Type: text/plain\r\n\r\none\r\n\
                    --x\r\nContent-Type: text/plain\r\n\r\ntwo\r\n\
                    --x--\r\n";
        let limits = MimeLimits {
            max_parts: 2,
            ..MimeLimits::default()
        };

        let error = parse_rfc5322_with_limits(raw, limits).expect_err("too many parts");

        assert_eq!(
            error.downcast_ref::<MimeLimitError>(),
            Some(&MimeLimitError::Parts { limit: 2 })
        );
    }

    #[test]
    fn decoded_part_and_aggregate_sizes_are_bounded() {
        let part_limits = MimeLimits {
            max_decoded_part_bytes: 4,
            ..MimeLimits::default()
        };
        let part_error =
            parse_rfc5322_with_limits(b"Content-Type: text/plain\r\n\r\n12345", part_limits)
                .expect_err("oversized decoded part");
        assert!(matches!(
            part_error.downcast_ref::<MimeLimitError>(),
            Some(MimeLimitError::DecodedPart { limit: 4, .. })
        ));

        let aggregate_limits = MimeLimits {
            max_decoded_part_bytes: 4,
            max_total_decoded_bytes: 5,
            ..MimeLimits::default()
        };
        let aggregate_error = parse_rfc5322_with_limits(
            b"MIME-Version: 1.0\r\n\
              Content-Type: multipart/mixed; boundary=x\r\n\r\n\
              --x\r\nContent-Type: text/plain\r\n\r\none\r\n\
              --x\r\nContent-Type: text/plain\r\n\r\ntwo\r\n\
              --x--\r\n",
            aggregate_limits,
        )
        .expect_err("oversized decoded aggregate");
        assert_eq!(
            aggregate_error.downcast_ref::<MimeLimitError>(),
            Some(&MimeLimitError::TotalDecoded { limit: 5 })
        );
    }

    #[test]
    fn reader_apis_enforce_encoded_size_without_path_reopen() {
        let limits = MimeLimits {
            max_message_bytes: 8,
            ..MimeLimits::default()
        };

        let error = parse_reader_with_limits(std::io::Cursor::new(b"123456789"), limits)
            .expect_err("oversized reader input")
            .to_string();

        assert!(error.contains("8-byte safety limit"), "{error}");
    }
}
