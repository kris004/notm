use std::{ffi::CString, fs, path::Path, process::Command};

use notm_mail::MAX_SEND_TIMEOUT_SECONDS;
use serde_json::Value;

struct PrivateConfig {
    temp: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl PrivateConfig {
    fn create(contents: &str) -> anyhow::Result<Self> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("notm-config.toml");
        fs::write(&path, contents)?;
        Ok(Self { temp, path })
    }

    fn replace(&self, contents: &str) -> anyhow::Result<()> {
        fs::write(&self.path, contents)?;
        Ok(())
    }

    fn command(&self) -> Command {
        isolated_notm_command(&self.path, self.temp.path())
    }
}

#[test]
fn print_config_rejects_unknown_keys_and_reports_the_file() -> anyhow::Result<()> {
    let config = PrivateConfig::create("[ui]\npgae_size = 25\n")?;
    let output = config.command().arg("print-config").output()?;

    assert_rejected(&output, &config.path, "pgae_size");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown field"),
        "unknown-key error did not explain the problem:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn print_config_rejects_invalid_values_with_dotted_keys() -> anyhow::Result<()> {
    let config = PrivateConfig::create("")?;
    for (contents, key) in [
        (
            "[notmuch]\nopen_readwrite_only_for_mutations = false\n",
            "notmuch.open_readwrite_only_for_mutations",
        ),
        ("[ui]\npage_size = 0\n", "ui.page_size"),
        ("[ui]\ntheme = \"sepia\"\n", "ui.theme"),
        (
            "[ui]\nthread_preview_lines = 0\n",
            "ui.thread_preview_lines",
        ),
        (
            "[ui]\nthread_preview_lines = 21\n",
            "ui.thread_preview_lines",
        ),
        ("[ui]\nlayout = \"diagonal\"\n", "ui.layout"),
        ("[ui]\nhtml_mode = \"unsafe_html\"\n", "ui.html_mode"),
        ("[send]\ntransport = \"smtp\"\n", "send.transport"),
        ("[send]\nmode = \"magic\"\n", "send.mode"),
        (
            "[send]\nmode = \"command_template\"\nargs = [\"--message\"]\n",
            "send.args",
        ),
        ("[sync]\ntimeout_seconds = 0\n", "sync.timeout_seconds"),
    ] {
        config.replace(contents)?;
        let output = config.command().arg("print-config").output()?;
        assert_rejected(&output, &config.path, key);
    }
    Ok(())
}

#[test]
fn print_config_enforces_the_persistable_send_timeout_range() -> anyhow::Result<()> {
    let config = PrivateConfig::create(&format!(
        "[send]\ntimeout_seconds = {MAX_SEND_TIMEOUT_SECONDS}\n"
    ))?;
    let accepted = config
        .command()
        .args(["print-config", "--show-secrets"])
        .output()?;
    let printed = assert_printed_config(&accepted)?;
    assert_eq!(
        printed["send"]["timeout_seconds"].as_u64(),
        Some(MAX_SEND_TIMEOUT_SECONDS)
    );

    for timeout in [
        "0".to_string(),
        "-0".to_string(),
        "-1".to_string(),
        "\"not-a-number\"".to_string(),
        (MAX_SEND_TIMEOUT_SECONDS + 1).to_string(),
        u128::MAX.to_string(),
    ] {
        let contents = format!("[send]\ntimeout_seconds = {timeout}\n");
        config.replace(&contents)?;
        let rejected = config.command().arg("print-config").output()?;
        assert_rejected(&rejected, &config.path, "timeout_seconds");
        assert_eq!(
            fs::read(&config.path)?,
            contents.as_bytes(),
            "rejected configuration was modified for {timeout}"
        );
    }
    Ok(())
}

#[test]
fn print_config_distinguishes_missing_explicit_and_default_paths() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let missing = temp.path().join("missing-notm.toml");
    let explicit = isolated_notm_command(&missing, temp.path())
        .arg("print-config")
        .output()?;
    assert_rejected(&explicit, &missing, "does not exist");

    let implicit = isolated_notm_command_without_config(temp.path())
        .arg("print-config")
        .output()?;
    assert!(
        implicit.status.success(),
        "missing default config path did not fall back to defaults:\n{}",
        String::from_utf8_lossy(&implicit.stderr)
    );
    Ok(())
}

#[test]
fn print_config_accepts_but_omits_legacy_keys() -> anyhow::Result<()> {
    let config = PrivateConfig::create(
        "[ui]\nconfirm_destructive_tag_actions = false\n\
         trusted_image_senders = [\"SPOOFED@EXAMPLE.TEST\", \"malformed sender\"]\n\
         \n[send]\none_live_self_test_per_run = true\n\
         \n[send.env]\nNOTM_CUSTOM_VARIABLE = \"custom value\"\n\
         \n[sync]\nshow_manual_sync_button = true\n",
    )?;
    let output = config
        .command()
        .args(["print-config", "--show-secrets"])
        .output()?;

    assert!(
        output.status.success(),
        "legacy configuration was rejected:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        printed["send"]["env"]["NOTM_CUSTOM_VARIABLE"],
        "custom value"
    );
    for (section, legacy_key) in [
        ("ui", "confirm_destructive_tag_actions"),
        ("ui", "trusted_image_senders"),
        ("send", "one_live_self_test_per_run"),
        ("sync", "show_manual_sync_button"),
    ] {
        assert!(
            printed[section].get(legacy_key).is_none(),
            "legacy key {section}.{legacy_key} was not omitted: {printed}"
        );
    }
    assert_eq!(
        printed["ui"]["remote_images"], false,
        "ignored legacy sender entries broadened the effective image policy"
    );
    Ok(())
}

#[test]
fn print_config_prefers_explicit_app_notmuch_and_identity_values() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let explicit_database = temp.path().join("explicit-database");
    let environment_database = temp.path().join("environment-database");
    let explicit_notmuch_config = temp.path().join("explicit-notmuch-config");
    let environment_notmuch_config = temp.path().join("environment-notmuch-config");
    write_notmuch_config(
        &explicit_notmuch_config,
        &temp.path().join("config-database"),
        "Notmuch Config User",
        "notmuch-config@example.test",
    )?;
    create_notmuch_database_with_metadata(&environment_database, &[])?;
    write_notmuch_config(
        &environment_notmuch_config,
        &temp.path().join("other-config-database"),
        "Environment User",
        "environment@example.test",
    )?;
    let app_config = PrivateConfig::create(&format!(
        "[notmuch]\n\
         database_path = {}\n\
         config_path = {}\n\
         profile = \"explicit-profile\"\n\
         \n[identity]\n\
         name = \"App User\"\n\
         other_email = []\n",
        toml_path(&explicit_database),
        toml_path(&explicit_notmuch_config),
    ))?;
    let output = app_config
        .command()
        .env("NOTMUCH_CONFIG", &environment_notmuch_config)
        .env("NOTMUCH_DATABASE", &environment_database)
        .env("NOTMUCH_PROFILE", "environment-profile")
        .args(["print-config", "--show-secrets"])
        .output()?;

    let printed = assert_printed_config(&output)?;
    assert_eq!(
        printed["notmuch"]["database_path"].as_str(),
        explicit_database.to_str()
    );
    assert_eq!(
        printed["notmuch"]["config_path"].as_str(),
        explicit_notmuch_config.to_str()
    );
    assert_eq!(
        printed["notmuch"]["profile"].as_str(),
        Some("explicit-profile")
    );
    assert_eq!(printed["identity"]["name"].as_str(), Some("App User"));
    assert_eq!(
        printed["identity"]["primary_email"].as_str(),
        Some("notmuch-config@example.test")
    );
    assert_eq!(printed["identity"]["other_email"], serde_json::json!([]));
    Ok(())
}

#[test]
fn print_config_honors_notmuch_environment_before_discovered_files() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let app_config = PrivateConfig::create("")?;
    let environment_database = temp.path().join("environment-database");
    let configured_database = temp.path().join("configured-database");
    let default_database = app_config.temp.path().join("default-database");
    let environment_config = temp.path().join("environment-notmuch-config");
    write_notmuch_config(
        &environment_config,
        &configured_database,
        "Environment User",
        "environment@example.test",
    )?;
    create_notmuch_database_with_metadata(&environment_database, &[])?;
    write_notmuch_config(
        &app_config.temp.path().join("config/notmuch/default/config"),
        &default_database,
        "Default User",
        "default@example.test",
    )?;
    let output = app_config
        .command()
        .env("NOTMUCH_CONFIG", &environment_config)
        .env("NOTMUCH_DATABASE", &environment_database)
        .args(["print-config", "--show-secrets"])
        .output()?;

    let printed = assert_printed_config(&output)?;
    assert_eq!(
        printed["notmuch"]["database_path"].as_str(),
        environment_database.to_str()
    );
    assert_eq!(
        printed["notmuch"]["config_path"].as_str(),
        environment_config.to_str()
    );
    assert_eq!(
        printed["identity"]["name"].as_str(),
        Some("Environment User")
    );
    assert_eq!(
        printed["identity"]["primary_email"].as_str(),
        Some("environment@example.test")
    );
    Ok(())
}

#[test]
fn print_config_honors_xdg_profile_then_profiled_legacy_config() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let app_config = PrivateConfig::create("")?;
    let xdg_profile_database = temp.path().join("xdg-profile-database");
    let legacy_profile_database = temp.path().join("legacy-profile-database");
    let xdg_profile_config = app_config.temp.path().join("config/notmuch/work/config");
    let legacy_profile_config = app_config.temp.path().join("home/.notmuch-config.work");
    write_notmuch_config(
        &xdg_profile_config,
        &xdg_profile_database,
        "XDG Profile User",
        "xdg-profile@example.test",
    )?;
    write_notmuch_config(
        &legacy_profile_config,
        &legacy_profile_database,
        "Legacy Profile User",
        "legacy-profile@example.test",
    )?;

    let xdg_output = app_config
        .command()
        .env("NOTMUCH_PROFILE", "work")
        .args(["print-config", "--show-secrets"])
        .output()?;
    let xdg_printed = assert_printed_config(&xdg_output)?;
    assert_eq!(
        xdg_printed["notmuch"]["database_path"].as_str(),
        xdg_profile_database.to_str()
    );
    assert_eq!(xdg_printed["notmuch"]["profile"], "work");
    assert_eq!(
        xdg_printed["identity"]["primary_email"].as_str(),
        Some("xdg-profile@example.test")
    );

    fs::remove_file(&xdg_profile_config)?;
    let legacy_output = app_config
        .command()
        .env("NOTMUCH_PROFILE", "work")
        .args(["print-config", "--show-secrets"])
        .output()?;
    let legacy_printed = assert_printed_config(&legacy_output)?;
    assert_eq!(
        legacy_printed["notmuch"]["database_path"].as_str(),
        legacy_profile_database.to_str()
    );
    assert_eq!(
        legacy_printed["identity"]["primary_email"].as_str(),
        Some("legacy-profile@example.test")
    );
    Ok(())
}

#[test]
fn print_config_honors_xdg_default_then_legacy_default_config() -> anyhow::Result<()> {
    let app_config = PrivateConfig::create("")?;
    let xdg_default_database = app_config.temp.path().join("xdg-default-database");
    let legacy_default_database = app_config.temp.path().join("legacy-default-database");
    let xdg_default_config = app_config.temp.path().join("config/notmuch/default/config");
    let legacy_default_config = app_config.temp.path().join("home/.notmuch-config");
    write_notmuch_config(
        &xdg_default_config,
        &xdg_default_database,
        "XDG Default User",
        "xdg-default@example.test",
    )?;
    write_notmuch_config(
        &legacy_default_config,
        &legacy_default_database,
        "Legacy Default User",
        "legacy-default@example.test",
    )?;

    let xdg_output = app_config
        .command()
        .args(["print-config", "--show-secrets"])
        .output()?;
    let xdg_printed = assert_printed_config(&xdg_output)?;
    assert_eq!(
        xdg_printed["notmuch"]["database_path"].as_str(),
        xdg_default_database.to_str()
    );
    assert_eq!(
        xdg_printed["identity"]["primary_email"].as_str(),
        Some("xdg-default@example.test")
    );

    fs::remove_file(&xdg_default_config)?;
    let legacy_output = app_config
        .command()
        .args(["print-config", "--show-secrets"])
        .output()?;
    let legacy_printed = assert_printed_config(&legacy_output)?;
    assert_eq!(
        legacy_printed["notmuch"]["database_path"].as_str(),
        legacy_default_database.to_str()
    );
    assert_eq!(
        legacy_printed["identity"]["primary_email"].as_str(),
        Some("legacy-default@example.test")
    );
    Ok(())
}

#[test]
fn print_config_uses_profiled_xdg_database_default() -> anyhow::Result<()> {
    let app_config = PrivateConfig::create("")?;
    let profile_config = app_config.temp.path().join("config/notmuch/work/config");
    write_identity_only_notmuch_config(&profile_config, "Profile User", "profile@example.test")?;
    let expected_database = app_config.temp.path().join("data/notmuch/work");
    create_notmuch_database_with_metadata(&expected_database, &[])?;
    let output = app_config
        .command()
        .env("NOTMUCH_PROFILE", "work")
        .args(["print-config", "--show-secrets"])
        .output()?;

    let printed = assert_printed_config(&output)?;
    assert_eq!(
        printed["notmuch"]["database_path"].as_str(),
        expected_database.to_str()
    );
    assert_eq!(
        printed["identity"]["primary_email"].as_str(),
        Some("profile@example.test")
    );
    Ok(())
}

#[test]
fn print_config_loads_identity_from_database_metadata() -> anyhow::Result<()> {
    let app_config = PrivateConfig::create("")?;
    let database_path = app_config.temp.path().join("index");
    let mail_root = app_config.temp.path().join("mail");
    fs::create_dir_all(&mail_root)?;
    create_notmuch_database_with_metadata(
        &database_path,
        &[
            ("database.mail_root", mail_root.to_string_lossy().as_ref()),
            ("user.name", "Database User"),
            ("user.primary_email", "database@example.test"),
            (
                "user.other_email",
                "first-alt@example.test;second-alt@example.test",
            ),
        ],
    )?;
    let notmuch_config = app_config.temp.path().join("split-notmuch-config");
    fs::write(
        &notmuch_config,
        format!("[database]\npath={}\n", database_path.display()),
    )?;
    let output = app_config
        .command()
        .env("NOTMUCH_CONFIG", &notmuch_config)
        .args(["print-config", "--show-secrets"])
        .output()?;

    let printed = assert_printed_config(&output)?;
    assert_eq!(
        printed["notmuch"]["database_path"].as_str(),
        database_path.to_str()
    );
    assert_eq!(printed["identity"]["name"].as_str(), Some("Database User"));
    assert_eq!(
        printed["identity"]["primary_email"].as_str(),
        Some("database@example.test")
    );
    assert_eq!(
        printed["identity"]["other_email"],
        serde_json::json!(["first-alt@example.test", "second-alt@example.test"])
    );
    Ok(())
}

fn isolated_notm_command(config_path: &Path, root: &Path) -> Command {
    let mut command = isolated_notm_command_without_config(root);
    command.arg("--config").arg(config_path);
    command
}

fn isolated_notm_command_without_config(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_notm"));
    command
        .env_remove("NOTMUCH_CONFIG")
        .env_remove("NOTMUCH_DATABASE")
        .env_remove("NOTMUCH_PROFILE")
        .env_remove("MAILDIR")
        .env("EMAIL", "")
        .env("NAME", "")
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"));
    command
}

fn assert_printed_config(output: &std::process::Output) -> anyhow::Result<Value> {
    anyhow::ensure!(
        output.status.success(),
        "print-config failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn write_notmuch_config(
    path: &Path,
    database_path: &Path,
    name: &str,
    primary_email: &str,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        path,
        format!(
            "[database]\npath={}\n\n[user]\nname={}\nprimary_email={}\nother_email=alt-{}\n",
            database_path.display(),
            name,
            primary_email,
            primary_email
        ),
    )?;
    Ok(())
}

fn write_identity_only_notmuch_config(
    path: &Path,
    name: &str,
    primary_email: &str,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        path,
        format!("[user]\nname={name}\nprimary_email={primary_email}\n"),
    )?;
    Ok(())
}

fn create_notmuch_database_with_metadata(
    path: &Path,
    values: &[(&str, &str)],
) -> anyhow::Result<()> {
    use notm_notmuch::ffi::{
        notmuch_database_create_with_config, notmuch_database_destroy, notmuch_database_set_config,
        notmuch_database_t, notmuch_status_t,
    };

    fs::create_dir_all(path)?;
    let database_path = CString::new(path.to_string_lossy().as_bytes())?;
    let no_config = CString::new("")?;
    let mut database: *mut notmuch_database_t = std::ptr::null_mut();
    let mut error_message = std::ptr::null_mut();
    let status = unsafe {
        notmuch_database_create_with_config(
            database_path.as_ptr(),
            no_config.as_ptr(),
            std::ptr::null(),
            &mut database,
            &mut error_message,
        )
    };
    anyhow::ensure!(
        status == notmuch_status_t::NOTMUCH_STATUS_SUCCESS,
        "failed to create fixture Notmuch database: {status:?}"
    );
    anyhow::ensure!(!database.is_null(), "libnotmuch returned a null database");

    for (key, value) in values {
        let key = CString::new(*key)?;
        let value = CString::new(*value)?;
        let status = unsafe { notmuch_database_set_config(database, key.as_ptr(), value.as_ptr()) };
        anyhow::ensure!(
            status == notmuch_status_t::NOTMUCH_STATUS_SUCCESS,
            "failed to set fixture Notmuch metadata: {status:?}"
        );
    }
    let status = unsafe { notmuch_database_destroy(database) };
    anyhow::ensure!(
        status == notmuch_status_t::NOTMUCH_STATUS_SUCCESS,
        "failed to close fixture Notmuch database: {status:?}"
    );
    Ok(())
}

fn toml_path(path: &Path) -> String {
    format!("{path:?}")
}

fn assert_rejected(output: &std::process::Output, path: &Path, expected: &str) {
    assert!(
        !output.status.success(),
        "invalid configuration unexpectedly succeeded:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&path.display().to_string()),
        "error did not identify configuration file {}:\n{stderr}",
        path.display()
    );
    assert!(
        stderr.contains(expected),
        "error did not identify {expected}:\n{stderr}"
    );
}
