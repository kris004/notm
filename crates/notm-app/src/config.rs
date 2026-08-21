use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::Context;
use notm_ui::model::{MAX_THREAD_PREVIEW_LINES, MessageViewPreference, ThemePreference};
use serde::{Deserialize, Serialize};

use crate::paths;

const REDACTED_VALUE: &str = "[REDACTED]";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

impl AppConfig {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.notmuch.open_readwrite_only_for_mutations,
            "notmuch.open_readwrite_only_for_mutations must be true; notm always opens searches and message views read-only"
        );
        anyhow::ensure!(
            self.ui.page_size > 0,
            "ui.page_size must be greater than zero"
        );
        anyhow::ensure!(
            self.ui.theme.parse::<ThemePreference>().is_ok(),
            "ui.theme must be exactly one of system, light, or dark; got {:?}",
            self.ui.theme
        );
        anyhow::ensure!(
            (1..=MAX_THREAD_PREVIEW_LINES).contains(&self.ui.thread_preview_lines),
            "ui.thread_preview_lines must be between 1 and {MAX_THREAD_PREVIEW_LINES}; got {}",
            self.ui.thread_preview_lines
        );
        anyhow::ensure!(
            is_supported_layout(&self.ui.layout),
            "ui.layout must be one of auto, three_pane (or columns), or stacked; got {:?}",
            self.ui.layout
        );
        anyhow::ensure!(
            matches!(
                self.ui.html_mode.as_str(),
                "sanitize_then_render_text_fallback" | "visual_html_preferred"
            ),
            "ui.html_mode must be one of sanitize_then_render_text_fallback or visual_html_preferred; got {:?}",
            self.ui.html_mode
        );
        anyhow::ensure!(
            self.send.transport == "external",
            "send.transport must be external; got {:?}",
            self.send.transport
        );
        anyhow::ensure!(
            matches!(
                self.send.mode.as_str(),
                "auto" | "stdin_rfc5322" | "file_arg" | "command_template"
            ),
            "send.mode must be one of auto, stdin_rfc5322, file_arg, or command_template; got {:?}",
            self.send.mode
        );
        anyhow::ensure!(
            self.send.mode != "command_template"
                || self.send.args.iter().any(|arg| arg.contains("{file}")),
            "send.args must include an entry containing {{file}} when send.mode is command_template"
        );
        anyhow::ensure!(
            self.sync.timeout_seconds > 0,
            "sync.timeout_seconds must be greater than zero"
        );
        Ok(())
    }

    pub(crate) fn redacted_for_display(&self) -> Self {
        let mut redacted = self.clone();

        if redacted.automation.token.is_some() {
            redacted.automation.token = Some(REDACTED_VALUE.to_string());
        }
        for value in redacted.send.env.values_mut() {
            *value = REDACTED_VALUE.to_string();
        }
        for argument in &mut redacted.send.args {
            *argument = REDACTED_VALUE.to_string();
        }
        if !redacted.sync.notmuch_database_update_command.is_empty() {
            redacted.sync.notmuch_database_update_command = REDACTED_VALUE.to_string();
        }
        if !redacted.sync.external_receive_command.is_empty() {
            redacted.sync.external_receive_command = REDACTED_VALUE.to_string();
        }

        redacted
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotmuchConfig {
    #[serde(default)]
    pub database_path: Option<PathBuf>,
    #[serde(default)]
    pub config_path: Option<PathBuf>,
    #[serde(default)]
    pub profile: Option<String>,
    /// Effective mail storage root loaded from Notmuch; not an app config key.
    #[serde(skip)]
    pub resolved_mail_root: Option<PathBuf>,
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
            resolved_mail_root: None,
            default_query: "tag:inbox and not tag:trash and not tag:spam".to_string(),
            excluded_tags: vec!["trash".to_string(), "spam".to_string()],
            open_readwrite_only_for_mutations: true,
            sync_maildir_flags_after_tag_change: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct IdentityConfig {
    pub name: Option<String>,
    pub primary_email: Option<String>,
    #[serde(default)]
    pub other_email: Vec<String>,
    #[serde(skip)]
    other_email_is_explicit: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityConfigInput {
    name: Option<String>,
    primary_email: Option<String>,
    other_email: Option<Vec<String>>,
}

impl<'de> Deserialize<'de> for IdentityConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let input = IdentityConfigInput::deserialize(deserializer)?;
        let other_email_is_explicit = input.other_email.is_some();
        Ok(Self {
            name: input.name,
            primary_email: input.primary_email,
            other_email: input.other_email.unwrap_or_default(),
            other_email_is_explicit,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    #[serde(default = "default_thread_preview_lines")]
    pub thread_preview_lines: usize,
    #[serde(default = "default_layout")]
    pub layout: String,
    #[serde(default = "default_true")]
    pub show_thread_numbers: bool,
    #[serde(default = "default_true")]
    pub show_thread_dates: bool,
    #[serde(default = "default_true")]
    pub show_thread_tags: bool,
    #[serde(default = "default_true")]
    pub show_thread_preview: bool,
    #[serde(default = "default_true")]
    pub show_keybind_hints: bool,
    #[serde(default)]
    pub remote_images: bool,
    #[serde(default)]
    pub trusted_image_senders: Vec<String>,
    #[serde(default = "default_html_mode")]
    pub html_mode: String,
    #[serde(default)]
    pub message_view_preferences: BTreeMap<String, MessageViewPreference>,
    #[serde(default)]
    pub sender_view_preferences: BTreeMap<String, MessageViewPreference>,
    #[serde(default)]
    pub start_maximized: bool,
    #[serde(default = "default_true")]
    pub show_sidebar: bool,
    #[serde(default = "default_true")]
    pub show_message_list: bool,
    #[serde(default = "default_true")]
    pub show_message_view: bool,
    #[serde(default)]
    pub show_debug_panel: bool,
    #[serde(default)]
    pub custom_saved_searches: Vec<SavedSearchConfig>,
    #[serde(default)]
    pub hidden_tag_searches: Vec<String>,
    #[serde(default, rename = "confirm_destructive_tag_actions", skip_serializing)]
    _legacy_confirm_destructive_tag_actions: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
            layout: "auto".to_string(),
            show_thread_numbers: true,
            show_thread_dates: true,
            show_thread_tags: true,
            show_thread_preview: true,
            show_keybind_hints: true,
            remote_images: false,
            trusted_image_senders: Vec::new(),
            html_mode: "sanitize_then_render_text_fallback".to_string(),
            message_view_preferences: BTreeMap::new(),
            sender_view_preferences: BTreeMap::new(),
            start_maximized: false,
            show_sidebar: true,
            show_message_list: true,
            show_message_view: true,
            show_debug_panel: false,
            custom_saved_searches: Vec::new(),
            hidden_tag_searches: Vec::new(),
            _legacy_confirm_destructive_tag_actions: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    #[serde(default, rename = "one_live_self_test_per_run", skip_serializing)]
    _legacy_one_live_self_test_per_run: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
            _legacy_one_live_self_test_per_run: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_sync_label")]
    pub manual_action_label: String,
    #[serde(default = "default_sync_timeout")]
    pub timeout_seconds: u64,
    #[serde(default)]
    pub notmuch_database_update_enabled: bool,
    #[serde(default)]
    pub notmuch_database_update_on_startup: bool,
    #[serde(default)]
    pub notmuch_database_update_command: String,
    #[serde(default)]
    pub external_receive_enabled: bool,
    #[serde(default)]
    pub external_receive_on_startup: bool,
    #[serde(default)]
    pub external_receive_command: String,
    #[serde(default, rename = "show_manual_sync_button", skip_serializing)]
    _legacy_show_manual_sync_button: Option<bool>,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            manual_action_label: "Sync".to_string(),
            timeout_seconds: 300,
            notmuch_database_update_enabled: false,
            notmuch_database_update_on_startup: false,
            notmuch_database_update_command: String::new(),
            external_receive_enabled: false,
            external_receive_on_startup: false,
            external_receive_command: String::new(),
            _legacy_show_manual_sync_button: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub socket_path: Option<PathBuf>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default = "default_screenshot_dir")]
    pub screenshot_dir: PathBuf,
    #[serde(default)]
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
            allow_live_send_test: false,
            allow_live_tag_test: false,
        }
    }
}

pub fn load(path_override: Option<PathBuf>) -> anyhow::Result<AppConfig> {
    let mut config = load_app_config(path_override)?;
    load_notmuch_context(&mut config)?;
    Ok(config)
}

pub(crate) fn load_app_config(path_override: Option<PathBuf>) -> anyhow::Result<AppConfig> {
    let explicit_path = path_override.is_some();
    let path = path_override.unwrap_or_else(paths::config_path);
    anyhow::ensure!(
        !explicit_path || path.exists(),
        "configuration file {} does not exist",
        path.display()
    );
    let config = if path.exists() {
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read configuration file {}", path.display()))?;
        toml::from_str::<AppConfig>(&contents)
            .with_context(|| format!("invalid configuration file {}", path.display()))?
    } else {
        AppConfig::default()
    };
    config
        .validate()
        .with_context(|| format!("invalid configuration file {}", path.display()))?;
    Ok(config)
}

fn load_notmuch_context(config: &mut AppConfig) -> anyhow::Result<()> {
    // Pass Notmuch's environment overrides explicitly so they retain their documented priority
    // when libnotmuch merges the external file with database configuration metadata. Leaving an
    // input unset delegates XDG, profile, legacy, MAILDIR, and HOME fallbacks to libnotmuch.
    let environment_database = nonempty_environment_path("NOTMUCH_DATABASE");
    let environment_config = nonempty_environment_path("NOTMUCH_CONFIG");
    let environment_profile = nonempty_environment_string("NOTMUCH_PROFILE");
    let open = notm_notmuch::OpenConfig {
        database_path: config
            .notmuch
            .database_path
            .clone()
            .or_else(|| environment_database.clone()),
        config_path: config
            .notmuch
            .config_path
            .clone()
            .or_else(|| environment_config.clone()),
        profile: config
            .notmuch
            .profile
            .clone()
            .or_else(|| environment_profile.clone()),
    };
    let database = notm_notmuch::Database::load_config(&open)
        .context("failed to load effective Notmuch configuration")?;

    if config.notmuch.database_path.is_none() {
        config.notmuch.database_path = nonempty_path(database.path());
    }
    if config.notmuch.config_path.is_none() {
        config.notmuch.config_path = environment_config;
    }
    if config.notmuch.profile.is_none() {
        config.notmuch.profile = environment_profile;
    }
    config.notmuch.resolved_mail_root = loaded_notmuch_value(&database, "database.mail_root")?
        .map(PathBuf::from)
        .or_else(|| config.notmuch.database_path.clone());

    if config.identity.name.is_none() {
        config.identity.name = loaded_notmuch_value(&database, "user.name")?;
    }
    if config.identity.primary_email.is_none() {
        config.identity.primary_email = loaded_notmuch_value(&database, "user.primary_email")?;
    }
    if !config.identity.other_email_is_explicit
        && config.identity.other_email.is_empty()
        && let Some(other_email) = loaded_notmuch_value(&database, "user.other_email")?
    {
        config.identity.other_email = other_email
            .split(';')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect();
    }
    Ok(())
}

fn loaded_notmuch_value(
    database: &notm_notmuch::Database,
    key: &str,
) -> anyhow::Result<Option<String>> {
    let value = database
        .config_value(key)
        .with_context(|| format!("failed to read effective Notmuch setting {key}"))?;
    Ok(nonempty_value(value))
}

fn nonempty_value(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn nonempty_path(value: String) -> Option<PathBuf> {
    nonempty_value(value).map(PathBuf::from)
}

fn nonempty_environment_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn nonempty_environment_string(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn is_supported_layout(value: &str) -> bool {
    matches!(
        value.trim().replace('-', "_").to_lowercase().as_str(),
        "" | "auto"
            | "three"
            | "three_pane"
            | "threepane"
            | "3pane"
            | "3_pane"
            | "column"
            | "columns"
            | "side_by_side"
            | "sidebyside"
            | "side_by_side_columns"
            | "stacked"
            | "stack"
            | "top"
            | "top_stack"
            | "list_above_message"
            | "sidebar_list_top"
    )
}

pub fn transport_mode(value: &str) -> notm_mail::TransportMode {
    match value {
        "stdin_rfc5322" => notm_mail::TransportMode::StdinRfc5322,
        "file_arg" => notm_mail::TransportMode::FileArg,
        "command_template" => notm_mail::TransportMode::CommandTemplate,
        _ => notm_mail::TransportMode::Auto,
    }
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

fn default_layout() -> String {
    "auto".to_string()
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

fn default_sync_timeout() -> u64 {
    300
}

fn default_screenshot_dir() -> PathBuf {
    PathBuf::from("artifacts/screenshots")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{AppConfig, REDACTED_VALUE};
    use notm_ui::model::MessageViewPreference;

    fn parse_validated(contents: &str) -> anyhow::Result<AppConfig> {
        let config = toml::from_str::<AppConfig>(contents)?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn load_captures_effective_mail_root_without_exposing_an_app_config_key() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let database_path = temp.path().join("index");
        let mail_root = temp.path().join("mail");
        let notmuch_config_path = temp.path().join("notmuch-config");
        let app_config_path = temp.path().join("notm-config.toml");
        fs::write(
            &notmuch_config_path,
            format!(
                "[database]\npath={}\nmail_root={}\n",
                database_path.display(),
                mail_root.display()
            ),
        )
        .expect("write Notmuch config");
        fs::write(
            &app_config_path,
            format!("[notmuch]\nconfig_path = {:?}\n", notmuch_config_path),
        )
        .expect("write notm config");

        let config = super::load(Some(app_config_path)).expect("load effective config");

        assert_eq!(config.notmuch.database_path.as_ref(), Some(&database_path));
        assert_eq!(config.notmuch.resolved_mail_root.as_ref(), Some(&mail_root));
        let printed = serde_json::to_value(&config).expect("serialize config");
        assert!(printed["notmuch"].get("resolved_mail_root").is_none());
    }

    #[test]
    fn display_redaction_replaces_secret_bearing_values_without_changing_shape() {
        let mut config = AppConfig::default();
        config.automation.token = Some("test-harness-secret".to_string());
        config
            .send
            .env
            .insert("ACCESS_TOKEN".to_string(), "environment-secret".to_string());
        config
            .send
            .env
            .insert("EMPTY_VALUE".to_string(), String::new());
        config.send.args = vec!["--password".to_string(), "argument-secret".to_string()];
        config.sync.external_receive_command = "receive --token sync-secret".to_string();
        config.sync.notmuch_database_update_command = "index --key update-secret".to_string();

        let redacted = config.redacted_for_display();

        assert_eq!(redacted.automation.token.as_deref(), Some(REDACTED_VALUE));
        assert_eq!(redacted.send.env.len(), 2);
        assert!(
            redacted
                .send
                .env
                .values()
                .all(|value| value == REDACTED_VALUE)
        );
        assert_eq!(redacted.send.args.len(), 2);
        assert!(
            redacted
                .send
                .args
                .iter()
                .all(|argument| argument == REDACTED_VALUE)
        );
        assert_eq!(redacted.sync.external_receive_command, REDACTED_VALUE);
        assert_eq!(
            redacted.sync.notmuch_database_update_command,
            REDACTED_VALUE
        );

        assert_eq!(
            config.automation.token.as_deref(),
            Some("test-harness-secret")
        );
        assert_eq!(
            config.send.env.get("ACCESS_TOKEN").map(String::as_str),
            Some("environment-secret")
        );
        assert_eq!(config.send.args[1], "argument-secret");
        assert!(config.sync.external_receive_command.contains("sync-secret"));
        assert!(
            config
                .sync
                .notmuch_database_update_command
                .contains("update-secret")
        );
    }

    #[test]
    fn display_redaction_preserves_absent_and_empty_sensitive_fields() {
        let config = AppConfig::default();

        let redacted = config.redacted_for_display();

        assert_eq!(redacted.automation.token, None);
        assert!(redacted.send.env.is_empty());
        assert!(redacted.send.args.is_empty());
        assert!(redacted.sync.external_receive_command.is_empty());
        assert!(redacted.sync.notmuch_database_update_command.is_empty());
    }

    #[test]
    fn ui_layout_defaults_to_auto() {
        let config: AppConfig = toml::from_str("").expect("empty config should deserialize");

        assert_eq!(config.ui.layout, "auto");
    }

    #[test]
    fn message_and_sender_view_preferences_deserialize_with_stable_values() {
        let config = parse_validated(
            "[ui.message_view_preferences]\n\
             \"message@example.test\" = \"text\"\n\
             \n[ui.sender_view_preferences]\n\
             \"sender@example.test\" = \"visual_html\"\n",
        )
        .expect("view preference maps should validate");

        assert_eq!(
            config
                .ui
                .message_view_preferences
                .get("message@example.test"),
            Some(&MessageViewPreference::Text)
        );
        assert_eq!(
            config.ui.sender_view_preferences.get("sender@example.test"),
            Some(&MessageViewPreference::VisualHtml)
        );
    }

    #[test]
    fn live_test_harness_mutations_are_disabled_by_default() {
        let config: AppConfig = toml::from_str("").expect("empty config should deserialize");

        assert!(!config.automation.allow_live_send_test);
        assert!(!config.automation.allow_live_tag_test);

        let enabled: AppConfig = toml::from_str(
            "[automation]\nallow_live_send_test = true\nallow_live_tag_test = true\n",
        )
        .expect("explicit live-test gates should deserialize");
        assert!(enabled.automation.allow_live_send_test);
        assert!(enabled.automation.allow_live_tag_test);
    }

    #[test]
    fn ui_layout_can_select_stacked() {
        let config: AppConfig =
            toml::from_str("[ui]\nlayout = \"stacked\"\n").expect("layout config");

        assert_eq!(config.ui.layout, "stacked");
    }

    #[test]
    fn documented_theme_values_and_preview_line_bounds_validate() {
        for theme in ["system", "light", "dark"] {
            for preview_lines in [1, super::MAX_THREAD_PREVIEW_LINES] {
                parse_validated(&format!(
                    "[ui]\ntheme = {theme:?}\nthread_preview_lines = {preview_lines}\n"
                ))
                .unwrap_or_else(|error| {
                    panic!(
                        "theme {theme:?} with {preview_lines} preview lines should validate: {error:#}"
                    )
                });
            }
        }
    }

    #[test]
    fn non_numeric_preview_line_count_is_rejected_during_deserialization() {
        let error = toml::from_str::<AppConfig>("[ui]\nthread_preview_lines = \"two\"\n")
            .expect_err("non-numeric preview line count should fail")
            .to_string();

        assert!(error.contains("thread_preview_lines"), "{error}");
    }

    #[test]
    fn unknown_config_keys_are_rejected_at_each_schema_level() {
        for (case, contents, unknown) in [
            ("root", "[appearance]\ntheme = \"dark\"\n", "appearance"),
            (
                "notmuch",
                "[notmuch]\ndefualt_query = \"tag:inbox\"\n",
                "defualt_query",
            ),
            (
                "identity",
                "[identity]\nprimay_email = \"me@example.test\"\n",
                "primay_email",
            ),
            ("UI", "[ui]\npgae_size = 20\n", "pgae_size"),
            ("send", "[send]\ncommmand = \"sendmail\"\n", "commmand"),
            (
                "drafts",
                "[drafts]\nsave_maildirr = true\n",
                "save_maildirr",
            ),
            ("sync", "[sync]\nenabeld = true\n", "enabeld"),
            ("automation", "[automation]\ntokne = \"secret\"\n", "tokne"),
            (
                "saved search",
                "[[ui.custom_saved_searches]]\nname = \"Inbox\"\nquery = \"tag:inbox\"\ncolour = \"blue\"\n",
                "colour",
            ),
        ] {
            let error = toml::from_str::<AppConfig>(contents)
                .expect_err("unknown key should fail")
                .to_string();
            assert!(
                error.contains(unknown),
                "{case} error did not identify {unknown:?}: {error}"
            );
        }
    }

    #[test]
    fn legacy_keys_and_arbitrary_send_environment_are_accepted_but_not_serialized() {
        let config = parse_validated(
            "[ui]\nconfirm_destructive_tag_actions = false\n\
             \n[send]\none_live_self_test_per_run = true\n\
             \n[send.env]\nNOTM_CUSTOM_VARIABLE = \"value\"\n\
             \n[sync]\nshow_manual_sync_button = true\n",
        )
        .expect("legacy configuration should remain compatible");

        assert_eq!(
            config
                .send
                .env
                .get("NOTM_CUSTOM_VARIABLE")
                .map(String::as_str),
            Some("value")
        );
        let serialized = toml::to_string(&config).expect("serialize configuration");
        for legacy_key in [
            "confirm_destructive_tag_actions",
            "one_live_self_test_per_run",
            "show_manual_sync_button",
        ] {
            assert!(
                !serialized.contains(legacy_key),
                "legacy key {legacy_key} leaked into serialized configuration:\n{serialized}"
            );
        }
    }

    #[test]
    fn documented_layout_values_and_existing_aliases_validate() {
        for layout in [
            "",
            "auto",
            "three_pane",
            "columns",
            "three-pane",
            "side-by-side",
            "stacked",
        ] {
            parse_validated(&format!("[ui]\nlayout = {layout:?}\n"))
                .unwrap_or_else(|error| panic!("layout {layout:?} should validate: {error:#}"));
        }
    }

    #[test]
    fn invalid_config_values_report_their_dotted_key() {
        for (case, contents, key) in [
            (
                "read-only invariant",
                "[notmuch]\nopen_readwrite_only_for_mutations = false\n",
                "notmuch.open_readwrite_only_for_mutations",
            ),
            ("zero page size", "[ui]\npage_size = 0\n", "ui.page_size"),
            ("theme", "[ui]\ntheme = \"auto\"\n", "ui.theme"),
            (
                "zero preview lines",
                "[ui]\nthread_preview_lines = 0\n",
                "ui.thread_preview_lines",
            ),
            (
                "too many preview lines",
                "[ui]\nthread_preview_lines = 21\n",
                "ui.thread_preview_lines",
            ),
            ("layout", "[ui]\nlayout = \"diagonal\"\n", "ui.layout"),
            (
                "HTML mode",
                "[ui]\nhtml_mode = \"unsafe_html\"\n",
                "ui.html_mode",
            ),
            (
                "transport",
                "[send]\ntransport = \"smtp\"\n",
                "send.transport",
            ),
            ("transport mode", "[send]\nmode = \"magic\"\n", "send.mode"),
            (
                "command template placeholder",
                "[send]\nmode = \"command_template\"\nargs = [\"--message\"]\n",
                "send.args",
            ),
        ] {
            let error = parse_validated(contents)
                .expect_err("invalid value should fail")
                .to_string();
            assert!(
                error.contains(key),
                "{case} error did not identify {key}: {error}"
            );
        }
    }

    #[test]
    fn command_template_mode_accepts_a_file_placeholder() {
        parse_validated(
            "[send]\nmode = \"command_template\"\nargs = [\"--message-file={file}\"]\n",
        )
        .expect("command-template args containing {file} should validate");
    }
}
