use super::*;
use refresh_bus::PrivateBus;

fn write_config(
    root: &Path,
    fixture: &notm_test_support::FixtureDatabase,
) -> anyhow::Result<PathBuf> {
    let config = root.join("app.toml");
    let marker = root.join("sync-must-not-run");
    let command = format!(
        "touch '{}'",
        marker.display().to_string().replace('\'', "'\\''")
    );
    fs::write(
        &config,
        format!(
            "[notmuch]\ndatabase_path = {}\nconfig_path = {}\ndefault_query = \"tag:inbox\"\n\
         [identity]\nname = \"Refresh Test\"\nprimary_email = \"refresh@example.test\"\n\
         [drafts]\nsave_maildir = false\nindex_after_save = false\n\
         [sync]\nenabled = true\nexternal_receive_enabled = true\nexternal_receive_on_startup = false\nexternal_receive_command = {}\n\
         [automation]\nallow_live_tag_test = true\n",
            toml_path(&fixture.root),
            toml_path(&fixture.config_path),
            toml::Value::String(command),
        ),
    )?;
    Ok(config)
}

fn refresh_command(bus: &PrivateBus, test_instances: bool) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_notm"));
    command.args(["refresh", "--all", "--timeout-seconds", "10"]);
    if test_instances {
        command.arg("--test-instances");
    }
    bus.configure(&mut command);
    command
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("SWAYSOCK");
    command
}

fn completed_refresh(bus: &PrivateBus, test_instances: bool, count: usize) -> anyhow::Result<()> {
    let output = refresh_command(bus, test_instances).output()?;
    ensure!(output.status.success(), "refresh failed: {output:?}");
    let report: Value = serde_json::from_slice(&output.stdout)?;
    ensure!(
        report == json!({"refreshed": count, "failed": []}),
        "{report}"
    );
    Ok(())
}

fn launch(
    root: &Path,
    config: &Path,
    bus: &PrivateBus,
    production: bool,
) -> anyhow::Result<FixtureApp> {
    FixtureApp::spawn_inner(
        root.to_path_buf(),
        "refresh-test",
        FixtureLaunchOptions {
            config_path: Some(config),
            session_bus_address: Some(&bus.address),
            production,
            ..FixtureLaunchOptions::default()
        },
    )
}

fn index_arrival(fixture: &notm_test_support::FixtureDatabase) -> anyhow::Result<()> {
    let path = fixture.maildir.join("new/refresh-arrival");
    fs::write(
        &path,
        "From: arrival@example.test\r\nTo: refresh@example.test\r\n\
         Subject: External refresh arrival\r\nMessage-ID: <external-refresh@example.test>\r\n\
         Date: Thu, 10 Sep 2026 01:00:00 +0000\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\nNew external message.\r\n",
    )?;
    fixture
        .open_readwrite()?
        .index_file_with_tags(&path, &["inbox", "unread"])?;
    Ok(())
}

fn select_first_result(driver: &mut UiDriver, query: &str) -> anyhow::Result<()> {
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    let scheduled = driver.command("run_search", json!({"query": query}))?;
    assert_eq!(scheduled["scheduled"], true, "{scheduled}");
    driver.wait_for_search(STARTUP_TIMEOUT)?;
    let selected = driver.command("select_thread_by_index", json!({"index": 0}))?;
    assert_eq!(selected["ok"], true, "{selected}");
    wait_for_thread_load_idle(driver, STARTUP_TIMEOUT)?;
    Ok(())
}

#[test]
fn external_refresh_normal_instance_without_test_harness() -> anyhow::Result<()> {
    if gtk_display_environment()?.is_none() {
        eprintln!("SKIP external_refresh_normal_instance_without_test_harness: no display");
        return Ok(());
    }
    let root = tempfile::tempdir()?;
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let config = write_config(root.path(), &fixture)?;
    let bus = PrivateBus::start()?;
    let revision = fixture.open_readonly()?.revision();
    let mut app = launch(&root.path().join("normal"), &config, &bus, true)?;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        ensure!(
            app.child.try_wait()?.is_none(),
            "app exited: {}",
            app.logs()
        );
        let output = refresh_command(&bus, false).output()?;
        if output.status.success()
            && serde_json::from_slice::<Value>(&output.stdout)?["refreshed"] == 1
        {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "normal refresh did not become ready: {output:?}\n{}",
            app.logs()
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
    ensure!(
        !app.socket_path.exists(),
        "production unexpectedly opened the developer harness"
    );
    ensure!(
        !root.path().join("sync-must-not-run").exists(),
        "refresh ran external sync"
    );
    assert_eq!(
        fixture.open_readonly()?.revision(),
        revision,
        "refresh modified mail"
    );
    completed_refresh(&bus, true, 0)?;
    Ok(())
}

#[test]
fn external_refresh_updates_multiple_searches_preserving_drafts_and_focus() -> anyhow::Result<()> {
    if gtk_display_environment()?.is_none() {
        eprintln!(
            "SKIP external_refresh_updates_multiple_searches_preserving_drafts_and_focus: no display"
        );
        return Ok(());
    }
    let root = tempfile::tempdir()?;
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let config = write_config(root.path(), &fixture)?;
    let bus = PrivateBus::start()?;
    let mut a = launch(&root.path().join("a"), &config, &bus, false)?;
    let mut b = launch(&root.path().join("b"), &config, &bus, false)?;
    let mut drivers = [a.connect("refresh-test")?, b.connect("refresh-test")?];
    let queries = ["tag:inbox", "tag:inbox and not tag:flagged"];
    let mut before = Vec::new();
    let mut drafts = Vec::new();
    let mut active_windows = Vec::new();
    let mut composer_fields = Vec::new();
    for (driver, query) in drivers.iter_mut().zip(queries) {
        select_first_result(driver, query)?;
        wait_for_thread_load_idle(driver, STARTUP_TIMEOUT)?;
        driver.command("open_compose", json!({}))?;
        driver.command("compose_set_to", json!({"value": "draft@example.test"}))?;
        driver.command("compose_set_subject", json!({"value": "Keep this draft"}))?;
        driver.command("compose_set_body", json!({"value": "Keep this body"}))?;
        let saved = driver.command("save_draft", json!({}))?;
        let path = PathBuf::from(
            saved["report"]["local_path"]
                .as_str()
                .context("saved draft path")?,
        );
        drafts.push((path.clone(), fs::read(&path)?));
        driver.command(
            "compose_set_body",
            json!({"value": "Unsaved edits must also survive"}),
        )?;
        driver.command("focus_search", json!({}))?;
        before.push(driver.command("app_state", json!({}))?);
        let entries = driver.command("entry_state", json!({}))?;
        active_windows.push(entries["window_is_active"].clone());
        composer_fields.push(entries["compose_fields"].clone());
    }
    index_arrival(&fixture)?;
    let revision = fixture.open_readonly()?.revision();
    // Normal service calls must never touch isolated test instances.
    completed_refresh(&bus, false, 0)?;
    for (driver, original) in drivers.iter_mut().zip(&before) {
        let state = driver.command("app_state", json!({}))?;
        assert_eq!(
            state["state"]["search_generation"],
            original["state"]["search_generation"]
        );
    }
    // This is a new CLI process using the production D-Bus method, NOT a
    // test-harness command. Success must mean both result models are updated.
    completed_refresh(&bus, true, 2)?;
    for ((((driver, original), query), active), fields) in drivers
        .iter_mut()
        .zip(&before)
        .zip(queries)
        .zip(active_windows)
        .zip(composer_fields)
    {
        let refreshed = driver.command("app_state", json!({}))?;
        assert_eq!(refreshed["state"]["current_query"], query);
        assert_eq!(refreshed["state"]["search_loading"], false);
        assert_eq!(
            refreshed["state"]["thread_total_count"].as_u64(),
            original["state"]["thread_total_count"]
                .as_u64()
                .map(|count| count + 1)
        );
        assert_eq!(
            refreshed["state"]["selected_thread"]["thread_id"],
            original["state"]["selected_thread"]["thread_id"]
        );
        assert_eq!(
            refreshed["state"]["active_draft"],
            original["state"]["active_draft"]
        );
        let entries = driver.command("entry_state", json!({}))?;
        assert_eq!(entries["compose_fields"], fields);
        assert_eq!(entries["search_has_focus"], true, "{entries}");
        assert_eq!(
            entries["window_is_active"], active,
            "refresh activated a window"
        );
    }
    for (path, bytes) in drafts {
        assert_eq!(fs::read(path)?, bytes, "refresh wrote a saved draft");
    }
    assert_eq!(
        fixture.open_readonly()?.revision(),
        revision,
        "refresh modified mail"
    );
    ensure!(
        !root.path().join("sync-must-not-run").exists(),
        "refresh ran external sync"
    );
    Ok(())
}

#[test]
fn external_refresh_waits_for_search_and_mutation_without_cancelling_them() -> anyhow::Result<()> {
    if gtk_display_environment()?.is_none() {
        eprintln!(
            "SKIP external_refresh_waits_for_search_and_mutation_without_cancelling_them: no display"
        );
        return Ok(());
    }
    let root = tempfile::tempdir()?;
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let config = write_config(root.path(), &fixture)?;
    let bus = PrivateBus::start()?;
    let mut app = launch(&root.path().join("app"), &config, &bus, false)?;
    let mut driver = app.connect("refresh-test")?;
    select_first_result(&mut driver, "tag:inbox")?;
    let search = driver.command(
        "run_search",
        json!({"query": "tag:inbox", "test_delay_ms": 1200}),
    )?;
    assert_eq!(search["scheduled"], true, "{search}");
    let mut refresh = ChildGuard(refresh_command(&bus, true).stdout(Stdio::piped()).spawn()?);
    thread::sleep(Duration::from_millis(150));
    ensure!(
        refresh.0.try_wait()?.is_none(),
        "refresh did not wait for the search"
    );
    let pending = driver.command("search_status", json!({}))?;
    assert_eq!(pending["loading"], true);
    assert_eq!(
        pending["generation"], search["generation"],
        "external refresh superseded the search"
    );
    index_arrival(&fixture)?;
    let status = refresh.0.wait()?;
    ensure!(status.success(), "refresh after pending search failed");
    let searched = driver.command("app_state", json!({}))?;
    ensure!(
        searched["state"]["search_generation"].as_u64() > search["generation"].as_u64(),
        "refresh incorrectly acknowledged the pre-existing search"
    );
    select_first_thread(&mut driver, "id:external-refresh@example.test")?;
    let tagged = driver.command(
        "tag_selected",
        json!({"add": ["refresh-race"], "test_delay_ms": 1200}),
    )?;
    assert_eq!(tagged["pending"], true, "{tagged}");
    let heartbeat = driver.command("health", json!({}))?["gtk_heartbeat"].as_u64();
    let mut refresh = ChildGuard(refresh_command(&bus, true).stdout(Stdio::piped()).spawn()?);
    thread::sleep(Duration::from_millis(150));
    ensure!(
        refresh.0.try_wait()?.is_none(),
        "refresh did not wait for the mutation"
    );
    assert_eq!(
        driver.command("tag_status", json!({}))?["in_progress"],
        true
    );
    ensure!(
        driver.command("health", json!({}))?["gtk_heartbeat"].as_u64() > heartbeat,
        "GTK stopped processing while refresh was queued"
    );
    ensure!(refresh.0.wait()?.success(), "refresh after mutation failed");
    let done = driver.command("app_state", json!({}))?;
    assert_eq!(
        done["state"]["current_query"],
        "id:external-refresh@example.test"
    );
    assert_eq!(done["state"]["tag_in_progress"], false);
    assert_eq!(done["state"]["search_loading"], false);
    assert_eq!(done["state"]["search_error"], Value::Null);
    ensure!(
        !root.path().join("sync-must-not-run").exists(),
        "refresh ran external sync"
    );
    Ok(())
}

#[test]
fn external_refresh_reports_search_failure_and_recovers() -> anyhow::Result<()> {
    if gtk_display_environment()?.is_none() {
        eprintln!("SKIP external_refresh_reports_search_failure_and_recovers: no display");
        return Ok(());
    }
    let root = tempfile::tempdir()?;
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let config = write_config(root.path(), &fixture)?;
    let bus = PrivateBus::start()?;
    let mut app = launch(&root.path().join("app"), &config, &bus, false)?;
    let mut driver = app.connect("refresh-test")?;
    select_first_result(&mut driver, "tag:inbox")?;
    let database = fixture.root.join(".notmuch");
    let unavailable = fixture.root.join("offline-index");
    fs::rename(&database, &unavailable)?;
    let output = refresh_command(&bus, true).output()?;
    ensure!(
        !output.status.success(),
        "unavailable database was reported refreshed: {output:?}"
    );
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["refreshed"], 0);
    ensure!(
        report["failed"][0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("SearchFailed")),
        "{report}"
    );
    assert_eq!(driver.command("health", json!({}))?["ok"], true);
    fs::rename(unavailable, database)?;
    completed_refresh(&bus, true, 1)?;
    Ok(())
}
