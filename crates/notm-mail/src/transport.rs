use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    io,
    path::{Component, Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};

use anyhow::Context;
use async_trait::async_trait;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    time::timeout,
};

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
        let output = run_command_with_timeout(
            command,
            Some(message.to_rfc5322().into_bytes()),
            self.timeout,
        )
        .await?;
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
        let output = run_command_with_timeout(command, None, self.timeout).await?;
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
        let output = run_command_with_timeout(command, None, self.timeout).await?;
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

async fn run_command_with_timeout(
    mut command: Command,
    input: Option<Vec<u8>>,
    timeout_duration: Duration,
) -> anyhow::Result<Output> {
    command.kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().context("starting send command")?;
    let child_id = child.id();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let execution = async {
        let (status, (), stdout, stderr) = tokio::try_join!(
            child.wait(),
            write_child_stdin(stdin, input),
            read_child_output(stdout),
            read_child_output(stderr),
        )?;
        Ok::<Output, io::Error>(Output {
            status,
            stdout,
            stderr,
        })
    };

    match timeout(timeout_duration, execution).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(err)) => {
            terminate_and_reap(&mut child, child_id)
                .await
                .context("cleaning up send command after an I/O failure")?;
            Err(err).context("communicating with send command")
        }
        Err(_) => {
            terminate_and_reap(&mut child, child_id)
                .await
                .with_context(|| {
                    format!("send command timed out after {timeout_duration:?} and cleanup failed")
                })?;
            anyhow::bail!("send command timed out after {timeout_duration:?}");
        }
    }
}

async fn write_child_stdin(
    mut stdin: Option<ChildStdin>,
    input: Option<Vec<u8>>,
) -> io::Result<()> {
    if let Some(input) = input {
        let stdin = stdin.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "send command stdin was not configured as a pipe",
            )
        })?;
        stdin.write_all(&input).await?;
    }
    Ok(())
}

async fn read_child_output<R>(mut output: Option<R>) -> io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    if let Some(output) = output.as_mut() {
        output.read_to_end(&mut bytes).await?;
    }
    Ok(bytes)
}

#[cfg(unix)]
async fn terminate_and_reap(child: &mut Child, child_id: Option<u32>) -> io::Result<()> {
    if let Some(child_id) = child_id {
        if let Err(group_err) = kill_process_group(child_id) {
            child.start_kill().map_err(|child_err| {
                io::Error::new(
                    child_err.kind(),
                    format!(
                        "could not kill send process group ({group_err}) or direct child ({child_err})"
                    ),
                )
            })?;
            child.wait().await?;
            return Err(io::Error::new(
                group_err.kind(),
                format!("could not kill send process group: {group_err}"),
            ));
        }
    } else {
        child.start_kill()?;
    }
    child.wait().await.map(|_| ())
}

#[cfg(unix)]
fn kill_process_group(child_id: u32) -> io::Result<()> {
    let process_group = libc::pid_t::try_from(child_id).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("send command process ID {child_id} is outside pid_t range"),
        )
    })?;
    // The command is placed into a new process group before it is spawned, so
    // this signal is scoped to the helper and descendants that did not
    // deliberately leave that group.
    let result = unsafe { libc::killpg(process_group, libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(err)
    }
}

#[cfg(not(unix))]
async fn terminate_and_reap(child: &mut Child, _child_id: Option<u32>) -> io::Result<()> {
    child.start_kill()?;
    child.wait().await.map(|_| ())
}

fn report_from_output(output: Output, captured_path: Option<String>) -> SendReport {
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

    fn test_message(body: impl Into<String>) -> ComposedMessage {
        ComposedMessage::new(
            "Sender <sender@example.test>".to_string(),
            vec!["recipient@example.test".to_string()],
            "Subject".to_string(),
            body.into(),
        )
    }

    #[cfg(unix)]
    fn write_executable_script(path: &Path, contents: &str) {
        use std::{fs, os::unix::fs::PermissionsExt};

        fs::write(path, contents).expect("write send helper");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .expect("make send helper executable");
    }

    #[cfg(unix)]
    fn assert_child_reaped(pid_path: &Path) {
        let pid = std::fs::read_to_string(pid_path)
            .expect("read send helper PID")
            .trim()
            .parse::<libc::pid_t>()
            .expect("parse send helper PID");
        let mut status = 0;
        // This process spawned the helper. ECHILD is therefore direct evidence
        // that the transport already waited for it instead of leaving a zombie.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        let err = io::Error::last_os_error();
        assert_eq!(result, -1, "send helper PID {pid} was not already reaped");
        assert_eq!(
            err.raw_os_error(),
            Some(libc::ECHILD),
            "unexpected waitpid result for send helper PID {pid}: {err}"
        );
    }

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

    #[cfg(unix)]
    #[tokio::test]
    async fn preserves_output_and_status_from_completed_command() {
        let temp = tempfile::tempdir().expect("create temp directory");
        let helper = temp.path().join("send-helper");
        write_executable_script(
            &helper,
            "#!/bin/sh\ncat >/dev/null\nprintf 'helper stdout'\nprintf 'helper stderr' >&2\nexit 7\n",
        );
        let transport = ExternalCommandTransport {
            command: helper,
            args: Vec::new(),
            mode: TransportMode::StdinRfc5322,
            working_dir: None,
            env: BTreeMap::new(),
            timeout: Duration::from_secs(3),
        };

        let report = transport
            .send(test_message("Body"))
            .await
            .expect("completed helper should return a report");

        assert!(!report.accepted);
        assert_eq!(report.exit_status, Some(7));
        assert_eq!(report.stdout, "helper stdout");
        assert_eq!(report.stderr, "helper stderr");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_covers_blocked_stdin_and_reaps_helper() {
        use std::time::Instant;

        let temp = tempfile::tempdir().expect("create temp directory");
        let helper = temp.path().join("send-helper");
        let pid_path = temp.path().join("helper.pid");
        write_executable_script(
            &helper,
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$1\"\nsleep 30\n",
        );
        let transport = ExternalCommandTransport {
            command: helper,
            args: vec![pid_path.display().to_string()],
            mode: TransportMode::StdinRfc5322,
            working_dir: None,
            env: BTreeMap::new(),
            timeout: Duration::from_secs(1),
        };
        let started = Instant::now();

        let err = transport
            .send(test_message("x".repeat(8 * 1024 * 1024)))
            .await
            .expect_err("helper that never reads stdin should time out");

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "blocked stdin was not covered by the transport timeout"
        );
        assert!(
            err.to_string().contains("timed out after 1s"),
            "unexpected timeout error: {err:#}"
        );
        assert_child_reaped(&pid_path);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_descendants_in_send_process_group() {
        let temp = tempfile::tempdir().expect("create temp directory");
        let helper = temp.path().join("send-helper");
        let pid_path = temp.path().join("helper.pid");
        let survived_path = temp.path().join("descendant-survived");
        write_executable_script(
            &helper,
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$1\"\n(\n  sleep 2\n  printf 'survived\\n' > \"$2\"\n) &\nwait\n",
        );
        let transport = ExternalCommandTransport {
            command: helper,
            args: vec![
                pid_path.display().to_string(),
                survived_path.display().to_string(),
            ],
            mode: TransportMode::FileArg,
            working_dir: None,
            env: BTreeMap::new(),
            timeout: Duration::from_secs(1),
        };

        let err = transport
            .send(test_message("Body"))
            .await
            .expect_err("waiting helper should time out");

        assert!(
            err.to_string().contains("timed out after 1s"),
            "unexpected timeout error: {err:#}"
        );
        assert_child_reaped(&pid_path);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(
            !survived_path.exists(),
            "send helper descendant survived process-group cleanup"
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
        let message = test_message("Body");

        let err = transport
            .send(message)
            .await
            .expect_err("missing {file} should fail before running command");
        assert!(err.to_string().contains("requires at least one send arg"));
    }
}
