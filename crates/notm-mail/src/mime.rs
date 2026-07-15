use std::{collections::BTreeMap, path::Path};

use anyhow::Context;
use mailparse::{MailHeaderMap, body::Body};
use serde::{Deserialize, Serialize};

use crate::html_sanitize::html_to_safe_text;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
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
    let mut decode_warnings = Vec::new();
    walk_part(
        &parsed,
        0,
        &mut text_parts,
        &mut html_parts,
        &mut attachments,
        &mut tree,
        &mut decode_warnings,
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
        decode_warnings,
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
    decode_warnings: &mut Vec<String>,
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
                decode_warnings,
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

    let attachment_like =
        is_attachment || (!mimetype.starts_with("text/") && mimetype != "message/rfc822");
    let description = part_description(&mimetype, filename.as_deref(), attachment_like);
    if attachment_like {
        match decode_part_bytes(part) {
            Ok(decoded) => {
                for warning in &decoded.warnings {
                    decode_warnings
                        .push(format!("Decoded non-conformant {description}: {warning}"));
                }
                attachments.push(Attachment {
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
                decode_warnings.push(format!("Could not decode {description}: {error}"));
                attachments.push(Attachment {
                    filename,
                    content_type: mimetype,
                    size: 0,
                    content_id,
                    decode_warnings: Vec::new(),
                    decode_error: Some(error),
                });
            }
        }
        return;
    }

    match validate_text_transfer_encoding(part) {
        Ok(warnings) => {
            for warning in warnings {
                decode_warnings.push(format!("Decoded non-conformant {description}: {warning}"));
            }
        }
        Err(err) => {
            decode_warnings.push(format!("Could not decode {description}: {err:#}"));
            return;
        }
    }
    let destination = if mimetype == "text/html" {
        html_parts
    } else {
        text_parts
    };
    match part.get_body() {
        Ok(body) => destination.push(body),
        Err(err) => {
            decode_warnings.push(format!("Could not decode {description}: {err}"));
        }
    }
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
    let bytes = std::fs::read(path)?;
    extract_attachments(&bytes)
}

/// Extract every attachment from an RFC 5322 message, returning an error if any
/// attachment cannot be decoded.
pub fn extract_attachments(bytes: &[u8]) -> anyhow::Result<Vec<ExtractedAttachment>> {
    let report = extract_attachments_detailed(bytes)?;
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
    let bytes = std::fs::read(path)?;
    extract_attachments_detailed(&bytes)
}

/// Extract decodable attachments from an RFC 5322 message and report failures
/// for individual MIME parts.
pub fn extract_attachments_detailed(bytes: &[u8]) -> anyhow::Result<AttachmentExtractionReport> {
    let parsed = mailparse::parse_mail(bytes)?;
    let mut report = AttachmentExtractionReport::default();
    let mut next_part_index = 0usize;
    collect_attachment_data(&parsed, &mut next_part_index, &mut report);
    Ok(report)
}

fn collect_attachment_data(
    part: &mailparse::ParsedMail<'_>,
    next_part_index: &mut usize,
    report: &mut AttachmentExtractionReport,
) {
    if !part.subparts.is_empty() {
        for subpart in &part.subparts {
            collect_attachment_data(subpart, next_part_index, report);
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
    let part_index = *next_part_index;
    *next_part_index += 1;
    let filename = filename.unwrap_or_else(|| "attachment.bin".to_string());
    let description = part_description(&mimetype, Some(&filename), true);
    match decode_part_bytes(part).with_context(|| format!("decoding {description}")) {
        Ok(decoded) => report.attachments.push(ExtractedAttachment {
            part_index,
            filename,
            content_type: mimetype,
            content_id: part.headers.get_first_value("Content-ID"),
            bytes: decoded.bytes,
            decode_warnings: decoded.warnings,
        }),
        Err(err) => report.failures.push(AttachmentDecodeFailure {
            part_index,
            filename,
            content_type: mimetype,
            error: format!("{err:#}"),
        }),
    }
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
}
