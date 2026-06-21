use std::path::PathBuf;

pub fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(path).join("notm/config.toml");
    }
    home_dir().join(".config/notm/config.toml")
}

pub fn notmuch_default_config_path() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"));
    let candidates = [
        xdg.join("notmuch/default/config"),
        home_dir().join(".notmuch-config"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
