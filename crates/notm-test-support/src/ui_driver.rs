use std::{
    io::{BufRead, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
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
}
