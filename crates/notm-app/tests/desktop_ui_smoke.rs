use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, ensure};
use notm_test_support::ui_driver::UiDriver;
use serde_json::{Value, json};

#[path = "support/gui_test_display.rs"]
mod gui_test_display;
#[path = "support/local_http_tracker.rs"]
mod local_http_tracker;
#[cfg(unix)]
#[path = "support/local_smtp.rs"]
mod local_smtp;

use gui_test_display::{GuiTestDisplay, gtk_display_environment};
use local_http_tracker::LocalHttpTracker;
#[cfg(unix)]
use local_smtp::{
    CapturedSmtpMessage, LocalSmtpCapture, parse_wire_with_python, write_python_submission_helper,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const LARGE_THREAD_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const TEST_HARNESS_APPLICATION_ID_ENV: &str = "NOTM_TEST_HARNESS_APPLICATION_ID";
const FIXTURE_STARTUP_RECOVERY_DELAY_ENV: &str = "NOTM_FIXTURE_TEST_STARTUP_RECOVERY_DELAY_MS";
const FIXTURE_RECOVERY_PATH_ENV: &str = "NOTM_FIXTURE_TEST_RECOVERY_PATH";
const FIXTURE_STARTUP_RECOVERY_GATE_ENV: &str = "NOTM_FIXTURE_TEST_STARTUP_RECOVERY_GATE";
const FIXTURE_LARGE_ATTACHMENT_BYTES_ENV: &str = "NOTM_FIXTURE_TEST_LARGE_ATTACHMENT_BYTES";
const FIXTURE_HUGE_BODY_BYTES_ENV: &str = "NOTM_FIXTURE_TEST_HUGE_BODY_BYTES";
const FIXTURE_SEARCH_THREADS_ENV: &str = "NOTM_FIXTURE_TEST_SEARCH_THREADS";

struct FixtureApp {
    child: Child,
    display: Option<GuiTestDisplay>,
    socket_path: PathBuf,
    log_path: PathBuf,
    work_dir: PathBuf,
    cleanup_work_dir: bool,
}

struct ChildGuard(Child);

#[derive(Default)]
struct FixtureLaunchOptions<'a> {
    config_path: Option<&'a Path>,
    message_id: Option<&'a str>,
    mailto_uri: Option<&'a str>,
    application_id: Option<&'a str>,
    fixture: bool,
    system_prefers_dark: Option<bool>,
    startup_recovery_delay_ms: Option<u64>,
    fixture_recovery_path: Option<PathBuf>,
    fixture_startup_recovery_gate: Option<PathBuf>,
    large_attachment_bytes: Option<usize>,
    huge_body_bytes: Option<usize>,
    search_threads: Option<usize>,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl FixtureApp {
    fn spawn(work_dir: PathBuf, token: &str) -> anyhow::Result<Self> {
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                fixture: true,
                ..FixtureLaunchOptions::default()
            },
        )
    }

    fn spawn_with_message_id(
        work_dir: PathBuf,
        token: &str,
        message_id: &str,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                message_id: Some(message_id),
                fixture: true,
                ..FixtureLaunchOptions::default()
            },
        )
    }

    fn spawn_with_mailto(work_dir: PathBuf, token: &str, mailto_uri: &str) -> anyhow::Result<Self> {
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                mailto_uri: Some(mailto_uri),
                fixture: true,
                ..FixtureLaunchOptions::default()
            },
        )
    }

    fn spawn_with_application_id(
        work_dir: PathBuf,
        token: &str,
        application_id: &str,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                application_id: Some(application_id),
                fixture: true,
                ..FixtureLaunchOptions::default()
            },
        )
    }

    fn spawn_with_startup_recovery_delay(
        work_dir: PathBuf,
        token: &str,
        milliseconds: u64,
    ) -> anyhow::Result<Self> {
        let recovery_path = work_dir.join("state/notm/draft.json");
        let recovery_gate = work_dir.join("startup-recovery.release");
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                fixture: true,
                startup_recovery_delay_ms: Some(milliseconds),
                fixture_recovery_path: Some(recovery_path),
                fixture_startup_recovery_gate: Some(recovery_gate),
                ..FixtureLaunchOptions::default()
            },
        )
    }

    fn spawn_with_large_attachment(
        work_dir: PathBuf,
        token: &str,
        bytes: usize,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                fixture: true,
                large_attachment_bytes: Some(bytes),
                ..FixtureLaunchOptions::default()
            },
        )
    }

    fn spawn_with_huge_body(work_dir: PathBuf, token: &str, bytes: usize) -> anyhow::Result<Self> {
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                fixture: true,
                huge_body_bytes: Some(bytes),
                ..FixtureLaunchOptions::default()
            },
        )
    }

    fn spawn_with_search_threads(
        work_dir: PathBuf,
        token: &str,
        count: usize,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                fixture: true,
                search_threads: Some(count),
                ..FixtureLaunchOptions::default()
            },
        )
    }

    fn spawn_with_mailto_and_startup_recovery_delay(
        work_dir: PathBuf,
        token: &str,
        mailto_uri: &str,
        milliseconds: u64,
    ) -> anyhow::Result<Self> {
        let recovery_path = work_dir.join("state/notm/draft.json");
        let recovery_gate = work_dir.join("startup-recovery.release");
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                mailto_uri: Some(mailto_uri),
                fixture: true,
                startup_recovery_delay_ms: Some(milliseconds),
                fixture_recovery_path: Some(recovery_path),
                fixture_startup_recovery_gate: Some(recovery_gate),
                ..FixtureLaunchOptions::default()
            },
        )
    }

    #[cfg(unix)]
    fn spawn_with_config(
        work_dir: PathBuf,
        token: &str,
        config_path: &std::path::Path,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                config_path: Some(config_path),
                ..FixtureLaunchOptions::default()
            },
        )
    }

    #[cfg(unix)]
    fn spawn_with_config_and_application_id(
        work_dir: PathBuf,
        token: &str,
        config_path: &std::path::Path,
        application_id: &str,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                config_path: Some(config_path),
                application_id: Some(application_id),
                ..FixtureLaunchOptions::default()
            },
        )
    }

    #[cfg(unix)]
    fn spawn_fixture_with_config(
        work_dir: PathBuf,
        token: &str,
        config_path: &std::path::Path,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                config_path: Some(config_path),
                fixture: true,
                ..FixtureLaunchOptions::default()
            },
        )
    }

    #[cfg(unix)]
    fn spawn_fixture_with_config_and_system_theme(
        work_dir: PathBuf,
        token: &str,
        config_path: &std::path::Path,
        prefers_dark: bool,
    ) -> anyhow::Result<Self> {
        Self::spawn_inner(
            work_dir,
            token,
            FixtureLaunchOptions {
                config_path: Some(config_path),
                fixture: true,
                system_prefers_dark: Some(prefers_dark),
                ..FixtureLaunchOptions::default()
            },
        )
    }

    fn spawn_inner(
        work_dir: PathBuf,
        token: &str,
        options: FixtureLaunchOptions<'_>,
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
        let display = GuiTestDisplay::start(&work_dir)?;
        if let Some(prefers_dark) = options.system_prefers_dark {
            let gtk_config_home = config_home.join("gtk-4.0");
            fs::create_dir_all(&gtk_config_home)?;
            fs::write(
                gtk_config_home.join("settings.ini"),
                format!(
                    "[Settings]\ngtk-theme-name=Default\n\
                     gtk-application-prefer-dark-theme={}\n\
                     gtk-interface-color-scheme={}\n",
                    if prefers_dark { "true" } else { "false" },
                    if prefers_dark { "dark" } else { "light" }
                ),
            )?;
        }
        let log = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&log_path)
            .with_context(|| format!("creating app log {}", log_path.display()))?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_notm"));
        if let Some(config_path) = options.config_path {
            command.arg("--config").arg(config_path);
        }
        command.arg("launch");
        if options.fixture {
            command.arg("--fixture");
        }
        command.args(["--test-harness", "--test-harness-socket"]);
        command
            .arg(&socket_path)
            .args(["--test-harness-token", token]);
        if let Some(message_id) = options.message_id {
            command.args(["--message-id", message_id]);
        }
        if let Some(mailto_uri) = options.mailto_uri {
            command.arg(mailto_uri);
        }
        command.env_remove(TEST_HARNESS_APPLICATION_ID_ENV);
        if let Some(application_id) = options.application_id {
            command.env(TEST_HARNESS_APPLICATION_ID_ENV, application_id);
        }
        command.env_remove(FIXTURE_STARTUP_RECOVERY_DELAY_ENV);
        if let Some(milliseconds) = options.startup_recovery_delay_ms {
            command.env(FIXTURE_STARTUP_RECOVERY_DELAY_ENV, milliseconds.to_string());
        }
        command.env_remove(FIXTURE_RECOVERY_PATH_ENV);
        if let Some(path) = options.fixture_recovery_path {
            command.env(FIXTURE_RECOVERY_PATH_ENV, path);
        }
        command.env_remove(FIXTURE_STARTUP_RECOVERY_GATE_ENV);
        if let Some(path) = options.fixture_startup_recovery_gate {
            command.env(FIXTURE_STARTUP_RECOVERY_GATE_ENV, path);
        }
        command.env_remove(FIXTURE_LARGE_ATTACHMENT_BYTES_ENV);
        if let Some(bytes) = options.large_attachment_bytes {
            command.env(FIXTURE_LARGE_ATTACHMENT_BYTES_ENV, bytes.to_string());
        }
        command.env_remove(FIXTURE_HUGE_BODY_BYTES_ENV);
        if let Some(bytes) = options.huge_body_bytes {
            command.env(FIXTURE_HUGE_BODY_BYTES_ENV, bytes.to_string());
        }
        command.env_remove(FIXTURE_SEARCH_THREADS_ENV);
        if let Some(count) = options.search_threads {
            command.env(FIXTURE_SEARCH_THREADS_ENV, count.to_string());
        }
        // Keep non-fixture smokes independent of the invoking account's
        // Notmuch selection and libnotmuch's NAME/EMAIL identity defaults.
        command
            .env_remove("NOTMUCH_CONFIG")
            .env_remove("NOTMUCH_DATABASE")
            .env_remove("NOTMUCH_PROFILE")
            .env_remove("MAILDIR")
            .env("EMAIL", "")
            .env("NAME", "");
        command.env_remove("GTK_THEME");
        if options.system_prefers_dark.is_some() {
            command.env("GDK_DEBUG", "default-settings");
        }
        display.configure_command(&mut command);
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
            display: Some(display),
            socket_path,
            log_path,
            work_dir,
            cleanup_work_dir: true,
        })
    }

    fn connect(&mut self, token: &str) -> anyhow::Result<UiDriver> {
        self.connect_with_command_timeout(token, Duration::from_secs(10))
    }

    fn connect_with_command_timeout(
        &mut self,
        token: &str,
        command_timeout: Duration,
    ) -> anyhow::Result<UiDriver> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait()? {
                anyhow::bail!(
                    "fixture app exited during startup with {status}\n{}",
                    self.logs()
                );
            }

            if self.socket_path.exists()
                && let Ok(driver) =
                    UiDriver::connect_with_timeout(&self.socket_path, token, command_timeout)
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
        let app_log = fs::read_to_string(&self.log_path)
            .unwrap_or_else(|err| format!("could not read app log: {err}"));
        match self
            .display
            .as_ref()
            .and_then(GuiTestDisplay::diagnostic_log)
        {
            Some(display_log) => {
                format!("--- notm log ---\n{app_log}\n--- compositor log ---\n{display_log}")
            }
            None => app_log,
        }
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> anyhow::Result<std::process::ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "fixture app did not exit within {timeout:?}\n{}",
                    self.logs()
                );
            }
            thread::sleep(STARTUP_POLL_INTERVAL);
        }
    }

    fn preserve_work_dir_on_drop(&mut self) {
        self.cleanup_work_dir = false;
    }

    fn request_message_id(
        &self,
        token: &str,
        application_id: &str,
        message_id: &str,
    ) -> anyhow::Result<()> {
        self.request_launch_target(
            token,
            application_id,
            &["--message-id", message_id],
            "message-id",
        )
    }

    fn request_mailto(
        &self,
        token: &str,
        application_id: &str,
        mailto_uri: &str,
    ) -> anyhow::Result<()> {
        self.request_launch_target(token, application_id, &[mailto_uri], "mailto")
    }

    fn request_launch_target(
        &self,
        token: &str,
        application_id: &str,
        target_args: &[&str],
        target_name: &str,
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_notm"));
        command.args([
            "launch",
            "--fixture",
            "--test-harness",
            "--test-harness-socket",
        ]);
        command
            .arg(&socket_path)
            .args(["--test-harness-token", token])
            .args(target_args)
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
            .stderr(Stdio::from(log));
        if let Some(display) = &self.display {
            display.configure_command(&mut command);
        }
        let child = command
            .spawn()
            .context("launching the secondary fixture app")?;
        let mut child = ChildGuard(child);

        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Some(status) = child.0.try_wait()? {
                ensure!(
                    status.success(),
                    "secondary {target_name} request failed with {status}\n{}",
                    fs::read_to_string(&log_path).unwrap_or_default()
                );
                return Ok(());
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "secondary {target_name} request did not exit within {STARTUP_TIMEOUT:?}\n{}",
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
        drop(self.display.take());
        if self.cleanup_work_dir {
            let _ = fs::remove_dir_all(&self.work_dir);
        }
    }
}

fn wait_for_search_generation_loading(
    driver: &mut UiDriver,
    generation: u64,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = driver.command("search_status", json!({}))?;
        ensure!(status["ok"] == true, "search status failed: {status}");
        let current_generation = status["generation"]
            .as_u64()
            .with_context(|| format!("search status had no generation: {status}"))?;
        ensure!(
            current_generation <= generation,
            "search generation {generation} was superseded before its loading state was observed: {status}"
        );
        if current_generation == generation && status["loading"] == true {
            return Ok(status);
        }
        ensure!(
            Instant::now() < deadline,
            "search generation {generation} did not report loading within {timeout:?}: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

#[test]
fn fixture_app_serves_authenticated_desktop_harness() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_app_serves_authenticated_desktop_harness: no GUI test display is available"
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
    driver.wait_for_search(STARTUP_TIMEOUT)?;

    let delayed_search = driver.command(
        "run_search",
        json!({"query": "subject:\"Unicode\"", "test_delay_ms": 1200}),
    )?;
    assert_eq!(
        delayed_search["ok"], true,
        "fixture search was not scheduled: {delayed_search}"
    );
    assert_eq!(
        delayed_search["scheduled"], true,
        "fixture search response did not report async scheduling: {delayed_search}"
    );
    let delayed_generation = delayed_search["generation"]
        .as_u64()
        .context("delayed search response had no generation")?;
    let outstanding = wait_for_search_generation_loading(
        &mut driver,
        delayed_generation,
        Duration::from_secs(2),
    )?;
    let responsive_health = driver.command("health", json!({}))?;
    assert_eq!(
        responsive_health["ok"], true,
        "harness stopped responding while a search was outstanding: {responsive_health}"
    );
    assert_eq!(
        outstanding["loading"], true,
        "delayed fixture search was not outstanding during the responsiveness check: {outstanding}"
    );
    let edited_search = driver.command("set_search_query", json!({"query": "tag:inbox"}))?;
    assert_eq!(
        edited_search["ok"], true,
        "editing the active query failed: {edited_search}"
    );
    let current_search = driver.command("search_status", json!({}))?;
    assert_eq!(
        current_search["loading"], true,
        "debounced query edit did not reserve background search work: {current_search}"
    );
    let current_generation = current_search["generation"]
        .as_u64()
        .context("debounced search status had no generation")?;
    ensure!(
        current_generation > delayed_generation,
        "query edit did not invalidate the outstanding generation: delayed={delayed_search}, current={current_search}"
    );
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    thread::sleep(Duration::from_millis(1300));
    let search = driver.command("app_state", json!({}))?;
    assert_eq!(
        search["state"]["current_query"], "tag:inbox",
        "stale delayed result replaced the current search: {search}"
    );
    assert_eq!(
        search["state"]["search_loading"], false,
        "stale delayed result changed the settled loading state: {search}"
    );
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

    let unread_scheduled = driver.command("select_saved_search", json!({"name": "Unread"}))?;
    assert_eq!(
        unread_scheduled["state"]["search_loading"], true,
        "saved search did not schedule background work: {unread_scheduled}"
    );
    let unread = driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(
        unread["state"]["current_query"], "tag:unread and not tag:trash and not tag:spam",
        "saved search loaded the wrong query: {unread}"
    );
    let unread_rows = json_array_at(&unread, &["state", "thread_list_items"])?;
    ensure!(
        !unread_rows.is_empty()
            && unread_rows.iter().all(|row| {
                row["tags"]
                    .as_array()
                    .is_some_and(|tags| tags.iter().any(|tag| tag == "unread"))
            }),
        "Unread saved search returned unexpected rows: {unread_rows:?}"
    );
    let inbox_scheduled = driver.command("select_saved_search", json!({"name": "Inbox"}))?;
    assert_eq!(
        inbox_scheduled["state"]["search_loading"], true,
        "Inbox restore did not schedule background work: {inbox_scheduled}"
    );
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    let direct_restore = driver.command("run_search", json!({"query": "tag:inbox"}))?;
    assert_eq!(
        direct_restore["scheduled"], true,
        "direct inbox restore was not scheduled: {direct_restore}"
    );
    let search = driver.wait_for_search(STARTUP_TIMEOUT)?;
    let rows = json_array_at(&search, &["state", "thread_list_items"])?;

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

    let started = driver.command("compose_send", json!({}))?;
    assert_eq!(started["ok"], true, "fixture send did not start: {started}");
    assert_eq!(
        started["pending"], true,
        "fixture send did not report pending work: {started}"
    );
    let send = driver.wait_for_send(STARTUP_TIMEOUT)?;
    assert_eq!(
        send["state"]["last_send_report"]["accepted"], true,
        "fixture composer send was not accepted: {send}"
    );
    let captured_path = send["state"]["last_send_report"]["captured_path"]
        .as_str()
        .with_context(|| format!("fixture send did not report a capture path: {send}"))?;
    let captured = fs::read_to_string(captured_path)
        .with_context(|| format!("reading fixture send capture {captured_path}"))?;
    ensure!(
        captured.contains("\r\nBcc: Hidden <hidden@example.test>, second@example.test\r\n"),
        "fixture composer dropped Bcc recipients from its send submission:\n{captured}"
    );

    let close = driver.command("close_main_window", json!({}))?;
    assert_eq!(close["ok"], true, "main-window close failed: {close}");
    drop(driver);
    let status = app.wait_for_exit(Duration::from_secs(3))?;
    ensure!(
        status.success(),
        "fixture app did not exit cleanly: {status}"
    );

    Ok(())
}

#[test]
fn fixture_rapid_searches_coalesce_and_apply_large_pages_incrementally() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_rapid_searches_coalesce_and_apply_large_pages_incrementally: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running coalesced-search UI stress with {display}");

    const EXTRA_THREADS: usize = 144;
    const BURST_SEARCHES: usize = 12;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-search-worker-ui-{run_id}"));
    let token = format!("notm-search-worker-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_search_threads(work_dir, &token, EXTRA_THREADS)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    let baseline = driver.command("search_status", json!({}))?;

    let barrier = Arc::new(Barrier::new(BURST_SEARCHES + 1));
    let mut burst = Vec::new();
    for index in 0..BURST_SEARCHES {
        let socket_path = app.socket_path.clone();
        let token = token.clone();
        let barrier = barrier.clone();
        burst.push(thread::spawn(move || -> anyhow::Result<Value> {
            let mut driver = UiDriver::connect(socket_path, token)?;
            barrier.wait();
            driver.command(
                "run_search",
                json!({
                    "query": format!("subject:\"discarded burst {index}\""),
                    "test_delay_ms": 1600,
                }),
            )
        }));
    }
    barrier.wait();
    for request in burst {
        let response = request.join().expect("burst search driver")?;
        assert_eq!(
            response["scheduled"], true,
            "burst search was not scheduled: {response}"
        );
    }

    let final_search = driver.command(
        "run_search",
        json!({"query": "tag:search-stress", "test_delay_ms": 800}),
    )?;
    assert_eq!(
        final_search["scheduled"], true,
        "final search was not scheduled: {final_search}"
    );
    let final_generation = final_search["generation"]
        .as_u64()
        .with_context(|| format!("final search had no generation: {final_search}"))?;

    let before = driver.command("health", json!({}))?;
    let input_started = Instant::now();
    let input = driver.command("send_key", json!({"key": "Escape"}))?;
    let input_elapsed = input_started.elapsed();
    ensure!(
        input_elapsed < Duration::from_millis(500),
        "GTK input blocked behind rapid search work for {input_elapsed:?}: {input}"
    );
    thread::sleep(Duration::from_millis(175));
    let after = driver.command("health", json!({}))?;
    ensure!(
        after["gtk_heartbeat"].as_u64().unwrap_or(0)
            > before["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK timers did not advance while the search worker was busy: before={before}, after={after}"
    );
    let outstanding = driver.command("search_status", json!({}))?;
    assert_eq!(
        outstanding["generation"], final_generation,
        "a stale search reclaimed the active generation: {outstanding}"
    );
    assert_eq!(
        outstanding["peak_active_preparations"], 1,
        "rapid searches ran preparation concurrently: {outstanding}"
    );

    let settled = driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(
        settled["state"]["current_query"], "tag:search-stress",
        "latest rapid search did not win: {settled}"
    );
    assert_eq!(
        settled["state"]["thread_list_items"]
            .as_array()
            .map(Vec::len),
        Some(100),
        "first stress page did not honor the configured page size: {settled}"
    );
    let first_page_status = driver.command("search_status", json!({}))?;
    ensure!(
        first_page_status["cancelled"].as_u64().unwrap_or(0)
            > baseline["cancelled"].as_u64().unwrap_or(0),
        "rapid searches did not cancel stale work: baseline={baseline}, final={first_page_status}"
    );
    ensure!(
        first_page_status["coalesced"].as_u64().unwrap_or(0)
            > baseline["coalesced"].as_u64().unwrap_or(0),
        "rapid searches did not coalesce queued work: baseline={baseline}, final={first_page_status}"
    );
    assert_eq!(
        first_page_status["peak_active_preparations"], 1,
        "search worker peak exceeded one: {first_page_status}"
    );
    let model = &first_page_status["model_update"];
    assert_eq!(model["busy"], false, "model update did not settle: {model}");
    ensure!(
        model["peak_rows_per_iteration"]
            .as_u64()
            .unwrap_or(u64::MAX)
            <= model["max_rows_per_update"].as_u64().unwrap_or(0),
        "thread model exceeded its GTK-iteration row budget: {model}"
    );
    assert_eq!(
        model["model_len"], 100,
        "thread model did not finish the first bounded replacement: {model}"
    );

    let appended = driver.command("load_more_threads", json!({"select_last": false}))?;
    assert_eq!(
        appended["scheduled"], true,
        "stress append was not scheduled: {appended}"
    );
    let appended = driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(
        appended["state"]["thread_list_items"]
            .as_array()
            .map(Vec::len),
        Some(EXTRA_THREADS),
        "bounded append did not load the remaining stress rows: {appended}"
    );
    let appended_status = driver.command("search_status", json!({}))?;
    assert_eq!(
        appended_status["model_update"]["model_len"], EXTRA_THREADS,
        "GTK model did not finish the bounded append: {appended_status}"
    );
    ensure!(
        appended_status["model_update"]["peak_rows_per_iteration"]
            .as_u64()
            .unwrap_or(u64::MAX)
            <= appended_status["model_update"]["max_rows_per_update"]
                .as_u64()
                .unwrap_or(0),
        "append exceeded the per-iteration GTK row budget: {appended_status}"
    );

    thread::sleep(Duration::from_millis(1700));
    let after_stale_deadline = driver.command("search_status", json!({}))?;
    assert_eq!(
        after_stale_deadline["current_query"], "tag:search-stress",
        "stale delayed search replaced the final result: {after_stale_deadline}"
    );
    let oversized = driver.command("save_settings", json!({"page_size": 1001}))?;
    assert_eq!(
        oversized["ok"], false,
        "oversized page setting was accepted: {oversized}"
    );
    ensure!(
        oversized["error"]
            .as_str()
            .is_some_and(|error| error.contains("between 1 and 1000")),
        "oversized page error did not report the portable bound: {oversized}"
    );

    eprintln!(
        "coalesced-search responsiveness passed: submitted={}, cancelled={}, coalesced={}, model_peak={}/{}",
        appended_status["submitted"],
        appended_status["cancelled"],
        appended_status["coalesced"],
        appended_status["model_update"]["peak_rows_per_iteration"],
        appended_status["model_update"]["max_rows_per_update"],
    );
    Ok(())
}

#[test]
fn fixture_visual_selection_navigation_matches_normal_viewport() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_visual_selection_navigation_matches_normal_viewport: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running visual-selection desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-visual-select-ui-{run_id}"));
    let token = format!("notm-visual-select-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    driver.command("resize_window", json!({"width": 1000, "height": 700}))?;

    let scheduled = driver.command("run_search", json!({"query": "*"}))?;
    assert_eq!(
        scheduled["scheduled"], true,
        "visual-selection fixture search was not scheduled: {scheduled}"
    );
    let initial = driver.wait_for_search(STARTUP_TIMEOUT)?;
    let rows = json_array_at(&initial, &["state", "thread_list_items"])?;
    ensure!(
        rows.len() >= 4,
        "visual-selection smoke needs at least four fixture threads: {initial}"
    );
    let selected = driver.command("select_thread_by_index", json!({"index": 0}))?;
    assert_eq!(selected["ok"], true, "initial selection failed: {selected}");
    thread::sleep(Duration::from_millis(350));
    let initial_viewport = driver.command("thread_selection_view_state", json!({}))?;
    ensure!(
        initial_viewport["scroll_upper"]
            .as_f64()
            .unwrap_or_default()
            > initial_viewport["scroll_page_size"]
                .as_f64()
                .unwrap_or_default(),
        "visual-selection smoke needs a scrollable thread list: {initial_viewport}"
    );
    let mut fully_visible_rows = 0_usize;
    for index in 0..rows.len() {
        let layout = driver.command("thread_row_layout", json!({"index": index}))?;
        if layout["row"]["fully_visible"] == true {
            fully_visible_rows += 1;
        } else {
            break;
        }
    }
    ensure!(
        fully_visible_rows > 0 && fully_visible_rows + 1 < rows.len(),
        "visual-selection smoke could not find a cursor target just below the viewport: visible={fully_visible_rows}, rows={}, viewport={initial_viewport}",
        rows.len()
    );
    let movement_steps = fully_visible_rows + 1;

    for _ in 0..movement_steps {
        driver.command("select_relative_thread", json!({"delta": 1}))?;
        thread::sleep(Duration::from_millis(250));
    }
    let normal_viewport = (
        driver.command("thread_selection_view_state", json!({}))?,
        driver.command("thread_row_layout", json!({"index": 0}))?,
    );
    driver.command("select_thread_by_index", json!({"index": 0}))?;
    thread::sleep(Duration::from_millis(350));

    let entered = driver
        .command("run_command", json!({"command": "visual_select"}))
        .context("entering visual select wedged the GTK main loop")?;
    assert_eq!(entered["ok"], true, "visual select failed: {entered}");
    assert_eq!(
        entered["state"]["visual_select_mode"], true,
        "visual select did not become active: {entered}"
    );

    let mut moved = serde_json::Value::Null;
    for _ in 0..movement_steps {
        moved = driver
            .command("select_relative_thread", json!({"delta": 1}))
            .context("moving the visual-selection cursor wedged the GTK main loop")?;
        thread::sleep(Duration::from_millis(250));
    }
    let visual_viewport = (
        driver.command("thread_selection_view_state", json!({}))?,
        driver.command("thread_row_layout", json!({"index": 0}))?,
    );
    assert_eq!(moved["ok"], true, "visual selection move failed: {moved}");
    assert_eq!(
        moved["selected_thread_index"], movement_steps,
        "visual selection did not move to the next thread: {moved}"
    );
    assert_eq!(
        moved["state"]["visual_select_cursor"], movement_steps,
        "visual selection cursor did not follow the selected row: {moved}"
    );
    ensure!(
        normal_viewport.0["row_visible"] == true && visual_viewport.0["row_visible"] == true,
        "normal or visual selection left the cursor outside the viewport: normal={normal_viewport:?}, visual={visual_viewport:?}"
    );
    assert_eq!(
        normal_viewport.1["row_visual_selected"], false,
        "normal navigation unexpectedly decorated the anchor: {normal_viewport:?}"
    );
    assert_eq!(
        visual_viewport.1["row_visual_selected"], true,
        "visual selection did not decorate the anchor in place: {visual_viewport:?}"
    );
    let normal_anchor_y = normal_viewport.1["row"]["y"]
        .as_f64()
        .with_context(|| format!("normal viewport lost the anchor row: {normal_viewport:?}"))?;
    let visual_anchor_y = visual_viewport.1["row"]["y"]
        .as_f64()
        .with_context(|| format!("visual viewport lost the anchor row: {visual_viewport:?}"))?;
    ensure!(
        (visual_anchor_y - normal_anchor_y).abs() <= 1.0,
        "visual selection moved the list differently from normal selection: normal={normal_viewport:?}, visual={visual_viewport:?}"
    );

    let health = driver
        .command("health", json!({}))
        .context("GTK main loop stopped responding after visual-selection refresh")?;
    assert_eq!(
        health["ok"], true,
        "thread-row refresh wedged the GTK main loop: {health}"
    );
    let cleared = driver.command("run_command", json!({"command": "clear_visual_selection"}))?;
    assert_eq!(
        cleared["state"]["visual_select_mode"], false,
        "visual selection did not clear: {cleared}"
    );

    Ok(())
}

#[test]
fn fixture_compose_rejects_attachment_header_injection() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_compose_rejects_attachment_header_injection: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running attachment-header desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-attachment-header-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let unsafe_filename = "résumé \"final\" \\ draft\r\nX-Injected-Filename: yes.txt";
    let attachment_path = work_dir.join(unsafe_filename);
    fs::write(&attachment_path, b"attachment header smoke")?;

    let token = format!("notm-attachment-header-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    let open_compose = driver.command("open_compose", json!({}))?;
    assert_eq!(
        open_compose["ok"], true,
        "fixture composer did not open: {open_compose}"
    );
    for (command, value) in [
        ("compose_set_from", "Fixture Sender <sender@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Attachment header desktop smoke"),
        ("compose_set_body", "Attachment header desktop smoke body"),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(
            response["ok"], true,
            "fixture composer command {command} failed: {response}"
        );
    }
    let add_attachment =
        driver.command("compose_add_attachment", json!({"path": attachment_path}))?;
    assert_eq!(
        add_attachment["ok"], true,
        "fixture attachment was not added: {add_attachment}"
    );

    let started = driver.command("compose_send", json!({}))?;
    assert_eq!(started["ok"], true, "fixture send did not start: {started}");
    assert_eq!(
        started["pending"], true,
        "fixture send did not report pending work: {started}"
    );
    let send = driver.wait_for_send(STARTUP_TIMEOUT)?;
    let error = send["state"]["last_error"]
        .as_str()
        .with_context(|| format!("attachment header injection reported no error: {send}"))?;
    ensure!(
        error.contains("attachment filename") && error.contains("control character U+000D"),
        "attachment header injection error was not actionable: {error}"
    );
    ensure!(
        send["state"]["last_send_report"].is_null(),
        "rejected attachment header injection produced a send report: {send}"
    );
    let database_path = send["state"]["database_path"]
        .as_str()
        .with_context(|| format!("fixture send reported no database path: {send}"))?;
    let capture_dir = Path::new(database_path).join("captured-send");
    let captured_count = match fs::read_dir(&capture_dir) {
        Ok(entries) => entries.filter_map(Result::ok).count(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };
    ensure!(
        captured_count == 0,
        "rejected attachment header injection wrote {captured_count} captured messages"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn fixture_harness_quarantines_external_commands() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_harness_quarantines_external_commands: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running fixture side-effect quarantine UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-fixture-safety-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let marker = work_dir.join("external-command-ran");
    let helper = work_dir.join("external-helper");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf 'external command ran\\n' >> \"$1\"\n",
    )?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
    let sync_command =
        toml::Value::String(format!("{} {}", helper.display(), marker.display())).to_string();
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[identity]\nname = \"Live User\"\nprimary_email = \"live@example.test\"\n\
             \n[send]\nenabled = true\ncommand = {}\nargs = [{}]\nmode = \"stdin_rfc5322\"\nsave_sent = true\nsent_maildir = {}\nindex_sent_after_send = true\n\
             \n[drafts]\nsave_maildir = true\nmaildir = {}\nindex_after_save = true\n\
             \n[sync]\nenabled = true\nexternal_receive_enabled = true\nexternal_receive_on_startup = true\nexternal_receive_command = {}\nnotmuch_database_update_enabled = true\nnotmuch_database_update_on_startup = true\nnotmuch_database_update_command = {}\n\
             \n[automation]\nallow_live_send_test = true\nallow_live_tag_test = true\n",
            toml_path(&helper),
            toml_path(&marker),
            toml_path(&work_dir.join("Live-Sent")),
            toml_path(&work_dir.join("Live-Drafts")),
            sync_command,
            sync_command,
        ),
    )?;

    let token = format!("notm-fixture-safety-ui-{run_id}");
    let mut app = FixtureApp::spawn_fixture_with_config(work_dir.clone(), &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    thread::sleep(Duration::from_millis(600));
    ensure!(
        !marker.exists(),
        "fixture launch ran configured startup command: {}\n{}",
        marker.display(),
        app.logs()
    );

    for (command, args) in [
        ("run_manual_sync", json!({})),
        ("run_command", json!({"command": ":sync"})),
    ] {
        let response = driver.command(command, args)?;
        assert_eq!(
            response["ok"], false,
            "fixture sync command was not blocked: {response}"
        );
        ensure!(
            response["error"]
                .as_str()
                .is_some_and(|error| error.contains("disabled in fixture mode")),
            "fixture sync error was not explicit: {response}"
        );
    }

    driver.command("open_compose", json!({}))?;
    for (command, value) in [
        ("compose_set_from", "Fixture User <fixture@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Fixture safety smoke"),
        ("compose_set_body", "Fixture safety body"),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }
    let started = driver.command("compose_send", json!({}))?;
    assert_eq!(started["ok"], true, "fixture send did not start: {started}");
    assert_eq!(
        started["pending"], true,
        "fixture send did not report pending work: {started}"
    );
    let send = driver.wait_for_send(STARTUP_TIMEOUT)?;
    assert_eq!(
        send["state"]["last_send_report"]["accepted"], true,
        "fixture fake send was not accepted: {send}"
    );
    let capture = send["state"]["last_send_report"]["captured_path"]
        .as_str()
        .with_context(|| format!("fixture send returned no capture path: {send}"))?;
    ensure!(
        PathBuf::from(capture).is_file(),
        "fixture capture does not exist: {capture}"
    );
    ensure!(
        !marker.exists(),
        "fixture send ran configured external helper: {}",
        marker.display()
    );
    ensure!(
        !work_dir.join("Live-Sent").exists() && !work_dir.join("Live-Drafts").exists(),
        "fixture mode wrote into configured live persistence directories"
    );

    select_first_thread(&mut driver, "subject:\"Unread inbox message\"")?;
    let reply = driver.command("reply_selected", json!({}))?;
    assert_eq!(
        reply["ok"], true,
        "fixture identity was not available for safe reply testing: {reply}"
    );
    assert_eq!(
        reply["pending"], true,
        "reply was not prepared asynchronously: {reply}"
    );
    wait_for_composer_preparation_idle(&mut driver, STARTUP_TIMEOUT)?;
    let reply = driver.command("app_state", json!({}))?;
    ensure!(
        reply["state"]["compose_fields"]["from"]
            .as_str()
            .is_some_and(|from| from.contains("fixture@example.test")),
        "fixture reply did not use the fixture identity: {reply}"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn slow_manual_sync_keeps_desktop_responsive() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP slow_manual_sync_keeps_desktop_responsive: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running non-blocking manual-sync UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-async-sync-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let marker = work_dir.join("sync-completed");
    let helper = work_dir.join("sync-helper");
    let send_marker = work_dir.join("send-should-not-run");
    let send_helper = work_dir.join("send-helper");
    fs::write(
        &helper,
        "#!/bin/sh\nsleep 3\nprintf 'completed\\n' > \"$1\"\n",
    )?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
    fs::write(
        &send_helper,
        "#!/bin/sh\ncat >/dev/null\nprintf 'sent\\n' > \"$1\"\n",
    )?;
    fs::set_permissions(&send_helper, fs::Permissions::from_mode(0o755))?;
    let sync_command =
        toml::Value::String(format!("{} {}", helper.display(), marker.display())).to_string();
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
             \n[identity]\nname = \"Fixture Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[sync]\nenabled = true\nexternal_receive_enabled = true\nexternal_receive_on_startup = false\nexternal_receive_command = {}\n\
             \n[send]\nenabled = true\ncommand = {}\nargs = [{}]\nmode = \"stdin_rfc5322\"\nsave_sent = false\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n\
             \n[automation]\nallow_live_send_test = true\nallow_live_tag_test = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            sync_command,
            toml_path(&send_helper),
            toml_path(&send_marker),
        ),
    )?;

    let token = format!("notm-async-sync-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir, &token, &config_path)?;
    let mut driver = app.connect(&token)?;

    select_first_thread(&mut driver, "subject:\"Unread inbox message\"")?;
    driver.command("open_compose", json!({}))?;
    for (command, value) in [
        ("compose_set_from", "Fixture Sender <sender@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Sync overlap draft"),
        ("compose_set_body", "Sync overlap body"),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }
    let saved_draft = driver.command("save_draft", json!({}))?;
    assert_eq!(
        saved_draft["ok"], true,
        "pre-sync draft save failed: {saved_draft}"
    );
    let saved_draft_path = saved_draft["report"]["local_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("saved draft has no local path: {saved_draft}"))?;
    let saved_draft_bytes = fs::read(&saved_draft_path)?;

    let started = Instant::now();
    let response = driver.command("run_manual_sync", json!({"test_refresh_delay_ms": 1200}))?;
    let start_elapsed = started.elapsed();
    assert_eq!(
        response["ok"], true,
        "manual sync did not start: {response}"
    );
    assert_eq!(
        response["pending"], true,
        "manual sync did not report pending work: {response}"
    );
    assert_eq!(
        response["state"]["sync_in_progress"], true,
        "manual sync was not marked in progress: {response}"
    );
    ensure!(
        start_elapsed < Duration::from_millis(750),
        "manual sync blocked for {start_elapsed:?} before responding"
    );

    let health_started = Instant::now();
    let health = driver.command("health", json!({}))?;
    let health_elapsed = health_started.elapsed();
    assert_eq!(health["ok"], true, "desktop became unhealthy: {health}");
    ensure!(
        health_elapsed < Duration::from_millis(750),
        "health waited {health_elapsed:?} for the sync helper"
    );
    let in_progress = driver.command("app_state", json!({}))?;
    assert_eq!(
        in_progress["state"]["sync_in_progress"], true,
        "slow helper completed before responsiveness was checked: {in_progress}"
    );

    let duplicate = driver.command("run_manual_sync", json!({}))?;
    assert_eq!(
        duplicate["ok"], false,
        "overlapping manual sync was accepted: {duplicate}"
    );
    ensure!(
        duplicate["error"]
            .as_str()
            .is_some_and(|error| error.contains("already running")),
        "overlapping sync error was not explicit: {duplicate}"
    );

    let fresh_path = fixture
        .maildir
        .join("cur")
        .join(format!("{run_id}.sync-refresh:2,"));
    fs::write(
        &fresh_path,
        format!(
            "From: refresh@example.test\r\nTo: fixture@example.test\r\n\
             Subject: Sync refresh arrival\r\nDate: Wed, 15 Jul 2026 16:00:00 -0600\r\n\
             Message-ID: <sync-refresh-{run_id}@fixture.test>\r\n\
             MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n\
             Arrived during sync.\r\n"
        ),
    )?;
    fixture
        .open_readwrite()?
        .index_file_with_tags(&fresh_path, &["inbox", "sync-refresh"])?;

    let edited = driver.command(
        "compose_set_subject",
        json!({"value": "Composer remains editable during sync"}),
    )?;
    assert_eq!(
        edited["compose_fields"]["subject"], "Composer remains editable during sync",
        "composer editing was blocked during sync: {edited}"
    );
    for (command, args) in [
        ("tag_selected", json!({"add": ["must-not-apply"]})),
        ("save_draft", json!({})),
        ("delete_active_draft", json!({})),
        ("compose_send", json!({})),
    ] {
        let blocked = driver.command(command, args)?;
        assert_eq!(
            blocked["ok"], false,
            "{command} was accepted during sync: {blocked}"
        );
        ensure!(
            blocked["error"]
                .as_str()
                .is_some_and(|error| error.contains("sync is in progress")),
            "{command} did not explain the sync conflict: {blocked}"
        );
    }
    ensure!(
        saved_draft_path.is_file(),
        "blocked draft deletion removed {}",
        saved_draft_path.display()
    );
    ensure!(
        !send_marker.exists(),
        "blocked send still executed its helper"
    );

    let restored = driver.command(
        "compose_set_subject",
        json!({"value": "Sync overlap draft"}),
    )?;
    assert_eq!(
        restored["ok"], true,
        "composer writing stayed blocked during sync: {restored}"
    );
    let before_close = draft_write_count(&draft_autosave_status(&mut driver)?)?;
    let closed = driver.command("clear_draft", json!({}))?;
    assert_eq!(
        closed["ok"], true,
        "unchanged active draft did not close during sync: {closed}"
    );
    assert_eq!(closed["pending_confirmation"], false);
    let close_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let autosave = draft_autosave_status(&mut driver)?;
        let state = driver.command("app_state", json!({}))?;
        if draft_write_count(&autosave)? > before_close
            && autosave["busy"] == false
            && state["state"]["active_draft"] == Value::Null
        {
            break;
        }
        ensure!(
            Instant::now() < close_deadline,
            "unchanged draft close did not flush and finish asynchronously: autosave={autosave}, state={state}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }

    let refresh_search = driver.command("run_search", json!({"query": "tag:sync-refresh"}))?;
    assert_eq!(
        refresh_search["scheduled"], true,
        "sync-time search was not scheduled: {refresh_search}"
    );
    let refresh_search = driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(
        refresh_search["state"]["sync_in_progress"], true,
        "sync finished before the responsiveness checks completed: {refresh_search}"
    );

    let refresh_deadline = Instant::now() + Duration::from_secs(6);
    let refreshing = loop {
        let state = driver.command("app_state", json!({}))?;
        let refresh_started = marker.is_file()
            && state["state"]["sync_in_progress"] == true
            && state["state"]["search_loading"] == true
            && state["state"]["last_operation"]
                .as_str()
                .is_some_and(|operation| operation.contains("refreshing messages"));
        if refresh_started {
            break state;
        }
        ensure!(
            Instant::now() < refresh_deadline,
            "sync did not enter its delayed refresh phase: {state}\n{}",
            app.logs()
        );
        thread::sleep(Duration::from_millis(50));
    };
    let sync_refresh_generation = refreshing["state"]["search_generation"]
        .as_u64()
        .context("sync refresh state had no search generation")?;
    let cleared = driver.command("set_search_query", json!({"query": ""}))?;
    assert_eq!(cleared["ok"], true, "clearing the search failed: {cleared}");
    thread::sleep(Duration::from_millis(250));
    let still_refreshing = driver.command("app_state", json!({}))?;
    assert_eq!(
        still_refreshing["state"]["sync_in_progress"], true,
        "clearing the search ended sync before a refresh outcome: {still_refreshing}"
    );
    assert_eq!(
        still_refreshing["state"]["search_loading"], true,
        "clearing the search cancelled the required sync refresh: {still_refreshing}"
    );
    ensure!(
        still_refreshing["state"]["full_search_outcome_generation"]
            .as_u64()
            .is_some_and(|generation| generation < sync_refresh_generation),
        "sync completed its deliberately delayed refresh too early: {still_refreshing}"
    );

    let deadline = Instant::now() + Duration::from_secs(8);
    let completed = loop {
        let state = driver.command("app_state", json!({}))?;
        if state["state"]["sync_in_progress"] == false {
            break state;
        }
        ensure!(
            Instant::now() < deadline,
            "manual sync did not finish before timeout: {state}\n{}",
            app.logs()
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        completed["state"]["last_error"],
        Value::Null,
        "manual sync failed: {completed}"
    );
    assert_eq!(
        completed["state"]["search_loading"], false,
        "sync reported completion before its refresh settled: {completed}"
    );
    assert_eq!(
        completed["state"]["current_query"], "tag:sync-refresh",
        "sync refresh replaced the user's active query: {completed}"
    );
    let refreshed_rows = json_array_at(&completed, &["state", "thread_list_items"])?;
    ensure!(
        refreshed_rows
            .iter()
            .any(|row| row["subject"] == "Sync refresh arrival"),
        "post-sync refresh did not expose the new message: {completed}"
    );
    ensure!(
        marker.is_file(),
        "manual sync helper did not create its marker"
    );

    let post_sync_tag = driver.command("tag_selected", json!({"add": ["post-sync-ok"]}))?;
    assert_eq!(
        post_sync_tag["ok"], true,
        "tagging stayed blocked after sync: {post_sync_tag}"
    );
    ensure!(
        fs::read(&saved_draft_path)? == saved_draft_bytes,
        "sync overlap mutated the persisted draft at {}",
        saved_draft_path.display()
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn startup_sync_runs_receive_then_database_update() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP startup_sync_runs_receive_then_database_update: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running startup-sync order and completion UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-startup-sync-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let marker = work_dir.join("sync-order");
    let empty_query = format!("tag:startup-sync-empty-{run_id}");
    let receive_command =
        toml::Value::String(format!("printf receive > {:?}; printf receive-ok", marker))
            .to_string();
    let update_command = toml::Value::String(format!(
        "test \"$(cat {:?})\" = receive && printf -- '-update' >> {:?} && printf update-ok",
        marker, marker
    ))
    .to_string();
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = {}\n\
             \n[sync]\nenabled = true\ntimeout_seconds = 5\n\
             external_receive_enabled = true\nexternal_receive_on_startup = true\nexternal_receive_command = {}\n\
             notmuch_database_update_enabled = true\nnotmuch_database_update_on_startup = true\nnotmuch_database_update_command = {}\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml::Value::String(empty_query),
            receive_command,
            update_command,
        ),
    )?;

    let token = format!("notm-startup-sync-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir, &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let completed = loop {
        let state = driver.command("app_state", json!({}))?;
        let order = fs::read_to_string(&marker).unwrap_or_default();
        if order == "receive-update" && state["state"]["sync_in_progress"] == false {
            break state;
        }
        ensure!(
            Instant::now() < deadline,
            "startup sync did not complete in order: marker={order:?} state={state}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };

    assert_eq!(completed["state"]["last_error"], Value::Null, "{completed}");
    let operation = completed["state"]["last_operation"]
        .as_str()
        .with_context(|| format!("startup sync has no completion report: {completed}"))?;
    ensure!(operation.starts_with("startup sync:"), "{operation}");
    ensure!(operation.contains("stdout=receive-ok"), "{operation}");
    ensure!(operation.contains("stdout=update-ok"), "{operation}");

    Ok(())
}

#[cfg(unix)]
#[test]
fn failed_manual_sync_reports_stderr_and_recovers() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP failed_manual_sync_reports_stderr_and_recovers: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running manual-sync failure recovery UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-failed-sync-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
             \n[sync]\nenabled = true\ntimeout_seconds = 5\n\
             external_receive_enabled = true\nexternal_receive_on_startup = false\n\
             external_receive_command = \"printf 'fetch diagnostic' >&2; exit 7\"\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
        ),
    )?;

    let token = format!("notm-failed-sync-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir, &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;

    for attempt in 1..=2 {
        let started = driver.command("run_manual_sync", json!({}))?;
        assert_eq!(started["ok"], true, "attempt {attempt}: {started}");
        assert_eq!(started["pending"], true, "attempt {attempt}: {started}");
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let completed = loop {
            let state = driver.command("app_state", json!({}))?;
            if state["state"]["sync_in_progress"] == false {
                break state;
            }
            ensure!(
                Instant::now() < deadline,
                "failed sync attempt {attempt} stayed pending: {state}\n{}",
                app.logs()
            );
            thread::sleep(STARTUP_POLL_INTERVAL);
        };
        let error = completed["state"]["last_error"]
            .as_str()
            .with_context(|| format!("attempt {attempt} reported no sync error: {completed}"))?;
        ensure!(error.contains("status=7"), "attempt {attempt}: {error}");
        ensure!(
            error.contains("stderr=fetch diagnostic"),
            "attempt {attempt}: {error}"
        );
    }

    let search = driver.command("run_search", json!({"query": "tag:inbox"}))?;
    assert_eq!(
        search["scheduled"], true,
        "UI stayed blocked after sync: {search}"
    );
    driver.wait_for_search(STARTUP_TIMEOUT)?;

    Ok(())
}

#[cfg(unix)]
#[test]
fn closing_main_window_waits_for_manual_sync() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP closing_main_window_waits_for_manual_sync: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running manual-sync application-lifetime UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-sync-lifetime-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let marker = work_dir.join("sync-completed");
    let helper = work_dir.join("sync-helper");
    fs::write(
        &helper,
        "#!/bin/sh\nsleep 2\nprintf 'completed\\n' > \"$1\"\n",
    )?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
    let sync_command =
        toml::Value::String(format!("{} {}", helper.display(), marker.display())).to_string();
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
             \n[sync]\nenabled = true\nexternal_receive_enabled = true\nexternal_receive_on_startup = false\nexternal_receive_command = {}\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            sync_command,
        ),
    )?;

    let token = format!("notm-sync-lifetime-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir, &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    let started = driver.command("run_manual_sync", json!({}))?;
    assert_eq!(started["ok"], true, "manual sync did not start: {started}");
    assert_eq!(
        started["state"]["sync_in_progress"], true,
        "manual sync was not pending: {started}"
    );

    let close = driver.command("close_main_window", json!({}))?;
    assert_eq!(close["ok"], true, "main-window close failed: {close}");
    drop(driver);
    thread::sleep(Duration::from_millis(250));
    ensure!(
        app.child.try_wait()?.is_none(),
        "app exited while its sync helper was still running\n{}",
        app.logs()
    );
    ensure!(
        !marker.exists(),
        "sync helper completed before lifetime check"
    );

    let status = app.wait_for_exit(Duration::from_secs(8))?;
    ensure!(
        status.success(),
        "app failed while finishing sync after close: {status}\n{}",
        app.logs()
    );
    ensure!(marker.is_file(), "sync helper was abandoned after close");

    Ok(())
}

#[cfg(unix)]
#[test]
fn live_harness_denies_ungated_mutations_and_reports_reply_noops() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP live_harness_denies_ungated_mutations_and_reports_reply_noops: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running live harness gate UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-live-gate-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let marker = work_dir.join("send-helper-ran");
    let helper = work_dir.join("send-helper");
    fs::write(&helper, "#!/bin/sh\nprintf 'sent\\n' > \"$1\"\n")?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
    let notmuch_config_path = work_dir.join("notmuch-config-without-identity");
    fs::write(
        &notmuch_config_path,
        format!("[database]\npath={}\n", fixture.root.display()),
    )?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
             \n[send]\nenabled = true\ncommand = {}\nargs = [{}]\nmode = \"stdin_rfc5322\"\nsave_sent = false\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n",
            toml_path(&fixture.root),
            toml_path(&notmuch_config_path),
            toml_path(&helper),
            toml_path(&marker),
        ),
    )?;

    let token = format!("notm-live-gate-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir.clone(), &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "subject:\"Unread inbox message\"")?;
    let tags_before = message_tags(&driver.command("app_state", json!({}))?)?;

    for (command, args, gate) in [
        ("archive_selected", json!({}), "allow_live_tag_test=true"),
        (
            "run_command",
            json!({"command": ":archive"}),
            "allow_live_tag_test=true",
        ),
    ] {
        let response = driver.command(command, args)?;
        assert_eq!(response["ok"], false, "ungated tag ran: {response}");
        ensure!(
            response["error"]
                .as_str()
                .is_some_and(|error| error.contains(gate)),
            "tag gate error did not name the opt-in: {response}"
        );
    }
    for (command, args) in [
        ("image_policy", json!({})),
        ("run_command", json!({"command": ":image_policy"})),
        ("trust_sender_images", json!({})),
        ("untrust_sender_images", json!({})),
        ("run_command", json!({"command": ":trust_sender_images"})),
        ("run_command", json!({"command": ":untrust_sender_images"})),
        ("image_policy_menu", json!({"visible": true})),
        (
            "standalone_image_policy",
            json!({"window_index": 0, "action": "sender_off"}),
        ),
    ] {
        let response = driver.command(command, args)?;
        assert_eq!(
            response["ok"], false,
            "live harness exposed fixture-only remote-image control {command}: {response}"
        );
        ensure!(
            response["error"]
                .as_str()
                .is_some_and(|error| error.contains("available only in fixture mode")),
            "fixture-only remote-image gate returned an unclear error for {command}: {response}"
        );
    }
    let tags_after = message_tags(&driver.command("app_state", json!({}))?)?;
    assert_eq!(
        tags_after, tags_before,
        "ungated tag operation changed tags"
    );

    driver.command("open_compose", json!({}))?;
    for (command, value) in [
        ("compose_set_from", "sender@example.test"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Must not send"),
        ("compose_set_body", "Must not send"),
    ] {
        driver.command(command, json!({"value": value}))?;
    }
    let send = driver.command("compose_send", json!({}))?;
    assert_eq!(send["ok"], false, "ungated live send ran: {send}");
    ensure!(
        send["error"]
            .as_str()
            .is_some_and(|error| error.contains("allow_live_send_test=true")),
        "send gate error did not name the opt-in: {send}"
    );
    ensure!(!marker.exists(), "ungated send helper was executed");

    for (command, value) in [
        ("compose_set_to", ""),
        ("compose_set_subject", ""),
        ("compose_set_body", ""),
    ] {
        let cleared = driver.command(command, json!({"value": value}))?;
        assert_eq!(
            cleared["ok"], true,
            "could not clear the gated-send fixture: {cleared}"
        );
    }
    for (command, args) in [
        ("reply_selected", json!({})),
        ("reply_all_selected", json!({})),
        ("run_command", json!({"command": ":reply"})),
    ] {
        let response = driver.command(command, args)?;
        assert_eq!(
            response["ok"], false,
            "missing-identity reply reported success: {response}"
        );
        ensure!(
            response["error"]
                .as_str()
                .is_some_and(|error| error.contains("No identity configured")),
            "reply no-op error was not truthful: {response}"
        );
    }

    Ok(())
}

#[cfg(unix)]
#[test]
fn default_draft_recovery_migrates_clears_and_reports_autosave_failures() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP default_draft_recovery_migrates_clears_and_reports_autosave_failures: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running durable draft recovery desktop UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-draft-recovery-ui-{run_id}"));
    let legacy_path = work_dir.join("cache/notm/draft.json");
    let recovery_path = work_dir.join("state/notm/draft.json");
    fs::create_dir_all(legacy_path.parent().expect("legacy draft parent"))?;
    fs::write(
        &legacy_path,
        serde_json::to_vec_pretty(&json!({
            "from": "Fixture User <fixture@example.test>",
            "to": "recipient@example.test",
            "cc": "",
            "bcc": "",
            "subject": "Recovered legacy draft",
            "body": "Recovery body"
        }))?,
    )?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
             \n[identity]\nname = \"Fixture User\"\nprimary_email = \"fixture@example.test\"\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
        ),
    )?;

    let token = format!("notm-draft-recovery-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir, &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    let recovery_status = wait_for_recovery_load_completion(&mut driver, Duration::from_secs(3))?;
    assert_eq!(
        recovery_status["outcome"], "loaded",
        "legacy recovery did not complete: {recovery_status}"
    );
    let recovered = driver.command("app_state", json!({}))?;
    assert_eq!(
        recovered["state"]["compose_fields"]["subject"], "Recovered legacy draft",
        "legacy cache draft was not recovered: {recovered}"
    );
    assert_eq!(
        recovered["state"]["compose_fields"]["body"], "Recovery body",
        "legacy cache draft body was not recovered: {recovered}"
    );
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    let recovery_confirmation = driver.command("pending_confirmation", json!({}))?;
    assert_eq!(
        recovery_confirmation["pending"],
        Value::Null,
        "startup message selection displaced a legitimate recovered draft: {recovery_confirmation}"
    );
    let recovered_after_search = driver.command("app_state", json!({}))?;
    assert_eq!(
        recovered_after_search["state"]["compose_fields"]["subject"], "Recovered legacy draft",
        "startup search replaced the recovered composer: {recovered_after_search}"
    );
    ensure!(
        recovery_path.is_file() && !legacy_path.exists(),
        "legacy draft was not moved from {} to {}",
        legacy_path.display(),
        recovery_path.display()
    );

    let before_clear = draft_write_count(&draft_autosave_status(&mut driver)?)?;
    for (command, value) in [("compose_set_body", ""), ("compose_set_subject", "")] {
        let cleared = driver.command(command, json!({"value": value}))?;
        assert_eq!(cleared["ok"], true, "composer clear failed: {cleared}");
    }
    ensure!(
        recovery_path.is_file(),
        "recovery draft disappeared while recipient content remained"
    );
    let cleared = driver.command("compose_set_to", json!({"value": ""}))?;
    assert_eq!(
        cleared["ok"], true,
        "final composer clear failed: {cleared}"
    );
    wait_for_draft_write_after(&mut driver, before_clear, Duration::from_secs(3))?;
    ensure!(
        !recovery_path.exists() && !legacy_path.exists(),
        "empty composer left stale recovery state"
    );

    fs::create_dir(&recovery_path)?;
    let before_failure = draft_write_count(&draft_autosave_status(&mut driver)?)?;
    let failed = driver.command(
        "compose_set_subject",
        json!({"value": "Autosave failure must be visible"}),
    )?;
    assert_eq!(failed["ok"], true, "composer update failed: {failed}");
    let failed_autosave =
        wait_for_draft_write_after(&mut driver, before_failure, Duration::from_secs(3))?;
    let failed_state = driver.command("app_state", json!({}))?;
    let last_error = failed_state["state"]["last_error"]
        .as_str()
        .with_context(|| format!("autosave failure was not recorded: {failed_state}"))?;
    ensure!(
        last_error.starts_with("Draft autosave failed:"),
        "unexpected autosave error: {last_error}"
    );
    let view = driver.command("html_view_state", json!({}))?;
    ensure!(
        view["status_text"]
            .as_str()
            .is_some_and(|status| status.starts_with("Draft autosave failed:")),
        "autosave failure was not shown in the desktop status: {view}"
    );
    let state_entries = fs::read_dir(recovery_path.parent().expect("recovery draft parent"))?
        .collect::<Result<Vec<_>, _>>()?;
    ensure!(
        state_entries.len() == 1 && state_entries[0].path() == recovery_path,
        "failed atomic autosave left temporary files: {state_entries:?}"
    );
    fs::remove_dir(&recovery_path)?;
    let failed_write_count = draft_write_count(&failed_autosave)?;
    let recovered_autosave = driver.command(
        "compose_set_subject",
        json!({"value": "Autosave recovered"}),
    )?;
    assert_eq!(
        recovered_autosave["ok"], true,
        "composer did not recover after transient autosave failure: {recovered_autosave}"
    );
    wait_for_draft_write_after(&mut driver, failed_write_count, Duration::from_secs(3))?;
    let recovered_state = driver.command("app_state", json!({}))?;
    assert_eq!(
        recovered_state["state"]["last_error"],
        Value::Null,
        "successful autosave left a stale failure: {recovered_state}"
    );
    let recovered_view = driver.command("html_view_state", json!({}))?;
    assert_eq!(
        recovered_view["status_text"], "Draft autosave recovered",
        "successful autosave did not clear the visible failure: {recovered_view}"
    );
    ensure!(
        recovery_path.is_file(),
        "recovered autosave did not recreate persistent draft state"
    );

    let initial_saved = driver.command("save_draft", json!({}))?;
    assert_eq!(
        initial_saved["ok"], true,
        "could not establish the durable named draft before cleanup-failure testing: {initial_saved}"
    );
    ensure!(
        !recovery_path.exists(),
        "successful named draft save did not clear recovery state"
    );
    fs::create_dir(&recovery_path)?;
    let saved_with_warning = driver.command("save_draft", json!({}))?;
    assert_eq!(
        saved_with_warning["ok"], true,
        "recovery cleanup failure was reported as a failed durable save: {saved_with_warning}"
    );
    let saved_path = saved_with_warning["report"]["local_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("partial-success save had no saved path: {saved_with_warning}"))?;
    ensure!(
        saved_path.is_file(),
        "recovery cleanup failure removed the durable saved draft"
    );
    ensure!(
        saved_with_warning["report"]["recovery_cleanup_warning"]
            .as_str()
            .is_some_and(|warning| warning.contains("could not remove recovery draft")),
        "partial-success save did not expose its cleanup warning: {saved_with_warning}"
    );
    let warning_state = driver.command("app_state", json!({}))?;
    ensure!(
        warning_state["state"]["last_error"]
            .as_str()
            .is_some_and(|warning| warning.contains("could not remove recovery draft")),
        "partial-success save did not retain its cleanup warning: {warning_state}"
    );
    fs::remove_dir(&recovery_path)?;

    Ok(())
}

fn draft_autosave_status(driver: &mut UiDriver) -> anyhow::Result<Value> {
    let status = driver.command("draft_autosave_status", json!({}))?;
    ensure!(
        status["ok"] == true,
        "draft autosave status failed: {status}"
    );
    Ok(status)
}

fn wait_for_recovery_load_completion(
    driver: &mut UiDriver,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = driver.command("recovery_load_status", json!({}))?;
        ensure!(status["ok"] == true, "recovery status failed: {status}");
        if status["busy"] == false && !status["completed_generation"].is_null() {
            return Ok(status);
        }
        ensure!(
            Instant::now() < deadline,
            "draft recovery did not complete within {timeout:?}: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

fn write_recovery_fields(path: &Path, subject: &str, body: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(&json!({
            "from": "Fixture User <fixture@example.test>",
            "to": "recipient@example.test",
            "cc": "",
            "bcc": "",
            "subject": subject,
            "body": body,
            "attachments": [],
            "in_reply_to": null,
            "references": [],
            "text_reply_quote": null,
            "html_reply_quote": null
        }))?,
    )?;
    Ok(())
}

fn wait_for_recovery_health_heartbeat(
    driver: &mut UiDriver,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let first = driver.command("health", json!({}))?;
    ensure!(first["ok"] == true, "fixture health failed: {first}");
    ensure!(
        first["recovery_load"]["busy"] == true,
        "startup recovery completed before the responsiveness warmup: {first}"
    );
    let first_heartbeat = first["gtk_heartbeat"]
        .as_u64()
        .with_context(|| format!("health response had no GTK heartbeat: {first}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        thread::sleep(STARTUP_POLL_INTERVAL);
        let health = driver.command("health", json!({}))?;
        ensure!(health["ok"] == true, "fixture health failed: {health}");
        ensure!(
            health["recovery_load"]["busy"] == true,
            "startup recovery completed before the timed responsiveness samples: {health}"
        );
        let heartbeat = health["gtk_heartbeat"]
            .as_u64()
            .with_context(|| format!("health response had no GTK heartbeat: {health}"))?;
        if heartbeat > first_heartbeat {
            return Ok(health);
        }
        ensure!(
            Instant::now() < deadline,
            "GTK heartbeat did not settle during startup recovery within {timeout:?}: first={first}, current={health}"
        );
    }
}

#[test]
fn fixture_slow_startup_recovery_is_responsive_and_stale_completion_is_safe() -> anyhow::Result<()>
{
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_slow_startup_recovery_is_responsive_and_stale_completion_is_safe: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running slow startup recovery UI smoke with {display}");

    let run_id = unique_run_id()?;
    {
        let work_dir = std::env::temp_dir().join(format!("notm-slow-startup-recovery-ui-{run_id}"));
        let recovery_path = work_dir.join("state/notm/draft.json");
        let recovery_gate = work_dir.join("startup-recovery.release");
        write_recovery_fields(
            &recovery_path,
            "Delayed startup recovery",
            "body recovered after slow startup I/O",
        )?;
        let token = format!("notm-slow-startup-recovery-ui-{run_id}");
        let mut app = FixtureApp::spawn_with_mailto_and_startup_recovery_delay(
            work_dir,
            &token,
            "mailto:startup@example.test?subject=Mailto%20after%20recovery",
            250,
        )?;
        let mut driver = app.connect(&token)?;
        let loading = driver.command("recovery_load_status", json!({}))?;
        assert_eq!(
            loading["busy"], true,
            "startup recovery was not outstanding after harness startup: {loading}"
        );
        let reported_recovery_path = loading["path"]
            .as_str()
            .map(PathBuf::from)
            .with_context(|| format!("recovery status had no path: {loading}"))?;
        assert_eq!(reported_recovery_path, recovery_path, "{loading}");

        let warmed_health =
            wait_for_recovery_health_heartbeat(&mut driver, Duration::from_secs(2))?;
        assert_eq!(
            warmed_health["recovery_load"]["busy"], true,
            "recovery gate opened during health warmup: {warmed_health}"
        );

        let first_started = Instant::now();
        let first_health = driver.command("health", json!({}))?;
        let first_elapsed = first_started.elapsed();
        thread::sleep(Duration::from_millis(150));
        let second_started = Instant::now();
        let second_health = driver.command("health", json!({}))?;
        let second_elapsed = second_started.elapsed();
        ensure!(
            first_elapsed < Duration::from_millis(500)
                && second_elapsed < Duration::from_millis(500),
            "health blocked behind delayed startup recovery: first={first_elapsed:?}, second={second_elapsed:?}"
        );
        ensure!(
            second_health["gtk_heartbeat"].as_u64().unwrap_or(0)
                > first_health["gtk_heartbeat"].as_u64().unwrap_or(0),
            "GTK heartbeat did not advance during delayed startup recovery: first={first_health}, second={second_health}"
        );
        assert_eq!(first_health["recovery_load"]["busy"], true);
        fs::write(&recovery_gate, b"release")?;

        let completed = wait_for_recovery_load_completion(&mut driver, Duration::from_secs(4))?;
        assert_eq!(
            completed["outcome"], "loaded",
            "delayed recovery did not load: {completed}"
        );
        driver.wait_for_search(STARTUP_TIMEOUT)?;
        let recovered = driver.command("app_state", json!({}))?;
        assert_eq!(
            recovered["state"]["compose_fields"]["body"], "body recovered after slow startup I/O",
            "startup search displaced the recovered composer: {recovered}"
        );
        let pending = driver.command("pending_confirmation", json!({}))?;
        assert_eq!(
            pending["pending"]["kind"], "mailto",
            "startup mailto did not wait for recovery and preserve the modal replacement workflow: {pending}"
        );
        assert_eq!(
            pending["compose_fields"]["body"], "body recovered after slow startup I/O",
            "startup mailto replaced recovery before confirmation: {pending}"
        );
        assert_eq!(
            recovery_body(&recovery_path)?,
            "body recovered after slow startup I/O"
        );
    }

    {
        let work_dir =
            std::env::temp_dir().join(format!("notm-stale-startup-recovery-ui-{run_id}"));
        let recovery_path = work_dir.join("state/notm/draft.json");
        let recovery_gate = work_dir.join("startup-recovery.release");
        write_recovery_fields(
            &recovery_path,
            "Stale startup recovery",
            "stale body must not replace a newer edit",
        )?;
        let token = format!("notm-stale-startup-recovery-ui-{run_id}");
        let mut app = FixtureApp::spawn_with_startup_recovery_delay(work_dir, &token, 250)?;
        let mut driver = app.connect(&token)?;
        let loading = driver.command("recovery_load_status", json!({}))?;
        assert_eq!(loading["busy"], true, "{loading}");
        let reported_recovery_path = loading["path"]
            .as_str()
            .map(PathBuf::from)
            .with_context(|| format!("recovery status had no path: {loading}"))?;
        assert_eq!(reported_recovery_path, recovery_path, "{loading}");
        let before_edit = draft_write_count(&draft_autosave_status(&mut driver)?)?;
        let edited = driver.command(
            "compose_set_body",
            json!({"value": "newer edit wins over startup recovery"}),
        )?;
        assert_eq!(edited["ok"], true, "composer edit failed: {edited}");
        fs::write(&recovery_gate, b"release")?;

        let completed = wait_for_recovery_load_completion(&mut driver, Duration::from_secs(4))?;
        assert_eq!(
            completed["outcome"], "superseded",
            "late recovery completion was not rejected: {completed}"
        );
        driver.wait_for_search(STARTUP_TIMEOUT)?;
        let state = driver.command("app_state", json!({}))?;
        assert_eq!(
            state["state"]["compose_fields"]["body"], "newer edit wins over startup recovery",
            "late recovery completion overwrote a newer edit: {state}"
        );
        wait_for_draft_write_after(&mut driver, before_edit, Duration::from_secs(3))?;
        assert_eq!(
            recovery_body(&recovery_path)?,
            "newer edit wins over startup recovery",
            "superseded recovery removed or replaced the newer durable recovery draft"
        );
    }

    Ok(())
}

fn draft_write_count(status: &Value) -> anyhow::Result<u64> {
    status["write_count"]
        .as_u64()
        .with_context(|| format!("draft autosave status had no write count: {status}"))
}

fn wait_for_draft_write_after(
    driver: &mut UiDriver,
    previous_write_count: u64,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = draft_autosave_status(driver)?;
        let write_count = draft_write_count(&status)?;
        let busy = status["busy"]
            .as_bool()
            .with_context(|| format!("draft autosave status had no busy flag: {status}"))?;
        if write_count > previous_write_count && !busy {
            return Ok(status);
        }
        ensure!(
            Instant::now() < deadline,
            "draft autosave did not finish a write after count {previous_write_count} within {timeout:?}: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

fn wait_for_draft_worker_after(
    driver: &mut UiDriver,
    previous_write_count: u64,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = draft_autosave_status(driver)?;
        if draft_write_count(&status)? > previous_write_count && status["busy"] == true {
            return Ok(status);
        }
        ensure!(
            Instant::now() < deadline,
            "draft autosave worker did not start after count {previous_write_count} within {timeout:?}: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

fn recovery_path_from_harness(driver: &mut UiDriver) -> anyhow::Result<PathBuf> {
    let state = driver.command("draft_list_state", json!({}))?;
    state["recovery_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("draft state had no recovery path: {state}"))
}

fn recovery_body(path: &Path) -> anyhow::Result<String> {
    let value: Value = serde_json::from_slice(
        &fs::read(path).with_context(|| format!("reading recovery draft {}", path.display()))?,
    )?;
    value["body"]
        .as_str()
        .map(ToOwned::to_owned)
        .with_context(|| format!("recovery draft had no body: {value}"))
}

fn regular_file_count(path: &Path) -> anyhow::Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    Ok(fs::read_dir(path)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .count())
}

fn wait_for_named_draft_io_idle(driver: &mut UiDriver, timeout: Duration) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = driver.command("draft_io_status", json!({}))?;
        if status["list_busy"] == false {
            return Ok(status);
        }
        ensure!(
            Instant::now() < deadline,
            "named-draft I/O did not become idle within {timeout:?}: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

fn wait_for_named_draft_generation(
    driver: &mut UiDriver,
    generation: u64,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = driver.command("draft_io_status", json!({}))?;
        if status["list_busy"] == false
            && status["list_completed_generation"].as_u64() == Some(generation)
        {
            return Ok(status);
        }
        ensure!(
            Instant::now() < deadline,
            "named-draft generation {generation} did not complete within {timeout:?}: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn prepare_app_work_dir_for_restart(
    app: &mut FixtureApp,
    terminate_abruptly: bool,
) -> anyhow::Result<()> {
    if app.child.try_wait()?.is_none() {
        ensure!(
            terminate_abruptly,
            "application was still running before a graceful restart"
        );
        app.child.kill()?;
        let status = app.child.wait()?;
        ensure!(
            !status.success(),
            "abruptly terminated application exited successfully: {status}"
        );
    }
    drop(app.display.take());
    for path in [&app.socket_path, &app.log_path] {
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("removing restart artifact {}", path.display()))?;
        }
    }
    let display_dir = app.work_dir.join("gui-display");
    if display_dir.exists() {
        fs::remove_dir_all(&display_dir)
            .with_context(|| format!("removing restart display {}", display_dir.display()))?;
    }
    Ok(())
}

#[test]
fn fixture_high_rate_draft_autosave_is_debounced_and_keeps_gtk_responsive() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_high_rate_draft_autosave_is_debounced_and_keeps_gtk_responsive: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running high-rate draft autosave UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-draft-debounce-ui-{run_id}"));
    let token = format!("notm-draft-debounce-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    let recovery_path = recovery_path_from_harness(&mut driver)?;

    let before_baseline = draft_write_count(&draft_autosave_status(&mut driver)?)?;
    for (command, value) in [
        ("compose_set_from", "Fixture User <fixture@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Debounced recovery draft"),
        ("compose_set_body", "baseline body"),
    ] {
        assert_eq!(
            driver.command(command, json!({"value": value}))?["ok"],
            true
        );
    }
    wait_for_draft_write_after(&mut driver, before_baseline, Duration::from_secs(3))?;
    let delayed = driver.command("set_fixture_draft_delay", json!({"milliseconds": 1200}))?;
    assert_eq!(
        delayed["ok"], true,
        "could not delay draft worker: {delayed}"
    );
    let before_burst = draft_write_count(&draft_autosave_status(&mut driver)?)?;

    let mut final_body = String::new();
    for index in 1..=24 {
        final_body = format!("continuous edit {index:02} {}", "x".repeat(index));
        let edited = driver.command("compose_set_body", json!({"value": final_body}))?;
        assert_eq!(
            edited["ok"], true,
            "continuous edit {index} failed: {edited}"
        );
    }
    let immediately_after = draft_autosave_status(&mut driver)?;
    assert_eq!(
        draft_write_count(&immediately_after)?,
        before_burst,
        "high-rate edits escaped the debounce window: {immediately_after}"
    );

    let active = wait_for_draft_worker_after(&mut driver, before_burst, Duration::from_secs(2))?;
    assert_eq!(
        draft_write_count(&active)?,
        before_burst + 1,
        "debounced burst launched more than one worker: {active}"
    );
    let first_health_started = Instant::now();
    let first_health = driver.command("health", json!({}))?;
    let first_health_elapsed = first_health_started.elapsed();
    thread::sleep(Duration::from_millis(150));
    let second_health_started = Instant::now();
    let second_health = driver.command("health", json!({}))?;
    let second_health_elapsed = second_health_started.elapsed();
    ensure!(
        first_health_elapsed < Duration::from_millis(500)
            && second_health_elapsed < Duration::from_millis(500),
        "health blocked behind delayed draft I/O: first={first_health_elapsed:?}, second={second_health_elapsed:?}"
    );
    ensure!(
        second_health["gtk_heartbeat"].as_u64().unwrap_or(0)
            > first_health["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat did not advance during delayed draft I/O: first={first_health}, second={second_health}"
    );

    let completed = wait_for_draft_write_after(&mut driver, before_burst, Duration::from_secs(4))?;
    assert_eq!(
        draft_write_count(&completed)?,
        before_burst + 1,
        "high-rate edits produced multiple recovery writes: {completed}"
    );
    assert_eq!(recovery_body(&recovery_path)?, final_body);
    ensure!(
        fs::read_dir(recovery_path.parent().expect("recovery parent"))?
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")),
        "atomic autosave left a temporary file beside {}",
        recovery_path.display()
    );
    Ok(())
}

#[test]
fn fixture_explicit_draft_save_keeps_gtk_responsive_and_preserves_newer_edits() -> anyhow::Result<()>
{
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_explicit_draft_save_keeps_gtk_responsive_and_preserves_newer_edits: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running async explicit draft-save UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-explicit-draft-save-ui-{run_id}"));
    let token = format!("notm-explicit-draft-save-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut save_driver = app.connect(&token)?;
    let mut observer = app.connect(&token)?;
    save_driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(save_driver.command("open_compose", json!({}))?["ok"], true);
    for (command, value) in [
        ("compose_set_from", "Fixture User <fixture@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Slow explicit save"),
        ("compose_set_body", "captured before slow save"),
    ] {
        assert_eq!(
            save_driver.command(command, json!({"value": value}))?["ok"],
            true
        );
    }
    let recovery_path = recovery_path_from_harness(&mut observer)?;
    assert_eq!(
        observer.command("set_fixture_draft_delay", json!({"milliseconds": 600}))?["ok"],
        true
    );

    let save = thread::spawn(move || save_driver.command("save_draft", json!({})));
    thread::sleep(Duration::from_millis(125));
    let before = observer.command("health", json!({}))?;
    assert_eq!(
        before["draft_save"]["busy"], true,
        "explicit save did not enter worker-backed persistence: {before}"
    );
    let edited = observer.command(
        "compose_set_body",
        json!({"value": "newer edit kept while explicit save runs"}),
    )?;
    assert_eq!(
        edited["ok"], true,
        "typing was blocked by draft I/O: {edited}"
    );
    thread::sleep(Duration::from_millis(175));
    let after = observer.command("health", json!({}))?;
    ensure!(
        after["gtk_heartbeat"].as_u64().unwrap_or(0)
            > before["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat stopped during explicit draft I/O: before={before}, after={after}"
    );

    let saved = save
        .join()
        .map_err(|_| anyhow::anyhow!("explicit draft-save driver panicked"))??;
    assert_eq!(saved["ok"], true, "explicit draft save failed: {saved}");
    let saved_path = saved["report"]["local_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("saved draft had no local path: {saved}"))?;
    ensure!(saved_path.is_file(), "saved draft file is missing");
    let saved_fields: Value = serde_json::from_slice(&fs::read(&saved_path)?)?;
    assert_eq!(saved_fields["body"], "captured before slow save");
    let recovery: Value = serde_json::from_slice(&fs::read(&recovery_path)?)?;
    assert_eq!(recovery["body"], "newer edit kept while explicit save runs");
    let status = observer.command("draft_io_status", json!({}))?;
    assert_eq!(
        status["save_busy"], false,
        "draft save stayed busy: {status}"
    );
    Ok(())
}

#[test]
fn fixture_draft_autosave_failure_preserves_last_good_and_retries() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_draft_autosave_failure_preserves_last_good_and_retries: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running draft autosave failure/retry UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-draft-failure-ui-{run_id}"));
    let token = format!("notm-draft-failure-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    let recovery_path = recovery_path_from_harness(&mut driver)?;
    let before_good = draft_write_count(&draft_autosave_status(&mut driver)?)?;
    assert_eq!(
        driver.command("compose_set_body", json!({"value": "last good body"}))?["ok"],
        true
    );
    let good = wait_for_draft_write_after(&mut driver, before_good, Duration::from_secs(3))?;
    let good_count = draft_write_count(&good)?;
    let good_bytes = fs::read(&recovery_path)?;

    assert_eq!(
        driver.command("fail_next_draft_write", json!({}))?["ok"],
        true
    );
    assert_eq!(
        driver.command(
            "compose_set_body",
            json!({"value": "failed replacement body"})
        )?["ok"],
        true
    );
    let failed = wait_for_draft_write_after(&mut driver, good_count, Duration::from_secs(3))?;
    ensure!(
        failed["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("injected draft write failure")),
        "injected write failure was not visible: {failed}"
    );
    assert_eq!(
        fs::read(&recovery_path)?,
        good_bytes,
        "failed atomic replace damaged the last good recovery draft"
    );

    let failed_count = draft_write_count(&failed)?;
    assert_eq!(
        driver.command(
            "compose_set_body",
            json!({"value": "successful retry body"})
        )?["ok"],
        true
    );
    let recovered = wait_for_draft_write_after(&mut driver, failed_count, Duration::from_secs(3))?;
    assert_eq!(
        recovered["last_error"],
        Value::Null,
        "successful retry left a stale autosave error: {recovered}"
    );
    assert_eq!(recovery_body(&recovery_path)?, "successful retry body");
    Ok(())
}

#[cfg(unix)]
#[test]
fn immediate_close_flush_survives_normal_and_abrupt_restart() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP immediate_close_flush_survives_normal_and_abrupt_restart: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running close/crash draft recovery UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-draft-boundary-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:notm-autosave-empty\"\n\
             \n[identity]\nname = \"Fixture Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n\
             \n[automation]\nallow_live_send_test = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
        ),
    )?;
    let recovery_path = work_dir.join("state/notm/draft.json");

    let token = format!("notm-draft-boundary-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir.clone(), &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    for (command, value) in [
        ("compose_set_from", "Fixture Sender <sender@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Immediate close recovery"),
        ("compose_set_body", "body flushed at close boundary"),
    ] {
        assert_eq!(
            driver.command(command, json!({"value": value}))?["ok"],
            true
        );
    }
    assert_eq!(driver.command("close_main_window", json!({}))?["ok"], true);
    let confirmation_id = pending_confirmation_id(&mut driver, "close_main_window")?;
    let accepted = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": confirmation_id}),
    )?;
    assert_eq!(
        accepted["ok"], true,
        "close confirmation failed: {accepted}"
    );
    drop(driver);
    let status = app.wait_for_exit(Duration::from_secs(5))?;
    ensure!(
        status.success(),
        "application did not exit after close flush: {status}\n{}",
        app.logs()
    );
    assert_eq!(
        recovery_body(&recovery_path)?,
        "body flushed at close boundary"
    );

    prepare_app_work_dir_for_restart(&mut app, false)?;
    let restart_token = format!("notm-draft-boundary-restart-ui-{run_id}");
    let mut restarted =
        FixtureApp::spawn_with_config(work_dir.clone(), &restart_token, &config_path)?;
    let mut restarted_driver = restarted.connect(&restart_token)?;
    restarted_driver.wait_for_search(STARTUP_TIMEOUT)?;
    let recovered = restarted_driver.command("app_state", json!({}))?;
    assert_eq!(
        recovered["state"]["compose_fields"]["body"], "body flushed at close boundary",
        "normal restart did not recover the close-boundary draft: {recovered}"
    );

    let before_crash = draft_write_count(&draft_autosave_status(&mut restarted_driver)?)?;
    assert_eq!(
        restarted_driver.command(
            "compose_set_body",
            json!({"value": "body preserved across abrupt termination"}),
        )?["ok"],
        true
    );
    wait_for_draft_write_after(&mut restarted_driver, before_crash, Duration::from_secs(3))?;
    assert_eq!(
        recovery_body(&recovery_path)?,
        "body preserved across abrupt termination"
    );
    drop(restarted_driver);
    prepare_app_work_dir_for_restart(&mut restarted, true)?;

    let crash_restart_token = format!("notm-draft-boundary-crash-ui-{run_id}");
    let mut crash_restarted =
        FixtureApp::spawn_with_config(work_dir, &crash_restart_token, &config_path)?;
    let mut crash_restarted_driver = crash_restarted.connect(&crash_restart_token)?;
    crash_restarted_driver.wait_for_search(STARTUP_TIMEOUT)?;
    let crash_recovered = crash_restarted_driver.command("app_state", json!({}))?;
    assert_eq!(
        crash_recovered["state"]["compose_fields"]["body"],
        "body preserved across abrupt termination",
        "abrupt restart did not recover the last completed atomic draft: {crash_recovered}"
    );
    Ok(())
}

#[test]
fn fixture_send_waits_for_draft_flush_and_aborts_on_flush_failure() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_send_waits_for_draft_flush_and_aborts_on_flush_failure: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running send/draft-flush ordering UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-send-draft-flush-ui-{run_id}"));
    let token = format!("notm-send-draft-flush-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    let recovery_path = recovery_path_from_harness(&mut driver)?;
    let capture_dir = recovery_path
        .parent()
        .expect("fixture recovery parent")
        .join("captured-send");
    assert_eq!(
        driver.command("set_fixture_draft_delay", json!({"milliseconds": 900}))?["ok"],
        true
    );
    for (command, value) in [
        ("compose_set_from", "Fixture User <fixture@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Send waits for recovery flush"),
        ("compose_set_body", "successful send flush body"),
    ] {
        assert_eq!(
            driver.command(command, json!({"value": value}))?["ok"],
            true
        );
    }

    let before_send_write_count = draft_write_count(&draft_autosave_status(&mut driver)?)?;
    let started = driver.command("compose_send", json!({}))?;
    assert_eq!(started["ok"], true, "fixture send did not start: {started}");
    assert_eq!(
        started["pending"], true,
        "send did not report pending: {started}"
    );
    thread::sleep(Duration::from_millis(150));
    let during_flush = driver.command("health", json!({}))?;
    assert_eq!(
        during_flush["ok"], true,
        "GTK blocked during send flush: {during_flush}"
    );
    assert_eq!(
        regular_file_count(&capture_dir)?,
        0,
        "fixture transport ran before the delayed draft flush completed"
    );
    let sent_generation = started["state"]["compose_generation"]
        .as_u64()
        .with_context(|| format!("send start had no composer generation: {started}"))?;
    let finalizing_deadline = Instant::now() + Duration::from_secs(3);
    let (finalizing, finalizing_autosave) = loop {
        let state = driver.command("draft_list_state", json!({}))?;
        let autosave = draft_autosave_status(&mut driver)?;
        if state["status_text"] == "Finalizing accepted send…"
            && state["compose_fields"]["subject"] == "Send waits for recovery flush"
            && autosave["busy"] == true
            && autosave["pending_generation"] == sent_generation
            && draft_write_count(&autosave)? >= before_send_write_count.saturating_add(2)
        {
            break (state, autosave);
        }
        ensure!(
            Instant::now() < finalizing_deadline,
            "send did not reach the active accepted-send recovery clear: state={state}, autosave={autosave}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    assert_eq!(
        regular_file_count(&capture_dir)?,
        1,
        "accepted-send recovery clear started before the fixture transport completed"
    );
    let mut last_edit = Value::Null;
    for (command, value) in [
        ("compose_set_subject", "Newer subject during final clear"),
        ("compose_set_body", "newer body during final clear"),
    ] {
        let edited = driver.command(command, json!({"value": value}))?;
        assert_eq!(
            edited["ok"], true,
            "{command} was blocked during accepted-send finalization: {edited}"
        );
        last_edit = edited;
    }
    assert_eq!(
        last_edit["compose_fields"]["subject"], "Newer subject during final clear",
        "accepted-send finalization did not retain the live subject edit: {last_edit}"
    );
    assert_eq!(
        last_edit["compose_fields"]["body"], "newer body during final clear",
        "accepted-send finalization did not retain the live body edit: {last_edit}"
    );
    let edited_state = driver.command("app_state", json!({}))?;
    ensure!(
        edited_state["state"]["compose_generation"]
            .as_u64()
            .is_some_and(|generation| generation > sent_generation),
        "composer edits did not supersede accepted-send generation {sent_generation}: {edited_state}"
    );
    thread::sleep(Duration::from_millis(150));
    let finalizing_health = driver.command("health", json!({}))?;
    ensure!(
        finalizing_health["gtk_heartbeat"].as_u64().unwrap_or(0)
            > during_flush["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat stopped before/during accepted-send finalization: before={during_flush}, finalizing={finalizing}, autosave={finalizing_autosave}, after={finalizing_health}"
    );
    let sent = driver.wait_for_send(Duration::from_secs(4))?;
    assert_eq!(
        sent["state"]["last_send_report"]["accepted"], true,
        "send was not accepted after draft flush: {sent}"
    );
    assert_eq!(
        sent["state"]["compose_fields"]["subject"], "Newer subject during final clear",
        "accepted-send final clear discarded the newer subject: {sent}"
    );
    assert_eq!(
        sent["state"]["compose_fields"]["body"], "newer body during final clear",
        "accepted-send final clear discarded the newer body: {sent}"
    );
    let recovery: Value = serde_json::from_slice(&fs::read(&recovery_path)?)?;
    assert_eq!(recovery["subject"], "Newer subject during final clear");
    assert_eq!(recovery["body"], "newer body during final clear");
    assert_eq!(regular_file_count(&capture_dir)?, 1);

    for (command, value) in [
        ("compose_set_from", "Fixture User <fixture@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Failed flush must abort send"),
        ("compose_set_body", "failed send flush body"),
    ] {
        assert_eq!(
            driver.command(command, json!({"value": value}))?["ok"],
            true
        );
    }
    assert_eq!(
        driver.command("fail_next_draft_write", json!({}))?["ok"],
        true
    );
    let failed_start = driver.command("compose_send", json!({}))?;
    assert_eq!(
        failed_start["ok"], true,
        "failed-flush send did not enter the pending state: {failed_start}"
    );
    let failed = driver.wait_for_send(Duration::from_secs(4))?;
    assert_eq!(
        failed["state"]["last_send_report"],
        Value::Null,
        "transport produced a report after draft flush failure: {failed}"
    );
    ensure!(
        failed["state"]["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("draft flush failed")
                && error.contains("injected draft write failure")),
        "draft flush failure was not visible: {failed}"
    );
    assert_eq!(
        failed["state"]["compose_fields"]["body"], "failed send flush body",
        "failed flush cleared the composer: {failed}"
    );
    assert_eq!(
        regular_file_count(&capture_dir)?,
        1,
        "transport ran despite the failed draft flush"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn fixture_named_draft_corruption_keeps_valid_rows_and_reports_warning() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_named_draft_corruption_keeps_valid_rows_and_reports_warning: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running named-draft corruption UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-draft-corruption-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let token = format!("notm-draft-corruption-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir.clone(), &token)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;

    let initial = driver.command("draft_list_state", json!({}))?;
    let drafts_dir = initial["drafts_dir"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("fixture exposed no named-draft directory: {initial}"))?;
    fs::create_dir_all(&drafts_dir)?;
    fs::write(
        drafts_dir.join("valid.json"),
        serde_json::to_vec_pretty(&json!({
            "from": "",
            "to": "recipient@example.test",
            "cc": "",
            "bcc": "",
            "subject": "Valid draft survives corruption",
            "body": "valid body",
            "attachments": [],
            "in_reply_to": null,
            "references": [],
            "text_reply_quote": null,
            "html_reply_quote": null,
        }))?,
    )?;
    fs::write(drafts_dir.join("malformed.json"), b"{truncated")?;
    fs::create_dir(drafts_dir.join("unreadable.json"))?;

    let requested = driver.command("refresh_named_drafts", json!({}))?;
    assert_eq!(
        requested["ok"], true,
        "refresh was not scheduled: {requested}"
    );
    let generation = requested["generation"]
        .as_u64()
        .with_context(|| format!("refresh had no generation: {requested}"))?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let completed = loop {
        let status = driver.command("draft_io_status", json!({}))?;
        if status["list_busy"] == false
            && status["list_completed_generation"].as_u64() == Some(generation)
        {
            break status;
        }
        ensure!(
            Instant::now() < deadline,
            "named-draft refresh did not complete: {status}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    let warning = completed["last_error"]
        .as_str()
        .with_context(|| format!("rejected entries did not produce a warning: {completed}"))?;
    ensure!(
        warning.contains("Named draft refresh warning:")
            && warning.contains("rejected 2")
            && warning.contains("malformed.json")
            && warning.contains("unreadable.json"),
        "rejected-entry warning was not useful: {completed}"
    );

    let drafts = driver.command("list_drafts", json!({}))?;
    let entries = json_array_at(&drafts, &["drafts"])?;
    ensure!(
        entries.len() == 1 && entries[0]["fields"]["subject"] == "Valid draft survives corruption",
        "valid named draft was lost when neighbors were rejected: {drafts}"
    );
    let ui = driver.command("draft_list_state", json!({}))?;
    let rows = json_array_at(&ui, &["list", "rows"])?;
    ensure!(
        rows.len() == 1
            && rows[0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("Valid draft survives corruption")),
        "GTK draft rows were not replaced with the valid subset: {ui}"
    );
    assert_eq!(ui["last_error"], warning);
    ensure!(
        ui["status_text"]
            .as_str()
            .is_some_and(|status| status.contains("Named draft refresh warning:")),
        "partial refresh warning was not visible in the status UI: {ui}"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn fixture_legacy_draft_migration_serializes_mutations_and_keeps_gtk_responsive()
-> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_legacy_draft_migration_serializes_mutations_and_keeps_gtk_responsive: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running legacy draft migration serialization UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-draft-migration-ui-{run_id}"));
    let legacy_dir = work_dir.join("legacy-drafts");
    let token = format!("notm-draft-migration-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    wait_for_named_draft_io_idle(&mut driver, STARTUP_TIMEOUT)?;

    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    for (command, value) in [
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Migration serialization sentinel"),
        ("compose_set_body", "saved before legacy migration"),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }
    let saved = driver.command("save_draft", json!({}))?;
    assert_eq!(
        saved["ok"], true,
        "initial named-draft save failed: {saved}"
    );
    let active_path = saved["report"]["local_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("initial save had no local path: {saved}"))?;
    wait_for_named_draft_io_idle(&mut driver, STARTUP_TIMEOUT)?;

    let paths = driver.command("draft_list_state", json!({}))?;
    let drafts_dir = paths["drafts_dir"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("fixture exposed no current draft directory: {paths}"))?;
    fs::create_dir_all(&drafts_dir)?;
    fs::create_dir_all(&legacy_dir)?;
    assert_eq!(regular_file_count(&drafts_dir)?, 1);

    let filler = serde_json::to_vec_pretty(&json!({
        "from": "Fixture User <fixture@example.test>",
        "to": "recipient@example.test",
        "cc": "",
        "bcc": "",
        "subject": "Pre-existing capacity draft",
        "body": "bounded migration fixture",
        "attachments": [],
        "in_reply_to": null,
        "references": [],
        "text_reply_quote": null,
        "html_reply_quote": null,
    }))?;
    for index in 0..254 {
        fs::write(
            drafts_dir.join(format!("pre-existing-{index:03}.json")),
            &filler,
        )?;
    }
    let legacy_path = legacy_dir.join("legacy-newcomer.json");
    fs::write(
        &legacy_path,
        serde_json::to_vec_pretty(&json!({
            "from": "Fixture User <fixture@example.test>",
            "to": "recipient@example.test",
            "cc": "",
            "bcc": "",
            "subject": "Legacy capacity draft",
            "body": "must migrate without racing a 257th write",
            "attachments": [],
            "in_reply_to": null,
            "references": [],
            "text_reply_quote": null,
            "html_reply_quote": null,
        }))?,
    )?;
    assert_eq!(regular_file_count(&drafts_dir)?, 255);
    assert_eq!(regular_file_count(&legacy_dir)?, 1);

    assert_eq!(
        driver.command("set_fixture_draft_delay", json!({"milliseconds": 1200}))?["ok"],
        true
    );
    let requested = driver.command(
        "refresh_named_drafts",
        json!({"migrate_legacy": true, "legacy_dir": legacy_dir}),
    )?;
    assert_eq!(
        requested["ok"], true,
        "migration was not scheduled: {requested}"
    );
    let generation = requested["generation"]
        .as_u64()
        .with_context(|| format!("migration had no generation: {requested}"))?;
    let active = driver.command("draft_io_status", json!({}))?;
    assert_eq!(
        active["migration_busy"], true,
        "migration was not exclusive: {active}"
    );
    assert_eq!(active["list_generation"].as_u64(), Some(generation));

    let before_health = driver.command("health", json!({}))?;
    let edited = driver.command(
        "compose_set_body",
        json!({"value": "composer edit while migration is delayed"}),
    )?;
    assert_eq!(
        edited["ok"], true,
        "migration blocked composer editing: {edited}"
    );
    for (command, args) in [
        ("save_draft", json!({})),
        ("delete_active_draft", json!({})),
        ("compose_send", json!({})),
    ] {
        let blocked = driver.command(command, args)?;
        assert_eq!(blocked["ok"], false, "{command} raced migration: {blocked}");
        ensure!(
            blocked["error"]
                .as_str()
                .is_some_and(|error| error.contains("legacy drafts are migrating")),
            "{command} did not explain the migration conflict: {blocked}"
        );
    }
    let overlapping = driver.command("refresh_named_drafts", json!({}))?;
    assert_eq!(
        overlapping["ok"], false,
        "refresh superseded a mutating migration: {overlapping}"
    );
    assert_eq!(overlapping["generation"].as_u64(), Some(generation));
    assert_eq!(regular_file_count(&drafts_dir)?, 255);
    assert_eq!(regular_file_count(&legacy_dir)?, 1);

    thread::sleep(Duration::from_millis(175));
    let after_health = driver.command("health", json!({}))?;
    ensure!(
        after_health["gtk_heartbeat"].as_u64().unwrap_or(0)
            > before_health["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat stopped during delayed migration: before={before_health}, after={after_health}"
    );
    let completed =
        wait_for_named_draft_generation(&mut driver, generation, Duration::from_secs(5))?;
    assert_eq!(completed["migration_busy"], false);
    assert_eq!(
        completed["last_error"],
        Value::Null,
        "migration failed: {completed}"
    );
    assert_eq!(regular_file_count(&drafts_dir)?, 256);
    assert_eq!(regular_file_count(&legacy_dir)?, 0);
    ensure!(
        !legacy_path.exists(),
        "migrated legacy source was not removed"
    );

    assert_eq!(
        driver.command("set_fixture_draft_delay", json!({"milliseconds": 0}))?["ok"],
        true
    );
    let delete = driver.command("delete_active_draft", json!({}))?;
    assert_eq!(
        delete["ok"], true,
        "delete stayed blocked after migration: {delete}"
    );
    assert_eq!(delete["pending_confirmation"], true);
    let delete_id = pending_confirmation_id(&mut driver, "delete_active_draft")?;
    let deleted = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": delete_id}),
    )?;
    assert_eq!(
        deleted["ok"], true,
        "post-migration delete failed: {deleted}"
    );
    wait_for_named_draft_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert!(!active_path.exists());
    assert_eq!(regular_file_count(&drafts_dir)?, 255);

    for (command, value) in [
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Post-migration save sentinel"),
        ("compose_set_body", "save after migration gate released"),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }
    let resaved = driver.command("save_draft", json!({}))?;
    assert_eq!(resaved["ok"], true, "post-migration save failed: {resaved}");
    wait_for_named_draft_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(regular_file_count(&drafts_dir)?, 256);

    let overflow_legacy = legacy_dir.join("overflow-newcomer.json");
    fs::write(&overflow_legacy, &filler)?;
    assert_eq!(
        driver.command("set_fixture_draft_delay", json!({"milliseconds": 350}))?["ok"],
        true
    );
    let rejected_request = driver.command(
        "refresh_named_drafts",
        json!({"migrate_legacy": true, "legacy_dir": legacy_dir}),
    )?;
    assert_eq!(rejected_request["ok"], true);
    let rejected_generation = rejected_request["generation"]
        .as_u64()
        .with_context(|| format!("failing migration had no generation: {rejected_request}"))?;
    let blocked = driver.command("save_draft", json!({}))?;
    assert_eq!(
        blocked["ok"], false,
        "save raced failing migration: {blocked}"
    );
    let rejected =
        wait_for_named_draft_generation(&mut driver, rejected_generation, Duration::from_secs(5))?;
    assert_eq!(rejected["migration_busy"], false);
    ensure!(
        rejected["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("would contain 257 JSON files")),
        "fatal migration policy error was not visible: {rejected}"
    );
    assert_eq!(regular_file_count(&drafts_dir)?, 256);
    assert_eq!(regular_file_count(&legacy_dir)?, 1);

    assert_eq!(
        driver.command("set_fixture_draft_delay", json!({"milliseconds": 0}))?["ok"],
        true
    );
    let final_delete = driver.command("delete_active_draft", json!({}))?;
    assert_eq!(
        final_delete["pending_confirmation"], true,
        "fatal migration left draft deletion blocked: {final_delete}"
    );
    let final_delete_id = pending_confirmation_id(&mut driver, "delete_active_draft")?;
    let final_deleted = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": final_delete_id}),
    )?;
    assert_eq!(
        final_deleted["ok"], true,
        "delete after migration error failed: {final_deleted}"
    );
    wait_for_named_draft_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(regular_file_count(&drafts_dir)?, 255);
    assert_eq!(regular_file_count(&legacy_dir)?, 1);

    Ok(())
}

#[cfg(unix)]
#[test]
fn fixture_saved_drafts_are_visible_activatable_and_delete_safely() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_saved_drafts_are_visible_activatable_and_delete_safely: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running saved-draft list desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-saved-draft-list-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        "[ui]\nlayout = \"columns\"\nstart_maximized = true\nshow_sidebar = false\n\
         show_message_list = false\nshow_message_view = true\n",
    )?;
    let token = format!("notm-saved-draft-list-ui-{run_id}");
    let mut app = FixtureApp::spawn_fixture_with_config(work_dir.clone(), &token, &config_path)?;
    let mut driver = app.connect(&token)?;

    driver.wait_for_search(STARTUP_TIMEOUT)?;
    let selection_deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let startup = driver.command("app_state", json!({}))?;
        let selection = driver.command("thread_selection_view_state", json!({}))?;
        let selection_settled = !startup["state"]["selected_thread"].is_null()
            && startup["state"]["last_operation"]
                .as_str()
                .is_some_and(|operation| operation.starts_with("previewed thread "))
            && selection["selected_local"].as_u64() == Some(0);
        if selection_settled {
            break;
        }
        ensure!(
            Instant::now() < selection_deadline,
            "startup thread selection did not settle: state={startup}, selection={selection}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    let compose_deadline = Instant::now() + STARTUP_TIMEOUT;
    let empty = loop {
        let state = driver.command("draft_list_state", json!({}))?;
        if state["section"]["mapped"] == true {
            break state;
        }
        ensure!(
            Instant::now() < compose_deadline,
            "composer draft section did not map: {state}\n{}",
            app.logs()
        );
        let reopened = driver.command("open_compose", json!({}))?;
        assert_eq!(
            reopened["ok"], true,
            "could not reassert the composer after startup selection: {reopened}"
        );
        assert_eq!(
            reopened["pending_confirmation"], false,
            "blank startup composer unexpectedly required confirmation: {reopened}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    assert_eq!(
        empty["section"]["mapped"], true,
        "section was not rendered: {empty}"
    );
    assert_eq!(
        empty["empty_state"]["text"], "No saved drafts",
        "empty-state label was not explicit: {empty}"
    );
    assert_eq!(
        empty["empty_state"]["mapped"], true,
        "empty-state label was not rendered: {empty}"
    );
    assert_eq!(
        empty["scroller"]["visible"], false,
        "empty list scrolled: {empty}"
    );
    assert_eq!(
        empty["delete_button"]["label"], "Delete selected draft",
        "draft delete action was not clearly labeled: {empty}"
    );
    assert_eq!(
        empty["delete_button"]["mapped"], true,
        "draft delete action was not rendered: {empty}"
    );
    ensure!(
        json_array_at(&empty, &["list", "rows"])?.is_empty(),
        "empty composer exposed draft rows: {empty}"
    );

    for (command, value) in [
        ("compose_set_to", "saved@example.test"),
        ("compose_set_subject", "Visible named draft"),
        (
            "compose_set_body",
            "This body must be restored by row activation.",
        ),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }
    let saved = driver.command("save_draft", json!({}))?;
    assert_eq!(saved["ok"], true, "draft save failed: {saved}");
    let saved_path = saved["report"]["local_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("saved draft had no local path: {saved}"))?;
    ensure!(saved_path.is_file(), "saved draft file is missing");

    let visible = driver.command("draft_list_state", json!({}))?;
    assert_eq!(
        visible["empty_state"]["visible"], false,
        "empty state remained visible beside a draft: {visible}"
    );
    assert_eq!(
        visible["scroller"]["mapped"], true,
        "saved-draft scroller was not rendered: {visible}"
    );
    assert_eq!(
        visible["list"]["mapped"], true,
        "draft list was not rendered: {visible}"
    );
    assert_eq!(visible["scroller"]["min_content_height"], 72);
    assert_eq!(visible["scroller"]["max_content_height"], 160);
    let rows = json_array_at(&visible, &["list", "rows"])?;
    ensure!(
        rows.len() == 1
            && rows[0]["mapped"] == true
            && rows[0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("Visible named draft")),
        "saved draft row was not visibly populated: {visible}"
    );

    let cleared = driver.command("clear_draft", json!({}))?;
    assert_eq!(
        cleared["ok"], true,
        "closing active draft failed: {cleared}"
    );
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    let activated = driver.command("activate_draft_by_index", json!({"index": 0}))?;
    assert_eq!(activated["ok"], true, "row activation failed: {activated}");
    assert_eq!(activated["list"]["selected_index"], 0);
    assert_eq!(
        activated["compose_fields"]["subject"], "Visible named draft",
        "row activation did not load the saved subject: {activated}"
    );
    assert_eq!(
        activated["compose_fields"]["body"], "This body must be restored by row activation.",
        "row activation did not load the saved body: {activated}"
    );
    assert_eq!(
        activated["active_draft"]["path"],
        saved_path.display().to_string(),
        "activated row did not become the active draft: {activated}"
    );
    assert_eq!(
        activated["delete_button"]["sensitive"], true,
        "selected-draft delete action was not enabled: {activated}"
    );

    let saved_bytes = fs::read(&saved_path)?;
    let recovery_path = activated["recovery_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("draft-list state had no recovery path: {activated}"))?;
    let recovery_bytes = read_optional_file(&recovery_path)?;
    let draft_dir = saved_path.parent().context("saved draft parent")?;
    fs::set_permissions(draft_dir, fs::Permissions::from_mode(0o555))?;
    let requested = driver.command("click_delete_selected_draft", json!({}))?;
    assert_eq!(
        requested["ok"], true,
        "draft delete confirmation was not requested: {requested}"
    );
    assert_eq!(requested["deleted"], false);
    assert_eq!(requested["pending_confirmation"], true);
    let pending = driver.command("pending_confirmation", json!({}))?;
    assert_eq!(pending["pending"]["kind"], "delete_named_draft");
    assert_eq!(pending["pending"]["visible"], true);
    let failed = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": pending["pending"]["id"]}),
    );
    fs::set_permissions(draft_dir, fs::Permissions::from_mode(0o755))?;
    let failed = failed?;
    assert_eq!(
        failed["ok"], false,
        "read-only draft deletion succeeded: {failed}"
    );
    ensure!(
        failed["last_error"]
            .as_str()
            .is_some_and(|error| error.starts_with("Saved draft delete failed:")),
        "failed persistence was not reported: {failed}"
    );
    assert_eq!(
        failed["active_draft"]["path"],
        saved_path.display().to_string(),
        "failed deletion cleared the active draft: {failed}"
    );
    ensure!(
        fs::read(&saved_path)? == saved_bytes
            && read_optional_file(&recovery_path)? == recovery_bytes,
        "failed deletion changed persisted draft or recovery bytes"
    );

    let retried = driver.command("click_delete_selected_draft", json!({}))?;
    assert_eq!(
        retried["pending_confirmation"], true,
        "draft delete retry did not request confirmation: {retried}"
    );
    let deleted = driver.command("respond_confirmation", json!({"response": "accept"}))?;
    assert_eq!(deleted["ok"], true, "draft delete retry failed: {deleted}");
    let deleted = driver.command("draft_list_state", json!({}))?;
    assert_eq!(
        deleted["active_draft"],
        Value::Null,
        "successful deletion left the deleted draft active: {deleted}"
    );
    assert_eq!(
        json_array_at(&deleted, &["list", "rows"])?.len(),
        0,
        "draft row survived confirmed deletion: {deleted}"
    );
    assert_eq!(
        deleted["compose_fields"]["subject"], "Visible named draft",
        "successful deletion unexpectedly cleared composer fields: {deleted}"
    );
    assert_eq!(
        deleted["empty_state"]["mapped"], true,
        "empty state did not return after deletion: {deleted}"
    );
    assert_eq!(deleted["scroller"]["visible"], false);
    assert_eq!(deleted["delete_button"]["sensitive"], false);
    assert_eq!(deleted["last_error"], Value::Null);
    ensure!(
        !saved_path.exists(),
        "successful delete left {saved_path:?}"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn indexed_maildir_draft_refresh_stays_clean_during_message_navigation() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP indexed_maildir_draft_refresh_stays_clean_during_message_navigation: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running indexed Maildir draft refresh UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-indexed-draft-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let draft_maildir = fixture.root.join("Drafts");
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
             \n[identity]\nname = \"Fixture Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[drafts]\nsave_maildir = true\nmaildir = {}\ntags = [\"draft\"]\nindex_after_save = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml_path(&draft_maildir),
        ),
    )?;

    let token = format!("notm-indexed-draft-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir, &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    let startup = driver.wait_for_search(STARTUP_TIMEOUT)?;
    let initial_generation = startup["state"]["search_generation"]
        .as_u64()
        .with_context(|| format!("startup state had no search generation: {startup}"))?;
    ensure!(
        json_array_at(&startup, &["state", "thread_list_items"])?.len() >= 2,
        "indexed-draft navigation smoke needs at least two inbox threads: {startup}"
    );
    let initial_selection = driver.command("select_thread_by_index", json!({"index": 0}))?;
    assert_eq!(
        initial_selection["ok"], true,
        "initial thread selection failed: {initial_selection}"
    );
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;

    let opened = driver.command("open_compose", json!({}))?;
    assert_eq!(opened["ok"], true, "composer did not open: {opened}");
    for (command, value) in [
        ("compose_set_from", "Fixture Sender <sender@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Indexed draft refresh regression"),
        (
            "compose_set_body",
            "Saving and indexing this draft must leave it clean and attached.",
        ),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }

    let saved = driver.command("save_draft", json!({}))?;
    assert_eq!(
        saved["ok"], true,
        "first indexed draft save failed: {saved}"
    );
    let saved_path = saved["report"]["maildir_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("indexed draft save had no Maildir path: {saved}"))?;
    let saved_message_id = saved["report"]["indexed_message_id"]
        .as_str()
        .with_context(|| format!("draft save did not report an indexed Message-ID: {saved}"))?;
    ensure!(
        saved_path.starts_with(&draft_maildir) && saved_path.is_file(),
        "indexed draft is not in the disposable Maildir: {}",
        saved_path.display()
    );
    assert_eq!(saved["report"]["local_path"], Value::Null, "{saved}");
    assert_eq!(
        driver.command("health", json!({}))?["ok"],
        true,
        "application stopped after the first draft save"
    );

    let refresh_status = driver.command("search_status", json!({}))?;
    let refresh_generation = refresh_status["generation"]
        .as_u64()
        .with_context(|| format!("draft refresh had no search generation: {refresh_status}"))?;
    ensure!(
        refresh_generation > initial_generation,
        "indexed draft save did not schedule a search refresh: startup={initial_generation}, status={refresh_status}"
    );
    let refreshed = driver.wait_for_search(STARTUP_TIMEOUT)?;
    ensure!(
        refreshed["state"]["full_search_outcome_generation"]
            .as_u64()
            .is_some_and(|generation| generation >= refresh_generation),
        "post-index search refresh did not settle: {refreshed}"
    );
    assert_eq!(
        refreshed["state"]["active_draft"]["path"],
        saved_path.display().to_string(),
        "post-index refresh silently detached the saved composer: {refreshed}"
    );
    assert_eq!(
        refreshed["state"]["active_draft"]["message_id"], saved_message_id,
        "post-index refresh changed the active draft identity: {refreshed}"
    );
    assert_eq!(
        refreshed["state"]["active_draft"]["indexed"], true,
        "Maildir draft was not retained as indexed: {refreshed}"
    );
    assert_eq!(
        refreshed["state"]["active_draft"]["saved_fields"], refreshed["state"]["compose_fields"],
        "post-index composer was dirty immediately after save: {refreshed}"
    );

    let query_options = notm_notmuch::QueryOptions {
        excluded_tags: Vec::new(),
        ..notm_notmuch::QueryOptions::default()
    };
    let indexed_count = fixture.open_readonly()?.count_messages(
        &format!("id:{saved_message_id} and tag:draft"),
        &query_options,
    )?;
    assert_eq!(
        indexed_count, 1,
        "saved Maildir draft was not indexed with its draft tag"
    );

    let resaved = driver.command("save_draft", json!({}))?;
    assert_eq!(
        resaved["ok"], true,
        "repeated clean draft save failed: {resaved}"
    );
    assert_eq!(
        resaved["report"]["maildir_path"],
        saved_path.display().to_string(),
        "repeated clean save created a different draft: {resaved}"
    );
    assert_eq!(
        resaved["report"]["indexed_message_id"], saved_message_id,
        "repeated clean save changed the draft Message-ID: {resaved}"
    );
    assert_eq!(
        driver.command("health", json!({}))?["ok"],
        true,
        "application stopped after the repeated draft save"
    );

    let selected_other = driver.command("select_thread_by_index", json!({"index": 1}))?;
    assert_eq!(
        selected_other["ok"], true,
        "clean saved draft prompted while selecting another message: {selected_other}"
    );
    let selected_again = driver.command("select_thread_by_index", json!({"index": 0}))?;
    assert_eq!(
        selected_again["ok"], true,
        "first navigation left an unsaved-changes prompt behind: {selected_again}"
    );
    let reopened = driver.command("open_compose", json!({}))?;
    assert_eq!(
        reopened["ok"], true,
        "second navigation left a stale hidden-composer prompt: {reopened}"
    );
    assert_eq!(
        reopened["pending_confirmation"], false,
        "opening a new composer found stale hidden draft changes: {reopened}"
    );
    assert_eq!(driver.command("health", json!({}))?["ok"], true);

    Ok(())
}

#[cfg(unix)]
#[test]
fn indexed_maildir_saved_draft_restart_does_not_prompt_as_unsaved() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP indexed_maildir_saved_draft_restart_does_not_prompt_as_unsaved: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running indexed Maildir saved-draft restart UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-draft-restart-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let draft_maildir = fixture.root.join("Drafts");
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
             \n[identity]\nname = \"Fixture Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[drafts]\nsave_maildir = true\nmaildir = {}\ntags = [\"draft\"]\nindex_after_save = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml_path(&draft_maildir),
        ),
    )?;

    let token = format!("notm-draft-restart-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir.clone(), &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    for (command, value) in [
        ("compose_set_from", "Fixture Sender <sender@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Restarted indexed draft regression"),
        (
            "compose_set_body",
            "A saved draft must not become unsaved recovery state after restart.",
        ),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }

    let saved = driver.command("save_draft", json!({}))?;
    assert_eq!(saved["ok"], true, "indexed draft save failed: {saved}");
    let saved_path = saved["report"]["maildir_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("indexed draft save had no Maildir path: {saved}"))?;
    let saved_message_id = saved["report"]["indexed_message_id"]
        .as_str()
        .with_context(|| format!("indexed draft save had no Message-ID: {saved}"))?
        .to_string();
    let recovery_path = work_dir.join("state/notm/draft.json");
    ensure!(saved_path.is_file(), "saved draft file is missing");
    ensure!(
        !recovery_path.exists(),
        "successful save left clean fields in transient recovery state at {}",
        recovery_path.display()
    );

    let before_post_save_edit = draft_write_count(&draft_autosave_status(&mut driver)?)?;
    let edited = driver.command(
        "compose_set_body",
        json!({"value": "A later edit must recreate transient recovery state."}),
    )?;
    assert_eq!(edited["ok"], true, "post-save edit failed: {edited}");
    wait_for_draft_write_after(&mut driver, before_post_save_edit, Duration::from_secs(3))?;
    ensure!(
        recovery_path.is_file(),
        "a post-save edit did not recreate transient recovery state"
    );
    let before_revert = draft_write_count(&draft_autosave_status(&mut driver)?)?;
    let reverted = driver.command(
        "compose_set_body",
        json!({"value": "A saved draft must not become unsaved recovery state after restart."}),
    )?;
    assert_eq!(reverted["ok"], true, "post-save revert failed: {reverted}");
    wait_for_draft_write_after(&mut driver, before_revert, Duration::from_secs(3))?;
    ensure!(
        !recovery_path.exists(),
        "returning exactly to saved fields left transient recovery state"
    );

    let closed = driver.command("close_main_window", json!({}))?;
    assert_eq!(
        closed["ok"], true,
        "clean saved draft blocked normal close: {closed}"
    );
    drop(driver);
    let status = app.wait_for_exit(Duration::from_secs(3))?;
    ensure!(
        status.success(),
        "first app process did not exit normally: {status}\n{}",
        app.logs()
    );
    ensure!(
        saved_path.is_file(),
        "normal close deleted the saved draft at {}",
        saved_path.display()
    );
    ensure!(
        !recovery_path.exists(),
        "normal close recreated recovery state for a clean saved draft at {}",
        recovery_path.display()
    );

    // Preserve the first process's XDG state while replacing its private display and
    // harness artifacts so the second process observes a genuine application restart.
    drop(app.display.take());
    for path in [&app.socket_path, &app.log_path] {
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("removing first-run artifact {}", path.display()))?;
        }
    }
    let display_dir = work_dir.join("gui-display");
    if display_dir.exists() {
        fs::remove_dir_all(&display_dir)
            .with_context(|| format!("removing first-run display {}", display_dir.display()))?;
    }

    let restart_token = format!("notm-draft-restart-second-ui-{run_id}");
    let mut restarted = FixtureApp::spawn_with_config(work_dir, &restart_token, &config_path)?;
    let mut restarted_driver = restarted.connect(&restart_token)?;
    restarted_driver.wait_for_search(STARTUP_TIMEOUT)?;
    let startup_deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let startup_state = restarted_driver.command("app_state", json!({}))?;
        if startup_state["state"]["selected_thread"] != Value::Null {
            break;
        }
        ensure!(
            Instant::now() < startup_deadline,
            "restart did not settle on its initial message: {startup_state}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
    // Let the search result's GTK selection callback finish before checking that a
    // normal action remains available; a stale prompt blocks all harness mutations.
    thread::sleep(Duration::from_secs(1));
    let restart_state = restarted_driver.command("app_state", json!({}))?;
    let drafts_search = restarted_driver.command("run_search", json!({"query": "tag:draft"}))?;
    assert_eq!(
        drafts_search["ok"],
        true,
        "an unchanged saved draft became an unsaved-composer prompt after restart: \
         recovery_exists={}, state={restart_state}, action={drafts_search}",
        recovery_path.exists()
    );
    let draft_search_result = restarted_driver.wait_for_search(STARTUP_TIMEOUT)?;
    ensure!(
        json_array_at(&draft_search_result, &["state", "thread_list_items"])?
            .iter()
            .any(|thread| thread["subject"] == "Restarted indexed draft regression"),
        "saved indexed draft was not accessible through tag:draft after restart: {draft_search_result}"
    );
    let draft_open_deadline = Instant::now() + STARTUP_TIMEOUT;
    let draft_result = loop {
        let current = restarted_driver.command("app_state", json!({}))?;
        if current["state"]["active_draft"]["path"] == saved_path.display().to_string() {
            break current;
        }
        ensure!(
            Instant::now() < draft_open_deadline,
            "saved indexed draft search never opened its composer: {current}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    assert_eq!(
        draft_result["state"]["active_draft"]["path"],
        saved_path.display().to_string(),
        "opening the saved indexed draft did not restore its active context: {draft_result}"
    );
    assert_eq!(
        draft_result["state"]["active_draft"]["message_id"], saved_message_id,
        "opening the saved indexed draft changed its Message-ID: {draft_result}"
    );
    assert_eq!(
        draft_result["state"]["compose_fields"]["subject"], "Restarted indexed draft regression",
        "opening the saved indexed draft lost its fields: {draft_result}"
    );
    ensure!(
        !recovery_path.exists(),
        "opening a clean saved draft recreated transient recovery state at {}",
        recovery_path.display()
    );
    let final_confirmation = restarted_driver.command("pending_confirmation", json!({}))?;
    assert_eq!(
        final_confirmation["pending"],
        Value::Null,
        "opening the persisted draft triggered an unsaved-composer warning: {final_confirmation}"
    );

    Ok(())
}

#[test]
fn fixture_indexed_draft_delete_removes_row_without_missing_body() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_indexed_draft_delete_removes_row_without_missing_body: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running indexed-draft delete UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-indexed-draft-delete-ui-{run_id}"));
    let token = format!("notm-indexed-draft-delete-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;

    let startup = driver.wait_for_search(STARTUP_TIMEOUT)?;
    let startup_generation = startup["state"]["search_generation"]
        .as_u64()
        .with_context(|| format!("startup state had no search generation: {startup}"))?;
    let search = driver.command("run_search", json!({"query": "tag:draft"}))?;
    assert_eq!(search["ok"], true, "draft search failed: {search}");
    ensure!(
        search["generation"]
            .as_u64()
            .is_some_and(|generation| generation > startup_generation),
        "draft search did not supersede the settled startup search: {search}"
    );
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    let open_deadline = Instant::now() + STARTUP_TIMEOUT;
    let opened = loop {
        let state = driver.command("app_state", json!({}))?;
        if state["state"]["active_draft"]["indexed"] == true {
            break state;
        }
        ensure!(
            Instant::now() < open_deadline,
            "fixture indexed draft did not open: {state}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    let draft_path = opened["state"]["active_draft"]["path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("opened indexed draft had no path: {opened}"))?;
    let message_id = opened["state"]["active_draft"]["message_id"]
        .as_str()
        .with_context(|| format!("opened indexed draft had no Message-ID: {opened}"))?
        .to_string();
    let thread_id = opened["state"]["selected_thread"]["thread_id"]
        .as_str()
        .with_context(|| format!("opened indexed draft had no thread ID: {opened}"))?
        .to_string();
    assert_eq!(
        opened["state"]["selected_message"]["message_id"], message_id,
        "active draft and selected message identities diverged: {opened}"
    );
    ensure!(
        draft_path.is_file(),
        "fixture indexed draft file is missing"
    );

    let delete = driver.command("delete_active_draft", json!({}))?;
    assert_eq!(
        delete["pending_confirmation"], true,
        "indexed draft delete did not request confirmation: {delete}"
    );
    let delete_id = pending_confirmation_id(&mut driver, "delete_active_draft")?;
    let deleted = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": delete_id}),
    )?;
    assert_eq!(
        deleted["ok"], true,
        "indexed draft delete failed: {deleted}"
    );
    let immediate_view = driver.command("message_view_text", json!({}))?;
    let rendered_missing_body = immediate_view["text"]
        .as_str()
        .is_some_and(|text| text.contains("Could not parse body"));
    let immediate_state = driver.command("app_state", json!({}))?;
    let deleted_row_still_present =
        json_array_at(&immediate_state, &["state", "thread_list_items"])?
            .iter()
            .any(|thread| thread["thread_id"] == thread_id);

    let refreshed = driver.wait_for_search(STARTUP_TIMEOUT)?;
    ensure!(
        !json_array_at(&refreshed, &["state", "thread_list_items"])?
            .iter()
            .any(|thread| thread["thread_id"] == thread_id),
        "deleted indexed draft remained in the message list: {refreshed}"
    );
    ensure!(
        !draft_path.exists(),
        "indexed draft file survived confirmed local deletion"
    );
    let final_view = driver.command("message_view_text", json!({}))?;
    ensure!(
        !deleted_row_still_present,
        "deleted draft remained in the message list while results reloaded: {immediate_state}"
    );
    ensure!(
        !rendered_missing_body,
        "deleting the selected draft rendered its now-missing file: {immediate_view}"
    );
    ensure!(
        final_view["text"].as_str().is_none_or(str::is_empty),
        "empty draft results retained stale message text: {final_view}"
    );

    Ok(())
}

#[cfg(unix)]
#[derive(Debug, PartialEq)]
struct DraftConfirmationSnapshot {
    compose_fields: Value,
    active_draft: Value,
    recovery_bytes: Option<Vec<u8>>,
    persisted_draft_bytes: BTreeMap<PathBuf, Vec<u8>>,
}

#[cfg(unix)]
fn read_optional_file(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(unix)]
fn capture_draft_confirmation_snapshot(
    driver: &mut UiDriver,
    recovery_path: &Path,
    persisted_drafts: &[PathBuf],
) -> anyhow::Result<DraftConfirmationSnapshot> {
    let state = driver.command("pending_confirmation", json!({}))?;
    let persisted_draft_bytes = persisted_drafts
        .iter()
        .map(|path| Ok((path.clone(), fs::read(path)?)))
        .collect::<anyhow::Result<_>>()?;
    Ok(DraftConfirmationSnapshot {
        compose_fields: state["compose_fields"].clone(),
        active_draft: state["active_draft"].clone(),
        recovery_bytes: read_optional_file(recovery_path)?,
        persisted_draft_bytes,
    })
}

#[cfg(unix)]
fn pending_confirmation_id(driver: &mut UiDriver, kind: &str) -> anyhow::Result<u64> {
    let pending = driver.command("pending_confirmation", json!({}))?;
    assert_eq!(pending["ok"], true, "pending-state query failed: {pending}");
    assert_eq!(
        pending["pending"]["kind"], kind,
        "unexpected confirmation action: {pending}"
    );
    assert_eq!(
        pending["pending"]["visible"], true,
        "real confirmation dialog was not visible: {pending}"
    );
    pending["pending"]["id"]
        .as_u64()
        .with_context(|| format!("confirmation had no numeric id: {pending}"))
}

#[cfg(unix)]
fn accept_send_confirmation(driver: &mut UiDriver) -> anyhow::Result<Value> {
    let id = pending_confirmation_id(driver, "send_composer")?;
    let accepted = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": id}),
    )?;
    assert_eq!(
        accepted["ok"], true,
        "saved-draft Send confirmation failed: {accepted}"
    );
    assert_eq!(accepted["last_completion"]["id"], id);
    assert_eq!(accepted["last_completion"]["accepted"], true);
    assert_eq!(accepted["last_completion"]["succeeded"], true);
    Ok(accepted)
}

#[cfg(unix)]
fn reject_confirmation_unchanged(
    driver: &mut UiDriver,
    id: u64,
    recovery_path: &Path,
    persisted_drafts: &[PathBuf],
    before: &DraftConfirmationSnapshot,
) -> anyhow::Result<Value> {
    let rejected = driver.command(
        "respond_confirmation",
        json!({"response": "reject", "id": id}),
    )?;
    assert_eq!(
        rejected["ok"], true,
        "confirmation rejection failed: {rejected}"
    );
    assert_eq!(rejected["pending"], Value::Null);
    assert_eq!(rejected["last_completion"]["id"], id);
    assert_eq!(rejected["last_completion"]["accepted"], false);
    assert_eq!(rejected["last_completion"]["succeeded"], true);
    let after = capture_draft_confirmation_snapshot(driver, recovery_path, persisted_drafts)?;
    ensure!(
        after == *before,
        "rejected confirmation mutated draft state\nbefore: {before:#?}\nafter: {after:#?}"
    );
    Ok(rejected)
}

#[cfg(unix)]
#[test]
fn fixture_draft_confirmations_preserve_rejected_state() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_draft_confirmations_preserve_rejected_state: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running draft-confirmation desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-draft-confirmation-ui-{run_id}"));
    let token = format!("notm-draft-confirmation-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;

    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    for (command, value) in [
        ("compose_set_to", "first@example.test"),
        ("compose_set_subject", "First persisted draft"),
        ("compose_set_body", "Original persisted body"),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }
    let first_saved = driver.command("save_draft", json!({}))?;
    assert_eq!(first_saved["ok"], true, "draft save failed: {first_saved}");
    let first_path = first_saved["report"]["local_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("saved draft had no path: {first_saved}"))?;
    let list_state = driver.command("draft_list_state", json!({}))?;
    let recovery_path = list_state["recovery_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("draft state had no recovery path: {list_state}"))?;

    let before_dirty = draft_write_count(&draft_autosave_status(&mut driver)?)?;
    let dirtied = driver.command(
        "compose_set_subject",
        json!({"value": "Dirty replacement must be confirmed"}),
    )?;
    assert_eq!(dirtied["ok"], true, "composer edit failed: {dirtied}");
    wait_for_draft_write_after(&mut driver, before_dirty, Duration::from_secs(3))?;
    let before_replacement = capture_draft_confirmation_snapshot(
        &mut driver,
        &recovery_path,
        std::slice::from_ref(&first_path),
    )?;
    let replacement = driver.command("open_compose", json!({}))?;
    assert_eq!(replacement["ok"], true);
    assert_eq!(replacement["pending_confirmation"], true);
    let replacement_id = pending_confirmation_id(&mut driver, "new")?;

    for (command, args) in [
        (
            "compose_set_subject",
            json!({"value": "must not replace pending bytes"}),
        ),
        ("save_draft", json!({})),
        ("clear_draft", json!({})),
        ("compose_send", json!({})),
        ("run_manual_sync", json!({})),
        ("close_main_window", json!({})),
        ("run_command", json!({"command": ":new"})),
    ] {
        let blocked = driver.command(command, args)?;
        assert_eq!(
            blocked["ok"], false,
            "{command} was accepted while a modal was pending: {blocked}"
        );
        ensure!(
            blocked["error"]
                .as_str()
                .is_some_and(|error| error.contains("confirmation is pending")),
            "{command} did not report the pending modal: {blocked}"
        );
    }
    let still_pending = driver.command("pending_confirmation", json!({}))?;
    assert_eq!(still_pending["pending"]["id"], replacement_id);
    assert_eq!(still_pending["pending"]["kind"], "new");
    let after_blocked_mutations = capture_draft_confirmation_snapshot(
        &mut driver,
        &recovery_path,
        std::slice::from_ref(&first_path),
    )?;
    assert_eq!(
        after_blocked_mutations, before_replacement,
        "blocked harness mutations changed pending composer or persisted bytes"
    );
    reject_confirmation_unchanged(
        &mut driver,
        replacement_id,
        &recovery_path,
        std::slice::from_ref(&first_path),
        &before_replacement,
    )?;

    let replacement = driver.command("open_compose", json!({}))?;
    assert_eq!(replacement["pending_confirmation"], true);
    let replacement_id = pending_confirmation_id(&mut driver, "new")?;
    let accepted = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": replacement_id}),
    )?;
    assert_eq!(accepted["ok"], true, "replacement failed: {accepted}");
    assert_eq!(accepted["last_completion"]["accepted"], true);
    assert_eq!(accepted["compose_fields"]["subject"], "");
    assert_eq!(accepted["compose_fields"]["body"], "");
    assert_eq!(accepted["active_draft"], Value::Null);
    ensure!(
        !recovery_path.exists() && first_path.is_file(),
        "accepted New did not clear recovery while preserving the named draft"
    );

    let before_transient = draft_write_count(&draft_autosave_status(&mut driver)?)?;
    for (command, value) in [
        ("compose_set_subject", "Transient discard must be confirmed"),
        (
            "compose_set_body",
            "Transient recovery bytes must survive rejection",
        ),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }
    wait_for_draft_write_after(&mut driver, before_transient, Duration::from_secs(3))?;
    let before_discard = capture_draft_confirmation_snapshot(
        &mut driver,
        &recovery_path,
        std::slice::from_ref(&first_path),
    )?;
    let discard = driver.command("clear_draft", json!({}))?;
    assert_eq!(discard["pending_confirmation"], true);
    let discard_id = pending_confirmation_id(&mut driver, "clear_composer")?;
    reject_confirmation_unchanged(
        &mut driver,
        discard_id,
        &recovery_path,
        std::slice::from_ref(&first_path),
        &before_discard,
    )?;

    let discard = driver.command("clear_draft", json!({}))?;
    assert_eq!(discard["pending_confirmation"], true);
    let discard_id = pending_confirmation_id(&mut driver, "clear_composer")?;
    let accepted = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": discard_id}),
    )?;
    assert_eq!(accepted["ok"], true, "discard failed: {accepted}");
    assert_eq!(accepted["compose_fields"]["subject"], "");
    assert_eq!(accepted["active_draft"], Value::Null);
    ensure!(
        !recovery_path.exists(),
        "accepted discard left recovery data"
    );

    let activated = driver.command("activate_draft_by_index", json!({"index": 0}))?;
    assert_eq!(
        activated["ok"], true,
        "draft activation failed: {activated}"
    );
    assert_eq!(
        activated["active_draft"]["path"],
        first_path.display().to_string()
    );
    let before_active_delete = capture_draft_confirmation_snapshot(
        &mut driver,
        &recovery_path,
        std::slice::from_ref(&first_path),
    )?;
    let send = driver.command("compose_send", json!({}))?;
    assert_eq!(
        send["pending_confirmation"], true,
        "sending an active persisted draft did not require confirmation: {send}"
    );
    assert_eq!(send["pending"], false);
    let send_id = pending_confirmation_id(&mut driver, "send_composer")?;
    reject_confirmation_unchanged(
        &mut driver,
        send_id,
        &recovery_path,
        std::slice::from_ref(&first_path),
        &before_active_delete,
    )?;

    let active_delete = driver.command("delete_active_draft", json!({}))?;
    assert_eq!(active_delete["ok"], true);
    let active_delete_id = pending_confirmation_id(&mut driver, "delete_active_draft")?;
    reject_confirmation_unchanged(
        &mut driver,
        active_delete_id,
        &recovery_path,
        std::slice::from_ref(&first_path),
        &before_active_delete,
    )?;

    let active_delete = driver.command("delete_active_draft", json!({}))?;
    assert_eq!(active_delete["ok"], true);
    let active_delete_id = pending_confirmation_id(&mut driver, "delete_active_draft")?;
    let accepted = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": active_delete_id}),
    )?;
    assert_eq!(accepted["ok"], true, "active delete failed: {accepted}");
    assert_eq!(accepted["active_draft"], Value::Null);
    ensure!(
        !first_path.exists() && !recovery_path.exists(),
        "accepted active deletion left persisted draft state"
    );
    driver.wait_for_search(STARTUP_TIMEOUT)?;

    for (command, value) in [
        ("compose_set_to", "named@example.test"),
        ("compose_set_subject", "Named deletion target"),
        (
            "compose_set_body",
            "Named draft bytes must survive rejection",
        ),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }
    let named_saved = driver.command("save_draft", json!({}))?;
    assert_eq!(named_saved["ok"], true, "draft save failed: {named_saved}");
    let named_path = named_saved["report"]["local_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("saved draft had no path: {named_saved}"))?;
    let closed = driver.command("clear_draft", json!({}))?;
    assert_eq!(
        closed["ok"], true,
        "unchanged draft did not close: {closed}"
    );
    assert_eq!(closed["pending_confirmation"], false);
    let closed_state = driver.command("pending_confirmation", json!({}))?;
    assert_eq!(closed_state["pending"], Value::Null);
    assert_eq!(closed_state["active_draft"], Value::Null);

    let selected = driver.command("select_draft_by_index", json!({"index": 0}))?;
    assert_eq!(
        selected["ok"], true,
        "named draft was not selectable: {selected}"
    );
    let before_unrelated = draft_write_count(&draft_autosave_status(&mut driver)?)?;
    for (command, value) in [
        ("compose_set_subject", "Unrelated transient composer"),
        (
            "compose_set_body",
            "Named deletion must not mutate these recovery bytes",
        ),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }
    wait_for_draft_write_after(&mut driver, before_unrelated, Duration::from_secs(3))?;
    let before_named_delete = capture_draft_confirmation_snapshot(
        &mut driver,
        &recovery_path,
        std::slice::from_ref(&named_path),
    )?;
    let named_delete = driver.command("click_delete_selected_draft", json!({}))?;
    assert_eq!(named_delete["pending_confirmation"], true);
    let named_delete_id = pending_confirmation_id(&mut driver, "delete_named_draft")?;
    reject_confirmation_unchanged(
        &mut driver,
        named_delete_id,
        &recovery_path,
        std::slice::from_ref(&named_path),
        &before_named_delete,
    )?;

    let named_delete = driver.command("click_delete_selected_draft", json!({}))?;
    assert_eq!(named_delete["pending_confirmation"], true);
    let named_delete_id = pending_confirmation_id(&mut driver, "delete_named_draft")?;
    let accepted = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": named_delete_id}),
    )?;
    assert_eq!(accepted["ok"], true, "named delete failed: {accepted}");
    ensure!(
        !named_path.exists(),
        "accepted named deletion left its file"
    );
    let after_named_delete = capture_draft_confirmation_snapshot(&mut driver, &recovery_path, &[])?;
    assert_eq!(
        after_named_delete.compose_fields, before_named_delete.compose_fields,
        "named deletion changed unrelated composer fields"
    );
    assert_eq!(
        after_named_delete.recovery_bytes, before_named_delete.recovery_bytes,
        "named deletion changed unrelated recovery bytes"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn validated_config_launches_and_invalid_layout_requests_are_rejected() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP validated_config_launches_and_invalid_layout_requests_are_rejected: no GUI test display is available"
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
    driver.wait_for_search(STARTUP_TIMEOUT)?;

    let page = driver.command("thread_page_info", json!({}))?;
    assert_eq!(
        page["page_size"], 1,
        "configured page size was ignored: {page}"
    );
    assert_eq!(
        page["loaded"], 1,
        "initial one-row page was not loaded: {page}"
    );
    assert_eq!(
        page["can_load_more"], true,
        "fixture did not expose another page: {page}"
    );
    let load_more = driver.command("load_more_threads", json!({"select_last": false}))?;
    assert_eq!(
        load_more["scheduled"], true,
        "paging was not scheduled in the background: {load_more}"
    );
    let paged = driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(
        paged["state"]["thread_loaded_count"], 2,
        "paging did not append the second fixture row: {paged}"
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
            .is_some_and(|error| error.contains("page size must be between 1 and 1000")),
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
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP invalid_config_exits_before_exposing_the_desktop_harness: no GUI test display is available"
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
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_cold_message_id_launch_preserves_target_and_startup_query: no GUI test display is available"
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
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_existing_instance_message_id_request_reaches_primary: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running existing-instance message-id desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-message-id-remote-ui-{run_id}"));
    let token = format!("notm-message-id-remote-ui-{run_id}");
    let application_id = format!("io.github.kris004.notm.test.r{}", run_id.replace('-', ""));
    let target = "thread-root-three-message@fixture.test";
    let mut app = FixtureApp::spawn_with_application_id(work_dir, &token, &application_id)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "subject:\"Read inbox message\"")?;
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;
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
    assert_eq!(
        state["state"]["current_query"],
        format!("id:{target}"),
        "outside-candidate target did not use the direct id-query fallback: {state}"
    );
    ensure!(
        state["state"]["search_generation"].as_u64().unwrap_or(0)
            > initial["state"]["search_generation"].as_u64().unwrap_or(0),
        "outside-candidate target did not schedule its direct fallback search: initial={initial}, state={state}"
    );
    assert_target_message_rendered(&mut driver)?;

    Ok(())
}

#[test]
fn fixture_existing_instance_absent_message_id_preserves_current_view() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_existing_instance_absent_message_id_preserves_current_view: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running absent existing-instance message-id UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-message-id-absent-remote-ui-{run_id}"));
    let token = format!("notm-message-id-absent-remote-ui-{run_id}");
    let application_id = format!("io.github.kris004.notm.test.r{}", run_id.replace('-', ""));
    let missing = "globally-absent-message@fixture.test";
    let query = "subject:\"Three message thread\"";
    let root_message_id = "thread-root-three-message@fixture.test";

    let mut app = FixtureApp::spawn_with_application_id(work_dir, &token, &application_id)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    let requested = driver.command("set_search_query", json!({"query": query}))?;
    assert_eq!(
        requested["ok"], true,
        "could not set visible query: {requested}"
    );
    let searched = driver.wait_for_search(STARTUP_TIMEOUT)?;
    let rows = json_array_at(&searched, &["state", "thread_list_items"])?;
    ensure!(rows.len() == 1, "expected one fixture thread: {searched}");
    let selected_thread = driver.command("select_thread_by_index", json!({"index": 0}))?;
    assert_eq!(
        selected_thread["ok"], true,
        "could not select fixture thread: {selected_thread}"
    );
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;
    let selected = driver.command("select_message_by_index", json!({"index": 0}))?;
    assert_eq!(
        selected["selected_message"]["message_id"], root_message_id,
        "fixture root message was not selected: {selected}"
    );
    let pane = driver.command("send_key", json!({"key": "h", "modifiers": ["control"]}))?;
    assert_eq!(pane["handled"], true, "Ctrl+h was not handled: {pane}");
    assert_eq!(pane["active_pane"], "Threads", "{pane}");

    let before_state = driver.command("app_state", json!({}))?;
    let before_entry = driver.command("entry_state", json!({}))?;
    let before_selection = driver.command("thread_selection_view_state", json!({}))?;
    let before_rendered = driver.command("message_view_text", json!({}))?;
    assert_eq!(
        before_state["state"]["current_query"], query,
        "{before_state}"
    );
    assert_eq!(before_entry["search"], query, "{before_entry}");
    assert_eq!(
        before_state["state"]["active_pane"], "Threads",
        "{before_state}"
    );
    assert_eq!(before_entry["active_pane"], "Threads", "{before_entry}");
    assert_eq!(
        before_state["state"]["selected_message"]["message_id"], root_message_id,
        "{before_state}"
    );
    assert_eq!(
        before_selection["selected_local"].as_u64(),
        Some(0),
        "{before_selection}"
    );
    ensure!(
        before_rendered["text"]
            .as_str()
            .is_some_and(|text| text.contains("Thread root body.")),
        "known message was not rendered before the absent request: {before_rendered}"
    );

    app.request_message_id(&token, &application_id, missing)?;
    let expected_status = format!("Message id not found: {missing}");
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let state = driver.command("app_state", json!({})).with_context(|| {
            format!(
                "reading state after absent message-id request\n{}",
                app.logs()
            )
        })?;
        let entry = driver.command("entry_state", json!({})).with_context(|| {
            format!(
                "reading entry after absent message-id request\n{}",
                app.logs()
            )
        })?;
        let load = driver
            .command("thread_load_status", json!({}))
            .with_context(|| {
                format!(
                    "reading load state after absent message-id request\n{}",
                    app.logs()
                )
            })?;
        if entry["status"].as_str() == Some(expected_status.as_str())
            && state["state"]["pending_open_message_id"].is_null()
            && state["state"]["search_loading"] == false
            && load["busy"] == false
        {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "absent message-id request did not settle: state={state}, entry={entry}, load={load}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }

    // Wait past SearchBar's debounce so an unnecessary programmatic change
    // cannot schedule a late replacement search after the rollback appears done.
    thread::sleep(Duration::from_millis(500));
    let after_state = driver
        .command("app_state", json!({}))
        .with_context(|| format!("reading settled absent-request state\n{}", app.logs()))?;
    let after_entry = driver
        .command("entry_state", json!({}))
        .with_context(|| format!("reading settled absent-request entry\n{}", app.logs()))?;
    let after_load = driver
        .command("thread_load_status", json!({}))
        .with_context(|| format!("reading settled absent-request loader\n{}", app.logs()))?;
    let after_selection = driver
        .command("thread_selection_view_state", json!({}))
        .with_context(|| format!("reading settled absent-request selection\n{}", app.logs()))?;
    let after_rendered = driver
        .command("message_view_text", json!({}))
        .with_context(|| format!("reading settled absent-request message\n{}", app.logs()))?;

    assert_eq!(after_entry["status"], expected_status, "{after_entry}");
    assert_eq!(
        after_state["state"]["pending_open_message_id"],
        Value::Null,
        "{after_state}"
    );
    assert_eq!(
        after_state["state"]["search_loading"], false,
        "{after_state}"
    );
    assert_eq!(after_load["busy"], false, "{after_load}");
    assert_eq!(
        after_state["state"]["current_query"], before_state["state"]["current_query"],
        "absent message request replaced the active query: {after_state}"
    );
    assert_eq!(
        after_state["state"]["search_generation"], before_state["state"]["search_generation"],
        "absent message request scheduled a direct fallback search: {after_state}"
    );
    assert_eq!(
        after_state["state"]["thread_list_items"], before_state["state"]["thread_list_items"],
        "absent message request replaced the visible result set: {after_state}"
    );
    assert_eq!(
        after_entry["search"], before_entry["search"],
        "absent message request replaced the visible search text: {after_entry}"
    );
    assert_eq!(
        after_state["state"]["selected_thread"], before_state["state"]["selected_thread"],
        "absent message request changed the state thread selection: {after_state}"
    );
    assert_eq!(
        after_state["state"]["selected_message"], before_state["state"]["selected_message"],
        "absent message request changed the state message selection: {after_state}"
    );
    assert_eq!(
        after_state["state"]["messages"], before_state["state"]["messages"],
        "absent message request replaced the loaded thread: {after_state}"
    );
    assert_eq!(
        after_selection["selected_local"], before_selection["selected_local"],
        "absent message request changed the GTK local selection: {after_selection}"
    );
    assert_eq!(
        after_selection["selected_abs"], before_selection["selected_abs"],
        "absent message request changed the GTK absolute selection: {after_selection}"
    );
    assert_eq!(
        after_state["state"]["active_pane"], before_state["state"]["active_pane"],
        "absent message request changed the state pane: {after_state}"
    );
    assert_eq!(
        after_entry["active_pane"], before_entry["active_pane"],
        "absent message request changed the visible pane: {after_entry}"
    );
    assert_eq!(
        after_rendered["text"], before_rendered["text"],
        "absent message request replaced or cleared the rendered message"
    );

    Ok(())
}

#[test]
fn fixture_cold_mailto_launch_opens_prefilled_composer() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_cold_mailto_launch_opens_prefilled_composer: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running cold mailto desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-mailto-ui-{run_id}"));
    let token = format!("notm-mailto-ui-{run_id}");
    let uri = "mailto:first@example.test?to=second@example.test&\
               cc=copy@example.test&bcc=hidden@example.test&\
               subject=caf%C3%A9+notes&body=first%20line%0D%0Asecond%20line";
    let mut app = FixtureApp::spawn_with_mailto(work_dir, &token, uri)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;

    let state = driver.command("app_state", json!({}))?;
    assert_eq!(
        state["state"]["current_query"], "tag:inbox",
        "mailto launch skipped or replaced the startup search: {state}"
    );
    assert_eq!(
        state["state"]["selected_message"],
        Value::Null,
        "startup selection hid the mailto composer: {state}"
    );
    assert_eq!(
        state["state"]["compose_fields"],
        json!({
            "from": "Fixture User <fixture@example.test>",
            "to": "first@example.test, second@example.test",
            "cc": "copy@example.test",
            "bcc": "hidden@example.test",
            "subject": "café+notes",
            "body": "first line\nsecond line",
            "attachments": [],
            "in_reply_to": null,
            "references": [],
            "text_reply_quote": null,
            "html_reply_quote": null,
        }),
        "mailto fields were not mapped into the composer: {state}"
    );
    assert_eq!(state["state"]["active_pane"], "Message", "{state}");
    let visible = driver.command("html_view_state", json!({}))?;
    assert_eq!(
        visible["visible_child"], "compose",
        "mailto composer was not visible after startup: {visible}"
    );

    Ok(())
}

#[test]
fn fixture_existing_instance_mailto_request_confirms_dirty_replacement() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_existing_instance_mailto_request_confirms_dirty_replacement: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running existing-instance mailto desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-mailto-remote-ui-{run_id}"));
    let token = format!("notm-mailto-remote-ui-{run_id}");
    let application_id = format!("io.github.kris004.notm.test.r{}", run_id.replace('-', ""));
    let mut app = FixtureApp::spawn_with_application_id(work_dir, &token, &application_id)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    assert_eq!(
        driver.command(
            "compose_set_subject",
            json!({"value": "Keep this draft until confirmed"}),
        )?["ok"],
        true
    );

    let uri = "mailto:new@example.test?subject=Replacement%20subject&body=Replacement%20body";
    app.request_mailto(&token, &application_id, uri)?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let pending = loop {
        let response = driver.command("pending_confirmation", json!({}))?;
        if response["pending"]["kind"] == "mailto" {
            break response;
        }
        ensure!(
            Instant::now() < deadline,
            "primary instance did not receive the mailto request: {response}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    assert_eq!(
        pending["compose_fields"]["subject"], "Keep this draft until confirmed",
        "remote mailto request replaced dirty state before confirmation: {pending}"
    );
    let confirmation_id = pending["pending"]["id"]
        .as_u64()
        .context("mailto confirmation had no numeric id")?;
    let accepted = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": confirmation_id}),
    )?;
    assert_eq!(
        accepted["ok"], true,
        "mailto replacement failed: {accepted}"
    );
    assert_eq!(accepted["compose_fields"]["to"], "new@example.test");
    assert_eq!(accepted["compose_fields"]["subject"], "Replacement subject");
    assert_eq!(accepted["compose_fields"]["body"], "Replacement body");
    assert_eq!(accepted["active_draft"], Value::Null);
    let visible = driver.command("html_view_state", json!({}))?;
    assert_eq!(visible["visible_child"], "compose", "{visible}");

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

#[cfg(unix)]
#[test]
fn fixture_reply_all_preserves_quoted_names_and_flattens_groups() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_reply_all_preserves_quoted_names_and_flattens_groups: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running reply-all address desktop UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-reply-all-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
             \n[identity]\nname = \"Fixture User\"\nprimary_email = \"fixture@example.test\"\nother_email = [\"alt@example.test\"]\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
        ),
    )?;
    let token = format!("notm-reply-all-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir, &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "id:reply-all-addresses@fixture.test")?;

    let reply = driver.command("reply_all_selected", json!({}))?;
    assert_eq!(reply["ok"], true, "reply-all command failed: {reply}");
    assert_eq!(
        reply["pending"], true,
        "reply-all was not prepared asynchronously: {reply}"
    );
    wait_for_composer_preparation_idle(&mut driver, STARTUP_TIMEOUT)?;
    let reply = driver.command("app_state", json!({}))?;
    let fields = &reply["state"]["compose_fields"];
    assert_eq!(
        fields["to"], r#"Sender <sender@example.test>, "Doe, Jane" <jane@example.test>"#,
        "reply-all did not preserve the quoted display name: {reply}"
    );
    assert_eq!(
        fields["cc"], r#""Smith, John" <john@example.test>, other@example.test"#,
        "reply-all did not flatten the recipient group in order: {reply}"
    );
    for identity in ["fixture@example.test", "alt@example.test"] {
        ensure!(
            !fields["to"].as_str().unwrap_or_default().contains(identity)
                && !fields["cc"].as_str().unwrap_or_default().contains(identity),
            "reply-all retained fixture identity {identity}: {reply}"
        );
    }

    Ok(())
}

#[test]
fn fixture_attachment_save_keeps_existing_files() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_attachment_save_keeps_existing_files: no GUI test display is available"
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
    assert_eq!(
        first["pending"], true,
        "first save was not asynchronous: {first}"
    );
    let first_status = wait_for_attachment_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    let first_path = first_status["last_completion"]["path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("first save returned no path: {first_status}"))?;
    assert_eq!(
        first_status["last_completion"]["applied"], true,
        "first save completion was stale: {first_status}"
    );
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
    assert_eq!(
        second["pending"], true,
        "second save was not asynchronous: {second}"
    );
    let second_status = wait_for_attachment_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    let second_path = second_status["last_completion"]["path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("second save returned no path: {second_status}"))?;
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

#[test]
fn fixture_attachment_io_stays_responsive_and_rejects_stale_ui_completion() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_attachment_io_stays_responsive_and_rejects_stale_ui_completion: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running asynchronous attachment I/O stress with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-attachment-async-ui-{run_id}"));
    let downloads = work_dir.join("downloads");
    fs::create_dir_all(&downloads)?;
    let token = format!("notm-attachment-async-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir.clone(), &token)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "subject:\"Attachment message\"")?;

    driver.command("set_fixture_attachment_delay", json!({"milliseconds": 900}))?;
    let slow_started = Instant::now();
    let slow = driver.command(
        "save_selected_attachment",
        json!({"index": 0, "dir": downloads}),
    )?;
    ensure!(
        slow_started.elapsed() < Duration::from_millis(500),
        "delayed attachment save blocked the GTK harness: elapsed={:?}, response={slow}",
        slow_started.elapsed()
    );
    assert_eq!(
        slow["pending"], true,
        "slow save did not remain pending: {slow}"
    );
    let slow_request = slow["request_id"]
        .as_u64()
        .with_context(|| format!("slow save returned no request ID: {slow}"))?;
    let first_health = driver.command("health", json!({}))?;
    assert_eq!(
        first_health["attachment_io"]["busy"], true,
        "health did not report delayed attachment I/O: {first_health}"
    );

    driver.command("set_fixture_attachment_delay", json!({"milliseconds": 0}))?;
    let current = driver.command(
        "save_selected_attachment",
        json!({"index": 0, "dir": downloads}),
    )?;
    let current_request = current["request_id"]
        .as_u64()
        .with_context(|| format!("current save returned no request ID: {current}"))?;
    ensure!(
        current_request > slow_request,
        "newer save did not receive a newer request ID: slow={slow}, current={current}"
    );
    let current_deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let status = driver.command("attachment_io_status", json!({}))?;
        if status["last_completion"]["request_id"] == current_request
            && status["last_completion"]["applied"] == true
        {
            assert_eq!(
                status["busy"], true,
                "slow stale write unexpectedly finished before the current write: {status}"
            );
            break;
        }
        ensure!(
            Instant::now() < current_deadline,
            "current attachment save did not complete in time: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }

    let compose = driver.command("open_compose", json!({}))?;
    assert_eq!(
        compose["ok"], true,
        "GTK input was not processed during stale attachment I/O: {compose}"
    );
    let cleared = driver.command("attachment_list_items", json!({}))?;
    assert_eq!(
        json_array_at(&cleared, &["attachments"])?.len(),
        0,
        "hiding the thread retained stale attachment items: {cleared}"
    );
    thread::sleep(Duration::from_millis(150));
    let second_health = driver.command("health", json!({}))?;
    ensure!(
        second_health["gtk_heartbeat"].as_u64().unwrap_or(0)
            > first_health["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat stopped during delayed attachment I/O: before={first_health}, after={second_health}"
    );
    assert_eq!(
        second_health["attachment_io"]["busy"], true,
        "delayed attachment I/O ended before the responsiveness assertion: {second_health}"
    );

    let settled = wait_for_attachment_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    ensure!(
        settled["stale_completion_count"].as_u64().unwrap_or(0) >= 1,
        "stale completion was not rejected: {settled}"
    );
    assert_eq!(
        settled["last_completion"]["request_id"], slow_request,
        "delayed stale completion did not arrive last: {settled}"
    );
    assert_eq!(
        settled["last_completion"]["applied"], false,
        "stale attachment completion updated the UI: {settled}"
    );
    for path in [downloads.join("note.txt"), downloads.join("note (1).txt")] {
        ensure!(
            path.is_file(),
            "accepted attachment save was lost: {}",
            path.display()
        );
        ensure!(
            String::from_utf8_lossy(&fs::read(&path)?).contains("attached text"),
            "saved attachment had unexpected contents: {}",
            path.display()
        );
    }
    let logs = driver.command("get_logs", json!({}))?;
    ensure!(
        logs["last_operation"]
            .as_str()
            .unwrap_or_default()
            .contains(&downloads.join("note.txt").display().to_string()),
        "stale completion replaced the current attachment operation: {logs}"
    );

    select_first_thread(&mut driver, "subject:\"Attachment message\"")?;
    let blocked_parent = work_dir.join("not-a-directory");
    fs::write(&blocked_parent, b"block attachment directory creation")?;
    driver.command("set_fixture_attachment_delay", json!({"milliseconds": 600}))?;
    let failed = driver.command(
        "save_selected_attachment",
        json!({"index": 0, "dir": blocked_parent.join("child")}),
    )?;
    assert_eq!(
        failed["pending"], true,
        "failure path blocked synchronously: {failed}"
    );
    let failure_health_before = driver.command("health", json!({}))?;
    thread::sleep(Duration::from_millis(150));
    let failure_health_after = driver.command("health", json!({}))?;
    ensure!(
        failure_health_after["gtk_heartbeat"].as_u64().unwrap_or(0)
            > failure_health_before["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat stopped during a failing attachment write: before={failure_health_before}, after={failure_health_after}"
    );
    let failure_status = wait_for_attachment_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        failure_status["last_completion"]["applied"], true,
        "current attachment failure was treated as stale: {failure_status}"
    );
    ensure!(
        failure_status["last_completion"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("saving attachment")),
        "typed attachment write failure was not reported: {failure_status}"
    );
    let failure_logs = driver.command("get_logs", json!({}))?;
    ensure!(
        failure_logs["recent_error"]
            .as_str()
            .is_some_and(|error| error.contains("saving attachment")),
        "attachment write failure was not visible in UI state: {failure_logs}"
    );

    let composer_cache_directory = work_dir.join("state/notm/compose-attachments");
    let count_composer_cache_files = || -> anyhow::Result<usize> {
        match fs::read_dir(&composer_cache_directory) {
            Ok(entries) => Ok(entries.collect::<Result<Vec<_>, _>>()?.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "reading composer cache directory {}",
                    composer_cache_directory.display()
                )
            }),
        }
    };
    let cache_metrics_start = driver.command("health", json!({}))?;
    let initial_submitted = cache_metrics_start["attachment_io"]["composer_cache"]["submitted"]
        .as_u64()
        .unwrap_or(0);
    let initial_cancelled = cache_metrics_start["attachment_io"]["composer_cache"]["cancelled"]
        .as_u64()
        .unwrap_or(0);
    let initial_heartbeat = cache_metrics_start["gtk_heartbeat"].as_u64().unwrap_or(0);

    const RAPID_CACHE_REQUESTS: u64 = 8;
    driver.command(
        "set_fixture_attachment_delay",
        json!({"milliseconds": 1200}),
    )?;
    for request_index in 0..RAPID_CACHE_REQUESTS {
        let cache_started = Instant::now();
        let forward = driver.command("forward_as_attachment_selected", json!({}))?;
        let command_elapsed = cache_started.elapsed();
        assert_eq!(
            forward["ok"], true,
            "forward-as-attachment request {request_index} did not start: {forward}"
        );
        assert_eq!(
            forward["pending"], true,
            "forward request {request_index} was not asynchronous: {forward}"
        );
        ensure!(
            command_elapsed < Duration::from_millis(500),
            "forward request {request_index} blocked GTK: elapsed={command_elapsed:?}, response={forward}"
        );
        wait_for_composer_preparation_idle(&mut driver, STARTUP_TIMEOUT)?;
    }
    let cache_health_before = driver.command("health", json!({}))?;
    let cache_before = &cache_health_before["attachment_io"]["composer_cache"];
    assert_eq!(
        cache_before["busy"], true,
        "health did not report delayed composer caching: {cache_health_before}"
    );
    ensure!(
        cache_health_before["gtk_heartbeat"].as_u64().unwrap_or(0) > initial_heartbeat,
        "GTK heartbeat did not advance during rapid cache requests: before={cache_metrics_start}, after={cache_health_before}"
    );
    ensure!(
        cache_before["submitted"].as_u64().unwrap_or(0) >= initial_submitted + RAPID_CACHE_REQUESTS,
        "rapid requests were not all submitted to the bounded cache service: {cache_health_before}"
    );
    assert_eq!(
        cache_before["peak_active_preparations"], 1,
        "composer cache ran more than one preparation concurrently: {cache_health_before}"
    );
    ensure!(
        cache_before["pending_requests"]
            .as_u64()
            .unwrap_or(u64::MAX)
            <= 1
            && cache_before["peak_pending_requests"]
                .as_u64()
                .unwrap_or(u64::MAX)
                <= 1,
        "composer cache retained an unbounded pending queue: {cache_health_before}"
    );
    ensure!(
        cache_before["cancelled"].as_u64().unwrap_or(0)
            >= initial_cancelled + RAPID_CACHE_REQUESTS - 1,
        "superseded cache requests were not cooperatively cancelled: {cache_health_before}"
    );
    driver.command(
        "compose_set_subject",
        json!({"value": "Keep newer composer edit"}),
    )?;
    thread::sleep(Duration::from_millis(150));
    let cache_health_after = driver.command("health", json!({}))?;
    ensure!(
        cache_health_after["gtk_heartbeat"].as_u64().unwrap_or(0)
            > cache_health_before["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat stopped during delayed composer caching: before={cache_health_before}, after={cache_health_after}"
    );
    let cancelled_cache = wait_for_composer_attachment_cache_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        cancelled_cache["composer_cache"]["outcome"], "cancelled",
        "typing did not cancel the pending composer cache: {cancelled_cache}"
    );
    ensure!(
        cancelled_cache["composer_cache"]["cancelled"]
            .as_u64()
            .unwrap_or(0)
            >= initial_cancelled + RAPID_CACHE_REQUESTS,
        "typing did not cooperatively cancel the latest cache worker: {cancelled_cache}"
    );
    assert_eq!(
        count_composer_cache_files()?,
        0,
        "cancelled composer caches left stale files in {}",
        composer_cache_directory.display()
    );
    // Cancellation is intentionally faster than the outer draft debounce;
    // wait for the state snapshot rather than mistaking a still-pending GTK
    // field capture for lost typing.
    thread::sleep(Duration::from_millis(350));
    let stale_composer = driver.command("app_state", json!({}))?;
    assert_eq!(
        stale_composer["state"]["compose_fields"]["subject"], "Keep newer composer edit",
        "stale composer cache completion replaced newer typing: {stale_composer}"
    );
    assert_eq!(
        json_array_at(&stale_composer, &["state", "compose_fields", "attachments"])?.len(),
        0,
        "stale composer cache completion installed its attachment: {stale_composer}"
    );

    driver.command("set_fixture_attachment_delay", json!({"milliseconds": 0}))?;
    let replacement = driver.command("forward_as_attachment_selected", json!({}))?;
    assert_eq!(
        replacement["pending"], true,
        "replacement preparation was not asynchronous: {replacement}"
    );
    wait_for_composer_preparation_idle(&mut driver, STARTUP_TIMEOUT)?;
    let replacement = driver.command("pending_confirmation", json!({}))?;
    assert!(
        replacement["pending"].is_object(),
        "dirty composer replacement did not preserve modal confirmation: {replacement}"
    );
    let accepted = driver.command("respond_confirmation", json!({"response": "accept"}))?;
    assert_eq!(
        accepted["ok"], true,
        "composer replacement failed: {accepted}"
    );
    let applied_cache = wait_for_composer_attachment_cache_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        applied_cache["composer_cache"]["outcome"], "applied",
        "accepted composer cache did not report an applied completion: {applied_cache}"
    );
    assert_eq!(
        applied_cache["composer_cache"]["completed_generation"],
        applied_cache["composer_cache"]["latest_generation"],
        "accepted composer cache was not the latest generation: {applied_cache}"
    );
    assert_eq!(
        applied_cache["composer_cache"]["peak_active_preparations"], 1,
        "composer cache exceeded one worker after the accepted replacement: {applied_cache}"
    );
    let cached_composer = driver.command("app_state", json!({}))?;
    let cached_paths = json_array_at(
        &cached_composer,
        &["state", "compose_fields", "attachments"],
    )?;
    assert_eq!(
        cached_paths.len(),
        1,
        "accepted forward did not install one cached attachment: {cached_composer}"
    );
    let cached_path = cached_paths[0]
        .as_str()
        .with_context(|| format!("cached forward path was not a string: {cached_composer}"))?;
    ensure!(
        Path::new(cached_path).starts_with(&composer_cache_directory),
        "accepted composer cache path escaped the isolated cache directory: {cached_path}"
    );
    assert_eq!(
        count_composer_cache_files()?,
        1,
        "accepted composer cache did not leave exactly one committed file"
    );
    ensure!(
        fs::read(cached_path)?.starts_with(b"From:"),
        "cached forward did not contain the exact RFC 5322 source: {cached_path}"
    );

    Ok(())
}

#[test]
fn fixture_closing_last_window_waits_for_atomic_attachment_save() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_closing_last_window_waits_for_atomic_attachment_save: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running atomic attachment lifetime UI smoke with {display}");

    const LARGE_ATTACHMENT_BYTES: usize = 6 * 1024 * 1024;
    const ATTACHMENT_FILENAME: &str = "fixture-0-00.txt";

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-attachment-lifetime-ui-{run_id}"));
    let failed_downloads = work_dir.join("failed-downloads");
    let completed_downloads = work_dir.join("completed-downloads");
    fs::create_dir_all(&failed_downloads)?;
    fs::create_dir_all(&completed_downloads)?;
    let preserved_target = failed_downloads.join(ATTACHMENT_FILENAME);
    fs::write(&preserved_target, b"preserve the prior destination")?;
    let failed_tree_before = directory_tree_snapshot(&failed_downloads)?;

    let token = format!("notm-attachment-lifetime-ui-{run_id}");
    let mut app =
        FixtureApp::spawn_with_large_attachment(work_dir, &token, LARGE_ATTACHMENT_BYTES)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "id:attachment-heavy-0@fixture.test")?;
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;

    let listed = driver.command("attachment_list_items", json!({}))?;
    let attachments = json_array_at(&listed, &["attachments"])?;
    ensure!(
        attachments.first().is_some_and(|attachment| {
            attachment["filename"] == ATTACHMENT_FILENAME
                && attachment["size"] == LARGE_ATTACHMENT_BYTES
        }),
        "large fixture attachment was not available first: {listed}"
    );

    let armed = driver.command("fail_next_attachment_write", json!({}))?;
    assert_eq!(
        armed["ok"], true,
        "fixture attachment failure did not arm: {armed}"
    );
    let failed = driver.command(
        "save_selected_attachment",
        json!({"index": 0, "dir": failed_downloads}),
    )?;
    assert_eq!(
        failed["pending"], true,
        "injected attachment write did not start asynchronously: {failed}"
    );
    let failed_status = wait_for_attachment_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    ensure!(
        failed_status["last_completion"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("injected attachment write failure")),
        "injected atomic attachment failure was not reported: {failed_status}"
    );
    assert_eq!(
        fs::read(&preserved_target)?,
        b"preserve the prior destination",
        "failed atomic save replaced the prior destination"
    );
    assert_eq!(
        directory_tree_snapshot(&failed_downloads)?,
        failed_tree_before,
        "failed atomic save left a numbered destination or temporary artifact"
    );

    let applied_delay = driver.command(
        "set_fixture_attachment_delay",
        json!({"milliseconds": 1200}),
    )?;
    assert_eq!(applied_delay["milliseconds"], 1200);
    let completed_target = completed_downloads.join(ATTACHMENT_FILENAME);
    let started = driver.command(
        "save_selected_attachment",
        json!({"index": 0, "dir": completed_downloads}),
    )?;
    assert_eq!(
        started["pending"], true,
        "delayed large attachment save did not start: {started}"
    );
    assert_eq!(
        driver.command("attachment_io_status", json!({}))?["busy"],
        true,
        "attachment worker was not active before closing"
    );
    ensure!(
        !completed_target.exists(),
        "atomic destination became visible before the delayed worker completed"
    );

    let closed = driver.command("close_main_window", json!({}))?;
    assert_eq!(closed["ok"], true, "main-window close failed: {closed}");
    drop(driver);
    thread::sleep(Duration::from_millis(250));
    ensure!(
        app.child.try_wait()?.is_none(),
        "application exited while its attachment worker was still pending\n{}",
        app.logs()
    );
    ensure!(
        !completed_target.exists(),
        "attachment destination was exposed before the delayed write completed"
    );

    let status = app.wait_for_exit(STARTUP_TIMEOUT)?;
    ensure!(
        status.success(),
        "application failed while finishing attachment save after close: {status}\n{}",
        app.logs()
    );
    let completed = fs::read(&completed_target)?;
    ensure!(
        completed.len() == LARGE_ATTACHMENT_BYTES && completed.iter().all(|byte| *byte == b'x'),
        "attachment destination did not contain the exact complete fixture payload: bytes={}",
        completed.len()
    );
    let completed_tree = directory_tree_snapshot(&completed_downloads)?;
    ensure!(
        completed_tree.len() == 2
            && completed_tree
                .get(Path::new("."))
                .is_some_and(Option::is_none)
            && completed_tree
                .get(Path::new(ATTACHMENT_FILENAME))
                .is_some_and(|entry| entry.as_deref() == Some(completed.as_slice())),
        "successful atomic attachment save left an unexpected partial or temporary artifact: {completed_tree:?}"
    );

    Ok(())
}

#[test]
fn fixture_attachment_save_chooser_and_private_open_are_deterministic() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_attachment_save_chooser_and_private_open_are_deterministic: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running attachment chooser/private-open UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-attachment-flow-ui-{run_id}"));
    let downloads = work_dir.join("downloads");
    fs::create_dir_all(&downloads)?;
    let selected_target = downloads.join("renamed-download.txt");
    fs::write(&selected_target, b"keep renamed target")?;
    let collision_target = downloads.join("renamed-download (1).txt");
    let cancelled_target = downloads.join("cancelled-download.txt");

    let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let artifacts_attachments = repository_root.join("artifacts/attachments");
    let artifacts_before = directory_tree_snapshot(&artifacts_attachments)?;

    let token = format!("notm-attachment-flow-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "subject:\"Attachment message\"")?;

    let save = driver.command("run_command", json!({"command": ":save_attachment"}))?;
    assert_eq!(save["ok"], true, "save chooser did not open: {save}");
    assert_eq!(
        save["pending"], true,
        "save completed without a chooser: {save}"
    );
    let chooser_id = save["chooser_id"]
        .as_u64()
        .with_context(|| format!("save chooser returned no id: {save}"))?;
    let pending = driver.command("attachment_test_state", json!({}))?;
    assert_eq!(
        pending["save_chooser"]["id"], chooser_id,
        "fixture did not expose the pending chooser: {pending}"
    );
    assert_eq!(
        pending["save_chooser"]["suggested_name"], "note.txt",
        "chooser did not propose the sanitized attachment name: {pending}"
    );
    assert_eq!(
        pending["save_chooser"]["visible"], true,
        "save chooser was pending but not visible: {pending}"
    );
    let open_temp_dir = pending["open_temp_dir"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("fixture did not report its private open directory: {pending}"))?;

    let accepted = driver.command(
        "respond_attachment_save",
        json!({"id": chooser_id, "response": "accept", "path": selected_target}),
    )?;
    assert_eq!(accepted["ok"], true, "chooser accept failed: {accepted}");
    assert_eq!(
        accepted["pending"], true,
        "accepted chooser write was not asynchronous: {accepted}"
    );
    let accepted_status = wait_for_attachment_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        accepted_status["last_completion"]["path"],
        collision_target.display().to_string(),
        "chooser did not honor the renamed full target and collision policy: {accepted_status}"
    );
    assert_eq!(fs::read(&selected_target)?, b"keep renamed target");
    ensure!(
        String::from_utf8_lossy(&fs::read(&collision_target)?).contains("attached text"),
        "accepted chooser target did not receive fixture attachment bytes"
    );

    let before_cancel_logs = driver.command("get_logs", json!({}))?;
    let before_cancel_app_state = driver.command("app_state", json!({}))?;
    let before_cancel_state = driver.command("attachment_test_state", json!({}))?;
    let cancel_save = driver.command("run_command", json!({"command": ":save_attachment"}))?;
    let cancel_id = cancel_save["chooser_id"]
        .as_u64()
        .with_context(|| format!("second save chooser returned no id: {cancel_save}"))?;
    let cancelled = driver.command(
        "respond_attachment_save",
        json!({"id": cancel_id, "response": "cancel", "path": cancelled_target}),
    )?;
    assert_eq!(cancelled["ok"], true, "chooser cancel failed: {cancelled}");
    assert_eq!(
        cancelled["accepted"], false,
        "cancel was reported as accept"
    );
    assert_eq!(cancelled["path"], Value::Null, "cancel returned a path");
    ensure!(
        !cancelled_target.exists(),
        "cancelled chooser unexpectedly wrote {}",
        cancelled_target.display()
    );
    let after_cancel_logs = driver.command("get_logs", json!({}))?;
    let after_cancel_app_state = driver.command("app_state", json!({}))?;
    let after_cancel_state = driver.command("attachment_test_state", json!({}))?;
    assert_eq!(
        after_cancel_logs, before_cancel_logs,
        "cancel changed operation/error state"
    );
    assert_eq!(
        after_cancel_app_state, before_cancel_app_state,
        "cancel changed application state"
    );
    assert_eq!(
        after_cancel_state["status_text"], before_cancel_state["status_text"],
        "cancel changed the visible status"
    );
    assert_eq!(
        after_cancel_state["save_chooser"],
        Value::Null,
        "cancel left a chooser pending: {after_cancel_state}"
    );

    let opened = driver.command("run_command", json!({"command": ":open_attachment"}))?;
    assert_eq!(
        opened["ok"], true,
        "private attachment Open failed: {opened}"
    );
    assert_eq!(
        opened["pending"], true,
        "Open was not asynchronous: {opened}"
    );
    let opened_io = wait_for_attachment_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    let opened_path = opened_io["last_completion"]["path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("Open returned no path: {opened_io}"))?;
    assert_eq!(opened_path.parent(), Some(open_temp_dir.as_path()));
    assert_eq!(
        opened_path.file_name().and_then(|name| name.to_str()),
        Some("note.txt")
    );
    ensure!(
        String::from_utf8_lossy(&fs::read(&opened_path)?).contains("attached text"),
        "private Open file did not contain fixture attachment bytes"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(&open_temp_dir)?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "private Open directory mode was {mode:o}");
    }
    let opened_state = driver.command("attachment_test_state", json!({}))?;
    assert_eq!(
        opened_state["fake_opener"], true,
        "fixture did not use its fake opener: {opened_state}"
    );
    let opener_calls = json_array_at(&opened_state, &["fake_opener_calls"])?;
    ensure!(
        opener_calls.len() == 1 && opener_calls[0] == opened_path.display().to_string(),
        "fixture opener did not receive exactly the private path: {opened_state}"
    );
    assert_eq!(
        directory_tree_snapshot(&artifacts_attachments)?,
        artifacts_before,
        "attachment Open changed artifacts/attachments"
    );

    let closed = driver.command("close_main_window", json!({}))?;
    assert_eq!(closed["ok"], true, "could not close fixture app: {closed}");
    drop(driver);
    let exit_status = app.wait_for_exit(STARTUP_TIMEOUT)?;
    eprintln!("attachment chooser/private-open fixture exited with {exit_status}");
    ensure!(
        exit_status.success(),
        "fixture app exited unsuccessfully with {exit_status}\n{}",
        app.logs()
    );
    ensure!(
        !open_temp_dir.exists(),
        "application exit did not remove {}",
        open_temp_dir.display()
    );

    Ok(())
}

#[test]
fn fixture_malformed_text_shows_a_decode_warning() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_malformed_text_shows_a_decode_warning: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running malformed MIME desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-mime-warning-ui-{run_id}"));
    let downloads = work_dir.join("downloads");
    fs::create_dir_all(&downloads)?;
    let token = format!("notm-mime-warning-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "subject:\"Malformed transfer encoding\"")?;
    let shown = driver.command("show_text_thread", json!({}))?;
    assert_eq!(
        shown["ok"], true,
        "could not show malformed message: {shown}"
    );

    let rendered = driver.command("message_view_text", json!({}))?;
    let text = rendered["text"]
        .as_str()
        .with_context(|| format!("message view response has no text: {rendered}"))?;
    ensure!(
        text.contains("MIME decode warnings:"),
        "malformed text was silently blanked: {rendered}"
    );
    ensure!(
        text.contains("Could not decode text/plain MIME part")
            && text.contains("Base64 decode error"),
        "message view did not explain the MIME failure: {rendered}"
    );

    select_first_thread(&mut driver, "subject:\"HTML with malformed attachment\"")?;
    let listed = driver.command("attachment_list_items", json!({}))?;
    let attachments = json_array_at(&listed, &["attachments"])?;
    ensure!(
        attachments.len() == 1
            && attachments[0]["filename"] == "good.txt"
            && attachments[0]["attachment_index"] == 1,
        "a malformed attachment hid its valid sibling or changed its MIME index: {listed}"
    );

    let saved = driver.command(
        "save_selected_attachment",
        json!({"index": 0, "dir": downloads}),
    )?;
    assert_eq!(
        saved["ok"], true,
        "valid sibling could not be saved: {saved}"
    );
    let saved_status = wait_for_attachment_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    let saved_path = saved_status["last_completion"]["path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("sibling save returned no path: {saved_status}"))?;
    assert_eq!(fs::read(saved_path)?, b"good sibling");

    let text_view = driver.command("show_text_thread", json!({}))?;
    assert_eq!(
        text_view["ok"], true,
        "could not show sibling text: {text_view}"
    );
    let rendered = driver.command("message_view_text", json!({}))?;
    let text = rendered["text"]
        .as_str()
        .with_context(|| format!("sibling message view response has no text: {rendered}"))?;
    ensure!(
        text.contains("broken.bin") && text.contains("decode failed") && text.contains("good.txt"),
        "attachment metadata or failure status was hidden in text view: {rendered}"
    );

    let visual = driver.command("show_visual_html", json!({}))?;
    assert_eq!(visual["ok"], true, "valid HTML could not render: {visual}");
    assert_eq!(
        visual["html_view"]["decode_warning_count"], 1,
        "visual HTML omitted MIME warning state: {visual}"
    );
    let status = visual["html_view"]["status_text"]
        .as_str()
        .with_context(|| format!("visual HTML response has no status: {visual}"))?;
    ensure!(
        status.contains("1 MIME decode warning"),
        "visual HTML status silently hid the MIME warning: {visual}"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn indexed_cid_and_remote_images_follow_message_and_sender_policy() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP indexed_cid_and_remote_images_follow_message_and_sender_policy: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running message/sender remote-image desktop UI smoke with {display}");

    let tracker = LocalHttpTracker::start()?;
    let run_id = unique_run_id()?;
    let test_root = tempfile::Builder::new()
        .prefix("notm-remote-image-ui-")
        .tempdir()?;
    let work_dir = test_root.path().join("app");
    fs::create_dir_all(&work_dir)?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        "[ui]\nremote_images = false\ntrusted_image_senders = []\nshow_keybind_hints = true\n",
    )?;
    let initial_seed_config = fs::read(&config_path)?;

    let token = format!("notm-remote-image-ui-{run_id}");
    let mut app = FixtureApp::spawn_fixture_with_config(work_dir.clone(), &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;

    let fixture_config_path = fixture_app_config_path(&mut driver)?;
    let fixture_database_path = fixture_config_path
        .parent()
        .context("fixture config path had no database parent")?;
    let fixture_notmuch_config = fixture_database_path
        .parent()
        .context("fixture database path had no temporary parent")?
        .join("notmuch-config");
    // Fixture mode deliberately redirects app-configuration writes away from the
    // supplied path. Seed that fixture-database destination so every durable
    // permission mutation remains outside the supplied seed and live config.
    ensure!(
        fixture_config_path != config_path
            && fixture_config_path
                .file_name()
                .and_then(|name| name.to_str())
                == Some(".notm-fixture-config.toml"),
        "fixture app did not redirect mutable settings to its fixture database: seed={}, mutable={}",
        config_path.display(),
        fixture_config_path.display()
    );
    fs::copy(&config_path, &fixture_config_path)?;
    index_related_cid_message(fixture_database_path, &fixture_notmuch_config, &tracker)?;
    index_remote_html_message(
        fixture_database_path,
        &fixture_notmuch_config,
        "remote-image-load-once@fixture.test",
        "Account Security <Shared@Example.Test>",
        "Remote image load once",
        &remote_image_adversarial_html(&tracker),
    )?;
    index_remote_html_message(
        fixture_database_path,
        &fixture_notmuch_config,
        "remote-image-spoofed-peer@fixture.test",
        "ACCOUNT SECURITY <shared@example.test>",
        "Same spoofable From",
        &format!(
            "<html><body><p>Same raw sender identity.</p><img src=\"{}\" alt=\"tracked\"></body></html>",
            tracker.url("/spoofed-peer")
        ),
    )?;
    index_remote_html_message(
        fixture_database_path,
        &fixture_notmuch_config,
        "remote-image-different-sender@fixture.test",
        "Different Sender <different@example.test>",
        "Different sender remote image",
        &format!(
            "<html><body><p>Different sender.</p><img src=\"{}\" alt=\"tracked\"></body></html>",
            tracker.url("/different-sender")
        ),
    )?;
    index_remote_html_message(
        fixture_database_path,
        &fixture_notmuch_config,
        "remote-image-malformed-from@fixture.test",
        "not a valid mailbox ???",
        "Malformed From remote image",
        &format!(
            "<html><body><p>Malformed sender.</p><img src=\"{}\" alt=\"tracked\"></body></html>",
            tracker.url("/malformed-from")
        ),
    )?;
    index_remote_html_message(
        fixture_database_path,
        &fixture_notmuch_config,
        "remote-image-ambiguous-from@fixture.test",
        "Shared <shared@example.test>, Attacker <attacker@example.test>",
        "Ambiguous From remote image",
        &format!(
            "<html><body><p>Ambiguous sender.</p><img src=\"{}\" alt=\"tracked\"></body></html>",
            tracker.url("/ambiguous-from")
        ),
    )?;

    select_first_thread(&mut driver, "id:remote-image-related-cid@fixture.test")?;
    let cid_view = show_visual_html_and_wait(&mut driver, false)?;
    assert_remote_images_blocked(&cid_view)?;
    assert_loaded_image_metrics(&cid_view, 7)?;
    tracker.ensure_stable(&[], Duration::from_millis(250))?;

    select_first_thread(&mut driver, "id:remote-image-load-once@fixture.test")?;
    let blocked = show_visual_html_and_wait(&mut driver, false)?;
    assert_remote_images_blocked(&blocked)?;
    assert_eq!(blocked["html_policy_text"], "Remote images blocked.");
    assert_eq!(blocked["selected_image_sender"], "shared@example.test");
    assert_eq!(blocked["sender_remote_images_allowed"], false);
    assert_eq!(blocked["trusted_image_senders"], json!([]));
    assert_sender_image_warning(&blocked, "shared@example.test")?;
    let compact_geometry = assert_image_menu_state(&blocked, true, false, true)?;
    tracker.ensure_stable(&[], Duration::from_millis(250))?;

    let image_prefix = driver.command("send_key", json!({"key": "i", "modifiers": ["shift"]}))?;
    assert_eq!(
        image_prefix["handled"], true,
        "physical Shift+I did not open the Images shortcut namespace: {image_prefix}"
    );
    ensure!(
        image_prefix["status_text"]
            .as_str()
            .is_some_and(|status| status.contains("m load for this message")
                && status.contains("a always load from this sender")),
        "Images shortcut prompt did not describe both actions: {image_prefix}"
    );
    let opened_menu = driver.command("html_view_state", json!({}))?;
    assert_eq!(
        opened_menu["image_policy_menu_visible"], true,
        "I did not open the fixed Images menu: {opened_menu}"
    );
    tracker.ensure_stable(&[], Duration::from_millis(100))?;
    let escaped = driver.command("send_key", json!({"key": "Escape"}))?;
    assert_eq!(
        escaped["handled"], true,
        "Escape did not cancel the Images shortcut namespace: {escaped}"
    );
    let escaped_view = driver.command("html_view_state", json!({}))?;
    assert_eq!(
        escaped_view["image_policy_menu_visible"], false,
        "Escape left the Images menu open: {escaped_view}"
    );
    assert_eq!(escaped_view["image_permission"], "blocked");
    tracker.ensure_stable(&[], Duration::from_millis(100))?;

    let image_prefix = driver.command("send_key", json!({"key": "i", "modifiers": ["shift"]}))?;
    assert_eq!(image_prefix["handled"], true, "{image_prefix}");
    let loaded = driver.command("send_key", json!({"key": "m"}))?;
    assert_eq!(
        loaded["handled"], true,
        "I m did not load images for the selected message: {loaded}"
    );
    let loaded_view =
        wait_for_html_view_permission(&mut driver, ExpectedImagePermission::MessageOnce, None)?;
    assert_remote_images_once(&loaded_view)?;
    assert_eq!(
        loaded_view["html_policy_text"],
        "Remote images loaded for this message."
    );
    let loaded_geometry = assert_image_menu_state(&loaded_view, false, false, true)?;
    assert_image_menu_geometry_stable(compact_geometry, loaded_geometry, "one-shot loading")?;
    assert_eq!(loaded_view["image_policy_menu_visible"], false);
    assert_eq!(loaded_view["trusted_image_senders"], json!([]));
    tracker.wait_for_requests(&["/load-once"], STARTUP_TIMEOUT)?;
    tracker.ensure_stable(&["/load-once"], Duration::from_millis(250))?;

    for (message_id, context) in [
        (
            "remote-image-spoofed-peer@fixture.test",
            "an as-yet untrusted message with the same case-varied spoofable From",
        ),
        (
            "remote-image-different-sender@fixture.test",
            "a message with a different sender",
        ),
        (
            "remote-image-malformed-from@fixture.test",
            "a malformed From value",
        ),
        (
            "remote-image-ambiguous-from@fixture.test",
            "an ambiguous multi-mailbox From value",
        ),
        (
            "remote-image-load-once@fixture.test",
            "the formerly approved message after navigating away",
        ),
    ] {
        select_first_thread(&mut driver, &format!("id:{message_id}"))?;
        let blocked = show_visual_html_and_wait(&mut driver, false)?;
        assert_remote_images_blocked(&blocked)
            .with_context(|| format!("remote content policy failed for {context}"))?;
        tracker
            .ensure_stable(&["/load-once"], Duration::from_millis(250))
            .with_context(|| format!("remote request escaped through {context}"))?;
    }

    let returned_blocked = driver.command("html_view_state", json!({}))?;
    assert_eq!(returned_blocked["image_permission"], "blocked");
    let returned_geometry = assert_image_menu_state(&returned_blocked, true, false, true)?;
    assert_image_menu_geometry_stable(
        compact_geometry,
        returned_geometry,
        "navigating away from a one-shot load",
    )?;

    let image_prefix = driver.command("send_key", json!({"key": "i", "modifiers": ["shift"]}))?;
    assert_eq!(image_prefix["handled"], true, "{image_prefix}");
    let always = driver.command("send_key", json!({"key": "a"}))?;
    assert_eq!(
        always["handled"], true,
        "I a did not persist sender-scoped image permission: {always}"
    );
    let sender_view =
        wait_for_html_view_permission(&mut driver, ExpectedImagePermission::Sender, None)?;
    assert_eq!(
        sender_view["image_policy_menu_visible"], false,
        "the Images menu did not close after trusting the sender: {sender_view}"
    );
    assert_eq!(
        sender_view["html_policy_text"],
        "Remote images load automatically for this sender."
    );
    assert_eq!(sender_view["global_remote_images_allowed"], false);
    assert_eq!(sender_view["sender_remote_images_allowed"], true);
    assert_eq!(
        sender_view["trusted_image_senders"],
        json!(["shared@example.test"])
    );
    assert_eq!(sender_view["sender_identity_authenticated"], false);
    assert_sender_image_warning(&sender_view, "shared@example.test")?;
    let sender_geometry = assert_image_menu_state(&sender_view, false, true, true)?;
    assert_image_menu_geometry_stable(
        compact_geometry,
        sender_geometry,
        "enabling sender-scoped loading",
    )?;
    tracker.wait_for_requests(&["/load-once", "/load-once"], STARTUP_TIMEOUT)?;

    let persisted_sender_bytes = fs::read(&fixture_config_path)?;
    let persisted_sender: toml::Value =
        String::from_utf8(persisted_sender_bytes.clone())?.parse()?;
    assert_eq!(
        persisted_sender["ui"]["remote_images"].as_bool(),
        Some(false),
        "sender trust unexpectedly enabled global remote images: {persisted_sender}"
    );
    assert_eq!(
        persisted_sender["ui"]["trusted_image_senders"]
            .as_array()
            .map(|values| values
                .iter()
                .filter_map(toml::Value::as_str)
                .collect::<Vec<_>>()),
        Some(vec!["shared@example.test"]),
        "sender trust was not persisted as one exact normalized mailbox: {persisted_sender}"
    );
    assert_eq!(
        fs::read(&config_path)?,
        initial_seed_config,
        "fixture sender permission mutated the supplied seed config instead of its isolated writable config"
    );

    select_first_thread(&mut driver, "id:remote-image-spoofed-peer@fixture.test")?;
    let spoofed =
        ensure_visual_html_and_wait_permission(&mut driver, ExpectedImagePermission::Sender, None)?;
    assert_eq!(
        spoofed["selected_image_sender"], "shared@example.test",
        "case/display-name normalization did not resolve the exact trusted mailbox: {spoofed}"
    );
    assert_eq!(spoofed["sender_identity_authenticated"], false);
    assert_sender_image_warning(&spoofed, "shared@example.test")?;
    let spoofed_geometry = assert_image_menu_state(&spoofed, false, true, true)?;
    assert_image_menu_geometry_stable(
        compact_geometry,
        spoofed_geometry,
        "rendering mail that merely claims the trusted From address",
    )?;
    tracker.wait_for_requests(
        &["/load-once", "/load-once", "/spoofed-peer"],
        STARTUP_TIMEOUT,
    )?;

    for (message_id, sender_sensitive, context) in [
        (
            "remote-image-different-sender@fixture.test",
            true,
            "a different sender",
        ),
        (
            "remote-image-malformed-from@fixture.test",
            false,
            "a malformed From value",
        ),
        (
            "remote-image-ambiguous-from@fixture.test",
            false,
            "an ambiguous multi-mailbox From value",
        ),
    ] {
        select_first_thread(&mut driver, &format!("id:{message_id}"))?;
        let unrelated = ensure_visual_html_and_wait_permission(
            &mut driver,
            ExpectedImagePermission::Blocked,
            None,
        )?;
        assert_remote_images_blocked(&unrelated)?;
        assert_eq!(unrelated["sender_remote_images_allowed"], false);
        let geometry = assert_image_menu_state(&unrelated, true, false, sender_sensitive)?;
        assert_image_menu_geometry_stable(compact_geometry, geometry, context)?;
        if !sender_sensitive {
            assert_eq!(unrelated["selected_image_sender"], Value::Null);
            ensure!(
                unrelated["sender_image_warning_text"].as_str().is_some_and(
                    |warning| warning.contains("does not contain exactly one valid address")
                ),
                "invalid From warning did not explain why sender trust is unavailable: {unrelated}"
            );
            let prefix = driver.command("send_key", json!({"key": "i", "modifiers": ["shift"]}))?;
            assert_eq!(prefix["handled"], true, "{prefix}");
            let rejected = driver.command("send_key", json!({"key": "a"}))?;
            assert_eq!(
                rejected["handled"], true,
                "I a was not consumed for invalid From: {rejected}"
            );
            ensure!(
                rejected["status_text"]
                    .as_str()
                    .is_some_and(|status| status.contains("exactly one valid address")),
                "invalid From sender action returned no clear error: {rejected}"
            );
        }
        tracker.ensure_stable(
            &["/load-once", "/load-once", "/spoofed-peer"],
            Duration::from_millis(250),
        )?;
    }

    select_first_thread(&mut driver, "id:remote-image-load-once@fixture.test")?;
    let trusted_original =
        ensure_visual_html_and_wait_permission(&mut driver, ExpectedImagePermission::Sender, None)?;
    tracker.wait_for_requests(
        &["/load-once", "/load-once", "/spoofed-peer", "/load-once"],
        STARTUP_TIMEOUT,
    )?;
    let trusted_generation = html_load_generation(&trusted_original)?;
    let prefix = driver.command("send_key", json!({"key": "i", "modifiers": ["shift"]}))?;
    assert_eq!(prefix["handled"], true, "{prefix}");
    let revoked = driver.command("send_key", json!({"key": "a"}))?;
    assert_eq!(
        revoked["handled"], true,
        "I a could not revoke sender-scoped image permission: {revoked}"
    );
    let revoked_view = wait_for_html_view_permission(
        &mut driver,
        ExpectedImagePermission::Blocked,
        Some(trusted_generation),
    )?;
    assert_remote_images_blocked(&revoked_view)?;
    assert_eq!(revoked_view["trusted_image_senders"], json!([]));
    let revoked_geometry = assert_image_menu_state(&revoked_view, true, false, true)?;
    assert_image_menu_geometry_stable(
        compact_geometry,
        revoked_geometry,
        "revoking sender-scoped loading",
    )?;
    tracker.ensure_stable(
        &["/load-once", "/load-once", "/spoofed-peer", "/load-once"],
        Duration::from_millis(500),
    )?;
    let revoked_config_bytes = fs::read(&fixture_config_path)?;
    let revoked_config: toml::Value = String::from_utf8(revoked_config_bytes.clone())?.parse()?;
    assert_eq!(revoked_config["ui"]["remote_images"].as_bool(), Some(false));
    assert_eq!(
        revoked_config["ui"]["trusted_image_senders"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "sender revocation was not persisted exactly: {revoked_config}"
    );

    fs::write(&fixture_config_path, "[ui\nremote_images = false\n")?;
    let rejected_generation = html_load_generation(&revoked_view)?;
    let rejected = driver.command("trust_sender_images", json!({}))?;
    assert_eq!(
        rejected["ok"], false,
        "sender trust reported success after its atomic persistence failed: {rejected}"
    );
    ensure!(
        rejected["last_error"].as_str().is_some(),
        "failed sender trust returned no persistence error: {rejected}"
    );
    assert_eq!(
        rejected["html_view"]["sender_image_trust_active"], false,
        "failed sender trust did not roll the checkbox back: {rejected}"
    );
    assert_eq!(
        rejected["html_view"]["sender_remote_images_allowed"], false,
        "failed sender trust changed runtime policy: {rejected}"
    );
    assert_eq!(
        html_load_generation(&rejected["html_view"])?,
        rejected_generation,
        "failed sender trust unexpectedly re-rendered the WebView"
    );
    tracker.ensure_stable(
        &["/load-once", "/load-once", "/spoofed-peer", "/load-once"],
        Duration::from_millis(250),
    )?;
    fs::write(&fixture_config_path, &revoked_config_bytes)?;

    let prefix = driver.command("send_key", json!({"key": "i", "modifiers": ["shift"]}))?;
    assert_eq!(prefix["handled"], true, "{prefix}");
    let reenabled = driver.command("send_key", json!({"key": "a"}))?;
    assert_eq!(reenabled["handled"], true, "{reenabled}");
    let reenabled_view =
        wait_for_html_view_permission(&mut driver, ExpectedImagePermission::Sender, None)?;
    tracker.wait_for_requests(
        &[
            "/load-once",
            "/load-once",
            "/spoofed-peer",
            "/load-once",
            "/load-once",
        ],
        STARTUP_TIMEOUT,
    )?;
    let reenabled_geometry = assert_image_menu_state(&reenabled_view, false, true, true)?;
    assert_image_menu_geometry_stable(
        compact_geometry,
        reenabled_geometry,
        "re-enabling sender-scoped loading",
    )?;

    let persisted_bytes = fs::read(&fixture_config_path)?;
    let persisted: toml::Value = String::from_utf8(persisted_bytes.clone())?.parse()?;
    assert_eq!(persisted["ui"]["remote_images"].as_bool(), Some(false));
    assert_eq!(
        persisted["ui"]["trusted_image_senders"][0].as_str(),
        Some("shared@example.test")
    );

    let lowercase_insert = driver.command("send_key", json!({"key": "i"}))?;
    assert_eq!(
        lowercase_insert["handled"], true,
        "plain lowercase i no longer enters Insert mode: {lowercase_insert}"
    );
    assert_eq!(
        lowercase_insert["input_mode"], "Insert",
        "plain lowercase i was captured by the Images namespace: {lowercase_insert}"
    );

    // The fixture database is intentionally process-scoped. Carry only the
    // persisted app preference into a fresh fixture process for the restart
    // assertion below; no fixture mail or Notmuch state escapes.
    fs::write(&config_path, persisted_bytes)?;

    let closed = driver.command("close_main_window", json!({}))?;
    assert_eq!(closed["ok"], true, "could not close first app: {closed}");
    drop(driver);
    let status = app.wait_for_exit(STARTUP_TIMEOUT)?;
    ensure!(
        status.success(),
        "first app did not exit cleanly: {status}\n{}",
        app.logs()
    );
    app.preserve_work_dir_on_drop();
    drop(app);
    prepare_fixture_work_dir_for_restart(&work_dir)?;

    let restart_token = format!("{token}-restart");
    let mut restarted =
        FixtureApp::spawn_fixture_with_config(work_dir, &restart_token, &config_path)?;
    let mut restarted_driver = restarted.connect(&restart_token)?;
    restarted_driver.wait_for_search(STARTUP_TIMEOUT)?;
    let restarted_config_path = fixture_app_config_path(&mut restarted_driver)?;
    let restarted_database_path = restarted_config_path
        .parent()
        .context("restarted fixture config path had no database parent")?;
    let restarted_notmuch_config = restarted_database_path
        .parent()
        .context("restarted fixture database path had no temporary parent")?
        .join("notmuch-config");
    index_remote_html_message(
        restarted_database_path,
        &restarted_notmuch_config,
        "remote-image-spoofed-peer@fixture.test",
        "ACCOUNT SECURITY <shared@example.test>",
        "Same spoofable From",
        &format!(
            "<html><body><p>Same raw sender identity.</p><img src=\"{}\" alt=\"tracked\"></body></html>",
            tracker.url("/spoofed-peer")
        ),
    )?;
    select_first_thread(
        &mut restarted_driver,
        "id:remote-image-spoofed-peer@fixture.test",
    )?;
    let mut restarted = restarted_driver.command("html_view_state", json!({}))?;
    if restarted["html_visible"] != true {
        restarted = restarted_driver.command("show_visual_html", json!({}))?;
        restarted = restarted["html_view"].clone();
    }
    let restarted_view = wait_for_html_view_permission_with_initial(
        &mut restarted_driver,
        ExpectedImagePermission::Sender,
        Some(&restarted),
        None,
    )?;
    assert_eq!(
        restarted_view["image_permission"], "sender",
        "sender-scoped remote-image permission did not survive restart: {restarted_view}"
    );
    assert_eq!(restarted_view["global_remote_images_allowed"], false);
    assert_eq!(
        restarted_view["trusted_image_senders"],
        json!(["shared@example.test"])
    );
    assert_eq!(restarted_view["sender_identity_authenticated"], false);
    assert_sender_image_warning(&restarted_view, "shared@example.test")?;
    tracker.wait_for_requests(
        &[
            "/load-once",
            "/load-once",
            "/spoofed-peer",
            "/load-once",
            "/load-once",
            "/spoofed-peer",
        ],
        STARTUP_TIMEOUT,
    )?;

    Ok(())
}

#[cfg(unix)]
#[test]
fn sender_image_revocation_refreshes_main_and_two_standalone_windows() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP sender_image_revocation_refreshes_main_and_two_standalone_windows: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running main/standalone sender-image revocation UI smoke with {display}");

    let tracker = LocalHttpTracker::start()?;
    let run_id = unique_run_id()?;
    let test_root = tempfile::Builder::new()
        .prefix("notm-standalone-sender-image-ui-")
        .tempdir()?;
    let work_dir = test_root.path().join("app");
    fs::create_dir_all(&work_dir)?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        "[ui]\nremote_images = false\ntrusted_image_senders = [\"policy@example.test\"]\nshow_keybind_hints = true\n",
    )?;
    let seed_config = fs::read(&config_path)?;

    let token = format!("notm-standalone-sender-image-ui-{run_id}");
    let mut app = FixtureApp::spawn_fixture_with_config(work_dir.clone(), &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;

    let initial_settings = driver.command("settings_test_state", json!({}))?;
    assert_eq!(
        initial_settings["remote_images"], false,
        "isolated fixture unexpectedly started with global remote images enabled: {initial_settings}"
    );
    let initial_sender_policy = driver.command("trusted_image_senders", json!({}))?;
    assert_eq!(
        initial_sender_policy["trusted_image_senders"],
        json!(["policy@example.test"]),
        "fixture did not load the exact sender permission from its supplied seed config: {initial_sender_policy}"
    );
    let fixture_config_path = initial_settings["app_config_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("fixture did not expose its config path: {initial_settings}"))?;
    ensure!(
        fixture_config_path != config_path
            && fixture_config_path
                .file_name()
                .and_then(|name| name.to_str())
                == Some(".notm-fixture-config.toml"),
        "standalone fixture did not isolate mutable config: seed={}, mutable={}",
        config_path.display(),
        fixture_config_path.display()
    );
    // Fixture mode loads the supplied seed but redirects every later settings
    // write into the disposable database. Seed that destination so sender
    // trust mutations preserve the rest of the isolated configuration.
    fs::copy(&config_path, &fixture_config_path)?;
    let fixture_database_path = fixture_config_path
        .parent()
        .context("fixture config path had no database parent")?;
    let fixture_notmuch_config = fixture_database_path
        .parent()
        .context("fixture database path had no temporary parent")?
        .join("notmuch-config");
    index_standalone_remote_policy_thread(
        fixture_database_path,
        &fixture_notmuch_config,
        &tracker,
    )?;
    index_remote_html_message(
        fixture_database_path,
        &fixture_notmuch_config,
        "standalone-policy-unrelated-main@fixture.test",
        "Unrelated Main <unrelated-main@example.test>",
        "Unrelated main one-shot images",
        &format!(
            "<html><body><p>Unrelated main message.</p><img src=\"{}\" alt=\"tracked\"></body></html>",
            tracker.url("/unrelated-main-once")
        ),
    )?;

    select_first_thread(&mut driver, "id:standalone-policy-html-root@fixture.test")?;
    let selected = driver.command("select_message_by_index", json!({"index": 0}))?;
    assert_eq!(
        selected["ok"], true,
        "could not select standalone fixture HTML root in main view: {selected}"
    );
    let hidden = driver.command(
        "set_pane_visibility",
        json!({"pane": "message", "visible": false}),
    )?;
    assert_eq!(
        hidden["ok"], true,
        "message pane could not be hidden before opening standalone windows: {hidden}"
    );
    for expected_count in 1..=2 {
        let opened = driver.command("open_selected_thread", json!({}))?;
        assert_eq!(
            opened["ok"], true,
            "standalone fixture window {expected_count} did not open: {opened}"
        );
        wait_for_standalone_window_count(&mut driver, expected_count)?;
        let sender_policy = driver.command("trusted_image_senders", json!({}))?;
        assert_eq!(
            sender_policy["trusted_image_senders"],
            json!(["policy@example.test"]),
            "opening standalone window {expected_count} changed sender trust: {sender_policy}"
        );
    }
    for window_index in 0..2 {
        let selected = driver.command(
            "standalone_select_message",
            json!({"window_index": window_index, "message_index": 0}),
        )?;
        assert_eq!(
            selected["ok"], true,
            "standalone window {window_index} could not select the HTML root: {selected}"
        );
        assert_eq!(
            selected["window"]["selected_message"]["message_id"],
            "standalone-policy-html-root@fixture.test",
            "standalone window {window_index} selected the wrong message: {selected}"
        );
        assert_eq!(
            selected["window"]["view"], "text",
            "standalone root loaded HTML before the explicit view action: {selected}"
        );
    }
    tracker.ensure_stable(&[], Duration::from_millis(250))?;

    let shown = driver.command(
        "set_pane_visibility",
        json!({"pane": "message", "visible": true}),
    )?;
    assert_eq!(
        shown["ok"], true,
        "message pane could not be restored beside standalone windows: {shown}"
    );
    let selected = driver.command("select_message_by_index", json!({"index": 0}))?;
    assert_eq!(selected["ok"], true, "{selected}");
    let sender_policy = driver.command("trusted_image_senders", json!({}))?;
    assert_eq!(
        sender_policy["trusted_image_senders"],
        json!(["policy@example.test"]),
        "restoring the main message pane changed sender trust: {sender_policy}"
    );
    let main_allowed =
        ensure_visual_html_and_wait_permission(&mut driver, ExpectedImagePermission::Sender, None)?;
    assert_eq!(main_allowed["selected_image_sender"], "policy@example.test");
    assert_eq!(main_allowed["global_remote_images_allowed"], false);
    assert_sender_image_warning(&main_allowed, "policy@example.test")?;
    let main_allowed_geometry = assert_image_menu_state(&main_allowed, false, true, true)?;
    tracker.wait_for_requests(&["/standalone-policy"], STARTUP_TIMEOUT)?;

    let opened_windows = wait_for_standalone_window_count(&mut driver, 2)?;
    for (window_index, window) in opened_windows.iter().enumerate() {
        assert_eq!(
            window["selected_message"]["message_id"], "standalone-policy-html-root@fixture.test",
            "standalone window {window_index} did not retain the selected HTML root: {window}"
        );
        let shown = driver.command(
            "standalone_show_visual_html",
            json!({"window_index": window_index}),
        )?;
        assert_eq!(
            shown["ok"], true,
            "standalone window {window_index} could not show Visual HTML: {shown}"
        );
    }

    let expected_allowed_requests = [
        "/standalone-policy",
        "/standalone-policy",
        "/standalone-policy",
    ];
    tracker.wait_for_requests(&expected_allowed_requests, STARTUP_TIMEOUT)?;
    let allowed =
        wait_for_standalone_remote_policy(&mut driver, ExpectedImagePermission::Sender, 2, None)?;
    let allowed_generations = standalone_html_generations(&allowed)?;
    let mut allowed_geometries = Vec::new();
    for (window_index, window) in allowed.iter().enumerate() {
        assert_eq!(
            window["selected_image_sender"], "policy@example.test",
            "window {window_index}: {window}"
        );
        assert_eq!(window["global_remote_images_allowed"], false);
        assert_sender_image_warning(window, "policy@example.test")?;
        allowed_geometries.push(assert_image_menu_state(window, false, true, true)?);
    }
    tracker.ensure_stable(&expected_allowed_requests, Duration::from_millis(250))?;

    select_first_thread(
        &mut driver,
        "id:standalone-policy-unrelated-main@fixture.test",
    )?;
    let unrelated_blocked =
        show_visual_html_and_wait_permission(&mut driver, ExpectedImagePermission::Blocked, None)?;
    assert_eq!(
        unrelated_blocked["selected_image_sender"],
        "unrelated-main@example.test"
    );
    assert_remote_images_blocked(&unrelated_blocked)?;
    let unrelated_blocked_generation = html_load_generation(&unrelated_blocked)?;
    tracker.ensure_stable(&expected_allowed_requests, Duration::from_millis(250))?;

    let loaded_once = driver.command("load_images_once", json!({}))?;
    assert_eq!(
        loaded_once["ok"], true,
        "unrelated main message could not load images once: {loaded_once}"
    );
    let unrelated_once = wait_for_html_view_permission(
        &mut driver,
        ExpectedImagePermission::MessageOnce,
        Some(unrelated_blocked_generation),
    )?;
    assert_remote_images_once(&unrelated_once)?;
    let unrelated_once_generation = html_load_generation(&unrelated_once)?;
    let requests_with_unrelated_once = [
        "/standalone-policy",
        "/standalone-policy",
        "/standalone-policy",
        "/unrelated-main-once",
    ];
    tracker.wait_for_requests(&requests_with_unrelated_once, STARTUP_TIMEOUT)?;
    tracker.ensure_stable(&requests_with_unrelated_once, Duration::from_millis(250))?;

    let revoked = driver.command(
        "standalone_image_policy",
        json!({"window_index": 0, "action": "sender_off"}),
    )?;
    assert_eq!(
        revoked["ok"], true,
        "standalone Images menu could not revoke sender trust: {revoked}"
    );
    assert_eq!(
        revoked["window"]["image_policy_menu_visible"], false,
        "standalone Images menu remained open after revocation: {revoked}"
    );

    let unrelated_after_revocation = driver.command("html_view_state", json!({}))?;
    assert_eq!(
        unrelated_after_revocation["selected_image_sender"],
        "unrelated-main@example.test"
    );
    assert_eq!(
        unrelated_after_revocation["image_permission"], "message_once",
        "revoking an unrelated standalone sender changed the main message's one-shot permission: {unrelated_after_revocation}"
    );
    assert_eq!(
        unrelated_after_revocation["image_loading_allowed"], true,
        "revoking an unrelated standalone sender disabled main WebKit image loading: {unrelated_after_revocation}"
    );
    assert_eq!(
        html_load_generation(&unrelated_after_revocation)?,
        unrelated_once_generation,
        "revoking an unrelated standalone sender re-rendered the main WebView"
    );
    assert_eq!(
        unrelated_after_revocation["trusted_image_senders"],
        json!([])
    );

    let blocked = wait_for_standalone_remote_policy(
        &mut driver,
        ExpectedImagePermission::Blocked,
        2,
        Some(&allowed_generations),
    )?;
    let blocked_generations = standalone_html_generations(&blocked)?;
    for (window_index, window) in blocked.iter().enumerate() {
        assert_eq!(window["sender_remote_images_allowed"], false);
        assert_eq!(window["html_policy_text"], "Remote images blocked.");
        assert_image_menu_geometry_stable(
            allowed_geometries[window_index],
            assert_image_menu_state(window, true, false, true)?,
            "revoking sender trust across standalone windows",
        )?;
    }
    tracker.ensure_stable(&requests_with_unrelated_once, Duration::from_millis(500))?;

    let revoked_config_bytes = fs::read(&fixture_config_path)?;
    let revoked_config: toml::Value = String::from_utf8(revoked_config_bytes.clone())?.parse()?;
    assert_eq!(revoked_config["ui"]["remote_images"].as_bool(), Some(false));
    assert_eq!(
        revoked_config["ui"]["trusted_image_senders"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "standalone revocation was not persisted exactly: {revoked_config}"
    );
    assert_eq!(
        fs::read(&config_path)?,
        seed_config,
        "standalone sender revocation mutated the supplied seed config"
    );

    select_first_thread(&mut driver, "id:standalone-policy-html-root@fixture.test")?;
    let selected = driver.command("select_message_by_index", json!({"index": 0}))?;
    assert_eq!(
        selected["ok"], true,
        "could not restore the policy sender in the main view: {selected}"
    );
    let main_blocked = ensure_visual_html_and_wait_permission(
        &mut driver,
        ExpectedImagePermission::Blocked,
        Some(unrelated_once_generation),
    )?;
    assert_remote_images_blocked(&main_blocked)?;
    assert_image_menu_geometry_stable(
        main_allowed_geometry,
        assert_image_menu_state(&main_blocked, true, false, true)?,
        "showing the revoked sender again in the main window",
    )?;
    tracker.ensure_stable(&requests_with_unrelated_once, Duration::from_millis(250))?;

    let reenabled = driver.command("trust_sender_images", json!({}))?;
    assert_eq!(
        reenabled["ok"], true,
        "main Images control could not restore sender trust: {reenabled}"
    );
    assert_eq!(
        reenabled["trusted_image_senders"],
        json!(["policy@example.test"]),
        "main Images control reported success without restoring exact sender trust: {reenabled}"
    );
    assert_eq!(
        reenabled["html_view"]["sender_remote_images_allowed"], true,
        "main Images control reported success before applying sender trust: {reenabled}"
    );
    let main_reenabled = wait_for_html_view_permission(
        &mut driver,
        ExpectedImagePermission::Sender,
        Some(html_load_generation(&main_blocked)?),
    )?;
    let allowed_again = wait_for_standalone_remote_policy(
        &mut driver,
        ExpectedImagePermission::Sender,
        2,
        Some(&blocked_generations),
    )?;
    let allowed_again_generations = standalone_html_generations(&allowed_again)?;
    let repeated_requests = [
        "/standalone-policy",
        "/standalone-policy",
        "/standalone-policy",
        "/unrelated-main-once",
        "/standalone-policy",
        "/standalone-policy",
        "/standalone-policy",
    ];
    tracker.wait_for_requests(&repeated_requests, STARTUP_TIMEOUT)?;

    let valid_fixture_config = fs::read(&fixture_config_path)?;
    fs::write(&fixture_config_path, "[ui\nremote_images = false\n")?;
    let rejected = driver.command(
        "standalone_image_policy",
        json!({"window_index": 1, "action": "sender_off"}),
    )?;
    assert_eq!(
        rejected["ok"], false,
        "standalone sender revocation reported success after persistence failed: {rejected}"
    );
    ensure!(
        rejected["error"].as_str().is_some(),
        "failed standalone sender revocation returned no error: {rejected}"
    );
    assert_eq!(
        rejected["window"]["sender_image_trust_active"], true,
        "failed standalone revocation did not roll its checkbox back: {rejected}"
    );
    assert_eq!(
        rejected["window"]["image_policy_menu_visible"], false,
        "failed standalone revocation left its menu open: {rejected}"
    );

    let unchanged_main = driver.command("html_view_state", json!({}))?;
    assert_eq!(unchanged_main["image_permission"], "sender");
    assert_eq!(
        html_load_generation(&unchanged_main)?,
        html_load_generation(&main_reenabled)?,
        "failed standalone revocation unexpectedly re-rendered the main WebView"
    );
    let unchanged = driver.command("standalone_message_windows", json!({}))?;
    let unchanged_windows = json_array_at(&unchanged, &["windows"])?;
    ensure!(
        unchanged_windows.iter().all(|window| {
            window["image_permission"] == "sender"
                && window["sender_image_trust_active"] == true
                && window["image_policy_menu_visible"] == false
        }),
        "failed standalone revocation did not roll every sender control back: {unchanged}"
    );
    assert_eq!(
        standalone_html_generations(unchanged_windows)?,
        allowed_again_generations,
        "failed standalone revocation unexpectedly re-rendered an open WebView"
    );
    tracker.ensure_stable(&repeated_requests, Duration::from_millis(300))?;
    fs::write(&fixture_config_path, valid_fixture_config)?;

    Ok(())
}

#[cfg(unix)]
#[test]
fn oversized_thread_rejection_restores_selection_before_tagging() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP oversized_thread_rejection_restores_selection_before_tagging: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running oversized-thread selection rollback smoke with {display}");

    const OVERSIZED_MESSAGE_COUNT: usize = notm_ui::model::MAX_LOADED_THREAD_MESSAGES + 1;

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-oversized-select-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;

    let query_tag = format!("oversized-select-{run_id}");
    let mutation_tag = format!("after-rejected-selection-{run_id}");
    let safe_message_id = format!("oversized-safe-{run_id}@fixture.test");
    let oversized_root_id = format!("oversized-root-{run_id}@fixture.test");
    let oversized_subject = "Oversized selection rejection";
    {
        let db = fixture.open_readwrite()?;
        let safe_path = fixture
            .maildir
            .join("cur")
            .join(format!("oversized-safe-{run_id}:2,S"));
        fs::write(
            &safe_path,
            format!(
                "From: Safe Sender <safe@example.test>\r\n\
                 To: Fixture User <fixture@example.test>\r\n\
                 Subject: Safe selection target\r\n\
                 Date: Tue, 25 Aug 2030 13:00:00 -0600\r\n\
                 Message-ID: <{safe_message_id}>\r\n\
                 MIME-Version: 1.0\r\n\
                 Content-Type: text/plain; charset=utf-8\r\n\r\n\
                 Safe selection body.\r\n"
            ),
        )?;
        db.index_file_with_tags(&safe_path, &["inbox", &query_tag])?;

        for index in 0..OVERSIZED_MESSAGE_COUNT {
            let message_id = if index == 0 {
                oversized_root_id.clone()
            } else {
                format!("oversized-reply-{index}-{run_id}@fixture.test")
            };
            let reply_headers = if index == 0 {
                String::new()
            } else {
                format!(
                    "In-Reply-To: <{oversized_root_id}>\r\n\
                     References: <{oversized_root_id}>\r\n"
                )
            };
            let path = fixture
                .maildir
                .join("cur")
                .join(format!("oversized-{run_id}-{index:04}:2,S"));
            fs::write(
                &path,
                format!(
                    "From: Oversized Sender <oversized@example.test>\r\n\
                     To: Fixture User <fixture@example.test>\r\n\
                     Subject: {}{oversized_subject}\r\n\
                     Date: Tue, 25 Aug 2026 12:00:00 -0600\r\n\
                     Message-ID: <{message_id}>\r\n\
                     {reply_headers}\
                     MIME-Version: 1.0\r\n\
                     Content-Type: text/plain; charset=utf-8\r\n\r\n\
                     Oversized message {index}.\r\n",
                    if index == 0 { "" } else { "Re: " },
                ),
            )?;
            db.index_file_with_tags(&path, &["inbox", &query_tag])?;
        }
    }

    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = {}\nexcluded_tags = []\n\
             \n[identity]\nname = \"Fixture User\"\nprimary_email = \"fixture@example.test\"\n\
             \n[send]\nenabled = false\n\
             \n[sync]\nenabled = false\n\
             \n[automation]\nallow_live_tag_test = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml::Value::String(format!("tag:{query_tag}")),
        ),
    )?;

    let token = format!("notm-oversized-select-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir, &token, &config_path)?;
    let mut driver = app.connect_with_command_timeout(&token, LARGE_THREAD_COMMAND_TIMEOUT)?;
    let search = driver.wait_for_search(STARTUP_TIMEOUT)?;
    let rows = json_array_at(&search, &["state", "thread_list_items"])?;
    ensure!(
        rows.len() == 2,
        "expected safe and oversized threads: {search}"
    );
    let safe_index = rows
        .iter()
        .position(|thread| thread["subject"] == "Safe selection target")
        .with_context(|| format!("safe thread was not present: {search}"))?;
    let safe_thread_id = rows[safe_index]["thread_id"]
        .as_str()
        .with_context(|| format!("safe thread had no ID: {search}"))?
        .to_string();
    let oversized_index = rows
        .iter()
        .position(|thread| thread["subject"] == oversized_subject)
        .with_context(|| format!("oversized thread was not present: {search}"))?;

    let selected = driver.command("select_thread_by_index", json!({"index": safe_index}))?;
    assert_eq!(
        selected["selected_thread"]["thread_id"], safe_thread_id,
        "safe thread was not selected before rejection: {selected}"
    );
    wait_for_thread_load_idle(&mut driver, LARGE_THREAD_COMMAND_TIMEOUT)?;

    let rejected = driver.command("select_thread_by_index", json!({"index": oversized_index}))?;
    assert_eq!(
        rejected["ok"], true,
        "oversized selection was not scheduled: {rejected}"
    );
    wait_for_thread_load_idle(&mut driver, LARGE_THREAD_COMMAND_TIMEOUT)?;
    let rejected = driver.command("app_state", json!({}))?;
    assert_eq!(
        rejected["state"]["selected_thread"]["thread_id"], safe_thread_id,
        "rejected oversized selection changed the state target: {rejected}"
    );
    let selection = driver.command("thread_selection_view_state", json!({}))?;
    assert_eq!(
        selection["selected_local"], safe_index,
        "GTK selection did not roll back with state: {selection}"
    );
    ensure!(
        rejected["state"]["last_error"]
            .as_str()
            .is_some_and(|error| {
                error.contains(&format!("contains {OVERSIZED_MESSAGE_COUNT} message(s)"))
                    && error.contains(&format!(
                        "safety limit of {}",
                        notm_ui::model::MAX_LOADED_THREAD_MESSAGES
                    ))
                    && error.contains("no partial thread was loaded")
            }),
        "oversized rejection was not surfaced: {rejected}"
    );

    let tagged = driver.command("tag_selected", json!({"add": [&mutation_tag]}))?;
    assert_eq!(
        tagged["ok"], true,
        "tag action after oversized rejection failed: {tagged}"
    );
    assert_eq!(
        tagged["pending"], true,
        "tag action after oversized rejection was not scheduled asynchronously: {tagged}"
    );
    let tagged = wait_for_tag(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        tagged["state"]["selected_thread"]["thread_id"], safe_thread_id,
        "post-rejection tag action no longer targeted the restored row: {tagged}"
    );

    let db = fixture.open_readonly()?;
    let query_options = notm_notmuch::QueryOptions {
        excluded_tags: Vec::new(),
        ..notm_notmuch::QueryOptions::default()
    };
    assert_eq!(
        db.count_messages(&format!("tag:{mutation_tag}"), &query_options)?,
        1,
        "post-rejection tag leaked onto the oversized thread"
    );
    assert_eq!(
        db.count_messages(
            &format!("id:{safe_message_id} and tag:{mutation_tag}"),
            &query_options,
        )?,
        1,
        "restored safe thread did not receive the tag"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn isolated_message_io_mime_survives_missing_copy_limits_and_restart() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP isolated_message_io_mime_survives_missing_copy_limits_and_restart: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running isolated message-I/O and MIME restart smoke with {display}");

    const MESSAGE_COUNT: usize = 1_001;
    const NESTING_DEPTH: usize = 80;

    const _: () = {
        assert!(notm_ui::model::MAX_THREAD_DETAIL_MESSAGES < MESSAGE_COUNT);
        assert!(MESSAGE_COUNT <= notm_ui::model::MAX_LOADED_THREAD_MESSAGES);
        assert!(
            LARGE_THREAD_COMMAND_TIMEOUT.as_secs()
                < notm_ui::automation::TEST_HARNESS_RESPONSE_TIMEOUT.as_secs()
        );
    };

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-message-io-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;

    let root_message_id = format!("message-io-root-{run_id}@fixture.test");
    let malformed_message_id = format!("message-io-malformed-{run_id}@fixture.test");
    let newest_message_id = format!("message-io-reply-1000-{run_id}@fixture.test");
    let subject = "Message I/O robustness thread";
    let root_bytes = message_io_attachment_message(&root_message_id, subject);
    let first_copy = fixture
        .maildir
        .join("cur")
        .join(format!("message-io-{run_id}-a:2,"));
    let second_copy = fixture
        .maildir
        .join("cur")
        .join(format!("message-io-{run_id}-b:2,"));
    fs::write(&first_copy, &root_bytes)?;
    fs::hard_link(&first_copy, &second_copy)?;

    let base_date = chrono::DateTime::parse_from_rfc3339("2026-06-18T20:00:00-06:00")?;
    {
        let db = fixture.open_readwrite()?;
        db.index_file_with_tags(&first_copy, &["inbox", "message-io-e2e"])?;
        db.index_file_with_tags(&second_copy, &["inbox", "message-io-e2e"])?;

        let malformed_path = fixture
            .maildir
            .join("cur")
            .join(format!("message-io-{run_id}-malformed:2,"));
        fs::write(
            &malformed_path,
            message_io_malformed_nested_message(
                &malformed_message_id,
                &root_message_id,
                subject,
                &(base_date + chrono::Duration::seconds(1)).to_rfc2822(),
                NESTING_DEPTH,
            ),
        )?;
        db.index_file_with_tags(&malformed_path, &["inbox", "message-io-e2e"])?;

        for index in 2..MESSAGE_COUNT {
            let message_id = format!("message-io-reply-{index}-{run_id}@fixture.test");
            let reply_path = fixture
                .maildir
                .join("cur")
                .join(format!("message-io-{run_id}-{index:04}:2,"));
            fs::write(
                &reply_path,
                message_io_thread_reply(
                    &message_id,
                    &root_message_id,
                    subject,
                    &(base_date + chrono::Duration::seconds(index as i64)).to_rfc2822(),
                    index,
                ),
            )?;
            db.index_file_with_tags(&reply_path, &["inbox", "message-io-e2e"])?;
        }
    }

    let query_options = notm_notmuch::QueryOptions {
        limit: 2,
        excluded_tags: Vec::new(),
        ..notm_notmuch::QueryOptions::default()
    };
    let root_summary = fixture
        .open_readonly()?
        .search_messages(&format!("id:{root_message_id}"), &query_options)?
        .into_iter()
        .next()
        .context("indexed message-I/O root was not found")?;
    ensure!(
        root_summary.filenames.len() >= 2,
        "duplicate root did not retain both indexed filenames: {root_summary:?}"
    );
    let missing_first = PathBuf::from(&root_summary.filenames[0]);
    let valid_later = root_summary
        .filenames
        .iter()
        .skip(1)
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .with_context(|| format!("root has no valid later filename: {root_summary:?}"))?;
    fs::remove_file(&missing_first)
        .with_context(|| format!("removing first indexed copy {}", missing_first.display()))?;
    ensure!(
        !missing_first.exists() && valid_later.is_file(),
        "missing-first setup failed: missing={}, later={}",
        missing_first.display(),
        valid_later.display()
    );

    let opener_marker = work_dir.join("fake-opener.log");
    install_isolated_text_opener(&work_dir, &opener_marker)?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = {}\nexcluded_tags = []\n\
             \n[identity]\nname = \"Fixture User\"\nprimary_email = \"fixture@example.test\"\n\
             \n[send]\nenabled = false\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n\
             \n[sync]\nenabled = false\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml::Value::String(format!("id:{root_message_id}")),
        ),
    )?;

    let token = format!("notm-message-io-first-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir.clone(), &token, &config_path)?;
    // This scenario validates correctness while intentionally materializing a
    // 1,001-message thread. Keep the ordinary 10-second responsiveness
    // deadline for every other smoke, but allow this correctness-only flow to
    // complete on slower CI runners.
    let mut driver = app.connect_with_command_timeout(&token, LARGE_THREAD_COMMAND_TIMEOUT)?;
    select_first_thread(&mut driver, &format!("id:{root_message_id}"))?;
    wait_for_thread_load_idle(&mut driver, LARGE_THREAD_COMMAND_TIMEOUT)?;
    let first_state = driver.command("app_state", json!({}))?;
    assert_complete_message_io_thread(
        &first_state,
        MESSAGE_COUNT,
        &newest_message_id,
        &root_message_id,
        &malformed_message_id,
    )?;
    let thread_id = first_state["state"]["selected_thread"]["thread_id"]
        .as_str()
        .with_context(|| format!("selected message-I/O thread has no ID: {first_state}"))?;
    let details = driver.command("thread_ui_details", json!({}))?;
    let detail = &details["thread_details"][thread_id];
    let warning = detail["load_warning"]
        .as_str()
        .with_context(|| format!("large thread has no explicit detail warning: {details}"))?;
    ensure!(
        warning.contains(&format!("contains {MESSAGE_COUNT} message(s)"))
            && warning.contains(&format!(
                "safety limit of {}",
                notm_ui::model::MAX_THREAD_DETAIL_MESSAGES
            ))
            && warning.contains("no partial thread was loaded"),
        "large thread detail warning was incomplete: {warning}"
    );
    ensure!(
        detail["preview"] == ""
            && detail["has_attachment"] == false
            && detail["has_encrypted"] == false
            && detail["has_signed"] == false,
        "large thread published partial row details despite its warning: {detail}"
    );

    select_loaded_message(&mut driver, &malformed_message_id)?;
    let malformed_raw = driver.command("show_raw_source", json!({}))?;
    assert_eq!(
        malformed_raw["ok"], true,
        "binary-tolerant raw view failed for malformed nested MIME: {malformed_raw}"
    );
    let malformed_raw_text = driver.command("message_view_text", json!({}))?;
    let malformed_raw_text = malformed_raw_text["text"]
        .as_str()
        .with_context(|| format!("malformed raw view returned no text: {malformed_raw_text}"))?;
    ensure!(
        malformed_raw_text.contains("X-Malformed: before-")
            && malformed_raw_text.contains("-after")
            && malformed_raw_text.contains("malformed UTF-8 body before")
            && malformed_raw_text.contains("after invalid bytes"),
        "binary-tolerant raw view lost the malformed message markers: {malformed_raw_text:?}"
    );

    let malformed = driver.command("show_text_thread", json!({}))?;
    assert_eq!(
        malformed["ok"], true,
        "bounded MIME failure did not remain recoverable: {malformed}"
    );
    let malformed_text = driver.command("message_view_text", json!({}))?;
    let malformed_text = malformed_text["text"]
        .as_str()
        .with_context(|| format!("malformed MIME view returned no text: {malformed_text}"))?;
    ensure!(
        malformed_text.contains("Could not parse body:")
            && malformed_text
                .to_ascii_lowercase()
                .contains("nesting depth")
            && malformed_text.to_ascii_lowercase().contains("limit"),
        "over-deep MIME did not expose its bounded, recoverable parse failure: {malformed_text:?}"
    );
    assert_eq!(driver.command("health", json!({}))?["ok"], true);

    select_loaded_message(&mut driver, &root_message_id)?;
    let raw = driver.command("show_raw_source", json!({}))?;
    assert_eq!(raw["ok"], true, "raw source failed via later copy: {raw}");
    let raw_text = driver.command("message_view_text", json!({}))?;
    let raw_text = raw_text["text"]
        .as_str()
        .with_context(|| format!("raw source returned no text: {raw_text}"))?;
    ensure!(
        raw_text.contains(&format!("Message-ID: <{root_message_id}>"))
            && raw_text.contains("Content-Disposition: attachment; filename=note.txt"),
        "raw source did not come from the valid later indexed copy: {raw_text:?}"
    );

    select_loaded_message(&mut driver, &root_message_id)?;
    let opened_path = open_message_io_attachment(
        &mut driver,
        &root_message_id,
        &opener_marker,
        "later indexed copy",
    )?;

    let persisted: toml::Value = fs::read_to_string(&config_path)?.parse()?;
    assert_eq!(
        persisted
            .get("ui")
            .and_then(|ui| ui.get("message_view_preferences"))
            .and_then(|preferences| preferences.get(root_message_id.as_str()))
            .and_then(toml::Value::as_str),
        Some("raw_source"),
        "raw per-message preference was not persisted: {persisted}"
    );

    let closed = driver.command("close_main_window", json!({}))?;
    assert_eq!(closed["ok"], true, "first process did not close: {closed}");
    drop(driver);
    let status = app.wait_for_exit(STARTUP_TIMEOUT)?;
    ensure!(
        status.success(),
        "first message-I/O process exited with {status}\n{}",
        app.logs()
    );
    ensure!(
        !opened_path.parent().is_some_and(Path::exists),
        "normal exit retained the private attachment Open directory: {}",
        opened_path.display()
    );

    drop(app.display.take());
    for path in [&app.socket_path, &app.log_path] {
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("removing first-run artifact {}", path.display()))?;
        }
    }
    let display_dir = work_dir.join("gui-display");
    if display_dir.exists() {
        fs::remove_dir_all(&display_dir)
            .with_context(|| format!("removing first-run display {}", display_dir.display()))?;
    }

    let moved_copy = fixture
        .maildir
        .join("cur")
        .join(format!("message-io-{run_id}-moved:2,S"));
    fs::rename(&valid_later, &moved_copy).with_context(|| {
        format!(
            "moving current indexed copy {} to {}",
            valid_later.display(),
            moved_copy.display()
        )
    })?;
    {
        let db = fixture.open_readwrite()?;
        db.remove_message_file(&valid_later)?;
        db.index_file_with_tags(&moved_copy, &["inbox", "message-io-e2e"])?;
    }
    let moved_summary = fixture
        .open_readonly()?
        .search_messages(&format!("id:{root_message_id}"), &query_options)?
        .into_iter()
        .next()
        .context("moved message-I/O root was not found after reindex")?;
    ensure!(
        !valid_later.exists()
            && moved_copy.is_file()
            && moved_summary
                .filenames
                .iter()
                .any(|path| Path::new(path) == moved_copy)
            && !moved_summary
                .filenames
                .iter()
                .any(|path| Path::new(path) == valid_later),
        "Maildir move/reindex did not replace the old indexed path: old={}, moved={}, filenames={:?}",
        valid_later.display(),
        moved_copy.display(),
        moved_summary.filenames
    );
    fs::remove_file(&opener_marker).context("clearing first-run isolated opener marker")?;

    let restart_token = format!("notm-message-io-restart-{run_id}");
    let mut restarted = FixtureApp::spawn_with_config(work_dir, &restart_token, &config_path)?;
    let mut restarted_driver =
        restarted.connect_with_command_timeout(&restart_token, LARGE_THREAD_COMMAND_TIMEOUT)?;
    select_first_thread(&mut restarted_driver, &format!("id:{root_message_id}"))?;
    wait_for_thread_load_idle(&mut restarted_driver, LARGE_THREAD_COMMAND_TIMEOUT)?;
    let restart_state = restarted_driver.command("app_state", json!({}))?;
    assert_complete_message_io_thread(
        &restart_state,
        MESSAGE_COUNT,
        &newest_message_id,
        &root_message_id,
        &malformed_message_id,
    )?;
    select_loaded_message(&mut restarted_driver, &root_message_id)?;
    let restored_raw = restarted_driver.command("message_view_text", json!({}))?;
    let restored_raw_text = restored_raw["text"]
        .as_str()
        .with_context(|| format!("restored raw view returned no text: {restored_raw}"))?;
    ensure!(
        restored_raw_text.contains(&format!("Message-ID: <{root_message_id}>"))
            && restored_raw_text.contains("Content-Disposition: attachment; filename=note.txt"),
        "restart did not restore the raw per-message view: {restored_raw}"
    );

    select_loaded_message(&mut restarted_driver, &root_message_id)?;
    open_message_io_attachment(
        &mut restarted_driver,
        &root_message_id,
        &opener_marker,
        "moved and reindexed copy after restart",
    )?;

    select_loaded_message(&mut restarted_driver, &root_message_id)?;
    let reply = restarted_driver.command("reply_selected", json!({}))?;
    assert_eq!(reply["ok"], true, "reply via moved copy failed: {reply}");
    assert_eq!(
        reply["pending"], true,
        "reply preparation was not asynchronous: {reply}"
    );
    let reply_preparation =
        wait_for_composer_preparation_idle(&mut restarted_driver, LARGE_THREAD_COMMAND_TIMEOUT)?;
    assert_eq!(
        reply_preparation["outcome"], "prepared",
        "reply via moved copy did not finish preparing: {reply_preparation}"
    );
    let reply = restarted_driver.command("app_state", json!({}))?;
    assert_eq!(
        reply["state"]["compose_fields"]["in_reply_to"],
        format!("<{root_message_id}>")
    );
    assert_eq!(
        reply["state"]["compose_fields"]["subject"],
        format!("Re: {subject}")
    );
    ensure!(
        reply["state"]["compose_fields"]["body"]
            .as_str()
            .is_some_and(|body| body.contains("> Valid attachment-bearing root body.")),
        "reply did not quote the selected valid root: {reply}"
    );

    Ok(())
}

#[test]
fn fixture_html_link_hints_label_visible_links_and_cancel() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_html_link_hints_label_visible_links_and_cancel: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running HTML link-hint desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-link-hints-ui-{run_id}"));
    let token = format!("notm-link-hints-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "id:html-message@fixture.test")?;

    let started = driver.command("send_key", json!({"key": "F", "modifiers": ["shift"]}))?;
    assert_eq!(
        started["handled"], true,
        "link-hint shortcut was not routed: {started}"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let hints = loop {
        let state = driver.command("link_hint_state", json!({}))?;
        if state["link_hints"]["active"] == true || state["link_hints"]["loading"] == false {
            break state;
        }
        ensure!(
            Instant::now() < deadline,
            "link hints did not finish loading: {state}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    assert_eq!(hints["link_hints"]["active"], true, "{hints}");
    assert_eq!(hints["link_hints"]["candidate_count"], 2, "{hints}");
    assert_eq!(hints["link_hints"]["overlay_count"], 2, "{hints}");
    let labels = json_array_at(&hints, &["link_hints", "labels"])?;
    ensure!(
        labels.len() == 2
            && labels
                .iter()
                .all(|label| label.as_str().is_some_and(|label| label.len() == 1))
            && labels[0] != labels[1],
        "visible links did not receive distinct single-key labels: {hints}"
    );

    let invalid = driver.command("input_link_hint", json!({"key": "1"}))?;
    assert_eq!(
        invalid["link_hints"]["active"], true,
        "invalid input unexpectedly closed link hints: {invalid}"
    );
    let pane_before = driver.command("app_state", json!({}))?["state"]["active_pane"].clone();
    let modal_h = driver.command("send_key", json!({"key": "H", "modifiers": ["shift"]}))?;
    assert_eq!(
        modal_h["handled"], true,
        "link hints did not consume H: {modal_h}"
    );
    let after_h = driver.command("link_hint_state", json!({}))?;
    assert_eq!(
        after_h["link_hints"]["active"], true,
        "H escaped link-hint mode into its normal binding: {after_h}"
    );
    assert_eq!(
        driver.command("app_state", json!({}))?["state"]["active_pane"],
        pane_before,
        "modal H moved the active pane"
    );
    let cancelled = driver.command("send_key", json!({"key": "Escape"}))?;
    assert_eq!(
        cancelled["handled"], true,
        "Escape was not routed: {cancelled}"
    );
    let cancelled = driver.command("link_hint_state", json!({}))?;
    assert_eq!(cancelled["link_hints"]["phase"], "idle", "{cancelled}");
    assert_eq!(cancelled["link_hints"]["overlay_count"], 0, "{cancelled}");

    Ok(())
}

#[test]
fn fixture_ctrl_e_y_scroll_message_list_without_changing_selection() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_ctrl_e_y_scroll_message_list_without_changing_selection: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running message-list viewport Ctrl+e/Ctrl+y UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-message-list-scroll-ui-{run_id}"));
    let token = format!("notm-message-list-scroll-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    driver.command("resize_window", json!({"width": 1000, "height": 420}))?;
    let scheduled = driver.command("run_search", json!({"query": "*"}))?;
    assert_eq!(
        scheduled["scheduled"], true,
        "search was not scheduled: {scheduled}"
    );
    let search = driver.wait_for_search(STARTUP_TIMEOUT)?;
    ensure!(
        search["state"]["thread_list_items"]
            .as_array()
            .is_some_and(|rows| rows.len() >= 8),
        "fixture did not provide enough message-list rows: {search}"
    );
    driver.command("select_thread_by_index", json!({"index": 0}))?;
    thread::sleep(Duration::from_millis(350));

    let deadline = Instant::now() + Duration::from_secs(5);
    let initial = loop {
        let viewport = driver.command("thread_selection_view_state", json!({}))?;
        let scrollable = viewport["scroll_upper"].as_f64().unwrap_or_default()
            > viewport["scroll_page_size"].as_f64().unwrap_or_default();
        if viewport["selected_abs"] == 0 && scrollable {
            break viewport;
        }
        ensure!(
            Instant::now() < deadline,
            "message-list viewport never became scrollable: {viewport}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    let initial_y = initial["scroll_value"]
        .as_f64()
        .with_context(|| format!("initial list scroll offset is missing: {initial}"))?;
    let selected_before =
        driver.command("app_state", json!({}))?["state"]["selected_thread"]["thread_id"].clone();

    let down = driver.command("send_key", json!({"key": "e", "modifiers": ["control"]}))?;
    assert_eq!(down["handled"], true, "Ctrl+e was not handled: {down}");
    let deadline = Instant::now() + Duration::from_secs(5);
    let down_y = loop {
        let viewport = driver.command("thread_selection_view_state", json!({}))?;
        let y = viewport["scroll_value"].as_f64().unwrap_or(initial_y);
        if y > initial_y {
            break y;
        }
        ensure!(
            Instant::now() < deadline,
            "Ctrl+e did not scroll the message list down: {viewport}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    assert_eq!(
        driver.command("app_state", json!({}))?["state"]["selected_thread"]["thread_id"],
        selected_before,
        "Ctrl+e changed the selected message-list row"
    );

    let up = driver.command("send_key", json!({"key": "y", "modifiers": ["control"]}))?;
    assert_eq!(up["handled"], true, "Ctrl+y was not handled: {up}");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let viewport = driver.command("thread_selection_view_state", json!({}))?;
        let y = viewport["scroll_value"].as_f64().unwrap_or(down_y);
        if y < down_y {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "Ctrl+y did not scroll the message list up: {viewport}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
    assert_eq!(
        driver.command("app_state", json!({}))?["state"]["selected_thread"]["thread_id"],
        selected_before,
        "Ctrl+y changed the selected message-list row"
    );

    Ok(())
}

#[test]
fn fixture_html_readiness_and_scroll_are_event_driven_and_generation_safe() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_html_readiness_and_scroll_are_event_driven_and_generation_safe: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running HTML lifecycle desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-html-lifecycle-ui-{run_id}"));
    let token = format!("notm-html-lifecycle-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "id:long-html-message@fixture.test")?;

    let visual = driver.command("show_visual_html", json!({}))?;
    assert_eq!(visual["ok"], true, "long HTML could not render: {visual}");
    let ready_deadline = Instant::now() + Duration::from_secs(5);
    let initial = loop {
        let lifecycle = driver.command("html_scroll_state", json!({}))?;
        if lifecycle["ready"] == true && lifecycle["scroll"]["canScroll"] == true {
            break lifecycle;
        }
        ensure!(
            Instant::now() < ready_deadline,
            "HTML lifecycle did not become ready and scrollable: {lifecycle}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    let initial_generation = initial["generation"]
        .as_u64()
        .with_context(|| format!("HTML lifecycle had no generation: {initial}"))?;
    let initial_y = initial["scroll"]["y"]
        .as_f64()
        .with_context(|| format!("HTML lifecycle had no scroll offset: {initial}"))?;

    let scheduled = driver.command("scroll_html_view_lines", json!({"lines": 8}))?;
    ensure!(
        scheduled["pending"] == true
            || scheduled["scroll"]["y"].as_f64().unwrap_or(initial_y) > initial_y,
        "HTML scroll was neither scheduled nor observed: {scheduled}"
    );
    let scroll_deadline = Instant::now() + Duration::from_secs(5);
    let scrolled = loop {
        let lifecycle = driver.command("html_scroll_state", json!({}))?;
        if lifecycle["scroll"]["y"]
            .as_f64()
            .is_some_and(|y| y > initial_y)
        {
            break lifecycle;
        }
        ensure!(
            Instant::now() < scroll_deadline,
            "event-driven HTML scroll did not complete: {lifecycle}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    let scrolled_fraction = scrolled["scroll"]["fraction"]
        .as_f64()
        .with_context(|| format!("HTML lifecycle had no scroll fraction: {scrolled}"))?;
    ensure!(
        scrolled_fraction > 0.0,
        "HTML scroll fraction did not advance: {scrolled}"
    );

    assert_eq!(driver.command("show_text_thread", json!({}))?["ok"], true);
    thread::sleep(STARTUP_POLL_INTERVAL);
    assert_eq!(driver.command("show_visual_html", json!({}))?["ok"], true);
    let restore_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let lifecycle = driver.command("html_scroll_state", json!({}))?;
        if lifecycle["ready"] == true
            && lifecycle["pending_restore"].is_null()
            && lifecycle["scroll"]["fraction"]
                .as_f64()
                .is_some_and(|fraction| fraction >= scrolled_fraction * 0.8)
        {
            break;
        }
        ensure!(
            Instant::now() < restore_deadline,
            "HTML scroll restoration did not complete after view replacement: {lifecycle}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }

    // Start two replacements without waiting for the first document to finish.
    // Only the newest document generation may become ready or publish metrics.
    assert_eq!(driver.command("show_visual_html", json!({}))?["ok"], true);
    assert_eq!(driver.command("show_visual_html", json!({}))?["ok"], true);
    let replacement_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let lifecycle = driver.command("html_scroll_state", json!({}))?;
        if lifecycle["ready"] == true
            && lifecycle["generation"]
                .as_u64()
                .is_some_and(|generation| generation >= initial_generation + 2)
        {
            assert_eq!(
                lifecycle["error"],
                Value::Null,
                "stale HTML completion surfaced an error: {lifecycle}"
            );
            break;
        }
        ensure!(
            Instant::now() < replacement_deadline,
            "newest HTML generation did not become ready: {lifecycle}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }

    Ok(())
}

#[test]
fn fixture_standalone_html_replacements_and_scroll_are_generation_safe() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_standalone_html_replacements_and_scroll_are_generation_safe: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running standalone HTML lifecycle desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-standalone-html-ui-{run_id}"));
    let token = format!("notm-standalone-html-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "id:long-html-message@fixture.test")?;
    assert_eq!(
        driver.command(
            "set_pane_visibility",
            json!({"pane": "message", "visible": false}),
        )?["ok"],
        true
    );
    driver.command("open_selected_thread", json!({}))?;

    let open_deadline = Instant::now() + Duration::from_secs(5);
    let opened = loop {
        let windows = driver.command("standalone_message_windows", json!({}))?;
        if !json_array_at(&windows, &["windows"])?.is_empty() {
            break windows;
        }
        ensure!(
            Instant::now() < open_deadline,
            "standalone HTML window did not open: {windows}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    assert_eq!(
        opened["window_limit"], 4,
        "window cap was not exposed: {opened}"
    );
    let first_window_id = opened["windows"][0]["id"]
        .as_u64()
        .with_context(|| format!("standalone window had no id: {opened}"))?;
    let empty_generation = opened["windows"][0]["html_lifecycle"]["generation"]
        .as_u64()
        .with_context(|| format!("standalone empty HTML had no lifecycle token: {opened}"))?;
    ensure!(
        empty_generation > 0,
        "standalone empty HTML bypassed the lifecycle: {opened}"
    );

    let first_visual = driver.command("standalone_show_visual_html", json!({"window_index": 0}))?;
    assert_eq!(
        first_visual["ok"], true,
        "standalone long HTML could not render: {first_visual}"
    );
    let first_generation = first_visual["window"]["html_lifecycle"]["generation"]
        .as_u64()
        .with_context(|| format!("standalone lifecycle had no generation: {first_visual}"))?;
    ensure!(
        first_generation > empty_generation,
        "first standalone message load reused the empty-document generation: {first_visual}"
    );

    // Replace the document twice more without waiting for either load to finish.
    // A stale load or scroll callback must not publish readiness for the newest token.
    assert_eq!(
        driver.command("standalone_show_visual_html", json!({"window_index": 0}),)?["ok"],
        true
    );
    assert_eq!(
        driver.command("standalone_show_visual_html", json!({"window_index": 0}),)?["ok"],
        true
    );

    let health_before_started = Instant::now();
    let health_before = driver.command("health", json!({}))?;
    let health_before_elapsed = health_before_started.elapsed();
    thread::sleep(Duration::from_millis(150));
    let health_after_started = Instant::now();
    let health_after = driver.command("health", json!({}))?;
    let health_after_elapsed = health_after_started.elapsed();
    ensure!(
        health_before_elapsed < Duration::from_millis(500)
            && health_after_elapsed < Duration::from_millis(500),
        "GTK health blocked behind standalone WebKit loading: before={health_before_elapsed:?}, after={health_after_elapsed:?}"
    );
    ensure!(
        health_after["gtk_heartbeat"].as_u64().unwrap_or(0)
            > health_before["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat did not advance during standalone WebKit replacements: before={health_before}, after={health_after}"
    );

    let ready_deadline = Instant::now() + Duration::from_secs(5);
    let ready = loop {
        let windows = driver.command("standalone_message_windows", json!({}))?;
        let lifecycle = &windows["windows"][0]["html_lifecycle"];
        if lifecycle["ready"] == true
            && lifecycle["generation"]
                .as_u64()
                .is_some_and(|generation| generation >= first_generation + 2)
            && lifecycle["scroll"]["canScroll"] == true
        {
            assert_eq!(
                lifecycle["error"],
                Value::Null,
                "stale standalone completion surfaced an error: {windows}"
            );
            break windows;
        }
        ensure!(
            Instant::now() < ready_deadline,
            "newest standalone HTML generation did not become ready: {windows}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    let initial_y = ready["windows"][0]["html_lifecycle"]["scroll"]["y"]
        .as_f64()
        .with_context(|| format!("standalone lifecycle had no scroll offset: {ready}"))?;

    let scheduled = driver.command(
        "standalone_scroll_html_lines",
        json!({"window_index": 0, "lines": 8}),
    )?;
    assert_eq!(
        scheduled["ok"], true,
        "standalone scroll failed: {scheduled}"
    );
    let scroll_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let windows = driver.command("standalone_message_windows", json!({}))?;
        let lifecycle = &windows["windows"][0]["html_lifecycle"];
        if lifecycle["scroll"]["y"]
            .as_f64()
            .is_some_and(|y| y > initial_y)
        {
            assert_eq!(lifecycle["error"], Value::Null, "{windows}");
            break;
        }
        ensure!(
            Instant::now() < scroll_deadline,
            "event-driven standalone HTML scroll did not complete: {windows}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }

    // Repeated opens retain prepared-thread Arcs. The controller must evict the
    // oldest window instead of allowing that cache ownership to grow forever.
    for expected_count in 2..=4 {
        driver.command("open_selected_thread", json!({}))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let windows = driver.command("standalone_message_windows", json!({}))?;
            if json_array_at(&windows, &["windows"])?.len() == expected_count {
                break;
            }
            ensure!(
                Instant::now() < deadline,
                "standalone window {expected_count} did not open: {windows}\n{}",
                app.logs()
            );
            thread::sleep(STARTUP_POLL_INTERVAL);
        }
    }
    driver.command("open_selected_thread", json!({}))?;
    let eviction_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let windows = driver.command("standalone_message_windows", json!({}))?;
        let snapshots = json_array_at(&windows, &["windows"])?;
        if snapshots.len() == 4
            && snapshots
                .iter()
                .all(|window| window["id"].as_u64() != Some(first_window_id))
        {
            break;
        }
        ensure!(
            Instant::now() < eviction_deadline,
            "oldest standalone window was not evicted at the cap: {windows}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }

    Ok(())
}

fn wait_for_active_resolved_view(
    driver: &mut UiDriver,
    expected: &str,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let state = driver.command("view_preference_state", json!({}))?;
        ensure!(state["ok"] == true, "view preference state failed: {state}");
        if state["resolved_view"] == expected && state["active_view"] == state["resolved_view"] {
            return Ok(state);
        }
        ensure!(
            Instant::now() < deadline,
            "message view did not render resolved view {expected:?} within {timeout:?}: {state}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

#[test]
fn fixture_message_and_sender_views_persist_with_message_precedence() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_message_and_sender_views_persist_with_message_precedence: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running persistent message-view desktop UI smoke with {display}");

    let run_id = unique_run_id()?;
    let shared = tempfile::tempdir()?;
    let config_path = shared.path().join("config.toml");
    fs::write(&config_path, "")?;

    let token = format!("notm-view-preference-first-{run_id}");
    let mut app = FixtureApp::spawn_fixture_with_config(
        std::env::temp_dir().join(format!("notm-view-preference-first-{run_id}")),
        &token,
        &config_path,
    )?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "id:html-message@fixture.test")?;
    let visual = driver.command("show_visual_html", json!({}))?;
    assert_eq!(
        visual["ok"], true,
        "HTML preference was not selected: {visual}"
    );
    let first_state = driver.command("view_preference_state", json!({}))?;
    assert_eq!(first_state["active_view"], "visual_html", "{first_state}");
    assert_eq!(
        first_state["message_view_preferences"]["html-message@fixture.test"], "visual_html",
        "{first_state}"
    );
    let fixture_config_path = fixture_app_config_path(&mut driver)?;
    let persisted_text = fs::read_to_string(&fixture_config_path)?;
    let persisted: toml::Value = persisted_text.parse()?;
    assert_eq!(
        persisted
            .get("ui")
            .and_then(|ui| ui.get("message_view_preferences"))
            .and_then(|preferences| preferences.get("html-message@fixture.test"))
            .and_then(toml::Value::as_str),
        Some("visual_html"),
        "persisted config did not contain the message preference:\n{persisted_text}"
    );
    fs::write(&config_path, &persisted_text)?;
    drop(driver);
    drop(app);

    let token = format!("notm-view-preference-second-{run_id}");
    let mut app = FixtureApp::spawn_fixture_with_config(
        std::env::temp_dir().join(format!("notm-view-preference-second-{run_id}")),
        &token,
        &config_path,
    )?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "id:html-message@fixture.test")?;
    let restored = driver.command("view_preference_state", json!({}))?;
    assert_eq!(restored["active_view"], "visual_html", "{restored}");
    assert_eq!(restored["resolved_view"], "visual_html", "{restored}");

    let text = driver.command("show_text_thread", json!({}))?;
    assert_eq!(text["ok"], true, "text preference was not selected: {text}");
    select_first_thread(&mut driver, "id:unicode@fixture.test")?;
    select_first_thread(&mut driver, "id:html-message@fixture.test")?;
    let changed = driver.command("view_preference_state", json!({}))?;
    assert_eq!(changed["active_view"], "text", "{changed}");
    assert_eq!(changed["resolved_view"], "text", "{changed}");

    select_first_thread(&mut driver, "id:sent-like@fixture.test")?;
    let raw = driver.command("show_raw_source", json!({}))?;
    assert_eq!(raw["ok"], true, "raw view was not selected: {raw}");
    let before_sender = driver.command("view_preference_state", json!({}))?;
    assert_eq!(
        before_sender["active_view"], "raw_source",
        "{before_sender}"
    );
    ensure!(
        before_sender["sender_button"]["label"]
            .as_str()
            .is_some_and(|label| label == "Always: Raw source (V a)"),
        "sender button did not describe the selected view: {before_sender}"
    );
    assert_eq!(
        before_sender["sender_button"]["active"], false,
        "unset sender rule was styled as active: {before_sender}"
    );
    let view_prefix = driver.command("send_key", json!({"key": "v", "modifiers": ["shift"]}))?;
    assert_eq!(
        view_prefix["handled"], true,
        "physical Shift+V did not open the View shortcut namespace: {view_prefix}"
    );
    ensure!(
        view_prefix["status_text"]
            .as_str()
            .is_some_and(|status| status.contains("a sender default")),
        "View shortcut prompt omitted the sender-default action: {view_prefix}"
    );
    let sender_key = driver.command("send_key", json!({"key": "a"}))?;
    assert_eq!(
        sender_key["handled"], true,
        "V a did not toggle the sender default: {sender_key}"
    );
    let sender_set = driver.command("view_preference_state", json!({}))?;
    assert_eq!(
        sender_set["sender_view_preferences"]["fixture@example.test"], "raw_source",
        "{sender_set}"
    );
    assert_eq!(
        sender_set["sender_button"]["active"], true,
        "enabled sender rule was not styled as active: {sender_set}"
    );
    fs::copy(fixture_app_config_path(&mut driver)?, &config_path)?;
    drop(driver);
    drop(app);

    let token = format!("notm-view-preference-third-{run_id}");
    let mut app = FixtureApp::spawn_fixture_with_config(
        std::env::temp_dir().join(format!("notm-view-preference-third-{run_id}")),
        &token,
        &config_path,
    )?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "id:thread-reply1-three-message@fixture.test")?;
    let selected = driver.command("select_message_by_index", json!({"index": 1}))?;
    assert_eq!(
        selected["selected_message"]["message_id"], "thread-reply1-three-message@fixture.test",
        "{selected}"
    );
    let sender_restored =
        wait_for_active_resolved_view(&mut driver, "raw_source", STARTUP_TIMEOUT)?;
    assert_eq!(
        sender_restored["active_view"], "raw_source",
        "{sender_restored}"
    );
    assert_eq!(
        sender_restored["resolved_view"], "raw_source",
        "{sender_restored}"
    );
    ensure!(
        sender_restored["sender_button"]["label"]
            .as_str()
            .is_some_and(|label| label == "Always: Raw source (V a)"),
        "restored sender rule was not reflected in the View menu: {sender_restored}"
    );
    assert_eq!(
        sender_restored["sender_button"]["active"], true,
        "restored sender rule was not styled as active: {sender_restored}"
    );

    let headers = driver.command("show_full_headers", json!({}))?;
    assert_eq!(
        headers["ok"], true,
        "header view was not selected: {headers}"
    );
    select_first_thread(&mut driver, "id:unicode@fixture.test")?;
    select_first_thread(&mut driver, "id:thread-reply1-three-message@fixture.test")?;
    driver.command("select_message_by_index", json!({"index": 1}))?;
    let message_override =
        wait_for_active_resolved_view(&mut driver, "full_headers", STARTUP_TIMEOUT)?;
    assert_eq!(
        message_override["active_view"], "full_headers",
        "{message_override}"
    );
    assert_eq!(
        message_override["resolved_view"], "full_headers",
        "{message_override}"
    );
    assert_eq!(
        message_override["sender_view_preferences"]["fixture@example.test"], "raw_source",
        "per-message selection unexpectedly replaced the sender rule: {message_override}"
    );

    assert_eq!(
        driver.command("show_raw_source", json!({}))?["ok"],
        true,
        "could not return to the matching sender view"
    );
    let sender_removed = driver.command("click_sender_view_preference", json!({}))?;
    assert_eq!(sender_removed["ok"], true, "{sender_removed}");
    ensure!(
        sender_removed["sender_view_preferences"]
            .get("fixture@example.test")
            .is_none(),
        "matching sender button did not remove the rule: {sender_removed}"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn external_file_arg_send_reports_existing_sent_copy() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP external_file_arg_send_reports_existing_sent_copy: no GUI test display is available"
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
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:notm-external-send-empty\"\n\
             \n[identity]\nname = \"Fixture Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[send]\nenabled = true\ntransport = \"external\"\ncommand = {}\nargs = [{}]\nmode = \"file_arg\"\ntimeout_seconds = 5\nsave_sent = true\nsent_maildir = {}\nindex_sent_after_send = false\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n\
             \n[automation]\nallow_live_send_test = true\n",
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
    driver.wait_for_search(STARTUP_TIMEOUT)?;
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

    let started = driver.command("compose_send", json!({}))?;
    assert_eq!(
        started["ok"], true,
        "configured send did not start: {started}"
    );
    assert_eq!(
        started["pending"], true,
        "configured send did not report pending work: {started}"
    );
    let send = driver.wait_for_send(STARTUP_TIMEOUT)?;
    assert_eq!(
        send["state"]["last_send_report"]["accepted"], true,
        "external file-argument send was not accepted: {send}"
    );
    let reported_path = send["state"]["last_send_report"]["captured_path"]
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

    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP timed_out_send_reports_failure_and_leaves_desktop_responsive: no GUI test display is available"
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
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:notm-timeout-send-empty\"\n\
             \n[identity]\nname = \"Fixture Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[send]\nenabled = true\ncommand = {}\nargs = [{}]\nmode = \"stdin_rfc5322\"\ntimeout_seconds = 1\nsave_sent = false\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n\
             \n[automation]\nallow_live_send_test = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml_path(&helper),
            toml_path(&survived_marker),
        ),
    )?;

    let token = format!("notm-send-timeout-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir, &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
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

    let send_started_at = Instant::now();
    let started = driver.command("compose_send", json!({}))?;
    assert_eq!(started["ok"], true, "timed send did not start: {started}");
    assert_eq!(
        started["pending"], true,
        "timed send did not report pending work: {started}"
    );
    ensure!(
        send_started_at.elapsed() < Duration::from_millis(750),
        "starting a slow send blocked the desktop for {:?}",
        send_started_at.elapsed()
    );

    let health_started_at = Instant::now();
    let health = driver.command("health", json!({}))?;
    assert_eq!(health["ok"], true, "desktop blocked during send: {health}");
    ensure!(
        health_started_at.elapsed() < Duration::from_millis(750),
        "health check blocked behind the send for {:?}",
        health_started_at.elapsed()
    );
    let pending = driver.command("app_state", json!({}))?;
    assert_eq!(
        pending["state"]["send_in_progress"], true,
        "send was not pending during responsiveness checks: {pending}"
    );
    let duplicate = driver.command("compose_send", json!({}))?;
    assert_eq!(
        duplicate["ok"], false,
        "duplicate send started: {duplicate}"
    );
    ensure!(
        duplicate["error"]
            .as_str()
            .is_some_and(|error| error.contains("send is already in progress")),
        "duplicate-send error was not explicit: {duplicate}"
    );

    let send = driver.wait_for_send(Duration::from_secs(5))?;
    ensure!(
        send["state"]["last_send_report"].is_null(),
        "timed-out send unexpectedly produced a report: {send}"
    );
    let last_error = send["state"]["last_error"]
        .as_str()
        .with_context(|| format!("timed-out send did not report an error: {send}"))?;
    ensure!(
        last_error.contains("send command timed out after 1s"),
        "unexpected timed-out send error: {last_error}"
    );

    assert_eq!(
        send["state"]["compose_fields"]["subject"], "Timeout desktop smoke",
        "failed send cleared the composer: {send}"
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
#[test]
fn slow_send_preserves_newer_composer_edits_and_serializes_writes() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP slow_send_preserves_newer_composer_edits_and_serializes_writes: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running non-blocking send-overlap desktop UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-async-send-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let send_capture = work_dir.join("sent-message.eml");
    let send_helper = work_dir.join("send-helper");
    fs::write(&send_helper, "#!/bin/sh\ncat > \"$1\"\nsleep 4\n")?;
    fs::set_permissions(&send_helper, fs::Permissions::from_mode(0o755))?;
    let sync_marker = work_dir.join("sync-must-not-run");
    let sync_helper = work_dir.join("sync-helper");
    fs::write(&sync_helper, "#!/bin/sh\nprintf 'ran\\n' > \"$1\"\n")?;
    fs::set_permissions(&sync_helper, fs::Permissions::from_mode(0o755))?;
    let sync_command = toml::Value::String(format!(
        "{} {}",
        sync_helper.display(),
        sync_marker.display()
    ))
    .to_string();
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:notm-slow-send-empty\"\n\
             \n[identity]\nname = \"Fixture Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[send]\nenabled = true\ncommand = {}\nargs = [{}]\nmode = \"stdin_rfc5322\"\ntimeout_seconds = 10\nsave_sent = false\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n\
             \n[sync]\nenabled = true\nexternal_receive_enabled = true\nexternal_receive_on_startup = false\nexternal_receive_command = {}\n\
             \n[automation]\nallow_live_send_test = true\nallow_live_tag_test = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml_path(&send_helper),
            toml_path(&send_capture),
            sync_command,
        ),
    )?;

    let token = format!("notm-async-send-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir.clone(), &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "subject:\"Unread inbox message\"")?;
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    for (command, value) in [
        ("compose_set_from", "Fixture Sender <sender@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Original slow-send subject"),
        ("compose_set_body", "Original slow-send body"),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }
    let saved = driver.command("save_draft", json!({}))?;
    assert_eq!(saved["ok"], true, "draft save failed: {saved}");
    let saved_draft_path = saved["report"]["local_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("saved draft had no local path: {saved}"))?;
    ensure!(saved_draft_path.is_file(), "saved draft is missing");

    let send_started_at = Instant::now();
    let started = driver.command("compose_send", json!({}))?;
    assert_eq!(
        started["ok"], true,
        "slow send confirmation was not requested: {started}"
    );
    assert_eq!(
        started["pending_confirmation"], true,
        "saved-draft send did not require confirmation: {started}"
    );
    assert_eq!(started["pending"], false);
    accept_send_confirmation(&mut driver)?;
    let sending = driver.command("app_state", json!({}))?;
    assert_eq!(
        sending["state"]["send_in_progress"], true,
        "accepted send confirmation did not start transport: {sending}"
    );
    ensure!(
        send_started_at.elapsed() < Duration::from_millis(750),
        "slow send blocked its start response for {:?}",
        send_started_at.elapsed()
    );

    for (command, value) in [
        ("compose_set_subject", "Newer subject kept during send"),
        ("compose_set_body", "Newer body kept during send"),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(
            response["ok"], true,
            "composer edit was blocked during send: {response}"
        );
    }
    let health = driver.command("health", json!({}))?;
    assert_eq!(health["ok"], true, "desktop blocked during send: {health}");
    let browsed = driver.command("select_thread_by_index", json!({"index": 0}))?;
    assert_eq!(browsed["ok"], true, "message browsing failed: {browsed}");
    let after_browse = driver.command("app_state", json!({}))?;
    assert_eq!(
        after_browse["state"]["active_draft"]["path"],
        saved_draft_path.display().to_string(),
        "message browsing detached the pending draft: {after_browse}"
    );

    assert_eq!(
        driver.command("select_draft_by_index", json!({"index": 0}))?["ok"],
        true
    );
    for (command, args) in [
        ("tag_selected", json!({"add": ["must-not-apply"]})),
        ("save_draft", json!({})),
        ("delete_active_draft", json!({})),
        ("delete_selected_draft", json!({})),
        ("load_selected_draft", json!({})),
        ("load_draft", json!({})),
        ("clear_draft", json!({})),
        ("open_compose", json!({})),
        ("reply_selected", json!({})),
        ("forward_selected", json!({})),
        ("run_manual_sync", json!({})),
    ] {
        let blocked = driver.command(command, args)?;
        assert_eq!(
            blocked["ok"], false,
            "{command} was accepted during send: {blocked}"
        );
        ensure!(
            blocked["error"].as_str().is_some_and(|error| {
                error.contains("send is") && error.contains("in progress")
            }),
            "{command} did not explain the send conflict: {blocked}"
        );
    }
    ensure!(
        saved_draft_path.is_file(),
        "blocked draft operation removed {}",
        saved_draft_path.display()
    );
    ensure!(!sync_marker.exists(), "blocked sync command still executed");

    let send = driver.wait_for_send(Duration::from_secs(8))?;
    assert_eq!(
        send["state"]["last_send_report"]["accepted"], true,
        "slow send was not accepted: {send}"
    );
    assert_eq!(
        send["state"]["compose_fields"]["subject"], "Newer subject kept during send",
        "accepted send discarded the newer subject: {send}"
    );
    assert_eq!(
        send["state"]["compose_fields"]["body"], "Newer body kept during send",
        "accepted send discarded the newer body: {send}"
    );
    ensure!(
        send["state"]["active_draft"].is_null(),
        "deleted sent draft remained active: {send}"
    );
    ensure!(
        send["state"]["last_error"].is_null(),
        "successful overlap send reported an error: {send}"
    );
    ensure!(
        !saved_draft_path.exists(),
        "accepted send did not remove its captured draft source"
    );
    let captured = fs::read_to_string(&send_capture)?;
    ensure!(
        captured.contains("\r\nSubject: Original slow-send subject\r\n")
            && captured.contains("\r\n\r\nOriginal slow-send body")
            && !captured.contains("Newer subject kept during send")
            && !captured.contains("Newer body kept during send"),
        "transport did not receive the immutable send snapshot:\n{captured}"
    );
    let recovery_path = work_dir.join("state/notm/draft.json");
    let recovery: Value = serde_json::from_slice(&fs::read(&recovery_path)?)?;
    assert_eq!(recovery["subject"], "Newer subject kept during send");
    assert_eq!(recovery["body"], "Newer body kept during send");

    let resaved = driver.command("save_draft", json!({}))?;
    assert_eq!(
        resaved["ok"], true,
        "draft writing did not resume after send: {resaved}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn closing_main_window_waits_for_send_finalization() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP closing_main_window_waits_for_send_finalization: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running send lifetime desktop UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-send-lifetime-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let send_capture = work_dir.join("sent-message.eml");
    let send_helper = work_dir.join("send-helper");
    fs::write(&send_helper, "#!/bin/sh\nsleep 2\ncat > \"$1\"\n")?;
    fs::set_permissions(&send_helper, fs::Permissions::from_mode(0o755))?;
    let sent_maildir = work_dir.join("Sent");
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:notm-close-send-empty\"\n\
             \n[identity]\nname = \"Fixture Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[send]\nenabled = true\ncommand = {}\nargs = [{}]\nmode = \"stdin_rfc5322\"\ntimeout_seconds = 10\nsave_sent = true\nsent_maildir = {}\nindex_sent_after_send = false\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n\
             \n[automation]\nallow_live_send_test = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml_path(&send_helper),
            toml_path(&send_capture),
            toml_path(&sent_maildir),
        ),
    )?;

    let token = format!("notm-send-lifetime-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir.clone(), &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    for (command, value) in [
        ("compose_set_from", "Fixture Sender <sender@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Close-window send lifetime"),
        ("compose_set_body", "Close-window send body"),
    ] {
        assert_eq!(
            driver.command(command, json!({"value": value}))?["ok"],
            true
        );
    }
    let saved = driver.command("save_draft", json!({}))?;
    let saved_draft_path = saved["report"]["local_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("saved draft had no local path: {saved}"))?;
    let recovery_path = work_dir.join("state/notm/draft.json");
    ensure!(
        saved_draft_path.is_file() && !recovery_path.exists(),
        "clean saved draft retained transient recovery state"
    );

    let started = driver.command("compose_send", json!({}))?;
    assert_eq!(
        started["ok"], true,
        "send confirmation was not requested: {started}"
    );
    assert_eq!(started["pending_confirmation"], true);
    assert_eq!(started["pending"], false);
    accept_send_confirmation(&mut driver)?;
    let close = driver.command("close_main_window", json!({}))?;
    assert_eq!(close["ok"], true, "main-window close failed: {close}");
    drop(driver);

    thread::sleep(Duration::from_millis(300));
    ensure!(
        app.child.try_wait()?.is_none(),
        "application exited before pending send finalization\n{}",
        app.logs()
    );
    ensure!(
        !send_capture.exists(),
        "slow transport completed unexpectedly early"
    );
    ensure!(
        saved_draft_path.is_file() && !recovery_path.exists(),
        "draft source or clean recovery state changed before transport acceptance"
    );

    let status = app.wait_for_exit(Duration::from_secs(8))?;
    ensure!(
        status.success(),
        "application exited with {status}\n{}",
        app.logs()
    );
    ensure!(
        send_capture.is_file(),
        "transport capture is missing after exit"
    );
    ensure!(
        sent_maildir
            .join("cur")
            .read_dir()?
            .next()
            .transpose()?
            .is_some(),
        "durable Sent copy was not finalized before exit"
    );
    ensure!(
        !saved_draft_path.exists(),
        "sent draft source survived finalization"
    );
    ensure!(
        !recovery_path.exists(),
        "recovery draft survived finalization"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn reactivating_during_send_reuses_the_pending_window_session() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP reactivating_during_send_reuses_the_pending_window_session: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running pending-send reactivation desktop UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-send-reactivate-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let send_capture = work_dir.join("sent-message.eml");
    let send_helper = work_dir.join("send-helper");
    fs::write(&send_helper, "#!/bin/sh\nsleep 4\ncat > \"$1\"\n")?;
    fs::set_permissions(&send_helper, fs::Permissions::from_mode(0o755))?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:notm-reactivate-send-empty\"\n\
             \n[identity]\nname = \"Fixture Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[send]\nenabled = true\ncommand = {}\nargs = [{}]\nmode = \"stdin_rfc5322\"\ntimeout_seconds = 10\nsave_sent = false\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n\
             \n[automation]\nallow_live_send_test = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml_path(&send_helper),
            toml_path(&send_capture),
        ),
    )?;

    let token = format!("notm-send-reactivate-ui-{run_id}");
    let application_id = format!("io.github.kris004.notm.test.r{}", run_id.replace('-', ""));
    let mut app = FixtureApp::spawn_with_config_and_application_id(
        work_dir,
        &token,
        &config_path,
        &application_id,
    )?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    for (command, value) in [
        ("compose_set_from", "Fixture Sender <sender@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Reactivated pending send"),
        ("compose_set_body", "Reactivation must reuse this state"),
    ] {
        assert_eq!(
            driver.command(command, json!({"value": value}))?["ok"],
            true
        );
    }
    let started = driver.command("compose_send", json!({}))?;
    assert_eq!(started["ok"], true, "send did not start: {started}");
    assert_eq!(driver.command("close_main_window", json!({}))?["ok"], true);
    thread::sleep(Duration::from_millis(200));
    ensure!(
        app.child.try_wait()?.is_none(),
        "primary exited while its send was pending"
    );

    app.request_message_id(
        &format!("secondary-{token}"),
        &application_id,
        "thread-root-three-message@fixture.test",
    )?;
    let reactivated = driver.command("app_state", json!({}))?;
    assert_eq!(
        reactivated["state"]["send_in_progress"], true,
        "reactivation created a second idle session: {reactivated}"
    );
    assert_eq!(
        reactivated["state"]["compose_fields"]["subject"], "Reactivated pending send",
        "reactivation lost the pending composer: {reactivated}"
    );
    let duplicate = driver.command("compose_send", json!({}))?;
    assert_eq!(
        duplicate["ok"], false,
        "reactivated session allowed a second send"
    );

    let completed = driver.wait_for_send(Duration::from_secs(8))?;
    assert_eq!(
        completed["state"]["last_send_report"]["accepted"], true,
        "reactivated pending send did not finish: {completed}"
    );
    ensure!(
        send_capture.is_file(),
        "reactivated transport capture is missing"
    );
    ensure!(
        app.child.try_wait()?.is_none(),
        "reactivated primary closed when its old pending send finished"
    );
    assert_eq!(driver.command("close_main_window", json!({}))?["ok"], true);
    drop(driver);
    let status = app.wait_for_exit(Duration::from_secs(3))?;
    ensure!(
        status.success(),
        "reactivated app did not exit cleanly: {status}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn accepted_send_aggregates_cleanup_failures_and_preserves_recovery() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP accepted_send_aggregates_cleanup_failures_and_preserves_recovery: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running send cleanup-error desktop UI smoke with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-send-cleanup-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let send_helper = work_dir.join("send-helper");
    fs::write(&send_helper, "#!/bin/sh\nsleep 2\ncat >/dev/null\n")?;
    fs::set_permissions(&send_helper, fs::Permissions::from_mode(0o755))?;
    let invalid_sent_maildir = work_dir.join("sent-is-a-file");
    fs::write(&invalid_sent_maildir, b"not a maildir")?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:notm-cleanup-send-empty\"\n\
             \n[identity]\nname = \"Fixture Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[send]\nenabled = true\ncommand = {}\nmode = \"stdin_rfc5322\"\ntimeout_seconds = 10\nsave_sent = true\nsent_maildir = {}\nindex_sent_after_send = false\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n\
             \n[automation]\nallow_live_send_test = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml_path(&send_helper),
            toml_path(&invalid_sent_maildir),
        ),
    )?;

    let token = format!("notm-send-cleanup-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir.clone(), &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    for (command, value) in [
        ("compose_set_from", "Fixture Sender <sender@example.test>"),
        ("compose_set_to", "recipient@example.test"),
        ("compose_set_subject", "Aggregate send cleanup failures"),
        ("compose_set_body", "Accepted transport with broken cleanup"),
    ] {
        assert_eq!(
            driver.command(command, json!({"value": value}))?["ok"],
            true
        );
    }
    let saved = driver.command("save_draft", json!({}))?;
    let saved_draft_path = saved["report"]["local_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("saved draft had no local path: {saved}"))?;
    let draft_dir = saved_draft_path.parent().context("saved draft parent")?;
    fs::set_permissions(draft_dir, fs::Permissions::from_mode(0o555))?;

    let started = driver.command("compose_send", json!({}))?;
    assert_eq!(
        started["ok"], true,
        "send confirmation was not requested: {started}"
    );
    assert_eq!(started["pending_confirmation"], true);
    assert_eq!(started["pending"], false);
    accept_send_confirmation(&mut driver)?;
    let recovery_path = work_dir.join("state/notm/draft.json");
    ensure!(
        !recovery_path.exists(),
        "clean saved draft unexpectedly retained recovery state"
    );
    fs::create_dir(&recovery_path)?;

    let send = driver.wait_for_send(Duration::from_secs(8));
    fs::set_permissions(draft_dir, fs::Permissions::from_mode(0o755))?;
    let send = send?;
    assert_eq!(
        send["state"]["last_send_report"]["accepted"], true,
        "accepted report was lost to cleanup failures: {send}"
    );
    let error = send["state"]["last_error"]
        .as_str()
        .with_context(|| format!("cleanup failures were not reported: {send}"))?;
    let sent_index = error
        .find("sent save/index failed")
        .with_context(|| format!("sent persistence failure missing: {error}"))?;
    let draft_index = error
        .find("draft delete failed")
        .with_context(|| format!("draft deletion failure missing: {error}"))?;
    ensure!(
        sent_index < draft_index,
        "cleanup failures were not reported in operation order: {error}"
    );
    ensure!(
        !error.contains("draft recovery clear failed"),
        "recovery cleanup ran after draft-source deletion failed: {error}"
    );
    ensure!(
        saved_draft_path.is_file(),
        "failed draft deletion removed its source"
    );
    assert_eq!(
        send["state"]["compose_fields"]["subject"], "Aggregate send cleanup failures",
        "cleanup failure cleared the composer: {send}"
    );
    ensure!(
        recovery_path.is_dir(),
        "recovery state was removed despite the surviving draft source"
    );
    fs::remove_dir(&recovery_path)?;
    Ok(())
}

#[test]
fn fixture_standalone_message_window_keeps_its_thread_snapshot() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_standalone_message_window_keeps_its_thread_snapshot: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running standalone message-window UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-standalone-window-ui-{run_id}"));
    let token = format!("notm-standalone-window-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;

    let visible_labels = driver.command("message_action_labels", json!({}))?;
    assert_eq!(visible_labels["respond"], "Respond (r)", "{visible_labels}");
    assert_eq!(visible_labels["archive"], "Archive (a)", "{visible_labels}");

    select_first_thread(&mut driver, "subject:\"Three message thread\"")?;
    let remembered = driver.command("show_raw_source", json!({}))?;
    assert_eq!(
        remembered["ok"], true,
        "could not seed the standalone message preference: {remembered}"
    );

    let hidden = driver.command(
        "set_pane_visibility",
        json!({"pane": "message", "visible": false}),
    )?;
    assert_eq!(hidden["ok"], true, "message pane did not hide: {hidden}");
    let hidden_labels = driver.command("message_action_labels", json!({}))?;
    assert_eq!(hidden_labels["respond"], "Respond", "{hidden_labels}");
    assert_eq!(hidden_labels["reply"], "Reply", "{hidden_labels}");
    assert_eq!(hidden_labels["view"], "View", "{hidden_labels}");
    assert_eq!(
        hidden_labels["collapse_quotes"], "Collapse quotes",
        "{hidden_labels}"
    );
    assert_eq!(hidden_labels["copy"], "Copy", "{hidden_labels}");
    assert_eq!(
        hidden_labels["archive"], "Archive (a)",
        "hiding the message pane suppressed a thread action binding: {hidden_labels}"
    );

    select_first_thread(&mut driver, "subject:\"Three message thread\"")?;
    let opened = driver.command("open_selected_thread", json!({}))?;
    assert_eq!(
        opened["ok"], true,
        "fixture thread did not open in a standalone window: {opened}"
    );

    let standalone_deadline = Instant::now() + Duration::from_secs(5);
    let standalone = loop {
        let snapshot = driver.command("standalone_message_windows", json!({}))?;
        if json_array_at(&snapshot, &["windows"])?.len() == 1 {
            break snapshot;
        }
        ensure!(
            Instant::now() < standalone_deadline,
            "standalone message window did not finish opening: {snapshot}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    let windows = json_array_at(&standalone, &["windows"])?;
    ensure!(
        windows.len() == 1,
        "expected one standalone message window: {standalone}"
    );
    assert_eq!(windows[0]["message_count"], 3, "{standalone}");
    assert_eq!(windows[0]["selected_index"], 2, "{standalone}");
    assert_eq!(
        windows[0]["view"], "raw",
        "standalone window ignored the selected message's saved view: {standalone}"
    );
    assert_eq!(
        windows[0]["selected_message"]["message_id"], "thread-reply2-three-message@fixture.test",
        "standalone did not start on the newest thread message: {standalone}"
    );

    select_first_thread(&mut driver, "subject:\"Unicode\"")?;
    let after_main_change = driver.command("standalone_message_windows", json!({}))?;
    let windows = json_array_at(&after_main_change, &["windows"])?;
    ensure!(
        windows.len() == 1,
        "changing the main selection replaced or duplicated the standalone window: \
         {after_main_change}"
    );
    assert_eq!(
        windows[0]["selected_message"]["message_id"], "thread-reply2-three-message@fixture.test",
        "main selection leaked into the standalone window: {after_main_change}"
    );
    assert_eq!(
        after_main_change["main_selected_message"]["message_id"], "unicode@fixture.test",
        "main selection did not change to the second thread: {after_main_change}"
    );

    let selected = driver.command(
        "standalone_select_message",
        json!({"window_index": 0, "message_index": 0}),
    )?;
    assert_eq!(
        selected["ok"], true,
        "standalone message navigation failed: {selected}"
    );
    assert_eq!(
        selected["window"]["selected_message"]["message_id"],
        "thread-root-three-message@fixture.test",
        "standalone navigation selected the wrong snapshot message: {selected}"
    );
    assert_eq!(selected["window"]["message_count"], 3, "{selected}");
    assert_eq!(
        selected["window"]["view"], "text",
        "standalone navigation carried the previous message's view instead of resolving the new message: {selected}"
    );
    assert_eq!(
        selected["main_selected_message"]["message_id"], "unicode@fixture.test",
        "standalone navigation changed the main message selection: {selected}"
    );

    let reply = driver.command(
        "standalone_respond",
        json!({"window_index": 0, "action": "reply"}),
    )?;
    assert_eq!(
        reply["ok"], true,
        "standalone reply did not bridge to the main composer: {reply}"
    );
    assert_eq!(
        reply["pending"], true,
        "standalone reply was not prepared asynchronously: {reply}"
    );
    wait_for_composer_preparation_idle(&mut driver, STARTUP_TIMEOUT)?;
    let reply = driver.command("app_state", json!({}))?;
    assert_eq!(
        reply["state"]["compose_fields"]["in_reply_to"], "<thread-root-three-message@fixture.test>",
        "standalone reply targeted the main thread instead of its snapshot: {reply}"
    );
    assert_eq!(
        reply["state"]["compose_fields"]["subject"], "Re: Three message thread",
        "standalone reply used the wrong subject: {reply}"
    );
    assert_eq!(
        reply["state"]["selected_message"]["message_id"], "unicode@fixture.test",
        "standalone reply rewrote the main message selection: {reply}"
    );
    let visibility = driver.command("pane_visibility", json!({}))?;
    assert_eq!(
        visibility["message_view"], true,
        "standalone reply did not reveal the main composer pane: {visibility}"
    );

    Ok(())
}

#[test]
fn fixture_older_draft_clear_does_not_cancel_newer_standalone_forward() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_older_draft_clear_does_not_cancel_newer_standalone_forward: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running composer clear/preparation ordering UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-composer-clear-epoch-ui-{run_id}"));
    let token = format!("notm-composer-clear-epoch-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;

    select_first_thread(&mut driver, "subject:\"Three message thread\"")?;
    let hidden = driver.command(
        "set_pane_visibility",
        json!({"pane": "message", "visible": false}),
    )?;
    assert_eq!(hidden["ok"], true, "message pane did not hide: {hidden}");
    let opened = driver.command("open_selected_thread", json!({}))?;
    assert_eq!(
        opened["ok"], true,
        "fixture thread did not open standalone: {opened}"
    );
    let standalone_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let standalone = driver.command("standalone_message_windows", json!({}))?;
        if json_array_at(&standalone, &["windows"])?.len() == 1 {
            break;
        }
        ensure!(
            Instant::now() < standalone_deadline,
            "standalone message window did not open: {standalone}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
    let selected = driver.command(
        "standalone_select_message",
        json!({"window_index": 0, "message_index": 0}),
    )?;
    assert_eq!(
        selected["ok"], true,
        "standalone source message could not be selected: {selected}"
    );
    let authoritative_path = selected["window"]["selected_message"]["filenames"]
        .as_array()
        .and_then(|filenames| filenames.first())
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .with_context(|| format!("standalone source had no filename: {selected}"))?;
    let authoritative_bytes = fs::read(&authoritative_path)?;

    assert_eq!(
        driver.command("set_fixture_draft_delay", json!({"milliseconds": 300}))?["ok"],
        true
    );
    assert_eq!(
        driver.command(
            "set_fixture_composer_preparation_delay",
            json!({"milliseconds": 800}),
        )?["ok"],
        true
    );
    let clear_before = draft_autosave_status(&mut driver)?;
    let clear_write_count = draft_write_count(&clear_before)?;
    let compose_generation = clear_before["compose_generation"]
        .as_u64()
        .with_context(|| format!("clear status had no compose generation: {clear_before}"))?;
    let transition_epoch = clear_before["transition_epoch"]
        .as_u64()
        .with_context(|| format!("clear status had no transition epoch: {clear_before}"))?;
    let clear = driver.command("clear_draft", json!({}))?;
    assert_eq!(clear["ok"], true, "delayed clear did not start: {clear}");
    assert_eq!(clear["pending_confirmation"], false, "{clear}");
    let clear_busy = wait_for_draft_worker_after(&mut driver, clear_write_count, STARTUP_TIMEOUT)?;
    assert_eq!(
        clear_busy["busy"], true,
        "delayed clear was not active: {clear_busy}"
    );

    let cache_before = driver.command("attachment_io_status", json!({}))?;
    let cache_generation = cache_before["composer_cache"]["latest_generation"]
        .as_u64()
        .with_context(|| format!("composer cache had no generation: {cache_before}"))?;
    let forward = driver.command(
        "standalone_respond",
        json!({"window_index": 0, "action": "forward_attachment"}),
    )?;
    assert_eq!(forward["ok"], true, "standalone forward failed: {forward}");
    assert_eq!(
        forward["pending"], true,
        "standalone forward was not prepared asynchronously: {forward}"
    );
    let preparation_generation = forward["generation"]
        .as_u64()
        .with_context(|| format!("standalone forward had no generation: {forward}"))?;
    assert_eq!(
        driver.command(
            "set_fixture_composer_preparation_delay",
            json!({"milliseconds": 0}),
        )?["ok"],
        true
    );
    assert_eq!(
        driver.command("set_fixture_draft_delay", json!({"milliseconds": 0}))?["ok"],
        true
    );

    let clear_completed =
        wait_for_draft_write_after(&mut driver, clear_write_count, STARTUP_TIMEOUT)?;
    assert_eq!(
        clear_completed["compose_generation"], compose_generation,
        "older clear changed the composer after the newer preparation started: before={clear_before}, after={clear_completed}"
    );
    assert_eq!(
        clear_completed["transition_epoch"],
        transition_epoch.saturating_add(1),
        "standalone preparation did not advance exactly one transition epoch: before={clear_before}, after={clear_completed}"
    );
    assert_eq!(
        clear_completed["last_error"],
        Value::Null,
        "older draft clear failed: {clear_completed}"
    );
    let in_flight = driver.command("composer_preparation_status", json!({}))?;
    assert_eq!(
        in_flight["busy"], true,
        "older clear cancelled the newer standalone preparation: {in_flight}"
    );
    assert_eq!(
        in_flight["generation"], preparation_generation,
        "older clear replaced the newer standalone preparation generation: {in_flight}"
    );
    assert_eq!(in_flight["outcome"], "pending", "{in_flight}");

    let prepared = wait_for_composer_preparation_generation(
        &mut driver,
        preparation_generation,
        STARTUP_TIMEOUT,
    )?;
    assert_eq!(
        prepared["outcome"], "prepared",
        "newer standalone preparation did not finish: {prepared}"
    );
    let cached =
        wait_for_new_composer_attachment_cache(&mut driver, cache_generation, STARTUP_TIMEOUT)?;
    assert_eq!(
        cached["composer_cache"]["outcome"], "applied",
        "standalone message source was not cached: {cached}"
    );
    assert_eq!(
        cached["composer_cache"]["completed_generation"],
        cached["composer_cache"]["latest_generation"],
        "standalone cache did not complete its requested generation: {cached}"
    );
    let final_state = driver.command("app_state", json!({}))?;
    ensure!(
        final_state["state"]["compose_generation"]
            .as_u64()
            .is_some_and(|generation| generation > compose_generation),
        "newer standalone preparation never applied composer fields: {final_state}"
    );
    let cached_path = final_state["state"]["compose_fields"]["attachments"]
        .as_array()
        .and_then(|attachments| attachments.first())
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .with_context(|| format!("standalone forward cached no attachment: {final_state}"))?;
    assert_eq!(
        fs::read(&cached_path)?,
        authoritative_bytes,
        "standalone forward cache did not preserve the authoritative source bytes"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn indexed_maildir_multiselect_refresh_race_updates_filenames_and_persists_after_restart()
-> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP indexed_maildir_multiselect_refresh_race_updates_filenames_and_persists_after_restart: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running indexed Maildir tag-race UI E2E with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let query_options = notm_notmuch::QueryOptions {
        limit: 1_000,
        excluded_tags: Vec::new(),
        ..notm_notmuch::QueryOptions::default()
    };
    let attachment_message_id = "attachment-message@fixture.test";

    // Give the attachment message two indexed files so the UI must ingest every
    // authoritative filename returned by Maildir flag synchronization.
    let readonly = fixture.open_readonly()?;
    let mut attachment_matches =
        readonly.search_messages(&format!("id:{attachment_message_id}"), &query_options)?;
    ensure!(
        attachment_matches.len() == 1,
        "attachment fixture lookup was not unique: {attachment_matches:?}"
    );
    let attachment_before_duplicate = attachment_matches.remove(0);
    ensure!(
        attachment_before_duplicate.filenames.len() == 1,
        "attachment fixture unexpectedly started with multiple files: {attachment_before_duplicate:?}"
    );
    readonly.close()?;
    let duplicate_path = fixture.maildir.join("new/tag-race-duplicate.fixture");
    fs::copy(&attachment_before_duplicate.filenames[0], &duplicate_path)?;
    let writable = fixture.open_readwrite()?;
    let duplicate_id = writable.index_file_with_tags(&duplicate_path, &["inbox"])?;
    assert_eq!(duplicate_id, attachment_message_id);
    writable.close()?;

    let readonly = fixture.open_readonly()?;
    let attachment_with_duplicate =
        readonly.search_messages(&format!("id:{attachment_message_id}"), &query_options)?;
    ensure!(
        attachment_with_duplicate.len() == 1 && attachment_with_duplicate[0].filenames.len() == 2,
        "duplicate attachment file was not indexed as a second filename: {attachment_with_duplicate:?}"
    );
    readonly.close()?;

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-indexed-tag-race-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let config_home = work_dir.join("config");
    let data_applications = work_dir.join("data/applications");
    fs::create_dir_all(&config_home)?;
    fs::create_dir_all(&data_applications)?;

    // A private text/plain handler makes the standard-user attachment Open action
    // deterministic without involving any application or settings outside this E2E.
    let opener_marker = work_dir.join("attachment-opener-call");
    let opener = work_dir.join("attachment-opener");
    fs::write(
        &opener,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" > {}\n",
            opener_marker.display()
        ),
    )?;
    fs::set_permissions(&opener, fs::Permissions::from_mode(0o755))?;
    fs::write(
        data_applications.join("notm-tag-race-opener.desktop"),
        format!(
            "[Desktop Entry]\nType=Application\nName=notm tag race opener\nExec={} %u\nMimeType=text/plain;\nNoDisplay=true\nTerminal=false\n",
            opener.display()
        ),
    )?;
    fs::write(
        config_home.join("mimeapps.list"),
        "[Default Applications]\ntext/plain=notm-tag-race-opener.desktop;\n",
    )?;

    let initial_query = "subject:\"Attachment message\" or subject:\"Unread inbox message\"";
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = {}\nexcluded_tags = []\nsync_maildir_flags_after_tag_change = true\n\
             \n[identity]\nname = \"Fixture User\"\nprimary_email = \"fixture@example.test\"\n\
             \n[drafts]\nsave_maildir = false\nindex_after_save = false\n\
             \n[automation]\nallow_live_tag_test = true\nallow_live_send_test = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml::Value::String(initial_query.to_string()),
        ),
    )?;

    let token = format!("notm-indexed-tag-race-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_config(work_dir.clone(), &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    let startup = driver.wait_for_search(STARTUP_TIMEOUT)?;
    let startup_rows = json_array_at(&startup, &["state", "thread_list_items"])?;
    ensure!(
        startup_rows.len() == 2,
        "tag-race query did not produce exactly two target threads: {startup}"
    );
    let target_thread_ids = startup_rows
        .iter()
        .map(|row| {
            row["thread_id"]
                .as_str()
                .map(ToOwned::to_owned)
                .with_context(|| format!("thread row had no ID: {row}"))
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    let attachment_index = startup_rows
        .iter()
        .position(|row| row["subject"] == "Attachment message")
        .context("tag-race search did not include the attachment thread")?;
    let other_index = startup_rows
        .iter()
        .position(|row| row["subject"] == "Unread inbox message")
        .context("tag-race search did not include the unread thread")?;

    let attachment_selected =
        driver.command("select_thread_by_index", json!({"index": attachment_index}))?;
    assert_eq!(
        attachment_selected["ok"], true,
        "could not schedule attachment-thread selection: {attachment_selected}"
    );
    wait_for_thread_load_idle(&mut driver, LARGE_THREAD_COMMAND_TIMEOUT)?;
    let attachment_selected = driver.command("app_state", json!({}))?;
    assert_eq!(
        attachment_selected["state"]["selected_thread"]["subject"], "Attachment message",
        "attachment thread did not settle as selected: {attachment_selected}"
    );
    let raw_preference = driver.command("show_raw_source", json!({}))?;
    assert_eq!(
        raw_preference["ok"], true,
        "could not seed raw view before opening the standalone window: {raw_preference}"
    );
    let hidden = driver.command(
        "set_pane_visibility",
        json!({"pane": "message", "visible": false}),
    )?;
    assert_eq!(hidden["ok"], true, "could not hide message pane: {hidden}");
    let opened = driver.command("open_selected_thread", json!({}))?;
    assert_eq!(
        opened["ok"], true,
        "could not open the pre-mutation standalone message window: {opened}"
    );
    let standalone_before = driver.command("standalone_message_windows", json!({}))?;
    let standalone_before_windows = json_array_at(&standalone_before, &["windows"])?;
    ensure!(
        standalone_before_windows.len() == 1
            && standalone_before_windows[0]["selected_message"]["message_id"]
                == attachment_message_id
            && standalone_before_windows[0]["view"] == "raw",
        "pre-mutation standalone attachment snapshot was not ready: {standalone_before}"
    );

    // Select both immutable thread snapshots, leaving the attachment thread active
    // so both its main-pane and standalone caches are live when filenames change.
    let first_multi =
        driver.command("toggle_multi_select_thread", json!({"index": other_index}))?;
    assert_eq!(
        first_multi["ok"], true,
        "first multi-select failed: {first_multi}"
    );
    let second_multi = driver.command(
        "toggle_multi_select_thread",
        json!({"index": attachment_index}),
    )?;
    assert_eq!(
        second_multi["ok"], true,
        "second multi-select failed: {second_multi}"
    );
    wait_for_thread_load_idle(&mut driver, LARGE_THREAD_COMMAND_TIMEOUT)?;
    let selected_ids = second_multi["multi_selected_threads"]
        .as_array()
        .with_context(|| format!("multi-selection did not return thread IDs: {second_multi}"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .with_context(|| format!("multi-selected ID was not a string: {value}"))
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    assert_eq!(selected_ids, target_thread_ids);

    let readonly = fixture.open_readonly()?;
    let before_target_messages = readonly
        .search_messages("*", &query_options)?
        .into_iter()
        .filter(|message| target_thread_ids.contains(&message.thread_id))
        .collect::<Vec<_>>();
    readonly.close()?;
    ensure!(
        before_target_messages.len() == 2,
        "selected target threads did not map to two fixture messages: {before_target_messages:?}"
    );
    ensure!(
        before_target_messages.iter().all(|message| {
            !message.tags.iter().any(|tag| tag == "flagged")
                && message
                    .filenames
                    .iter()
                    .all(|filename| Path::new(filename).is_file())
        }),
        "selected targets were not in a clean pre-mutation state: {before_target_messages:?}"
    );
    let old_filenames = before_target_messages
        .iter()
        .map(|message| {
            (
                message.message_id.clone(),
                message
                    .filenames
                    .iter()
                    .map(PathBuf::from)
                    .collect::<BTreeSet<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        old_filenames.get(attachment_message_id).map(BTreeSet::len),
        Some(2),
        "the selected attachment message lost its second file before mutation"
    );

    // Change the database after selection so a refreshed `*` result has a new row
    // at the front. The tag worker must still use the two captured thread IDs.
    let interloper_path = fixture.maildir.join("cur/tag-race-interloper.fixture:2,");
    fs::write(
        &interloper_path,
        "From: interloper@example.test\r\nTo: fixture@example.test\r\nSubject: Tag race interloper\r\nDate: Thu, 18 Jun 2037 20:00:00 -0600\r\nMessage-ID: <tag-race-interloper@fixture.test>\r\n\r\nnewest row\r\n",
    )?;
    let writable = fixture.open_readwrite()?;
    let interloper_id = writable.index_file_with_tags(&interloper_path, &["inbox", "unread"])?;
    assert_eq!(interloper_id, "tag-race-interloper@fixture.test");
    writable.close()?;

    let race_tag = format!("notm/tag-race-{run_id}");
    let rejected_tag = format!("notm/rejected-race-{run_id}");
    let loader_before_race = driver.command("thread_load_status", json!({}))?;
    let cancelled_before_race = loader_before_race["cancelled"].as_u64().unwrap_or(0);
    let refresh = driver.command("run_search", json!({"query": "*", "test_delay_ms": 1_200}))?;
    assert_eq!(
        refresh["scheduled"], true,
        "delayed refresh was not scheduled: {refresh}"
    );
    let delayed = driver.command("set_fixture_thread_delay", json!({"milliseconds": 1_200}))?;
    assert_eq!(
        delayed["ok"], true,
        "could not delay preparation during the tag race: {delayed}"
    );
    let delayed_preparation =
        driver.command("select_thread_by_index", json!({"index": other_index}))?;
    assert_eq!(
        delayed_preparation["ok"], true,
        "could not schedule the overlapping thread preparation: {delayed_preparation}"
    );
    let delayed_status = driver.command("thread_load_status", json!({}))?;
    assert_eq!(
        delayed_status["busy"], true,
        "overlapping thread preparation was not active: {delayed_status}"
    );
    let delayed_generation = delayed_status["generation"]
        .as_u64()
        .with_context(|| format!("overlapping preparation had no generation: {delayed_status}"))?;
    driver.command("set_fixture_thread_delay", json!({"milliseconds": 0}))?;
    let tagged = driver.command(
        "tag_selected",
        json!({
            "add": [race_tag.clone(), "flagged"],
            "test_delay_ms": 600,
        }),
    )?;
    assert_eq!(tagged["ok"], true, "tag mutation was rejected: {tagged}");
    assert_eq!(
        tagged["pending"], true,
        "tag mutation did not remain asynchronous: {tagged}"
    );
    let cancelled_preparation = driver.command("thread_load_status", json!({}))?;
    assert_eq!(
        cancelled_preparation["busy"], false,
        "tag mutation left the stale preparation active: delayed_generation={delayed_generation}, status={cancelled_preparation}"
    );
    ensure!(
        cancelled_preparation["cancelled"]
            .as_u64()
            .is_some_and(|cancelled| cancelled > cancelled_before_race),
        "tag mutation did not cancel overlapping generation {delayed_generation}: before={loader_before_race}, after={cancelled_preparation}"
    );
    let retained_after_cancel = driver.command("app_state", json!({}))?;
    assert_eq!(
        retained_after_cancel["state"]["selected_thread"]["thread_id"],
        attachment_before_duplicate.thread_id,
        "tag cancellation let delayed preparation replace the retained attachment thread: {retained_after_cancel}"
    );
    assert_eq!(
        driver.command("health", json!({}))?["ok"],
        true,
        "GTK stopped responding while the delayed tag worker was active"
    );
    let repeated = driver.command("tag_selected", json!({"add": [rejected_tag.clone()]}))?;
    assert_eq!(
        repeated["ok"], false,
        "rapid conflicting tag action was accepted: {repeated}"
    );
    ensure!(
        repeated["error"]
            .as_str()
            .is_some_and(|error| error.contains("tag change is already in progress")),
        "rapid action rejection did not explain the conflict: {repeated}"
    );
    let selected_before_navigation =
        driver.command("app_state", json!({}))?["state"]["selected_thread"]["thread_id"].clone();
    let navigation = driver.command("select_thread_by_index", json!({"index": other_index}))?;
    assert_eq!(
        navigation["ok"], false,
        "thread navigation changed the visible selection during a tag write: {navigation}"
    );
    let selected_after_navigation = driver.command("app_state", json!({}))?;
    assert_eq!(
        selected_after_navigation["state"]["selected_thread"]["thread_id"],
        selected_before_navigation,
        "rejected navigation desynchronized the visible and model selections: {selected_after_navigation}"
    );
    let message_navigation = driver.command("select_message_by_index", json!({"index": 0}))?;
    assert_eq!(
        message_navigation["ok"], false,
        "message navigation was accepted while Maildir paths could be changing: {message_navigation}"
    );

    let completed = wait_for_tag(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        completed["state"]["last_error"],
        Value::Null,
        "tag mutation did not complete cleanly: {completed}\n{}",
        app.logs()
    );
    let settled = driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(
        settled["state"]["current_query"], "*",
        "the refresh raced by the tag operation was not reconciled: {settled}"
    );
    let settled_rows = json_array_at(&settled, &["state", "thread_list_items"])?;
    ensure!(
        settled_rows
            .first()
            .is_some_and(|row| row["subject"] == "Tag race interloper"),
        "the post-selection database change did not reorder refreshed results: {settled}"
    );

    let readonly = fixture.open_readonly()?;
    let all_messages = readonly.search_messages("*", &query_options)?;
    readonly.close()?;
    let after_by_id = all_messages
        .iter()
        .cloned()
        .map(|message| (message.message_id.clone(), message))
        .collect::<BTreeMap<_, _>>();
    for message in &all_messages {
        let was_selected = target_thread_ids.contains(&message.thread_id);
        assert_eq!(
            message.tags.iter().any(|tag| tag == &race_tag),
            was_selected,
            "exact target tag membership was wrong for {} in thread {}",
            message.message_id,
            message.thread_id
        );
        ensure!(
            !message.tags.iter().any(|tag| tag == &rejected_tag),
            "rejected rapid tag action changed {}",
            message.message_id
        );
        if was_selected {
            ensure!(
                message.tags.iter().any(|tag| tag == "flagged"),
                "selected message was not flagged: {message:?}"
            );
            let current_paths = message
                .filenames
                .iter()
                .map(PathBuf::from)
                .collect::<BTreeSet<_>>();
            ensure!(
                current_paths.iter().all(|path| path.is_file()),
                "database reported a non-current filename for {}: {current_paths:?}",
                message.message_id
            );
            let old_paths = old_filenames
                .get(&message.message_id)
                .with_context(|| format!("missing old filenames for {}", message.message_id))?;
            ensure!(
                old_paths.iter().all(|path| !path.exists()),
                "a pre-mutation filename still exists for {}: old={old_paths:?}, current={current_paths:?}",
                message.message_id
            );
            ensure!(
                current_paths.is_disjoint(old_paths),
                "Maildir flag sync did not rename every file for {}: old={old_paths:?}, current={current_paths:?}",
                message.message_id
            );
        }
    }
    let attachment_after = after_by_id
        .get(attachment_message_id)
        .context("attachment message disappeared after tag mutation")?;
    assert_eq!(
        attachment_after.filenames.len(),
        2,
        "Maildir sync did not preserve both indexed attachment files: {attachment_after:?}"
    );
    ensure!(
        after_by_id
            .get(&interloper_id)
            .is_some_and(|message| !message.tags.iter().any(|tag| tag == &race_tag)),
        "the newly front-positioned thread was tagged instead of an immutable target"
    );

    // Recreate the attachment message's pre-rename paths as readable, unindexed
    // poison files. Cached-path-first reads must still use the raw authoritative
    // path mappings from the tag report rather than accepting these stale files.
    const STALE_PATH_SENTINEL: &str = "STALE-PATH-SENTINEL";
    let stale_attachment_paths = old_filenames
        .get(attachment_message_id)
        .context("attachment message had no pre-rename paths")?
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    let poison_message = format!(
        "From: poison@example.test\r\nTo: fixture@example.test\r\nSubject: Poison stale attachment source\r\nDate: Thu, 18 Jun 2037 20:01:00 -0600\r\nMessage-ID: <{attachment_message_id}>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=stale-path-boundary\r\n\r\n--stale-path-boundary\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{STALE_PATH_SENTINEL} body\r\n--stale-path-boundary\r\nContent-Type: text/plain; name=note.txt\r\nContent-Disposition: attachment; filename=note.txt\r\n\r\n{STALE_PATH_SENTINEL} attachment\r\n--stale-path-boundary--\r\n"
    );
    for path in &stale_attachment_paths {
        fs::write(path, poison_message.as_bytes())
            .with_context(|| format!("creating readable stale path {}", path.display()))?;
    }

    let retained_attachments = driver.command("attachment_list_items", json!({}))?;
    ensure!(
        json_array_at(&retained_attachments, &["attachments"])?
            .iter()
            .any(|attachment| attachment["filename"] == "note.txt"),
        "tag reconciliation lost the retained attachment payload: {retained_attachments}"
    );
    let retained_downloads = work_dir.join("retained-authoritative-downloads");
    fs::create_dir_all(&retained_downloads)?;
    let retained_save = driver.command(
        "save_selected_attachment",
        json!({"index": 0, "dir": retained_downloads}),
    )?;
    assert_eq!(
        retained_save["pending"], true,
        "retained attachment save did not start: {retained_save}"
    );
    let retained_save_status = wait_for_attachment_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    let retained_saved_path = retained_save_status["last_completion"]["path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| {
            format!("retained attachment save returned no path: {retained_save_status}")
        })?;
    let retained_saved = fs::read(&retained_saved_path)?;
    ensure!(
        String::from_utf8_lossy(&retained_saved).contains("attached text")
            && !String::from_utf8_lossy(&retained_saved).contains(STALE_PATH_SENTINEL),
        "retained attachment payload read a stale pre-rename source: {}",
        String::from_utf8_lossy(&retained_saved)
    );

    let verify_ui_thread = |driver: &mut UiDriver, thread_id: &str| -> anyhow::Result<Value> {
        select_first_thread(driver, &format!("thread:{thread_id}"))?;
        wait_for_thread_load_idle(driver, LARGE_THREAD_COMMAND_TIMEOUT)?;
        let state = driver.command("app_state", json!({}))?;
        let ui_messages = json_array_at(&state, &["state", "messages"])?;
        ensure!(
            !ui_messages.is_empty(),
            "UI loaded no messages for exact thread {thread_id}: {state}"
        );
        for ui_message in ui_messages {
            let message_id = ui_message["message_id"]
                .as_str()
                .with_context(|| format!("UI message had no ID: {ui_message}"))?;
            let authoritative = after_by_id.get(message_id).with_context(|| {
                format!("UI loaded unknown database message {message_id}: {ui_message}")
            })?;
            let ui_filenames = ui_message["filenames"]
                .as_array()
                .with_context(|| format!("UI message had no filenames: {ui_message}"))?
                .iter()
                .map(|filename| {
                    filename
                        .as_str()
                        .map(PathBuf::from)
                        .with_context(|| format!("UI filename was not a string: {filename}"))
                })
                .collect::<anyhow::Result<BTreeSet<_>>>()?;
            let authoritative_filenames = authoritative
                .filenames
                .iter()
                .map(PathBuf::from)
                .collect::<BTreeSet<_>>();
            assert_eq!(
                ui_filenames, authoritative_filenames,
                "UI retained stale filenames for {message_id}: {state}"
            );
            ensure!(
                ui_filenames.iter().all(|path| path.is_file()),
                "UI exposed a missing filename for {message_id}: {ui_filenames:?}"
            );
        }
        Ok(state)
    };
    for thread_id in &target_thread_ids {
        verify_ui_thread(&mut driver, thread_id)?;
    }

    let standalone_after = driver.command("standalone_message_windows", json!({}))?;
    let standalone_after_windows = json_array_at(&standalone_after, &["windows"])?;
    ensure!(
        standalone_after_windows.len() == 1,
        "tag completion lost or duplicated the pre-existing standalone window: {standalone_after}"
    );
    let standalone_message = &standalone_after_windows[0]["selected_message"];
    assert_eq!(standalone_message["message_id"], attachment_message_id);
    assert_eq!(
        standalone_after_windows[0]["view"], "raw",
        "standalone raw view changed during filename reconciliation: {standalone_after}"
    );
    let standalone_filenames = standalone_message["filenames"]
        .as_array()
        .with_context(|| {
            format!("standalone message had no filenames after rename: {standalone_after}")
        })?
        .iter()
        .map(|filename| {
            filename
                .as_str()
                .map(PathBuf::from)
                .with_context(|| format!("standalone filename was not a string: {filename}"))
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    assert_eq!(
        standalone_filenames,
        attachment_after
            .filenames
            .iter()
            .map(PathBuf::from)
            .collect::<BTreeSet<_>>(),
        "standalone snapshot retained pre-rename paths: {standalone_after}"
    );
    let standalone_rerender = driver.command(
        "standalone_select_message",
        json!({"window_index": 0, "message_index": 0}),
    )?;
    assert_eq!(
        standalone_rerender["ok"], true,
        "standalone raw view could not read the renamed message: {standalone_rerender}"
    );
    assert_eq!(standalone_rerender["window"]["view"], "raw");

    let visible = driver.command(
        "set_pane_visibility",
        json!({"pane": "message", "visible": true}),
    )?;
    assert_eq!(
        visible["ok"], true,
        "could not restore message pane: {visible}"
    );
    let attachment_thread_id = attachment_after.thread_id.clone();
    verify_ui_thread(&mut driver, &attachment_thread_id)?;
    let raw = driver.command("show_raw_source", json!({}))?;
    assert_eq!(
        raw["ok"], true,
        "raw view could not open the renamed message: {raw}"
    );
    let raw_text = driver.command("message_view_text", json!({}))?;
    ensure!(
        raw_text["text"]
            .as_str()
            .is_some_and(|text| text.contains("Subject: Attachment message")
                && !text.contains(STALE_PATH_SENTINEL)),
        "main raw view exposed the readable stale path: {raw_text}"
    );
    let listed = driver.command("attachment_list_items", json!({}))?;
    ensure!(
        json_array_at(&listed, &["attachments"])?
            .iter()
            .any(|attachment| attachment["filename"] == "note.txt"),
        "attachment cache was not rebuilt from renamed files: {listed}"
    );
    let attachment_opened = driver.command("open_attachment", json!({"index": 0}))?;
    assert_eq!(
        attachment_opened["ok"], true,
        "attachment Open used a stale message path: {attachment_opened}"
    );
    assert_eq!(
        attachment_opened["pending"], true,
        "attachment Open was not asynchronous: {attachment_opened}"
    );
    let attachment_completion = wait_for_attachment_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        attachment_completion["last_completion"]["request_id"], attachment_opened["request_id"],
        "attachment Open completed a different request: started={attachment_opened}, completion={attachment_completion}"
    );
    assert_eq!(
        attachment_completion["last_completion"]["applied"], true,
        "attachment Open completion was stale: {attachment_completion}"
    );
    ensure!(
        attachment_completion["last_completion"]["error"].is_null(),
        "attachment Open failed: {attachment_completion}"
    );
    let opened_attachment_path = attachment_completion["last_completion"]["path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("attachment Open returned no path: {attachment_completion}"))?;
    ensure!(
        String::from_utf8_lossy(&fs::read(&opened_attachment_path)?).contains("attached text"),
        "attachment Open did not extract the expected bytes"
    );
    let opener_call = wait_for_file_text(&opener_marker, STARTUP_TIMEOUT)?;
    ensure!(
        opener_call.contains(&opened_attachment_path.display().to_string()),
        "private attachment handler received the wrong target: expected={}, actual={opener_call:?}",
        opened_attachment_path.display()
    );

    let main_reply = driver.command("reply_selected", json!({}))?;
    assert_eq!(
        main_reply["ok"], true,
        "main reply could not parse the renamed message: {main_reply}"
    );
    assert_eq!(
        main_reply["pending"], true,
        "main reply was not prepared asynchronously: {main_reply}"
    );
    let main_preparation_generation = main_reply["generation"]
        .as_u64()
        .with_context(|| format!("main reply returned no preparation generation: {main_reply}"))?;
    let main_preparation = wait_for_composer_preparation_generation(
        &mut driver,
        main_preparation_generation,
        STARTUP_TIMEOUT,
    )?;
    assert_eq!(
        main_preparation["outcome"], "prepared",
        "main reply did not finish preparing: {main_preparation}"
    );
    let main_reply = driver.command("app_state", json!({}))?;
    assert_eq!(
        main_reply["state"]["compose_fields"]["in_reply_to"],
        "<attachment-message@fixture.test>"
    );
    ensure!(
        main_reply["state"]["compose_fields"]["body"]
            .as_str()
            .is_some_and(|body| !body.contains(STALE_PATH_SENTINEL)),
        "main reply quoted the readable stale path: {main_reply}"
    );
    for command in [
        "compose_set_to",
        "compose_set_cc",
        "compose_set_bcc",
        "compose_set_subject",
        "compose_set_body",
    ] {
        let cleared = driver.command(command, json!({"value": ""}))?;
        assert_eq!(cleared["ok"], true, "could not clear main reply: {cleared}");
    }
    let main_clear_before = draft_autosave_status(&mut driver)?;
    let main_clear_write_count = draft_write_count(&main_clear_before)?;
    let main_clear_compose_generation = main_clear_before["compose_generation"]
        .as_u64()
        .with_context(|| {
            format!("main clear status had no composer generation: {main_clear_before}")
        })?;
    let clear_main_reply = driver.command("clear_draft", json!({}))?;
    assert_eq!(
        clear_main_reply["ok"], true,
        "could not close cleared main reply: {clear_main_reply}"
    );
    assert_eq!(clear_main_reply["pending_confirmation"], false);
    let main_clear_completed =
        wait_for_draft_write_after(&mut driver, main_clear_write_count, STARTUP_TIMEOUT)?;
    ensure!(
        main_clear_completed["compose_generation"]
            .as_u64()
            .is_some_and(|generation| generation > main_clear_compose_generation),
        "main reply clear did not reach its composer boundary: before={main_clear_before}, after={main_clear_completed}"
    );
    assert_eq!(
        main_clear_completed["last_error"],
        Value::Null,
        "main reply clear failed: {main_clear_completed}"
    );

    let standalone_reply = driver.command(
        "standalone_respond",
        json!({"window_index": 0, "action": "reply"}),
    )?;
    assert_eq!(
        standalone_reply["ok"], true,
        "pre-existing standalone window could not reply after rename: {standalone_reply}"
    );
    assert_eq!(
        standalone_reply["pending"], true,
        "standalone reply was not prepared asynchronously: {standalone_reply}"
    );
    let standalone_preparation_generation =
        standalone_reply["generation"].as_u64().with_context(|| {
            format!("standalone reply returned no preparation generation: {standalone_reply}")
        })?;
    let standalone_preparation = wait_for_composer_preparation_generation(
        &mut driver,
        standalone_preparation_generation,
        STARTUP_TIMEOUT,
    )?;
    assert_eq!(
        standalone_preparation["outcome"], "prepared",
        "standalone reply did not finish preparing: {standalone_preparation}"
    );
    let standalone_reply = driver.command("app_state", json!({}))?;
    assert_eq!(
        standalone_reply["state"]["compose_fields"]["in_reply_to"],
        "<attachment-message@fixture.test>"
    );
    ensure!(
        standalone_reply["state"]["compose_fields"]["body"]
            .as_str()
            .is_some_and(|body| !body.contains(STALE_PATH_SENTINEL)),
        "standalone reply quoted the readable stale path: {standalone_reply}"
    );
    for command in [
        "compose_set_to",
        "compose_set_cc",
        "compose_set_bcc",
        "compose_set_subject",
        "compose_set_body",
    ] {
        let cleared = driver.command(command, json!({"value": ""}))?;
        assert_eq!(
            cleared["ok"], true,
            "could not clear standalone reply: {cleared}"
        );
    }
    let standalone_clear_before = draft_autosave_status(&mut driver)?;
    let standalone_clear_write_count = draft_write_count(&standalone_clear_before)?;
    let standalone_clear_compose_generation = standalone_clear_before["compose_generation"]
        .as_u64()
        .with_context(|| {
            format!("standalone clear status had no composer generation: {standalone_clear_before}")
        })?;
    let clear_standalone_reply = driver.command("clear_draft", json!({}))?;
    assert_eq!(
        clear_standalone_reply["ok"], true,
        "could not close cleared standalone reply: {clear_standalone_reply}"
    );
    assert_eq!(clear_standalone_reply["pending_confirmation"], false);
    let standalone_clear_completed =
        wait_for_draft_write_after(&mut driver, standalone_clear_write_count, STARTUP_TIMEOUT)?;
    ensure!(
        standalone_clear_completed["compose_generation"]
            .as_u64()
            .is_some_and(|generation| generation > standalone_clear_compose_generation),
        "standalone reply clear did not reach its composer boundary: before={standalone_clear_before}, after={standalone_clear_completed}"
    );
    assert_eq!(
        standalone_clear_completed["last_error"],
        Value::Null,
        "standalone reply clear failed: {standalone_clear_completed}"
    );

    // Keep the pre-mutation standalone window alive through the fresh main
    // reloads above, then exercise its independently retained lazy source.
    // The resulting dirty forward is closed through the real modal main-window
    // workflow below; that workflow intentionally preserves recovery state.
    let cache_before_forward = driver.command("attachment_io_status", json!({}))?;
    let previous_cache_generation = cache_before_forward["composer_cache"]["latest_generation"]
        .as_u64()
        .with_context(|| {
            format!("composer cache had no generation before forward: {cache_before_forward}")
        })?;
    let retained_standalone_forward = driver.command(
        "standalone_respond",
        json!({"window_index": 0, "action": "forward_attachment"}),
    )?;
    assert_eq!(
        retained_standalone_forward["pending"], true,
        "retained standalone forward did not prepare asynchronously: {retained_standalone_forward}"
    );
    let retained_standalone_generation = retained_standalone_forward["generation"]
        .as_u64()
        .with_context(|| {
            format!(
                "retained standalone forward returned no preparation generation: {retained_standalone_forward}"
            )
        })?;
    let retained_standalone_preparation = wait_for_composer_preparation_generation(
        &mut driver,
        retained_standalone_generation,
        STARTUP_TIMEOUT,
    )?;
    assert_eq!(
        retained_standalone_preparation["outcome"], "prepared",
        "retained standalone forward did not finish preparing: {retained_standalone_preparation}"
    );
    let retained_standalone_cache = wait_for_new_composer_attachment_cache(
        &mut driver,
        previous_cache_generation,
        STARTUP_TIMEOUT,
    )?;
    assert_eq!(
        retained_standalone_cache["composer_cache"]["outcome"], "applied",
        "retained standalone source was not cached: {retained_standalone_cache}"
    );
    assert_eq!(
        retained_standalone_cache["composer_cache"]["completed_generation"],
        retained_standalone_cache["composer_cache"]["latest_generation"],
        "retained standalone cache did not complete its requested generation: {retained_standalone_cache}"
    );
    let retained_forward_state = driver.command("app_state", json!({}))?;
    let retained_forward_paths = json_array_at(
        &retained_forward_state,
        &["state", "compose_fields", "attachments"],
    )?;
    ensure!(
        retained_forward_paths.len() == 1,
        "retained standalone forward did not cache exactly one source: {retained_forward_state}"
    );
    let retained_forward_path = retained_forward_paths[0]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| {
            format!("retained standalone forward path was not a string: {retained_forward_state}")
        })?;
    let retained_forward_bytes = fs::read(&retained_forward_path)?;
    ensure!(
        String::from_utf8_lossy(&retained_forward_bytes).contains("Subject: Attachment message")
            && !String::from_utf8_lossy(&retained_forward_bytes).contains(STALE_PATH_SENTINEL),
        "retained standalone forward cached a stale pre-rename source: {}",
        String::from_utf8_lossy(&retained_forward_bytes)
    );

    for path in &stale_attachment_paths {
        fs::remove_file(path)
            .with_context(|| format!("removing readable stale path {}", path.display()))?;
    }

    let standalone_closed = driver.command("close_standalone_message_windows", json!({}))?;
    assert_eq!(
        standalone_closed["closed"], 1,
        "could not close the exercised standalone window: {standalone_closed}"
    );
    let closed = driver.command("close_main_window", json!({}))?;
    assert_eq!(closed["ok"], true, "first app close failed: {closed}");
    let close_id = pending_confirmation_id(&mut driver, "close_main_window")?;
    let accepted_close = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": close_id}),
    )?;
    assert_eq!(
        accepted_close["ok"], true,
        "could not close the retained standalone forward at main-window Close: {accepted_close}"
    );
    let recovery_path = accepted_close["recovery_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| {
            format!("main-window Close returned no recovery path: {accepted_close}")
        })?;
    drop(driver);
    let status = app.wait_for_exit(Duration::from_secs(5))?;
    ensure!(
        status.success(),
        "first app did not exit normally: {status}\n{}",
        app.logs()
    );

    // The restart phase verifies tag/path persistence, not draft recovery.
    // Remove only this disposable process's expected recovery file so it does
    // not raise a modal while the restarted harness switches between threads.
    ensure!(
        recovery_path.is_file(),
        "main-window Close did not preserve the expected isolated recovery file: {}",
        recovery_path.display()
    );
    fs::remove_file(&recovery_path).with_context(|| {
        format!(
            "removing isolated retained-forward recovery {}",
            recovery_path.display()
        )
    })?;

    // Preserve the clean XDG state and Notmuch database while replacing only the
    // first process's private display and harness artifacts.
    drop(app.display.take());
    for path in [&app.socket_path, &app.log_path] {
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("removing first-run artifact {}", path.display()))?;
        }
    }
    let display_dir = work_dir.join("gui-display");
    if display_dir.exists() {
        fs::remove_dir_all(&display_dir)
            .with_context(|| format!("removing first-run display {}", display_dir.display()))?;
    }

    let restart_token = format!("notm-indexed-tag-race-restart-ui-{run_id}");
    let mut restarted = FixtureApp::spawn_with_config(work_dir, &restart_token, &config_path)?;
    let mut restarted_driver = restarted.connect(&restart_token)?;
    restarted_driver.wait_for_search(STARTUP_TIMEOUT)?;
    let persisted_search = restarted_driver.command(
        "run_search",
        json!({"query": format!("tag:\"{race_tag}\"")}),
    )?;
    assert_eq!(
        persisted_search["scheduled"], true,
        "restart tag query was not scheduled: {persisted_search}"
    );
    let persisted = restarted_driver.wait_for_search(STARTUP_TIMEOUT)?;
    let persisted_thread_ids = json_array_at(&persisted, &["state", "thread_list_items"])?
        .iter()
        .map(|row| {
            row["thread_id"]
                .as_str()
                .map(ToOwned::to_owned)
                .with_context(|| format!("persisted row had no thread ID: {row}"))
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    assert_eq!(
        persisted_thread_ids, target_thread_ids,
        "restart did not preserve the exact target tags: {persisted}"
    );
    for thread_id in &target_thread_ids {
        verify_ui_thread(&mut restarted_driver, thread_id)?;
    }

    let reopened = fixture.open_readonly()?;
    let persisted_messages = reopened.search_messages("*", &query_options)?;
    reopened.close()?;
    for message in persisted_messages {
        let was_selected = target_thread_ids.contains(&message.thread_id);
        assert_eq!(
            message.tags.iter().any(|tag| tag == &race_tag),
            was_selected,
            "restart changed exact tag membership for {}",
            message.message_id
        );
        if was_selected {
            let prior = after_by_id
                .get(&message.message_id)
                .with_context(|| format!("missing post-tag message {}", message.message_id))?;
            assert_eq!(
                message.filenames, prior.filenames,
                "restart changed authoritative filenames for {}",
                message.message_id
            );
            ensure!(
                message
                    .filenames
                    .iter()
                    .all(|filename| Path::new(filename).is_file()),
                "restart exposed a missing filename for {}: {:?}",
                message.message_id,
                message.filenames
            );
        }
    }

    let restart_close = restarted_driver.command("close_main_window", json!({}))?;
    assert_eq!(
        restart_close["ok"], true,
        "restarted app close failed: {restart_close}"
    );
    drop(restarted_driver);
    let restart_status = restarted.wait_for_exit(Duration::from_secs(5))?;
    ensure!(
        restart_status.success(),
        "restarted app did not exit normally: {restart_status}\n{}",
        restarted.logs()
    );

    Ok(())
}

#[test]
fn fixture_current_message_navigation_and_tagging_are_explicit() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_current_message_navigation_and_tagging_are_explicit: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running current-message navigation/tagging UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-message-tag-ui-{run_id}"));
    let token = format!("notm-message-tag-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;

    let query = "subject:\"Three message thread\"";
    select_first_thread(&mut driver, query)?;
    let before_state = driver.command("app_state", json!({}))?;
    let before = message_tags(&before_state)?;
    ensure!(before.len() == 3, "expected fixture thread: {before_state}");

    let root_id = "thread-root-three-message@fixture.test";
    let reply1_id = "thread-reply1-three-message@fixture.test";
    let reply2_id = "thread-reply2-three-message@fixture.test";
    let selected = driver.command("select_message_by_index", json!({"index": 0}))?;
    assert_eq!(selected["selected_message"]["message_id"], root_id);
    let action_labels = driver.command("message_action_labels", json!({}))?;
    assert_eq!(
        action_labels["message"], "Message 1/3 (J/K)",
        "message navigation hint is missing: {action_labels}"
    );
    let entry_state = driver.command("entry_state", json!({}))?;
    assert_eq!(
        entry_state["main_shortcut_controller_count"], 1,
        "main-window shortcuts are split across overlapping controllers: {entry_state}"
    );

    let pane_left = driver.command("send_key", json!({"key": "h", "modifiers": ["control"]}))?;
    assert_eq!(
        pane_left["handled"], true,
        "Ctrl+h was not routed: {pane_left}"
    );
    assert_eq!(pane_left["active_pane"], "Threads", "{pane_left}");
    let pane_right = driver.command("send_key", json!({"key": "l", "modifiers": ["control"]}))?;
    assert_eq!(
        pane_right["handled"], true,
        "Ctrl+l was not routed: {pane_right}"
    );
    assert_eq!(pane_right["active_pane"], "Message", "{pane_right}");

    let shortcut_next = driver.command("send_key", json!({"key": "J", "modifiers": ["shift"]}))?;
    assert_eq!(
        shortcut_next["handled"], true,
        "J was not routed: {shortcut_next}"
    );
    let shortcut_state = driver.command("app_state", json!({}))?;
    assert_eq!(
        shortcut_state["state"]["selected_message"]["message_id"], reply1_id,
        "J did not select the next message: {shortcut_state}"
    );
    let shortcut_previous =
        driver.command("send_key", json!({"key": "K", "modifiers": ["shift"]}))?;
    assert_eq!(
        shortcut_previous["handled"], true,
        "K was not routed: {shortcut_previous}"
    );
    let shortcut_state = driver.command("app_state", json!({}))?;
    assert_eq!(
        shortcut_state["state"]["selected_message"]["message_id"], root_id,
        "K did not select the previous message: {shortcut_state}"
    );

    let next = driver.command("select_relative_message", json!({"delta": 1}))?;
    assert_eq!(next["ok"], true, "next-message navigation failed: {next}");
    assert_eq!(next["selected_index"], 1);
    assert_eq!(next["selected_message"]["message_id"], reply1_id);
    let last = driver.command("select_relative_message", json!({"delta": 20}))?;
    assert_eq!(last["selected_index"], 2);
    assert_eq!(last["selected_message"]["message_id"], reply2_id);
    let still_last = driver.command("select_relative_message", json!({"delta": 1}))?;
    assert_eq!(
        still_last["selected_index"], 2,
        "navigation did not clamp: {still_last}"
    );
    let previous = driver.command("select_relative_message", json!({"delta": -1}))?;
    assert_eq!(previous["selected_index"], 1);
    assert_eq!(previous["selected_message"]["message_id"], reply1_id);

    driver.command("select_message_by_index", json!({"index": 0}))?;
    let controls = driver.command("message_tag_state", json!({}))?;
    assert_eq!(
        controls["menu_visible"], true,
        "message tag menu is hidden: {controls}"
    );
    assert_eq!(
        controls["menu_sensitive"], true,
        "message tag menu is disabled: {controls}"
    );
    assert_eq!(
        controls["archive_label"], "Archive message (M a)",
        "wrong current-message archive binding: {controls}"
    );
    assert_eq!(
        controls["read_label"], "Mark message read (M u)",
        "wrong current-message read action: {controls}"
    );
    assert_eq!(
        controls["flag_label"], "Flag message (M f)",
        "wrong current-message flag binding: {controls}"
    );
    assert_eq!(
        controls["trash_label"], "Move message to trash (M t)",
        "wrong current-message trash binding: {controls}"
    );
    assert_eq!(
        controls["spam_label"], "Mark message as spam (M s)",
        "wrong current-message spam binding: {controls}"
    );
    assert_eq!(
        controls["custom_apply_label"], "Add tag (M T)",
        "wrong current-message custom-tag binding: {controls}"
    );
    assert_eq!(
        controls["menu_label"], "Tag message (M)",
        "message-only scope is not visible in the action label: {controls}"
    );

    let custom_tag = format!("message-only-{run_id}");
    let tagged = driver.command(
        "click_message_tag_action",
        json!({"action": "custom", "tag": custom_tag}),
    )?;
    assert_eq!(
        tagged["ok"], true,
        "current-message tag action failed: {tagged}"
    );
    assert_eq!(
        tagged["pending"], true,
        "tag did not run asynchronously: {tagged}"
    );
    let tagged = wait_for_tag(&mut driver, STARTUP_TIMEOUT)?;
    let after = message_tags(&tagged)?;
    for (message_id, tags) in &after {
        assert_eq!(
            tags.contains(&custom_tag),
            message_id == root_id,
            "current-message action changed the wrong message: {after:?}"
        );
        let expected_without_custom = before
            .get(message_id)
            .with_context(|| format!("missing original message tags for {message_id}"))?;
        let actual_without_custom = tags
            .iter()
            .filter(|tag| *tag != &custom_tag)
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            &actual_without_custom, expected_without_custom,
            "current-message action changed unrelated tags on {message_id}"
        );
    }
    ensure!(
        tagged["state"]["selected_thread"]["tags"]
            .as_array()
            .is_some_and(|tags| tags.iter().any(|tag| tag == &custom_tag)),
        "thread summary did not retain the tagged message's aggregate tag: {tagged}"
    );

    let controls = driver.command("message_tag_state", json!({}))?;
    assert_eq!(
        controls["custom_apply_label"], "Remove tag (M T)",
        "custom tag toggle did not follow the current message: {controls}"
    );
    let actions = driver.command("undo_tag_actions", json!({}))?;
    let actions = json_array_at(&actions, &["actions"])?;
    ensure!(
        actions.len() == 1,
        "expected one message-only undo action: {actions:?}"
    );
    ensure!(
        actions[0]["label"]
            .as_str()
            .is_some_and(|label| label.contains("1 message")),
        "undo label does not identify message scope: {}",
        actions[0]
    );
    let mutations = json_array_at(&actions[0], &["mutations"])?;
    assert_eq!(
        mutations.len(),
        1,
        "message-only undo is not exact: {actions:?}"
    );
    assert_eq!(mutations[0]["message_id"], root_id);

    let undone = driver.command("undo_last_tag", json!({}))?;
    assert_eq!(undone["ok"], true, "message-only undo failed: {undone}");
    assert_eq!(
        undone["pending"], true,
        "undo did not run asynchronously: {undone}"
    );
    let restored = message_tags(&wait_for_tag(&mut driver, STARTUP_TIMEOUT)?)?;
    assert_eq!(
        restored, before,
        "message-only undo did not restore exact tags"
    );

    Ok(())
}

#[test]
fn fixture_tag_undo_restores_each_messages_original_tags() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_tag_undo_restores_each_messages_original_tags: no GUI test display is available"
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
        tagged["pending"], true,
        "tag did not run asynchronously: {tagged}"
    );
    let tagged = wait_for_tag(&mut driver, STARTUP_TIMEOUT)?;
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
        undone["pending"], true,
        "undo did not run asynchronously: {undone}"
    );
    let undone = wait_for_tag(&mut driver, STARTUP_TIMEOUT)?;
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

    let restored = message_tags(&driver.command("app_state", json!({}))?)?;
    assert_eq!(restored, before);

    Ok(())
}

#[cfg(unix)]
#[test]
fn fixture_settings_preview_limits_apply_without_partial_persistence() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_settings_preview_limits_apply_without_partial_persistence: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running Settings preview-limit UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-settings-preview-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        "[ui]\ntheme = \"system\"\nthread_preview_lines = 2\nshow_thread_preview = true\n",
    )?;
    let token = format!("notm-settings-preview-ui-{run_id}");
    let mut app = FixtureApp::spawn_fixture_with_config(work_dir, &token, &config_path)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;

    assert_eq!(driver.command("open_settings", json!({}))?["ok"], true);
    let initial = driver.command("settings_test_state", json!({}))?;
    assert_eq!(initial["dialog"]["visible"], true, "{initial}");
    assert_eq!(initial["theme"]["requested"], "system", "{initial}");
    assert_eq!(initial["preview"]["configured_lines"], 2, "{initial}");
    assert_eq!(initial["preview"]["rendered"]["lines"], 2, "{initial}");
    assert_eq!(initial["preview"]["rendered"]["visible"], true, "{initial}");
    let initial_preview_text = initial["preview"]["rendered"]["text"].clone();
    ensure!(
        initial_preview_text
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "fixture did not render a real thread preview label: {initial}"
    );
    let saved_config_path = PathBuf::from(
        initial["app_config_path"]
            .as_str()
            .with_context(|| format!("Settings state has no app config path: {initial}"))?,
    );
    let original_saved_bytes = fs::read(&saved_config_path).ok();

    for (theme, lines, response) in [
        ("system", json!("not-a-number"), "apply"),
        ("system", json!("not-a-number"), "save"),
        ("system", json!(0), "apply"),
        ("system", json!(0), "save"),
        ("system", json!(21), "apply"),
        ("system", json!(21), "save"),
        ("sepia", json!(2), "save"),
    ] {
        let rejected = driver.command(
            "respond_settings",
            json!({
                "response": response,
                "theme": theme,
                "thread_preview_lines": lines,
                "show_thread_preview": true,
            }),
        )?;
        assert_eq!(
            rejected["ok"], false,
            "invalid Settings input succeeded: {rejected}"
        );
        assert_eq!(rejected["state"]["dialog"]["visible"], true, "{rejected}");
        assert_eq!(
            rejected["state"]["preview"]["configured_lines"], 2,
            "invalid Settings input changed runtime state: {rejected}"
        );
        assert_eq!(
            rejected["state"]["preview"]["rendered"]["lines"], 2,
            "invalid Settings input changed the rendered label: {rejected}"
        );
        assert_eq!(
            rejected["state"]["preview"]["rendered"]["text"], initial_preview_text,
            "invalid Settings input changed cached/rendered preview content: {rejected}"
        );
        assert_eq!(
            rejected["state"]["theme"]["requested"], "system",
            "{rejected}"
        );
        assert_eq!(
            fs::read(&saved_config_path).ok(),
            original_saved_bytes,
            "invalid Settings input partially persisted to {}",
            saved_config_path.display()
        );
    }

    for lines in [1, 3] {
        let applied = driver.command(
            "respond_settings",
            json!({
                "response": "apply",
                "theme": "system",
                "thread_preview_lines": lines,
                "show_thread_preview": true,
            }),
        )?;
        assert_eq!(applied["ok"], true, "{applied}");
        assert_eq!(
            applied["state"]["preview"]["configured_lines"], lines,
            "{applied}"
        );
        assert_eq!(
            applied["state"]["preview"]["rendered"]["lines"], lines,
            "{applied}"
        );
        assert_eq!(
            applied["state"]["preview"]["rendered"]["visible"], true,
            "{applied}"
        );
        assert_eq!(
            fs::read(&saved_config_path).ok(),
            original_saved_bytes,
            "Apply unexpectedly persisted Settings"
        );
    }

    let hidden = driver.command(
        "respond_settings",
        json!({
            "response": "apply",
            "theme": "system",
            "thread_preview_lines": 3,
            "show_thread_preview": false,
        }),
    )?;
    assert_eq!(hidden["ok"], true, "{hidden}");
    assert_eq!(
        hidden["state"]["preview"]["configured_lines"], 3,
        "{hidden}"
    );
    assert_eq!(
        hidden["state"]["preview"]["show_thread_preview"], false,
        "{hidden}"
    );
    assert_eq!(
        hidden["state"]["preview"]["rendered"],
        Value::Null,
        "{hidden}"
    );

    let saved = driver.command(
        "respond_settings",
        json!({
            "response": "save",
            "theme": "system",
            "thread_preview_lines": 3,
            "show_thread_preview": true,
        }),
    )?;
    assert_eq!(saved["ok"], true, "{saved}");
    assert_eq!(saved["state"]["dialog"], Value::Null, "{saved}");
    assert_eq!(saved["state"]["preview"]["configured_lines"], 3, "{saved}");
    assert_eq!(saved["state"]["preview"]["rendered"]["lines"], 3, "{saved}");
    let persisted: toml::Value = fs::read_to_string(&saved_config_path)?.parse()?;
    assert_eq!(persisted["ui"]["theme"].as_str(), Some("system"));
    assert_eq!(
        persisted["ui"]["thread_preview_lines"].as_integer(),
        Some(3)
    );
    assert_eq!(persisted["ui"]["show_thread_preview"].as_bool(), Some(true));

    Ok(())
}

#[cfg(unix)]
#[test]
fn bcc_only_local_smtp_submission_uses_hidden_envelope_recipient() -> anyhow::Result<()> {
    let work_dir = tempfile::tempdir().context("creating Bcc-only SMTP work directory")?;
    let smtp = LocalSmtpCapture::start()?;
    let submit_helper = work_dir.path().join("submit-local-smtp");
    write_python_submission_helper(&submit_helper, smtp.port())?;

    let mut message = notm_mail::ComposedMessage::new(
        "Jörg Sender <sender@example.test>".to_string(),
        Vec::new(),
        format!("Bcc-only Unicode submission {}", "秘密".repeat(60)),
        "The sole recipient must remain private.".to_string(),
    );
    message.bcc = vec!["Hidden <hidden@example.test>".to_string()];
    let raw = message.to_rfc5322()?;
    ensure!(
        !raw.starts_with(b"To:") && !raw.windows(b"\r\nTo:".len()).any(|line| line == b"\r\nTo:"),
        "Bcc-only pre-submission message contained an empty To field"
    );

    let mut child = Command::new(&submit_helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting Bcc-only local SMTP helper")?;
    std::io::Write::write_all(child.stdin.as_mut().context("opening helper stdin")?, &raw)?;
    let output = child
        .wait_with_output()
        .context("waiting for Bcc-only local SMTP helper")?;
    ensure!(
        output.status.success(),
        "Bcc-only local SMTP helper failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let captured = smtp
        .wait_for_messages(1, Duration::from_secs(10))?
        .pop()
        .context("Bcc-only SMTP capture was empty")?;
    assert_eq!(captured.rcpt_to, ["hidden@example.test"]);
    let parsed = parse_captured_smtp_wire(
        work_dir.path(),
        "bcc-only",
        &captured,
        "sender@example.test",
    )?;
    ensure!(
        parsed["to"].as_array().is_some_and(|to| to.is_empty())
            && parsed["cc"].as_array().is_some_and(|cc| cc.is_empty())
            && parsed["bcc"].as_array().is_some_and(|bcc| bcc.is_empty()),
        "Bcc-only captured wire exposed a destination field: {parsed}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn clean_xdg_local_smtp_wire_interoperability() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP clean_xdg_local_smtp_wire_interoperability: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running clean-XDG local-SMTP wire interoperability E2E with {display}");

    let fixture = notm_test_support::FixtureDatabase::create()?;
    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-local-smtp-e2e-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let smtp = LocalSmtpCapture::start()?;
    let submit_helper = work_dir.join("submit-local-smtp");
    write_python_submission_helper(&submit_helper, smtp.port())?;

    let attachment_name = "résumé-überprüfung-非常に長い添付ファイル名-2026-final.bin";
    let attachment_path = work_dir.join(attachment_name);
    let attachment_bytes = b"notm-local-smtp-attachment\0\xff\n".repeat(4096);
    fs::write(&attachment_path, &attachment_bytes)?;

    let draft_maildir = fixture.root.join("Drafts");
    let config_path = work_dir.join("notm.toml");
    fs::write(
        &config_path,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
             \n[identity]\nname = \"Jörg Sender\"\nprimary_email = \"sender@example.test\"\n\
             \n[send]\nenabled = true\ntransport = \"external\"\ncommand = {}\nargs = []\nmode = \"stdin_rfc5322\"\ntimeout_seconds = 10\nsave_sent = false\n\
             \n[drafts]\nsave_maildir = true\nmaildir = {}\ntags = [\"draft\"]\nindex_after_save = true\n\
             \n[automation]\nallow_live_send_test = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml_path(&submit_helper),
            toml_path(&draft_maildir),
        ),
    )?;

    let to_addresses = (0..14)
        .map(|index| format!("recipient{index}+sorting@example.test"))
        .collect::<Vec<_>>();
    let to = to_addresses
        .iter()
        .enumerate()
        .map(|(index, address)| format!("非常に長い Unicode Recipient Nummer {index} <{address}>"))
        .collect::<Vec<_>>()
        .join(", ");
    let cc = "\"Doe, Zoë\" <zoe@example.test>, O'Hara <customer+tag@example.test>";
    let bcc = "Miyuki 秘密 <hidden@example.test>, hidden+archive@example.test";
    let subject = format!(
        "Interoperability Grüße — {}",
        "非常に長い件名 café Привет مرحبا ".repeat(14)
    );
    let long_body_line = "x".repeat(2500);
    let body =
        format!("Unicode body: café ☕ Привет مرحبا.\n\n{long_body_line}\n\nFinal paragraph.");

    let first_token = format!("notm-local-smtp-first-{run_id}");
    let mut first_app =
        FixtureApp::spawn_with_config(work_dir.clone(), &first_token, &config_path)?;
    let mut first_driver = first_app.connect(&first_token)?;
    first_driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(first_driver.command("open_compose", json!({}))?["ok"], true);
    for (command, value) in [
        ("compose_set_from", "Jörg Sender <sender@example.test>"),
        ("compose_set_to", to.as_str()),
        ("compose_set_cc", cc),
        ("compose_set_bcc", bcc),
        ("compose_set_body", body.as_str()),
    ] {
        let response = first_driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }

    let injection_subject = "ordinary subject\r\nBcc: injected@example.test";
    assert_eq!(
        first_driver.command("compose_set_subject", json!({"value": injection_subject}),)?["ok"],
        true
    );
    let injection_start = first_driver.command("compose_send", json!({}))?;
    let injection_error = if injection_start["pending"] == true {
        let failed = first_driver.wait_for_send(STARTUP_TIMEOUT)?;
        ensure!(
            failed["state"]["last_send_report"].is_null(),
            "header injection unexpectedly produced a send report: {failed}"
        );
        failed["state"]["last_error"]
            .as_str()
            .with_context(|| format!("header injection had no actionable error: {failed}"))?
            .to_string()
    } else {
        ensure!(
            injection_start["ok"] == false,
            "header injection neither failed nor started: {injection_start}"
        );
        injection_start["error"]
            .as_str()
            .with_context(|| {
                format!("synchronous header-injection failure had no error: {injection_start}")
            })?
            .to_string()
    };
    let normalized_error = injection_error.to_ascii_lowercase();
    ensure!(
        normalized_error.contains("subject")
            && (normalized_error.contains("newline")
                || normalized_error.contains("line break")
                || normalized_error.contains("cr/lf")
                || normalized_error.contains("control character")),
        "header-injection error was not actionable: {injection_error}"
    );
    smtp.ensure_no_message(Duration::from_millis(250))?;

    assert_eq!(
        first_driver.command("compose_set_subject", json!({"value": subject}))?["ok"],
        true
    );
    let attached =
        first_driver.command("compose_add_attachment", json!({"path": attachment_path}))?;
    assert_eq!(attached["ok"], true, "attachment add failed: {attached}");
    let saved = first_driver.command("save_draft", json!({}))?;
    assert_eq!(saved["ok"], true, "indexed draft save failed: {saved}");
    let saved_path = saved["report"]["maildir_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("indexed draft save reported no Maildir path: {saved}"))?;
    let saved_message_id = saved["report"]["indexed_message_id"]
        .as_str()
        .with_context(|| format!("indexed draft save reported no Message-ID: {saved}"))?
        .to_string();
    ensure!(saved_path.is_file(), "saved draft file is missing");

    assert_eq!(
        first_driver.command("close_main_window", json!({}))?["ok"],
        true
    );
    drop(first_driver);
    let first_status = first_app.wait_for_exit(Duration::from_secs(8))?;
    ensure!(
        first_status.success(),
        "first local-SMTP app process failed: {first_status}\n{}",
        first_app.logs()
    );

    drop(first_app.display.take());
    for path in [&first_app.socket_path, &first_app.log_path] {
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("removing first-run artifact {}", path.display()))?;
        }
    }
    let display_dir = work_dir.join("gui-display");
    if display_dir.exists() {
        fs::remove_dir_all(&display_dir)
            .with_context(|| format!("removing first-run display {}", display_dir.display()))?;
    }

    let restart_token = format!("notm-local-smtp-second-{run_id}");
    let mut restarted_app =
        FixtureApp::spawn_with_config(work_dir.clone(), &restart_token, &config_path)?;
    let mut driver = restarted_app.connect(&restart_token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    let draft_search = driver.command("run_search", json!({"query": "tag:draft"}))?;
    assert_eq!(
        draft_search["ok"], true,
        "draft search failed: {draft_search}"
    );
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    let draft_deadline = Instant::now() + STARTUP_TIMEOUT;
    let reopened = loop {
        let state = driver.command("app_state", json!({}))?;
        if state["state"]["active_draft"]["path"] == saved_path.display().to_string() {
            break state;
        }
        ensure!(
            Instant::now() < draft_deadline,
            "restart never reopened the indexed draft: {state}\n{}",
            restarted_app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    assert_eq!(
        reopened["state"]["active_draft"]["message_id"], saved_message_id,
        "restart changed the draft Message-ID: {reopened}"
    );
    assert_eq!(
        reopened["state"]["compose_fields"]["subject"], subject,
        "restart lost the saved subject: {reopened}"
    );
    let reopened_from = notm_mail::address::parse_one_checked(
        reopened["state"]["compose_fields"]["from"]
            .as_str()
            .context("reopened draft From is not text")?,
    )?;
    assert_eq!(reopened_from.email, "sender@example.test");
    assert_eq!(reopened_from.name.as_deref(), Some("Jörg Sender"));
    for (field, expected) in [
        ("to", to_addresses.iter().cloned().collect::<BTreeSet<_>>()),
        (
            "cc",
            ["zoe@example.test", "customer+tag@example.test"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
        (
            "bcc",
            ["hidden@example.test", "hidden+archive@example.test"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
    ] {
        let value = reopened["state"]["compose_fields"][field]
            .as_str()
            .with_context(|| format!("reopened draft {field} is not text: {reopened}"))?;
        let actual = notm_mail::address::parse_address_list_checked(value)?
            .into_iter()
            .map(|address| address.email)
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected, "restart changed saved {field} semantics");
    }
    assert_eq!(
        reopened["state"]["compose_fields"]["body"]
            .as_str()
            .map(|value| value.replace("\r\n", "\n")),
        Some(body.clone()),
        "restart lost the saved body: {reopened}"
    );
    ensure!(
        reopened["state"]["compose_fields"]["attachments"]
            .as_array()
            .is_some_and(|attachments| attachments.len() == 1),
        "restart lost the saved attachment: {reopened}"
    );

    let compose_start = driver.command("compose_send", json!({}))?;
    assert_eq!(
        compose_start["pending_confirmation"], true,
        "saved draft Send did not request confirmation: {compose_start}"
    );
    accept_send_confirmation(&mut driver)?;
    let compose_send = driver.wait_for_send(STARTUP_TIMEOUT)?;
    assert_eq!(
        compose_send["state"]["last_send_report"]["accepted"], true,
        "saved Unicode message was not accepted: {compose_send}"
    );
    let mut compose_messages = smtp.wait_for_messages(1, STARTUP_TIMEOUT)?;
    let compose_capture = compose_messages.pop().expect("one composed message");

    select_first_thread(&mut driver, "id:html-message@fixture.test")?;
    let reply = driver.command("reply_selected", json!({}))?;
    assert_eq!(reply["ok"], true, "HTML reply did not open: {reply}");
    assert_eq!(
        reply["pending"], true,
        "HTML reply preparation was not asynchronous: {reply}"
    );
    let reply_preparation = wait_for_composer_preparation_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        reply_preparation["outcome"], "prepared",
        "HTML reply did not finish preparing: {reply_preparation}"
    );
    for (command, value) in [
        ("compose_set_cc", "Reply Team <reply+cc@example.test>"),
        (
            "compose_set_bcc",
            "Reply Archive <reply-hidden@example.test>",
        ),
        (
            "compose_set_body",
            "Réponse Unicode café ☕ with both text and HTML alternatives.",
        ),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }
    let reply_start = driver.command("compose_send", json!({}))?;
    assert_eq!(
        reply_start["pending"], true,
        "reply did not start: {reply_start}"
    );
    let reply_send = driver.wait_for_send(STARTUP_TIMEOUT)?;
    assert_eq!(
        reply_send["state"]["last_send_report"]["accepted"], true,
        "reply was not accepted: {reply_send}"
    );
    let mut reply_messages = smtp.wait_for_messages(1, STARTUP_TIMEOUT)?;
    let reply_capture = reply_messages.pop().expect("one reply message");

    select_first_thread(&mut driver, "id:attachment-message@fixture.test")?;
    let forward = driver.command("forward_as_attachment_selected", json!({}))?;
    assert_eq!(
        forward["ok"], true,
        "forward-as-attachment did not open: {forward}"
    );
    assert_eq!(
        forward["pending"], true,
        "forward-as-attachment preparation was not asynchronous: {forward}"
    );
    let forward_preparation = wait_for_composer_preparation_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        forward_preparation["outcome"], "prepared",
        "forward-as-attachment did not finish preparing: {forward_preparation}"
    );
    let forward_cache = wait_for_composer_attachment_cache_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        forward_cache["composer_cache"]["outcome"], "applied",
        "forward-as-attachment cache was not applied: {forward_cache}"
    );
    for (command, value) in [
        (
            "compose_set_to",
            "Forward Recipient <forward+tag@example.test>",
        ),
        (
            "compose_set_bcc",
            "Forward Archive <forward-hidden@example.test>",
        ),
        (
            "compose_set_body",
            "Forward body with Unicode Grüße and an attached original message.",
        ),
    ] {
        let response = driver.command(command, json!({"value": value}))?;
        assert_eq!(response["ok"], true, "{command} failed: {response}");
    }
    let forward_start = driver.command("compose_send", json!({}))?;
    assert_eq!(
        forward_start["pending"], true,
        "forward did not start: {forward_start}"
    );
    let forward_send = driver.wait_for_send(STARTUP_TIMEOUT)?;
    assert_eq!(
        forward_send["state"]["last_send_report"]["accepted"], true,
        "forward was not accepted: {forward_send}"
    );
    let mut forward_messages = smtp.wait_for_messages(1, STARTUP_TIMEOUT)?;
    let forward_capture = forward_messages.pop().expect("one forwarded message");
    smtp.ensure_no_message(Duration::from_millis(250))?;

    assert_eq!(driver.command("close_main_window", json!({}))?["ok"], true);
    drop(driver);
    let restarted_status = restarted_app.wait_for_exit(Duration::from_secs(8))?;
    ensure!(
        restarted_status.success(),
        "restarted local-SMTP app process failed: {restarted_status}\n{}",
        restarted_app.logs()
    );

    let compose_parsed = parse_captured_smtp_wire(
        &work_dir,
        "composed",
        &compose_capture,
        "sender@example.test",
    )?;
    assert_eq!(compose_parsed["subject"], subject);
    assert_eq!(compose_parsed["from"][0]["name"], "Jörg Sender");
    assert_eq!(
        parsed_addresses(&compose_parsed, "to")?,
        to_addresses.into_iter().collect()
    );
    assert_eq!(
        parsed_addresses(&compose_parsed, "cc")?,
        ["zoe@example.test", "customer+tag@example.test"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
    let mut compose_envelope = parsed_addresses(&compose_parsed, "to")?;
    compose_envelope.extend(parsed_addresses(&compose_parsed, "cc")?);
    compose_envelope.extend([
        "hidden@example.test".to_string(),
        "hidden+archive@example.test".to_string(),
    ]);
    assert_eq!(
        compose_capture
            .rcpt_to
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        compose_envelope,
        "SMTP envelope did not preserve To/Cc/Bcc semantics"
    );
    let compose_parts = json_array_at(&compose_parsed, &["parts"])?;
    let attachment = compose_parts
        .iter()
        .find(|part| part["filename"] == attachment_name)
        .with_context(|| format!("independent parser found no attachment: {compose_parsed}"))?;
    assert_eq!(attachment["size"], attachment_bytes.len());
    assert_eq!(
        attachment["sha256"],
        "6ba2c82fe27d84e01d50bdb16550eda371f429957df9f8bb2414758419cc7ee6"
    );
    ensure!(
        compose_parts.iter().any(|part| {
            part["content_type"] == "text/plain"
                && part["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(&long_body_line))
        }),
        "independent parser did not recover the long Unicode body: {compose_parsed}"
    );

    let reply_parsed =
        parse_captured_smtp_wire(&work_dir, "reply", &reply_capture, "sender@example.test")?;
    assert_eq!(reply_parsed["subject"], "Re: HTML message");
    assert_eq!(reply_parsed["in_reply_to"], "<html-message@fixture.test>");
    ensure!(
        reply_parsed["references"]
            .as_str()
            .is_some_and(|references| references.contains("<html-message@fixture.test>")),
        "reply lost References threading: {reply_parsed}"
    );
    assert_eq!(
        reply_capture
            .rcpt_to
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        [
            "html@example.test",
            "reply+cc@example.test",
            "reply-hidden@example.test",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    );
    let reply_parts = json_array_at(&reply_parsed, &["parts"])?;
    for content_type in ["text/plain", "text/html"] {
        ensure!(
            reply_parts.iter().any(|part| {
                part["content_type"] == content_type
                    && part["text"]
                        .as_str()
                        .is_some_and(|text| text.contains("Réponse Unicode"))
            }),
            "reply did not round-trip its {content_type} alternative: {reply_parsed}"
        );
    }

    let forward_parsed = parse_captured_smtp_wire(
        &work_dir,
        "forward",
        &forward_capture,
        "sender@example.test",
    )?;
    assert_eq!(forward_parsed["subject"], "Fwd: Attachment message");
    assert_eq!(
        forward_capture
            .rcpt_to
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        ["forward+tag@example.test", "forward-hidden@example.test"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
    let forwarded_part = json_array_at(&forward_parsed, &["parts"])?
        .iter()
        .find(|part| part["content_type"] == "message/rfc822")
        .with_context(|| {
            format!("independent parser found no attached message: {forward_parsed}")
        })?;
    assert_eq!(
        forwarded_part["nested_message_id"],
        "<attachment-message@fixture.test>"
    );
    assert_eq!(forwarded_part["nested_subject"], "Attachment message");
    assert_eq!(
        forwarded_part["filename"], "forwarded-attachment-message.eml",
        "forwarded message filename was not exact: {forwarded_part}"
    );
    ensure!(
        !forwarded_part["content_transfer_encoding"]
            .as_str()
            .is_some_and(|encoding| encoding.eq_ignore_ascii_case("base64")),
        "message/rfc822 was incorrectly base64 encoded: {forwarded_part}"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn clean_xdg_duplicate_draft_headers_preserve_recipients_and_reject_authors() -> anyhow::Result<()>
{
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP clean_xdg_duplicate_draft_headers_preserve_recipients_and_reject_authors: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running duplicate draft-header UI E2E with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-duplicate-draft-ui-{run_id}"));
    fs::create_dir_all(&work_dir)?;
    let token = format!("notm-duplicate-draft-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;

    // Fixture mode permits confirmation-dialog automation, but its disposable
    // database is created in the child process. Discover that private database
    // and add the malformed interoperability fixtures only after launch.
    let startup_state = driver.command("app_state", json!({}))?;
    let database_path = startup_state["state"]["database_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("fixture app reported no database path: {startup_state}"))?;
    let config_path = database_path
        .parent()
        .context("fixture database has no parent directory")?
        .join("notmuch-config");
    let draft_maildir = database_path.join("Drafts");
    for child in ["cur", "new", "tmp"] {
        fs::create_dir_all(draft_maildir.join(child))?;
    }

    let invalid_path = draft_maildir.join("cur/duplicate-from.eml:2,D");
    fs::write(
        &invalid_path,
        concat!(
            "From: First Author <first@example.test>\r\n",
            "From: Second Author <second@example.test>\r\n",
            "To: recipient@example.test\r\n",
            "Subject: Duplicate From draft\r\n",
            "Date: Wed, 26 Aug 2026 03:00:00 +0000\r\n",
            "Message-ID: <duplicate-from-draft@example.test>\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Malformed author draft.\r\n",
        ),
    )?;
    let valid_path = draft_maildir.join("cur/duplicate-recipients.eml:2,D");
    fs::write(
        &valid_path,
        concat!(
            "From: Fixture User <fixture@example.test>\r\n",
            "To: First Recipient <first@example.test>\r\n",
            "To: Second Recipient <second@example.test>\r\n",
            "Cc: First Carbon <cc-one@example.test>\r\n",
            "Cc: Second Carbon <cc-two@example.test>\r\n",
            "Bcc: First Hidden <hidden-one@example.test>\r\n",
            "Bcc: Second Hidden <hidden-two@example.test>\r\n",
            "Subject: Duplicate recipient draft\r\n",
            "Date: Wed, 26 Aug 2026 03:01:00 +0000\r\n",
            "Message-ID: <duplicate-recipient-draft@example.test>\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
            "\r\n",
            "Duplicate recipient draft body.\r\n",
        ),
    )?;
    {
        let db = notm_notmuch::Database::open(
            &notm_notmuch::OpenConfig {
                database_path: Some(database_path),
                config_path: Some(config_path),
                profile: None,
            },
            notm_notmuch::DatabaseMode::ReadWrite,
        )?;
        db.index_fixture_file(&invalid_path, &["draft"])?;
        db.index_fixture_file(&valid_path, &["draft"])?;
    }

    let invalid_search = driver.command(
        "run_search",
        json!({"query": "id:duplicate-from-draft@example.test"}),
    )?;
    assert_eq!(
        invalid_search["ok"], true,
        "invalid draft search failed: {invalid_search}"
    );
    let invalid_result = driver.wait_for_search(STARTUP_TIMEOUT)?;
    ensure!(
        json_array_at(&invalid_result, &["state", "thread_list_items"])?
            .iter()
            .any(|thread| thread["subject"] == "Duplicate From draft"),
        "duplicate-From draft was not indexed: {invalid_result}"
    );
    assert_eq!(
        driver.command("select_thread_by_index", json!({"index": 0}))?["ok"],
        true
    );
    let invalid_deadline = Instant::now() + STARTUP_TIMEOUT;
    let invalid_state = loop {
        let state = driver.command("app_state", json!({}))?;
        if state["state"]["last_error"].as_str().is_some() {
            break state;
        }
        ensure!(
            Instant::now() < invalid_deadline,
            "duplicate From draft did not report an error: {state}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    let invalid_error = invalid_state["state"]["last_error"]
        .as_str()
        .context("duplicate From draft error is not text")?;
    ensure!(
        invalid_error.contains("From")
            && (invalid_error.contains("multiple") || invalid_error.contains("exactly one")),
        "duplicate From error was not actionable: {invalid_error}"
    );
    ensure!(
        invalid_state["state"]["active_draft"].is_null(),
        "malformed duplicate-From draft became editable: {invalid_state}"
    );

    let valid_search = driver.command(
        "run_search",
        json!({"query": "id:duplicate-recipient-draft@example.test"}),
    )?;
    assert_eq!(
        valid_search["ok"], true,
        "valid draft search failed: {valid_search}"
    );
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(
        driver.command("select_thread_by_index", json!({"index": 0}))?["ok"],
        true
    );
    let valid_deadline = Instant::now() + STARTUP_TIMEOUT;
    let opened = loop {
        let state = driver.command("app_state", json!({}))?;
        if state["state"]["active_draft"]["path"] == valid_path.display().to_string() {
            break state;
        }
        ensure!(
            Instant::now() < valid_deadline,
            "duplicate-recipient draft did not open: {state}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    };
    for (field, expected) in [
        (
            "to",
            ["first@example.test", "second@example.test"].as_slice(),
        ),
        (
            "cc",
            ["cc-one@example.test", "cc-two@example.test"].as_slice(),
        ),
        (
            "bcc",
            ["hidden-one@example.test", "hidden-two@example.test"].as_slice(),
        ),
    ] {
        let value = opened["state"]["compose_fields"][field]
            .as_str()
            .with_context(|| format!("opened {field} field is not text: {opened}"))?;
        let actual = notm_mail::address::parse_address_list_checked(value)?
            .into_iter()
            .map(|address| address.email)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            actual,
            expected.iter().map(|value| value.to_string()).collect(),
            "opening duplicate {field} fields changed recipients"
        );
    }

    assert_eq!(
        driver.command(
            "compose_set_body",
            json!({"value": "Duplicate recipients survive reopen and replacement save."}),
        )?["ok"],
        true
    );
    let save = driver.command("save_draft", json!({}))?;
    assert_eq!(
        save["pending_confirmation"], true,
        "replacement save did not confirm: {save}"
    );
    let confirmation_id = pending_confirmation_id(&mut driver, "save_draft_replacement")?;
    let accepted = driver.command(
        "respond_confirmation",
        json!({"response": "accept", "id": confirmation_id}),
    )?;
    assert_eq!(accepted["ok"], true, "replacement save failed: {accepted}");
    let replacement_path = accepted["active_draft"]["path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("replacement save reported no active path: {accepted}"))?;
    ensure!(
        replacement_path
            .extension()
            .and_then(|value| value.to_str())
            == Some("json"),
        "fixture replacement was not saved to the isolated named-draft store: {}",
        replacement_path.display()
    );
    ensure!(
        !valid_path.exists(),
        "replaced duplicate-header source remained on disk: {}",
        valid_path.display()
    );
    let replacement: Value = serde_json::from_slice(&fs::read(&replacement_path)?)?;
    for (field, value, expected) in [
        (
            "to",
            replacement["to"].as_str().unwrap_or_default(),
            ["first@example.test", "second@example.test"].as_slice(),
        ),
        (
            "cc",
            replacement["cc"].as_str().unwrap_or_default(),
            ["cc-one@example.test", "cc-two@example.test"].as_slice(),
        ),
        (
            "bcc",
            replacement["bcc"].as_str().unwrap_or_default(),
            ["hidden-one@example.test", "hidden-two@example.test"].as_slice(),
        ),
    ] {
        let actual = notm_mail::address::parse_address_list_checked(value)?
            .into_iter()
            .map(|address| address.email)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            actual,
            expected.iter().map(|value| value.to_string()).collect(),
            "replacement save changed {field} recipient semantics"
        );
    }
    assert_eq!(
        replacement["body"],
        "Duplicate recipients survive reopen and replacement save."
    );

    assert_eq!(driver.command("close_main_window", json!({}))?["ok"], true);
    drop(driver);
    let status = app.wait_for_exit(Duration::from_secs(8))?;
    ensure!(
        status.success(),
        "duplicate draft-header app failed: {status}"
    );
    Ok(())
}

#[cfg(unix)]
fn parse_captured_smtp_wire(
    work_dir: &Path,
    label: &str,
    captured: &CapturedSmtpMessage,
    expected_sender: &str,
) -> anyhow::Result<Value> {
    assert_smtp_wire_conformance(label, &captured.data)?;
    assert_eq!(
        captured.mail_from, expected_sender,
        "{label} SMTP envelope sender changed"
    );
    let path = work_dir.join(format!("captured-{label}.eml"));
    fs::write(&path, &captured.data)?;
    let parsed = parse_wire_with_python(&path)?;
    ensure!(
        parsed["defects"]
            .as_array()
            .is_some_and(|defects| defects.is_empty()),
        "independent parser reported top-level defects in {label}: {parsed}"
    );
    for part in json_array_at(&parsed, &["parts"])? {
        ensure!(
            part["defects"]
                .as_array()
                .is_some_and(|defects| defects.is_empty()),
            "independent parser reported a MIME defect in {label}: {part}"
        );
    }
    ensure!(
        parsed["bcc"].as_array().is_some_and(|bcc| bcc.is_empty()),
        "Bcc leaked into captured {label} wire bytes: {parsed}"
    );
    ensure!(
        parsed["message_id"]
            .as_str()
            .is_some_and(|message_id| message_id.starts_with('<') && message_id.ends_with('>')),
        "{label} has no standards-shaped Message-ID: {parsed}"
    );
    ensure!(
        parsed["date"].as_str().is_some_and(|date| !date.is_empty()),
        "{label} has no Date header: {parsed}"
    );
    Ok(parsed)
}

#[cfg(unix)]
fn assert_smtp_wire_conformance(label: &str, wire: &[u8]) -> anyhow::Result<()> {
    ensure!(wire.ends_with(b"\r\n"), "{label} wire does not end in CRLF");
    for (index, byte) in wire.iter().copied().enumerate() {
        if byte == b'\n' {
            ensure!(
                index > 0 && wire[index - 1] == b'\r',
                "{label} wire contains a bare LF at byte {index}"
            );
        } else if byte == b'\r' {
            ensure!(
                wire.get(index + 1) == Some(&b'\n'),
                "{label} wire contains a bare CR at byte {index}"
            );
        }
    }
    for line in wire.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        ensure!(
            line.len() <= 998,
            "{label} wire line is {} octets (RFC 5322 maximum is 998)",
            line.len()
        );
    }
    let separator = b"\r\n\r\n";
    let header_end = wire
        .windows(separator.len())
        .position(|window| window == separator)
        .with_context(|| format!("{label} wire has no header/body separator"))?;
    let header = &wire[..header_end];
    for line in header.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        ensure!(
            line.len() <= 78,
            "{label} header line is {} octets (safe fold target is 78): {}",
            line.len(),
            String::from_utf8_lossy(line)
        );
        ensure!(
            !line
                .split(|byte| *byte == b':')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(b"bcc")),
            "{label} leaked a Bcc field"
        );
    }
    let lower = String::from_utf8_lossy(header).to_ascii_lowercase();
    ensure!(
        lower.contains("=?utf-8?"),
        "{label} did not RFC 2047-encode its Unicode headers:\n{}",
        String::from_utf8_lossy(header)
    );
    ensure!(
        header.windows(3).any(|window| window == b"\r\n ")
            || header.windows(3).any(|window| window == b"\r\n\t"),
        "{label} did not contain a folded header"
    );
    Ok(())
}

#[cfg(unix)]
fn parsed_addresses(parsed: &Value, field: &str) -> anyhow::Result<BTreeSet<String>> {
    json_array_at(parsed, &[field])?
        .iter()
        .map(|mailbox| {
            mailbox["address"]
                .as_str()
                .map(str::to_string)
                .with_context(|| format!("parsed {field} mailbox has no address: {mailbox}"))
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn fixture_send_timeout_validation_preserves_last_valid_value_across_restart() -> anyhow::Result<()>
{
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_send_timeout_validation_preserves_last_valid_value_across_restart: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running restart-backed send-timeout Settings UI smoke with {display}");

    let root = tempfile::tempdir()?;
    let config_path = root.path().join("notm.toml");
    fs::write(&config_path, "[send]\ntimeout_seconds = 73\n")?;
    let original_config = fs::read(&config_path)?;

    let first_token = format!("notm-settings-timeout-first-{}", unique_run_id()?);
    let mut first_app = FixtureApp::spawn_fixture_with_config(
        root.path().join("first-launch"),
        &first_token,
        &config_path,
    )?;
    let mut first_driver = first_app.connect(&first_token)?;
    first_driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(
        first_driver.command("open_settings", json!({}))?["ok"],
        true
    );
    let initial = first_driver.command("settings_test_state", json!({}))?;
    assert_eq!(initial["configured_send_timeout_seconds"], 73, "{initial}");
    assert_eq!(initial["dialog"]["send_timeout_seconds"], "73", "{initial}");
    let settings_output_path = initial["app_config_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("Settings state has no output config path: {initial}"))?;
    let original_settings_output = fs::read(&settings_output_path).ok();
    let timeout_above_maximum = (notm_mail::MAX_SEND_TIMEOUT_SECONDS + 1).to_string();

    for (timeout, response) in [
        ("0", "apply"),
        ("-0", "save"),
        ("-1", "save"),
        ("not-a-number", "save"),
        (&timeout_above_maximum, "save"),
        ("9223372036854775807", "save"),
        ("340282366920938463463374607431768211455", "save"),
    ] {
        let rejected = first_driver.command(
            "respond_settings",
            json!({
                "response": response,
                "send_timeout_seconds": timeout,
            }),
        )?;
        assert_eq!(
            rejected["ok"], false,
            "invalid send timeout {timeout:?} unexpectedly succeeded: {rejected}"
        );
        ensure!(
            rejected["error"]
                .as_str()
                .is_some_and(|error| error.contains("send.timeout_seconds")),
            "timeout validation error was not actionable: {rejected}"
        );
        assert_eq!(
            rejected["state"]["dialog"]["visible"], true,
            "invalid timeout closed the Settings dialog: {rejected}"
        );
        assert_eq!(
            rejected["state"]["dialog"]["send_timeout_seconds"], timeout,
            "Settings did not retain the rejected text for correction: {rejected}"
        );
        assert_eq!(
            rejected["state"]["configured_send_timeout_seconds"], 73,
            "invalid timeout changed the running launch setting: {rejected}"
        );
        assert_eq!(
            fs::read(&config_path)?,
            original_config,
            "invalid timeout partially changed {}",
            config_path.display()
        );
        assert_eq!(
            fs::read(&settings_output_path).ok(),
            original_settings_output,
            "invalid timeout partially changed Settings output {}",
            settings_output_path.display()
        );
    }

    let maximum_timeout = notm_mail::MAX_SEND_TIMEOUT_SECONDS.to_string();
    let saved = first_driver.command(
        "respond_settings",
        json!({
            "response": "save",
            "send_timeout_seconds": maximum_timeout,
        }),
    )?;
    assert_eq!(saved["ok"], true, "maximum timeout did not save: {saved}");
    assert_eq!(
        saved["state"]["dialog"],
        Value::Null,
        "successful timeout Save did not close Settings: {saved}"
    );
    assert_eq!(
        saved["state"]["configured_send_timeout_seconds"], 73,
        "send timeout unexpectedly changed without the documented relaunch: {saved}"
    );
    let saved_config = fs::read_to_string(&settings_output_path)?;
    let saved_toml: toml::Value = toml::from_str(&saved_config)?;
    assert_eq!(
        saved_toml["send"]["timeout_seconds"].as_integer(),
        Some(i64::try_from(notm_mail::MAX_SEND_TIMEOUT_SECONDS)?),
        "Settings did not persist the maximum valid timeout: {saved_config}"
    );
    assert_eq!(
        fs::read(&config_path)?,
        original_config,
        "fixture Settings escaped its isolated output path"
    );
    assert_eq!(
        first_driver.command("close_main_window", json!({}))?["ok"],
        true
    );
    drop(first_driver);
    let first_status = first_app.wait_for_exit(Duration::from_secs(8))?;
    ensure!(
        first_status.success(),
        "first Settings process failed: {first_status}"
    );
    drop(first_app);

    // Fixture mode deliberately writes to a child-owned disposable config,
    // never to the supplied source. Copy those exact persisted bytes into the
    // next process's isolated input path to exercise a real reload.
    fs::write(&config_path, &saved_config)?;

    let second_token = format!("notm-settings-timeout-second-{}", unique_run_id()?);
    let mut second_app = FixtureApp::spawn_fixture_with_config(
        root.path().join("second-launch"),
        &second_token,
        &config_path,
    )?;
    let mut second_driver = second_app.connect(&second_token)?;
    second_driver.wait_for_search(STARTUP_TIMEOUT)?;
    assert_eq!(
        second_driver.command("open_settings", json!({}))?["ok"],
        true
    );
    let restarted = second_driver.command("settings_test_state", json!({}))?;
    assert_eq!(
        restarted["configured_send_timeout_seconds"],
        notm_mail::MAX_SEND_TIMEOUT_SECONDS,
        "restart did not load the last valid timeout: {restarted}"
    );
    assert_eq!(
        restarted["dialog"]["send_timeout_seconds"],
        notm_mail::MAX_SEND_TIMEOUT_SECONDS.to_string(),
        "restart did not restore the last valid timeout in Settings: {restarted}"
    );
    assert_eq!(
        fs::read_to_string(&config_path)?,
        saved_config,
        "restart changed the saved maximum timeout configuration"
    );

    let closed_dialog = second_driver.command("respond_settings", json!({"response": "close"}))?;
    assert_eq!(closed_dialog["ok"], true, "{closed_dialog}");
    assert_eq!(
        second_driver.command("close_main_window", json!({}))?["ok"],
        true
    );
    drop(second_driver);
    let second_status = second_app.wait_for_exit(Duration::from_secs(8))?;
    ensure!(
        second_status.success(),
        "restarted Settings process failed: {second_status}"
    );

    Ok(())
}

#[cfg(unix)]
#[test]
fn fixture_theme_modes_follow_both_simulated_system_preferences() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_theme_modes_follow_both_simulated_system_preferences: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running Settings theme UI smoke with {display}");

    for system_prefers_dark in [false, true] {
        let run_id = unique_run_id()?;
        let mode = if system_prefers_dark { "dark" } else { "light" };
        let work_dir = std::env::temp_dir().join(format!("notm-settings-theme-{mode}-ui-{run_id}"));
        fs::create_dir_all(&work_dir)?;
        let config_path = work_dir.join("notm.toml");
        fs::write(
            &config_path,
            "[ui]\ntheme = \"system\"\nthread_preview_lines = 2\n",
        )?;
        let token = format!("notm-settings-theme-{mode}-ui-{run_id}");
        let mut app = FixtureApp::spawn_fixture_with_config_and_system_theme(
            work_dir,
            &token,
            &config_path,
            system_prefers_dark,
        )?;
        let mut driver = app.connect(&token)?;
        driver.wait_for_search(STARTUP_TIMEOUT)?;
        assert_eq!(driver.command("open_settings", json!({}))?["ok"], true);
        let initial = driver.command("settings_test_state", json!({}))?;
        assert_eq!(initial["theme"]["requested"], "system", "{mode}: {initial}");
        assert_eq!(initial["theme"]["effective"], mode, "{mode}: {initial}");
        assert_eq!(
            initial["preview"]["configured_lines"], 2,
            "{mode}: {initial}"
        );

        let mut states = BTreeMap::new();
        for requested in ["system", "light", "dark"] {
            let applied = driver.command(
                "respond_settings",
                json!({
                    "response": "apply",
                    "theme": requested,
                    "thread_preview_lines": 2,
                    "show_thread_preview": true,
                }),
            )?;
            assert_eq!(applied["ok"], true, "{mode}/{requested}: {applied}");
            let theme = applied["state"]["theme"].clone();
            assert_eq!(theme["requested"], requested, "{mode}/{requested}: {theme}");
            let expected_effective = if requested == "system" {
                mode
            } else {
                requested
            };
            assert_eq!(
                theme["effective"], expected_effective,
                "{mode}/{requested}: resolved theme_bg_color did not match: {theme}"
            );
            ensure!(
                theme["resolved_theme_bg_color"]
                    .as_str()
                    .is_some_and(|color| !color.is_empty()),
                "{mode}/{requested}: no resolved theme_bg_color: {theme}"
            );
            ensure!(
                theme["resolved_theme_bg_luminance"].as_f64().is_some(),
                "{mode}/{requested}: no resolved luminance: {theme}"
            );
            if requested != "system" {
                assert_eq!(
                    theme["gtk_theme_name"], "Default",
                    "{mode}/{requested}: {theme}"
                );
                assert_eq!(
                    theme["gtk_application_prefer_dark_theme"],
                    requested == "dark",
                    "{mode}/{requested}: {theme}"
                );
                if !theme["gtk_interface_color_scheme"].is_null() {
                    assert_eq!(
                        theme["gtk_interface_color_scheme"], requested,
                        "{mode}/{requested}: GTK 4.20 override was not applied: {theme}"
                    );
                }
            }
            states.insert(requested, theme);
        }

        let light_luminance = states["light"]["resolved_theme_bg_luminance"]
            .as_f64()
            .context("light theme luminance")?;
        let dark_luminance = states["dark"]["resolved_theme_bg_luminance"]
            .as_f64()
            .context("dark theme luminance")?;
        ensure!(
            light_luminance > 0.5 && dark_luminance < 0.5,
            "{mode}: forced themes did not resolve to distinct supported backgrounds: {states:?}"
        );
        ensure!(
            states["light"]["resolved_theme_bg_color"] != states["dark"]["resolved_theme_bg_color"],
            "{mode}: forced themes resolved to the same color: {states:?}"
        );

        let restored = driver.command(
            "respond_settings",
            json!({
                "response": "apply",
                "theme": "system",
                "thread_preview_lines": 2,
                "show_thread_preview": true,
            }),
        )?;
        assert_eq!(restored["ok"], true, "{mode}: {restored}");
        let restored_theme = &restored["state"]["theme"];
        assert_eq!(restored_theme["requested"], "system", "{mode}: {restored}");
        assert_eq!(restored_theme["effective"], mode, "{mode}: {restored}");
        assert_eq!(
            restored_theme["gtk_application_prefer_dark_theme"], system_prefers_dark,
            "{mode}: System did not reset the legacy application override: {restored}"
        );
        if !restored_theme["gtk_interface_color_scheme"].is_null() {
            assert_eq!(
                restored_theme["gtk_interface_color_scheme"], mode,
                "{mode}: System did not restore the simulated GTK interface scheme: {restored}"
            );
        }

        let saved = driver.command(
            "respond_settings",
            json!({
                "response": "save",
                "theme": "dark",
                "thread_preview_lines": 2,
                "show_thread_preview": true,
            }),
        )?;
        assert_eq!(saved["ok"], true, "{mode}: {saved}");
        assert_eq!(saved["state"]["dialog"], Value::Null, "{mode}: {saved}");
        assert_eq!(
            saved["state"]["theme"]["requested"], "dark",
            "{mode}: {saved}"
        );
        assert_eq!(
            saved["state"]["theme"]["effective"], "dark",
            "{mode}: {saved}"
        );
        let saved_config_path = saved["state"]["app_config_path"]
            .as_str()
            .with_context(|| format!("{mode}: Save reported no app config path: {saved}"))?;
        let persisted: toml::Value = fs::read_to_string(saved_config_path)?.parse()?;
        assert_eq!(persisted["ui"]["theme"].as_str(), Some("dark"));
        assert_eq!(
            persisted["ui"]["thread_preview_lines"].as_integer(),
            Some(2)
        );
    }

    Ok(())
}

#[test]
fn fixture_near_limit_html_preparation_and_rendering_keep_gtk_responsive() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_near_limit_html_preparation_and_rendering_keep_gtk_responsive: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running near-limit HTML responsiveness UI stress with {display}");

    const HUGE_BODY_BYTES: usize = 3 * 1024 * 1024 + 768 * 1024;
    const UI_RESPONSE_LIMIT: Duration = Duration::from_millis(750);
    const PREPARED_THREAD_LIMIT: u64 = 96 * 1024 * 1024;

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-huge-html-ui-{run_id}"));
    let token = format!("notm-huge-html-ui-{run_id}");
    let mut app = FixtureApp::spawn_with_huge_body(work_dir, &token, HUGE_BODY_BYTES)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;

    assert_eq!(
        driver.command("set_fixture_thread_delay", json!({"milliseconds": 900}),)?["ok"],
        true
    );
    let preparation_started = Instant::now();
    select_first_thread(&mut driver, "id:huge-html-body@fixture.test")?;
    let initial_load = driver.command("thread_load_status", json!({}))?;
    assert_eq!(
        initial_load["busy"], true,
        "near-limit MIME preparation did not stay outstanding: {initial_load}"
    );

    let first_health = driver.command("health", json!({}))?;
    let first_heartbeat = first_health["gtk_heartbeat"].as_u64().unwrap_or(0);
    let mut last_heartbeat = first_heartbeat;
    let mut max_preparation_command = Duration::ZERO;
    let mut preparation_samples = 0_u32;
    let preparation_deadline = Instant::now() + STARTUP_TIMEOUT;
    let settled_load = loop {
        thread::sleep(Duration::from_millis(50));
        let health = responsive_harness_command(
            &mut driver,
            "health",
            json!({}),
            UI_RESPONSE_LIMIT,
            "near-limit MIME preparation",
            &mut max_preparation_command,
        )?;
        assert_eq!(health["ok"], true, "fixture app became unhealthy: {health}");
        last_heartbeat = health["gtk_heartbeat"].as_u64().unwrap_or(last_heartbeat);
        preparation_samples = preparation_samples.saturating_add(1);

        let status = responsive_harness_command(
            &mut driver,
            "thread_load_status",
            json!({}),
            UI_RESPONSE_LIMIT,
            "near-limit MIME preparation",
            &mut max_preparation_command,
        )?;
        if status["busy"] == false {
            break status;
        }
        ensure!(
            Instant::now() < preparation_deadline,
            "near-limit MIME preparation did not finish: {status}\n{}",
            app.logs()
        );
    };
    ensure!(
        preparation_samples >= 3,
        "near-limit preparation completed before sustained responsiveness sampling: samples={preparation_samples}"
    );
    ensure!(
        last_heartbeat > first_heartbeat,
        "GTK heartbeat did not advance during near-limit MIME preparation: first={first_health}, last={last_heartbeat}"
    );
    assert_eq!(settled_load["prepared_message_count"], 1, "{settled_load}");
    assert_eq!(
        settled_load["prepared_attachment_count"], 0,
        "{settled_load}"
    );
    ensure!(
        settled_load["prepared_retained_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > HUGE_BODY_BYTES as u64 && bytes < PREPARED_THREAD_LIMIT),
        "near-limit HTML payload escaped the prepared-thread byte budget: {settled_load}"
    );
    let selected = driver.command("app_state", json!({}))?;
    assert_eq!(
        selected["state"]["selected_message"]["message_id"], "huge-html-body@fixture.test",
        "near-limit fixture message was not selected: {selected}"
    );

    // The worker completion above applies the prepared near-limit text payload
    // to GTK. Re-apply it explicitly so both that bounded main-thread update
    // and a harness input queued around it are covered by the latency budget.
    let before_text = driver.command("health", json!({}))?;
    let text_started = Instant::now();
    let mut max_text_command = Duration::ZERO;
    let text = responsive_harness_command(
        &mut driver,
        "show_text_thread",
        json!({}),
        UI_RESPONSE_LIMIT,
        "near-limit GTK text rendering",
        &mut max_text_command,
    )?;
    assert_eq!(text["ok"], true, "near-limit text could not render: {text}");
    let text_view = responsive_harness_command(
        &mut driver,
        "html_view_state",
        json!({}),
        UI_RESPONSE_LIMIT,
        "near-limit GTK text rendering",
        &mut max_text_command,
    )?;
    assert_eq!(
        text_view["visible_child"], "text",
        "near-limit body was not applied to the GTK text view: {text_view}"
    );
    thread::sleep(Duration::from_millis(100));
    let after_text = responsive_harness_command(
        &mut driver,
        "health",
        json!({}),
        UI_RESPONSE_LIMIT,
        "near-limit GTK text rendering",
        &mut max_text_command,
    )?;
    ensure!(
        after_text["gtk_heartbeat"].as_u64().unwrap_or(0)
            > before_text["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat did not advance across near-limit text rendering: before={before_text}, after={after_text}"
    );
    let text_elapsed = text_started.elapsed();

    let before_webkit = driver.command("health", json!({}))?;
    let webkit_started = Instant::now();
    let mut max_webkit_command = Duration::ZERO;
    let visual = responsive_harness_command(
        &mut driver,
        "show_visual_html",
        json!({}),
        UI_RESPONSE_LIMIT,
        "near-limit WebKit rendering",
        &mut max_webkit_command,
    )?;
    assert_eq!(
        visual["ok"], true,
        "near-limit HTML document could not render: {visual}"
    );
    ensure!(
        visual["html_view"]["html_bytes"]
            .as_u64()
            .is_some_and(
                |bytes| bytes >= (HUGE_BODY_BYTES - 1024) as u64 && bytes < 4 * 1024 * 1024
            ),
        "fixture body was not near the responsive HTML limit: {visual}"
    );
    let first_load_generation = visual["html_view"]["load_generation"]
        .as_u64()
        .with_context(|| format!("near-limit WebKit load had no generation: {visual}"))?;
    ensure!(
        visual["html_view"]["loading"] == true
            || visual["html_view"]["completed_load_generation"]
                .as_u64()
                .unwrap_or(0)
                < first_load_generation,
        "near-limit WebKit load completed before it could be responsiveness-tested: {visual}"
    );

    // Supersede the still-loading near-limit document. Only this newest token
    // may publish readiness or scroll metrics after the older WebKit callbacks
    // eventually arrive.
    let replacement = responsive_harness_command(
        &mut driver,
        "show_visual_html",
        json!({}),
        UI_RESPONSE_LIMIT,
        "near-limit WebKit replacement",
        &mut max_webkit_command,
    )?;
    assert_eq!(
        replacement["ok"], true,
        "near-limit replacement could not render: {replacement}"
    );
    let load_generation = replacement["html_view"]["load_generation"]
        .as_u64()
        .with_context(|| format!("replacement WebKit load had no generation: {replacement}"))?;
    ensure!(
        load_generation > first_load_generation,
        "near-limit replacement reused the stale generation: first={visual}, replacement={replacement}"
    );

    let webkit_deadline = Instant::now() + STARTUP_TIMEOUT;
    let ready = loop {
        thread::sleep(Duration::from_millis(50));
        let health = responsive_harness_command(
            &mut driver,
            "health",
            json!({}),
            UI_RESPONSE_LIMIT,
            "near-limit WebKit rendering",
            &mut max_webkit_command,
        )?;
        assert_eq!(health["ok"], true, "fixture app became unhealthy: {health}");
        last_heartbeat = health["gtk_heartbeat"].as_u64().unwrap_or(last_heartbeat);

        let lifecycle = responsive_harness_command(
            &mut driver,
            "html_scroll_state",
            json!({}),
            UI_RESPONSE_LIMIT,
            "near-limit WebKit rendering",
            &mut max_webkit_command,
        )?;
        if lifecycle["ready"] == true
            && lifecycle["completed_generation"] == load_generation
            && lifecycle["scroll"]["canScroll"] == true
        {
            break lifecycle;
        }
        ensure!(
            Instant::now() < webkit_deadline,
            "near-limit WebKit load did not become ready and scrollable: {lifecycle}\n{}",
            app.logs()
        );
    };
    ensure!(
        last_heartbeat > before_webkit["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat did not advance while WebKit rendered the near-limit document: before={before_webkit}, after={last_heartbeat}"
    );
    thread::sleep(Duration::from_millis(250));
    let after_stale_completion = responsive_harness_command(
        &mut driver,
        "html_scroll_state",
        json!({}),
        UI_RESPONSE_LIMIT,
        "near-limit stale WebKit completion",
        &mut max_webkit_command,
    )?;
    assert_eq!(
        after_stale_completion["generation"], load_generation,
        "stale near-limit WebKit completion replaced the newest generation: {after_stale_completion}"
    );
    assert_eq!(
        after_stale_completion["completed_generation"], load_generation,
        "stale near-limit WebKit completion changed readiness: {after_stale_completion}"
    );
    assert_eq!(
        after_stale_completion["error"],
        Value::Null,
        "stale near-limit WebKit completion surfaced an error: {after_stale_completion}"
    );

    let initial_y = ready["scroll"]["y"]
        .as_f64()
        .with_context(|| format!("near-limit lifecycle had no scroll offset: {ready}"))?;
    let scroll = responsive_harness_command(
        &mut driver,
        "scroll_html_view_lines",
        json!({"lines": 8}),
        UI_RESPONSE_LIMIT,
        "near-limit WebKit input",
        &mut max_webkit_command,
    )?;
    ensure!(
        scroll["pending"] == true
            || scroll["scroll"]["y"].as_f64().unwrap_or(initial_y) > initial_y,
        "near-limit WebKit view did not accept scroll input: {scroll}"
    );
    let scroll_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let lifecycle = responsive_harness_command(
            &mut driver,
            "html_scroll_state",
            json!({}),
            UI_RESPONSE_LIMIT,
            "near-limit WebKit input",
            &mut max_webkit_command,
        )?;
        if lifecycle["scroll"]["y"]
            .as_f64()
            .is_some_and(|y| y > initial_y)
        {
            break;
        }
        ensure!(
            Instant::now() < scroll_deadline,
            "near-limit WebKit view did not process scroll input: {lifecycle}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }

    eprintln!(
        "near-limit HTML responsiveness passed: body={} bytes, retained={} bytes, preparation={:?}, preparation_samples={}, max_preparation_command={:?}, text={:?}, max_text_command={:?}, webkit={:?}, generations={}->{}, max_webkit_command={:?}",
        visual["html_view"]["html_bytes"],
        settled_load["prepared_retained_bytes"],
        preparation_started.elapsed(),
        preparation_samples,
        max_preparation_command,
        text_elapsed,
        max_text_command,
        webkit_started.elapsed(),
        first_load_generation,
        load_generation,
        max_webkit_command,
    );

    Ok(())
}

#[test]
fn fixture_fast_tag_cancels_delayed_thread_load_without_switching_visible_state()
-> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_fast_tag_cancels_delayed_thread_load_without_switching_visible_state: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running thread-load/tag cancellation UI regression with {display}");

    const DELAYED_LOAD_MS: u64 = 1_200;
    const STALE_COMPLETION_WINDOW: Duration = Duration::from_millis(1_400);
    const UI_RESPONSE_LIMIT: Duration = Duration::from_millis(500);

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-thread-tag-race-ui-{run_id}"));
    let token = format!("notm-thread-tag-race-ui-{run_id}");
    let mutation_tag = format!("thread-load-race-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;

    let query = "subject:\"Read inbox message\" or subject:\"Unread inbox message\"";
    let scheduled = driver.command("run_search", json!({"query": query}))?;
    assert_eq!(
        scheduled["scheduled"], true,
        "thread-load/tag fixture search was not scheduled: {scheduled}"
    );
    let search = driver.wait_for_search(STARTUP_TIMEOUT)?;
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;
    let rows = json_array_at(&search, &["state", "thread_list_items"])?;
    ensure!(
        rows.len() == 2,
        "thread-load/tag fixture query did not return two rows: {search}"
    );
    let initial_index = rows
        .iter()
        .position(|row| row["subject"] == "Read inbox message")
        .with_context(|| format!("initial thread was missing: {search}"))?;
    let delayed_index = rows
        .iter()
        .position(|row| row["subject"] == "Unread inbox message")
        .with_context(|| format!("delayed thread was missing: {search}"))?;
    let initial_thread_id = rows[initial_index]["thread_id"]
        .as_str()
        .with_context(|| format!("initial row had no thread ID: {}", rows[initial_index]))?
        .to_string();
    let delayed_thread_id = rows[delayed_index]["thread_id"]
        .as_str()
        .with_context(|| format!("delayed row had no thread ID: {}", rows[delayed_index]))?
        .to_string();

    let selected = driver.command("select_thread_by_index", json!({"index": initial_index}))?;
    assert_eq!(
        selected["ok"], true,
        "initial thread selection was not scheduled: {selected}"
    );
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;
    let selected_message = driver.command("select_message_by_index", json!({"index": 0}))?;
    assert_eq!(
        selected_message["ok"], true,
        "initial message could not be selected: {selected_message}"
    );
    let before_state = driver.command("app_state", json!({}))?;
    let before_view = driver.command("message_view_text", json!({}))?;
    let before_selection = driver.command("thread_selection_view_state", json!({}))?;
    assert_eq!(
        before_state["state"]["selected_thread"]["thread_id"], initial_thread_id,
        "initial thread did not settle before the race: {before_state}"
    );
    let initial_message_id = before_state["state"]["selected_message"]["message_id"]
        .as_str()
        .with_context(|| format!("initial state had no selected message: {before_state}"))?
        .to_string();
    let before_message_ids = json_array_at(&before_state, &["state", "messages"])?
        .iter()
        .map(|message| {
            message["message_id"]
                .as_str()
                .map(ToOwned::to_owned)
                .with_context(|| format!("loaded message had no ID: {message}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(
        !before_message_ids.is_empty(),
        "initial thread had no prepared messages: {before_state}"
    );
    ensure!(
        before_view["text"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "initial message had no visible prepared text: {before_view}"
    );
    assert_eq!(
        before_selection["selected_local"].as_u64(),
        Some(initial_index as u64),
        "GTK did not settle on the initial row: {before_selection}"
    );
    let loader_before = driver.command("thread_load_status", json!({}))?;
    let cancelled_before = loader_before["cancelled"].as_u64().unwrap_or(0);

    let delayed = driver.command(
        "set_fixture_thread_delay",
        json!({"milliseconds": DELAYED_LOAD_MS}),
    )?;
    assert_eq!(
        delayed["ok"], true,
        "could not delay thread load: {delayed}"
    );
    let delayed_selection =
        driver.command("select_thread_by_index", json!({"index": delayed_index}))?;
    assert_eq!(
        delayed_selection["ok"], true,
        "delayed target selection was not scheduled: {delayed_selection}"
    );
    let delayed_status = driver.command("thread_load_status", json!({}))?;
    assert_eq!(
        delayed_status["busy"], true,
        "delayed target loader was not active: {delayed_status}"
    );
    let delayed_generation = delayed_status["generation"]
        .as_u64()
        .with_context(|| format!("delayed loader had no generation: {delayed_status}"))?;
    let retained = driver.command("app_state", json!({}))?;
    assert_eq!(
        retained["state"]["selected_thread"]["thread_id"], initial_thread_id,
        "delayed selection replaced retained state before preparation: {retained}"
    );
    assert_eq!(
        retained["state"]["selected_message"]["message_id"], initial_message_id,
        "delayed selection replaced the retained message before preparation: {retained}"
    );
    driver.command("set_fixture_thread_delay", json!({"milliseconds": 0}))?;

    let health_started = Instant::now();
    let health_before = driver.command("health", json!({}))?;
    let health_elapsed = health_started.elapsed();
    ensure!(
        health_elapsed < UI_RESPONSE_LIMIT,
        "health blocked behind delayed thread load for {health_elapsed:?}: {health_before}"
    );
    assert_eq!(
        health_before["thread_load"]["generation"], delayed_generation,
        "health did not report the delayed loader generation: {health_before}"
    );

    let tag_started = Instant::now();
    let tagged = driver.command("tag_selected", json!({"add": [&mutation_tag]}))?;
    let tag_elapsed = tag_started.elapsed();
    ensure!(
        tag_elapsed < UI_RESPONSE_LIMIT,
        "fast tag scheduling blocked for {tag_elapsed:?}: {tagged}"
    );
    assert_eq!(tagged["ok"], true, "fast tag was rejected: {tagged}");
    assert_eq!(
        tagged["pending"], true,
        "fast tag did not run asynchronously: {tagged}"
    );
    let tag_completion = wait_for_tag(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        tag_completion["state"]["last_error"],
        Value::Null,
        "fast tag did not complete cleanly: {tag_completion}\n{}",
        app.logs()
    );

    // The pre-fix worker ignores the tag refresh and publishes after its full
    // fixture delay. Keep one explicit bounded stale-completion window so that
    // ordering bug cannot hide behind an early assertion.
    thread::sleep(STALE_COMPLETION_WINDOW);

    let final_health_started = Instant::now();
    let final_health = driver.command("health", json!({}))?;
    let final_health_elapsed = final_health_started.elapsed();
    ensure!(
        final_health_elapsed < UI_RESPONSE_LIMIT,
        "health blocked after tag/load cancellation for {final_health_elapsed:?}: {final_health}"
    );
    ensure!(
        final_health["gtk_heartbeat"].as_u64().unwrap_or(0)
            > health_before["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat did not advance through the tag/load race: before={health_before}, after={final_health}"
    );
    let final_loader = driver.command("thread_load_status", json!({}))?;
    assert_eq!(
        final_loader["busy"], false,
        "thread loader remained active after the stale-completion window: {final_loader}"
    );
    ensure!(
        final_loader["cancelled"].as_u64().unwrap_or(0) > cancelled_before,
        "tag mutation did not cancel the delayed loader: before={loader_before}, after={final_loader}"
    );

    let final_state = driver.command("app_state", json!({}))?;
    let final_view = driver.command("message_view_text", json!({}))?;
    let final_selection = driver.command("thread_selection_view_state", json!({}))?;
    assert_eq!(
        final_state["state"]["selected_thread"]["thread_id"], initial_thread_id,
        "stale delayed load replaced the retained tagged thread: {final_state}"
    );
    assert_eq!(
        final_state["state"]["selected_message"]["message_id"], initial_message_id,
        "stale delayed load replaced the retained tagged message: {final_state}"
    );
    let final_message_ids = json_array_at(&final_state, &["state", "messages"])?
        .iter()
        .map(|message| {
            message["message_id"]
                .as_str()
                .map(ToOwned::to_owned)
                .with_context(|| format!("final loaded message had no ID: {message}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    assert_eq!(
        final_message_ids, before_message_ids,
        "stale delayed load replaced the visible prepared message set: {final_state}"
    );
    assert_eq!(
        final_view["text"], before_view["text"],
        "stale delayed load replaced the visible prepared text: before={before_view}, after={final_view}"
    );
    assert_eq!(
        final_selection["selected_local"].as_u64(),
        Some(initial_index as u64),
        "GTK selection did not return to the retained tagged row: {final_selection}"
    );

    let final_rows = json_array_at(&final_state, &["state", "thread_list_items"])?;
    let row_has_tag = |thread_id: &str| -> anyhow::Result<bool> {
        let row = final_rows
            .iter()
            .find(|row| row["thread_id"].as_str() == Some(thread_id))
            .with_context(|| format!("final result omitted thread {thread_id}: {final_state}"))?;
        Ok(row["tags"]
            .as_array()
            .is_some_and(|tags| tags.iter().any(|tag| tag == &mutation_tag)))
    };
    assert!(
        row_has_tag(&initial_thread_id)?,
        "fast tag missed the retained exact thread: {final_state}"
    );
    assert!(
        !row_has_tag(&delayed_thread_id)?,
        "fast tag leaked onto the delayed target: {final_state}"
    );

    Ok(())
}

#[test]
fn fixture_delayed_thread_loading_is_generation_safe_and_responsive() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_delayed_thread_loading_is_generation_safe_and_responsive: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running delayed thread-loading UI stress with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-thread-loader-ui-{run_id}"));
    let token = format!("notm-thread-loader-ui-{run_id}");
    const LARGE_ATTACHMENT_BYTES: usize = 6 * 1024 * 1024;
    let mut app =
        FixtureApp::spawn_with_large_attachment(work_dir, &token, LARGE_ATTACHMENT_BYTES)?;
    let mut driver = app.connect(&token)?;
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;

    let scheduled = driver.command("run_search", json!({"query": "*"}))?;
    assert_eq!(
        scheduled["scheduled"], true,
        "search was not scheduled: {scheduled}"
    );
    let search = driver.wait_for_search(STARTUP_TIMEOUT)?;
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;
    let current_thread_id =
        driver.command("app_state", json!({}))?["state"]["selected_thread"]["thread_id"]
            .as_str()
            .map(ToOwned::to_owned);
    let rows = json_array_at(&search, &["state", "thread_list_items"])?;
    let candidates = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            let is_draft = row["tags"]
                .as_array()
                .is_some_and(|tags| tags.iter().any(|tag| tag == "draft"));
            let is_current = row["thread_id"]
                .as_str()
                .is_some_and(|thread_id| current_thread_id.as_deref() == Some(thread_id));
            !is_draft && !is_current
        })
        .take(2)
        .map(|(index, row)| {
            Ok((
                index,
                row["thread_id"]
                    .as_str()
                    .with_context(|| format!("thread row had no id: {row}"))?
                    .to_string(),
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    ensure!(
        candidates.len() == 2,
        "fixture search did not provide two non-draft thread-switch targets: {search}"
    );

    let delayed = driver.command("set_fixture_thread_delay", json!({"milliseconds": 1200}))?;
    assert_eq!(
        delayed["ok"], true,
        "could not delay thread loader: {delayed}"
    );
    driver.command("select_thread_by_index", json!({"index": candidates[0].0}))?;
    let first_load = driver.command("thread_load_status", json!({}))?;
    assert_eq!(
        first_load["busy"], true,
        "first delayed load was not active: {first_load}"
    );
    let first_generation = first_load["generation"]
        .as_u64()
        .with_context(|| format!("first delayed load had no generation: {first_load}"))?;

    driver.command("set_fixture_thread_delay", json!({"milliseconds": 300}))?;
    driver.command("select_thread_by_index", json!({"index": candidates[1].0}))?;
    let final_load = driver.command("thread_load_status", json!({}))?;
    assert_eq!(
        final_load["busy"], true,
        "final delayed load was not active: {final_load}"
    );
    let final_generation = final_load["generation"]
        .as_u64()
        .with_context(|| format!("final delayed load had no generation: {final_load}"))?;
    ensure!(
        final_generation > first_generation,
        "rapid selection did not invalidate the earlier generation: first={first_load}, final={final_load}"
    );

    let first_health_started = Instant::now();
    let first_health = driver.command("health", json!({}))?;
    let first_health_elapsed = first_health_started.elapsed();
    thread::sleep(Duration::from_millis(150));
    let second_health_started = Instant::now();
    let second_health = driver.command("health", json!({}))?;
    let second_health_elapsed = second_health_started.elapsed();
    ensure!(
        first_health_elapsed < Duration::from_millis(500)
            && second_health_elapsed < Duration::from_millis(500),
        "health blocked behind delayed thread work: first={first_health_elapsed:?}, second={second_health_elapsed:?}"
    );
    ensure!(
        second_health["gtk_heartbeat"].as_u64().unwrap_or(0)
            > first_health["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat did not advance during delayed thread work: first={first_health}, second={second_health}"
    );
    assert_eq!(
        second_health["thread_load"]["generation"], final_generation,
        "health did not report the active final generation: {second_health}"
    );

    wait_for_thread_load_idle(&mut driver, Duration::from_secs(5))?;
    let settled = driver.command("app_state", json!({}))?;
    assert_eq!(
        settled["state"]["selected_thread"]["thread_id"], candidates[1].1,
        "newest delayed selection did not win: {settled}"
    );
    thread::sleep(Duration::from_millis(1100));
    let after_stale_completion = driver.command("app_state", json!({}))?;
    assert_eq!(
        after_stale_completion["state"]["selected_thread"]["thread_id"], candidates[1].1,
        "stale delayed completion replaced the final selection: {after_stale_completion}"
    );
    let stale_status = driver.command("thread_load_status", json!({}))?;
    assert_eq!(
        stale_status["busy"], false,
        "stale completion reactivated the loader: {stale_status}"
    );
    assert_eq!(
        stale_status["peak_active_preparations"], 1,
        "rapid switching prepared multiple thread payloads concurrently: {stale_status}"
    );
    ensure!(
        stale_status["cancelled"].as_u64().unwrap_or(0) >= 1,
        "rapid switching did not cancel stale preparation work: {stale_status}"
    );

    driver.command("set_fixture_thread_delay", json!({"milliseconds": 900}))?;
    select_first_thread(&mut driver, "id:attachment-heavy-0@fixture.test")?;
    let heavy_load = driver.command("thread_load_status", json!({}))?;
    assert_eq!(
        heavy_load["busy"], true,
        "attachment-heavy load was not delayed: {heavy_load}"
    );
    ensure!(
        heavy_load["generation"].as_u64().unwrap_or(0) > final_generation,
        "attachment-heavy load did not use a fresh generation: {heavy_load}"
    );
    let heavy_health_before = driver.command("health", json!({}))?;
    thread::sleep(Duration::from_millis(150));
    let heavy_health_after = driver.command("health", json!({}))?;
    ensure!(
        heavy_health_after["gtk_heartbeat"].as_u64().unwrap_or(0)
            > heavy_health_before["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat did not advance during attachment-heavy loading: before={heavy_health_before}, after={heavy_health_after}"
    );
    assert_eq!(
        heavy_health_after["thread_load"]["busy"], true,
        "attachment-heavy loader finished before the responsiveness assertion: {heavy_health_after}"
    );
    let bounded_heavy_load = wait_for_thread_load_idle(&mut driver, Duration::from_secs(5))?;
    ensure!(
        bounded_heavy_load["prepared_retained_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0 && bytes < LARGE_ATTACHMENT_BYTES as u64),
        "decoded large-attachment payload appears resident in prepared content: {bounded_heavy_load}"
    );
    assert_eq!(
        bounded_heavy_load["prepared_attachment_count"], 72,
        "attachment-heavy metadata count was not bounded/reported: {bounded_heavy_load}"
    );
    assert_eq!(
        bounded_heavy_load["peak_active_preparations"], 1,
        "attachment-heavy preparation overlapped another payload producer: {bounded_heavy_load}"
    );

    let heavy_state = driver.command("app_state", json!({}))?;
    let heavy_messages = json_array_at(&heavy_state, &["state", "messages"])?;
    assert_eq!(
        heavy_messages.len(),
        3,
        "attachment-heavy thread did not load all messages: {heavy_state}"
    );
    ensure!(
        heavy_messages
            .iter()
            .any(|message| message["message_id"] == "attachment-heavy-0@fixture.test"),
        "explicit attachment-heavy target was missing: {heavy_state}"
    );
    let listed = driver.command("attachment_list_items", json!({}))?;
    assert_eq!(
        json_array_at(&listed, &["attachments"])?.len(),
        72,
        "attachment-heavy metadata was incomplete: {listed}"
    );
    let row_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let attachment_state = driver.command("attachment_test_state", json!({}))?;
        if attachment_state["row_count"] == 72 {
            break;
        }
        ensure!(
            Instant::now() < row_deadline,
            "attachment rows were not incrementally completed: {attachment_state}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }

    let downloads = app.work_dir.join("large-attachment-downloads");
    fs::create_dir_all(&downloads)?;
    driver.command("set_fixture_attachment_delay", json!({"milliseconds": 600}))?;
    let before_lazy_save = driver.command("health", json!({}))?;
    let lazy_save = driver.command(
        "save_selected_attachment",
        json!({"index": 0, "dir": downloads}),
    )?;
    assert_eq!(
        lazy_save["pending"], true,
        "large lazy attachment save did not start asynchronously: {lazy_save}"
    );
    thread::sleep(Duration::from_millis(150));
    let during_lazy_save = driver.command("health", json!({}))?;
    ensure!(
        during_lazy_save["gtk_heartbeat"].as_u64().unwrap_or(0)
            > before_lazy_save["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat stopped while lazily reading/decoding the large attachment: before={before_lazy_save}, during={during_lazy_save}"
    );
    assert_eq!(
        during_lazy_save["attachment_io"]["busy"], true,
        "large lazy attachment save finished before responsiveness was measured: {during_lazy_save}"
    );
    let saved = wait_for_attachment_io_idle(&mut driver, STARTUP_TIMEOUT)?;
    let saved_path = saved["last_completion"]["path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("large lazy save returned no path: {saved}"))?;
    assert_eq!(
        fs::metadata(&saved_path)?.len(),
        LARGE_ATTACHMENT_BYTES as u64,
        "large lazy save did not extract the requested payload: {saved}"
    );

    driver.command("set_fixture_thread_delay", json!({"milliseconds": 0}))?;
    driver.command("run_search", json!({"query": "*"}))?;
    let composer_search = driver.wait_for_search(STARTUP_TIMEOUT)?;
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;
    let before_composer = driver.command("app_state", json!({}))?;
    let before_composer_thread = before_composer["state"]["selected_thread"]["thread_id"]
        .as_str()
        .map(ToOwned::to_owned);
    let composer_target = json_array_at(&composer_search, &["state", "thread_list_items"])?
        .iter()
        .enumerate()
        .find(|(_, row)| {
            let is_draft = row["tags"]
                .as_array()
                .is_some_and(|tags| tags.iter().any(|tag| tag == "draft"));
            let is_current = row["thread_id"]
                .as_str()
                .is_some_and(|thread_id| before_composer_thread.as_deref() == Some(thread_id));
            !is_draft && !is_current
        })
        .map(|(index, row)| (index, row["thread_id"].clone()))
        .with_context(|| format!("no delayed preview target remained: {composer_search}"))?;
    let before_cancel = driver.command("thread_load_status", json!({}))?;
    driver.command("set_fixture_thread_delay", json!({"milliseconds": 800}))?;
    driver.command(
        "select_thread_by_index",
        json!({"index": composer_target.0}),
    )?;
    let delayed_preview = driver.command("thread_load_status", json!({}))?;
    assert_eq!(
        delayed_preview["busy"], true,
        "composer cancellation regression did not start a delayed preview: {delayed_preview}"
    );
    let before_typing = driver.command("health", json!({}))?;
    assert_eq!(driver.command("open_compose", json!({}))?["ok"], true);
    assert_eq!(
        driver.command(
            "compose_set_subject",
            json!({"value": "Typing cancels delayed preview"}),
        )?["ok"],
        true
    );
    assert_eq!(
        driver.command(
            "compose_set_body",
            json!({"value": "A stale thread completion must not replace this composer."}),
        )?["ok"],
        true
    );
    thread::sleep(Duration::from_millis(175));
    let during_typing = driver.command("health", json!({}))?;
    ensure!(
        during_typing["gtk_heartbeat"].as_u64().unwrap_or(0)
            > before_typing["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat stopped while typing cancelled a delayed preview: before={before_typing}, during={during_typing}"
    );
    thread::sleep(Duration::from_millis(850));
    let after_cancel = driver.command("thread_load_status", json!({}))?;
    assert_eq!(
        after_cancel["busy"], false,
        "cancelled delayed preview remained active: {after_cancel}"
    );
    ensure!(
        after_cancel["cancelled"].as_u64().unwrap_or(0)
            > before_cancel["cancelled"].as_u64().unwrap_or(0),
        "opening and typing in the composer did not cancel the delayed preview: before={before_cancel}, after={after_cancel}"
    );
    let after_typing = driver.command("app_state", json!({}))?;
    assert_eq!(
        after_typing["state"]["compose_fields"]["subject"], "Typing cancels delayed preview",
        "stale preview replaced the newer composer: target={}, state={after_typing}",
        composer_target.1
    );
    assert_eq!(
        after_typing["state"]["selected_thread"]["thread_id"],
        before_composer_thread
            .map(Value::String)
            .unwrap_or(Value::Null),
        "cancelled preview changed the selected thread after its worker settled: {after_typing}"
    );
    let pending = driver.command("pending_confirmation", json!({}))?;
    assert_eq!(
        pending["pending"],
        Value::Null,
        "stale preview reached the dirty-composer replacement workflow after cancellation: {pending}"
    );

    Ok(())
}

#[test]
fn fixture_slow_composer_preparation_is_generation_safe_and_responsive() -> anyhow::Result<()> {
    let Some(display) = gtk_display_environment()? else {
        eprintln!(
            "SKIP fixture_slow_composer_preparation_is_generation_safe_and_responsive: no GUI test display is available"
        );
        return Ok(());
    };
    eprintln!("running async composer-preparation UI smoke with {display}");

    let run_id = unique_run_id()?;
    let work_dir = std::env::temp_dir().join(format!("notm-composer-preparation-ui-{run_id}"));
    let token = format!("notm-composer-preparation-ui-{run_id}");
    let mut app = FixtureApp::spawn(work_dir, &token)?;
    let mut driver = app.connect(&token)?;
    select_first_thread(&mut driver, "subject:\"Three message thread\"")?;

    driver.command(
        "set_fixture_composer_preparation_delay",
        json!({"milliseconds": 800}),
    )?;
    let started = Instant::now();
    let slow_reply = driver.command("reply_selected", json!({}))?;
    assert_eq!(
        slow_reply["ok"], true,
        "slow reply failed to start: {slow_reply}"
    );
    assert_eq!(
        slow_reply["pending"], true,
        "slow reply was not asynchronous: {slow_reply}"
    );
    ensure!(
        started.elapsed() < Duration::from_millis(500),
        "slow reply preparation blocked its command response for {:?}: {slow_reply}",
        started.elapsed()
    );
    let before = driver.command("health", json!({}))?;
    assert_eq!(
        before["composer_preparation"]["busy"], true,
        "health did not report slow composer preparation: {before}"
    );
    driver.command(
        "compose_set_subject",
        json!({"value": "Newer typing must win"}),
    )?;
    thread::sleep(Duration::from_millis(175));
    let after = driver.command("health", json!({}))?;
    ensure!(
        after["gtk_heartbeat"].as_u64().unwrap_or(0)
            > before["gtk_heartbeat"].as_u64().unwrap_or(0),
        "GTK heartbeat stopped during composer preparation: before={before}, after={after}"
    );
    let stale = wait_for_composer_preparation_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        stale["outcome"], "superseded",
        "newer typing did not supersede slow preparation: {stale}"
    );
    let state = driver.command("app_state", json!({}))?;
    assert_eq!(
        state["state"]["compose_fields"]["subject"], "Newer typing must win",
        "stale reply preparation replaced newer typing: {state}"
    );

    driver.command("compose_set_subject", json!({"value": ""}))?;
    driver.command(
        "set_fixture_composer_preparation_delay",
        json!({"milliseconds": 700}),
    )?;
    let first = driver.command("reply_selected", json!({}))?;
    assert_eq!(
        first["pending"], true,
        "first rapid response was not pending: {first}"
    );
    driver.command(
        "set_fixture_composer_preparation_delay",
        json!({"milliseconds": 0}),
    )?;
    let second = driver.command("forward_selected", json!({}))?;
    assert_eq!(
        second["pending"], true,
        "newer rapid response was not pending: {second}"
    );
    let latest = wait_for_composer_preparation_idle(&mut driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        latest["outcome"], "prepared",
        "newer response did not win: {latest}"
    );
    let state = driver.command("app_state", json!({}))?;
    ensure!(
        state["state"]["compose_fields"]["subject"]
            .as_str()
            .is_some_and(|subject| subject.starts_with("Fwd:")),
        "newer forward was not applied: {state}"
    );
    thread::sleep(Duration::from_millis(850));
    let after_stale_worker = driver.command("app_state", json!({}))?;
    ensure!(
        after_stale_worker["state"]["compose_fields"]["subject"]
            .as_str()
            .is_some_and(|subject| subject.starts_with("Fwd:")),
        "older slow reply overtook the newer forward: {after_stale_worker}"
    );

    Ok(())
}

fn wait_for_thread_load_idle(driver: &mut UiDriver, timeout: Duration) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = driver.command("thread_load_status", json!({}))?;
        ensure!(status["ok"] == true, "thread load status failed: {status}");
        if status["busy"] == false {
            return Ok(status);
        }
        ensure!(
            Instant::now() < deadline,
            "thread load did not become idle within {timeout:?}: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

fn responsive_harness_command(
    driver: &mut UiDriver,
    command: &str,
    args: Value,
    limit: Duration,
    phase: &str,
    max_elapsed: &mut Duration,
) -> anyhow::Result<Value> {
    let started = Instant::now();
    let response = driver.command(command, args)?;
    let elapsed = started.elapsed();
    *max_elapsed = (*max_elapsed).max(elapsed);
    ensure!(
        elapsed < limit,
        "GTK input {command:?} blocked for {elapsed:?} during {phase}; response={response}"
    );
    Ok(response)
}

fn wait_for_composer_preparation_idle(
    driver: &mut UiDriver,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = driver.command("composer_preparation_status", json!({}))?;
        ensure!(
            status["ok"] == true,
            "composer preparation status failed: {status}"
        );
        if status["busy"] == false {
            return Ok(status);
        }
        ensure!(
            Instant::now() < deadline,
            "composer preparation did not become idle within {timeout:?}: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

fn wait_for_composer_preparation_generation(
    driver: &mut UiDriver,
    requested_generation: u64,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = driver.command("composer_preparation_status", json!({}))?;
        ensure!(
            status["ok"] == true,
            "composer preparation status failed: {status}"
        );
        let busy = status["busy"]
            .as_bool()
            .with_context(|| format!("composer preparation status had no busy flag: {status}"))?;
        let active_generation = status["generation"].as_u64();
        let completed_generation = status["completed_generation"].as_u64();
        if !busy && completed_generation == Some(requested_generation) {
            return Ok(status);
        }
        ensure!(
            !active_generation.is_some_and(|generation| generation > requested_generation)
                && !completed_generation
                    .is_some_and(|generation| generation > requested_generation),
            "composer preparation generation {requested_generation} was superseded: {status}"
        );
        ensure!(
            busy || !matches!(
                status["outcome"].as_str(),
                Some("cancelled" | "superseded" | "failed" | "rejected")
            ),
            "composer preparation generation {requested_generation} became idle without completing: {status}"
        );
        ensure!(
            Instant::now() < deadline,
            "composer preparation generation {requested_generation} did not complete within {timeout:?}: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

fn wait_for_attachment_io_idle(driver: &mut UiDriver, timeout: Duration) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = driver.command("attachment_io_status", json!({}))?;
        ensure!(
            status["ok"] == true,
            "attachment I/O status failed: {status}"
        );
        if status["busy"] == false {
            return Ok(status);
        }
        ensure!(
            Instant::now() < deadline,
            "attachment I/O did not become idle within {timeout:?}: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

fn wait_for_composer_attachment_cache_idle(
    driver: &mut UiDriver,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = driver.command("attachment_io_status", json!({}))?;
        ensure!(
            status["ok"] == true,
            "attachment I/O status failed: {status}"
        );
        if status["composer_cache"]["busy"] == false {
            return Ok(status);
        }
        ensure!(
            Instant::now() < deadline,
            "composer attachment cache did not become idle within {timeout:?}: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

fn wait_for_new_composer_attachment_cache(
    driver: &mut UiDriver,
    previous_generation: u64,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = driver.command("attachment_io_status", json!({}))?;
        ensure!(
            status["ok"] == true,
            "attachment I/O status failed: {status}"
        );
        let cache = &status["composer_cache"];
        let latest_generation = cache["latest_generation"].as_u64().with_context(|| {
            format!("composer attachment cache had no latest generation: {status}")
        })?;
        let completed_generation = cache["completed_generation"].as_u64();
        let busy = cache["busy"]
            .as_bool()
            .with_context(|| format!("composer attachment cache had no busy flag: {status}"))?;
        if latest_generation > previous_generation
            && completed_generation == Some(latest_generation)
            && !busy
        {
            return Ok(status);
        }
        ensure!(
            busy || latest_generation == previous_generation
                || !matches!(cache["outcome"].as_str(), Some("cancelled" | "failed")),
            "composer attachment cache generation after {previous_generation} became idle without completing: {status}"
        );
        ensure!(
            Instant::now() < deadline,
            "composer attachment cache did not complete a generation after {previous_generation} within {timeout:?}: {status}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

fn select_first_thread(driver: &mut UiDriver, query: &str) -> anyhow::Result<()> {
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    driver.command("thread_selection_view_state", json!({}))?;
    let scheduled = driver.command("run_search", json!({"query": query}))?;
    ensure!(
        scheduled["scheduled"] == true,
        "fixture search was not scheduled: {scheduled}"
    );
    let search = driver.wait_for_search(STARTUP_TIMEOUT)?;
    driver.command("thread_selection_view_state", json!({}))?;
    let rows = json_array_at(&search, &["state", "thread_list_items"])?;
    ensure!(rows.len() == 1, "expected one fixture thread: {search}");
    let selected = driver.command("select_thread_by_index", json!({"index": 0}))?;
    assert_eq!(
        selected["ok"], true,
        "could not select fixture thread: {selected}"
    );
    Ok(())
}

fn wait_for_tag(driver: &mut UiDriver, timeout: Duration) -> anyhow::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let status = driver.command("tag_status", json!({}))?;
        if status["in_progress"] == false {
            let state = driver.command("app_state", json!({}))?;
            if state["state"]["search_loading"] == true {
                return driver.wait_for_search(timeout);
            }
            return Ok(state);
        }
        ensure!(
            Instant::now() < deadline,
            "tag operation did not finish within {timeout:?}: {status}"
        );
        let health = driver.command("health", json!({}))?;
        ensure!(
            health["ok"] == true,
            "app became unresponsive during tag: {health}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn index_remote_html_message(
    database_path: &Path,
    notmuch_config_path: &Path,
    message_id: &str,
    from: &str,
    subject: &str,
    html: &str,
) -> anyhow::Result<()> {
    let filename = message_id.replace(['@', '<', '>'], "-");
    let path = database_path
        .join("account.fixture/cur")
        .join(format!("remote-image-{filename}-{}:2,S", unique_run_id()?));
    let raw = format!(
        "From: {from}\r\nTo: fixture@example.test\r\nSubject: {subject}\r\n\
         Date: Tue, 25 Aug 2026 12:00:00 -0600\r\nMessage-ID: <{message_id}>\r\n\
         MIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{html}"
    );
    fs::write(&path, raw)
        .with_context(|| format!("writing remote-image fixture {}", path.display()))?;
    let open = notm_notmuch::OpenConfig {
        database_path: Some(database_path.to_path_buf()),
        config_path: Some(notmuch_config_path.to_path_buf()),
        profile: None,
    };
    notm_notmuch::Database::open(&open, notm_notmuch::DatabaseMode::ReadWrite)?
        .index_file_with_tags(&path, &["inbox"])
        .with_context(|| format!("indexing remote-image fixture {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn index_related_cid_message(
    database_path: &Path,
    notmuch_config_path: &Path,
    tracker: &LocalHttpTracker,
) -> anyhow::Result<()> {
    const TINY_JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/2wBDAQkJCQwLDBgNDRgyIRwhMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjL/wAARCAACAAIDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwD3+iiigD//2Q==";
    let path = database_path
        .join("account.fixture/cur")
        .join(format!("remote-image-related-cid-{}:2,S", unique_run_id()?));
    let mut raw = String::from(
        "From: not a valid mailbox ???\r\n\
         To: fixture@example.test\r\n\
         Subject: Related CID image scans\r\n\
         Date: Tue, 25 Aug 2026 11:59:00 -0600\r\n\
         Message-ID: <remote-image-related-cid@fixture.test>\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/related; boundary=related\r\n\r\n\
         --related\r\n\
         Content-Type: multipart/alternative; boundary=alternative\r\n\r\n\
         --alternative\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n\
         Seven message-local scans.\r\n\
         --alternative\r\n\
         Content-Type: text/html; charset=utf-8\r\n\r\n\
         <html><body><p>Seven message-local scans.</p>",
    );
    for index in 0..7 {
        raw.push_str(&format!(
            "<img src=\"cid:scan-{index}@fixture.test\" alt=\"scan {index}\">"
        ));
    }
    raw.push_str(&format!(
        "<img src=\"{}\" alt=\"remote tracker\"></body></html>\r\n\
         --alternative--\r\n",
        tracker.url("/cid-remote")
    ));
    for index in 0..7 {
        raw.push_str(&format!(
            "--related\r\n\
             Content-Type: image/jpeg; name=scan-{index}.jpg\r\n\
             Content-Disposition: inline; filename=scan-{index}.jpg\r\n\
             Content-ID: <scan-{index}@fixture.test>\r\n\
             Content-Transfer-Encoding: base64\r\n\r\n\
             {TINY_JPEG}\r\n"
        ));
    }
    raw.push_str("--related--\r\n");
    fs::write(&path, raw)
        .with_context(|| format!("writing related CID fixture {}", path.display()))?;
    let open = notm_notmuch::OpenConfig {
        database_path: Some(database_path.to_path_buf()),
        config_path: Some(notmuch_config_path.to_path_buf()),
        profile: None,
    };
    notm_notmuch::Database::open(&open, notm_notmuch::DatabaseMode::ReadWrite)?
        .index_file_with_tags(&path, &["inbox"])
        .with_context(|| format!("indexing related CID fixture {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn index_standalone_remote_policy_thread(
    database_path: &Path,
    notmuch_config_path: &Path,
    tracker: &LocalHttpTracker,
) -> anyhow::Result<()> {
    let maildir = database_path.join("account.fixture/cur");
    let root_path = maildir.join(format!("standalone-policy-root-{}:2,S", unique_run_id()?));
    let reply_path = maildir.join(format!("standalone-policy-reply-{}:2,S", unique_run_id()?));
    fs::write(
        &root_path,
        format!(
            "From: Policy Sender <policy@example.test>\r\nTo: fixture@example.test\r\n\
             Subject: Standalone remote policy\r\nDate: Tue, 25 Aug 2026 12:00:00 -0600\r\n\
             Message-ID: <standalone-policy-html-root@fixture.test>\r\nMIME-Version: 1.0\r\n\
             Content-Type: text/html; charset=utf-8\r\n\r\n\
             <html><body><p>Standalone HTML root.</p><img src=\"{}\" alt=\"tracked\"></body></html>",
            tracker.url("/standalone-policy")
        ),
    )?;
    fs::write(
        &reply_path,
        "From: Policy Sender <policy@example.test>\r\nTo: fixture@example.test\r\n\
         Subject: Re: Standalone remote policy\r\nDate: Tue, 25 Aug 2026 12:01:00 -0600\r\n\
         Message-ID: <standalone-policy-text-reply@fixture.test>\r\n\
         In-Reply-To: <standalone-policy-html-root@fixture.test>\r\n\
         References: <standalone-policy-html-root@fixture.test>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nRequest-free text reply.",
    )?;

    let open = notm_notmuch::OpenConfig {
        database_path: Some(database_path.to_path_buf()),
        config_path: Some(notmuch_config_path.to_path_buf()),
        profile: None,
    };
    let database = notm_notmuch::Database::open(&open, notm_notmuch::DatabaseMode::ReadWrite)?;
    database.index_file_with_tags(&root_path, &["inbox"])?;
    database.index_file_with_tags(&reply_path, &["inbox"])?;
    Ok(())
}

#[cfg(unix)]
fn remote_image_adversarial_html(tracker: &LocalHttpTracker) -> String {
    format!(
        r#"<!doctype html>
<html>
<head>
  <style>@import url("{css_import}"); body {{ background-image: url("{style_block}"); }}</style>
  <link rel="stylesheet" href="{stylesheet}">
  <meta http-equiv="refresh" content="0; url={meta_refresh}">
</head>
<body background="{background_attribute}">
  <img src="{load_once}" alt="ordinary approved image">
  <div style="background-image:url('{inline_style}')">inline CSS URL</div>
  <img srcset="{srcset_one} 1x, {srcset_two} 2x" alt="srcset only">
  <picture><source srcset="{picture_source} 1x"><img alt="picture fallback"></picture>
  <video poster="{video_poster}"><source src="{media_source}"></video>
  <iframe src="{iframe}" srcdoc="&lt;img src=&quot;{srcdoc_image}&quot;&gt;"></iframe>
  <object data="{object}"></object>
  <embed src="{embed}">
  <svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink">
    <image href="{svg_href}" xlink:href="{svg_xlink}"></image>
    <use href="{svg_use}"></use>
  </svg>
</body>
</html>"#,
        css_import = tracker.url("/css-import.css"),
        style_block = tracker.url("/style-block"),
        stylesheet = tracker.url("/linked.css"),
        meta_refresh = tracker.url("/meta-refresh.html"),
        background_attribute = tracker.url("/background-attribute"),
        load_once = tracker.url("/load-once"),
        inline_style = tracker.url("/inline-style"),
        srcset_one = tracker.url("/srcset-one"),
        srcset_two = tracker.url("/srcset-two"),
        picture_source = tracker.url("/picture-source"),
        video_poster = tracker.url("/video-poster"),
        media_source = tracker.url("/media-source"),
        iframe = tracker.url("/iframe.html"),
        srcdoc_image = tracker.url("/srcdoc-image"),
        object = tracker.url("/object.html"),
        embed = tracker.url("/embed"),
        svg_href = tracker.url("/svg-href"),
        svg_xlink = tracker.url("/svg-xlink"),
        svg_use = tracker.url("/svg-use"),
    )
}

#[cfg(unix)]
fn show_visual_html_and_wait(driver: &mut UiDriver, images_allowed: bool) -> anyhow::Result<Value> {
    let expected = if images_allowed {
        ExpectedImagePermission::MessageOnce
    } else {
        ExpectedImagePermission::Blocked
    };
    show_visual_html_and_wait_permission(driver, expected, None)
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedImagePermission {
    Blocked,
    MessageOnce,
    Sender,
    AllMessages,
}

#[cfg(unix)]
impl ExpectedImagePermission {
    fn label(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::MessageOnce => "message_once",
            Self::Sender => "sender",
            Self::AllMessages => "all_messages",
        }
    }

    fn images_allowed(self) -> bool {
        self != Self::Blocked
    }
}

#[cfg(unix)]
fn show_visual_html_and_wait_permission(
    driver: &mut UiDriver,
    expected: ExpectedImagePermission,
    previous_generation: Option<u64>,
) -> anyhow::Result<Value> {
    let shown = driver.command("show_visual_html", json!({}))?;
    assert_eq!(shown["ok"], true, "visual HTML render failed: {shown}");
    wait_for_html_view_permission_with_initial(
        driver,
        expected,
        Some(&shown["html_view"]),
        previous_generation,
    )
}

#[cfg(unix)]
fn ensure_visual_html_and_wait_permission(
    driver: &mut UiDriver,
    expected: ExpectedImagePermission,
    previous_generation: Option<u64>,
) -> anyhow::Result<Value> {
    let mut initial = driver.command("html_view_state", json!({}))?;
    if initial["html_visible"] != true {
        let shown = driver.command("show_visual_html", json!({}))?;
        assert_eq!(shown["ok"], true, "visual HTML render failed: {shown}");
        initial = shown["html_view"].clone();
    }
    wait_for_html_view_permission_with_initial(
        driver,
        expected,
        Some(&initial),
        previous_generation,
    )
}

#[cfg(unix)]
fn wait_for_html_view_permission(
    driver: &mut UiDriver,
    expected: ExpectedImagePermission,
    previous_generation: Option<u64>,
) -> anyhow::Result<Value> {
    wait_for_html_view_permission_with_initial(driver, expected, None, previous_generation)
}

#[cfg(unix)]
fn wait_for_html_view_permission_with_initial(
    driver: &mut UiDriver,
    expected: ExpectedImagePermission,
    initial: Option<&Value>,
    previous_generation: Option<u64>,
) -> anyhow::Result<Value> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut next = initial.cloned();
    loop {
        let view = match next.take() {
            Some(view) => view,
            None => driver.command("html_view_state", json!({}))?,
        };
        ensure!(view["ok"] == true, "HTML view inspection failed: {view}");
        ensure!(
            view["html_visible"] == true && view["has_html"] == true,
            "selected HTML message was not visible: {view}"
        );
        assert_eq!(
            view["global_remote_images_allowed"],
            expected == ExpectedImagePermission::AllMessages,
            "global remote-image state did not match the expected policy: {view}"
        );
        assert_eq!(
            view["sender_remote_images_allowed"],
            expected == ExpectedImagePermission::Sender,
            "sender remote-image state did not match the expected policy: {view}"
        );
        assert_eq!(
            view["image_loading_allowed"],
            expected.images_allowed(),
            "WebKit image loading did not match the selected-message policy: {view}"
        );
        assert_eq!(
            view["image_permission"],
            expected.label(),
            "HTML view exposed an ambiguous image permission: {view}"
        );
        assert_eq!(
            view["network_session_ephemeral"], true,
            "HTML view did not use an ephemeral WebKit network session: {view}"
        );
        let loading = view["loading"]
            .as_bool()
            .with_context(|| format!("HTML view did not expose WebKit loading state: {view}"))?;
        let load_generation = view["load_generation"]
            .as_u64()
            .with_context(|| format!("HTML view did not expose its load generation: {view}"))?;
        let completed_load_generation = view["completed_load_generation"]
            .as_u64()
            .with_context(|| format!("HTML view did not expose its completed load: {view}"))?;
        ensure!(
            load_generation > 0,
            "HTML view did not schedule the requested document load: {view}"
        );
        let generation_advanced =
            previous_generation.is_none_or(|previous| load_generation > previous);
        if !loading && completed_load_generation == load_generation && generation_advanced {
            return Ok(view);
        }
        ensure!(
            Instant::now() < deadline,
            "HTML view did not advance and complete a deterministic load cycle from {previous_generation:?}: {view}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn html_load_generation(view: &Value) -> anyhow::Result<u64> {
    view["load_generation"]
        .as_u64()
        .with_context(|| format!("HTML view exposed no load generation: {view}"))
}

#[cfg(unix)]
fn assert_remote_images_blocked(view: &Value) -> anyhow::Result<()> {
    let policy = view["html_policy_text"]
        .as_str()
        .with_context(|| format!("blocked HTML view had no policy text: {view}"))?
        .to_ascii_lowercase();
    ensure!(
        policy.contains("remote") && policy.contains("blocked"),
        "blocked HTML view did not explain its remote-content state: {view}"
    );
    Ok(())
}

#[cfg(unix)]
fn assert_loaded_image_metrics(view: &Value, expected: u64) -> anyhow::Result<()> {
    let images = view
        .get("images")
        .with_context(|| format!("HTML view exposed no DOM image metrics: {view}"))?;
    ensure!(
        images["total"].as_u64() == Some(expected)
            && images["loaded"].as_u64() == Some(expected)
            && images["failed"].as_u64() == Some(0)
            && images["pending"].as_u64() == Some(0),
        "HTML view did not render all {expected} expected images: {view}"
    );
    Ok(())
}

#[cfg(unix)]
fn assert_remote_images_once(view: &Value) -> anyhow::Result<()> {
    let status = view["status_text"]
        .as_str()
        .with_context(|| format!("one-shot HTML view had no status text: {view}"))?
        .to_ascii_lowercase();
    ensure!(
        status.contains("remote")
            && status.contains("message")
            && (status.contains("once") || status.contains("only")),
        "one-shot HTML view did not explain its selected-message-only scope: {view}"
    );
    Ok(())
}

#[cfg(unix)]
fn assert_sender_image_warning(view: &Value, sender: &str) -> anyhow::Result<()> {
    let warning = view["sender_image_warning_text"]
        .as_str()
        .with_context(|| format!("Images menu exposed no sender warning: {view}"))?
        .to_ascii_lowercase();
    ensure!(
        warning.contains(&sender.to_ascii_lowercase())
            && warning.contains("from address")
            && (warning.contains("not authenticated") || warning.contains("cannot authenticate"))
            && warning.contains("forged message")
            && warning.contains("load remote images"),
        "sender image warning did not clearly describe the spoofing boundary for {sender}: {view}"
    );
    Ok(())
}

#[cfg(unix)]
fn assert_image_menu_state(
    view: &Value,
    load_once_sensitive: bool,
    sender_active: bool,
    sender_sensitive: bool,
) -> anyhow::Result<(i64, i64, i64)> {
    assert_eq!(
        view["image_policy_button_label"], "Images (I)",
        "remote-image menu label must remain compact and state-independent: {view}"
    );
    assert_eq!(
        view["image_policy_button_sensitive"], true,
        "remote-image menu must remain available so sender permission can be revoked: {view}"
    );
    assert_eq!(
        view["load_images_once_label"], "Load for this message (I m)",
        "one-shot action label changed unexpectedly: {view}"
    );
    assert_eq!(
        view["load_images_once_sensitive"], load_once_sensitive,
        "one-shot action sensitivity did not match the rendered policy: {view}"
    );
    assert_eq!(
        view["sender_image_trust_label"], "Always load from this sender (I a)",
        "sender image toggle label changed unexpectedly: {view}"
    );
    assert_eq!(
        view["sender_image_trust_active"], sender_active,
        "sender image toggle did not expose its current state: {view}"
    );
    assert_eq!(
        view["sender_image_trust_sensitive"], sender_sensitive,
        "sender image toggle sensitivity did not match the exact From mailbox: {view}"
    );

    let width = view["image_policy_button_width"]
        .as_i64()
        .with_context(|| format!("image menu exposed no allocated width: {view}"))?;
    let height = view["image_policy_button_height"]
        .as_i64()
        .with_context(|| format!("image menu exposed no allocated height: {view}"))?;
    let row_height = view["image_policy_row_height"]
        .as_i64()
        .with_context(|| format!("image policy row exposed no allocated height: {view}"))?;
    ensure!(
        width > 0 && height > 0 && row_height > 0,
        "image policy controls were not allocated: {view}"
    );
    ensure!(
        height <= 64,
        "compact Images menu grew to an implausible multi-line height: {view}"
    );
    ensure!(
        row_height <= 64,
        "single-line image policy row grew to an implausible multi-line height: {view}"
    );
    Ok((width, height, row_height))
}

#[cfg(unix)]
fn assert_image_menu_geometry_stable(
    expected: (i64, i64, i64),
    observed: (i64, i64, i64),
    context: &str,
) -> anyhow::Result<()> {
    let stable = [expected.0, expected.1, expected.2]
        .into_iter()
        .zip([observed.0, observed.1, observed.2])
        .all(|(before, after)| (before - after).abs() <= 1);
    ensure!(
        stable,
        "image menu geometry changed after {context}: expected {expected:?}, observed {observed:?}"
    );
    Ok(())
}

#[cfg(unix)]
fn wait_for_standalone_window_count(
    driver: &mut UiDriver,
    expected_windows: usize,
) -> anyhow::Result<Vec<Value>> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let state = driver.command("standalone_message_windows", json!({}))?;
        let windows = json_array_at(&state, &["windows"])?;
        if windows.len() == expected_windows {
            return Ok(windows.to_vec());
        }
        ensure!(
            Instant::now() < deadline,
            "expected {expected_windows} standalone windows: {state}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn wait_for_standalone_remote_policy(
    driver: &mut UiDriver,
    expected: ExpectedImagePermission,
    expected_windows: usize,
    previous_generations: Option<&[u64]>,
) -> anyhow::Result<Vec<Value>> {
    if let Some(previous_generations) = previous_generations {
        ensure!(
            previous_generations.len() == expected_windows,
            "expected {expected_windows} prior standalone generations, got {}",
            previous_generations.len()
        );
    }
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let state = driver.command("standalone_message_windows", json!({}))?;
        let windows = json_array_at(&state, &["windows"])?;
        ensure!(
            windows.len() == expected_windows,
            "expected {expected_windows} standalone windows while waiting for image policy: {state}"
        );
        for window in windows {
            ensure!(
                window["view"] == "html" && window["html_visible"] == true,
                "standalone HTML message was not visible: {state}"
            );
            assert_eq!(
                window["global_remote_images_allowed"],
                expected == ExpectedImagePermission::AllMessages,
                "standalone global image policy did not match the expected permission: {state}"
            );
            assert_eq!(
                window["sender_remote_images_allowed"],
                expected == ExpectedImagePermission::Sender,
                "standalone sender image policy did not match the expected permission: {state}"
            );
            assert_eq!(
                window["image_loading_allowed"],
                expected.images_allowed(),
                "standalone WebKit image loading did not match its resolved permission: {state}"
            );
            assert_eq!(
                window["image_permission"],
                expected.label(),
                "standalone window exposed an ambiguous image permission: {state}"
            );
            assert_eq!(
                window["network_session_ephemeral"], true,
                "standalone HTML did not use an ephemeral WebKit network session: {state}"
            );
        }
        let completed_expected_loads = windows.iter().enumerate().all(|(window_index, window)| {
            let generation = window["html_lifecycle"]["generation"].as_u64();
            let completed = window["html_lifecycle"]["completed_generation"].as_u64();
            generation.is_some_and(|generation| {
                generation > 0
                    && completed == Some(generation)
                    && previous_generations
                        .is_none_or(|previous| generation > previous[window_index])
            })
        });
        if windows.iter().all(|window| window["loading"] == false) && completed_expected_loads {
            return Ok(windows.to_vec());
        }
        ensure!(
            Instant::now() < deadline,
            "standalone HTML load did not advance and complete under its expected image policy: previous_generations={previous_generations:?}, state={state}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn standalone_html_generations(windows: &[Value]) -> anyhow::Result<Vec<u64>> {
    windows
        .iter()
        .enumerate()
        .map(|(window_index, window)| {
            window["html_lifecycle"]["generation"]
                .as_u64()
                .with_context(|| {
                    format!(
                        "standalone window {window_index} exposed no HTML load generation: {window}"
                    )
                })
        })
        .collect()
}

#[cfg(unix)]
fn prepare_fixture_work_dir_for_restart(work_dir: &Path) -> anyhow::Result<()> {
    for path in [work_dir.join("h.sock"), work_dir.join("notm.log")] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("removing restart path {}", path.display()));
            }
        }
    }
    let display_dir = work_dir.join("gui-display");
    match fs::remove_dir_all(&display_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("removing prior GUI display state {}", display_dir.display())
            });
        }
    }
    Ok(())
}

fn fixture_app_config_path(driver: &mut UiDriver) -> anyhow::Result<PathBuf> {
    let state = driver.command("settings_test_state", json!({}))?;
    state["app_config_path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("fixture app did not report its isolated config path: {state}"))
}

fn directory_tree_snapshot(root: &Path) -> anyhow::Result<BTreeMap<PathBuf, Option<Vec<u8>>>> {
    fn visit(
        root: &Path,
        directory: &Path,
        snapshot: &mut BTreeMap<PathBuf, Option<Vec<u8>>>,
    ) -> anyhow::Result<()> {
        for entry in fs::read_dir(directory)
            .with_context(|| format!("reading directory snapshot {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("snapshot entry is below its root")
                .to_path_buf();
            if entry.file_type()?.is_dir() {
                snapshot.insert(relative, None);
                visit(root, &path, snapshot)?;
            } else {
                snapshot.insert(relative, Some(fs::read(&path)?));
            }
        }
        Ok(())
    }

    let mut snapshot = BTreeMap::new();
    if root.exists() {
        snapshot.insert(PathBuf::from("."), None);
        visit(root, root, &mut snapshot)?;
    }
    Ok(snapshot)
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
fn message_io_attachment_message(message_id: &str, subject: &str) -> Vec<u8> {
    format!(
        "From: Message I/O Sender <sender@example.test>\r\n\
         To: Fixture User <fixture@example.test>\r\n\
         Subject: {subject}\r\n\
         Date: Thu, 18 Jun 2026 20:00:00 -0600\r\n\
         Message-ID: <{message_id}>\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=message-io-root\r\n\r\n\
         --message-io-root\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n\
         Valid attachment-bearing root body.\r\n\
         --message-io-root\r\n\
         Content-Type: text/plain; name=note.txt\r\n\
         Content-Disposition: attachment; filename=note.txt\r\n\
         Content-Transfer-Encoding: base64\r\n\r\n\
         bWVzc2FnZS1JL08gYXR0YWNobWVudA0K\r\n\
         --message-io-root--\r\n"
    )
    .into_bytes()
}

#[cfg(unix)]
fn message_io_malformed_nested_message(
    message_id: &str,
    root_message_id: &str,
    subject: &str,
    date: &str,
    depth: usize,
) -> Vec<u8> {
    let mut raw = format!(
        "From: Broken MIME <broken@example.test>\r\n\
         To: Fixture User <fixture@example.test>\r\n\
         Subject: Re: {subject}\r\n\
         Date: {date}\r\n\
         Message-ID: <{message_id}>\r\n\
         In-Reply-To: <{root_message_id}>\r\n\
         References: <{root_message_id}>\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: multipart/mixed; boundary=message-io-malformed\r\n\
         X-Malformed: before-"
    )
    .into_bytes();
    raw.extend_from_slice(&[0xff, 0xfe]);
    raw.extend_from_slice(
        b"-after\r\n\r\n\
          --message-io-malformed\r\n\
          Content-Type: text/plain; charset=utf-8\r\n\r\n\
          malformed UTF-8 body before ",
    );
    raw.extend_from_slice(&[0xff, 0xfe]);
    raw.extend_from_slice(b" after invalid bytes\r\n--message-io-malformed\r\n");
    append_message_io_nested_multipart(&mut raw, depth);
    raw.extend_from_slice(b"\r\n--message-io-malformed--\r\n");
    raw
}

#[cfg(unix)]
fn append_message_io_nested_multipart(raw: &mut Vec<u8>, depth: usize) {
    if depth == 0 {
        raw.extend_from_slice(
            b"Content-Type: text/plain; charset=utf-8\r\n\r\ndeep MIME leaf marker",
        );
        return;
    }

    let boundary = format!("message-io-depth-{depth}");
    raw.extend_from_slice(
        format!("Content-Type: multipart/mixed; boundary={boundary}\r\n\r\n--{boundary}\r\n")
            .as_bytes(),
    );
    append_message_io_nested_multipart(raw, depth - 1);
    raw.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
}

#[cfg(unix)]
fn message_io_thread_reply(
    message_id: &str,
    root_message_id: &str,
    subject: &str,
    date: &str,
    index: usize,
) -> Vec<u8> {
    format!(
        "From: Reply {index} <reply-{index}@example.test>\r\n\
         To: Fixture User <fixture@example.test>\r\n\
         Subject: Re: {subject}\r\n\
         Date: {date}\r\n\
         Message-ID: <{message_id}>\r\n\
         In-Reply-To: <{root_message_id}>\r\n\
         References: <{root_message_id}>\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n\
         Message-I/O thread reply {index}.\r\n"
    )
    .into_bytes()
}

#[cfg(unix)]
fn open_message_io_attachment(
    driver: &mut UiDriver,
    root_message_id: &str,
    opener_marker: &Path,
    source_description: &str,
) -> anyhow::Result<PathBuf> {
    let listed = driver.command("attachment_list_items", json!({}))?;
    let attachments = json_array_at(&listed, &["attachments"])?;
    let attachment_index = attachments
        .iter()
        .position(|attachment| {
            attachment["message_id"] == root_message_id && attachment["filename"] == "note.txt"
        })
        .with_context(|| {
            format!("root attachment was not listed via {source_description}: {listed}")
        })?;
    let opened = driver.command("open_attachment", json!({"index": attachment_index}))?;
    ensure!(
        opened["ok"] == true,
        "root attachment could not be opened via {source_description}: {opened}"
    );
    assert_eq!(
        opened["pending"], true,
        "root attachment Open was not asynchronous via {source_description}: {opened}"
    );
    let completion = wait_for_attachment_io_idle(driver, STARTUP_TIMEOUT)?;
    assert_eq!(
        completion["last_completion"]["request_id"], opened["request_id"],
        "attachment Open completed a different request via {source_description}: started={opened}, completion={completion}"
    );
    assert_eq!(
        completion["last_completion"]["applied"], true,
        "attachment Open completion was stale via {source_description}: {completion}"
    );
    ensure!(
        completion["last_completion"]["error"].is_null(),
        "attachment Open failed via {source_description}: {completion}"
    );
    let opened_path = completion["last_completion"]["path"]
        .as_str()
        .map(PathBuf::from)
        .with_context(|| format!("attachment Open returned no path: {completion}"))?;
    ensure!(
        fs::read(&opened_path)? == b"message-I/O attachment\r\n",
        "attachment bytes changed when opened via {source_description}"
    );
    let opener_call = wait_for_file_text(opener_marker, STARTUP_TIMEOUT)?;
    ensure!(
        opener_call.contains(&opened_path.display().to_string()),
        "isolated opener did not receive {} via {source_description}: {opener_call:?}",
        opened_path.display()
    );
    Ok(opened_path)
}

#[cfg(unix)]
fn install_isolated_text_opener(work_dir: &Path, marker: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let applications = work_dir.join("data/applications");
    let config_home = work_dir.join("config");
    fs::create_dir_all(&applications)?;
    fs::create_dir_all(&config_home)?;
    let opener = work_dir.join("fake-open");
    fs::write(
        &opener,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" > {}\n",
            shell_single_quote(marker)
        ),
    )?;
    fs::set_permissions(&opener, fs::Permissions::from_mode(0o755))?;

    let desktop_id = "notm-message-io-test-opener.desktop";
    fs::write(
        applications.join(desktop_id),
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=notm message-I/O test opener\n\
             Exec={} %u\n\
             MimeType=text/plain;application/octet-stream;\n\
             NoDisplay=true\n",
            opener.display()
        ),
    )?;
    fs::write(
        applications.join("mimeinfo.cache"),
        format!("[MIME Cache]\ntext/plain={desktop_id};\napplication/octet-stream={desktop_id};\n"),
    )?;
    fs::write(
        config_home.join("mimeapps.list"),
        format!(
            "[Default Applications]\ntext/plain={desktop_id}\napplication/octet-stream={desktop_id}\n\
             [Added Associations]\ntext/plain={desktop_id};\napplication/octet-stream={desktop_id};\n"
        ),
    )?;
    Ok(())
}

#[cfg(unix)]
fn shell_single_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[cfg(unix)]
fn wait_for_file_text(path: &Path, timeout: Duration) -> anyhow::Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        match fs::read_to_string(path) {
            Ok(value) if !value.trim().is_empty() => return Ok(value),
            Ok(_) | Err(_) if Instant::now() < deadline => {
                thread::sleep(STARTUP_POLL_INTERVAL);
            }
            Ok(_) => anyhow::bail!("{} stayed empty for {timeout:?}", path.display()),
            Err(error) => anyhow::bail!(
                "{} was not written within {timeout:?}: {error}",
                path.display()
            ),
        }
    }
}

#[cfg(unix)]
fn assert_complete_message_io_thread(
    state: &Value,
    expected_count: usize,
    newest_message_id: &str,
    root_message_id: &str,
    malformed_message_id: &str,
) -> anyhow::Result<()> {
    let messages = json_array_at(state, &["state", "messages"])?;
    let reported_total = state["state"]["selected_thread"]["total_messages"]
        .as_u64()
        .unwrap_or_default();
    ensure!(
        messages.len() == expected_count,
        "message-I/O thread was silently truncated: loaded {}, expected {}, thread reported {}",
        messages.len(),
        expected_count,
        reported_total
    );
    ensure!(
        reported_total == expected_count as u64,
        "message-I/O thread summary reported {reported_total}, expected {expected_count}"
    );
    let actual_newest = messages
        .last()
        .and_then(|message| message["message_id"].as_str());
    ensure!(
        actual_newest == Some(newest_message_id),
        "message-I/O thread did not load its actual newest message: got {actual_newest:?}, expected {newest_message_id}"
    );
    for message_id in [root_message_id, malformed_message_id, newest_message_id] {
        ensure!(
            messages
                .iter()
                .any(|message| message["message_id"] == message_id),
            "message-I/O thread omitted {message_id} from {} loaded messages",
            messages.len()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn select_loaded_message(driver: &mut UiDriver, message_id: &str) -> anyhow::Result<Value> {
    let state = driver.command("app_state", json!({}))?;
    let messages = json_array_at(&state, &["state", "messages"])?;
    let index = messages
        .iter()
        .position(|message| message["message_id"] == message_id)
        .with_context(|| {
            format!(
                "loaded thread has no message {message_id} among {} messages",
                messages.len()
            )
        })?;
    let selected = driver.command("select_message_by_index", json!({"index": index}))?;
    ensure!(
        selected["ok"] == true && selected["selected_message"]["message_id"] == message_id,
        "could not select message {message_id}: {selected}"
    );
    Ok(selected)
}

#[cfg(unix)]
fn toml_path(path: &std::path::Path) -> String {
    toml::Value::String(path.display().to_string()).to_string()
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
