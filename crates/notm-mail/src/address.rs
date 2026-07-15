use std::collections::BTreeSet;

use email_address::EmailAddress;
use mailparse::{MailAddr, SingleInfo, addrparse};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MailAddress {
    pub name: Option<String>,
    pub email: String,
}

pub fn parse_address_list(input: &str) -> Vec<MailAddress> {
    let Ok(addresses) = addrparse(input) else {
        return Vec::new();
    };

    addresses
        .iter()
        .flat_map(|address| match address {
            MailAddr::Single(single) => std::slice::from_ref(single),
            // Recipient fields ultimately need individual mailboxes. Preserve
            // each group member's display name and address, but discard the
            // group label because `MailAddress` intentionally models a mailbox.
            MailAddr::Group(group) => group.addrs.as_slice(),
        })
        .filter_map(mail_address_from_single)
        .collect()
}

pub fn parse_one(input: &str) -> Option<MailAddress> {
    let single = addrparse(input).ok()?.extract_single_info()?;
    mail_address_from_single(&single)
}

fn mail_address_from_single(single: &SingleInfo) -> Option<MailAddress> {
    let email = single.addr.trim();
    EmailAddress::is_valid(email).then(|| MailAddress {
        name: single
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned),
        email: email.to_string(),
    })
}

pub fn format_address(addr: &MailAddress) -> String {
    match &addr.name {
        Some(name) if !name.is_empty() => format!("{} <{}>", quote_name(name), addr.email),
        _ => addr.email.clone(),
    }
}

pub fn quote_name(name: &str) -> String {
    if name.contains(',') || name.contains('"') {
        format!("\"{}\"", name.replace('"', "\\\""))
    } else {
        name.to_string()
    }
}

pub fn dedupe_addresses(addrs: impl IntoIterator<Item = MailAddress>) -> Vec<MailAddress> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for addr in addrs {
        let key = addr.email.to_lowercase();
        if seen.insert(key) {
            out.push(addr);
        }
    }
    out
}

pub fn exclude_identities(addrs: Vec<MailAddress>, identities: &[String]) -> Vec<MailAddress> {
    let identities: BTreeSet<String> = identities.iter().map(|s| s.to_lowercase()).collect();
    addrs
        .into_iter()
        .filter(|a| !identities.contains(&a.email.to_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(name: Option<&str>, email: &str) -> MailAddress {
        MailAddress {
            name: name.map(ToOwned::to_owned),
            email: email.to_string(),
        }
    }

    #[test]
    fn parses_existing_simple_address_forms() {
        assert_eq!(
            parse_address_list("alice@example.test, Bob Example <bob@example.test>"),
            vec![
                address(None, "alice@example.test"),
                address(Some("Bob Example"), "bob@example.test"),
            ]
        );
        assert_eq!(
            parse_one("Carol Example <carol@example.test>"),
            Some(address(Some("Carol Example"), "carol@example.test"))
        );
    }

    #[test]
    fn preserves_quoted_display_name_commas() {
        assert_eq!(
            parse_address_list(
                r#""Doe, Jane" <jane@example.test>, "Smith, John" <john@example.test>"#,
            ),
            vec![
                address(Some("Doe, Jane"), "jane@example.test"),
                address(Some("Smith, John"), "john@example.test"),
            ]
        );
    }

    #[test]
    fn flattens_address_groups_in_recipient_order() {
        assert_eq!(
            parse_address_list(
                r#"Friends: "Doe, Jane" <jane@example.test>, john@example.test; Outside <outside@example.test>"#,
            ),
            vec![
                address(Some("Doe, Jane"), "jane@example.test"),
                address(None, "john@example.test"),
                address(Some("Outside"), "outside@example.test"),
            ]
        );
        assert!(parse_address_list("Undisclosed recipients:;").is_empty());
    }

    #[test]
    fn rejects_malformed_addresses_with_email_address_validation() {
        assert!(parse_address_list("not-an-address").is_empty());
        assert!(parse_one("Bad Address <bad@>").is_none());
        assert_eq!(
            parse_address_list("Valid <valid@example.test>, Bad <bad@>"),
            vec![address(Some("Valid"), "valid@example.test")]
        );
    }

    #[test]
    fn dedupe_is_case_insensitive_and_keeps_the_first_address() {
        assert_eq!(
            dedupe_addresses([
                address(Some("First"), "Alice@Example.test"),
                address(Some("Second"), "alice@example.test"),
                address(None, "bob@example.test"),
            ]),
            vec![
                address(Some("First"), "Alice@Example.test"),
                address(None, "bob@example.test"),
            ]
        );
    }

    #[test]
    fn formatting_preserves_simple_names_and_quotes_special_names() {
        assert_eq!(
            format_address(&address(Some("Bob Example"), "bob@example.test")),
            "Bob Example <bob@example.test>"
        );
        assert_eq!(
            format_address(&address(Some("Doe, Jane"), "jane@example.test")),
            r#""Doe, Jane" <jane@example.test>"#
        );
    }
}
