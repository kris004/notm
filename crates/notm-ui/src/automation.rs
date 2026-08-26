use std::{
    fs,
    io::{BufRead, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Bounded server-side fail-safe for authenticated test-harness commands.
///
/// Ordinary drivers impose a shorter 10-second responsiveness deadline. This
/// stays slightly above the 30-second opt-in deadline used by correctness-only
/// scenarios so the server does not preempt the client that owns the timeout.
pub const TEST_HARNESS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(35);

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
    let (listener, socket_guard) = bind_listener(&config.socket_path)?;
    thread::Builder::new()
        .name("notm-test-harness".into())
        .spawn(move || {
            let _socket_guard = socket_guard;
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
        })?;
    Ok(())
}

fn bind_listener(socket_path: &Path) -> anyhow::Result<(UnixListener, SocketPathGuard)> {
    bind_listener_with_setup(socket_path, |listener, socket_path| {
        fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))?;
        let metadata = fs::symlink_metadata(socket_path)?;
        if !metadata.file_type().is_socket() || metadata.permissions().mode() & 0o777 != 0o600 {
            anyhow::bail!(
                "test harness socket `{}` was not created with owner-only permissions",
                socket_path.display()
            );
        }

        listener.set_nonblocking(true)?;
        Ok(())
    })
}

fn bind_listener_with_setup(
    socket_path: &Path,
    setup: impl FnOnce(&UnixListener, &Path) -> anyhow::Result<()>,
) -> anyhow::Result<(UnixListener, SocketPathGuard)> {
    remove_stale_socket(socket_path)?;

    let listener = UnixListener::bind(socket_path)?;
    let socket_guard = SocketPathGuard::new(socket_path)?;
    setup(&listener, socket_path)?;
    Ok((listener, socket_guard))
}

fn remove_stale_socket(socket_path: &Path) -> anyhow::Result<()> {
    let metadata = match fs::symlink_metadata(socket_path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };

    if !metadata.file_type().is_socket() {
        anyhow::bail!(
            "refusing to replace non-socket test harness path `{}`",
            socket_path.display()
        );
    }

    match std::os::unix::net::UnixStream::connect(socket_path) {
        Ok(_) => anyhow::bail!(
            "test harness socket `{}` is already in use",
            socket_path.display()
        ),
        Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(anyhow::anyhow!(
                "could not verify whether test harness socket `{}` is stale: {err}",
                socket_path.display()
            ));
        }
    }

    let current = match fs::symlink_metadata(socket_path) {
        Ok(current) => current,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    if !same_socket(&metadata, &current) {
        anyhow::bail!(
            "test harness socket `{}` changed while it was being checked",
            socket_path.display()
        );
    }
    fs::remove_file(socket_path)?;
    Ok(())
}

fn same_socket(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.file_type().is_socket()
        && right.file_type().is_socket()
        && left.dev() == right.dev()
        && left.ino() == right.ino()
}

#[derive(Debug)]
struct SocketPathGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl SocketPathGuard {
    fn new(path: &Path) -> anyhow::Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_socket() {
            anyhow::bail!("test harness path `{}` is not a socket", path.display());
        }
        Ok(Self {
            path: path.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

impl Drop for SocketPathGuard {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
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
        match resp_rx.recv_timeout(TEST_HARNESS_RESPONSE_TIMEOUT) {
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
    default_socket_path_for(
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .as_deref(),
        std::process::id(),
    )
}

fn default_socket_path_for(runtime_dir: Option<&Path>, process_id: u32) -> PathBuf {
    runtime_dir
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| Path::new("/tmp"))
        .join(format!("notm-{process_id}.sock"))
}

#[cfg(test)]
mod tests {
    use std::os::unix::{
        fs::{MetadataExt, PermissionsExt, symlink},
        net::{UnixListener, UnixStream},
    };

    use super::*;

    #[test]
    fn refuses_to_replace_a_regular_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket_path = directory.path().join("automation.sock");
        fs::write(&socket_path, "keep me").expect("regular file");

        let error = bind_listener(&socket_path).expect_err("regular file must be rejected");

        assert!(error.to_string().contains("refusing to replace non-socket"));
        assert_eq!(fs::read_to_string(socket_path).unwrap(), "keep me");
    }

    #[test]
    fn refuses_to_replace_a_symlink() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target_path = directory.path().join("target");
        let socket_path = directory.path().join("automation.sock");
        fs::write(&target_path, "keep me").expect("target file");
        symlink(&target_path, &socket_path).expect("socket-path symlink");

        let error = bind_listener(&socket_path).expect_err("symlink must be rejected");

        assert!(error.to_string().contains("refusing to replace non-socket"));
        assert!(fs::symlink_metadata(&socket_path).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(target_path).unwrap(), "keep me");
    }

    #[test]
    fn refuses_to_replace_an_active_socket() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket_path = directory.path().join("automation.sock");
        let active_listener = UnixListener::bind(&socket_path).expect("active listener");
        let original = fs::symlink_metadata(&socket_path).expect("socket metadata");

        let error = bind_listener(&socket_path).expect_err("active socket must be rejected");

        assert!(error.to_string().contains("already in use"));
        let current = fs::symlink_metadata(&socket_path).expect("socket remains");
        assert_eq!(
            (current.dev(), current.ino()),
            (original.dev(), original.ino())
        );
        drop(active_listener);
    }

    #[test]
    fn replaces_a_stale_socket_with_an_owner_only_socket() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket_path = directory.path().join("automation.sock");
        let stale_listener = UnixListener::bind(&socket_path).expect("stale listener");
        drop(stale_listener);
        let stale_error = UnixStream::connect(&socket_path).expect_err("socket must be stale");
        assert_eq!(stale_error.kind(), std::io::ErrorKind::ConnectionRefused);

        let (_listener, guard) = bind_listener(&socket_path).expect("replacement listener");

        let current = fs::symlink_metadata(&socket_path).expect("replacement metadata");
        UnixStream::connect(&socket_path).expect("replacement listener must accept connections");
        assert_eq!(current.permissions().mode() & 0o777, 0o600);
        drop(guard);
        assert!(!socket_path.exists());
    }

    #[test]
    fn cleanup_guard_does_not_remove_a_replacement_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket_path = directory.path().join("automation.sock");
        let (_listener, guard) = bind_listener(&socket_path).expect("listener");
        fs::remove_file(&socket_path).expect("remove bound path");
        fs::write(&socket_path, "replacement").expect("replacement file");

        drop(guard);

        assert_eq!(fs::read_to_string(socket_path).unwrap(), "replacement");
    }

    #[test]
    fn setup_failure_removes_the_partially_created_socket() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket_path = directory.path().join("automation.sock");

        let error = bind_listener_with_setup(&socket_path, |_listener, _socket_path| {
            anyhow::bail!("injected setup failure")
        })
        .expect_err("setup must fail");

        assert!(error.to_string().contains("injected setup failure"));
        assert!(!socket_path.exists());
    }

    #[test]
    fn default_path_prefers_an_absolute_runtime_directory() {
        assert_eq!(
            default_socket_path_for(Some(Path::new("/run/user/1000")), 42),
            Path::new("/run/user/1000/notm-42.sock")
        );
        assert_eq!(
            default_socket_path_for(Some(Path::new("relative")), 42),
            Path::new("/tmp/notm-42.sock")
        );
    }
}
