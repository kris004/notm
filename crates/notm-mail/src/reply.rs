use serde::{Deserialize, Serialize};

use crate::{
    address::{dedupe_addresses, exclude_identities, format_address, parse_address_list},
    compose::{ComposedMessage, Identity},
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
    let mut message = ComposedMessage::new(
        identity.formatted(),
        to,
        normalize_subject_for_reply(&original.subject),
        quote_body(&original.safe_body),
    );
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
