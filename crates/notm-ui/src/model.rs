use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use notm_mail::SendReport;
use notm_notmuch::{MessageSummary, Revision, ThreadSummary};
use serde::{Deserialize, Serialize};

/// Largest supported visual line limit for a thread-list body preview.
///
/// Preview text remains bounded independently of this display limit so changing
/// the setting does not require a different thread-detail cache entry.
pub const MAX_THREAD_PREVIEW_LINES: usize = 20;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageViewPreference {
    Text,
    VisualHtml,
    FullHeaders,
    RawSource,
}

impl MessageViewPreference {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::VisualHtml => "Visual HTML",
            Self::FullHeaders => "Full headers",
            Self::RawSource => "Raw source",
        }
    }
}

impl ThemePreference {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

impl std::fmt::Display for ThemePreference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseThemePreferenceError;

impl std::fmt::Display for ParseThemePreferenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("expected exactly one of system, light, or dark")
    }
}

impl std::error::Error for ParseThemePreferenceError {}

impl std::str::FromStr for ThemePreference {
    type Err = ParseThemePreferenceError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "system" => Ok(Self::System),
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            _ => Err(ParseThemePreferenceError),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiState {
    pub current_query: String,
    pub visible_saved_search: Option<String>,
    pub thread_list_items: Vec<ThreadSummary>,
    pub thread_total_count: u32,
    pub thread_loaded_count: usize,
    pub thread_window_offset: usize,
    pub thread_page_size: usize,
    pub can_load_more_threads: bool,
    pub thread_details: BTreeMap<String, ThreadUiDetails>,
    pub selected_thread: Option<ThreadSummary>,
    pub selected_message: Option<MessageSummary>,
    pub messages: Vec<MessageSummary>,
    pub visible_tags: Vec<String>,
    pub address_suggestions: Vec<String>,
    pub trusted_image_senders: Vec<String>,
    pub pending_open_message_id: Option<String>,
    pub compose_fields: ComposeFields,
    #[serde(default)]
    pub compose_generation: u64,
    pub active_draft: Option<ActiveDraft>,
    pub input_mode: InputMode,
    pub active_pane: ActivePane,
    pub last_send_report: Option<SendReport>,
    #[serde(default)]
    pub send_in_progress: bool,
    #[serde(default)]
    pub sync_in_progress: bool,
    #[serde(default)]
    pub tag_in_progress: bool,
    #[serde(default)]
    pub tag_generation: u64,
    #[serde(default)]
    pub tag_warning: Option<String>,
    #[serde(default)]
    pub tag_paths_uncertain: bool,
    pub last_error: Option<String>,
    pub last_operation: Option<String>,
    #[serde(default)]
    pub search_loading: bool,
    #[serde(default)]
    pub search_generation: u64,
    #[serde(default)]
    pub full_search_outcome_generation: u64,
    #[serde(default)]
    pub full_search_outcome_error: Option<String>,
    #[serde(default)]
    pub pending_search_query: Option<String>,
    #[serde(default)]
    pub search_error: Option<String>,
    pub database_path: Option<String>,
    pub database_revision: Option<Revision>,
    pub automation_enabled: bool,
    pub screenshot_path: Option<PathBuf>,
    pub quote_collapse_enabled: bool,
    pub prefer_html_view: bool,
    #[serde(default)]
    pub message_view_preferences: BTreeMap<String, MessageViewPreference>,
    #[serde(default)]
    pub sender_view_preferences: BTreeMap<String, MessageViewPreference>,
    #[serde(default)]
    pub theme: ThemePreference,
    #[serde(default = "default_thread_preview_lines")]
    pub thread_preview_lines: usize,
    pub show_thread_numbers: bool,
    pub show_thread_dates: bool,
    pub show_thread_tags: bool,
    pub show_thread_preview: bool,
    pub show_keybind_hints: bool,
    pub layout_preference: LayoutPreference,
    pub content_layout: ContentLayout,
    pub visual_select_mode: bool,
    pub visual_select_anchor: Option<usize>,
    pub visual_select_cursor: Option<usize>,
    pub visual_selected_threads: BTreeSet<String>,
    #[serde(default)]
    pub visual_selected_thread_snapshots: BTreeMap<String, ThreadSummary>,
    #[serde(default)]
    pub visual_selection_request_generation: u64,
    pub visual_selection_pending_range: Option<(usize, usize)>,
    pub multi_selected_threads: BTreeSet<String>,
    #[serde(default)]
    pub multi_selected_thread_snapshots: BTreeMap<String, ThreadSummary>,
}

const fn default_thread_preview_lines() -> usize {
    2
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            current_query: "tag:inbox and not tag:trash and not tag:spam".to_string(),
            visible_saved_search: Some("Inbox".to_string()),
            thread_list_items: Vec::new(),
            thread_total_count: 0,
            thread_loaded_count: 0,
            thread_window_offset: 0,
            thread_page_size: 100,
            can_load_more_threads: false,
            thread_details: BTreeMap::new(),
            selected_thread: None,
            selected_message: None,
            messages: Vec::new(),
            visible_tags: Vec::new(),
            address_suggestions: Vec::new(),
            trusted_image_senders: Vec::new(),
            pending_open_message_id: None,
            compose_fields: ComposeFields::default(),
            compose_generation: 0,
            active_draft: None,
            input_mode: InputMode::Normal,
            active_pane: ActivePane::Threads,
            last_send_report: None,
            send_in_progress: false,
            sync_in_progress: false,
            tag_in_progress: false,
            tag_generation: 0,
            tag_warning: None,
            tag_paths_uncertain: false,
            last_error: None,
            last_operation: None,
            search_loading: false,
            search_generation: 0,
            full_search_outcome_generation: 0,
            full_search_outcome_error: None,
            pending_search_query: None,
            search_error: None,
            database_path: None,
            database_revision: None,
            automation_enabled: false,
            screenshot_path: None,
            quote_collapse_enabled: false,
            prefer_html_view: false,
            message_view_preferences: BTreeMap::new(),
            sender_view_preferences: BTreeMap::new(),
            theme: ThemePreference::System,
            thread_preview_lines: 2,
            show_thread_numbers: true,
            show_thread_dates: true,
            show_thread_tags: true,
            show_thread_preview: true,
            show_keybind_hints: true,
            layout_preference: LayoutPreference::Auto,
            content_layout: ContentLayout::ThreePane,
            visual_select_mode: false,
            visual_select_anchor: None,
            visual_select_cursor: None,
            visual_selected_threads: BTreeSet::new(),
            visual_selected_thread_snapshots: BTreeMap::new(),
            visual_selection_request_generation: 0,
            visual_selection_pending_range: None,
            multi_selected_threads: BTreeSet::new(),
            multi_selected_thread_snapshots: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadUiDetails {
    pub has_attachment: bool,
    pub has_encrypted: bool,
    pub has_signed: bool,
    pub preview: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Insert,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActivePane {
    Sidebar,
    Threads,
    Message,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LayoutPreference {
    #[default]
    Auto,
    ThreePane,
    Stacked,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContentLayout {
    #[default]
    ThreePane,
    Stacked,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComposeFields {
    pub from: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub attachments: Vec<String>,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub text_reply_quote: Option<String>,
    #[serde(default)]
    pub html_reply_quote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveDraft {
    pub path: PathBuf,
    pub message_id: Option<String>,
    pub indexed: bool,
    pub saved_fields: ComposeFields,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{MessageViewPreference, ThemePreference, UiState};

    #[test]
    fn theme_preference_accepts_only_the_documented_exact_values() {
        for (value, expected) in [
            ("system", ThemePreference::System),
            ("light", ThemePreference::Light),
            ("dark", ThemePreference::Dark),
        ] {
            assert_eq!(ThemePreference::from_str(value), Ok(expected));
            assert_eq!(expected.as_str(), value);
        }

        for invalid in ["", "System", "LIGHT", "dark ", "auto"] {
            assert!(
                ThemePreference::from_str(invalid).is_err(),
                "unexpectedly accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn ui_state_without_new_settings_fields_uses_compatible_defaults() {
        let mut serialized = serde_json::to_value(UiState::default()).expect("serialize UI state");
        let object = serialized.as_object_mut().expect("UI state object");
        object.remove("theme");
        object.remove("thread_preview_lines");
        object.remove("message_view_preferences");
        object.remove("sender_view_preferences");

        let restored: UiState = serde_json::from_value(serialized).expect("deserialize old state");
        assert_eq!(restored.theme, ThemePreference::System);
        assert_eq!(restored.thread_preview_lines, 2);
        assert!(restored.message_view_preferences.is_empty());
        assert!(restored.sender_view_preferences.is_empty());
    }

    #[test]
    fn message_view_preferences_have_stable_config_values_and_labels() {
        for (preference, serialized, label) in [
            (MessageViewPreference::Text, "\"text\"", "Text"),
            (
                MessageViewPreference::VisualHtml,
                "\"visual_html\"",
                "Visual HTML",
            ),
            (
                MessageViewPreference::FullHeaders,
                "\"full_headers\"",
                "Full headers",
            ),
            (
                MessageViewPreference::RawSource,
                "\"raw_source\"",
                "Raw source",
            ),
        ] {
            assert_eq!(serde_json::to_string(&preference).unwrap(), serialized);
            assert_eq!(preference.label(), label);
        }
    }
}
