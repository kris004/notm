use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, ensure};
use notm_test_support::ui_driver::UiDriver;
use serde_json::{Value, json};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(50);

struct FixtureApp {
    child: Child,
    socket_path: PathBuf,
    log_path: PathBuf,
    work_dir: PathBuf,
}

impl FixtureApp {
    fn spawn(work_dir: PathBuf, token: &str) -> anyhow::Result<Self> {
        Self::spawn_inner(work_dir, token, None)
    }

    #[cfg(unix)]
    fn spawn_with_config(
        work_dir: PathBuf,
        token: &str,
        config_path: &std::path::Path,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(work_dir, token, Some(config_path))
    }

    fn spawn_inner(
        work_dir: PathBuf,
        token: &str,
        config_path: Option<&std::path::Path>,
    ) -> anyhow::Result<Self> {
        let socket_path = work_dir.join("test-harness.sock");
        let log_path = work_dir.join("notm.log");
        let home = work_dir.join("home");
        let config_home = work_dir.join("config");
        let cache_home = work_dir.join("cache");
        let data_home = work_dir.join("data");
        for directory in [&home, &config_home, &cache_home, &data_home] {
            fs::create_dir_all(directory)
                .with_context(|| format!("creating test directory {}", directory.display()))?;
        }
        let log = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&log_path)
            .with_context(|| format!("creating app log {}", log_path.display()))?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_notm"));
        if let Some(config_path) = config_path {
            command.arg("--config").arg(config_path).args([
                "launch",
                "--test-harness",
                "--test-harness-socket",
            ]);
        } else {
            command.args([
                "launch",
                "--fixture",
                "--test-harness",
                "--test-harness-socket",
            ]);
        }
        let child = command
            .arg(&socket_path)
            .args(["--test-harness-token", token])
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", config_home)
            .env("XDG_CACHE_HOME", cache_home)
            .env("XDG_DATA_HOME", data_home)
            .env("GSETTINGS_BACKEND", "memory")
            .env("NO_AT_BRIDGE", "1")
            .env("GTK_USE_PORTAL", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()
            .context("launching the fixture app")?;

        Ok(Self {
            child,
            socket_path,
            log_path,
            work_dir,
        })
    }

    fn connect(&mut self, token: &str) -> anyhow::Result<UiDriver> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait()? {
                anyhow::bail!(
                    "fixture app exited during startup with {status}\n{}",
                    self.logs()
                );
            }

            if self.socket_path.exists()
                && let Ok(driver) = UiDriver::connect(&self.socket_path, token)
            {
                return Ok(driver);
            }

            if Instant::now() >= deadline {
                anyhow::bail!(
                    "fixture app did not expose its test harness within {STARTUP_TIMEOUT:?}\n{}",
                    self.logs()
                );
            }
            thread::sleep(STARTUP_POLL_INTERVAL);
        }
    }

    fn logs(&self) -> String {
        fs::read_to_string(&self.log_path)
            .unwrap_or_else(|err| format!("could not read app log: {err}"))
    }
}

impl Drop for FixtureApp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.work_dir);
    }
}

#[test]
fn fixture_app_serves_authenticated_desktop_harness() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment() else {
        eprintln!(
            "SKIP fixture_app_serves_authenticated_desktop_harness: no DISPLAY or \
             WAYLAND_DISPLAY is available"
        );
        return Ok(());
    };
    eprintln!("running desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-ui-smoke-{run_id}"));
    let token = format!("notm-ui-smoke-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;

    let mut unauthorized = app.connect("wrong-token")?;
    let response = unauthorized.command("health", json!({}))?;
    assert_eq!(
        response["ok"], false,
        "unexpected auth response: {response}"
    );
    assert_eq!(
        response["error"], "invalid token",
        "unexpected auth response: {response}"
    );
    drop(unauthorized);

    let mut driver = app.connect(&token)?;
    let health = driver.command("health", json!({}))?;
    assert_eq!(health["ok"], true, "unhealthy fixture app: {health}");
    assert_eq!(
        health["state"], "running",
        "unhealthy fixture app: {health}"
    );

    let search = driver.command("run_search", json!({"query": "tag:inbox"}))?;
    assert_eq!(search["ok"], true, "fixture search failed: {search}");
    let rows = json_array_at(&search, &["state", "thread_list_items"])?;
    ensure!(!rows.is_empty(), "fixture search returned no thread rows");
    ensure!(
        rows.iter()
            .any(|row| row["subject"] == "Unread inbox message"),
        "fixture search did not return the known inbox message: {rows:?}"
    );
    ensure!(
        rows.iter().all(|row| {
            row["tags"]
                .as_array()
                .is_some_and(|tags| tags.iter().any(|tag| tag == "inbox"))
        }),
        "tag:inbox returned a row without the inbox tag: {rows:?}"
    );

    let page = driver.command("thread_page_info", json!({}))?;
    assert_eq!(page["ok"], true, "page inspection failed: {page}");
    assert_eq!(
        page["current_query"], "tag:inbox",
        "page query did not match the requested fixture search: {page}"
    );
    assert_eq!(
        page["loaded"].as_u64(),
        Some(rows.len() as u64),
        "page metadata did not report the rendered fixture rows: {page}"
    );
    ensure!(
        page["loaded"].as_u64().is_some_and(|loaded| loaded > 0),
        "page metadata reported no loaded fixture rows: {page}"
    );

    let open_compose = driver.command("open_compose", json!({}))?;
    assert_eq!(
        open_compose["ok"], true,
        "fixture composer did not open: {open_compose}"
    );
    for (command, value) in [
        ("compose_set_from", "Fixture Sender <sender@example.test>"),
        ("compose_set_to", "Visible <visible@example.test>"),
        (
            "compose_set_bcc",
            "Hidden <hidden@example.test>, second@example.test",
        ),
        ("compose_set_subject", "Bcc desktop smoke"),
        ("compose_set_body", "Desktop smoke body"),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(
            response["ok"], true,
            "fixture composer command {command} failed: {response}"
        );
    }

    let send = driver.command("compose_send", json!({}))?;
    assert_eq!(
        send["last_send_report"]["accepted"], true,
        "fixture composer send was not accepted: {send}"
    );
    let captured_path = send["last_send_report"]["captured_path"]
        .as_str()
        .with_context(|| format!("fixture send did not report a capture path: {send}"))?;
    let captured = fs::read_to_string(captured_path)
        .with_context(|| format!("reading fixture send capture {captured_path}"))?;
    ensure!(
        captured.contains("\r\nBcc: Hidden <hidden@example.test>, second@example.test\r\n"),
        "fixture composer dropped Bcc recipients from its send submission:\n{captured}"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn timed_out_send_reports_failure_and_leaves_desktop_responsive() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment() else {
        eprintln!(
            "SKIP timed_out_send_reports_failure_and_leaves_desktop_responsive: no DISPLAY or \
             WAYLAND_DISPLAY is available"
        );
        return Ok(());
    };
    eprintln!("running send-timeout desktop UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-send-timeout-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let helper = work_dir.join("send-helper");
    let survived_marker = work_dir.join("descendant-survived");
    fs::write(
        &helper,
        "#!/bin/sh\n(\n  sleep 2\n  printf 'survived\\n' > \"$1\"\n) &\nwait\n",
    )?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
             \n[identity]\nname = \"Fixture Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[send]\nenabled = true\ncommand = {}\nargs = [{}]\nmode = \"stdin_rfc5322\"\ntimeout_seconds = 1\nsave_sent = false\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml_path(&helper),
            toml_path(&survived_marker),
        ),
    )?;

    let token = format!("notm-send-timeout-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir, &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    let health = driver.command("health", json!({}))?;
    assert_eq!(health["ok"], true, "unhealthy configured app: {health}");

    let open_compose = driver.command("open_compose", json!({}))?;
    assert_eq!(
        open_compose["ok"], true,
        "configured composer did not open: {open_compose}"
    );
    for (command, value) in [
        ("compose_set_from", "Fixture Sender <sender@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Timeout desktop smoke"),
        ("compose_set_body", "Timeout desktop smoke body"),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(
            response["ok"], true,
            "configured composer command {command} failed: {response}"
        );
    }

    let send = driver.command("compose_send", json!({}))?;
    ensure!(
        send["last_send_report"].is_null(),
        "timed-out send unexpectedly produced a report: {send}"
    );
    let last_error = send["last_error"]
        .as_str()
        .with_context(|| format!("timed-out send did not report an error: {send}"))?;
    ensure!(
        last_error.contains("send command timed out after 1s"),
        "unexpected timed-out send error: {last_error}"
    );

    let state = driver.command("app_state", json!({}))?;
    assert_eq!(
        state["state"]["compose_fields"]["subject"], "Timeout desktop smoke",
        "failed send cleared the composer: {state}"
    );
    let health = driver.command("health", json!({}))?;
    assert_eq!(
        health["ok"], true,
        "desktop did not recover after send timeout: {health}"
    );

    thread::sleep(Duration::from_secs(2));
    ensure!(
        !survived_marker.exists(),
        "send helper descendant survived after the UI reported a timeout"
    );

    Ok(())
}

#[cfg(unix)]
fn toml_path(path: &std::path::Path) -> String {
    toml::Value::String(path.display().to_string()).to_string()
}

fn gtk_display_environment() -> Option<String> {
    display_environment_from(|name| std::env::var(name).ok())
}

fn display_environment_from(
    mut get_variable: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    ["WAYLAND_DISPLAY", "DISPLAY"].into_iter().find_map(|name| {
        get_variable(name)
            .filter(|value| !value.is_empty())
            .map(|value| format!("{name}={value}"))
    })
}

fn unique_run_id() -> anyhow::Result<String> {
    let epoch_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_nanos();
    Ok(format!("{}-{epoch_nanos}", std::process::id()))
}

fn json_array_at<'a>(value: &'a Value, path: &[&str]) -> anyhow::Result<&'a Vec<Value>> {
    let mut current = value;
    for key in path {
        current = current
            .get(*key)
            .with_context(|| format!("response has no `{}` field: {value}", path.join(".")))?;
    }
    current.as_array().with_context(|| {
        format!(
            "response field `{}` is not an array: {value}",
            path.join(".")
        )
    })
}

#[test]
fn display_gate_requires_a_nonempty_display_name() {
    assert_eq!(display_environment_from(|_| None), None);
    assert_eq!(
        display_environment_from(|name| match name {
            "WAYLAND_DISPLAY" => Some(String::new()),
            "DISPLAY" => Some(":42".to_string()),
            _ => None,
        }),
        Some("DISPLAY=:42".to_string())
    );
}
