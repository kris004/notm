use std::path::{Path, PathBuf};

pub fn expected_screenshot(path: impl AsRef<Path>, name: &str) -> PathBuf {
    path.as_ref().join(name)
}
