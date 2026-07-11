use std::{fs, path::Path, process::Command};

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
        ("[ui]\nlayout = \"diagonal\"\n", "ui.layout"),
        ("[ui]\nhtml_mode = \"unsafe_html\"\n", "ui.html_mode"),
        ("[send]\ntransport = \"smtp\"\n", "send.transport"),
        ("[send]\nmode = \"magic\"\n", "send.mode"),
        (
            "[send]\nmode = \"command_template\"\nargs = [\"--message\"]\n",
            "send.args",
        ),
    ] {
        config.replace(contents)?;
        let output = config.command().arg("print-config").output()?;
        assert_rejected(&output, &config.path, key);
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
        ("send", "one_live_self_test_per_run"),
        ("sync", "show_manual_sync_button"),
    ] {
        assert!(
            printed[section].get(legacy_key).is_none(),
            "legacy key {section}.{legacy_key} was not omitted: {printed}"
        );
    }
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
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"));
    command
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
