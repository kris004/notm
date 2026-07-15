use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{Duration, Utc};
use notm_notmuch::{Database, DatabaseMode, OpenConfig};
use tempfile::TempDir;

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
    vec![
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
    ]
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
        raw: "From: html@example.test\r\nTo: fixture@example.test\r\nSubject: HTML message\r\nDate: Thu, 18 Jun 2026 20:00:00 -0600\r\nMessage-ID: <html-message@fixture.test>\r\nMIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><h1>Hello</h1><script>alert(1)</script><p>Safe <b>HTML</b>.</p><img src=\"https://example.test/pixel\"></body></html>".to_string(),
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

fn attachment_msg() -> FixtureMessage {
    FixtureMessage {
        raw: "From: attach@example.test\r\nTo: fixture@example.test\r\nSubject: Attachment message\r\nDate: Thu, 18 Jun 2026 20:01:00 -0600\r\nMessage-ID: <attachment-message@fixture.test>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=fixture-boundary\r\n\r\n--fixture-boundary\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nMessage with attachment.\r\n--fixture-boundary\r\nContent-Type: text/plain; name=note.txt\r\nContent-Disposition: attachment; filename=note.txt\r\n\r\nattached text\r\n--fixture-boundary--\r\n".to_string(),
        tags: vec!["inbox"],
    }
}

fn html_with_malformed_attachment_msg() -> FixtureMessage {
    FixtureMessage {
        raw: "From: broken-attachment@example.test\r\nTo: fixture@example.test\r\nSubject: HTML with malformed attachment\r\nDate: Thu, 18 Jun 2026 20:01:15 -0600\r\nMessage-ID: <html-malformed-attachment@fixture.test>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=broken-attachment-boundary\r\n\r\n--broken-attachment-boundary\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><p>Readable HTML body.</p></body></html>\r\n--broken-attachment-boundary\r\nContent-Type: application/octet-stream; name=broken.bin\r\nContent-Disposition: attachment; filename=broken.bin\r\nContent-Transfer-Encoding: base64\r\n\r\n!!!!\r\n--broken-attachment-boundary\r\nContent-Type: text/plain; name=good.txt\r\nContent-Disposition: attachment; filename=good.txt\r\nContent-Transfer-Encoding: base64\r\n\r\nZ29vZCBzaWJsaW5n\r\n--broken-attachment-boundary--\r\n".to_string(),
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
