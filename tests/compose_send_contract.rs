use notm_mail::{AttachmentInput, ComposedMessage, FakeSendTransport, SendTransport};

#[tokio::test]
async fn fake_transport_captures_valid_rfc5322() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let transport = FakeSendTransport {
        capture_dir: dir.path().to_path_buf(),
    };
    transport.probe().await?;
    let report = transport
        .send(ComposedMessage::new(
            "Sender <sender@example.test>".into(),
            vec!["recipient@example.test".into()],
            "Contract subject".into(),
            "Contract body".into(),
        ))
        .await?;
    assert!(report.accepted);
    let path = report.captured_path.expect("captured path");
    let raw = std::fs::read_to_string(path)?;
    assert!(raw.contains("Subject: Contract subject"));
    assert!(raw.contains("\r\n\r\nContract body"));
    Ok(())
}

#[tokio::test]
async fn fake_transport_captures_bcc_recipients_for_submission() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let transport = FakeSendTransport {
        capture_dir: dir.path().to_path_buf(),
    };
    let mut message = ComposedMessage::new(
        "Sender <sender@example.test>".into(),
        vec!["Visible <visible@example.test>".into()],
        "Bcc contract".into(),
        "Contract body".into(),
    );
    message.bcc = vec![
        "Hidden <hidden@example.test>".into(),
        "second@example.test".into(),
    ];

    let report = transport.send(message).await?;

    assert!(report.accepted);
    let path = report.captured_path.expect("captured path");
    let raw = std::fs::read_to_string(path)?;
    assert!(raw.contains("\r\nBcc: Hidden <hidden@example.test>, second@example.test\r\n"));
    Ok(())
}

#[tokio::test]
async fn fake_transport_preserves_attachment_part() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let transport = FakeSendTransport {
        capture_dir: dir.path().to_path_buf(),
    };
    let mut message = ComposedMessage::new(
        "Sender <sender@example.test>".into(),
        vec!["recipient@example.test".into()],
        "Attachment contract".into(),
        "See attached.".into(),
    );
    message.attachments.push(AttachmentInput {
        filename: "note.txt".into(),
        content_type: "text/plain".into(),
        bytes: b"attached body\n".to_vec(),
        source_path: None,
    });

    let report = transport.send(message).await?;
    assert!(report.accepted);
    let path = report.captured_path.expect("captured path");
    let raw = std::fs::read_to_string(path)?;
    assert!(raw.contains("Content-Type: multipart/mixed;"));
    assert!(raw.contains("filename=\"note.txt\""));
    assert!(raw.contains("YXR0YWNoZWQgYm9keQo="));
    Ok(())
}

#[tokio::test]
async fn fake_transport_sends_html_reply_as_alternative() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let transport = FakeSendTransport {
        capture_dir: dir.path().to_path_buf(),
    };
    let mut message = ComposedMessage::new(
        "Sender <sender@example.test>".into(),
        vec!["recipient@example.test".into()],
        "Re: HTML".into(),
        "Thanks".into(),
    );
    message.text_reply_quote = Some("\n\n> Original text".into());
    message.html_reply_quote = Some("<br><br><blockquote><b>Original HTML</b></blockquote>".into());

    let report = transport.send(message).await?;
    assert!(report.accepted);
    let path = report.captured_path.expect("captured path");
    let raw = std::fs::read_to_string(path)?;
    assert!(raw.contains("Content-Type: multipart/alternative;"));
    assert!(raw.contains("Content-Type: text/plain; charset=utf-8"));
    assert!(raw.contains("Thanks\r\n\r\n> Original text"));
    assert!(raw.contains("Content-Type: text/html; charset=utf-8"));
    assert!(raw.contains("<div>Thanks</div><br><br><blockquote><b>Original HTML</b></blockquote>"));
    Ok(())
}
