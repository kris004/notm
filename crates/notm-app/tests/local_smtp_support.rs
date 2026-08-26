#![cfg(unix)]

use std::{collections::BTreeSet, fs, time::Duration};

#[path = "support/local_smtp.rs"]
mod local_smtp;

use local_smtp::{LocalSmtpCapture, parse_wire_with_python, write_python_submission_helper};

#[test]
fn python_helper_preserves_bytes_and_removes_bcc_only_at_smtp_boundary() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let helper = directory.path().join("submit-local-smtp");
    let capture = LocalSmtpCapture::start()?;
    write_python_submission_helper(&helper, capture.port())?;
    capture.ensure_no_message(Duration::from_millis(20))?;

    let raw = concat!(
        "From: =?UTF-8?B?SsO2cmc=?= <sender@example.test>\r\n",
        "To: Visible <visible@example.test>\r\n",
        "Bcc: Hidden <hidden@example.test>,\r\n",
        " second@example.test\r\n",
        "Subject: =?UTF-8?B?R3LDvMOfZQ==?=\r\n",
        "Message-ID: <support@example.test>\r\n",
        "Date: Wed, 26 Aug 2026 00:00:00 -0600\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "Content-Transfer-Encoding: quoted-printable\r\n\r\n",
        "Body\r\n",
    )
    .as_bytes();
    let expected_wire = concat!(
        "From: =?UTF-8?B?SsO2cmc=?= <sender@example.test>\r\n",
        "To: Visible <visible@example.test>\r\n",
        "Subject: =?UTF-8?B?R3LDvMOfZQ==?=\r\n",
        "Message-ID: <support@example.test>\r\n",
        "Date: Wed, 26 Aug 2026 00:00:00 -0600\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "Content-Transfer-Encoding: quoted-printable\r\n\r\n",
        "Body\r\n",
    )
    .as_bytes();
    let mut child = std::process::Command::new(&helper)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    use std::io::Write as _;
    child.stdin.take().expect("helper stdin").write_all(raw)?;
    let status = child.wait()?;
    assert!(status.success(), "submission helper failed with {status}");

    let mut messages = capture.wait_for_messages(1, Duration::from_secs(10))?;
    let message = messages.pop().expect("one captured message");
    assert_eq!(message.mail_from, "sender@example.test");
    assert_eq!(
        message.rcpt_to.into_iter().collect::<BTreeSet<_>>(),
        [
            "hidden@example.test".to_string(),
            "second@example.test".to_string(),
            "visible@example.test".to_string(),
        ]
        .into_iter()
        .collect()
    );
    assert!(
        !message
            .data
            .windows(b"Bcc:".len())
            .any(|window| window.eq_ignore_ascii_case(b"Bcc:"))
    );
    assert_eq!(message.data, expected_wire);

    let wire_path = directory.path().join("captured.eml");
    fs::write(&wire_path, &message.data)?;
    let parsed = parse_wire_with_python(&wire_path)?;
    assert_eq!(parsed["subject"], "Grüße");
    assert_eq!(parsed["from"][0]["name"], "Jörg");
    assert_eq!(parsed["bcc"], serde_json::json!([]));
    assert_eq!(parsed["defects"], serde_json::json!([]));
    Ok(())
}
