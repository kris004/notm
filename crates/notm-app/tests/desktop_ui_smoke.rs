use std::{
    collections::{BTreeMap, BTreeSet},
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
const TEST_HARNESS_APPLICATION_ID_ENV: &str = "NOTM_TEST_HARNESS_APPLICATION_ID";

struct FixtureApp {
    child: Child,
    socket_path: PathBuf,
    log_path: PathBuf,
    work_dir: PathBuf,
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl FixtureApp {
    fn spawn(work_dir: PathBuf, token: &str) -> anyhow::Result<Self> {
        Self::spawn_inner(work_dir, token, None, None, None)
    }

    fn spawn_with_message_id(
        work_dir: PathBuf,
        token: &str,
        message_id: &str,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(work_dir, token, None, Some(message_id), None)
    }

    fn spawn_with_application_id(
        work_dir: PathBuf,
        token: &str,
        application_id: &str,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(work_dir, token, None, None, Some(application_id))
    }

    #[cfg(unix)]
    fn spawn_with_config(
        work_dir: PathBuf,
        token: &str,
        config_path: &std::path::Path,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(work_dir, token, Some(config_path), None, None)
    }

    fn spawn_inner(
        work_dir: PathBuf,
        token: &str,
        config_path: Option<&std::path::Path>,
        message_id: Option<&str>,
        application_id: Option<&str>,
    ) -> anyhow::Result<Self> {
        let socket_path = work_dir.join("h.sock");
        let log_path = work_dir.join("notm.log");
        let home = work_dir.join("home");
        let config_home = work_dir.join("config");
        let cache_home = work_dir.join("cache");
        let data_home = work_dir.join("data");
        let state_home = work_dir.join("state");
        for directory in [&home, &config_home, &cache_home, &data_home, &state_home] {
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
        command
            .arg(&socket_path)
            .args(["--test-harness-token", token]);
        if let Some(message_id) = message_id {
            command.args(["--message-id", message_id]);
        }
        command.env_remove(TEST_HARNESS_APPLICATION_ID_ENV);
        if let Some(application_id) = application_id {
            command.env(TEST_HARNESS_APPLICATION_ID_ENV, application_id);
        }
        let child = command
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", config_home)
            .env("XDG_CACHE_HOME", cache_home)
            .env("XDG_DATA_HOME", data_home)
            .env("XDG_STATE_HOME", state_home)
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

    fn request_message_id(
        &self,
        token: &str,
        application_id: &str,
        message_id: &str,
    ) -> anyhow::Result<()> {
        let secondary = self.work_dir.join("secondary");
        let home = secondary.join("home");
        let config_home = secondary.join("config");
        let cache_home = secondary.join("cache");
        let data_home = secondary.join("data");
        let state_home = secondary.join("state");
        for directory in [&home, &config_home, &cache_home, &data_home, &state_home] {
            fs::create_dir_all(directory)
                .with_context(|| format!("creating test directory {}", directory.display()))?;
        }
        let log_path = secondary.join("notm.log");
        let log = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&log_path)
            .with_context(|| format!("creating app log {}", log_path.display()))?;
        let socket_path = secondary.join("h.sock");
        let child = Command::new(env!("CARGO_BIN_EXE_notm"))
            .args([
                "launch",
                "--fixture",
                "--test-harness",
                "--test-harness-socket",
            ])
            .arg(&socket_path)
            .args(["--test-harness-token", token, "--message-id", message_id])
            .env(TEST_HARNESS_APPLICATION_ID_ENV, application_id)
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", config_home)
            .env("XDG_CACHE_HOME", cache_home)
            .env("XDG_DATA_HOME", data_home)
            .env("XDG_STATE_HOME", state_home)
            .env("GSETTINGS_BACKEND", "memory")
            .env("NO_AT_BRIDGE", "1")
            .env("GTK_USE_PORTAL", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()
            .context("launching the secondary fixture app")?;
        let mut child = ChildGuard(child);

        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Some(status) = child.0.try_wait()? {
                ensure!(
                    status.success(),
                    "secondary message-id request failed with {status}\n{}",
                    fs::read_to_string(&log_path).unwrap_or_default()
                );
                return Ok(());
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "secondary message-id request did not exit within {STARTUP_TIMEOUT:?}\n{}",
                    fs::read_to_string(&log_path).unwrap_or_default()
                );
            }
            thread::sleep(STARTUP_POLL_INTERVAL);
        }
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
fn validated_config_launches_and_invalid_layout_requests_are_rejected() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment() else {
        eprintln!(
            "SKIP validated_config_launches_and_invalid_layout_requests_are_rejected: no DISPLAY or \
             WAYLAND_DISPLAY is available"
        );
        return Ok(());
    };
    eprintln!("running validated-config desktop UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-valid-config-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
             \n[ui]\npage_size = 1\nlayout = \"columns\"\nhtml_mode = \"visual_html_preferred\"\n\
             \n[send]\ntransport = \"external\"\nmode = \"auto\"\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
        ),
    )?;

    let token = format!("notm-valid-config-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir, &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    let health = driver.command("health", json!({}))?;
    assert_eq!(health["ok"], true, "validated app was unhealthy: {health}");

    let page = driver.command("thread_page_info", json!({}))?;
    assert_eq!(
        page["page_size"], 1,
        "configured page size was ignored: {page}"
    );
    let before = driver.command("layout_state", json!({}))?;
    assert_eq!(
        before["layout_preference"], "three_pane",
        "documented columns alias was not applied: {before}"
    );

    let rejected = driver.command("set_layout", json!({"layout": "diagonal"}))?;
    assert_eq!(
        rejected["ok"], false,
        "invalid harness layout unexpectedly succeeded: {rejected}"
    );
    ensure!(
        rejected["error"]
            .as_str()
            .is_some_and(|error| error.contains("unknown layout")),
        "invalid harness layout returned an unclear error: {rejected}"
    );
    let after = driver.command("layout_state", json!({}))?;
    assert_eq!(
        after["layout_preference"], before["layout_preference"],
        "invalid harness layout changed the active preference: before={before}, after={after}"
    );

    let blank_layout = driver.command("set_layout", json!({"layout": ""}))?;
    assert_eq!(
        blank_layout["ok"], true,
        "legacy blank layout was not treated as auto: {blank_layout}"
    );
    assert_eq!(
        blank_layout["layout"]["layout_preference"], "auto",
        "legacy blank layout did not select auto: {blank_layout}"
    );

    let original_config = fs::read_to_string(&config_path)?;
    let rejected_page_size = driver.command("save_settings", json!({"page_size": 0}))?;
    assert_eq!(
        rejected_page_size["ok"], false,
        "zero page size unexpectedly persisted: {rejected_page_size}"
    );
    ensure!(
        rejected_page_size["error"]
            .as_str()
            .is_some_and(|error| error.contains("page size must be greater than zero")),
        "zero page size returned an unclear error: {rejected_page_size}"
    );
    assert_eq!(
        fs::read_to_string(&config_path)?,
        original_config,
        "rejected page size still modified the configuration"
    );

    let saved = driver.command("save_settings", json!({"page_size": 2}))?;
    assert_eq!(saved["ok"], true, "valid settings were not saved: {saved}");
    let persisted = fs::read_to_string(&config_path)?.parse::<toml::Value>()?;
    assert_eq!(persisted["ui"]["page_size"].as_integer(), Some(2));
    assert_eq!(
        persisted["notmuch"]["open_readwrite_only_for_mutations"].as_bool(),
        Some(true),
        "settings save did not enforce the read-only Notmuch invariant: {persisted}"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn invalid_config_exits_before_exposing_the_desktop_harness() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment() else {
        eprintln!(
            "SKIP invalid_config_exits_before_exposing_the_desktop_harness: no DISPLAY or \
             WAYLAND_DISPLAY is available"
        );
        return Ok(());
    };
    eprintln!("running invalid-config desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-invalid-config-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let config_path = work_dir.join("notm.toml");
    fs::write(&config_path, "[ui]\nlayout = \"diagonal\"\n")?;

    let token = format!("notm-invalid-config-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir, &token, &config_path)?;
    let startup_error = match app.connect(&token) {
        Ok(_) => anyhow::bail!("invalid config exposed the desktop harness"),
        Err(error) => error,
    };
    let logs = app.logs();
    ensure!(
        startup_error.to_string().contains("exited during startup"),
        "invalid config failed for an unexpected reason: {startup_error:#}"
    );
    ensure!(
        logs.contains(&config_path.display().to_string()) && logs.contains("ui.layout"),
        "invalid config error did not include its path and field:\n{logs}"
    );
    ensure!(
        !app.socket_path.exists(),
        "invalid config exposed a desktop harness socket"
    );

    Ok(())
}

#[test]
fn fixture_cold_message_id_launch_preserves_target_and_startup_query() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment() else {
        eprintln!(
            "SKIP fixture_cold_message_id_launch_preserves_target_and_startup_query: no DISPLAY or \
             WAYLAND_DISPLAY is available"
        );
        return Ok(());
    };
    eprintln!("running cold message-id desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-message-id-ui-{run_id}"));
    let token = format!("notm-message-id-ui-{run_id}");
    let target = "thread-root-three-message@fixture.test";
    let mut app = FixtureApp::spawn_with_message_id(work_dir, &token, target)?;
    let mut driver = app.connect(&token)?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let state = loop {
        let response = driver.command("app_state", json!({}))?;
        if response["state"]["selected_message"]["message_id"] == target
            && response["state"]["pending_open_message_id"].is_null()
        {
            break response;
        }
        ensure!(
            Instant::now() < deadline,
            "cold message-id launch did not select {target}: {response}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };

    assert_eq!(
        state["state"]["current_query"], "tag:inbox",
        "cold target replaced the startup query instead of selecting within it: {state}"
    );
    assert_eq!(
        state["state"]["active_pane"], "Message",
        "cold target did not focus the message pane: {state}"
    );
    assert_target_message_rendered(&mut driver)?;

    Ok(())
}

#[test]
fn fixture_existing_instance_message_id_request_reaches_primary() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment() else {
        eprintln!(
            "SKIP fixture_existing_instance_message_id_request_reaches_primary: no DISPLAY or \
             WAYLAND_DISPLAY is available"
        );
        return Ok(());
    };
    eprintln!("running existing-instance message-id desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-message-id-remote-ui-{run_id}"));
    let token = format!("notm-message-id-remote-ui-{run_id}");
    let application_id = format!("dev.notm.Notm.Test.r{}", run_id.replace('-', ""));
    let target = "thread-root-three-message@fixture.test";
    let mut app = FixtureApp::spawn_with_application_id(work_dir, &token, &application_id)?;
    let mut driver = app.connect(&token)?;
    let initial = driver.command("app_state", json!({}))?;
    assert_ne!(
        initial["state"]["selected_message"]["message_id"], target,
        "primary fixture unexpectedly started on the remote target: {initial}"
    );

    app.request_message_id(&token, &application_id, target)?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let state = loop {
        let response = driver.command("app_state", json!({}))?;
        if response["state"]["selected_message"]["message_id"] == target
            && response["state"]["pending_open_message_id"].is_null()
        {
            break response;
        }
        ensure!(
            Instant::now() < deadline,
            "primary instance did not receive message-id request for {target}: {response}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };

    assert_eq!(state["state"]["active_pane"], "Message", "{state}");
    assert_eq!(state["state"]["current_query"], "tag:inbox", "{state}");
    assert_target_message_rendered(&mut driver)?;

    Ok(())
}

fn assert_target_message_rendered(driver: &mut UiDriver) -> anyhow::Result<()> {
    let rendered = driver.command("message_view_text", json!({}))?;
    let text = rendered["text"]
        .as_str()
        .with_context(|| format!("message view response has no text: {rendered}"))?;
    ensure!(
        text.contains("Thread root body."),
        "message view did not render the targeted root message: {rendered}"
    );
    ensure!(
        !text.contains("Reply two body with quote."),
        "message view rendered the thread's last message instead of the target: {rendered}"
    );
    Ok(())
}

#[test]
fn fixture_attachment_save_keeps_existing_files() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment() else {
        eprintln!(
            "SKIP fixture_attachment_save_keeps_existing_files: no DISPLAY or \
             WAYLAND_DISPLAY is available"
        );
        return Ok(());
    };
    eprintln!("running attachment no-clobber UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-attachment-ui-{run_id}"));
    let downloads = work_dir.join("downloads");
    fs::create_dir_all(&downloads)?;
    let original_path = downloads.join("note.txt");
    fs::write(&original_path, b"keep this file")?;

    let token = format!("notm-attachment-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "subject:\"Attachment message\"")?;

    let listed = driver.command("attachment_list_items", json!({}))?;
    let attachments = json_array_at(&listed, &["attachments"])?;
    ensure!(
        attachments.len() == 1 && attachments[0]["filename"] == "note.txt",
        "fixture attachment was not available in the UI: {listed}"
    );

    let first = driver.command(
        "save_selected_attachment",
        json!({"index": 0, "dir": downloads}),
    )?;
    assert_eq!(first["ok"], true, "first attachment save failed: {first}");
    let first_path = first["path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("first save returned no path: {first}"))?;
    assert_eq!(first_path, downloads.join("note (1).txt"));
    assert_eq!(fs::read(&original_path)?, b"keep this file");
    ensure!(
        String::from_utf8_lossy(&fs::read(&first_path)?).contains("attached text"),
        "first saved file did not contain fixture attachment bytes"
    );

    let second = driver.command(
        "save_selected_attachment",
        json!({"index": 0, "dir": downloads}),
    )?;
    assert_eq!(
        second["ok"], true,
        "second attachment save failed: {second}"
    );
    let second_path = second["path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("second save returned no path: {second}"))?;
    assert_eq!(second_path, downloads.join("note (2).txt"));
    assert_eq!(fs::read(&original_path)?, b"keep this file");
    ensure!(
        String::from_utf8_lossy(&fs::read(&second_path)?).contains("attached text"),
        "second saved file did not contain fixture attachment bytes"
    );

    let logs = driver.command("get_logs", json!({}))?;
    let last_operation = logs["last_operation"]
        .as_str()
        .with_context(|| format!("attachment save was not logged: {logs}"))?;
    ensure!(
        last_operation.contains(&second_path.display().to_string()),
        "attachment log did not report the collision-free path: {logs}"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn external_file_arg_send_reports_existing_sent_copy() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment() else {
        eprintln!(
            "SKIP external_file_arg_send_reports_existing_sent_copy: no DISPLAY or \
             WAYLAND_DISPLAY is available"
        );
        return Ok(());
    };
    eprintln!("running external sent-copy desktop UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-sent-copy-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let helper = work_dir.join("send-helper");
    let helper_message_path = work_dir.join("helper-message-path");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf '%s' \"$2\" > \"$1\"\ntest -s \"$2\"\n",
    )?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
    let sent_maildir = work_dir.join("Sent");
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
             \n[identity]\nname = \"Fixture Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[send]\nenabled = true\ntransport = \"external\"\ncommand = {}\nargs = [{}]\nmode = \"file_arg\"\ntimeout_seconds = 5\nsave_sent = true\nsent_maildir = {}\nindex_sent_after_send = false\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml_path(&helper),
            toml_path(&helper_message_path),
            toml_path(&sent_maildir),
        ),
    )?;

    let token = format!("notm-sent-copy-ui-{run_id}");
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
        ("compose_set_subject", "Durable sent-copy desktop smoke"),
        ("compose_set_body", "Durable sent-copy body"),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(
            response["ok"], true,
            "configured composer command {command} failed: {response}"
        );
    }

    let send = driver.command("compose_send", json!({}))?;
    assert_eq!(
        send["last_send_report"]["accepted"], true,
        "external file-argument send was not accepted: {send}"
    );
    let reported_path = send["last_send_report"]["captured_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("sent-copy send did not report a durable path: {send}"))?;
    ensure!(
        reported_path.starts_with(&sent_maildir) && reported_path.is_file(),
        "reported sent copy is not an existing file under {}: {}",
        sent_maildir.display(),
        reported_path.display()
    );

    let temporary_path = PathBuf::from(
        fs::read_to_string(&helper_message_path)
            .with_context(|| format!("reading helper path {}", helper_message_path.display()))?,
    );
    ensure!(
        temporary_path != reported_path,
        "send report exposed the helper temporary file: {}",
        reported_path.display()
    );
    ensure!(
        !temporary_path.exists(),
        "helper temporary file still exists after send: {}",
        temporary_path.display()
    );

    let sent = fs::read_to_string(&reported_path)
        .with_context(|| format!("reading durable sent copy {}", reported_path.display()))?;
    ensure!(
        sent.contains("\r\nSubject: Durable sent-copy desktop smoke\r\n")
            && sent.contains("\r\n\r\nDurable sent-copy body"),
        "durable sent copy did not contain the composed RFC5322 message:\n{sent}"
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

#[test]
fn fixture_tag_undo_restores_each_messages_original_tags() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment() else {
        eprintln!(
            "SKIP fixture_tag_undo_restores_each_messages_original_tags: no DISPLAY or \
             WAYLAND_DISPLAY is available"
        );
        return Ok(());
    };
    eprintln!("running exact tag undo UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-tag-undo-ui-{run_id}"));
    let legacy_path = work_dir.join("state/notm/tag-undo.json");
    fs::create_dir_all(legacy_path.parent().expect("legacy undo parent"))?;
    fs::write(
        &legacy_path,
        r#"[{"query":"*","mutation":{"add":[],"remove":["inbox"],"sync_maildir_flags":false},"label":"legacy"}]"#,
    )?;

    let token = format!("notm-tag-undo-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;

    let legacy = driver.command("undo_tag_actions", json!({}))?;
    ensure!(
        json_array_at(&legacy, &["actions"])?.is_empty(),
        "unsafe legacy undo entries were not invalidated: {legacy}"
    );

    let query = "subject:\"Three message thread\"";
    select_first_thread(&mut driver, query)?;
    let before = message_tags(&driver.command("app_state", json!({}))?)?;
    ensure!(
        before.len() == 3,
        "expected fixture thread messages: {before:?}"
    );

    let tagged = driver.command(
        "tag_selected",
        json!({"add": ["inbox"], "remove": ["unread"]}),
    )?;
    assert_eq!(
        tagged["state"]["last_error"],
        Value::Null,
        "tag operation failed: {tagged}"
    );
    let actions = driver.command("undo_tag_actions", json!({}))?;
    let actions = json_array_at(&actions, &["actions"])?;
    ensure!(
        actions.len() == 1,
        "expected one exact undo entry: {actions:?}"
    );
    let mutations = json_array_at(&actions[0], &["mutations"])?;
    ensure!(
        mutations.len() == 2,
        "mixed thread should record only two changed messages: {mutations:?}"
    );
    ensure!(
        actions[0].get("query").is_none(),
        "undo entry still targets a mutable query: {}",
        actions[0]
    );

    let undone = driver.command("undo_last_tag", json!({}))?;
    assert_eq!(
        undone["state"]["last_error"],
        Value::Null,
        "undo operation failed: {undone}"
    );
    let actions = driver.command("undo_tag_actions", json!({}))?;
    ensure!(
        json_array_at(&actions, &["actions"])?.is_empty(),
        "undo history was not consumed: {actions}"
    );

    select_first_thread(&mut driver, query)?;
    let restored = message_tags(&driver.command("app_state", json!({}))?)?;
    assert_eq!(restored, before);

    Ok(())
}

fn select_first_thread(driver: &mut UiDriver, query: &str) -> anyhow::Result<()> {
    let search = driver.command("run_search", json!({"query": query}))?;
    let rows = json_array_at(&search, &["state", "thread_list_items"])?;
    ensure!(rows.len() == 1, "expected one fixture thread: {search}");
    let selected = driver.command("select_thread_by_index", json!({"index": 0}))?;
    assert_eq!(
        selected["ok"], true,
        "could not select fixture thread: {selected}"
    );
    Ok(())
}

fn message_tags(state: &Value) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    json_array_at(state, &["state", "messages"])?
        .iter()
        .map(|message| {
            let message_id = message["message_id"]
                .as_str()
                .with_context(|| format!("message has no id: {message}"))?
                .to_string();
            let tags = message["tags"]
                .as_array()
                .with_context(|| format!("message has no tags: {message}"))?
                .iter()
                .map(|tag| {
                    tag.as_str()
                        .map(ToOwned::to_owned)
                        .with_context(|| format!("tag is not a string: {tag}"))
                })
                .collect::<anyhow::Result<BTreeSet<_>>>()?;
            Ok((message_id, tags))
        })
        .collect()
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
