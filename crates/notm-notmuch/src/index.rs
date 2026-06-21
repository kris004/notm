use std::path::Path;

use crate::{Database, Result};

impl Database {
    pub fn index_fixture_file(&self, path: &Path, tags: &[&str]) -> Result<String> {
        self.index_file_with_tags(path, tags)
    }
}
