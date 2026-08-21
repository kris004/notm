#[cfg(unix)]
mod unix {
    use std::{
        env, fs,
        os::unix::fs::PermissionsExt,
        path::Path,
        process::{Command, Output},
    };

    use notm_mail::ProbeReport;

    fn write_config(path: &Path, command: &Path) {
        write_config_with_options(path, command, None, None);
    }

    fn write_config_with_options(
        path: &Path,
        command: &Path,
        working_dir: Option<&Path>,
        configured_path: Option<&Path>,
    ) {
        let mut contents = format!("[send]\nenabled = true\ncommand = {}\n", toml_path(command));
        if let Some(working_dir) = working_dir {
            contents.push_str(&format!("working_dir = {}\n", toml_path(working_dir)));
        }
        if let Some(configured_path) = configured_path {
            contents.push_str(&format!(
                "\n[send.env]\nPATH = {}\n",
                toml_path(configured_path)
            ));
        }
        fs::write(path, contents).expect("write test config");
    }

    fn toml_path(path: &Path) -> toml::Value {
        toml::Value::String(
            path.to_str()
                .expect("temporary path should be valid UTF-8")
                .to_string(),
        )
    }

    fn run_probe(config: &Path, path: Option<std::ffi::OsString>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_notm"));
        command
            .arg("--config")
            .arg(config)
            .arg("probe-send")
            // A send probe must not depend on—or even try to load—the
            // invoking account's Notmuch selection.
            .env(
                "NOTMUCH_CONFIG",
                config.with_file_name("missing-notmuch-config"),
            )
            .env(
                "NOTMUCH_DATABASE",
                config.with_file_name("missing-notmuch-database"),
            )
            .env("NOTMUCH_PROFILE", "unavailable-probe-profile");
        if let Some(path) = path {
            command.env("PATH", path);
        }
        command.output().expect("run notm probe-send")
    }

    fn parse_report(output: &Output) -> ProbeReport {
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "probe stdout should be a JSON report: {error}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    #[test]
    fn probe_send_resolves_bare_command_through_path() {
        let temp = tempfile::tempdir().expect("create temp directory");
        let bin_dir = temp.path().join("bin");
        fs::create_dir(&bin_dir).expect("create temp bin directory");
        let helper = bin_dir.join("notm-probe-helper");
        fs::write(&helper, "#!/bin/sh\nexit 0\n").expect("write helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))
            .expect("make helper executable");

        let config = temp.path().join("config.toml");
        write_config(&config, Path::new("notm-probe-helper"));
        let inherited_path = env::var_os("PATH").unwrap_or_default();
        let search_path =
            env::join_paths(std::iter::once(bin_dir).chain(env::split_paths(&inherited_path)))
                .expect("construct PATH");

        let output = run_probe(&config, Some(search_path));
        assert!(
            output.status.success(),
            "probe should succeed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = parse_report(&output);
        assert!(
            report.ok,
            "bare helper should resolve through PATH: {report:?}"
        );
    }

    #[test]
    fn probe_send_uses_configured_relative_path_from_working_directory() {
        let temp = tempfile::tempdir().expect("create temp directory");
        let working_dir = temp.path().join("work");
        let bin_dir = working_dir.join("bin");
        let inherited_bin = temp.path().join("inherited-bin");
        fs::create_dir_all(&bin_dir).expect("create configured bin directory");
        fs::create_dir(&inherited_bin).expect("create inherited bin directory");
        let helper = bin_dir.join("notm-configured-helper");
        fs::write(&helper, "#!/bin/sh\nexit 0\n").expect("write helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))
            .expect("make helper executable");

        let config = temp.path().join("config.toml");
        write_config_with_options(
            &config,
            Path::new("notm-configured-helper"),
            Some(&working_dir),
            Some(Path::new("bin")),
        );
        let inherited_path = env::join_paths([inherited_bin]).expect("construct inherited PATH");

        let output = run_probe(&config, Some(inherited_path));
        assert!(
            output.status.success(),
            "probe should use configured relative PATH from its working directory: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = parse_report(&output);
        assert!(report.ok, "configured helper should resolve: {report:?}");
        assert!(
            report
                .details
                .iter()
                .any(|detail| detail.contains(&helper.display().to_string())),
            "probe should report the configured helper path: {report:?}"
        );
    }

    #[test]
    fn failed_probe_returns_unsuccessful_process_status() {
        let temp = tempfile::tempdir().expect("create temp directory");
        let config = temp.path().join("config.toml");
        write_config(&config, &temp.path().join("missing-send-helper"));

        let output = run_probe(&config, None);
        assert!(
            !output.status.success(),
            "failed probe unexpectedly exited successfully; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let report = parse_report(&output);
        assert!(!report.ok, "missing helper probe should report ok=false");
    }
}
