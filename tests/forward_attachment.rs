use notm_mail::{compose::Identity, forward::build_attachment_forward, mime::parse_rfc5322};

#[test]
fn forward_as_attachment_builds_rfc822_attachment() -> anyhow::Result<()> {
    let raw = b"From: alice@example.test\r\nTo: fixture@example.test\r\nSubject: Forward me\r\nMessage-ID: <forward-me@example.test>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nhello\r\n";
    let parsed = parse_rfc5322(raw)?;
    let identity = Identity {
        name: Some("Fixture User".to_string()),
        email: "fixture@example.test".to_string(),
    };
    let forward = build_attachment_forward(&parsed, &identity, raw.to_vec());
    assert_eq!(forward.subject, "Fwd: Forward me");
    assert_eq!(forward.attachments.len(), 1);
    assert_eq!(forward.attachments[0].content_type, "message/rfc822");
    assert!(forward.attachments[0].filename.ends_with(".eml"));
    assert_eq!(forward.attachments[0].bytes, raw);
    Ok(())
}
