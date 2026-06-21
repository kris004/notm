use std::collections::BTreeSet;

use email_address::EmailAddress;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MailAddress {
    pub name: Option<String>,
    pub email: String,
}

pub fn parse_address_list(input: &str) -> Vec<MailAddress> {
    input
        .split(',')
        .filter_map(|part| parse_one(part.trim()))
        .collect()
}

pub fn parse_one(input: &str) -> Option<MailAddress> {
    if input.is_empty() {
        return None;
    }
    if let (Some(start), Some(end)) = (input.rfind('<'), input.rfind('>')) {
        let email = input[start + 1..end].trim();
        if EmailAddress::is_valid(email) {
            let name = input[..start].trim().trim_matches('"').trim();
            return Some(MailAddress {
                name: (!name.is_empty()).then(|| name.to_string()),
                email: email.to_string(),
            });
        }
    }
    EmailAddress::is_valid(input).then(|| MailAddress {
        name: None,
        email: input.to_string(),
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
