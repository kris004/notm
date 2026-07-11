use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};

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
        let search_path = self
            .env
            .get("PATH")
            .map(|path| path.into())
            .or_else(|| env::var_os("PATH"));
        let Some(resolved_command) = resolve_executable(
            &self.command,
            self.working_dir.as_deref(),
            search_path.as_deref(),
        ) else {
            details.push(format!("command not found: {}", self.command.display()));
            return Ok(ProbeReport { ok: false, details });
        };
        if is_bare_command(&self.command) {
            details.push(format!(
                "command resolved through PATH: {} -> {}",
                self.command.display(),
                resolved_command.display()
            ));
        } else if resolved_command == self.command {
            details.push(format!("command exists: {}", self.command.display()));
        } else {
            details.push(format!(
                "command resolved from working directory: {} -> {}",
                self.command.display(),
                resolved_command.display()
            ));
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

fn resolve_executable(
    command: &Path,
    working_dir: Option<&Path>,
    search_path: Option<&OsStr>,
) -> Option<PathBuf> {
    if !is_bare_command(command) {
        let candidate = resolve_from_working_dir(command, working_dir);
        return is_executable_file(&candidate).then_some(candidate);
    }

    env::split_paths(search_path?)
        .map(|directory| resolve_from_working_dir(&directory.join(command), working_dir))
        .find(|candidate| is_executable_file(candidate))
}

fn is_bare_command(command: &Path) -> bool {
    let mut components = command.components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn resolve_from_working_dir(path: &Path, working_dir: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else if let Some(working_dir) = working_dir {
        working_dir.join(path)
    } else {
        path.to_path_buf()
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

impl ExternalCommandTransport {
    async fn send_stdin(&self, message: ComposedMessage) -> anyhow::Result<SendReport> {
        let mut command = self.base_command();
        command
            .args(&self.args)
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
        if !self.args.iter().any(|arg| arg.contains("{file}")) {
            anyhow::bail!(
                "command_template send mode requires at least one send arg containing `{{file}}`"
            );
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn resolves_executable_bare_name_from_supplied_search_path() {
        use std::{fs, os::unix::fs::PermissionsExt};

        let temp = tempfile::tempdir().expect("create temp directory");
        let bin_dir = temp.path().join("bin");
        fs::create_dir(&bin_dir).expect("create bin directory");
        let helper = bin_dir.join("send-helper");
        fs::write(&helper, "#!/bin/sh\nexit 0\n").expect("write helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))
            .expect("make helper executable");
        let search_path = env::join_paths([&bin_dir]).expect("construct search path");

        assert_eq!(
            resolve_executable(Path::new("send-helper"), None, Some(&search_path)),
            Some(helper)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_resolves_relative_command_from_working_directory() {
        use std::{fs, os::unix::fs::PermissionsExt};

        let temp = tempfile::tempdir().expect("create temp directory");
        let helper = temp.path().join("send-helper");
        fs::write(&helper, "#!/bin/sh\nexit 0\n").expect("write helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))
            .expect("make helper executable");
        let transport = ExternalCommandTransport {
            command: PathBuf::from("./send-helper"),
            args: Vec::new(),
            mode: TransportMode::StdinRfc5322,
            working_dir: Some(temp.path().to_path_buf()),
            env: BTreeMap::new(),
            timeout: Duration::from_secs(1),
        };

        let report = transport.probe().await.expect("probe relative helper");

        assert!(report.ok, "relative helper should resolve: {report:?}");
        assert!(
            report
                .details
                .iter()
                .any(|detail| detail.contains("resolved from working directory")),
            "probe should explain relative-command resolution: {report:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_executable_file_on_search_path() {
        use std::{fs, os::unix::fs::PermissionsExt};

        let temp = tempfile::tempdir().expect("create temp directory");
        let helper = temp.path().join("send-helper");
        fs::write(&helper, "#!/bin/sh\nexit 0\n").expect("write helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o644))
            .expect("leave helper non-executable");
        let search_path = env::join_paths([temp.path()]).expect("construct search path");

        assert_eq!(
            resolve_executable(Path::new("send-helper"), None, Some(&search_path)),
            None
        );
    }

    #[tokio::test]
    async fn command_template_requires_file_placeholder() {
        let transport = ExternalCommandTransport {
            command: PathBuf::from("unused"),
            args: vec!["--message".to_string()],
            mode: TransportMode::CommandTemplate,
            working_dir: None,
            env: BTreeMap::new(),
            timeout: Duration::from_secs(1),
        };
        let message = ComposedMessage::new(
            "Sender <sender@example.test>".to_string(),
            vec!["recipient@example.test".to_string()],
            "Subject".to_string(),
            "Body".to_string(),
        );

        let err = transport
            .send(message)
            .await
            .expect_err("missing {file} should fail before running command");
        assert!(err.to_string().contains("requires at least one send arg"));
    }
}
