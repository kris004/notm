use std::path::PathBuf;

pub fn config_path() -> PathBuf {
    config_path_from(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn config_path_from(xdg_config_home: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    xdg_config_home
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        })
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("notm/config.toml")
}

#[cfg(test)]
mod tests {
    use super::config_path_from;
    use std::path::PathBuf;

    #[test]
    fn config_path_uses_only_absolute_xdg_and_home_roots() {
        assert_eq!(
            config_path_from(
                Some(PathBuf::from("/tmp/config")),
                Some(PathBuf::from("/tmp/home"))
            ),
            PathBuf::from("/tmp/config/notm/config.toml")
        );
        assert_eq!(
            config_path_from(
                Some(PathBuf::from("relative-config")),
                Some(PathBuf::from("/tmp/home"))
            ),
            PathBuf::from("/tmp/home/.config/notm/config.toml")
        );
        assert_eq!(
            config_path_from(Some(PathBuf::new()), None),
            PathBuf::from(".config/notm/config.toml")
        );
    }
}
