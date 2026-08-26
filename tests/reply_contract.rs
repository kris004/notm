use mailparse::MailHeaderMap;
use notm_mail::{ReplyKind, build_reply, compose::Identity, mime::parse_rfc5322};

#[test]
fn reply_all_excludes_own_identity_and_sets_thread_headers() -> anyhow::Result<()> {
    let raw = b"From: Alice <alice@example.test>\r\nTo: Me <me@example.test>, Bob <bob@example.test>\r\nCc: Other <other@example.test>\r\nReply-To: reply@example.test\r\nSubject: Hello\r\nMessage-ID: <orig@example.test>\r\nReferences: <older@example.test>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nBody";
    let parsed = parse_rfc5322(raw)?;
    let identity = Identity {
        name: Some("Me".into()),
        email: "me@example.test".into(),
    };
    let reply = build_reply(
        &parsed,
        &identity,
        &["me@example.test".into()],
        ReplyKind::All,
    );
    assert_eq!(reply.subject, "Re: Hello");
    assert!(reply.to.iter().any(|v| v.contains("reply@example.test")));
    assert!(!reply.to.iter().any(|v| v.contains("me@example.test")));
    assert_eq!(reply.in_reply_to.as_deref(), Some("<orig@example.test>"));
    assert!(
        reply
            .references
            .contains(&"<orig@example.test>".to_string())
    );
    Ok(())
}

#[test]
fn reply_preserves_references_with_whitespace_inside_message_ids() -> anyhow::Result<()> {
    let raw = br#"From: Alice <alice@example.test>
To: Me <me@example.test>
Subject: Legacy threading
Message-ID: <current@example.test>
References: <root@example.test> <"quoted id"@example.test>
Content-Type: text/plain; charset=utf-8

Body"#;
    let parsed = parse_rfc5322(raw)?;
    let identity = Identity {
        name: Some("Me".into()),
        email: "me@example.test".into(),
    };

    let reply = build_reply(
        &parsed,
        &identity,
        &["me@example.test".into()],
        ReplyKind::Sender,
    );
    let rendered = reply.to_rfc5322()?;
    let reparsed = mailparse::parse_mail(&rendered)?;

    assert_eq!(
        reparsed.headers.get_first_value("References").as_deref(),
        Some("<root@example.test> <\"quoted id\"@example.test> <current@example.test>")
    );
    assert_eq!(
        reparsed.headers.get_first_value("In-Reply-To").as_deref(),
        Some("<current@example.test>")
    );
    Ok(())
}

#[test]
fn reply_strips_legacy_reference_phrases_while_preserving_message_ids() -> anyhow::Result<()> {
    let raw = br#"From: Alice <alice@example.test>
To: Me <me@example.test>
Subject: Legacy phrase threading
Message-ID: <current@example.test>
References: =?UTF-8?B?5pel5pys6Kqe?= . Example <root@example.test> "=?UTF-8?Q?Caf=C3=A9?= <not-an-id>" <""@legacy.example> trailing phrase
Content-Type: text/plain; charset=utf-8

Body"#;
    let parsed = parse_rfc5322(raw)?;
    assert!(
        parsed.references.contains("日本語") && parsed.references.contains("Café"),
        "mailparse should expose decoded RFC 2047 phrase words: {:?}",
        parsed.references
    );
    let identity = Identity {
        name: Some("Me".into()),
        email: "me@example.test".into(),
    };

    let reply = build_reply(
        &parsed,
        &identity,
        &["me@example.test".into()],
        ReplyKind::Sender,
    );
    let rendered = reply.to_rfc5322()?;
    let reparsed = mailparse::parse_mail(&rendered)?;

    assert_eq!(
        reparsed.headers.get_first_value("References").as_deref(),
        Some("<root@example.test> <\"\"@legacy.example> <current@example.test>")
    );
    assert_eq!(
        reparsed.headers.get_first_value("In-Reply-To").as_deref(),
        Some("<current@example.test>")
    );
    Ok(())
}

#[test]
fn reply_all_preserves_quoted_names_and_flattens_recipient_groups() -> anyhow::Result<()> {
    let raw = br#"From: Sender <sender@example.test>
To: Me <me@example.test>, "Doe, Jane" <jane@example.test>
Cc: Friends: "Smith, John" <john@example.test>, other@example.test;
Subject: Group reply
Message-ID: <group-reply@example.test>
Content-Type: text/plain; charset=utf-8

Body"#;
    let parsed = parse_rfc5322(raw)?;
    let identity = Identity {
        name: Some("Me".into()),
        email: "me@example.test".into(),
    };

    let reply = build_reply(
        &parsed,
        &identity,
        &["me@example.test".into()],
        ReplyKind::All,
    );

    assert_eq!(
        reply.to,
        vec![
            "Sender <sender@example.test>",
            r#""Doe, Jane" <jane@example.test>"#,
        ]
    );
    assert_eq!(
        reply.cc,
        vec![r#""Smith, John" <john@example.test>"#, "other@example.test",]
    );
    Ok(())
}

#[test]
fn html_only_reply_keeps_html_quote_hidden_from_plain_composer() -> anyhow::Result<()> {
    let raw = b"From: Alice <alice@example.test>\r\nTo: Me <me@example.test>\r\nSubject: HTML Hello\r\nDate: Thu, 25 Jun 2026 05:30:00 +0000\r\nMessage-ID: <html-orig@example.test>\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><p><b>Hello</b> from HTML</p><script>bad()</script></body></html>";
    let parsed = parse_rfc5322(raw)?;
    let identity = Identity {
        name: Some("Me".into()),
        email: "me@example.test".into(),
    };
    let reply = build_reply(
        &parsed,
        &identity,
        &["me@example.test".into()],
        ReplyKind::Sender,
    );

    assert_eq!(reply.body, "");
    assert!(
        reply
            .text_reply_quote
            .as_deref()
            .is_some_and(|body| body.contains("> Hello from HTML"))
    );
    let html_quote = reply.html_reply_quote.as_deref().expect("html quote");
    assert!(html_quote.contains("<blockquote"));
    assert!(html_quote.contains("<b>Hello</b>"));
    assert!(!html_quote.contains("<script>"));
    Ok(())
}
