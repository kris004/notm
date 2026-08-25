use serde::{Deserialize, Serialize};

/// Editable message fields carried by an RFC 6068 `mailto` URI.
///
/// Header fields that notm cannot represent safely in its composer are ignored.
/// In particular, a URI cannot choose the sender, add attachments, or configure
/// transport-related headers.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailtoRequest {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
}

pub fn parse_mailto_uri(uri: &str) -> anyhow::Result<MailtoRequest> {
    anyhow::ensure!(
        uri.get(..7)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("mailto:")),
        "URI scheme must be mailto"
    );

    let without_scheme = &uri[7..];
    anyhow::ensure!(
        !without_scheme.starts_with("//"),
        "mailto URI must not contain an authority"
    );

    // RFC 6068 says fragments are meaningless for mailto URIs and should be
    // ignored. Split before parsing so an encoded `%23` remains message data.
    let without_fragment = without_scheme
        .split_once('#')
        .map_or(without_scheme, |(value, _)| value);
    let (recipient_part, query) = without_fragment
        .split_once('?')
        .map_or((without_fragment, None), |(recipient, query)| {
            (recipient, Some(query))
        });

    let mut request = MailtoRequest::default();
    append_address_value(
        &mut request.to,
        percent_decode(recipient_part, "recipient")?,
        "recipient",
    )?;

    let mut subject_seen = false;
    let mut body_seen = false;
    if let Some(query) = query {
        for encoded_field in query.split('&').filter(|field| !field.is_empty()) {
            let (encoded_name, encoded_value) = encoded_field
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("mailto header field must contain '='"))?;
            let name = percent_decode(encoded_name, "header name")?;
            validate_header_name(&name)?;
            let value = percent_decode(encoded_value, &format!("{name} value"))?;

            match name.to_ascii_lowercase().as_str() {
                "to" => append_address_value(&mut request.to, value, "to")?,
                "cc" => append_address_value(&mut request.cc, value, "cc")?,
                "bcc" => append_address_value(&mut request.bcc, value, "bcc")?,
                "subject" if !subject_seen => {
                    ensure_header_value_is_safe(&value, "subject")?;
                    request.subject = value;
                    subject_seen = true;
                }
                "body" if !body_seen => {
                    ensure_body_value_is_safe(&value)?;
                    request.body = normalize_body_line_breaks(&value);
                    body_seen = true;
                }
                // RFC 6068 allows arbitrary header field names, but resolving
                // untrusted links into originator, routing, MIME, or attachment
                // state would be surprising and unsafe. The composer supports
                // only the explicitly handled editable fields above.
                _ => {}
            }
        }
    }

    Ok(request)
}

fn append_address_value(
    destination: &mut Vec<String>,
    value: String,
    field: &str,
) -> anyhow::Result<()> {
    ensure_header_value_is_safe(&value, field)?;
    let value = value.trim();
    if !value.is_empty() {
        destination.push(value.to_string());
    }
    Ok(())
}

fn validate_header_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| (33..=126).contains(&byte) && byte != b':'),
        "mailto header name is invalid"
    );
    Ok(())
}

fn ensure_header_value_is_safe(value: &str, field: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value.chars().any(char::is_control),
        "mailto {field} contains a control character"
    );
    Ok(())
}

fn ensure_body_value_is_safe(value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\r' | '\n' | '\t')),
        "mailto body contains an unsupported control character"
    );
    Ok(())
}

fn normalize_body_line_breaks(value: &str) -> String {
    value.replace("\r\n", "\n").replace('\r', "\n")
}

fn percent_decode(value: &str, field: &str) -> anyhow::Result<String> {
    let source = value.as_bytes();
    let mut decoded = Vec::with_capacity(source.len());
    let mut index = 0;
    while index < source.len() {
        if source[index] != b'%' {
            decoded.push(source[index]);
            index += 1;
            continue;
        }

        let high = source
            .get(index + 1)
            .and_then(|byte| hex_value(*byte))
            .ok_or_else(|| anyhow::anyhow!("mailto {field} has invalid percent-encoding"))?;
        let low = source
            .get(index + 2)
            .and_then(|byte| hex_value(*byte))
            .ok_or_else(|| anyhow::anyhow!("mailto {field} has invalid percent-encoding"))?;
        decoded.push((high << 4) | low);
        index += 3;
    }

    String::from_utf8(decoded)
        .map_err(|_| anyhow::anyhow!("mailto {field} is not valid UTF-8 after decoding"))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recipient_headers_utf8_and_plain_text_body() {
        let request = parse_mailto_uri(
            "mailto:alice@example.test,bob@example.test?\
             cc=carol@example.test&bcc=hidden@example.test&\
             subject=caf%C3%A9+notes&body=first%20line%0D%0Asecond%20line%20%26%20more",
        )
        .expect("valid mailto URI");

        assert_eq!(
            request,
            MailtoRequest {
                to: vec!["alice@example.test,bob@example.test".to_string()],
                cc: vec!["carol@example.test".to_string()],
                bcc: vec!["hidden@example.test".to_string()],
                subject: "café+notes".to_string(),
                body: "first line\nsecond line & more".to_string(),
            }
        );
    }

    #[test]
    fn combines_path_and_query_recipients_case_insensitively() {
        let request = parse_mailto_uri(
            "MAILTO:first@example.test?TO=second@example.test&\
             to=third@example.test&CC=copy@example.test#ignored-fragment",
        )
        .expect("valid mailto URI");

        assert_eq!(
            request.to,
            [
                "first@example.test",
                "second@example.test",
                "third@example.test"
            ]
        );
        assert_eq!(request.cc, ["copy@example.test"]);
    }

    #[test]
    fn ignores_headers_the_composer_cannot_represent() {
        let request = parse_mailto_uri(
            "mailto:person@example.test?from=attacker@example.test&\
             attachment=file%3A%2F%2F%2Ftmp%2Fsecret&\
             content-type=text%2Fhtml&subject=Safe",
        )
        .expect("valid mailto URI");

        assert_eq!(request.to, ["person@example.test"]);
        assert_eq!(request.subject, "Safe");
        assert!(request.cc.is_empty());
        assert!(request.bcc.is_empty());
        assert!(request.body.is_empty());
    }

    #[test]
    fn accepts_an_empty_recipient_for_an_editable_blank_message() {
        let request = parse_mailto_uri("mailto:?subject=Choose%20a%20recipient")
            .expect("mailto URI may omit the recipient");

        assert!(request.to.is_empty());
        assert_eq!(request.subject, "Choose a recipient");
    }

    #[test]
    fn rejects_malformed_or_unsafe_mailto_values() {
        for (uri, expected) in [
            ("https://example.test", "scheme must be mailto"),
            (
                "mailto://person@example.test",
                "must not contain an authority",
            ),
            ("mailto:person@example.test?subject", "must contain '='"),
            ("mailto:person%ZZ@example.test", "percent-encoding"),
            ("mailto:person@example.test?subject=%FF", "valid UTF-8"),
            (
                "mailto:person@example.test?subject=hello%0Aworld",
                "control character",
            ),
            ("mailto:person%0A@example.test", "control character"),
            (
                "mailto:person@example.test?body=hello%00world",
                "unsupported control character",
            ),
        ] {
            let error = parse_mailto_uri(uri).expect_err("URI should be rejected");
            assert!(
                error.to_string().contains(expected),
                "unexpected error for {uri:?}: {error:#}"
            );
        }
    }
}
