use std::{fs, process::Command};

use serde_json::{Value, json};
use tempfile::TempDir;

const REDACTED_VALUE: &str = "[REDACTED]";
const TOKEN_SECRET: &str = "notm-secret-test-harness-token";
const ENV_SECRET: &str = "notm-secret-send-environment";
const ARG_SECRET: &str = "notm-secret-send-argument";
const RECEIVE_SECRET: &str = "notm-secret-receive-command";
const UPDATE_SECRET: &str = "notm-secret-update-command";

struct PrivateConfig {
    temp: TempDir,
    path: std::path::PathBuf,
}

impl PrivateConfig {
    fn create() -> anyhow::Result<Self> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.toml");
        fs::write(
            &path,
            format!(
                "[identity]\nname = \"Safe Display Name\"\nprimary_email = \"safe@example.test\"\n\
                 \n[send]\nargs = [\"--credential\", \"{ARG_SECRET}\"]\nenv = {{ ACCESS_TOKEN = \"{ENV_SECRET}\", EMPTY_VALUE = \"\" }}\n\
                 \n[sync]\nexternal_receive_command = \"receive --token {RECEIVE_SECRET}\"\nnotmuch_database_update_command = \"update --token {UPDATE_SECRET}\"\n\
                 \n[automation]\ntoken = \"{TOKEN_SECRET}\"\n"
            ),
        )?;
        Ok(Self { temp, path })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_notm"));
        command
            .arg("--config")
            .arg(&self.path)
            .env("HOME", self.temp.path().join("home"))
            .env("XDG_CONFIG_HOME", self.temp.path().join("config"))
            .env("XDG_CACHE_HOME", self.temp.path().join("cache"))
            .env("XDG_DATA_HOME", self.temp.path().join("data"));
        command
    }
}

#[test]
fn print_config_redacts_secret_bearing_values_by_default() -> anyhow::Result<()> {
    let config = PrivateConfig::create()?;
    let output = config.command().arg("print-config").output()?;

    assert_success(&output, "redacted print-config");
    let stdout = String::from_utf8(output.stdout)?;
    for secret in sensitive_sentinels() {
        assert!(
            !stdout.contains(secret),
            "default print-config exposed sentinel {secret:?}:\n{stdout}"
        );
    }

    let printed: Value = serde_json::from_str(&stdout)?;
    assert_eq!(printed["identity"]["name"], "Safe Display Name");
    assert_eq!(printed["automation"]["token"], REDACTED_VALUE);
    assert_eq!(
        printed["send"]["env"],
        json!({"ACCESS_TOKEN": REDACTED_VALUE, "EMPTY_VALUE": REDACTED_VALUE})
    );
    assert_eq!(
        printed["send"]["args"],
        json!([REDACTED_VALUE, REDACTED_VALUE])
    );
    assert_eq!(printed["sync"]["external_receive_command"], REDACTED_VALUE);
    assert_eq!(
        printed["sync"]["notmuch_database_update_command"],
        REDACTED_VALUE
    );

    Ok(())
}

#[test]
fn print_config_show_secrets_preserves_compatibility_output() -> anyhow::Result<()> {
    let config = PrivateConfig::create()?;
    let output = config
        .command()
        .args(["print-config", "--show-secrets"])
        .output()?;

    assert_success(&output, "unredacted print-config");
    let stdout = String::from_utf8(output.stdout)?;
    for secret in sensitive_sentinels() {
        assert!(
            stdout.contains(secret),
            "--show-secrets omitted sentinel {secret:?}:\n{stdout}"
        );
    }
    let printed: Value = serde_json::from_str(&stdout)?;
    assert_eq!(printed["automation"]["token"], TOKEN_SECRET);
    assert_eq!(printed["send"]["env"]["ACCESS_TOKEN"], ENV_SECRET);
    assert_eq!(printed["send"]["args"][1], ARG_SECRET);
    assert!(
        printed["sync"]["external_receive_command"]
            .as_str()
            .is_some_and(|command| command.contains(RECEIVE_SECRET))
    );
    assert!(
        printed["sync"]["notmuch_database_update_command"]
            .as_str()
            .is_some_and(|command| command.contains(UPDATE_SECRET))
    );

    Ok(())
}

#[test]
fn print_config_help_warns_about_unredacted_output() -> anyhow::Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_notm"))
        .args(["print-config", "--help"])
        .output()?;

    assert_success(&output, "print-config help");
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("--show-secrets"),
        "missing flag help:\n{stdout}"
    );
    assert!(
        stdout.to_ascii_lowercase().contains("unsafe for logs"),
        "help does not warn about unsafe output:\n{stdout}"
    );
    assert!(
        stdout.to_ascii_lowercase().contains("redacted by default"),
        "help does not describe the safe default:\n{stdout}"
    );

    Ok(())
}

fn sensitive_sentinels() -> [&'static str; 5] {
    [
        TOKEN_SECRET,
        ENV_SECRET,
        ARG_SECRET,
        RECEIVE_SECRET,
        UPDATE_SECRET,
    ]
}

fn assert_success(output: &std::process::Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
