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
