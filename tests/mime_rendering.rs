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
    let parsed = notm_mail::mime::parse_reader(db.open_message_file(&msg)?)?;
    assert!(!parsed.attachments.is_empty());
    Ok(())
}

#[test]
fn malformed_text_transfer_encoding_is_visible_to_callers() -> anyhow::Result<()> {
    let raw = b"From: a@example.test\r\nTo: b@example.test\r\nSubject: broken\r\n\
                Content-Type: text/plain; charset=utf-8\r\n\
                Content-Transfer-Encoding: base64\r\n\r\n!!!!";

    let parsed = parse_rfc5322(raw)?;

    assert!(parsed.safe_body.is_empty());
    assert_eq!(parsed.decode_warnings.len(), 1);
    assert!(parsed.decode_warnings[0].contains("Could not decode text/plain MIME part"));
    assert!(parsed.decode_warnings[0].contains("Base64 decode error"));
    Ok(())
}

#[test]
fn decode_status_serialization_is_backward_compatible_and_sparse() -> anyhow::Result<()> {
    let plain = parse_rfc5322(b"Content-Type: text/plain\r\n\r\nhello")?;
    let plain_json = serde_json::to_value(&plain)?;
    assert!(plain_json.get("decode_warnings").is_none());

    let malformed = parse_rfc5322(
        b"MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n\
          --x\r\nContent-Type: application/octet-stream; name=broken.bin\r\n\
          Content-Disposition: attachment; filename=broken.bin\r\n\
          Content-Transfer-Encoding: base64\r\n\r\n!!!!\r\n--x--\r\n",
    )?;
    assert_eq!(malformed.attachments.len(), 1);
    assert!(malformed.attachments[0].decode_error.is_some());
    let malformed_json = serde_json::to_value(&malformed)?;
    let attachment = &malformed_json["attachments"][0];
    assert!(attachment.get("decode_error").is_some());
    assert!(attachment.get("decode_warnings").is_none());

    let legacy_attachment = serde_json::json!({
        "filename": "old.bin",
        "content_type": "application/octet-stream",
        "size": 3,
        "content_id": null
    });
    let decoded: notm_mail::mime::Attachment = serde_json::from_value(legacy_attachment)?;
    assert!(decoded.decode_warnings.is_empty());
    assert!(decoded.decode_error.is_none());
    Ok(())
}
