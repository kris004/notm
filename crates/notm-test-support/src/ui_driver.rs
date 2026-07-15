use std::{
    io::{BufRead, Write},
    os::unix::net::UnixStream,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

pub struct UiDriver {
    stream: UnixStream,
    token: String,
}

impl UiDriver {
    pub fn connect(path: impl AsRef<Path>, token: impl Into<String>) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self {
            stream,
            token: token.into(),
        })
    }

    pub fn command(&mut self, command: &str, args: Value) -> anyhow::Result<Value> {
        let req = json!({"token": self.token, "command": command, "args": args});
        writeln!(self.stream, "{}", serde_json::to_string(&req)?)?;
        let mut reader = std::io::BufReader::new(self.stream.try_clone()?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        Ok(serde_json::from_str(&line)?)
    }

    pub fn wait_for_search(&mut self, timeout: Duration) -> anyhow::Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.command("search_status", json!({}))?;
            anyhow::ensure!(status["ok"] == true, "search status failed: {status}");
            let loading = status["loading"]
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("search status has no loading flag: {status}"))?;
            if !loading {
                if let Some(error) = status["error"].as_str() {
                    anyhow::bail!("search failed: {error}");
                }
                return self.command("app_state", json!({}));
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "search did not complete within {timeout:?}: {status}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn wait_for_send(&mut self, timeout: Duration) -> anyhow::Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let response = self.command("app_state", json!({}))?;
            anyhow::ensure!(response["ok"] == true, "app state failed: {response}");
            let in_progress = response["state"]["send_in_progress"]
                .as_bool()
                .ok_or_else(|| {
                    anyhow::anyhow!("app state has no send-in-progress flag: {response}")
                })?;
            if !in_progress {
                return Ok(response);
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "send did not complete within {timeout:?}: {response}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }
}
