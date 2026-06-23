use std::{collections::BTreeMap, path::PathBuf, process::Stdio, time::Duration};

use async_trait::async_trait;
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

use crate::{
    compose::ComposedMessage,
    send::{ProbeReport, SendReport, TransportDescription},
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum TransportMode {
    Auto,
    StdinRfc5322,
    FileArg,
    CommandTemplate,
}

#[async_trait]
pub trait SendTransport: Send + Sync {
    fn describe(&self) -> TransportDescription;
    async fn probe(&self) -> anyhow::Result<ProbeReport>;
    async fn send(&self, message: ComposedMessage) -> anyhow::Result<SendReport>;
}

#[derive(Debug, Clone)]
pub struct FakeSendTransport {
    pub capture_dir: PathBuf,
}

#[async_trait]
impl SendTransport for FakeSendTransport {
    fn describe(&self) -> TransportDescription {
        TransportDescription {
            name: "fake".to_string(),
            mode: "capture".to_string(),
            command: None,
        }
    }

    async fn probe(&self) -> anyhow::Result<ProbeReport> {
        std::fs::create_dir_all(&self.capture_dir)?;
        Ok(ProbeReport {
            ok: true,
            details: vec![format!("capture dir {}", self.capture_dir.display())],
        })
    }

    async fn send(&self, message: ComposedMessage) -> anyhow::Result<SendReport> {
        std::fs::create_dir_all(&self.capture_dir)?;
        let path = self.capture_dir.join(format!(
            "{}.eml",
            message.message_id.trim_matches(['<', '>'])
        ));
        std::fs::write(&path, message.to_rfc5322())?;
        Ok(SendReport {
            accepted: true,
            exit_status: Some(0),
            stdout: String::new(),
            stderr: String::new(),
            captured_path: Some(path.display().to_string()),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ExternalCommandTransport {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub mode: TransportMode,
    pub working_dir: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub timeout: Duration,
}

#[async_trait]
impl SendTransport for ExternalCommandTransport {
    fn describe(&self) -> TransportDescription {
        TransportDescription {
            name: "external-command".to_string(),
            mode: format!("{:?}", self.mode),
            command: Some(self.command.display().to_string()),
        }
    }

    async fn probe(&self) -> anyhow::Result<ProbeReport> {
        let mut details = Vec::new();
        if !self.command.exists() {
            return Ok(ProbeReport {
                ok: false,
                details: vec![format!("command not found: {}", self.command.display())],
            });
        }
        details.push(format!("command exists: {}", self.command.display()));
        if let Some(dir) = &self.working_dir {
            if dir.is_dir() {
                details.push(format!("working directory exists: {}", dir.display()));
            } else {
                return Ok(ProbeReport {
                    ok: false,
                    details: vec![format!("working directory missing: {}", dir.display())],
                });
            }
        }
        if let Ok(text) = std::fs::read_to_string(&self.command)
            && text.contains("gmi send")
        {
            details.push("script looks like the configured Gmail/lieer send helper; auto mode uses stdin-RFC5322 and appends -t when no explicit recipient/template args are configured".to_string());
            if let Some(repo) = extract_gmailieer_repo(&text) {
                if std::path::Path::new(&repo).is_dir() {
                    details.push(format!("lieer repo exists: {repo}"));
                } else {
                    details.push(format!("lieer repo not found: {repo}"));
                }
            }
        }
        Ok(ProbeReport { ok: true, details })
    }

    async fn send(&self, message: ComposedMessage) -> anyhow::Result<SendReport> {
        let mode = match self.mode {
            TransportMode::Auto => TransportMode::StdinRfc5322,
            ref mode => mode.clone(),
        };
        match mode {
            TransportMode::StdinRfc5322 => self.send_stdin(message).await,
            TransportMode::FileArg => self.send_file_arg(message).await,
            TransportMode::CommandTemplate => self.send_template(message).await,
            TransportMode::Auto => unreachable!(),
        }
    }
}

impl ExternalCommandTransport {
    async fn send_stdin(&self, message: ComposedMessage) -> anyhow::Result<SendReport> {
        let mut command = self.base_command();
        command
            .args(self.effective_stdin_args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(message.to_rfc5322().as_bytes()).await?;
        }
        let output = timeout(self.timeout, child.wait_with_output()).await??;
        Ok(report_from_output(output, None))
    }

    async fn send_file_arg(&self, message: ComposedMessage) -> anyhow::Result<SendReport> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("message.eml");
        std::fs::write(&path, message.to_rfc5322())?;
        let mut command = self.base_command();
        command
            .args(&self.args)
            .arg(&path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = timeout(self.timeout, command.output()).await??;
        Ok(report_from_output(output, Some(path.display().to_string())))
    }

    async fn send_template(&self, message: ComposedMessage) -> anyhow::Result<SendReport> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("message.eml");
        std::fs::write(&path, message.to_rfc5322())?;
        let rendered_args = self
            .args
            .iter()
            .map(|arg| arg.replace("{file}", &path.display().to_string()))
            .collect::<Vec<_>>();
        let mut command = self.base_command();
        command
            .args(rendered_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = timeout(self.timeout, command.output()).await??;
        Ok(report_from_output(output, Some(path.display().to_string())))
    }

    fn effective_stdin_args(&self) -> Vec<String> {
        let mut args = self.args.clone();
        if matches!(self.mode, TransportMode::Auto)
            && !args.iter().any(|arg| arg == "-t")
            && self.looks_like_lieer_send_helper()
        {
            args.push("-t".to_string());
        }
        args
    }

    fn looks_like_lieer_send_helper(&self) -> bool {
        std::fs::read_to_string(&self.command)
            .map(|text| text.contains("gmi send"))
            .unwrap_or(false)
    }

    fn base_command(&self) -> Command {
        let mut command = Command::new(&self.command);
        if let Some(dir) = &self.working_dir {
            command.current_dir(dir);
        }
        command.envs(&self.env);
        command
    }
}

fn report_from_output(output: std::process::Output, captured_path: Option<String>) -> SendReport {
    SendReport {
        accepted: output.status.success(),
        exit_status: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        captured_path,
    }
}

fn extract_gmailieer_repo(script: &str) -> Option<String> {
    script.lines().find_map(|line| {
        line.trim()
            .strip_prefix("repo=${GMAILIEER_REPO:-")
            .and_then(|rest| rest.strip_suffix('}'))
            .map(ToOwned::to_owned)
    })
}
