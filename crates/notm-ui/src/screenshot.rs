use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
    process::Command,
};

pub fn capture_screenshot(dir: impl AsRef<Path>, name: &str) -> anyhow::Result<PathBuf> {
    ensure_screenshot_basename(name)?;
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let candidates: &[(&str, &[&str])] = &[
        ("gnome-screenshot", &["-f"]),
        ("grim", &[]),
        ("spectacle", &["-b", "-n", "-o"]),
        ("import", &["-window", "root"]),
        ("scrot", &[]),
        ("xwd", &["-root", "-out"]),
    ];
    for (cmd, args) in candidates {
        if command_exists(cmd) {
            let status = match *cmd {
                "gnome-screenshot" | "spectacle" | "xwd" => {
                    Command::new(cmd).args(*args).arg(&path).status()
                }
                "grim" | "scrot" | "import" => Command::new(cmd).args(*args).arg(&path).status(),
                _ => continue,
            };
            if let Ok(status) = status
                && status.success()
                && path.exists()
            {
                return Ok(path);
            }
        }
    }
    anyhow::bail!(
        "no working screenshot backend found (tried gnome-screenshot, grim, spectacle, import, scrot, xwd)"
    )
}

fn ensure_screenshot_basename(name: &str) -> anyhow::Result<()> {
    let mut components = Path::new(name).components();
    anyhow::ensure!(
        matches!(components.next(), Some(Component::Normal(component)) if component == OsStr::new(name))
            && components.next().is_none(),
        "screenshot name must be a single file name"
    );
    Ok(())
}

fn command_exists(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|p| p.join(cmd))
                .find(|p| p.exists())
        })
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::ensure_screenshot_basename;

    #[test]
    fn accepts_a_single_file_name() {
        assert!(ensure_screenshot_basename("notm-thread.png").is_ok());
    }

    #[test]
    fn rejects_paths_and_special_components() {
        for name in [
            "",
            ".",
            "..",
            "../outside.png",
            "nested/shot.png",
            "/tmp/shot.png",
        ] {
            assert!(
                ensure_screenshot_basename(name).is_err(),
                "unexpectedly accepted {name:?}"
            );
        }
    }
}
