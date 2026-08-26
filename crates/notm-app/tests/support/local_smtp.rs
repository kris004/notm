use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, ensure};
use serde_json::Value;

#[derive(Debug)]
pub(crate) struct CapturedSmtpMessage {
    pub(crate) mail_from: String,
    pub(crate) rcpt_to: Vec<String>,
    pub(crate) data: Vec<u8>,
}

pub(crate) struct LocalSmtpCapture {
    address: SocketAddrV4,
    messages: Receiver<anyhow::Result<CapturedSmtpMessage>>,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl LocalSmtpCapture {
    pub(crate) fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .context("binding the local SMTP capture server")?;
        listener
            .set_nonblocking(true)
            .context("making the local SMTP listener nonblocking")?;
        let address = match listener.local_addr()? {
            std::net::SocketAddr::V4(address) => address,
            std::net::SocketAddr::V6(_) => unreachable!("the listener is explicitly IPv4"),
        };
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let (sender, messages) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("notm-local-smtp-capture".to_string())
            .spawn(move || {
                while !worker_shutdown.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if let Err(error) = handle_connection(stream, &sender) {
                                let _ = sender.send(Err(error));
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => {
                            let _ = sender.send(Err(error).context("accepting an SMTP connection"));
                            break;
                        }
                    }
                }
            })?;

        Ok(Self {
            address,
            messages,
            shutdown,
            worker: Some(worker),
        })
    }

    pub(crate) fn port(&self) -> u16 {
        self.address.port()
    }

    pub(crate) fn wait_for_messages(
        &self,
        count: usize,
        timeout: Duration,
    ) -> anyhow::Result<Vec<CapturedSmtpMessage>> {
        let deadline = Instant::now() + timeout;
        let mut messages = Vec::with_capacity(count);
        while messages.len() < count {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(
                !remaining.is_zero(),
                "local SMTP server captured only {} of {count} messages before {timeout:?}",
                messages.len()
            );
            let message = self.messages.recv_timeout(remaining).with_context(|| {
                format!(
                    "waiting for local SMTP message {} of {count}",
                    messages.len() + 1
                )
            })??;
            messages.push(message);
        }
        Ok(messages)
    }

    pub(crate) fn ensure_no_message(&self, timeout: Duration) -> anyhow::Result<()> {
        match self.messages.recv_timeout(timeout) {
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("local SMTP capture server stopped unexpectedly")
            }
            Ok(Err(error)) => Err(error),
            Ok(Ok(message)) => anyhow::bail!(
                "unexpected SMTP message from {:?} to {:?}",
                message.mail_from,
                message.rcpt_to
            ),
        }
    }
}

impl Drop for LocalSmtpCapture {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // Wake a nonblocking accept loop promptly without contacting anything
        // outside this process.
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    sender: &mpsc::Sender<anyhow::Result<CapturedSmtpMessage>>,
) -> anyhow::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    write_response(&mut stream, b"220 localhost notm test capture\r\n")?;

    let mut mail_from = None;
    let mut recipients = Vec::new();
    loop {
        let line = read_smtp_line(&mut reader)?;
        let command = String::from_utf8_lossy(&line);
        let trimmed = command.trim_end_matches(['\r', '\n']);
        let upper = trimmed.to_ascii_uppercase();

        if upper.starts_with("EHLO ") {
            write_response(&mut stream, b"250-localhost\r\n250 SIZE 104857600\r\n")?;
        } else if upper.starts_with("HELO ") {
            write_response(&mut stream, b"250 localhost\r\n")?;
        } else if upper.starts_with("MAIL FROM:") {
            mail_from = Some(parse_smtp_path(&trimmed[10..]));
            recipients.clear();
            write_response(&mut stream, b"250 sender accepted\r\n")?;
        } else if upper.starts_with("RCPT TO:") {
            ensure!(mail_from.is_some(), "SMTP RCPT arrived before MAIL");
            recipients.push(parse_smtp_path(&trimmed[8..]));
            write_response(&mut stream, b"250 recipient accepted\r\n")?;
        } else if upper == "DATA" {
            let from = mail_from
                .clone()
                .context("SMTP DATA arrived before MAIL FROM")?;
            ensure!(
                !recipients.is_empty(),
                "SMTP DATA arrived without recipients"
            );
            write_response(&mut stream, b"354 end data with <CRLF>.<CRLF>\r\n")?;
            let data = read_smtp_data(&mut reader)?;
            sender
                .send(Ok(CapturedSmtpMessage {
                    mail_from: from,
                    rcpt_to: recipients.clone(),
                    data,
                }))
                .context("reporting a captured SMTP message")?;
            mail_from = None;
            recipients.clear();
            write_response(&mut stream, b"250 message captured\r\n")?;
        } else if upper == "RSET" {
            mail_from = None;
            recipients.clear();
            write_response(&mut stream, b"250 reset\r\n")?;
        } else if upper == "NOOP" {
            write_response(&mut stream, b"250 ok\r\n")?;
        } else if upper == "QUIT" {
            write_response(&mut stream, b"221 closing connection\r\n")?;
            return Ok(());
        } else {
            anyhow::bail!("unsupported SMTP command {trimmed:?}");
        }
    }
}

fn read_smtp_line(reader: &mut BufReader<TcpStream>) -> anyhow::Result<Vec<u8>> {
    let mut line = Vec::new();
    let read = reader.read_until(b'\n', &mut line)?;
    ensure!(read > 0, "SMTP client closed the connection unexpectedly");
    ensure!(line.ends_with(b"\r\n"), "SMTP command did not use CRLF");
    Ok(line)
}

fn read_smtp_data(reader: &mut BufReader<TcpStream>) -> anyhow::Result<Vec<u8>> {
    let mut data = Vec::new();
    loop {
        let line = read_smtp_line(reader)?;
        if line == b".\r\n" {
            break;
        }
        if line.starts_with(b"..") {
            data.extend_from_slice(&line[1..]);
        } else {
            data.extend_from_slice(&line);
        }
    }
    Ok(data)
}

fn parse_smtp_path(value: &str) -> String {
    let value = value.trim();
    let value = value.split_ascii_whitespace().next().unwrap_or(value);
    value
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .unwrap_or(value)
        .to_string()
}

fn write_response(stream: &mut TcpStream, response: &[u8]) -> anyhow::Result<()> {
    stream.write_all(response)?;
    stream.flush()?;
    Ok(())
}

pub(crate) fn write_python_submission_helper(path: &Path, port: u16) -> anyhow::Result<()> {
    let script = format!(
        r#"#!/usr/bin/env python3
import smtplib
import sys
from email import policy
from email.parser import BytesParser
from email.utils import getaddresses

raw = sys.stdin.buffer.read()
message = BytesParser(policy=policy.default).parsebytes(raw)

def addresses(name):
    return [address for _, address in getaddresses([str(value) for value in message.get_all(name, [])]) if address]

senders = addresses("From")
if len(senders) != 1:
    raise SystemExit(f"expected exactly one From mailbox, got {{senders!r}}")
recipients = addresses("To") + addresses("Cc") + addresses("Bcc")
if not recipients:
    raise SystemExit("message has no envelope recipients")

separator = b"\r\n\r\n"
if separator not in raw:
    raise SystemExit("message has no CRLF header/body separator")
header, body = raw.split(separator, 1)
lines = header.splitlines(keepends=True)
kept = []
skipping_bcc = False
for line in lines:
    continuation = line.startswith((b" ", b"\t"))
    if continuation:
        if not skipping_bcc:
            kept.append(line)
        continue
    skipping_bcc = line.split(b":", 1)[0].strip().lower() == b"bcc"
    if not skipping_bcc:
        kept.append(line)
wire = b"".join(kept) + separator + body

with smtplib.SMTP("127.0.0.1", {port}, timeout=10) as smtp:
    refused = smtp.sendmail(senders[0], recipients, wire)
if refused:
    raise SystemExit(f"SMTP recipients refused: {{refused!r}}")
"#
    );
    fs::write(path, script).with_context(|| format!("writing helper {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("making helper {} executable", path.display()))?;
    Ok(())
}

pub(crate) fn parse_wire_with_python(path: &Path) -> anyhow::Result<Value> {
    let parser = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/parse_wire.py");
    let output = Command::new("python3")
        .args(["-I", "-B"])
        .arg(&parser)
        .arg(path)
        .output()
        .with_context(|| format!("running independent parser {}", parser.display()))?;
    ensure!(
        output.status.success(),
        "independent parser failed with {}:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "decoding independent parser output: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}
