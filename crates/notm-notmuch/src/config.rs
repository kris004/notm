use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Database, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigProfile {
    pub database_path: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoadedIdentity {
    pub name: Option<String>,
    pub primary_email: Option<String>,
    pub other_email: Vec<String>,
}

impl Database {
    pub fn config_value(&self, key: &str) -> Result<String> {
        self.get_config_raw(key)
    }
}

pub fn parse_notmuch_config_identity(path: impl Into<PathBuf>) -> LoadedIdentity {
    let path = path.into();
    let Ok(text) = fs::read_to_string(path) else {
        return LoadedIdentity::default();
    };
    let mut section = String::new();
    let mut identity = LoadedIdentity::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = name.to_string();
            continue;
        }
        if section == "user"
            && let Some((key, value)) = line.split_once('=')
        {
            let value = value.trim().to_string();
            match key.trim() {
                "name" => identity.name = Some(value),
                "primary_email" => identity.primary_email = Some(value),
                "other_email" => {
                    identity.other_email = value
                        .split(';')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(ToOwned::to_owned)
                        .collect();
                }
                _ => {}
            }
        }
    }
    identity
}

pub fn parse_notmuch_config_database_path(path: impl Into<PathBuf>) -> Option<PathBuf> {
    let path = path.into();
    let text = fs::read_to_string(path).ok()?;
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = name.to_string();
            continue;
        }
        if section == "database"
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "path"
        {
            return Some(PathBuf::from(value.trim()));
        }
    }
    None
}
