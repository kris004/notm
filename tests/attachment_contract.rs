#[test]
fn extracts_attachment_bytes_from_fixture_message() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let db = fixture.open_readonly()?;
    let msg = db
        .search_messages(
            "subject:\"Attachment message\"",
            &notm_notmuch::QueryOptions::default(),
        )?
        .remove(0);
    let attachments =
        notm_mail::mime::extract_attachments_from_reader(db.open_message_file(&msg)?)?;
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].filename, "note.txt");
    assert!(String::from_utf8_lossy(&attachments[0].bytes).contains("attached text"));
    Ok(())
}

#[test]
fn saves_fixture_attachment_without_replacing_existing_file() -> anyhow::Result<()> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let db = fixture.open_readonly()?;
    let message = db
        .search_messages(
            "subject:\"Attachment message\"",
            &notm_notmuch::QueryOptions::default(),
        )?
        .remove(0);
    let attachments =
        notm_mail::mime::extract_attachments_from_reader(db.open_message_file(&message)?)?;
    let attachment = attachments.first().expect("fixture attachment");
    let downloads = tempfile::tempdir()?;
    let original_path = downloads.path().join("note.txt");
    std::fs::write(&original_path, b"keep this file")?;

    let saved_path = notm_mail::attachments::save_attachment_without_overwrite(
        downloads.path(),
        &attachment.filename,
        &attachment.bytes,
    )?;

    assert_eq!(saved_path, downloads.path().join("note (1).txt"));
    assert_eq!(std::fs::read(original_path)?, b"keep this file");
    assert_eq!(std::fs::read(saved_path)?, attachment.bytes);
    Ok(())
}

#[test]
fn malformed_attachment_transfer_encoding_returns_an_error() {
    let raw = b"MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n\
                --x\r\nContent-Type: application/octet-stream; name=broken.bin\r\n\
                Content-Disposition: attachment; filename=broken.bin\r\n\
                Content-Transfer-Encoding: base64\r\n\r\n!!!!\r\n--x--\r\n";

    let err = notm_mail::mime::extract_attachments(raw)
        .expect_err("malformed attachment must not become zero-byte data");
    let error = format!("{err:#}");
    assert!(error.contains("attachment \"broken.bin\""), "{error}");
    assert!(error.contains("Base64 decode error"), "{error}");
}

#[test]
fn detailed_extraction_preserves_valid_siblings_and_mime_indexes() -> anyhow::Result<()> {
    let raw = b"MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n\
                --x\r\nContent-Type: application/octet-stream; name=broken.bin\r\n\
                Content-Disposition: attachment; filename=broken.bin\r\n\
                Content-Transfer-Encoding: bas64\r\n\r\naGVsbG8=\r\n\
                --x\r\nContent-Type: text/plain; name=good.txt\r\n\
                Content-Disposition: attachment; filename=good.txt\r\n\
                Content-Transfer-Encoding: base64\r\n\r\nZ29vZCBzaWJsaW5n\r\n--x--\r\n";

    let report = notm_mail::mime::extract_attachments_detailed(raw)?;

    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].part_index, 0);
    assert_eq!(report.failures[0].filename, "broken.bin");
    assert!(
        report.failures[0]
            .error
            .contains("unsupported Content-Transfer-Encoding")
    );
    assert_eq!(report.attachments.len(), 1);
    assert_eq!(report.attachments[0].part_index, 1);
    assert_eq!(report.attachments[0].filename, "good.txt");
    assert_eq!(report.attachments[0].bytes, b"good sibling");

    let strict_error = notm_mail::mime::extract_attachments(raw)
        .expect_err("strict extraction must reject a message with any corrupt attachment");
    assert!(format!("{strict_error:#}").contains("broken.bin"));
    Ok(())
}
