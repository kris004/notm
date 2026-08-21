use std::{
    io,
    process::{Output, Stdio},
    time::Duration,
};

use anyhow::Context;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    time::timeout,
};

/// Maximum number of bytes retained from each external command output stream.
///
/// The runner continues draining stdout and stderr after reaching this limit so
/// a verbose child cannot block on a full pipe. Excess bytes are discarded.
pub const EXTERNAL_COMMAND_OUTPUT_LIMIT: usize = 64 * 1024;

/// Runs an external command with bounded output capture and a wall-clock timeout.
///
/// `operation` identifies the command in returned errors, such as `"send"` or
/// `"receive"`. Standard input is piped only when `input` is present; otherwise
/// it is disconnected. Stdout and stderr are always captured, with at most
/// [`EXTERNAL_COMMAND_OUTPUT_LIMIT`] bytes retained from each stream.
///
/// A completed command is returned as [`Output`] regardless of its exit status.
/// On Unix, the command starts in a new process group so timeout and I/O-error
/// cleanup terminate its descendants before reaping the direct child.
pub async fn run_external_command(
    operation: &str,
    mut command: Command,
    input: Option<Vec<u8>>,
    timeout_duration: Duration,
) -> anyhow::Result<Output> {
    if input.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command
        .spawn()
        .with_context(|| format!("starting {operation} command"))?;
    let child_id = child.id();
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let execution = async {
        let (status, (), stdout, stderr) = tokio::try_join!(
            child.wait(),
            write_child_stdin(stdin, input, operation),
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
            terminate_and_reap(&mut child, child_id, operation)
                .await
                .with_context(|| format!("cleaning up {operation} command after an I/O failure"))?;
            Err(err).with_context(|| format!("communicating with {operation} command"))
        }
        Err(_) => {
            terminate_and_reap(&mut child, child_id, operation)
                .await
                .with_context(|| {
                    format!(
                        "{operation} command timed out after {timeout_duration:?} and cleanup failed"
                    )
                })?;
            anyhow::bail!("{operation} command timed out after {timeout_duration:?}");
        }
    }
}

async fn write_child_stdin(
    mut stdin: Option<ChildStdin>,
    input: Option<Vec<u8>>,
    operation: &str,
) -> io::Result<()> {
    if let Some(input) = input {
        let stdin = stdin.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                format!("{operation} command stdin was not configured as a pipe"),
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
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8192];
    if let Some(output) = output.as_mut() {
        loop {
            let read = output.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            let remaining = EXTERNAL_COMMAND_OUTPUT_LIMIT.saturating_sub(captured.len());
            captured.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    Ok(captured)
}

#[cfg(unix)]
async fn terminate_and_reap(
    child: &mut Child,
    child_id: Option<u32>,
    operation: &str,
) -> io::Result<()> {
    if let Some(child_id) = child_id {
        if let Err(group_err) = kill_process_group(child_id, operation) {
            child.start_kill().map_err(|child_err| {
                io::Error::new(
                    child_err.kind(),
                    format!(
                        "could not kill {operation} process group ({group_err}) or direct child ({child_err})"
                    ),
                )
            })?;
            child.wait().await?;
            return Err(io::Error::new(
                group_err.kind(),
                format!("could not kill {operation} process group: {group_err}"),
            ));
        }
    } else {
        child.start_kill()?;
    }
    child.wait().await.map(|_| ())
}

#[cfg(unix)]
fn kill_process_group(child_id: u32, operation: &str) -> io::Result<()> {
    let process_group = libc::pid_t::try_from(child_id).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{operation} command process ID {child_id} is outside pid_t range"),
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
async fn terminate_and_reap(
    child: &mut Child,
    _child_id: Option<u32>,
    _operation: &str,
) -> io::Result<()> {
    child.start_kill()?;
    child.wait().await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[cfg(unix)]
    fn write_executable_script(path: &Path, contents: &str) {
        use std::{fs, os::unix::fs::PermissionsExt};

        fs::write(path, contents).expect("write command helper");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .expect("make command helper executable");
    }

    #[cfg(unix)]
    fn assert_child_reaped(pid_path: &Path) {
        let pid = std::fs::read_to_string(pid_path)
            .expect("read command helper PID")
            .trim()
            .parse::<libc::pid_t>()
            .expect("parse command helper PID");
        let mut status = 0;
        // This process spawned the helper. ECHILD is direct evidence that the
        // runner already waited for it instead of leaving a zombie.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        let err = io::Error::last_os_error();
        assert_eq!(result, -1, "command helper PID {pid} was not reaped");
        assert_eq!(
            err.raw_os_error(),
            Some(libc::ECHILD),
            "unexpected waitpid result for command helper PID {pid}: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preserves_output_and_nonzero_status_from_completed_command() {
        let temp = tempfile::tempdir().expect("create temp directory");
        let helper = temp.path().join("command-helper");
        write_executable_script(
            &helper,
            "#!/bin/sh\nprintf 'normal stdout'\nprintf 'normal stderr' >&2\nexit 7\n",
        );

        let output = run_external_command(
            "receive",
            Command::new(helper),
            None,
            Duration::from_secs(3),
        )
        .await
        .expect("completed command should return output");

        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"normal stdout");
        assert_eq!(output.stderr, b"normal stderr");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounds_each_output_stream_while_draining_to_completion() {
        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "dd if=/dev/zero bs=65536 count=2 2>/dev/null; \
                 dd if=/dev/zero bs=65536 count=2 >&2 2>/dev/null",
        );

        let output = run_external_command("database update", command, None, Duration::from_secs(3))
            .await
            .expect("verbose command should complete without blocking");

        assert!(output.status.success());
        assert_eq!(output.stdout.len(), EXTERNAL_COMMAND_OUTPUT_LIMIT);
        assert_eq!(output.stderr.len(), EXTERNAL_COMMAND_OUTPUT_LIMIT);
        assert!(output.stdout.iter().all(|byte| *byte == 0));
        assert!(output.stderr.iter().all(|byte| *byte == 0));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_uses_operation_name_and_kills_descendants_before_reaping() {
        let temp = tempfile::tempdir().expect("create temp directory");
        let helper = temp.path().join("command-helper");
        let pid_path = temp.path().join("helper.pid");
        let survived_path = temp.path().join("descendant-survived");
        write_executable_script(
            &helper,
            "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$1\"\n(\n  sleep 2\n  printf 'survived\\n' > \"$2\"\n) &\nwait\n",
        );
        let mut command = Command::new(helper);
        command.arg(&pid_path).arg(&survived_path);

        let err = run_external_command("receive", command, None, Duration::from_secs(1))
            .await
            .expect_err("waiting command should time out");

        assert_eq!(
            err.to_string(),
            "receive command timed out after 1s",
            "timeout should identify the requested operation: {err:#}"
        );
        assert_child_reaped(&pid_path);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(
            !survived_path.exists(),
            "command descendant survived process-group cleanup"
        );
    }
}
