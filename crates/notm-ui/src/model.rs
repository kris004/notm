use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use notm_mail::SendReport;
use notm_notmuch::{MessageSummary, Revision, ThreadSummary};
use serde::{Deserialize, Serialize};

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
    pub compose_fields: ComposeFields,
    pub active_draft: Option<ActiveDraft>,
    pub input_mode: InputMode,
    pub active_pane: ActivePane,
    pub last_send_report: Option<SendReport>,
    pub last_error: Option<String>,
    pub last_operation: Option<String>,
    pub database_path: Option<String>,
    pub database_revision: Option<Revision>,
    pub automation_enabled: bool,
    pub screenshot_path: Option<PathBuf>,
    pub quote_collapse_enabled: bool,
    pub prefer_html_view: bool,
    pub show_thread_numbers: bool,
    pub show_thread_dates: bool,
    pub show_thread_tags: bool,
    pub show_thread_preview: bool,
    pub show_keybind_hints: bool,
    pub visual_select_mode: bool,
    pub visual_select_anchor: Option<usize>,
    pub visual_select_cursor: Option<usize>,
    pub visual_selected_threads: BTreeSet<String>,
    pub visual_selection_pending_range: Option<(usize, usize)>,
    pub multi_selected_threads: BTreeSet<String>,
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
            compose_fields: ComposeFields::default(),
            active_draft: None,
            input_mode: InputMode::Normal,
            active_pane: ActivePane::Threads,
            last_send_report: None,
            last_error: None,
            last_operation: None,
            database_path: None,
            database_revision: None,
            automation_enabled: false,
            screenshot_path: None,
            quote_collapse_enabled: false,
            prefer_html_view: false,
            show_thread_numbers: true,
            show_thread_dates: true,
            show_thread_tags: true,
            show_thread_preview: true,
            show_keybind_hints: true,
            visual_select_mode: false,
            visual_select_anchor: None,
            visual_select_cursor: None,
            visual_selected_threads: BTreeSet::new(),
            visual_selection_pending_range: None,
            multi_selected_threads: BTreeSet::new(),
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
