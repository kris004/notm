use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{Duration, Utc};
use notm_notmuch::{Database, DatabaseMode, OpenConfig};
use tempfile::TempDir;

const HUGE_HTML_BODY_BYTES_ENV: &str = "NOTM_FIXTURE_TEST_HUGE_BODY_BYTES";
const MAX_HUGE_HTML_BODY_BYTES: usize = 4 * 1024 * 1024 - 64 * 1024;
const EXTRA_SEARCH_THREADS_ENV: &str = "NOTM_FIXTURE_TEST_SEARCH_THREADS";
const MAX_EXTRA_SEARCH_THREADS: usize = 256;

pub struct FixtureDatabase {
    _temp: TempDir,
    pub root: PathBuf,
    pub maildir: PathBuf,
    pub config_path: PathBuf,
}

impl FixtureDatabase {
    pub fn create() -> anyhow::Result<Self> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("mail");
        let inbox_cur = root.join("account.fixture/cur");
        let inbox_new = root.join("account.fixture/new");
        let inbox_tmp = root.join("account.fixture/tmp");
        fs::create_dir_all(&inbox_cur)?;
        fs::create_dir_all(&inbox_new)?;
        fs::create_dir_all(&inbox_tmp)?;
        let config_path = temp.path().join("notmuch-config");
        fs::write(
            &config_path,
            format!(
                "[database]\npath={}\n\n[user]\nname=Fixture User\nprimary_email=fixture@example.test\nother_email=alt@example.test\n\n[new]\ntags=\nignore=\n\n[search]\nexclude_tags=trash;spam\n\n[maildir]\nsynchronize_flags=true\n",
                root.display()
            ),
        )?;
        let open = OpenConfig {
            database_path: Some(root.clone()),
            config_path: Some(config_path.clone()),
            profile: None,
        };
        let db = Database::create(&open)?;
        let messages = fixture_messages();
        for (idx, msg) in messages.iter().enumerate() {
            let flags = if msg.tags.contains(&"unread") {
                ""
            } else {
                "S"
            };
            let path = inbox_cur.join(format!("{:04}.fixture:2,{}", idx + 1, flags));
            fs::write(&path, &msg.raw)?;
            db.index_fixture_file(&path, &msg.tags)?;
        }
        drop(db);
        Ok(Self {
            _temp: temp,
            maildir: root.join("account.fixture"),
            root,
            config_path,
        })
    }

    pub fn open_readonly(&self) -> anyhow::Result<Database> {
        Ok(Database::open(&self.open_config(), DatabaseMode::ReadOnly)?)
    }

    pub fn open_readwrite(&self) -> anyhow::Result<Database> {
        Ok(Database::open(
            &self.open_config(),
            DatabaseMode::ReadWrite,
        )?)
    }

    pub fn open_config(&self) -> OpenConfig {
        OpenConfig {
            database_path: Some(self.root.clone()),
            config_path: Some(self.config_path.clone()),
            profile: None,
        }
    }
}

struct FixtureMessage {
    raw: String,
    tags: Vec<&'static str>,
}

fn fixture_messages() -> Vec<FixtureMessage> {
    let now = Utc::now();
    let thread_id = "three-message";
    let mut messages = vec![
        msg(
            "unread inbox message",
            "alice@example.test",
            "fixture@example.test",
            "Unread inbox message",
            "Plain unread inbox body.",
            &["inbox", "unread"],
            now,
            None,
            None,
            None,
        ),
        msg(
            "read inbox message",
            "bob@example.test",
            "fixture@example.test",
            "Read inbox message",
            "Plain read inbox body.",
            &["inbox"],
            now - Duration::minutes(1),
            None,
            None,
            None,
        ),
        msg(
            &format!("thread-root-{thread_id}"),
            "carol@example.test",
            "fixture@example.test",
            "Three message thread",
            "Thread root body.",
            &["inbox", "unread"],
            now - Duration::minutes(10),
            None,
            None,
            None,
        ),
        msg(
            &format!("thread-reply1-{thread_id}"),
            "fixture@example.test",
            "carol@example.test",
            "Re: Three message thread",
            "Reply one body.",
            &["sent"],
            now - Duration::minutes(9),
            Some(&format!("<thread-root-{thread_id}@fixture.test>")),
            None,
            None,
        ),
        msg(
            &format!("thread-reply2-{thread_id}"),
            "dave@example.test",
            "fixture@example.test",
            "Re: Three message thread",
            "Reply two body with quote.\n\n> quoted\n> quoted\n> quoted",
            &["inbox"],
            now - Duration::minutes(8),
            Some(&format!("<thread-reply1-{thread_id}@fixture.test>")),
            None,
            None,
        ),
        html_msg(),
        long_html_msg(),
        attachment_msg(),
        html_with_malformed_attachment_msg(),
        msg(
            "multi-recipient",
            "erin@example.test",
            "fixture@example.test, alt@example.test",
            "Multiple recipients",
            "Multiple recipient body.",
            &["inbox"],
            now - Duration::minutes(3),
            None,
            None,
            Some("replyto@example.test"),
        ),
        reply_all_address_msg(),
        msg(
            "sent-like",
            "fixture@example.test",
            "frank@example.test",
            "Sent like message",
            "Sent body.",
            &["sent"],
            now - Duration::minutes(4),
            None,
            None,
            None,
        ),
        msg(
            "draft-like",
            "fixture@example.test",
            "grace@example.test",
            "Draft like message",
            "Draft body.",
            &["draft"],
            now - Duration::minutes(5),
            None,
            None,
            None,
        ),
        msg(
            "spam-trash",
            "spam@example.test",
            "fixture@example.test",
            "Spam trash message",
            "Spam body.",
            &["spam", "trash"],
            now - Duration::minutes(6),
            None,
            None,
            None,
        ),
        malformed_msg(),
        malformed_transfer_encoding_msg(),
        unicode_msg(),
    ];
    if let Some(bytes) = fixture_huge_html_body_bytes() {
        messages.push(huge_html_msg(bytes));
    }
    let extra_search_threads = std::env::var(EXTRA_SEARCH_THREADS_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_EXTRA_SEARCH_THREADS);
    messages.extend((0..extra_search_threads).map(|index| {
        msg(
            &format!("search-stress-{index}"),
            "search-stress@example.test",
            "fixture@example.test",
            &format!("Search stress row {index:04}"),
            "Small body used to exercise bounded search result model updates.",
            &["search-stress"],
            now - Duration::seconds(100 + index as i64),
            None,
            None,
            None,
        )
    }));
    messages.extend(attachment_heavy_thread());
    messages
}

fn fixture_huge_html_body_bytes() -> Option<usize> {
    std::env::var(HUGE_HTML_BODY_BYTES_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|bytes| *bytes > 0)
        .map(|bytes| bytes.min(MAX_HUGE_HTML_BODY_BYTES))
}

#[allow(clippy::too_many_arguments)]
fn msg(
    id: &str,
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
    tags: &[&'static str],
    date: chrono::DateTime<Utc>,
    in_reply_to: Option<&str>,
    references: Option<&str>,
    reply_to: Option<&str>,
) -> FixtureMessage {
    let message_id = format!("<{id}@fixture.test>");
    let mut raw = format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nDate: {}\r\nMessage-ID: {message_id}\r\nMIME-Version: 1.0\r\n",
        date.to_rfc2822()
    );
    if let Some(value) = in_reply_to {
        raw.push_str(&format!(
            "In-Reply-To: {value}\r\nReferences: {} {value}\r\n",
            references.unwrap_or("")
        ));
    }
    if let Some(value) = reply_to {
        raw.push_str(&format!("Reply-To: {value}\r\n"));
    }
    raw.push_str("Content-Type: text/plain; charset=utf-8\r\n\r\n");
    raw.push_str(body);
    FixtureMessage {
        raw,
        tags: tags.to_vec(),
    }
}

fn html_msg() -> FixtureMessage {
    FixtureMessage {
        raw: "From: html@example.test\r\nTo: fixture@example.test\r\nSubject: HTML message\r\nDate: Thu, 18 Jun 2026 20:00:00 -0600\r\nMessage-ID: <html-message@fixture.test>\r\nMIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><h1>Hello</h1><script>alert(1)</script><p>Safe <b>HTML</b>.</p><p><a href=\"https://example.test/first\">First fixture link</a> and <a href=\"mailto:fixture@example.test\">fixture email link</a>.</p><img src=\"https://example.test/pixel\"></body></html>".to_string(),
        tags: vec!["inbox", "unread"],
    }
}

fn long_html_msg() -> FixtureMessage {
    let body = (1..=80)
        .map(|n| format!("<p>Scrollable HTML fixture row {n}</p>"))
        .collect::<Vec<_>>()
        .join("\r\n");
    FixtureMessage {
        raw: format!(
            "From: long-html@example.test\r\nTo: fixture@example.test\r\nSubject: Long HTML message\r\nDate: Thu, 18 Jun 2026 20:00:30 -0600\r\nMessage-ID: <long-html-message@fixture.test>\r\nMIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><h1>Long HTML</h1>{body}</body></html>"
        ),
        tags: vec!["inbox"],
    }
}

fn huge_html_msg(target_bytes: usize) -> FixtureMessage {
    const PREFIX: &str = "<html><body><h1>Near-limit HTML body</h1>";
    const SUFFIX: &str = "</body></html>";
    let paragraph = format!(
        "<p>Near-limit rendering fixture content {}.</p>\r\n",
        "0123456789abcdef ".repeat(52)
    );

    let target_bytes = target_bytes.max(PREFIX.len() + SUFFIX.len());
    let mut body = String::with_capacity(target_bytes);
    body.push_str(PREFIX);
    while body.len() + paragraph.len() + SUFFIX.len() <= target_bytes {
        body.push_str(&paragraph);
    }
    body.extend(std::iter::repeat_n(
        'x',
        target_bytes.saturating_sub(body.len() + SUFFIX.len()),
    ));
    body.push_str(SUFFIX);

    FixtureMessage {
        raw: format!(
            "From: huge-html@example.test\r\nTo: fixture@example.test\r\nSubject: Near-limit HTML body\r\nDate: Thu, 18 Jun 2026 20:00:45 -0600\r\nMessage-ID: <huge-html-body@fixture.test>\r\nMIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{body}"
        ),
        tags: vec!["inbox"],
    }
}

fn attachment_msg() -> FixtureMessage {
    FixtureMessage {
        raw: "From: attach@example.test\r\nTo: fixture@example.test\r\nSubject: Attachment message\r\nDate: Thu, 18 Jun 2026 20:01:00 -0600\r\nMessage-ID: <attachment-message@fixture.test>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=fixture-boundary\r\n\r\n--fixture-boundary\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nMessage with attachment.\r\n--fixture-boundary\r\nContent-Type: text/plain; name=note.txt\r\nContent-Disposition: attachment; filename=note.txt\r\n\r\nattached text\r\n--fixture-boundary--\r\n".to_string(),
        tags: vec!["inbox"],
    }
}

fn attachment_heavy_thread() -> Vec<FixtureMessage> {
    const MESSAGE_COUNT: usize = 3;
    const ATTACHMENTS_PER_MESSAGE: usize = 24;
    const LARGE_ATTACHMENT_BYTES_ENV: &str = "NOTM_FIXTURE_TEST_LARGE_ATTACHMENT_BYTES";
    const MAX_LARGE_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;
    let large_attachment_bytes = std::env::var(LARGE_ATTACHMENT_BYTES_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_LARGE_ATTACHMENT_BYTES);
    (0..MESSAGE_COUNT)
        .map(|message_index| {
            let message_id = format!("attachment-heavy-{message_index}@fixture.test");
            let boundary = format!("attachment-heavy-boundary-{message_index}");
            let mut raw = format!(
                "From: attachment-heavy@example.test\r\nTo: fixture@example.test\r\nSubject: {}Attachment-heavy thread\r\nDate: Thu, 18 Jun 2026 20:03:0{message_index} -0600\r\nMessage-ID: <{message_id}>\r\n",
                if message_index == 0 { "" } else { "Re: " }
            );
            if message_index > 0 {
                raw.push_str(
                    "In-Reply-To: <attachment-heavy-0@fixture.test>\r\nReferences: <attachment-heavy-0@fixture.test>\r\n",
                );
            }
            raw.push_str(&format!(
                "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary={boundary}\r\n\r\n--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nAttachment-heavy fixture message {message_index}.\r\n"
            ));
            for attachment_index in 0..ATTACHMENTS_PER_MESSAGE {
                let filename = format!("fixture-{message_index}-{attachment_index:02}.txt");
                let payload = if message_index == 0
                    && attachment_index == 0
                    && large_attachment_bytes > 0
                {
                    "x".repeat(large_attachment_bytes)
                } else {
                    format!(
                        "attachment {message_index}/{attachment_index}: {}",
                        "0123456789abcdef".repeat(64)
                    )
                };
                raw.push_str(&format!(
                    "--{boundary}\r\nContent-Type: text/plain; name={filename}\r\nContent-Disposition: attachment; filename={filename}\r\n\r\n{payload}\r\n"
                ));
            }
            raw.push_str(&format!("--{boundary}--\r\n"));
            FixtureMessage {
                raw,
                tags: if message_index == 0 {
                    vec!["inbox"]
                } else {
                    vec!["sent"]
                },
            }
        })
        .collect()
}

fn html_with_malformed_attachment_msg() -> FixtureMessage {
    FixtureMessage {
        raw: "From: broken-attachment@example.test\r\nTo: fixture@example.test\r\nSubject: HTML with malformed attachment\r\nDate: Thu, 18 Jun 2026 20:01:15 -0600\r\nMessage-ID: <html-malformed-attachment@fixture.test>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=broken-attachment-boundary\r\n\r\n--broken-attachment-boundary\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><p>Readable HTML body.</p></body></html>\r\n--broken-attachment-boundary\r\nContent-Type: application/octet-stream; name=broken.bin\r\nContent-Disposition: attachment; filename=broken.bin\r\nContent-Transfer-Encoding: base64\r\n\r\n!!!!\r\n--broken-attachment-boundary\r\nContent-Type: text/plain; name=good.txt\r\nContent-Disposition: attachment; filename=good.txt\r\nContent-Transfer-Encoding: base64\r\n\r\nZ29vZCBzaWJsaW5n\r\n--broken-attachment-boundary--\r\n".to_string(),
        tags: vec!["inbox"],
    }
}

fn reply_all_address_msg() -> FixtureMessage {
    FixtureMessage {
        raw: "From: Sender <sender@example.test>\r\nTo: Fixture User <fixture@example.test>, \"Doe, Jane\" <jane@example.test>\r\nCc: Project Team: \"Smith, John\" <john@example.test>, other@example.test, Fixture Alias <alt@example.test>;\r\nSubject: Quoted and grouped recipients\r\nDate: Thu, 18 Jun 2026 20:01:30 -0600\r\nMessage-ID: <reply-all-addresses@fixture.test>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nReply-all address fixture body.\r\n".to_string(),
        tags: vec!["inbox"],
    }
}

fn malformed_msg() -> FixtureMessage {
    FixtureMessage {
        raw: "From malformed@example.test\nTo: fixture@example.test\nSubject: Malformed but parseable\nMessage-ID: <malformed@fixture.test>\n\nbody".to_string(),
        tags: vec!["inbox"],
    }
}

fn malformed_transfer_encoding_msg() -> FixtureMessage {
    FixtureMessage {
        raw: "From: broken@example.test\r\nTo: fixture@example.test\r\nSubject: Malformed transfer encoding\r\nDate: Thu, 18 Jun 2026 20:01:30 -0600\r\nMessage-ID: <malformed-transfer-encoding@fixture.test>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n!!!!".to_string(),
        tags: vec!["inbox"],
    }
}

fn unicode_msg() -> FixtureMessage {
    FixtureMessage {
        raw: "From: unicode@example.test\r\nTo: fixture@example.test\r\nSubject: Unicode ☕ message\r\nDate: Thu, 18 Jun 2026 20:02:00 -0600\r\nMessage-ID: <unicode@fixture.test>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nUnicode body: café ☕ Привет\r\n".to_string(),
        tags: vec!["inbox", "unread"],
    }
}

pub fn fixture_root_exists(path: &Path) -> bool {
    path.join(".notmuch").exists()
}
