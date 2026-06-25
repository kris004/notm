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
