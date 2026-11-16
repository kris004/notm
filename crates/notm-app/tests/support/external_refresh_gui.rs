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

fn position_fixture(root: &Path) -> anyhow::Result<(notm_test_support::FixtureDatabase, PathBuf)> {
    let fixture = notm_test_support::FixtureDatabase::create()?;
    let db = fixture.open_readwrite()?;
    for index in 0..96 {
        let path = fixture.maildir.join(format!("new/position-{index}"));
        fs::write(
            &path,
            format!(
                "From: sender@example.test\r\nTo: reader@example.test\r\n\
             Subject: Position message {index:03}\r\nMessage-ID: <position-{index}@example.test>\r\n\
             Date: {} Nov 2026 {:02}:00:00 +0000\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\r\nPosition fixture message {index}.\r\n",
                1 + index / 24,
                index % 24,
            ),
        )?;
        db.index_file_with_tags(&path, &["inbox", "unread", "position-test"])?;
    }
    drop(db);
    let config = write_config(root, &fixture)?;
    let mut text = fs::read_to_string(&config)?;
    text.push_str("\n[ui]\npage_size = 20\nlayout = \"columns\"\n");
    fs::write(&config, text)?;
    Ok((fixture, config))
}

fn position_snapshot(driver: &mut UiDriver) -> anyhow::Result<(Value, Value)> {
    let viewport = driver.command("thread_selection_view_state", json!({}))?;
    let state = driver.command("app_state", json!({}))?;
    Ok((state, viewport))
}

fn assert_position_preserved(driver: &mut UiDriver, before: &(Value, Value)) -> anyhow::Result<()> {
    let old_rows = before.0["state"]["thread_list_items"]
        .as_array()
        .context("old rows")?;
    let anchor = before.1["anchor_index"]
        .as_u64()
        .context("visible anchor")? as usize;
    let anchor_id = &old_rows[anchor]["thread_id"];
    let top = before.1["anchor_top"].as_f64().context("anchor offset")?;
    // Sample repeatedly: delayed selection/scroll callbacks must not snap back
    // after an initially correct result.
    for _ in 0..3 {
        let (state, viewport) = position_snapshot(driver)?;
        assert_eq!(
            state["state"]["selected_thread"]["thread_id"],
            before.0["state"]["selected_thread"]["thread_id"],
            "cursor moved: {viewport}"
        );
        let rows = state["state"]["thread_list_items"]
            .as_array()
            .context("new rows")?;
        let index = rows
            .iter()
            .position(|row| &row["thread_id"] == anchor_id)
            .context("anchor lost")?;
        let layout = driver.command("thread_row_layout", json!({"index": index}))?;
        let y = layout["row"]["y"]
            .as_f64()
            .context("anchor no longer visible")?;
        ensure!(
            (y - top).abs() <= 1.0,
            "viewport moved: before={}, after={viewport}, row={layout}",
            before.1
        );
    }
    Ok(())
}

#[test]
fn external_refresh_preserves_read_cursor_and_viewport_beyond_first_page() -> anyhow::Result<()> {
    if gtk_display_environment()?.is_none() {
        eprintln!(
            "SKIP external_refresh_preserves_read_cursor_and_viewport_beyond_first_page: no display"
        );
        return Ok(());
    }
    let root = tempfile::tempdir()?;
    let (fixture, config) = position_fixture(root.path())?;
    let bus = PrivateBus::start()?;
    let mut app = launch(&root.path().join("app"), &config, &bus, false)?;
    let mut driver = app.connect("refresh-test")?;
    select_first_result(&mut driver, "tag:position-test")?;
    driver.command("resize_window", json!({"width": 1100, "height": 500}))?;
    // Cover an appended window and a directly located later page.
    for located in [false, true] {
        if located {
            let selected = driver.command("select_thread_absolute", json!({"index": 80}))?;
            assert_eq!(selected["ok"], true, "{selected}");
            driver.wait_for_search(STARTUP_TIMEOUT)?;
        } else {
            for _ in 0..2 {
                driver.command("load_more_threads", json!({"select_last": false}))?;
                driver.wait_for_search(STARTUP_TIMEOUT)?;
            }
        }
        driver.command(
            "select_thread_by_index",
            json!({"index": if located {8} else {38}}),
        )?;
        wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;
        // Scroll independently of the cursor, including a partial-row offset.
        let scrolled = driver.command("scroll_thread_list_lines", json!({"lines": 4.25}))?;
        assert_eq!(scrolled["ok"], true, "{scrolled}");
        let before = position_snapshot(&mut driver)?;
        ensure!(
            before.1["scroll_value"].as_f64().unwrap_or_default() > 0.0,
            "{before:?}"
        );
        let tagged = driver.command("mark_read_selected", json!({}))?;
        assert_eq!(tagged["ok"], true, "{tagged}");
        wait_for_tag(&mut driver, STARTUP_TIMEOUT)?;
        assert_position_preserved(&mut driver, &before).with_context(|| app.logs())?;

        // A new message preceding this window changes absolute positions.
        let path = fixture
            .maildir
            .join(format!("new/position-arrival-{located}"));
        fs::write(
            &path,
            format!(
                "From: new@example.test\r\nTo: reader@example.test\r\nSubject: New arrival\r\n\
             Message-ID: <arrival-{located}@example.test>\r\nDate: 29 Nov 2026 00:00:00 +0000\r\n\r\nArrival.\r\n"
            ),
        )?;
        fixture
            .open_readwrite()?
            .index_file_with_tags(&path, &["inbox", "unread", "position-test"])?;
        completed_refresh(&bus, true, 1)?;
        assert_position_preserved(&mut driver, &before).with_context(|| app.logs())?;

        let sync = driver.command("run_manual_sync", json!({}))?;
        assert_eq!(sync["ok"], true, "{sync}");
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            let state = driver.command("app_state", json!({}))?;
            if state["state"]["sync_in_progress"] == false {
                break;
            }
            ensure!(Instant::now() < deadline, "sync did not complete: {state}");
            thread::sleep(STARTUP_POLL_INTERVAL);
        }
        assert_position_preserved(&mut driver, &before).with_context(|| app.logs())?;
    }
    Ok(())
}

#[test]
fn external_refresh_removed_unread_row_keeps_neighbour_and_viewport() -> anyhow::Result<()> {
    if gtk_display_environment()?.is_none() {
        eprintln!(
            "SKIP external_refresh_removed_unread_row_keeps_neighbour_and_viewport: no display"
        );
        return Ok(());
    }
    let root = tempfile::tempdir()?;
    let (_fixture, config) = position_fixture(root.path())?;
    let bus = PrivateBus::start()?;
    let mut app = launch(&root.path().join("app"), &config, &bus, false)?;
    let mut driver = app.connect("refresh-test")?;
    select_first_result(&mut driver, "tag:position-test and tag:unread")?;
    driver.command("resize_window", json!({"width": 1100, "height": 500}))?;
    driver.command("select_thread_by_index", json!({"index": 10}))?;
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;
    driver.command("focus_search", json!({}))?;
    let mut before = position_snapshot(&mut driver)?;
    let next = before.0["state"]["thread_list_items"][11].clone();
    driver.command("mark_read_selected", json!({}))?;
    wait_for_tag(&mut driver, STARTUP_TIMEOUT)?;
    completed_refresh(&bus, true, 1)?;
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;
    before.0["state"]["selected_thread"] = next;
    assert_position_preserved(&mut driver, &before).with_context(|| app.logs())?;
    assert_eq!(
        driver.command("entry_state", json!({}))?["search_has_focus"],
        true,
        "loading the surviving neighbour stole focus"
    );
    let state = driver.command("app_state", json!({}))?;
    assert_eq!(state["state"]["thread_total_count"], 95);
    assert_eq!(
        state["state"]["selected_message"]["thread_id"],
        state["state"]["selected_thread"]["thread_id"]
    );
    let message_id = state["state"]["selected_message"]["message_id"]
        .as_str()
        .context("selected message")?;
    select_first_result(&mut driver, &format!("id:{message_id} and tag:unread"))?;
    driver.command("mark_read_selected", json!({}))?;
    wait_for_tag(&mut driver, STARTUP_TIMEOUT)?;
    completed_refresh(&bus, true, 1)?;
    let empty = driver.command("app_state", json!({}))?;
    assert_eq!(empty["state"]["thread_total_count"], 0);
    assert!(empty["state"]["selected_thread"].is_null());
    let viewport = driver.command("thread_selection_view_state", json!({}))?;
    assert!(viewport["selected_local"].is_null());
    Ok(())
}

#[test]
fn external_refresh_sync_keeps_navigation_made_during_worker() -> anyhow::Result<()> {
    if gtk_display_environment()?.is_none() {
        eprintln!("SKIP external_refresh_sync_keeps_navigation_made_during_worker: no display");
        return Ok(());
    }
    let root = tempfile::tempdir()?;
    let (_fixture, config) = position_fixture(root.path())?;
    let bus = PrivateBus::start()?;
    let mut app = launch(&root.path().join("app"), &config, &bus, false)?;
    let mut driver = app.connect("refresh-test")?;
    select_first_result(&mut driver, "tag:position-test")?;
    driver.command("resize_window", json!({"width": 1100, "height": 500}))?;
    let sync = driver.command("run_manual_sync", json!({"test_refresh_delay_ms": 1500}))?;
    assert_eq!(sync["ok"], true, "{sync}");
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let state = driver.command("app_state", json!({}))?;
        if state["state"]["search_loading"] == true {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "sync never started refresh: {state}"
        );
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
    let selected = driver.command("select_thread_by_index", json!({"index": 12}))?;
    assert_eq!(selected["ok"], true, "{selected}");
    wait_for_thread_load_idle(&mut driver, STARTUP_TIMEOUT)?;
    driver.command("scroll_thread_list_lines", json!({"lines": 4.25}))?;
    let before = position_snapshot(&mut driver)?;
    loop {
        let state = driver.command("app_state", json!({}))?;
        if state["state"]["sync_in_progress"] == false {
            break;
        }
        ensure!(Instant::now() < deadline, "sync did not finish: {state}");
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
    assert_position_preserved(&mut driver, &before).with_context(|| app.logs())?;
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
