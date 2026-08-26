use mailparse::MailHeaderMap;
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
    let raw = std::fs::read(path)?;
    let parsed = mailparse::parse_mail(&raw)?;
    assert_eq!(
        parsed.headers.get_first_value("Subject").as_deref(),
        Some("Contract subject")
    );
    assert_eq!(parsed.get_body()?, "Contract body\r\n");
    assert_crlf_and_line_limits(&raw);
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
    let raw = std::fs::read(path)?;
    let parsed = mailparse::parse_mail(&raw)?;
    assert_eq!(
        parsed.headers.get_first_value("Bcc").as_deref(),
        Some("Hidden <hidden@example.test>, second@example.test")
    );
    assert_eq!(
        parsed.headers.get_first_value("To").as_deref(),
        Some("Visible <visible@example.test>")
    );
    assert!(!parsed.get_body()?.contains("hidden@example.test"));
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
    let raw = std::fs::read(path)?;
    let parsed = mailparse::parse_mail(&raw)?;
    assert_eq!(parsed.ctype.mimetype, "multipart/mixed");
    assert_eq!(parsed.subparts.len(), 2);
    assert_eq!(
        parsed.subparts[1]
            .get_content_disposition()
            .params
            .get("filename")
            .map(String::as_str),
        Some("note.txt")
    );
    assert_eq!(parsed.subparts[1].get_body_raw()?, b"attached body\n");
    assert_crlf_and_line_limits(&raw);
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
    let raw = std::fs::read(path)?;
    let parsed = mailparse::parse_mail(&raw)?;
    assert_eq!(parsed.ctype.mimetype, "multipart/alternative");
    assert_eq!(parsed.subparts.len(), 2);
    assert_eq!(parsed.subparts[0].ctype.mimetype, "text/plain");
    assert_eq!(
        parsed.subparts[0].get_body()?.replace("\r\n", "\n"),
        "Thanks\n\n> Original text"
    );
    assert_eq!(parsed.subparts[1].ctype.mimetype, "text/html");
    assert_eq!(
        parsed.subparts[1].get_body()?,
        "<div>Thanks</div><br><br><blockquote><b>Original HTML</b></blockquote>"
    );
    Ok(())
}

#[tokio::test]
async fn fake_transport_rejects_header_injection_without_a_capture() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let transport = FakeSendTransport {
        capture_dir: dir.path().to_path_buf(),
    };
    let message = ComposedMessage::new(
        "Sender <sender@example.test>".into(),
        vec!["recipient@example.test".into()],
        "Safe subject\r\nBcc: attacker@example.test".into(),
        "Body".into(),
    );

    let error = transport
        .send(message)
        .await
        .expect_err("injected header must fail before capture");

    assert!(error.to_string().contains("control character"));
    assert_eq!(std::fs::read_dir(dir.path())?.count(), 0);
    Ok(())
}

#[tokio::test]
async fn fake_transport_captures_folded_unicode_headers_and_long_payloads() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let transport = FakeSendTransport {
        capture_dir: dir.path().to_path_buf(),
    };
    let subject = "Interop résumé 世界 🚀 ".repeat(30);
    let mut message = ComposedMessage::new(
        format!("{} <sender@example.test>", "送信者 Café ".repeat(10)),
        (0..40)
            .map(|index| format!("Person {index:02} <person{index:02}@example.test>"))
            .collect(),
        subject.clone(),
        "x".repeat(5_000),
    );
    message.attachments.push(AttachmentInput {
        filename: format!("{}-report.txt", "長い名前".repeat(30)),
        content_type: "text/plain".into(),
        bytes: vec![0xa5; 24_000],
        source_path: None,
    });

    let report = transport.send(message).await?;
    let raw = std::fs::read(report.captured_path.expect("capture path"))?;
    let parsed = mailparse::parse_mail(&raw)?;

    assert_eq!(
        parsed.headers.get_first_value("Subject").as_deref(),
        Some(subject.as_str())
    );
    let recipients =
        mailparse::addrparse_header(parsed.headers.get_first_header("To").expect("To header"))?;
    assert_eq!(recipients.count_addrs(), 40);
    assert_eq!(parsed.subparts[0].get_body()?.trim_end().len(), 5_000);
    assert_eq!(parsed.subparts[1].get_body_raw()?, vec![0xa5; 24_000]);
    assert_crlf_and_line_limits(&raw);
    Ok(())
}

fn assert_crlf_and_line_limits(raw: &[u8]) {
    assert!(raw.ends_with(b"\r\n"));
    for (index, byte) in raw.iter().enumerate() {
        match byte {
            b'\r' => assert_eq!(raw.get(index + 1), Some(&b'\n')),
            b'\n' => assert!(index > 0 && raw[index - 1] == b'\r'),
            _ => {}
        }
    }
    for line in raw.split(|byte| *byte == b'\n') {
        assert!(line.strip_suffix(b"\r").unwrap_or(line).len() <= 998);
    }
}
