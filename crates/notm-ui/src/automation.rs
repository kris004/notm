use std::{
    io::{BufRead, Write},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug)]
pub struct AutomationRequest {
    pub command: String,
    pub args: Value,
    pub response: mpsc::Sender<Value>,
    pub response_written: mpsc::Receiver<()>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationConfig {
    pub socket_path: PathBuf,
    pub token: String,
}

pub fn spawn(config: AutomationConfig, tx: mpsc::Sender<AutomationRequest>) -> anyhow::Result<()> {
    if config.socket_path.exists() {
        std::fs::remove_file(&config.socket_path)?;
    }
    let listener = UnixListener::bind(&config.socket_path)?;
    listener.set_nonblocking(true)?;
    thread::spawn(move || {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let tx = tx.clone();
                    let token = config.token.clone();
                    thread::spawn(move || handle_client(stream, token, tx));
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(err) => {
                    eprintln!("notm test harness accept failed: {err}");
                    break;
                }
            }
        }
    });
    Ok(())
}

fn handle_client(
    stream: std::os::unix::net::UnixStream,
    token: String,
    tx: mpsc::Sender<AutomationRequest>,
) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let reader = std::io::BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        let parsed: serde_json::Result<Incoming> = serde_json::from_str(&line);
        let Ok(incoming) = parsed else {
            let _ = writeln!(writer, "{}", json!({"ok":false,"error":"invalid json"}));
            continue;
        };
        if incoming.token != token {
            let _ = writeln!(writer, "{}", json!({"ok":false,"error":"invalid token"}));
            continue;
        }
        let (resp_tx, resp_rx) = mpsc::channel();
        let (written_tx, written_rx) = mpsc::channel();
        if tx
            .send(AutomationRequest {
                command: incoming.command,
                args: incoming.args,
                response: resp_tx,
                response_written: written_rx,
            })
            .is_err()
        {
            let _ = writeln!(
                writer,
                "{}",
                json!({"ok":false,"error":"app channel closed"})
            );
            continue;
        }
        match resp_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(value) => {
                let _ = writeln!(writer, "{}", value);
                let _ = writer.flush();
                let _ = written_tx.send(());
            }
            Err(_) => {
                let _ = writeln!(
                    writer,
                    "{}",
                    json!({"ok":false,"error":"test harness command timed out"})
                );
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct Incoming {
    token: String,
    command: String,
    #[serde(default)]
    args: Value,
}

pub fn default_socket_path() -> PathBuf {
    Path::new("/tmp").join(format!("notm-{}.sock", std::process::id()))
}
