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
    let attachments = notm_mail::mime::extract_attachments_from_file(&msg.filenames[0])?;
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
    let attachments = notm_mail::mime::extract_attachments_from_file(&message.filenames[0])?;
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
