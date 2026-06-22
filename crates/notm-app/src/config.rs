use std::{collections::BTreeMap, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub notmuch: NotmuchConfig,
    #[serde(default)]
    pub identity: IdentityConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub send: SendConfig,
    #[serde(default)]
    pub drafts: DraftsConfig,
    #[serde(default)]
    pub sync: SyncConfig,
    #[serde(default)]
    pub automation: AutomationConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotmuchConfig {
    #[serde(default)]
    pub database_path: Option<PathBuf>,
    #[serde(default)]
    pub config_path: Option<PathBuf>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default = "default_notmuch_query")]
    pub default_query: String,
    #[serde(default = "default_excluded_tags")]
    pub excluded_tags: Vec<String>,
    #[serde(default = "default_true")]
    pub open_readwrite_only_for_mutations: bool,
    #[serde(default = "default_true")]
    pub sync_maildir_flags_after_tag_change: bool,
}

impl Default for NotmuchConfig {
    fn default() -> Self {
        Self {
            database_path: None,
            config_path: None,
            profile: None,
            default_query: "tag:inbox and not tag:trash and not tag:spam".to_string(),
            excluded_tags: vec!["trash".to_string(), "spam".to_string()],
            open_readwrite_only_for_mutations: true,
            sync_maildir_flags_after_tag_change: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IdentityConfig {
    pub name: Option<String>,
    pub primary_email: Option<String>,
    #[serde(default)]
    pub other_email: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    #[serde(default = "default_thread_preview_lines")]
    pub thread_preview_lines: usize,
    #[serde(default)]
    pub remote_images: bool,
    #[serde(default)]
    pub trusted_image_senders: Vec<String>,
    #[serde(default = "default_html_mode")]
    pub html_mode: String,
    #[serde(default)]
    pub start_maximized: bool,
    #[serde(default)]
    pub show_debug_panel: bool,
    #[serde(default = "default_true")]
    pub confirm_destructive_tag_actions: bool,
    #[serde(default)]
    pub custom_saved_searches: Vec<SavedSearchConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavedSearchConfig {
    pub name: String,
    pub query: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "system".to_string(),
            page_size: 100,
            thread_preview_lines: 2,
            remote_images: false,
            trusted_image_senders: Vec::new(),
            html_mode: "sanitize_then_render_text_fallback".to_string(),
            start_maximized: false,
            show_debug_panel: false,
            confirm_destructive_tag_actions: true,
            custom_saved_searches: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_send_transport")]
    pub transport: String,
    #[serde(default)]
    pub command: Option<PathBuf>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_send_mode")]
    pub mode: String,
    #[serde(default)]
    pub working_dir: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_send_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub save_sent: bool,
    #[serde(default)]
    pub sent_maildir: Option<PathBuf>,
    #[serde(default = "default_sent_tags")]
    pub sent_tags: Vec<String>,
    #[serde(default)]
    pub index_sent_after_send: bool,
    #[serde(default = "default_true")]
    pub one_live_self_test_per_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftsConfig {
    #[serde(default = "default_true")]
    pub save_maildir: bool,
    #[serde(default)]
    pub maildir: Option<PathBuf>,
    #[serde(default = "default_draft_tags")]
    pub tags: Vec<String>,
    #[serde(default = "default_true")]
    pub index_after_save: bool,
}

impl Default for DraftsConfig {
    fn default() -> Self {
        Self {
            save_maildir: true,
            maildir: None,
            tags: vec!["draft".to_string()],
            index_after_save: true,
        }
    }
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            transport: "external".to_string(),
            command: None,
            args: Vec::new(),
            mode: "auto".to_string(),
            working_dir: None,
            env: BTreeMap::new(),
            timeout_seconds: 120,
            save_sent: false,
            sent_maildir: None,
            sent_tags: vec!["sent".to_string()],
            index_sent_after_send: false,
            one_live_self_test_per_run: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_sync_label")]
    pub manual_action_label: String,
    #[serde(default)]
    pub notmuch_database_update_enabled: bool,
    #[serde(default)]
    pub notmuch_database_update_command: String,
    #[serde(default)]
    pub external_receive_enabled: bool,
    #[serde(default)]
    pub external_receive_command: String,
    #[serde(default)]
    pub show_manual_sync_button: bool,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            manual_action_label: "Sync".to_string(),
            notmuch_database_update_enabled: false,
            notmuch_database_update_command: String::new(),
            external_receive_enabled: false,
            external_receive_command: String::new(),
            show_manual_sync_button: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutomationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub socket_path: Option<PathBuf>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default = "default_screenshot_dir")]
    pub screenshot_dir: PathBuf,
    #[serde(default = "default_true")]
    pub allow_live_send_test: bool,
    #[serde(default)]
    pub allow_live_tag_test: bool,
}

impl Default for AutomationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            socket_path: None,
            token: None,
            screenshot_dir: PathBuf::from("artifacts/screenshots"),
            allow_live_send_test: true,
            allow_live_tag_test: false,
        }
    }
}

pub fn load(path_override: Option<PathBuf>) -> anyhow::Result<AppConfig> {
    let path = path_override.unwrap_or_else(paths::config_path);
    let mut config = if path.exists() {
        toml::from_str::<AppConfig>(&fs::read_to_string(&path)?)?
    } else {
        AppConfig::default()
    };
    let notmuch_config = config
        .notmuch
        .config_path
        .clone()
        .or_else(paths::notmuch_default_config_path);
    if config.notmuch.config_path.is_none() {
        config.notmuch.config_path = notmuch_config.clone();
    }
    if config.notmuch.database_path.is_none()
        && let Some(path) = &notmuch_config
    {
        config.notmuch.database_path =
            notm_notmuch::config::parse_notmuch_config_database_path(path);
    }
    if (config.identity.primary_email.is_none() || config.identity.name.is_none())
        && let Some(path) = &notmuch_config
    {
        let identity = notm_notmuch::config::parse_notmuch_config_identity(path);
        if config.identity.name.is_none() {
            config.identity.name = identity.name;
        }
        if config.identity.primary_email.is_none() {
            config.identity.primary_email = identity.primary_email;
        }
        if config.identity.other_email.is_empty() {
            config.identity.other_email = identity.other_email;
        }
    }
    if config.send.command.is_none()
        && let Some(candidate) = find_in_path("aerc-gmail-send")
    {
        config.send.command = Some(candidate);
    }
    Ok(config)
}

pub fn transport_mode(value: &str) -> notm_mail::TransportMode {
    match value {
        "stdin_rfc5322" => notm_mail::TransportMode::StdinRfc5322,
        "file_arg" => notm_mail::TransportMode::FileArg,
        "command_template" => notm_mail::TransportMode::CommandTemplate,
        _ => notm_mail::TransportMode::Auto,
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn default_true() -> bool {
    true
}

fn default_notmuch_query() -> String {
    "tag:inbox and not tag:trash and not tag:spam".to_string()
}

fn default_excluded_tags() -> Vec<String> {
    vec!["trash".to_string(), "spam".to_string()]
}

fn default_theme() -> String {
    "system".to_string()
}

fn default_page_size() -> usize {
    100
}

fn default_thread_preview_lines() -> usize {
    2
}

fn default_html_mode() -> String {
    "sanitize_then_render_text_fallback".to_string()
}

fn default_send_transport() -> String {
    "external".to_string()
}

fn default_send_mode() -> String {
    "auto".to_string()
}

fn default_send_timeout() -> u64 {
    120
}

fn default_sent_tags() -> Vec<String> {
    vec!["sent".to_string()]
}

fn default_draft_tags() -> Vec<String> {
    vec!["draft".to_string()]
}

fn default_sync_label() -> String {
    "Sync".to_string()
}

fn default_screenshot_dir() -> PathBuf {
    PathBuf::from("artifacts/screenshots")
}
