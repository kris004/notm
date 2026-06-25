use serde::{Deserialize, Serialize};

use crate::{
    address::{dedupe_addresses, exclude_identities, format_address, parse_address_list},
    compose::{ComposedMessage, Identity},
    html_sanitize::sanitize_html,
    mime::ParsedMessage,
    rfc5322::normalize_subject_for_reply,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReplyKind {
    Sender,
    All,
}

pub fn build_reply(
    original: &ParsedMessage,
    identity: &Identity,
    my_emails: &[String],
    kind: ReplyKind,
) -> ComposedMessage {
    let mut recipients = if !original.reply_to.trim().is_empty() {
        parse_address_list(&original.reply_to)
    } else {
        parse_address_list(&original.from)
    };
    let mut cc = Vec::new();
    if kind == ReplyKind::All {
        recipients.extend(parse_address_list(&original.to));
        cc.extend(parse_address_list(&original.cc));
        recipients = exclude_identities(dedupe_addresses(recipients), my_emails);
        cc = exclude_identities(dedupe_addresses(cc), my_emails);
    }
    let to = recipients.iter().map(format_address).collect::<Vec<_>>();
    let cc = cc.iter().map(format_address).collect::<Vec<_>>();
    let html_reply_quote = original
        .html_body
        .as_deref()
        .map(|html| html_quote_body(original, html));
    let visible_body = if html_reply_quote.is_some() {
        String::new()
    } else {
        quote_body(&original.safe_body)
    };
    let mut message = ComposedMessage::new(
        identity.formatted(),
        to,
        normalize_subject_for_reply(&original.subject),
        visible_body,
    );
    if let Some(html_quote) = html_reply_quote {
        message.text_reply_quote = Some(quote_body(&original.safe_body));
        message.html_reply_quote = Some(html_quote);
    }
    message.cc = cc;
    if !original.message_id.is_empty() {
        message.in_reply_to = Some(original.message_id.clone());
        message.references = original
            .references
            .split_whitespace()
            .map(ToOwned::to_owned)
            .chain(std::iter::once(original.message_id.clone()))
            .collect();
    }
    message
}

fn quote_body(body: &str) -> String {
    let mut out = String::from("\n\n");
    for line in body.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn html_quote_body(original: &ParsedMessage, html: &str) -> String {
    let mut attribution = String::new();
    let date = original
        .headers
        .get("Date")
        .map(String::as_str)
        .unwrap_or("");
    let from = original.from.trim();
    if !date.trim().is_empty() || !from.is_empty() {
        attribution.push_str("<div class=\"notm-reply-attribution\">");
        attribution.push_str("On ");
        if !date.trim().is_empty() {
            attribution.push_str(&escape_html(date.trim()));
        }
        if !from.is_empty() {
            if !date.trim().is_empty() {
                attribution.push_str(", ");
            }
            attribution.push_str(&escape_html(from));
        }
        attribution.push_str(" wrote:</div>");
    }
    format!(
        "<br><br>{attribution}<blockquote type=\"cite\" style=\"margin:0 0 0 .8em; border-left:2px solid #729fcf; padding-left:.8em;\">{}</blockquote>",
        sanitize_html(html)
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
