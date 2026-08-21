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
            let state = self.command("app_state", json!({}))?;
            anyhow::ensure!(state["ok"] == true, "app state failed: {state}");
            if search_snapshots_are_settled(&status, &state)? {
                if let Some(error) = status["error"].as_str() {
                    anyhow::bail!("search failed: {error}");
                }
                return Ok(state);
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "search did not complete within {timeout:?}: status={status}, state={state}"
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

fn search_snapshots_are_settled(status: &Value, state: &Value) -> anyhow::Result<bool> {
    let status_loading = status["loading"]
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("search status has no loading flag: {status}"))?;
    let status_generation = status["generation"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("search status has no generation: {status}"))?;
    let state_loading = state["state"]["search_loading"]
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("app state has no search-loading flag: {state}"))?;
    let state_generation = state["state"]["search_generation"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("app state has no search generation: {state}"))?;

    Ok(status_generation > 0
        && status_generation == state_generation
        && !status_loading
        && !state_loading)
}

#[cfg(test)]
mod tests {
    use super::search_snapshots_are_settled;
    use serde_json::json;

    #[test]
    fn search_wait_rejects_startup_and_between_snapshot_races() {
        let idle_before_startup = json!({"loading": false, "generation": 0});
        let idle_state = json!({
            "state": {"search_loading": false, "search_generation": 0}
        });
        assert!(!search_snapshots_are_settled(&idle_before_startup, &idle_state).unwrap());

        let prior_status = json!({"loading": false, "generation": 1});
        let newer_search_state = json!({
            "state": {"search_loading": true, "search_generation": 2}
        });
        assert!(!search_snapshots_are_settled(&prior_status, &newer_search_state).unwrap());

        let settled_status = json!({"loading": false, "generation": 2});
        let settled_state = json!({
            "state": {"search_loading": false, "search_generation": 2}
        });
        assert!(search_snapshots_are_settled(&settled_status, &settled_state).unwrap());
    }
}
