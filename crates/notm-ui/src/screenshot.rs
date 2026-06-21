use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub fn capture_screenshot(dir: impl AsRef<Path>, name: &str) -> anyhow::Result<PathBuf> {
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

fn command_exists(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|p| p.join(cmd))
                .find(|p| p.exists())
        })
        .is_some()
}
