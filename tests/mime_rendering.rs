use notm_mail::mime::parse_rfc5322;

#[test]
fn sanitizes_html_and_detects_attachments() -> anyhow::Result<()> {
    let html = b"From: a@example.test\r\nTo: b@example.test\r\nSubject: h\r\nMessage-ID: <h@test>\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><script>x()</script><b>Hello</b></body></html>";
    let parsed = parse_rfc5322(html)?;
    assert!(parsed.safe_body.contains("Hello"));
    assert!(!parsed.safe_body.contains("script"));

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let db = fixture.open_readonly()?;
    let options = notm_notmuch::QueryOptions::default();
    let msg = db
        .search_messages("subject:\"Attachment message\"", &options)?
        .remove(0);
    let parsed = notm_mail::mime::parse_file(&msg.filenames[0])?;
    assert!(!parsed.attachments.is_empty());
    Ok(())
}
