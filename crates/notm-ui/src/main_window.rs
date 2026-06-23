use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::{Mutex, OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use chrono::Utc;
use gtk::prelude::*;
use gtk4 as gtk;
use notm_mail::{
    ComposedMessage, ExternalCommandTransport, FakeSendTransport, ReplyKind, SendTransport,
    TransportMode,
    address::{dedupe_addresses, format_address, parse_address_list},
    build_reply,
    compose::{AttachmentInput, Identity},
    forward::{build_attachment_forward, build_inline_forward},
    html_sanitize::sanitize_html,
    mime::{extract_attachments_from_file, parse_file},
};
use notm_notmuch::{Database, DatabaseMode, OpenConfig, QueryOptions, SortOrder, TagMutation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sourceview5::{Buffer as SourceBuffer, View as SourceView, VimIMContext};
use uuid::Uuid;
use webkit6::{
    NavigationPolicyDecision, PolicyDecisionType,
    prelude::{PolicyDecisionExt, WebViewExt},
};

use crate::{
    automation::{self, AutomationConfig, AutomationRequest},
    model::{ActiveDraft, ActivePane, ComposeFields, InputMode, ThreadUiDetails, UiState},
    screenshot, shortcuts,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSearch {
    pub name: String,
    pub query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchOptions {
    pub database_path: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub profile: Option<String>,
    pub default_query: String,
    pub excluded_tags: Vec<String>,
    pub page_size: usize,
    pub identity_name: Option<String>,
    pub primary_email: Option<String>,
    pub other_email: Vec<String>,
    pub send_enabled: bool,
    pub send_command: Option<PathBuf>,
    pub send_args: Vec<String>,
    pub send_mode: TransportMode,
    pub send_working_dir: Option<PathBuf>,
    pub send_env: BTreeMap<String, String>,
    pub send_timeout_seconds: u64,
    pub fake_send_capture_dir: Option<PathBuf>,
    pub save_sent: bool,
    pub sent_maildir: Option<PathBuf>,
    pub sent_tags: Vec<String>,
    pub index_sent_after_send: bool,
    pub save_drafts_to_maildir: bool,
    pub draft_maildir: Option<PathBuf>,
    pub draft_tags: Vec<String>,
    pub index_draft_after_save: bool,
    pub sync_enabled: bool,
    pub manual_sync_label: String,
    pub notmuch_database_update_enabled: bool,
    pub notmuch_database_update_on_startup: bool,
    pub notmuch_database_update_command: String,
    pub external_receive_enabled: bool,
    pub external_receive_on_startup: bool,
    pub external_receive_command: String,
    pub screenshot_dir: PathBuf,
    pub automation_enabled: bool,
    pub automation_socket: Option<PathBuf>,
    pub automation_token: Option<String>,
    pub show_debug_panel: bool,
    pub start_maximized: bool,
    pub remote_images: bool,
    pub html_mode: String,
    pub trusted_image_senders: Vec<String>,
    pub hidden_tag_searches: Vec<String>,
    pub sync_maildir_flags_after_tag_change: bool,
    pub draft_path: Option<PathBuf>,
    pub drafts_dir: Option<PathBuf>,
    pub app_config_path: Option<PathBuf>,
    pub custom_saved_searches: Vec<SavedSearch>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            database_path: None,
            config_path: None,
            profile: None,
            default_query: "tag:inbox and not tag:trash and not tag:spam".to_string(),
            excluded_tags: vec!["trash".to_string(), "spam".to_string()],
            page_size: 100,
            identity_name: None,
            primary_email: None,
            other_email: Vec::new(),
            send_enabled: true,
            send_command: None,
            send_args: Vec::new(),
            send_mode: TransportMode::Auto,
            send_working_dir: None,
            send_env: BTreeMap::new(),
            send_timeout_seconds: 120,
            fake_send_capture_dir: None,
            save_sent: false,
            sent_maildir: None,
            sent_tags: vec!["sent".to_string()],
            index_sent_after_send: false,
            save_drafts_to_maildir: true,
            draft_maildir: None,
            draft_tags: vec!["draft".to_string()],
            index_draft_after_save: true,
            sync_enabled: false,
            manual_sync_label: "Sync".to_string(),
            notmuch_database_update_enabled: false,
            notmuch_database_update_on_startup: false,
            notmuch_database_update_command: String::new(),
            external_receive_enabled: false,
            external_receive_on_startup: false,
            external_receive_command: String::new(),
            screenshot_dir: PathBuf::from("artifacts/screenshots"),
            automation_enabled: false,
            automation_socket: None,
            automation_token: None,
            show_debug_panel: false,
            start_maximized: false,
            remote_images: false,
            html_mode: "sanitize_then_render_text_fallback".to_string(),
            trusted_image_senders: Vec::new(),
            hidden_tag_searches: Vec::new(),
            sync_maildir_flags_after_tag_change: true,
            draft_path: None,
            drafts_dir: None,
            app_config_path: None,
            custom_saved_searches: Vec::new(),
        }
    }
}

pub fn launch(options: LaunchOptions) -> anyhow::Result<()> {
    let app = gtk::Application::builder()
        .application_id("dev.notm.Notm")
        .build();
    app.connect_activate(move |app| build_ui(app, options.clone()));
    app.run_with_args(&["notm"]);
    Ok(())
}

#[derive(Clone)]
struct Widgets {
    window: gtk::ApplicationWindow,
    left_pane: gtk::ScrolledWindow,
    thread_pane: gtk::Box,
    message_pane: gtk::Box,
    saved_box: gtk::Box,
    saved_name_entry: gtk::Entry,
    saved_query_entry: gtk::Entry,
    save_search_button: gtk::Button,
    custom_tag_entry: gtk::Entry,
    search_entry: gtk::Entry,
    search_button: gtk::Button,
    search_generation: Rc<Cell<u64>>,
    search_suggestions_list: gtk::ListBox,
    search_completion: Rc<RefCell<Option<SearchCompletionSession>>>,
    hidden_tag_searches: HiddenTagSearchStore,
    thread_list: gtk::ListBox,
    thread_result_label: gtk::Label,
    load_more_button: gtk::Button,
    thread_scrolled: gtk::ScrolledWindow,
    compose_button: gtk::Button,
    debug_button: gtk::Button,
    palette_button: gtk::Button,
    settings_button: gtk::Button,
    help_button: gtk::Button,
    archive_button: gtk::Button,
    read_toggle_button: gtk::Button,
    flag_toggle_button: gtk::Button,
    trash_button: gtk::Button,
    spam_button: gtk::Button,
    tag_command_entry: gtk::Entry,
    tag_command_apply_button: gtk::Button,
    tag_menu_button: gtk::MenuButton,
    tag_menu_box: gtk::Box,
    add_custom_tag_button: gtk::Button,
    remove_custom_tag_button: gtk::Button,
    undo_tag_button: gtk::MenuButton,
    undo_menu_box: gtk::Box,
    undo_last_tag_button: gtk::Button,
    undo_list_tag_button: gtk::Button,
    message_stack: gtk::Stack,
    message_view: gtk::TextView,
    message_scrolled: gtk::ScrolledWindow,
    html_view: webkit6::WebView,
    html_scrolled: gtk::ScrolledWindow,
    response_menu_button: gtk::MenuButton,
    reply_button: gtk::Button,
    reply_all_button: gtk::Button,
    forward_button: gtk::Button,
    forward_attachment_button: gtk::Button,
    response_menu_box: gtk::Box,
    message_menu_button: gtk::MenuButton,
    message_menu_box: gtk::Box,
    view_menu_button: gtk::MenuButton,
    view_menu_box: gtk::Box,
    view_text_button: gtk::Button,
    view_html_button: gtk::Button,
    view_headers_button: gtk::Button,
    view_raw_button: gtk::Button,
    image_policy_button: gtk::Button,
    html_policy_row: gtk::Box,
    html_policy_label: gtk::Label,
    message_header_label: gtk::Label,
    collapse_quotes_button: gtk::Button,
    copy_menu_button: gtk::MenuButton,
    copy_menu_box: gtk::Box,
    copy_message_id_button: gtk::Button,
    copy_thread_id_button: gtk::Button,
    copy_from_email_button: gtk::Button,
    copy_to_email_button: gtk::Button,
    copy_cc_email_button: gtk::Button,
    copy_subject_button: gtk::Button,
    quote_collapse: Rc<Cell<bool>>,
    attachment_title: gtk::Label,
    attachment_scrolled: gtk::ScrolledWindow,
    attachment_list: gtk::ListBox,
    attachment_items: Rc<RefCell<Vec<ThreadAttachmentItem>>>,
    tag_search_box: gtk::Box,
    draft_path: PathBuf,
    debug_view: gtk::TextView,
    status_label: gtk::Label,
    compose_from: gtk::Entry,
    compose_to: gtk::Entry,
    compose_cc: gtk::Entry,
    compose_bcc: gtk::Entry,
    compose_subject: gtk::Entry,
    compose_body: SourceView,
    compose_vim_context: VimIMContext,
    compose_scrolled: gtk::ScrolledWindow,
    compose_attachments: gtk::Label,
    add_attachment_button: gtk::Button,
    save_draft_button: gtk::Button,
    clear_draft_button: gtk::Button,
    delete_local_draft_button: gtk::Button,
    send_button: gtk::Button,
    address_suggestions_list: gtk::ListBox,
    active_address_entry: Rc<RefCell<Option<gtk::Entry>>>,
    active_address_field: Rc<Cell<Option<RecipientField>>>,
    address_completion: Rc<RefCell<Option<AddressCompletionSession>>>,
    draft_list: gtk::ListBox,
    drafts_dir: PathBuf,
}

type SharedState = Rc<RefCell<UiState>>;
type UndoState = Rc<RefCell<Vec<UndoTagAction>>>;
type SavedSearchStore = Rc<RefCell<Vec<SavedSearch>>>;
type HiddenTagSearchStore = Rc<RefCell<BTreeSet<String>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecipientField {
    To,
    Cc,
    Bcc,
}

#[derive(Debug, Clone)]
struct AddressCompletionSession {
    field: RecipientField,
    base: String,
    suggestions: Vec<String>,
    next_index: usize,
    generated_text: Option<String>,
    suppress_next_change: bool,
}

#[derive(Debug, Clone)]
struct SearchCompletionSession {
    base: String,
    cursor_position: i32,
    suggestions: Vec<String>,
    next_index: usize,
    generated_text: Option<String>,
    suppress_next_change: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UndoTagAction {
    query: String,
    mutation: TagMutation,
    label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageViewKind {
    Text,
    Html,
    Headers,
    Raw,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThreadAttachmentItem {
    message_index: usize,
    attachment_index: usize,
    message_id: String,
    filename: String,
    content_type: String,
    size: usize,
}

#[derive(Debug, Clone)]
struct SearchData {
    query: String,
    threads: Vec<notm_notmuch::ThreadSummary>,
    details: BTreeMap<String, ThreadUiDetails>,
    count: u32,
    offset: usize,
    limit: usize,
    tags: Vec<String>,
    database_path: String,
    revision: notm_notmuch::Revision,
    cached: bool,
}

struct SearchResponse {
    generation: u64,
    result: anyhow::Result<SearchData>,
}

struct AddressSuggestionsResponse {
    result: anyhow::Result<Vec<String>>,
}

struct ThreadPageResponse {
    generation: u64,
    target_index: usize,
    visual_anchor_index: Option<usize>,
    result: anyhow::Result<SearchData>,
}

struct ThreadRangeSelectionResponse {
    generation: u64,
    anchor_index: usize,
    cursor_index: usize,
    result: anyhow::Result<BTreeSet<String>>,
}

static SEARCH_CACHE: OnceLock<Mutex<BTreeMap<String, SearchData>>> = OnceLock::new();
static THREAD_DETAIL_CACHE: OnceLock<Mutex<BTreeMap<String, ThreadUiDetails>>> = OnceLock::new();

const SIDEBAR_MIN_WIDTH: i32 = 136;
const THREAD_LIST_MIN_WIDTH: i32 = 320;
const COMPOSE_BODY_MIN_HEIGHT: i32 = 96;
const COMPOSE_BODY_NATURAL_HEIGHT: i32 = 260;
const KEYBOARD_CURSOR_CLASS: &str = "notm-keyboard-cursor";

fn build_ui(app: &gtk::Application, options: LaunchOptions) {
    install_css();

    let initial_state = UiState {
        current_query: options.default_query.clone(),
        thread_page_size: options.page_size,
        automation_enabled: options.automation_enabled,
        database_path: options
            .database_path
            .as_ref()
            .map(|p| p.display().to_string()),
        prefer_html_view: options.html_mode == "visual_html_preferred",
        trusted_image_senders: normalize_sender_list(&options.trusted_image_senders),
        compose_fields: ComposeFields {
            from: identity(&options)
                .map(|i| i.formatted())
                .unwrap_or_default(),
            ..ComposeFields::default()
        },
        ..UiState::default()
    };
    let state = Rc::new(RefCell::new(initial_state));
    let undo_state: UndoState = Rc::new(RefCell::new(load_undo_tag_actions()));
    let search_generation = Rc::new(Cell::new(0_u64));
    let hidden_tag_searches: HiddenTagSearchStore = Rc::new(RefCell::new(
        options.hidden_tag_searches.iter().cloned().collect(),
    ));
    let quote_collapse = Rc::new(Cell::new(false));

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("notm")
        .default_width(1500)
        .default_height(900)
        .build();
    window.set_widget_name("notm-main-window");
    if options.start_maximized {
        window.maximize();
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let toolbar = button_flow(8);
    toolbar.set_margin_start(8);
    toolbar.set_margin_end(8);
    toolbar.set_margin_top(8);
    toolbar.set_margin_bottom(8);

    let compose_button = gtk::Button::with_label("Compose");
    compose_button.set_widget_name("notm-compose-button");
    let debug_button = gtk::Button::with_label("Debug");
    let palette_button = gtk::Button::with_label("Commands");
    let settings_button = gtk::Button::with_label("Settings");
    let help_button = gtk::Button::with_label("Help");
    for b in [
        &compose_button,
        &debug_button,
        &palette_button,
        &settings_button,
        &help_button,
    ] {
        toolbar.insert(b, -1);
    }
    root.append(&toolbar);

    let left = gtk::Box::new(gtk::Orientation::Vertical, 6);
    left.set_widget_name("notm-left-sidebar-content");
    left.set_size_request(SIDEBAR_MIN_WIDTH, -1);
    left.set_focusable(true);

    let sidebar_scrolled = gtk::ScrolledWindow::new();
    sidebar_scrolled.set_widget_name("notm-left-sidebar");
    sidebar_scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    sidebar_scrolled.set_size_request(SIDEBAR_MIN_WIDTH, -1);
    sidebar_scrolled.set_min_content_width(SIDEBAR_MIN_WIDTH);
    sidebar_scrolled.set_hexpand(false);
    sidebar_scrolled.set_vexpand(true);
    sidebar_scrolled.set_focusable(true);
    sidebar_scrolled.set_margin_start(8);
    sidebar_scrolled.set_margin_end(8);
    sidebar_scrolled.set_margin_top(8);
    sidebar_scrolled.set_margin_bottom(8);
    sidebar_scrolled.set_child(Some(&left));

    let sidebar_title = gtk::Label::new(Some("Saved searches"));
    sidebar_title.add_css_class("heading");
    sidebar_title.set_xalign(0.0);
    left.append(&sidebar_title);
    let saved_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    saved_box.set_widget_name("notm-saved-searches");
    left.append(&saved_box);
    let (custom_search_button, custom_search_box) = menu_button_with_box(
        "Add custom search",
        "notm-custom-search-menu-button",
        &state,
    );
    let saved_editor_title = gtk::Label::new(Some("Custom saved search"));
    saved_editor_title.set_xalign(0.0);
    saved_editor_title.add_css_class("dim-label");
    saved_editor_title.set_wrap(true);
    custom_search_box.append(&saved_editor_title);
    let saved_name_entry = entry_with_placeholder("Name");
    saved_name_entry.set_widget_name("notm-saved-search-name");
    saved_name_entry.set_width_chars(10);
    saved_name_entry.set_max_width_chars(10);
    let saved_query_entry = entry_with_placeholder("Query");
    saved_query_entry.set_widget_name("notm-saved-search-query");
    saved_query_entry.set_width_chars(10);
    saved_query_entry.set_max_width_chars(10);
    custom_search_box.append(&saved_name_entry);
    custom_search_box.append(&saved_query_entry);
    let saved_editor_buttons = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let save_search_button = gtk::Button::with_label("Save search");
    save_search_button.set_widget_name("notm-save-search-button");
    saved_editor_buttons.append(&save_search_button);
    custom_search_box.append(&saved_editor_buttons);
    left.append(&custom_search_button);

    let tag_title = gtk::Label::new(Some("Tags"));
    tag_title.set_xalign(0.0);
    tag_title.add_css_class("heading");
    left.append(&tag_title);
    let tag_search_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    tag_search_box.set_widget_name("notm-tag-searches");
    left.append(&tag_search_box);
    let manual_sync_button = if options.sync_enabled {
        let sync_button = gtk::Button::with_label(&options.manual_sync_label);
        sync_button.set_widget_name("notm-manual-sync-button");
        left.append(&sync_button);
        Some(sync_button)
    } else {
        None
    };

    let middle = gtk::Box::new(gtk::Orientation::Vertical, 6);
    middle.set_widget_name("notm-thread-pane");
    middle.set_margin_start(8);
    middle.set_margin_end(8);
    middle.set_margin_top(8);
    middle.set_margin_bottom(8);
    middle.set_size_request(THREAD_LIST_MIN_WIDTH, -1);
    middle.set_focusable(true);

    let controls_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    controls_box.set_hexpand(true);
    controls_box.set_halign(gtk::Align::Fill);
    middle.append(&controls_box);

    let search_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let search_entry = gtk::Entry::new();
    search_entry.set_widget_name("notm-search-entry");
    search_entry.set_hexpand(true);
    search_entry.set_text(&options.default_query);
    search_entry.set_placeholder_text(Some(
        "Notmuch query, e.g. tag:inbox and not tag:trash and not tag:spam",
    ));
    let search_suggestions_list = gtk::ListBox::new();
    search_suggestions_list.set_widget_name("notm-search-suggestions-list");
    search_suggestions_list.set_selection_mode(gtk::SelectionMode::Single);
    search_suggestions_list.add_css_class("boxed-list");
    search_suggestions_list.set_hexpand(true);
    search_suggestions_list.set_focusable(false);
    search_suggestions_list.set_visible(false);
    let search_completion = Rc::new(RefCell::new(None::<SearchCompletionSession>));
    let search_button = gtk::Button::with_label("Search");
    search_button.set_widget_name("notm-search-button");
    search_row.append(&search_entry);
    search_row.append(&search_button);
    controls_box.append(&search_row);
    controls_box.append(&search_suggestions_list);
    let helper = gtk::Label::new(Some(
        "Syntax: tag:inbox, from:alice, subject:report, thread:<id>, *",
    ));
    helper.set_xalign(0.0);
    helper.add_css_class("dim-label");
    controls_box.append(&helper);

    let action_outer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    action_outer.set_hexpand(true);
    action_outer.set_halign(gtk::Align::Fill);
    action_outer.set_valign(gtk::Align::Start);
    let action_row = button_flow(4);
    action_row.set_halign(gtk::Align::Fill);
    action_row.set_hexpand(true);
    let archive_button = gtk::Button::with_label("Archive");
    let read_button = gtk::Button::with_label("Mark read");
    read_button.set_widget_name("notm-read-toggle-button");
    let flag_button = gtk::Button::with_label("Flag");
    flag_button.set_widget_name("notm-flag-toggle-button");
    let trash_button = gtk::Button::with_label("Trash");
    let spam_button = gtk::Button::with_label("Spam");
    let (undo_button, undo_menu_box) =
        menu_button_with_box("Undo", "notm-undo-tag-menu-button", &state);
    undo_button.set_widget_name("notm-undo-tag-button");
    undo_button.add_css_class("suggested-action");
    undo_button.set_halign(gtk::Align::End);
    undo_button.set_valign(gtk::Align::Start);
    undo_button.set_vexpand(false);
    undo_button.set_visible(false);
    undo_button.set_tooltip_text(Some("Undo recent tag operations."));
    undo_menu_box.set_spacing(6);
    undo_menu_box.set_margin_start(6);
    undo_menu_box.set_margin_end(6);
    undo_menu_box.set_margin_top(6);
    undo_menu_box.set_margin_bottom(6);
    let undo_last_button = gtk::Button::with_label("Undo last");
    undo_last_button.set_widget_name("notm-undo-last-tag-button");
    let undo_list_button = gtk::Button::with_label("Undo multiple");
    undo_list_button.set_widget_name("notm-undo-list-tag-button");
    undo_menu_box.append(&undo_last_button);
    undo_menu_box.append(&undo_list_button);
    let (tag_menu_button, tag_menu_box) =
        menu_button_with_box("Tag…", "notm-custom-tag-menu-button", &state);
    tag_menu_box.set_spacing(6);
    tag_menu_box.set_margin_start(6);
    tag_menu_box.set_margin_end(6);
    tag_menu_box.set_margin_top(6);
    tag_menu_box.set_margin_bottom(6);
    let single_tag_label = gtk::Label::new(Some("Single tag"));
    single_tag_label.set_xalign(0.0);
    single_tag_label.add_css_class("dim-label");
    tag_menu_box.append(&single_tag_label);
    let custom_tag_entry = entry_with_placeholder("tag");
    custom_tag_entry.set_widget_name("notm-custom-tag-entry");
    custom_tag_entry.set_width_chars(18);
    custom_tag_entry.set_hexpand(true);
    let tag_button_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tag_button_row.set_hexpand(true);
    tag_button_row.set_homogeneous(true);
    let add_tag_button = gtk::Button::with_label("Add tag");
    add_tag_button.set_widget_name("notm-add-custom-tag-button");
    add_tag_button.set_hexpand(true);
    add_tag_button.set_halign(gtk::Align::Fill);
    let remove_tag_button = gtk::Button::with_label("Remove tag");
    remove_tag_button.set_widget_name("notm-remove-custom-tag-button");
    remove_tag_button.set_hexpand(true);
    remove_tag_button.set_halign(gtk::Align::Fill);
    remove_tag_button.set_visible(false);
    tag_button_row.append(&add_tag_button);
    tag_button_row.append(&remove_tag_button);
    tag_menu_box.append(&custom_tag_entry);
    tag_menu_box.append(&tag_button_row);
    let multi_tag_label = gtk::Label::new(Some("Multiple tag changes"));
    multi_tag_label.set_xalign(0.0);
    multi_tag_label.add_css_class("dim-label");
    tag_menu_box.append(&multi_tag_label);
    let tag_command_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tag_command_row.set_widget_name("notm-tag-command-row");
    tag_command_row.set_hexpand(true);
    let tag_command_entry = entry_with_placeholder("-inbox +books +flagged");
    tag_command_entry.set_widget_name("notm-tag-command-entry");
    tag_command_entry.set_hexpand(true);
    let tag_command_apply_button = gtk::Button::with_label("Apply");
    tag_command_apply_button.set_widget_name("notm-run-tag-command-button");
    tag_command_row.append(&tag_command_entry);
    tag_command_row.append(&tag_command_apply_button);
    tag_menu_box.append(&tag_command_row);
    for b in [
        &archive_button,
        &read_button,
        &flag_button,
        &trash_button,
        &spam_button,
    ] {
        action_row.insert(b, -1);
    }
    action_row.insert(&tag_menu_button, -1);
    let undo_row = button_flow(4);
    undo_row.set_widget_name("notm-undo-tag-row");
    undo_row.set_hexpand(false);
    undo_row.set_halign(gtk::Align::End);
    undo_row.set_valign(gtk::Align::Start);
    undo_row.set_min_children_per_line(1);
    undo_row.set_max_children_per_line(1);
    undo_button.set_hexpand(false);
    undo_button.set_halign(gtk::Align::Fill);
    undo_button.set_valign(gtk::Align::Fill);
    undo_row.insert(&undo_button, -1);
    action_outer.append(&action_row);
    action_outer.append(&undo_row);
    controls_box.append(&action_outer);

    let thread_list = gtk::ListBox::new();
    thread_list.set_widget_name("notm-thread-list");
    thread_list.set_selection_mode(gtk::SelectionMode::Single);
    let scrolled_threads = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&thread_list)
        .build();
    middle.append(&scrolled_threads);
    let thread_result_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let thread_result_label = gtk::Label::new(Some("No results loaded"));
    thread_result_label.set_widget_name("notm-thread-result-label");
    thread_result_label.set_xalign(0.0);
    thread_result_label.set_hexpand(true);
    let load_more_button = gtk::Button::with_label("Load more");
    load_more_button.set_widget_name("notm-load-more-threads-button");
    load_more_button.set_sensitive(false);
    thread_result_row.append(&thread_result_label);
    thread_result_row.append(&load_more_button);
    middle.append(&thread_result_row);

    let right = gtk::Box::new(gtk::Orientation::Vertical, 6);
    right.set_widget_name("notm-message-pane");
    right.set_margin_start(8);
    right.set_margin_end(8);
    right.set_margin_top(8);
    right.set_margin_bottom(8);
    right.set_hexpand(true);
    right.set_vexpand(true);
    right.set_focusable(true);

    let message_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    message_actions.set_widget_name("notm-message-actions");
    message_actions.set_halign(gtk::Align::Start);
    let (response_menu_button, response_menu_box) =
        menu_button_with_box("Respond", "notm-response-menu-button", &state);
    let reply_button = gtk::Button::with_label("Reply");
    reply_button.set_widget_name("notm-reply-button");
    let reply_all_button = gtk::Button::with_label("Reply all");
    reply_all_button.set_widget_name("notm-reply-all-button");
    let forward_button = gtk::Button::with_label("Forward");
    forward_button.set_widget_name("notm-forward-button");
    let forward_attachment_button = gtk::Button::with_label("Forward attached");
    forward_attachment_button.set_widget_name("notm-forward-attachment-button");
    for b in [
        &reply_button,
        &reply_all_button,
        &forward_button,
        &forward_attachment_button,
    ] {
        response_menu_box.append(b);
    }
    let (message_menu_button, message_menu_box) =
        menu_button_with_box("Message", "notm-message-menu-button", &state);
    let (view_menu_button, view_menu_box) =
        menu_button_with_box("View", "notm-view-menu-button", &state);
    let view_text_button = gtk::Button::with_label("Text");
    view_text_button.set_widget_name("notm-view-text-button");
    let view_html_button = gtk::Button::with_label("Visual HTML");
    view_html_button.set_widget_name("notm-view-html-button");
    let view_headers_button = gtk::Button::with_label("Full headers");
    view_headers_button.set_widget_name("notm-view-headers-button");
    let view_raw_button = gtk::Button::with_label("Raw source");
    view_raw_button.set_widget_name("notm-view-raw-button");
    for b in [
        &view_text_button,
        &view_html_button,
        &view_headers_button,
        &view_raw_button,
    ] {
        view_menu_box.append(b);
    }
    let image_policy_button = gtk::Button::with_label("Load images once");
    image_policy_button.set_widget_name("notm-image-policy-button");
    let collapse_quotes_button = gtk::Button::with_label("Collapse quotes");
    collapse_quotes_button.set_widget_name("notm-collapse-quotes-button");
    let (copy_menu_button, copy_menu_box) =
        menu_button_with_box("Copy", "notm-copy-menu-button", &state);
    let copy_message_id_button = gtk::Button::with_label("Copy message id");
    copy_message_id_button.set_widget_name("notm-copy-message-id-button");
    let copy_thread_id_button = gtk::Button::with_label("Copy thread id");
    copy_thread_id_button.set_widget_name("notm-copy-thread-id-button");
    let copy_from_email_button = gtk::Button::with_label("Copy from email");
    copy_from_email_button.set_widget_name("notm-copy-from-email-button");
    let copy_to_email_button = gtk::Button::with_label("Copy to email");
    copy_to_email_button.set_widget_name("notm-copy-to-email-button");
    let copy_cc_email_button = gtk::Button::with_label("Copy cc email");
    copy_cc_email_button.set_widget_name("notm-copy-cc-email-button");
    let copy_subject_button = gtk::Button::with_label("Copy subject");
    copy_subject_button.set_widget_name("notm-copy-subject-button");
    for b in [
        &copy_message_id_button,
        &copy_thread_id_button,
        &copy_from_email_button,
        &copy_to_email_button,
        &copy_cc_email_button,
        &copy_subject_button,
    ] {
        copy_menu_box.append(b);
    }
    message_actions.append(&response_menu_button);
    message_actions.append(&message_menu_button);
    message_actions.append(&view_menu_button);
    message_actions.append(&collapse_quotes_button);
    message_actions.append(&copy_menu_button);
    right.append(&message_actions);

    let attachment_title = gtk::Label::new(Some("Attachments in thread"));
    attachment_title.set_xalign(0.0);
    attachment_title.add_css_class("dim-label");
    attachment_title.set_visible(false);
    right.append(&attachment_title);
    let attachment_list = gtk::ListBox::new();
    attachment_list.set_widget_name("notm-attachment-list");
    attachment_list.set_selection_mode(gtk::SelectionMode::Single);
    attachment_list.add_css_class("boxed-list");
    let scrolled_attachments = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(false)
        .child(&attachment_list)
        .build();
    scrolled_attachments.set_visible(false);
    right.append(&scrolled_attachments);

    let html_policy_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    html_policy_row.set_widget_name("notm-html-policy-row");
    html_policy_row.set_visible(false);
    let html_policy_label = gtk::Label::new(None);
    html_policy_label.set_widget_name("notm-html-policy-label");
    html_policy_label.set_xalign(0.0);
    html_policy_label.set_wrap(true);
    html_policy_label.set_hexpand(true);
    html_policy_label.add_css_class("dim-label");
    image_policy_button.set_halign(gtk::Align::End);
    html_policy_row.append(&html_policy_label);
    html_policy_row.append(&image_policy_button);
    right.append(&html_policy_row);

    let message_header_label = gtk::Label::new(None);
    message_header_label.set_widget_name("notm-message-header");
    message_header_label.set_xalign(0.0);
    message_header_label.set_wrap(true);
    message_header_label.set_selectable(true);
    message_header_label.set_visible(false);
    right.append(&message_header_label);

    let message_view = gtk::TextView::new();
    message_view.set_widget_name("notm-message-view");
    message_view.set_editable(false);
    message_view.set_monospace(false);
    message_view.set_wrap_mode(gtk::WrapMode::WordChar);
    let scrolled_message = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&message_view)
        .build();
    let html_view = webkit6::WebView::new();
    html_view.set_widget_name("notm-html-view");
    html_view.set_hexpand(true);
    html_view.set_vexpand(true);
    configure_html_webview(&html_view, options.remote_images);
    let scrolled_html = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&html_view)
        .build();
    let message_stack = gtk::Stack::new();
    message_stack.set_widget_name("notm-message-stack");
    message_stack.set_hexpand(true);
    message_stack.set_vexpand(true);
    message_stack.set_hhomogeneous(false);
    message_stack.set_vhomogeneous(false);
    message_stack.add_named(&scrolled_message, Some("text"));
    message_stack.add_named(&scrolled_html, Some("html"));
    message_stack.set_visible_child_name("text");
    right.append(&message_stack);

    let composer_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    composer_box.set_widget_name("notm-composer");
    composer_box.set_hexpand(true);
    composer_box.set_vexpand(true);
    let compose_from = entry_with_placeholder("From");
    let compose_to = entry_with_placeholder("To");
    let compose_cc = entry_with_placeholder("Cc");
    let compose_bcc = entry_with_placeholder("Bcc");
    let compose_subject = entry_with_placeholder("Subject");
    let compose_body_buffer = SourceBuffer::builder()
        .highlight_matching_brackets(true)
        .highlight_syntax(false)
        .build();
    let compose_body = SourceView::builder()
        .buffer(&compose_body_buffer)
        .highlight_current_line(false)
        .hexpand(true)
        .monospace(true)
        .vexpand(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .build();
    compose_body.set_widget_name("notm-compose-body");
    let compose_vim_context = attach_compose_vim_context(&compose_body);
    let scrolled_compose_body = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_width(false)
        .propagate_natural_height(false)
        .min_content_width(240)
        .min_content_height(COMPOSE_BODY_MIN_HEIGHT)
        .max_content_height(COMPOSE_BODY_NATURAL_HEIGHT)
        .child(&compose_body)
        .build();
    let address_suggestions_list = gtk::ListBox::new();
    address_suggestions_list.set_widget_name("notm-address-suggestions-list");
    address_suggestions_list.set_selection_mode(gtk::SelectionMode::Single);
    address_suggestions_list.add_css_class("boxed-list");
    address_suggestions_list.set_hexpand(true);
    address_suggestions_list.set_focusable(false);
    address_suggestions_list.set_visible(false);
    let active_address_entry = Rc::new(RefCell::new(None::<gtk::Entry>));
    let active_address_field = Rc::new(Cell::new(None::<RecipientField>));
    let address_completion = Rc::new(RefCell::new(None::<AddressCompletionSession>));
    let compose_attachments = gtk::Label::new(Some("No attachments"));
    compose_attachments.set_widget_name("notm-compose-attachments");
    compose_attachments.set_xalign(0.0);
    compose_attachments.set_wrap(true);
    compose_attachments.add_css_class("dim-label");
    let composer_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    composer_actions.set_hexpand(true);
    let composer_left_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let add_attachment_button = gtk::Button::with_label("Add attachment…");
    let save_draft_button = gtk::Button::with_label("Save draft");
    save_draft_button.set_widget_name("notm-save-draft-button");
    let clear_draft_button = gtk::Button::with_label("Discard draft");
    let delete_local_draft_button = gtk::Button::with_label("Delete local draft");
    delete_local_draft_button.set_widget_name("notm-delete-local-draft-button");
    delete_local_draft_button.add_css_class("destructive-action");
    delete_local_draft_button.set_visible(false);
    let send_button = gtk::Button::with_label("Send");
    send_button.set_widget_name("notm-send-button");
    for b in [
        &add_attachment_button,
        &save_draft_button,
        &clear_draft_button,
        &send_button,
    ] {
        composer_left_actions.append(b);
    }
    let composer_action_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    composer_action_spacer.set_hexpand(true);
    composer_actions.append(&composer_left_actions);
    composer_actions.append(&composer_action_spacer);
    composer_actions.append(&delete_local_draft_button);
    composer_box.append(&compose_from);
    composer_box.append(&compose_to);
    composer_box.append(&address_suggestions_list);
    composer_box.append(&compose_cc);
    composer_box.append(&compose_bcc);
    composer_box.append(&compose_subject);
    composer_box.append(&scrolled_compose_body);
    composer_box.append(&compose_attachments);
    let draft_list = gtk::ListBox::new();
    draft_list.set_widget_name("notm-draft-list");
    draft_list.set_selection_mode(gtk::SelectionMode::Single);
    composer_box.append(&composer_actions);
    message_stack.add_named(&composer_box, Some("compose"));

    let debug_view = gtk::TextView::new();
    debug_view.set_widget_name("notm-debug-panel");
    debug_view.set_editable(false);
    debug_view.set_monospace(true);
    debug_view.set_visible(options.show_debug_panel);
    debug_view.set_size_request(-1, 150);
    right.append(&debug_view);

    let content_paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    content_paned.set_wide_handle(true);
    content_paned.set_start_child(Some(&middle));
    content_paned.set_end_child(Some(&right));
    content_paned.set_position(560);
    content_paned.set_hexpand(true);
    content_paned.set_vexpand(true);

    let outer_paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    outer_paned.set_wide_handle(true);
    outer_paned.set_start_child(Some(&sidebar_scrolled));
    outer_paned.set_end_child(Some(&content_paned));
    outer_paned.set_resize_start_child(false);
    outer_paned.set_resize_end_child(true);
    outer_paned.set_shrink_start_child(false);
    outer_paned.set_position(SIDEBAR_MIN_WIDTH);
    outer_paned.set_hexpand(true);
    outer_paned.set_vexpand(true);
    root.append(&outer_paned);

    let status_label = gtk::Label::new(Some("Ready"));
    status_label.set_widget_name("notm-status-bar");
    status_label.set_xalign(0.0);
    status_label.set_margin_start(8);
    status_label.set_margin_end(8);
    status_label.set_margin_bottom(8);
    root.append(&status_label);
    window.set_child(Some(&root));
    connect_html_navigation_policy(&html_view, &status_label);

    let widgets = Widgets {
        window: window.clone(),
        left_pane: sidebar_scrolled.clone(),
        thread_pane: middle.clone(),
        message_pane: right.clone(),
        saved_box,
        saved_name_entry,
        saved_query_entry,
        save_search_button: save_search_button.clone(),
        custom_tag_entry,
        search_entry,
        search_button: search_button.clone(),
        search_generation,
        search_suggestions_list,
        search_completion,
        hidden_tag_searches,
        thread_list,
        thread_result_label,
        load_more_button,
        thread_scrolled: scrolled_threads,
        compose_button: compose_button.clone(),
        debug_button: debug_button.clone(),
        palette_button: palette_button.clone(),
        settings_button: settings_button.clone(),
        help_button: help_button.clone(),
        archive_button: archive_button.clone(),
        read_toggle_button: read_button.clone(),
        flag_toggle_button: flag_button.clone(),
        trash_button: trash_button.clone(),
        spam_button: spam_button.clone(),
        tag_command_entry: tag_command_entry.clone(),
        tag_command_apply_button: tag_command_apply_button.clone(),
        tag_menu_button: tag_menu_button.clone(),
        tag_menu_box: tag_menu_box.clone(),
        add_custom_tag_button: add_tag_button.clone(),
        remove_custom_tag_button: remove_tag_button.clone(),
        undo_tag_button: undo_button.clone(),
        undo_menu_box: undo_menu_box.clone(),
        undo_last_tag_button: undo_last_button.clone(),
        undo_list_tag_button: undo_list_button.clone(),
        message_stack,
        message_view,
        message_scrolled: scrolled_message.clone(),
        html_view,
        html_scrolled: scrolled_html.clone(),
        response_menu_button,
        reply_button: reply_button.clone(),
        reply_all_button: reply_all_button.clone(),
        forward_button: forward_button.clone(),
        forward_attachment_button: forward_attachment_button.clone(),
        response_menu_box: response_menu_box.clone(),
        message_menu_button,
        message_menu_box,
        view_menu_button,
        view_menu_box: view_menu_box.clone(),
        view_text_button,
        view_html_button,
        view_headers_button,
        view_raw_button,
        image_policy_button,
        html_policy_row,
        html_policy_label,
        message_header_label,
        collapse_quotes_button,
        copy_menu_button,
        copy_menu_box: copy_menu_box.clone(),
        copy_message_id_button,
        copy_thread_id_button,
        copy_from_email_button,
        copy_to_email_button,
        copy_cc_email_button,
        copy_subject_button,
        quote_collapse,
        attachment_title,
        attachment_scrolled: scrolled_attachments,
        attachment_list,
        attachment_items: Rc::new(RefCell::new(Vec::new())),
        tag_search_box,
        draft_path: options
            .draft_path
            .clone()
            .unwrap_or_else(default_draft_path),
        debug_view,
        status_label,
        compose_from,
        compose_to,
        compose_cc,
        compose_bcc,
        compose_subject,
        compose_body,
        compose_vim_context: compose_vim_context.clone(),
        compose_scrolled: scrolled_compose_body.clone(),
        compose_attachments,
        add_attachment_button: add_attachment_button.clone(),
        save_draft_button: save_draft_button.clone(),
        clear_draft_button: clear_draft_button.clone(),
        delete_local_draft_button: delete_local_draft_button.clone(),
        send_button: send_button.clone(),
        address_suggestions_list,
        active_address_entry,
        active_address_field,
        address_completion,
        draft_list,
        drafts_dir: options
            .drafts_dir
            .clone()
            .unwrap_or_else(default_drafts_dir),
    };
    update_active_pane_visuals(&widgets, &state);
    update_message_action_buttons(&options, &widgets, &state);
    set_undo_tag_available(&widgets, !undo_state.borrow().is_empty());
    if let Some(id) = identity(&options) {
        widgets.compose_from.set_text(&id.formatted());
    }

    let saved_search_store = Rc::new(RefCell::new(options.custom_saved_searches.clone()));
    refresh_saved_searches(&options, &widgets, &state, &saved_search_store);
    connect_saved_search_editor(
        &options,
        &widgets,
        &state,
        &saved_search_store,
        &save_search_button,
    );
    connect_custom_tag_editor(
        &options,
        &widgets,
        &state,
        &undo_state,
        &add_tag_button,
        &remove_tag_button,
    );
    connect_notmuch_tag_command_editor(&options, &widgets, &state, &undo_state);
    if let Some(sync_button) = manual_sync_button {
        let opts = options.clone();
        let w = widgets.clone();
        let st = state.clone();
        sync_button.connect_clicked(move |_| run_manual_sync(&opts, &w, &st));
    }

    connect_actions(
        &options,
        &widgets,
        &state,
        &undo_state,
        &search_button,
        &archive_button,
        &read_button,
        &flag_button,
        &trash_button,
        &spam_button,
        &undo_last_button,
        &undo_list_button,
        &compose_button,
        &reply_button,
        &reply_all_button,
        &forward_button,
        &forward_attachment_button,
        &debug_button,
        &palette_button,
        &settings_button,
        &help_button,
        &send_button,
    );
    connect_compose_helpers(
        &options,
        &widgets,
        &state,
        &add_attachment_button,
        &save_draft_button,
        &clear_draft_button,
        &delete_local_draft_button,
    );
    connect_compose_vim_context(&options, &widgets, &state, &compose_vim_context);
    connect_message_actions(&options, &widgets, &state);
    connect_recipient_autocomplete(&widgets.compose_to, &widgets, &state);
    connect_recipient_autocomplete(&widgets.compose_cc, &widgets, &state);
    connect_recipient_autocomplete(&widgets.compose_bcc, &widgets, &state);
    connect_address_suggestion_list(&widgets, &state);
    connect_search_debounce(&options, &widgets, &state);
    connect_search_autocomplete(&widgets, &state);
    connect_input_mode_focus(&widgets, &state);
    install_shortcuts(&options, &widgets, &state, &undo_state, &saved_search_store);
    connect_auto_load_more(&options, &widgets, &state);

    if options.automation_enabled {
        setup_automation(&options, &widgets, &state, &undo_state, &saved_search_store);
    }

    restore_draft_if_present(&widgets, &state);
    refresh_draft_list(&widgets);
    window.present();
    widgets
        .status_label
        .set_text("Starting notm; loading mail…");
    widgets
        .thread_result_label
        .set_text("Loading initial search…");
    {
        let opts = options.clone();
        let w = widgets.clone();
        let st = state.clone();
        let query = options.default_query.clone();
        gtk::glib::timeout_add_local_once(Duration::from_millis(0), move || {
            run_search_async(&opts, &w, &st, &query);
            refresh_address_suggestions_async(&opts, &w, &st);
        });
    }
    {
        let opts = options.clone();
        let w = widgets.clone();
        let st = state.clone();
        gtk::glib::timeout_add_local_once(Duration::from_millis(250), move || {
            run_startup_sync(&opts, &w, &st);
        });
    }
    update_debug(&widgets, &state);
}

fn built_in_saved_searches() -> Vec<SavedSearch> {
    vec![
        SavedSearch {
            name: "Inbox".to_string(),
            query: "tag:inbox and not tag:trash and not tag:spam".to_string(),
        },
        SavedSearch {
            name: "Unread".to_string(),
            query: "tag:unread and not tag:trash and not tag:spam".to_string(),
        },
        SavedSearch {
            name: "Flagged".to_string(),
            query: "tag:flagged".to_string(),
        },
        SavedSearch {
            name: "Sent".to_string(),
            query: "tag:sent".to_string(),
        },
        SavedSearch {
            name: "Drafts".to_string(),
            query: "tag:draft".to_string(),
        },
        SavedSearch {
            name: "Trash".to_string(),
            query: "tag:trash".to_string(),
        },
        SavedSearch {
            name: "All".to_string(),
            query: "*".to_string(),
        },
    ]
}

fn saved_search_binding(name: &str) -> Option<&'static str> {
    match name {
        "Inbox" => Some("g i"),
        "Unread" => Some("g u"),
        "Flagged" => Some("g f"),
        "Sent" => Some("g s"),
        "Drafts" => Some("g d"),
        "Trash" => Some("g t"),
        "All" => Some("g a"),
        _ => None,
    }
}

fn update_saved_search_button_labels(widgets: &Widgets, state: &SharedState) {
    let mut custom_index = 0_usize;
    update_saved_search_button_labels_in_widget(
        &widgets.saved_box.clone().upcast(),
        state,
        &mut custom_index,
    );
}

fn update_saved_search_button_labels_in_widget(
    widget: &gtk::Widget,
    state: &SharedState,
    custom_index: &mut usize,
) {
    if let Ok(button) = widget.clone().downcast::<gtk::Button>()
        && button.widget_name().starts_with("notm-saved-search-")
    {
        let name = button
            .tooltip_text()
            .map(|text| text.to_string())
            .unwrap_or_else(|| strip_binding_suffix(&button.label().unwrap_or_default()));
        set_button_label(
            &button,
            &name,
            saved_search_binding(&name).unwrap_or_default(),
            state,
        );
        if state.borrow().visible_saved_search.as_deref() == Some(name.as_str()) {
            button.add_css_class("suggested-action");
        } else {
            button.remove_css_class("suggested-action");
        }
    } else if let Ok(button) = widget.clone().downcast::<gtk::Button>()
        && button
            .widget_name()
            .starts_with("notm-custom-saved-search-")
    {
        *custom_index += 1;
        let name = button
            .tooltip_text()
            .map(|text| text.to_string())
            .unwrap_or_else(|| strip_binding_suffix(&button.label().unwrap_or_default()));
        let binding = if *custom_index <= 9 {
            format!("g c {}", *custom_index)
        } else {
            String::new()
        };
        set_button_label(&button, &name, &binding, state);
        if state.borrow().visible_saved_search.as_deref() == Some(name.as_str()) {
            button.add_css_class("suggested-action");
        } else {
            button.remove_css_class("suggested-action");
        }
    }
    let mut child = widget.first_child();
    while let Some(child_widget) = child {
        child = child_widget.next_sibling();
        update_saved_search_button_labels_in_widget(&child_widget, state, custom_index);
    }
}

fn refresh_saved_searches(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    saved_store: &SavedSearchStore,
) {
    while let Some(child) = widgets.saved_box.first_child() {
        widgets.saved_box.remove(&child);
    }
    let default_title = gtk::Label::new(Some("Default"));
    default_title.set_xalign(0.0);
    default_title.add_css_class("dim-label");
    widgets.saved_box.append(&default_title);
    for saved in built_in_saved_searches() {
        append_saved_search_button(
            options,
            widgets,
            state,
            saved_store,
            &widgets.saved_box,
            saved,
            false,
        );
    }
    let custom_searches = saved_store.borrow().clone();
    let custom_title = gtk::Label::new(Some("Custom"));
    custom_title.set_xalign(0.0);
    custom_title.add_css_class("dim-label");
    custom_title.set_margin_top(6);
    widgets.saved_box.append(&custom_title);
    if custom_searches.is_empty() {
        let label = gtk::Label::new(Some("No custom searches."));
        label.set_xalign(0.0);
        label.add_css_class("dim-label");
        widgets.saved_box.append(&label);
    } else {
        for saved in custom_searches {
            append_saved_search_button(
                options,
                widgets,
                state,
                saved_store,
                &widgets.saved_box,
                saved,
                true,
            );
        }
    }
    update_saved_search_button_labels(widgets, state);
    update_tag_searches(options, widgets, state);
}

fn append_saved_search_button(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    saved_store: &SavedSearchStore,
    container: &impl IsA<gtk::Box>,
    saved: SavedSearch,
    custom: bool,
) -> gtk::Button {
    let btn = gtk::Button::with_label(&saved.name);
    let prefix = if custom {
        "notm-custom-saved-search"
    } else {
        "notm-saved-search"
    };
    btn.set_widget_name(&format!("{prefix}-{}", widget_token(&saved.name)));
    btn.set_tooltip_text(Some(&saved.name));
    let saved_name = saved.name.clone();
    let st = state.clone();
    let w = widgets.clone();
    let opts = options.clone();
    btn.connect_clicked(move |_| {
        activate_saved_search(&opts, &w, &st, &saved.name, &saved.query);
    });
    if custom {
        connect_custom_saved_search_context_menu(
            options,
            widgets,
            state,
            saved_store,
            &btn,
            &saved_name,
        );
    }
    container.append(&btn);
    btn
}

fn activate_saved_search(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    name: &str,
    query: &str,
) {
    state.borrow_mut().visible_saved_search = Some(name.to_string());
    widgets.saved_name_entry.set_text(name);
    widgets.saved_query_entry.set_text(query);
    widgets.search_entry.set_text(query);
    run_search(options, widgets, state, query);
}

fn connect_custom_saved_search_context_menu(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    saved_store: &SavedSearchStore,
    button: &gtk::Button,
    name: &str,
) {
    let click = gtk::GestureClick::new();
    click.set_button(3);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let store = saved_store.clone();
    let name = name.to_string();
    let parent = button.clone();
    click.connect_pressed(move |_, _, x, y| {
        let popover = gtk::Popover::new();
        popover.set_has_arrow(true);
        popover.set_parent(&parent);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let delete = gtk::Button::with_label("Delete custom search");
        delete.add_css_class("destructive-action");
        menu.append(&delete);
        popover.set_child(Some(&menu));
        let opts = opts.clone();
        let w = w.clone();
        let st = st.clone();
        let store = store.clone();
        let name = name.clone();
        let popover_for_delete = popover.clone();
        delete.connect_clicked(move |_| {
            match delete_custom_search_by_name(&opts, &w, &st, &store, &name) {
                Ok(()) => w.status_label.set_text("Deleted custom search"),
                Err(err) => w
                    .status_label
                    .set_text(&format!("Delete search failed: {err}")),
            }
            popover_for_delete.popdown();
        });
        popover.popup();
    });
    button.add_controller(click);
}

fn update_tag_searches(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    while let Some(child) = widgets.tag_search_box.first_child() {
        widgets.tag_search_box.remove(&child);
    }
    let tags = state.borrow().visible_tags.clone();
    if tags.is_empty() {
        let label = gtk::Label::new(Some("Run a search to load tags."));
        label.set_xalign(0.0);
        label.add_css_class("dim-label");
        widgets.tag_search_box.append(&label);
        return;
    }

    let hidden = widgets.hidden_tag_searches.borrow();
    let mut direct = Vec::new();
    let mut grouped = BTreeMap::<String, Vec<String>>::new();
    for tag in tags
        .into_iter()
        .filter(|tag| !hidden.contains(tag))
        .filter(|tag| !is_duplicate_tag_search(options, tag))
    {
        if let Some((root, _)) = tag.split_once('/') {
            grouped.entry(root.to_string()).or_default().push(tag);
        } else {
            direct.push(tag);
        }
    }
    drop(hidden);

    if direct.is_empty() && grouped.is_empty() {
        let label = gtk::Label::new(Some("All tag searches hidden."));
        label.set_xalign(0.0);
        label.add_css_class("dim-label");
        widgets.tag_search_box.append(&label);
        return;
    }

    direct.sort_by_key(|tag| tag.to_lowercase());
    for tag in direct {
        append_tag_search_button(options, widgets, state, &tag, &tag);
    }

    for (root, mut tags) in grouped {
        tags.sort_by_key(|tag| tag.to_lowercase());
        let (button, menu) = menu_button_with_box(
            &root,
            &format!("notm-tag-group-{}", widget_token(&root)),
            state,
        );
        button.set_tooltip_text(Some(&root));
        for tag in tags {
            let label = tag
                .strip_prefix(&format!("{root}/"))
                .unwrap_or(&tag)
                .to_string();
            append_tag_search_button_to_box(options, widgets, state, &menu, &label, &tag);
        }
        widgets.tag_search_box.append(&button);
    }
    update_tag_search_button_labels(widgets, state);
}

fn append_tag_search_button(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    label: &str,
    tag: &str,
) {
    append_tag_search_button_to_box(options, widgets, state, &widgets.tag_search_box, label, tag);
}

fn append_tag_search_button_to_box(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    container: &gtk::Box,
    label: &str,
    tag: &str,
) {
    let button = gtk::Button::with_label(label);
    button.set_widget_name(&format!("notm-tag-search-{}", widget_token(tag)));
    button.set_tooltip_text(Some(tag));
    let tag = tag.to_string();
    let query = tag_query(&tag);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    connect_tag_search_context_menu(options, widgets, state, &button, &tag);
    button.connect_clicked(move |_| activate_saved_search(&opts, &w, &st, &tag, &query));
    container.append(&button);
}

fn connect_tag_search_context_menu(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    button: &gtk::Button,
    tag: &str,
) {
    let click = gtk::GestureClick::new();
    click.set_button(3);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let tag_name = tag.to_string();
    let parent = button.downgrade();
    click.connect_pressed(move |_, _, x, y| {
        let Some(parent) = parent.upgrade() else {
            return;
        };
        let popover = gtk::Popover::new();
        popover.set_has_arrow(true);
        popover.set_parent(&parent);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let hide = gtk::Button::with_label("Hide tag search");
        menu.append(&hide);
        popover.set_child(Some(&menu));
        let opts = opts.clone();
        let w = w.clone();
        let st = st.clone();
        let tag_name = tag_name.clone();
        let popover_for_hide = popover.clone();
        hide.connect_clicked(move |_| {
            popover_for_hide.popdown();
            popover_for_hide.unparent();
            {
                let mut hidden = w.hidden_tag_searches.borrow_mut();
                hidden.insert(tag_name.clone());
                match persist_hidden_tag_searches(&opts, &hidden) {
                    Ok(()) => {
                        if opts.app_config_path.is_some() {
                            w.status_label.set_text("Hidden tag search");
                        } else {
                            w.status_label
                                .set_text("Hidden tag search for this session only");
                        }
                    }
                    Err(err) => {
                        hidden.remove(&tag_name);
                        w.status_label.set_text(&format!("Hide tag failed: {err}"));
                        update_debug(&w, &st);
                        return;
                    }
                }
            }
            if st.borrow().visible_saved_search.as_deref() == Some(tag_name.as_str()) {
                st.borrow_mut().visible_saved_search = None;
            }
            update_tag_searches(&opts, &w, &st);
            update_debug(&w, &st);
        });
        popover.popup();
    });
    button.add_controller(click);
}

fn tag_query(tag: &str) -> String {
    format!("tag:\"{}\"", tag.replace('\\', "\\\\").replace('"', "\\\""))
}

fn tag_button_base_label(tag: &str) -> String {
    tag.rsplit_once('/')
        .map(|(_, leaf)| leaf.to_string())
        .unwrap_or_else(|| tag.to_string())
}

fn collect_visible_tag_button_targets(widgets: &Widgets) -> Vec<String> {
    let mut targets = Vec::new();
    let mut child = widgets.tag_search_box.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Ok(button) = widget.clone().downcast::<gtk::Button>()
            && let Some(tag) = button.tooltip_text()
        {
            targets.push(tag.to_string());
        } else if let Ok(menu_button) = widget.clone().downcast::<gtk::MenuButton>()
            && let Some(root) = menu_button.tooltip_text()
        {
            targets.push(root.to_string());
        }
    }
    targets
}

fn top_level_tag_menu_button_by_root(widgets: &Widgets, root: &str) -> Option<gtk::MenuButton> {
    let mut child = widgets.tag_search_box.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Ok(menu_button) = widget.downcast::<gtk::MenuButton>()
            && menu_button.tooltip_text().as_deref() == Some(root)
        {
            return Some(menu_button);
        }
    }
    None
}

fn update_tag_search_button_labels(widgets: &Widgets, state: &SharedState) {
    let targets = collect_visible_tag_button_targets(widgets);
    update_tag_search_button_labels_in_widget(
        &widgets.tag_search_box.clone().upcast(),
        &targets,
        state,
    );
}

fn update_tag_search_button_labels_in_widget(
    widget: &gtk::Widget,
    targets: &[String],
    state: &SharedState,
) {
    let active_tag = active_tag_search(state);
    if let Ok(button) = widget.clone().downcast::<gtk::Button>()
        && let Some(tag) = button.tooltip_text()
    {
        let tag = tag.to_string();
        let base = tag_button_base_label(&tag);
        let binding = targets
            .iter()
            .position(|target| target == &tag)
            .filter(|index| *index < 9)
            .map(|index| format!("g {}", index + 1))
            .unwrap_or_default();
        set_button_label(&button, &base, &binding, state);
        if active_tag.as_deref() == Some(tag.as_str()) {
            button.add_css_class("suggested-action");
        } else {
            button.remove_css_class("suggested-action");
        }
    }
    if let Ok(menu_button) = widget.clone().downcast::<gtk::MenuButton>()
        && let Some(popover) = menu_button.popover()
        && let Some(child) = popover.child()
    {
        if let Some(root) = menu_button.tooltip_text() {
            let root = root.to_string();
            let binding = targets
                .iter()
                .position(|target| target == &root)
                .filter(|index| *index < 9)
                .map(|index| format!("g {}", index + 1))
                .unwrap_or_default();
            set_menu_button_label(&menu_button, &root, &binding, state);
            let selected_in_group = active_tag
                .as_deref()
                .is_some_and(|tag| tag.starts_with(&format!("{root}/")));
            if selected_in_group {
                menu_button.add_css_class("suggested-action");
            } else {
                menu_button.remove_css_class("suggested-action");
            }
        }
        update_tag_search_button_labels_in_widget(&child, targets, state);
    }
    let mut child = widget.first_child();
    while let Some(child_widget) = child {
        child = child_widget.next_sibling();
        update_tag_search_button_labels_in_widget(&child_widget, targets, state);
    }
}

fn active_tag_search(state: &SharedState) -> Option<String> {
    let state = state.borrow();
    state
        .visible_saved_search
        .as_deref()
        .and_then(|selected| {
            state
                .visible_tags
                .iter()
                .find(|tag| *tag == selected)
                .cloned()
        })
        .or_else(|| parse_single_tag_query(&state.current_query))
}

fn open_visible_tag_by_key(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    key: gtk::gdk::Key,
) -> bool {
    let Some(digit) = key_to_digit(key) else {
        return false;
    };
    if !(1..=9).contains(&digit) {
        return false;
    }
    let targets = collect_visible_tag_button_targets(widgets);
    let Some(tag) = targets.get(digit as usize - 1).cloned() else {
        return false;
    };
    if let Some(menu_button) = top_level_tag_menu_button_by_root(widgets, &tag) {
        menu_button.popup();
        widgets
            .status_label
            .set_text("Tag group opened; use arrows/Enter or click a subtag");
    } else {
        activate_saved_search(options, widgets, state, &tag, &tag_query(&tag));
        set_active_pane(widgets, state, ActivePane::Threads);
    }
    true
}

fn open_custom_saved_search_by_key(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    saved_store: &SavedSearchStore,
    key: gtk::gdk::Key,
) -> bool {
    let Some(digit) = key_to_digit(key) else {
        return false;
    };
    if !(1..=9).contains(&digit) {
        return false;
    }
    let custom_searches = saved_store.borrow();
    let Some(saved) = custom_searches.get(digit as usize - 1) else {
        widgets
            .status_label
            .set_text("No custom search for that number");
        return true;
    };
    activate_saved_search(options, widgets, state, &saved.name, &saved.query);
    set_active_pane(widgets, state, ActivePane::Threads);
    true
}

fn custom_saved_search_prompt(saved_store: &SavedSearchStore) -> String {
    let searches = saved_store.borrow();
    if searches.is_empty() {
        return "No custom saved searches".to_string();
    }
    let bindings = searches
        .iter()
        .take(9)
        .enumerate()
        .map(|(index, saved)| format!("{} {}", index + 1, saved.name))
        .collect::<Vec<_>>()
        .join(", ");
    format!("Custom search: {bindings}")
}

fn is_duplicate_tag_search(options: &LaunchOptions, tag: &str) -> bool {
    if ["inbox", "unread", "flagged", "sent", "draft", "trash"].contains(&tag) {
        return true;
    }
    options
        .custom_saved_searches
        .iter()
        .filter_map(|saved| parse_single_tag_query(&saved.query))
        .any(|saved_tag| saved_tag == tag)
}

fn connect_saved_search_editor(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    saved_store: &SavedSearchStore,
    save_search_button: &gtk::Button,
) {
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let store = saved_store.clone();
    save_search_button.connect_clicked(move |_| {
        match save_custom_search_from_entries(&opts, &w, &st, &store) {
            Ok(()) => w.status_label.set_text("Saved custom search"),
            Err(err) => w
                .status_label
                .set_text(&format!("Save search failed: {err}")),
        }
    });

    for entry in [
        widgets.saved_name_entry.clone(),
        widgets.saved_query_entry.clone(),
    ] {
        let w = widgets.clone();
        let st = state.clone();
        let store = saved_store.clone();
        entry.connect_changed(move |_| {
            update_saved_search_editor_actions(&w, &st, &store);
            let w = w.clone();
            let st = st.clone();
            let store = store.clone();
            gtk::glib::idle_add_local_once(move || {
                update_saved_search_editor_actions(&w, &st, &store);
            });
        });
    }
    update_saved_search_editor_actions(widgets, state, saved_store);
}

fn update_saved_search_editor_actions(
    widgets: &Widgets,
    state: &SharedState,
    saved_store: &SavedSearchStore,
) {
    let name = widgets.saved_name_entry.text().trim().to_string();
    let query = widgets.saved_query_entry.text().trim().to_string();
    let has_values = !name.is_empty() && !query.is_empty();
    let built_in_name = built_in_saved_searches()
        .iter()
        .any(|saved| saved.name.eq_ignore_ascii_case(&name));
    let baseline = selected_saved_search_baseline(state, saved_store);
    let changed = baseline
        .as_ref()
        .is_none_or(|(base_name, base_query)| name != *base_name || query != *base_query);
    let save_visible = has_values && changed && !built_in_name;

    widgets.save_search_button.set_visible(save_visible);
}

fn selected_saved_search_baseline(
    state: &SharedState,
    saved_store: &SavedSearchStore,
) -> Option<(String, String)> {
    let selected = state.borrow().visible_saved_search.clone()?;
    if let Some(saved) = built_in_saved_searches()
        .into_iter()
        .find(|saved| saved.name == selected)
    {
        return Some((saved.name, saved.query));
    }
    if let Some(saved) = saved_store
        .borrow()
        .iter()
        .find(|saved| saved.name.eq_ignore_ascii_case(&selected))
        .cloned()
    {
        return Some((saved.name, saved.query));
    }
    let tags = state.borrow().visible_tags.clone();
    tags.into_iter()
        .find(|tag| tag == &selected || tag_query(tag) == selected)
        .map(|tag| (tag.clone(), tag_query(&tag)))
}

fn save_custom_search_from_entries(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    saved_store: &SavedSearchStore,
) -> anyhow::Result<()> {
    let name = widgets.saved_name_entry.text().trim().to_string();
    let query = widgets.saved_query_entry.text().trim().to_string();
    anyhow::ensure!(!name.is_empty(), "saved search name is empty");
    anyhow::ensure!(!query.is_empty(), "saved search query is empty");
    anyhow::ensure!(
        !built_in_saved_searches()
            .iter()
            .any(|saved| saved.name.eq_ignore_ascii_case(&name)),
        "built-in saved searches cannot be overwritten"
    );
    {
        let mut searches = saved_store.borrow_mut();
        if let Some(existing) = searches
            .iter_mut()
            .find(|saved| saved.name.eq_ignore_ascii_case(&name))
        {
            existing.name = name.clone();
            existing.query = query.clone();
        } else {
            searches.push(SavedSearch {
                name: name.clone(),
                query: query.clone(),
            });
        }
        searches.sort_by_key(|saved| saved.name.to_lowercase());
        persist_custom_saved_searches(options, &searches)?;
    }
    refresh_saved_searches(options, widgets, state, saved_store);
    widgets.search_entry.set_text(&query);
    state.borrow_mut().visible_saved_search = Some(name);
    update_saved_search_editor_actions(widgets, state, saved_store);
    run_search(options, widgets, state, &query);
    Ok(())
}

fn delete_custom_search_from_entries(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    saved_store: &SavedSearchStore,
) -> anyhow::Result<()> {
    let name = widgets.saved_name_entry.text().trim().to_string();
    anyhow::ensure!(!name.is_empty(), "saved search name is empty");
    {
        let mut searches = saved_store.borrow_mut();
        let before = searches.len();
        searches.retain(|saved| !saved.name.eq_ignore_ascii_case(&name));
        if searches.len() != before {
            persist_custom_saved_searches(options, &searches)?;
        } else {
            hide_tag_search_from_entries(options, widgets, state)?;
        }
    }
    refresh_saved_searches(options, widgets, state, saved_store);
    widgets.saved_name_entry.set_text("");
    widgets.saved_query_entry.set_text("");
    update_saved_search_editor_actions(widgets, state, saved_store);
    Ok(())
}

fn delete_custom_search_by_name(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    saved_store: &SavedSearchStore,
    name: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(!name.trim().is_empty(), "saved search name is empty");
    {
        let mut searches = saved_store.borrow_mut();
        let before = searches.len();
        searches.retain(|saved| !saved.name.eq_ignore_ascii_case(name));
        anyhow::ensure!(searches.len() != before, "custom search not found");
        persist_custom_saved_searches(options, &searches)?;
    }
    if state.borrow().visible_saved_search.as_deref() == Some(name) {
        state.borrow_mut().visible_saved_search = None;
    }
    if widgets
        .saved_name_entry
        .text()
        .trim()
        .eq_ignore_ascii_case(name)
    {
        widgets.saved_name_entry.set_text("");
        widgets.saved_query_entry.set_text("");
    }
    refresh_saved_searches(options, widgets, state, saved_store);
    update_saved_search_editor_actions(widgets, state, saved_store);
    Ok(())
}

fn hide_tag_search_from_entries(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> anyhow::Result<()> {
    let name = widgets.saved_name_entry.text().trim().to_string();
    let query = widgets.saved_query_entry.text().trim().to_string();
    let visible_tags = state.borrow().visible_tags.clone();
    let tag = visible_tags
        .iter()
        .find(|tag| tag.eq_ignore_ascii_case(&name))
        .cloned()
        .or_else(|| parse_single_tag_query(&query))
        .filter(|tag| visible_tags.iter().any(|visible| visible == tag))
        .ok_or_else(|| anyhow::anyhow!("custom search not found"))?;
    {
        let mut hidden = widgets.hidden_tag_searches.borrow_mut();
        hidden.insert(tag);
        persist_hidden_tag_searches(options, &hidden)?;
    }
    Ok(())
}

fn parse_single_tag_query(query: &str) -> Option<String> {
    let value = query.trim().strip_prefix("tag:")?.trim();
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        let inner = &value[1..value.len() - 1];
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                out.push(chars.next().unwrap_or(ch));
            } else {
                out.push(ch);
            }
        }
        Some(out)
    } else if !value.is_empty() && !value.contains(char::is_whitespace) {
        Some(value.to_string())
    } else {
        None
    }
}

fn persist_custom_saved_searches(
    options: &LaunchOptions,
    searches: &[SavedSearch],
) -> anyhow::Result<()> {
    let Some(path) = &options.app_config_path else {
        return Ok(());
    };
    let mut value = if path.exists() {
        std::fs::read_to_string(path)?
            .parse::<toml::Value>()
            .unwrap_or_else(|_| toml::Value::Table(Default::default()))
    } else {
        toml::Value::Table(Default::default())
    };
    if !value.is_table() {
        value = toml::Value::Table(Default::default());
    }
    let root = value.as_table_mut().expect("value is table");
    let ui = root
        .entry("ui".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    if !ui.is_table() {
        *ui = toml::Value::Table(Default::default());
    }
    let ui = ui.as_table_mut().expect("ui is table");
    ui.insert(
        "custom_saved_searches".to_string(),
        toml::Value::try_from(searches)?,
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&value)?)?;
    Ok(())
}

fn persist_hidden_tag_searches(
    options: &LaunchOptions,
    hidden_tags: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let Some(path) = &options.app_config_path else {
        return Ok(());
    };
    let mut value = if path.exists() {
        std::fs::read_to_string(path)?
            .parse::<toml::Value>()
            .unwrap_or_else(|_| toml::Value::Table(Default::default()))
    } else {
        toml::Value::Table(Default::default())
    };
    if !value.is_table() {
        value = toml::Value::Table(Default::default());
    }
    let root = value.as_table_mut().expect("value is table");
    let hidden = hidden_tags
        .iter()
        .cloned()
        .map(toml::Value::String)
        .collect::<Vec<_>>();
    table_entry(root, "ui").insert(
        "hidden_tag_searches".to_string(),
        toml::Value::Array(hidden),
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&value)?)?;
    Ok(())
}

fn persist_trusted_image_senders(
    options: &LaunchOptions,
    senders: &[String],
) -> anyhow::Result<()> {
    let Some(path) = &options.app_config_path else {
        return Ok(());
    };
    let mut value = if path.exists() {
        std::fs::read_to_string(path)?
            .parse::<toml::Value>()
            .unwrap_or_else(|_| toml::Value::Table(Default::default()))
    } else {
        toml::Value::Table(Default::default())
    };
    if !value.is_table() {
        value = toml::Value::Table(Default::default());
    }
    let root = value.as_table_mut().expect("value is table");
    let trusted = normalize_sender_list(senders)
        .into_iter()
        .map(toml::Value::String)
        .collect::<Vec<_>>();
    table_entry(root, "ui").insert(
        "trusted_image_senders".to_string(),
        toml::Value::Array(trusted),
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&value)?)?;
    Ok(())
}

fn connect_custom_tag_editor(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    add_tag_button: &gtk::Button,
    remove_tag_button: &gtk::Button,
) {
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    add_tag_button.connect_clicked(move |_| {
        if apply_custom_tag_from_entry(&opts, &w, &st, &undo, true) {
            prepare_custom_tag_entry_for_next(&w, &st);
        }
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    widgets.custom_tag_entry.connect_activate(move |_| {
        if apply_custom_tag_from_entry_auto(&opts, &w, &st, &undo) {
            prepare_custom_tag_entry_for_next(&w, &st);
        }
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    remove_tag_button.connect_clicked(move |_| {
        if apply_custom_tag_from_entry(&opts, &w, &st, &undo, false) {
            prepare_custom_tag_entry_for_next(&w, &st);
        }
    });

    let w = widgets.clone();
    let st = state.clone();
    widgets
        .custom_tag_entry
        .connect_changed(move |_| update_custom_tag_controls(&w, &st));

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w = widgets.clone();
    let st = state.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            close_custom_tag_editor(&w, &st);
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    widgets.custom_tag_entry.add_controller(controller);

    if let Some(popover) = widgets.tag_menu_button.popover() {
        let w = widgets.clone();
        let st = state.clone();
        popover.connect_closed(move |_| {
            if tag_editor_insert_mode_active(&w, &st) {
                enter_normal_mode(&w, &st);
            }
        });
    }

    update_custom_tag_controls(widgets, state);
}

fn apply_custom_tag_from_entry_auto(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) -> bool {
    let add = !custom_tag_can_remove(widgets, state);
    apply_custom_tag_from_entry(options, widgets, state, undo_state, add)
}

fn apply_custom_tag_from_entry(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    add: bool,
) -> bool {
    let tag = widgets.custom_tag_entry.text().trim().to_string();
    if tag.is_empty() {
        widgets.status_label.set_text("Tag name is empty");
        return false;
    }
    let mutation = if add {
        TagMutation {
            add: vec![tag],
            remove: Vec::new(),
            sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
        }
    } else {
        TagMutation {
            add: Vec::new(),
            remove: vec![tag],
            sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
        }
    };
    let applied = tag_selected(options, widgets, state, undo_state, mutation);
    update_custom_tag_controls(widgets, state);
    applied
}

fn apply_notmuch_tag_command_text(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    command: &str,
) -> bool {
    let command = command.trim();
    if command.is_empty() {
        widgets
            .status_label
            .set_text("Tag command is empty; use e.g. -inbox +books");
        return false;
    }
    match parse_notmuch_tag_command(command) {
        Ok((add, remove)) => {
            let applied = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add,
                    remove,
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            update_custom_tag_controls(widgets, state);
            applied
        }
        Err(err) => {
            widgets
                .status_label
                .set_text(&format!("Tag command failed: {err}"));
            false
        }
    }
}

fn parse_notmuch_tag_command(command: &str) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let mut tokens = command.split_whitespace().peekable();
    if tokens.peek().is_some_and(|token| *token == "notmuch") {
        tokens.next();
    }
    if tokens.peek().is_some_and(|token| *token == "tag") {
        tokens.next();
    }

    let mut add = BTreeSet::new();
    let mut remove = BTreeSet::new();
    for token in tokens {
        if token == "--" {
            anyhow::bail!("query terms are not supported here; selected messages are the target");
        }
        let Some(op) = token.chars().next() else {
            continue;
        };
        if op != '+' && op != '-' {
            anyhow::bail!("expected +tag or -tag token, got `{token}`");
        }
        let tag = unquote_tag_command_token(&token[1..]);
        if tag.is_empty() {
            anyhow::bail!("empty tag in `{token}`");
        }
        if op == '+' {
            add.insert(tag);
        } else {
            remove.insert(tag);
        }
    }
    if add.is_empty() && remove.is_empty() {
        anyhow::bail!("command needs at least one +tag or -tag");
    }
    Ok((add.into_iter().collect(), remove.into_iter().collect()))
}

fn unquote_tag_command_token(token: &str) -> String {
    let token = token.trim();
    if token.len() >= 2
        && ((token.starts_with('"') && token.ends_with('"'))
            || (token.starts_with('\'') && token.ends_with('\'')))
    {
        token[1..token.len() - 1].to_string()
    } else {
        token.to_string()
    }
}

fn connect_notmuch_tag_command_editor(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) {
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    widgets.tag_command_apply_button.connect_clicked(move |_| {
        if apply_notmuch_tag_command_text(&opts, &w, &st, &undo, &w.tag_command_entry.text()) {
            close_notmuch_tag_command_editor(&w, &st);
        }
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    widgets.tag_command_entry.connect_activate(move |entry| {
        if apply_notmuch_tag_command_text(&opts, &w, &st, &undo, &entry.text()) {
            close_notmuch_tag_command_editor(&w, &st);
        } else {
            entry.select_region(0, -1);
        }
    });

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w = widgets.clone();
    let st = state.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            close_notmuch_tag_command_editor(&w, &st);
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    widgets.tag_command_entry.add_controller(controller);
}

fn open_notmuch_tag_command_editor(widgets: &Widgets, state: &SharedState) {
    widgets.tag_menu_button.popup();
    set_input_mode(
        widgets,
        state,
        InputMode::Insert,
        "Insert mode: tag command (Esc for normal)",
    );
    widgets.tag_command_entry.grab_focus();
    widgets.tag_command_entry.select_region(0, -1);
}

fn close_notmuch_tag_command_editor(widgets: &Widgets, state: &SharedState) {
    if let Some(popover) = widgets.tag_menu_button.popover() {
        popover.popdown();
    }
    enter_normal_mode(widgets, state);
}

fn update_custom_tag_controls(widgets: &Widgets, state: &SharedState) {
    let has_tag = !widgets.custom_tag_entry.text().trim().is_empty();
    let can_remove = custom_tag_can_remove(widgets, state);
    widgets.add_custom_tag_button.set_visible(!can_remove);
    widgets.remove_custom_tag_button.set_visible(can_remove);
    widgets.add_custom_tag_button.set_sensitive(has_tag);
    widgets.remove_custom_tag_button.set_sensitive(has_tag);
}

fn custom_tag_can_remove(widgets: &Widgets, state: &SharedState) -> bool {
    let tag = widgets.custom_tag_entry.text().trim().to_string();
    !tag.is_empty()
        && tag_targets_any(state, |thread| {
            thread.tags.iter().any(|existing| existing == &tag)
        })
}

fn open_custom_tag_editor(widgets: &Widgets, state: &SharedState) {
    update_custom_tag_controls(widgets, state);
    widgets.tag_menu_button.popup();
    set_input_mode(
        widgets,
        state,
        InputMode::Insert,
        "Insert mode: tag (Esc for normal)",
    );
    widgets.custom_tag_entry.grab_focus();
    widgets.custom_tag_entry.select_region(0, -1);
}

fn prepare_custom_tag_entry_for_next(widgets: &Widgets, state: &SharedState) {
    update_custom_tag_controls(widgets, state);
    widgets.tag_menu_button.popup();
    set_input_mode(
        widgets,
        state,
        InputMode::Insert,
        "Tag applied; type another tag or Esc for normal",
    );
    widgets.custom_tag_entry.grab_focus();
    widgets.custom_tag_entry.select_region(0, -1);
}

fn tag_target_thread_ids(state: &SharedState) -> BTreeSet<String> {
    let state = state.borrow();
    if state.visual_select_mode && !state.visual_selected_threads.is_empty() {
        state.visual_selected_threads.clone()
    } else {
        state
            .selected_thread
            .iter()
            .map(|thread| thread.thread_id.clone())
            .collect()
    }
}

fn tag_target_threads(state: &SharedState) -> Vec<notm_notmuch::ThreadSummary> {
    let state = state.borrow();
    let target_ids = if state.visual_select_mode && !state.visual_selected_threads.is_empty() {
        state.visual_selected_threads.clone()
    } else {
        state
            .selected_thread
            .iter()
            .map(|thread| thread.thread_id.clone())
            .collect()
    };
    if target_ids.is_empty() {
        return Vec::new();
    }

    let mut seen = BTreeSet::new();
    let mut threads = Vec::new();
    for thread in &state.thread_list_items {
        if target_ids.contains(&thread.thread_id) && seen.insert(thread.thread_id.clone()) {
            threads.push(thread.clone());
        }
    }
    if let Some(thread) = &state.selected_thread
        && target_ids.contains(&thread.thread_id)
        && seen.insert(thread.thread_id.clone())
    {
        threads.push(thread.clone());
    }
    threads
}

fn tag_targets_any<F>(state: &SharedState, predicate: F) -> bool
where
    F: FnMut(&notm_notmuch::ThreadSummary) -> bool,
{
    tag_target_threads(state).iter().any(predicate)
}

fn tag_query_for_thread_ids(thread_ids: &BTreeSet<String>) -> String {
    thread_ids
        .iter()
        .map(|thread_id| format!("thread:{thread_id}"))
        .collect::<Vec<_>>()
        .join(" or ")
}

fn thread_ids_from_tag_query(query: &str) -> BTreeSet<String> {
    query
        .split_whitespace()
        .filter_map(|token| token.strip_prefix("thread:"))
        .map(|thread_id| thread_id.trim_matches(['(', ')']))
        .filter(|thread_id| !thread_id.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn tag_target_status_label(count: usize) -> String {
    match count {
        0 => "no threads".to_string(),
        1 => "1 thread".to_string(),
        count => format!("{} threads", format_count(count)),
    }
}

fn close_custom_tag_editor(widgets: &Widgets, state: &SharedState) {
    if let Some(popover) = widgets.tag_menu_button.popover() {
        popover.popdown();
    }
    if tag_editor_insert_mode_active(widgets, state) {
        enter_normal_mode(widgets, state);
    }
}

fn tag_editor_insert_mode_active(widgets: &Widgets, state: &SharedState) -> bool {
    state.borrow().input_mode == InputMode::Insert
        && (widgets.status_label.text().starts_with("Insert mode: tag")
            || widgets.status_label.text().starts_with("Tag applied;"))
}

fn widget_token(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn attach_compose_vim_context(compose_body: &SourceView) -> VimIMContext {
    let vim_context = VimIMContext::new();
    let key_controller = gtk::EventControllerKey::new();
    key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    key_controller.set_im_context(Some(&vim_context));
    compose_body.add_controller(key_controller);
    vim_context.set_client_widget(Some(compose_body));
    vim_context
}

fn connect_compose_vim_context(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    vim_context: &VimIMContext,
) {
    let status = widgets.status_label.clone();
    let compose_body = widgets.compose_body.clone();
    vim_context.connect_command_bar_text_notify(move |context| {
        update_compose_vim_status(&compose_body, &status, context);
    });

    let status = widgets.status_label.clone();
    let compose_body = widgets.compose_body.clone();
    vim_context.connect_command_text_notify(move |context| {
        update_compose_vim_status(&compose_body, &status, context);
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    vim_context.connect_write(move |_, _, path| match save_current_draft(&opts, &w, &st) {
        Ok(report) => {
            refresh_draft_list(&w);
            let destination = report
                .maildir_path
                .as_ref()
                .or(report.local_path.as_ref())
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "draft store".to_string());
            let suffix = path
                .map(|requested| format!("; ignored Vim file path {requested}"))
                .unwrap_or_default();
            w.status_label
                .set_text(&format!("Vim :w saved draft to {destination}{suffix}"));
        }
        Err(err) => w.status_label.set_text(&format!("Vim :w failed: {err}")),
    });
}

fn compose_vim_ready_for_app_escape(vim_context: &VimIMContext) -> bool {
    vim_context.command_bar_text().is_empty() && vim_context.command_text().is_empty()
}

fn update_compose_vim_status(
    compose_body: &SourceView,
    status_label: &gtk::Label,
    vim_context: &VimIMContext,
) {
    if !compose_body.has_focus() {
        return;
    }
    let command_bar = vim_context.command_bar_text();
    let command_text = vim_context.command_text();
    let text = if !command_bar.is_empty() {
        command_bar.to_string()
    } else if !command_text.is_empty() {
        format!("Vim {command_text}")
    } else {
        "Vim composer".to_string()
    };
    status_label.set_text(&text);
}

#[allow(clippy::too_many_arguments)]
fn connect_compose_helpers(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    add_attachment_button: &gtk::Button,
    save_draft_button: &gtk::Button,
    clear_draft_button: &gtk::Button,
    delete_local_draft_button: &gtk::Button,
) {
    for entry in [
        widgets.compose_from.clone(),
        widgets.compose_to.clone(),
        widgets.compose_cc.clone(),
        widgets.compose_bcc.clone(),
        widgets.compose_subject.clone(),
    ] {
        let w = widgets.clone();
        let st = state.clone();
        entry.connect_changed(move |_| autosave_draft_from_widgets(&w, &st));
    }
    let w = widgets.clone();
    let st = state.clone();
    widgets
        .compose_body
        .buffer()
        .connect_changed(move |_| autosave_draft_from_widgets(&w, &st));

    let w = widgets.clone();
    let st = state.clone();
    let opts = options.clone();
    save_draft_button.connect_clicked(move |_| {
        match save_current_draft(&opts, &w, &st) {
            Ok(report) => {
                let destination = report
                    .maildir_path
                    .as_ref()
                    .or(report.local_path.as_ref())
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "draft store".to_string());
                w.status_label
                    .set_text(&format!("Draft saved to {destination}"));
                if report.indexed_message_id.is_some() {
                    let current = st.borrow().current_query.clone();
                    run_search(&opts, &w, &st, &current);
                }
            }
            Err(err) => w
                .status_label
                .set_text(&format!("Draft save failed: {err}")),
        }
        refresh_draft_list(&w);
    });

    let w = widgets.clone();
    let st = state.clone();
    clear_draft_button.connect_clicked(move |_| {
        let active_draft = st.borrow().active_draft.clone();
        let has_unsaved_changes = active_draft
            .as_ref()
            .is_some_and(|draft| compose_fields(&w, &st) != draft.saved_fields);
        clear_draft_widgets(&w, &st);
        match clear_draft_file(&w.draft_path) {
            Ok(()) => {
                let status = match (active_draft.is_some(), has_unsaved_changes) {
                    (true, true) => "Draft changes discarded",
                    (true, false) => "Draft closed",
                    (false, _) => "Draft discarded",
                };
                w.status_label.set_text(status);
            }
            Err(err) => w
                .status_label
                .set_text(&format!("Draft clear failed: {err}")),
        }
    });

    let w = widgets.clone();
    let st = state.clone();
    let opts = options.clone();
    delete_local_draft_button.connect_clicked(move |_| {
        delete_active_draft_from_ui(&opts, &w, &st);
    });

    let w = widgets.clone();
    let st = state.clone();
    add_attachment_button.connect_clicked(move |_| {
        let dialog = gtk::FileChooserNative::new(
            Some("Add attachment"),
            Some(&w.window),
            gtk::FileChooserAction::Open,
            Some("Attach"),
            Some("Cancel"),
        );
        let w2 = w.clone();
        let st2 = st.clone();
        dialog.connect_response(move |dialog, response| {
            if response == gtk::ResponseType::Accept
                && let Some(file) = dialog.file()
                && let Some(path) = file.path()
            {
                add_attachment_path(&w2, &st2, path);
            }
            dialog.destroy();
        });
        dialog.show();
    });
}

fn connect_message_actions(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets.view_text_button.connect_clicked(move |_| {
        let scroll = current_message_scroll_fraction(&w);
        st.borrow_mut().prefer_html_view = false;
        show_rendered_selected_thread(&opts, &w, &st);
        restore_message_scroll_fraction(&w, scroll);
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets.view_html_button.connect_clicked(move |_| {
        let scroll = current_message_scroll_fraction(&w);
        st.borrow_mut().prefer_html_view = true;
        show_visual_html_selected_message(&opts, &w, &st);
        restore_message_scroll_fraction(&w, scroll);
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets
        .view_headers_button
        .connect_clicked(move |_| show_full_headers(&opts, &w, &st));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets
        .view_raw_button
        .connect_clicked(move |_| show_raw_source(&opts, &w, &st));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets
        .image_policy_button
        .connect_clicked(move |_| activate_image_policy_button(&opts, &w, &st));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets
        .collapse_quotes_button
        .connect_clicked(move |_| toggle_quote_collapse(&opts, &w, &st));

    let w = widgets.clone();
    let st = state.clone();
    widgets
        .copy_message_id_button
        .connect_clicked(move |_| copy_selected_message_id(&w, &st));

    let w = widgets.clone();
    let st = state.clone();
    widgets
        .copy_thread_id_button
        .connect_clicked(move |_| copy_selected_thread_id(&w, &st));

    let w = widgets.clone();
    let st = state.clone();
    widgets
        .copy_from_email_button
        .connect_clicked(move |_| copy_selected_message_emails(&w, &st, MessageEmailField::From));

    let w = widgets.clone();
    let st = state.clone();
    widgets
        .copy_to_email_button
        .connect_clicked(move |_| copy_selected_message_emails(&w, &st, MessageEmailField::To));

    let w = widgets.clone();
    let st = state.clone();
    widgets
        .copy_cc_email_button
        .connect_clicked(move |_| copy_selected_message_emails(&w, &st, MessageEmailField::Cc));

    let w = widgets.clone();
    let st = state.clone();
    widgets
        .copy_subject_button
        .connect_clicked(move |_| copy_selected_message_subject(&w, &st));
}

fn connect_recipient_autocomplete(entry: &gtk::Entry, widgets: &Widgets, state: &SharedState) {
    let w = widgets.clone();
    let st = state.clone();
    let entry_for_change = entry.clone();
    entry.connect_changed(move |entry| {
        let text = entry.text().to_string();
        let field = recipient_field_for_entry(&w, &entry_for_change);
        {
            let mut completion = w.address_completion.borrow_mut();
            if let Some(session) = completion.as_mut()
                && Some(session.field) == field
                && session.suppress_next_change
            {
                if session.generated_text.as_deref() == Some(text.as_str()) {
                    session.suppress_next_change = false;
                    autosave_draft_from_widgets(&w, &st);
                    return;
                }
                if text.is_empty() && session.generated_text.is_some() {
                    return;
                }
            }
        }
        if address_completion_current_matches(&w, field, &text) {
            autosave_draft_from_widgets(&w, &st);
            return;
        }
        reset_address_completion(&w);
        set_active_address_entry(&w, &entry_for_change);
        if field.is_some() && field == w.active_address_field.get() {
            update_address_suggestions_for_entry(&w, &st, &entry_for_change, &text);
        } else {
            hide_address_suggestions(&w);
        }
        autosave_draft_from_widgets(&w, &st);
    });
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let entry_clone = entry.clone();
    let w = widgets.clone();
    let st = state.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        set_active_address_entry(&w, &entry_clone);
        w.active_address_field
            .set(recipient_field_for_entry(&w, &entry_clone));
        if key == gtk::gdk::Key::Tab && complete_recipient_entry(&w, &st, &entry_clone) {
            autosave_draft_from_widgets(&w, &st);
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Escape {
            reset_address_completion(&w);
            hide_address_suggestions(&w);
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    entry.add_controller(controller);

    let w = widgets.clone();
    let focus = gtk::EventControllerFocus::new();
    let entry_for_enter = entry.clone();
    focus.connect_enter(move |_| {
        set_active_address_entry(&w, &entry_for_enter);
        w.active_address_field
            .set(recipient_field_for_entry(&w, &entry_for_enter));
        place_address_suggestions_after_entry(&w, &entry_for_enter);
        hide_address_suggestions(&w);
    });
    let w = widgets.clone();
    let entry_for_leave = entry.clone();
    focus.connect_leave(move |_| {
        let w = w.clone();
        let field = recipient_field_for_entry(&w, &entry_for_leave);
        gtk::glib::timeout_add_local_once(Duration::from_millis(150), move || {
            if w.active_address_field.get() == field {
                w.active_address_field.set(None);
                hide_address_suggestions(&w);
            }
        });
    });
    entry.add_controller(focus);
}

fn connect_address_suggestion_list(widgets: &Widgets, state: &SharedState) {
    let w = widgets.clone();
    let st = state.clone();
    widgets
        .address_suggestions_list
        .connect_row_activated(move |_, row| {
            let Some(child) = row.child() else {
                return;
            };
            let Ok(label) = child.downcast::<gtk::Label>() else {
                return;
            };
            let entry = active_address_entry(&w);
            apply_recipient_suggestion(&entry, &label.text());
            hide_address_suggestions(&w);
            autosave_draft_from_widgets(&w, &st);
        });
}

fn connect_search_debounce(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let (tx, rx) = mpsc::channel::<SearchResponse>();
    let opts = options.clone();
    let w = widgets.clone();
    widgets.search_entry.connect_changed(move |entry| {
        let query = entry.text().to_string();
        if query.trim().is_empty() {
            return;
        }
        let generation = w.search_generation.get().saturating_add(1);
        w.search_generation.set(generation);
        let tx = tx.clone();
        let opts = opts.clone();
        gtk::glib::timeout_add_local_once(Duration::from_millis(350), move || {
            thread::spawn(move || {
                let result = execute_search(&opts, &query);
                let _ = tx.send(SearchResponse { generation, result });
            });
        });
    });

    let w = widgets.clone();
    let st = state.clone();
    let poll_opts = options.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        while let Ok(response) = rx.try_recv() {
            if response.generation == w.search_generation.get() {
                match response.result {
                    Ok(data) => apply_search_data(&poll_opts, &w, &st, data),
                    Err(err) => apply_search_error(&w, &st, err),
                }
            } else {
                st.borrow_mut().last_operation = Some(format!(
                    "discarded stale search generation {}",
                    response.generation
                ));
            }
        }
        gtk::glib::ControlFlow::Continue
    });
}

fn connect_search_autocomplete(widgets: &Widgets, state: &SharedState) {
    let completion_active = Rc::new(Cell::new(false));
    let focus_generation = Rc::new(Cell::new(0_u64));
    let w = widgets.clone();
    let st = state.clone();
    let active = completion_active.clone();
    widgets.search_entry.connect_changed(move |entry| {
        let text = entry.text().to_string();
        {
            let mut session_ref = w.search_completion.borrow_mut();
            if let Some(session) = session_ref.as_mut()
                && session.suppress_next_change
            {
                if session.generated_text.as_deref() == Some(text.as_str()) {
                    session.suppress_next_change = false;
                    return;
                }
                if text.is_empty() && session.generated_text.is_some() {
                    return;
                }
            }
        }
        if search_completion_current_matches(&w, &text) {
            return;
        }
        reset_search_completion(&w);
        if active.get() {
            update_search_suggestions(&w, &st, &text, entry.position());
        } else {
            hide_search_suggestions(&w);
        }
    });

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let entry = widgets.search_entry.clone();
    let w = widgets.clone();
    let st = state.clone();
    let active = completion_active.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        active.set(true);
        if key == gtk::gdk::Key::Tab && apply_next_search_completion(&entry, &w, &st) {
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Escape {
            reset_search_completion(&w);
            hide_search_suggestions(&w);
        }
        gtk::glib::Propagation::Proceed
    });
    widgets.search_entry.add_controller(controller);

    let w = widgets.clone();
    let focus = gtk::EventControllerFocus::new();
    let st = state.clone();
    let active = completion_active.clone();
    let generation = focus_generation.clone();
    focus.connect_enter(move |_| {
        active.set(true);
        generation.set(generation.get().saturating_add(1));
        update_search_suggestions(&w, &st, &w.search_entry.text(), w.search_entry.position());
    });
    let w = widgets.clone();
    let active = completion_active.clone();
    let generation = focus_generation.clone();
    focus.connect_leave(move |_| {
        let leave_generation = generation.get().saturating_add(1);
        generation.set(leave_generation);
        let w = w.clone();
        let active = active.clone();
        let generation = generation.clone();
        gtk::glib::timeout_add_local_once(Duration::from_millis(150), move || {
            if generation.get() == leave_generation {
                active.set(false);
                hide_search_suggestions(&w);
            }
        });
    });
    widgets.search_entry.add_controller(focus);

    let w = widgets.clone();
    widgets
        .search_suggestions_list
        .connect_row_activated(move |_, row| {
            let Some(child) = row.child() else {
                return;
            };
            let Ok(label) = child.downcast::<gtk::Label>() else {
                return;
            };
            apply_search_completion(&w.search_entry, &label.text());
            reset_search_completion(&w);
            hide_search_suggestions(&w);
        });
}

fn update_search_suggestions(
    widgets: &Widgets,
    state: &SharedState,
    input: &str,
    cursor_position: i32,
) {
    let suggestions = matching_search_suggestions(input, cursor_position, state, 8);
    if suggestions.is_empty() {
        hide_search_suggestions(widgets);
    } else {
        *widgets.search_completion.borrow_mut() = Some(SearchCompletionSession {
            base: input.to_string(),
            cursor_position,
            suggestions: suggestions.clone(),
            next_index: 0,
            generated_text: None,
            suppress_next_change: false,
        });
        populate_search_suggestions_list(widgets, &suggestions);
        let width = widgets.search_entry.width().max(360);
        widgets.search_suggestions_list.set_size_request(width, -1);
        widgets.search_suggestions_list.set_visible(true);
    }
}

fn hide_search_suggestions(widgets: &Widgets) {
    populate_search_suggestions_list(widgets, &[]);
    widgets.search_suggestions_list.set_visible(false);
}

fn reset_search_completion(widgets: &Widgets) {
    *widgets.search_completion.borrow_mut() = None;
}

fn populate_search_suggestions_list(widgets: &Widgets, suggestions: &[String]) {
    while let Some(child) = widgets.search_suggestions_list.first_child() {
        widgets.search_suggestions_list.remove(&child);
    }
    for suggestion in suggestions {
        let row = gtk::ListBoxRow::new();
        row.set_widget_name(&format!(
            "notm-search-suggestion-{}",
            widget_token(suggestion)
        ));
        row.set_focusable(false);
        let label = gtk::Label::new(Some(suggestion));
        label.set_xalign(0.0);
        label.set_margin_start(6);
        label.set_margin_end(6);
        label.set_margin_top(3);
        label.set_margin_bottom(3);
        row.set_child(Some(&label));
        widgets.search_suggestions_list.append(&row);
    }
}

fn apply_search_completion(entry: &gtk::Entry, replacement: &str) {
    let current = entry.text().to_string();
    let (next, next_cursor) = search_completion_text(&current, entry.position(), replacement);
    entry.set_text(&next);
    entry.set_position(next_cursor);
}

fn apply_next_search_completion(
    entry: &gtk::Entry,
    widgets: &Widgets,
    state: &SharedState,
) -> bool {
    let current = entry.text().to_string();
    let reuse_session = widgets
        .search_completion
        .borrow()
        .as_ref()
        .is_some_and(|session| search_session_matches_current(session, &current));
    if !reuse_session {
        let suggestions = matching_search_suggestions(&current, entry.position(), state, 20);
        if suggestions.is_empty() {
            hide_search_suggestions(widgets);
            return false;
        }
        *widgets.search_completion.borrow_mut() = Some(SearchCompletionSession {
            base: current.clone(),
            cursor_position: entry.position(),
            suggestions,
            next_index: 0,
            generated_text: None,
            suppress_next_change: false,
        });
    }

    let (next, next_cursor, index, suggestions) = {
        let mut session_ref = widgets.search_completion.borrow_mut();
        let Some(session) = session_ref.as_mut() else {
            return false;
        };
        if session.suggestions.is_empty() {
            *session_ref = None;
            return false;
        }
        if let Some(current_index) = search_generated_index(session, &current) {
            session.next_index = current_index.saturating_add(1);
        }
        let index = session.next_index % session.suggestions.len();
        let (next, next_cursor) = search_completion_text(
            &session.base,
            session.cursor_position,
            &session.suggestions[index],
        );
        session.generated_text = Some(next.clone());
        session.suppress_next_change = true;
        session.next_index = index + 1;
        (next, next_cursor, index, session.suggestions.clone())
    };

    entry.set_text(&next);
    entry.set_position(next_cursor);
    populate_search_suggestions_list(widgets, &suggestions);
    widgets.search_suggestions_list.set_visible(true);
    if let Some(row) = widgets.search_suggestions_list.row_at_index(index as i32) {
        widgets.search_suggestions_list.select_row(Some(&row));
    }
    true
}

fn search_completion_current_matches(widgets: &Widgets, text: &str) -> bool {
    widgets
        .search_completion
        .borrow()
        .as_ref()
        .is_some_and(|session| search_session_matches_current(session, text))
}

fn search_session_matches_current(session: &SearchCompletionSession, current: &str) -> bool {
    session.base == current
        || session.generated_text.as_deref() == Some(current)
        || search_generated_index(session, current).is_some()
}

fn search_generated_index(session: &SearchCompletionSession, current: &str) -> Option<usize> {
    session.suggestions.iter().position(|suggestion| {
        search_completion_text(&session.base, session.cursor_position, suggestion).0 == current
    })
}

fn search_completion_text(current: &str, cursor_position: i32, replacement: &str) -> (String, i32) {
    let cursor = char_index_to_byte(current, cursor_position.max(0) as usize);
    let (start, end) = search_token_bounds(current, cursor);
    let replacement = if replacement.ends_with(' ') || replacement.ends_with(':') {
        replacement.to_string()
    } else {
        format!("{replacement} ")
    };
    let next = format!("{}{}{}", &current[..start], replacement, &current[end..]);
    let next_cursor = start + replacement.len();
    (next.clone(), byte_index_to_char(&next, next_cursor))
}

fn matching_search_suggestions(
    input: &str,
    cursor_position: i32,
    state: &SharedState,
    limit: usize,
) -> Vec<String> {
    let cursor = char_index_to_byte(input, cursor_position.max(0) as usize);
    let (start, end) = search_token_bounds(input, cursor);
    let token = input[start..end].trim();
    if token.is_empty() {
        return Vec::new();
    }
    let token_lower = token.to_lowercase();
    let mut candidates = Vec::new();
    if let Some(tag_prefix) = token_lower.strip_prefix("tag:") {
        let raw_prefix = tag_prefix.trim_matches('"').trim_matches('\'');
        let tags = state.borrow().visible_tags.clone();
        for tag in tags {
            let tag_lower = tag.to_lowercase();
            if raw_prefix.is_empty()
                || tag_lower.starts_with(raw_prefix)
                || tag_lower.contains(raw_prefix)
            {
                candidates.push(format!("tag:{}", quote_notmuch_value(&tag)));
            }
        }
    } else {
        candidates.extend(
            [
                "tag:",
                "from:",
                "to:",
                "cc:",
                "subject:",
                "thread:",
                "id:",
                "date:",
                "folder:",
                "path:",
                "property:",
                "and",
                "or",
                "not",
                "*",
            ]
            .into_iter()
            .filter(|candidate| candidate.starts_with(&token_lower))
            .map(str::to_string),
        );
        for tag in state.borrow().visible_tags.iter() {
            if tag.to_lowercase().starts_with(&token_lower) {
                candidates.push(format!("tag:{}", quote_notmuch_value(tag)));
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates.truncate(limit);
    candidates
}

fn search_token_bounds(input: &str, cursor: usize) -> (usize, usize) {
    let cursor = cursor.min(input.len());
    let start = input[..cursor]
        .char_indices()
        .rev()
        .find(|(_, ch)| search_token_separator(*ch))
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    let end = input[cursor..]
        .char_indices()
        .find(|(_, ch)| search_token_separator(*ch))
        .map(|(index, _)| cursor + index)
        .unwrap_or(input.len());
    (start, end)
}

fn search_token_separator(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '(' | ')')
}

fn char_index_to_byte(input: &str, char_index: usize) -> usize {
    input
        .char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(input.len())
}

fn byte_index_to_char(input: &str, byte_index: usize) -> i32 {
    input[..byte_index.min(input.len())].chars().count() as i32
}

fn quote_notmuch_value(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '@'))
    {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn set_input_mode(widgets: &Widgets, state: &SharedState, mode: InputMode, status: &str) {
    state.borrow_mut().input_mode = mode;
    update_button_binding_labels(widgets, state);
    update_active_pane_visuals(widgets, state);
    widgets.status_label.set_text(status);
}

fn enter_normal_mode(widgets: &Widgets, state: &SharedState) {
    let keep_composer_focus = compose_view_is_visible(widgets) && composer_has_focus(widgets);
    set_input_mode(widgets, state, InputMode::Normal, "Normal mode");
    if !keep_composer_focus {
        focus_active_pane(widgets, state);
    }
}

fn enter_insert_mode_for_search(widgets: &Widgets, state: &SharedState) {
    state.borrow_mut().active_pane = ActivePane::Threads;
    set_input_mode(
        widgets,
        state,
        InputMode::Insert,
        "Insert mode: search (Esc for normal)",
    );
    widgets.search_entry.grab_focus();
}

fn enter_insert_mode_for_active_pane(widgets: &Widgets, state: &SharedState) {
    set_input_mode(
        widgets,
        state,
        InputMode::Insert,
        "Insert mode (Esc for normal)",
    );
    let active_pane = state.borrow().active_pane;
    match active_pane {
        ActivePane::Sidebar => focus_sidebar_insert_target(widgets),
        ActivePane::Threads => {
            widgets.search_entry.grab_focus();
        }
        ActivePane::Message if compose_view_is_visible(widgets) => {
            focus_composer_insert_target(widgets)
        }
        ActivePane::Message => {
            widgets.message_view.grab_focus();
        }
    };
}

fn focus_active_pane(widgets: &Widgets, state: &SharedState) {
    update_active_pane_visuals(widgets, state);
    match state.borrow().active_pane {
        ActivePane::Sidebar => {
            focus_sidebar_default(widgets);
        }
        ActivePane::Threads => {
            widgets.thread_list.grab_focus();
        }
        ActivePane::Message => {
            if compose_view_is_visible(widgets) {
                focus_first_composer_field(widgets);
            } else if html_view_is_visible(widgets) {
                widgets.html_view.grab_focus();
            } else {
                widgets.message_view.grab_focus();
            }
        }
    }
}

fn set_active_pane(widgets: &Widgets, state: &SharedState, pane: ActivePane) {
    state.borrow_mut().active_pane = pane;
    if state.borrow().input_mode == InputMode::Normal {
        focus_active_pane(widgets, state);
    }
    let name = match pane {
        ActivePane::Sidebar => "sidebar",
        ActivePane::Threads => "thread list",
        ActivePane::Message if compose_view_is_visible(widgets) => "composer",
        ActivePane::Message => "message view",
    };
    widgets
        .status_label
        .set_text(&format!("Active pane: {name}"));
    update_active_pane_visuals(widgets, state);
    update_debug(widgets, state);
}

fn move_active_pane(widgets: &Widgets, state: &SharedState, delta: isize) {
    let current = match state.borrow().active_pane {
        ActivePane::Sidebar => 0_i32,
        ActivePane::Threads => 1,
        ActivePane::Message => 2,
    };
    let next = (current + delta as i32).clamp(0, 2);
    let pane = match next {
        0 => ActivePane::Sidebar,
        1 => ActivePane::Threads,
        _ => ActivePane::Message,
    };
    set_active_pane(widgets, state, pane);
}

fn update_active_pane_visuals(widgets: &Widgets, state: &SharedState) {
    let active = state.borrow().active_pane;
    set_active_pane_class(&widgets.left_pane, active == ActivePane::Sidebar);
    set_active_pane_class(&widgets.thread_pane, active == ActivePane::Threads);
    set_active_pane_class(&widgets.message_pane, active == ActivePane::Message);
}

fn set_active_pane_class<W>(widget: &W, active: bool)
where
    W: IsA<gtk::Widget>,
{
    if active {
        widget.add_css_class("notm-active-pane");
    } else {
        widget.remove_css_class("notm-active-pane");
    }
}

fn composer_has_focus(widgets: &Widgets) -> bool {
    composer_focus_targets(widgets)
        .iter()
        .any(widget_contains_focus)
}

fn focus_first_composer_field(widgets: &Widgets) {
    let targets = composer_focus_targets(widgets);
    focus_widget_at(&targets, 0);
}

fn focus_composer_insert_target(widgets: &Widgets) {
    if composer_has_focus(widgets) {
        return;
    }
    focus_first_composer_field(widgets);
}

fn focus_sidebar_insert_target(widgets: &Widgets) {
    if widget_contains_focus(widgets.saved_name_entry.upcast_ref())
        || widget_contains_focus(widgets.saved_query_entry.upcast_ref())
    {
        return;
    }
    widgets.saved_query_entry.grab_focus();
}

fn focus_sidebar_default(widgets: &Widgets) {
    let mut targets = Vec::new();
    collect_sidebar_focus_targets(&widgets.left_pane.clone().upcast(), &mut targets);
    if let Some(index) = targets.iter().position(widget_contains_focus) {
        mark_keyboard_cursor(&targets, index);
    } else {
        focus_widget_at(&targets, 0);
    }
}

fn move_sidebar_focus(widgets: &Widgets, delta: isize) {
    let mut targets = Vec::new();
    collect_sidebar_focus_targets(&widgets.left_pane.clone().upcast(), &mut targets);
    focus_relative_widget(&targets, delta);
}

fn activate_focused_sidebar_widget(widgets: &Widgets, state: &SharedState) {
    let mut targets = Vec::new();
    collect_sidebar_focus_targets(&widgets.left_pane.clone().upcast(), &mut targets);
    let Some(focused) = targets.into_iter().find(widget_contains_focus) else {
        move_sidebar_focus(widgets, 1);
        return;
    };
    if let Ok(button) = focused.clone().downcast::<gtk::Button>() {
        button.emit_clicked();
    } else if let Ok(menu_button) = focused.clone().downcast::<gtk::MenuButton>() {
        menu_button.popup();
    } else if focused.downcast::<gtk::Entry>().is_ok() {
        enter_insert_mode_for_active_pane(widgets, state);
    }
}

fn collect_sidebar_focus_targets(widget: &gtk::Widget, targets: &mut Vec<gtk::Widget>) {
    if !widget.is_visible() || !widget.is_sensitive() {
        return;
    }
    if widget.clone().downcast::<gtk::Button>().is_ok()
        || widget.clone().downcast::<gtk::MenuButton>().is_ok()
        || widget.clone().downcast::<gtk::Entry>().is_ok()
    {
        targets.push(widget.clone());
    }
    if let Ok(menu_button) = widget.clone().downcast::<gtk::MenuButton>()
        && let Some(popover) = menu_button.popover()
        && let Some(child) = popover.child()
    {
        collect_sidebar_focus_targets(&child, targets);
    }
    let mut child = widget.first_child();
    while let Some(child_widget) = child {
        child = child_widget.next_sibling();
        collect_sidebar_focus_targets(&child_widget, targets);
    }
}

fn move_composer_focus(widgets: &Widgets, delta: isize) {
    let targets = composer_focus_targets(widgets);
    focus_relative_widget(&targets, delta);
}

fn composer_focus_targets(widgets: &Widgets) -> Vec<gtk::Widget> {
    [
        widgets.compose_from.clone().upcast::<gtk::Widget>(),
        widgets.compose_to.clone().upcast::<gtk::Widget>(),
        widgets.compose_cc.clone().upcast::<gtk::Widget>(),
        widgets.compose_bcc.clone().upcast::<gtk::Widget>(),
        widgets.compose_subject.clone().upcast::<gtk::Widget>(),
        widgets.compose_body.clone().upcast::<gtk::Widget>(),
    ]
    .into_iter()
    .filter(|widget| widget.is_visible() && widget.is_sensitive())
    .collect()
}

fn focus_relative_widget(targets: &[gtk::Widget], delta: isize) {
    if targets.is_empty() {
        return;
    }
    let current = targets
        .iter()
        .position(widget_contains_focus)
        .unwrap_or_else(|| {
            if delta.is_negative() {
                targets.len()
            } else {
                usize::MAX
            }
        });
    let next = if current == usize::MAX {
        0
    } else {
        current
            .saturating_add_signed(delta)
            .min(targets.len().saturating_sub(1))
    };
    focus_widget_at(targets, next);
}

fn focus_widget_at(targets: &[gtk::Widget], index: usize) {
    if targets.is_empty() {
        return;
    }
    let index = index.min(targets.len().saturating_sub(1));
    mark_keyboard_cursor(targets, index);
    targets[index].grab_focus();
}

fn mark_keyboard_cursor(targets: &[gtk::Widget], index: usize) {
    for target in targets {
        target.remove_css_class(KEYBOARD_CURSOR_CLASS);
    }
    if let Some(target) = targets.get(index) {
        target.add_css_class(KEYBOARD_CURSOR_CLASS);
    }
}

fn widget_contains_focus(widget: &gtk::Widget) -> bool {
    if widget.has_focus() || widget.is_focus() || widget.has_visible_focus() {
        return true;
    }
    let mut child = widget.focus_child();
    while let Some(child_widget) = child {
        if widget_contains_focus(&child_widget) {
            return true;
        }
        child = child_widget.focus_child();
    }
    false
}

fn scroll_adjustment(adjustment: &gtk::Adjustment, delta: f64) {
    let lower = adjustment.lower();
    let upper = (adjustment.upper() - adjustment.page_size()).max(lower);
    let value = if delta.is_infinite() && delta.is_sign_positive() {
        upper
    } else if delta.is_infinite() && delta.is_sign_negative() {
        lower
    } else {
        (adjustment.value() + delta).clamp(lower, upper)
    };
    adjustment.set_value(value);
}

fn scroll_window_lines(scrolled: &gtk::ScrolledWindow, lines: f64) {
    scroll_adjustment(&scrolled.vadjustment(), lines * 40.0);
}

fn scroll_window_pages(scrolled: &gtk::ScrolledWindow, pages: f64) {
    let adjustment = scrolled.vadjustment();
    scroll_adjustment(&adjustment, adjustment.page_size() * pages);
}

fn scroll_window_to_edge(scrolled: &gtk::ScrolledWindow, bottom: bool) {
    let adjustment = scrolled.vadjustment();
    if bottom {
        scroll_adjustment(&adjustment, f64::INFINITY);
    } else {
        scroll_adjustment(&adjustment, f64::NEG_INFINITY);
    }
}

fn active_message_scrolled(widgets: &Widgets) -> gtk::ScrolledWindow {
    if compose_view_is_visible(widgets) {
        widgets.compose_scrolled.clone()
    } else if html_view_is_visible(widgets) {
        widgets.html_scrolled.clone()
    } else {
        widgets.message_scrolled.clone()
    }
}

fn vim_scroll_lines(widgets: &Widgets, state: &SharedState, lines: f64) {
    match state.borrow().active_pane {
        ActivePane::Threads => {}
        ActivePane::Sidebar => scroll_window_lines(&widgets.thread_scrolled, lines),
        ActivePane::Message if html_view_is_visible(widgets) => {
            scroll_html_view_lines(widgets, lines)
        }
        ActivePane::Message => scroll_window_lines(&active_message_scrolled(widgets), lines),
    }
}

fn vim_scroll_pages(widgets: &Widgets, state: &SharedState, pages: f64) {
    match state.borrow().active_pane {
        ActivePane::Threads => {}
        ActivePane::Sidebar => scroll_window_pages(&widgets.thread_scrolled, pages),
        ActivePane::Message if html_view_is_visible(widgets) => {
            scroll_html_view_pages(widgets, pages)
        }
        ActivePane::Message => scroll_window_pages(&active_message_scrolled(widgets), pages),
    }
}

fn vim_scroll_to_edge(widgets: &Widgets, state: &SharedState, bottom: bool) {
    match state.borrow().active_pane {
        ActivePane::Threads => {}
        ActivePane::Sidebar => scroll_window_to_edge(&widgets.thread_scrolled, bottom),
        ActivePane::Message if html_view_is_visible(widgets) => {
            scroll_html_view_to_edge(widgets, bottom)
        }
        ActivePane::Message => scroll_window_to_edge(&active_message_scrolled(widgets), bottom),
    }
}

fn scroll_html_view_lines(widgets: &Widgets, lines: f64) {
    evaluate_html_scroll_script(
        widgets,
        &format!(
            "const e = document.scrollingElement || document.documentElement || document.body; \
             e.scrollBy(0, {}); \
             JSON.stringify({{y:e.scrollTop,h:e.scrollHeight,c:e.clientHeight}});",
            (lines * 40.0).round()
        ),
    );
}

fn scroll_html_view_pages(widgets: &Widgets, pages: f64) {
    evaluate_html_scroll_script(
        widgets,
        &format!(
            "const e = document.scrollingElement || document.documentElement || document.body; \
             e.scrollBy(0, Math.round(window.innerHeight * {})); \
             JSON.stringify({{y:e.scrollTop,h:e.scrollHeight,c:e.clientHeight}});",
            pages
        ),
    );
}

fn scroll_html_view_to_edge(widgets: &Widgets, bottom: bool) {
    let target = if bottom { "e.scrollHeight" } else { "0" };
    evaluate_html_scroll_script(
        widgets,
        &format!(
            "const e = document.scrollingElement || document.documentElement || document.body; \
             e.scrollTo(0, {target}); \
             JSON.stringify({{y:e.scrollTop,h:e.scrollHeight,c:e.clientHeight}});"
        ),
    );
}

fn current_message_scroll_fraction(widgets: &Widgets) -> Option<f64> {
    if html_view_is_visible(widgets) {
        html_scroll_fraction(widgets)
    } else if widgets
        .message_stack
        .visible_child_name()
        .is_some_and(|name| name.as_str() == "text")
    {
        Some(adjustment_scroll_fraction(
            &widgets.message_scrolled.vadjustment(),
        ))
    } else {
        None
    }
}

fn restore_message_scroll_fraction(widgets: &Widgets, fraction: Option<f64>) {
    let Some(fraction) = fraction else {
        return;
    };
    if html_view_is_visible(widgets) {
        restore_html_scroll_fraction(widgets, fraction);
    } else if widgets
        .message_stack
        .visible_child_name()
        .is_some_and(|name| name.as_str() == "text")
    {
        let scrolled = widgets.message_scrolled.clone();
        gtk::glib::idle_add_local_once(move || {
            restore_adjustment_scroll_fraction(&scrolled.vadjustment(), fraction);
        });
    }
}

fn adjustment_scroll_fraction(adjustment: &gtk::Adjustment) -> f64 {
    let max = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
    let range = max - adjustment.lower();
    if range <= 0.0 {
        0.0
    } else {
        ((adjustment.value() - adjustment.lower()) / range).clamp(0.0, 1.0)
    }
}

fn restore_adjustment_scroll_fraction(adjustment: &gtk::Adjustment, fraction: f64) {
    let lower = adjustment.lower();
    let max = (adjustment.upper() - adjustment.page_size()).max(lower);
    adjustment.set_value((lower + (max - lower) * fraction.clamp(0.0, 1.0)).clamp(lower, max));
}

fn html_scroll_fraction(widgets: &Widgets) -> Option<f64> {
    let value = evaluate_html_javascript_json_sync(
        widgets,
        "const e = document.scrollingElement || document.documentElement || document.body; \
         const max = Math.max(0, e.scrollHeight - e.clientHeight); \
         JSON.stringify({fraction:max > 0 ? e.scrollTop / max : 0});",
    )
    .ok()?;
    value.get("fraction").and_then(serde_json::Value::as_f64)
}

fn restore_html_scroll_fraction(widgets: &Widgets, fraction: f64) {
    let view = widgets.html_view.clone();
    gtk::glib::timeout_add_local_once(Duration::from_millis(100), move || {
        view.evaluate_javascript(
            &format!(
                "const e = document.scrollingElement || document.documentElement || document.body; \
                 const max = Math.max(0, e.scrollHeight - e.clientHeight); \
                 e.scrollTo(0, max * {});",
                fraction.clamp(0.0, 1.0)
            ),
            Some("notm-scroll"),
            Some("notm://scroll-restore"),
            None::<&gtk::gio::Cancellable>,
            |_| {},
        );
    });
}

fn evaluate_html_scroll_script(widgets: &Widgets, script: &str) {
    let status = widgets.status_label.clone();
    widgets.html_view.evaluate_javascript(
        script,
        Some("notm-scroll"),
        Some("notm://scroll"),
        None::<&gtk::gio::Cancellable>,
        move |result| {
            if let Err(err) = result {
                status.set_text(&format!("HTML scroll failed: {err}"));
            }
        },
    );
}

fn evaluate_html_javascript_json_sync(
    widgets: &Widgets,
    script: &str,
) -> anyhow::Result<serde_json::Value> {
    let slot: Rc<RefCell<Option<anyhow::Result<serde_json::Value>>>> = Rc::new(RefCell::new(None));
    let slot_for_callback = slot.clone();
    widgets.html_view.evaluate_javascript(
        script,
        Some("notm-automation"),
        Some("notm://automation"),
        None::<&gtk::gio::Cancellable>,
        move |result| {
            let parsed = match result {
                Ok(value) => serde_json::from_str::<serde_json::Value>(&value.to_str())
                    .map_err(anyhow::Error::from),
                Err(err) => Err(anyhow::anyhow!(err.to_string())),
            };
            *slot_for_callback.borrow_mut() = Some(parsed);
        },
    );
    let context = gtk::glib::MainContext::default();
    let started = Instant::now();
    while slot.borrow().is_none() && started.elapsed() < Duration::from_secs(2) {
        while context.pending() {
            context.iteration(false);
        }
        if slot.borrow().is_none() {
            thread::sleep(Duration::from_millis(10));
        }
    }
    slot.borrow_mut()
        .take()
        .unwrap_or_else(|| Err(anyhow::anyhow!("HTML JavaScript evaluation timed out")))
}

fn spin_main_context_for(duration: Duration) {
    let context = gtk::glib::MainContext::default();
    let started = Instant::now();
    while started.elapsed() < duration {
        while context.pending() {
            context.iteration(false);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn html_scroll_state(widgets: &Widgets) -> serde_json::Value {
    match evaluate_html_javascript_json_sync(
        widgets,
        "const e = document.scrollingElement || document.documentElement || document.body; \
         JSON.stringify({y:e.scrollTop,h:e.scrollHeight,c:e.clientHeight, \
         canScroll:e.scrollHeight > e.clientHeight});",
    ) {
        Ok(value) => json!({"ok": true, "scroll": value}),
        Err(err) => json!({"ok": false, "error": err.to_string()}),
    }
}

fn select_thread_edge(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    bottom: bool,
) {
    let len = state.borrow().thread_list_items.len();
    if len == 0 {
        return;
    }
    let index = if bottom { len - 1 } else { 0 };
    select_thread_index_clamped(options, widgets, state, index);
}

fn select_thread_absolute(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    one_based: usize,
) {
    let mut target = one_based.saturating_sub(1);
    let (window_offset, len, total, query) = {
        let state = state.borrow();
        (
            state.thread_window_offset,
            state.thread_list_items.len(),
            state.thread_total_count as usize,
            state.current_query.clone(),
        )
    };
    if len == 0 {
        return;
    }
    if total > 0 {
        target = target.min(total - 1);
    }
    if (window_offset..window_offset + len).contains(&target) {
        select_thread_index_clamped(options, widgets, state, target - window_offset);
        return;
    }
    load_thread_page_containing_index(options, widgets, state, &query, target);
}

fn select_thread_index_clamped(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    index: usize,
) {
    let len = state.borrow().thread_list_items.len();
    if len == 0 {
        return;
    }
    let index = index.min(len - 1);
    if let Some(row) = widgets.thread_list.row_at_index(index as i32) {
        let already_selected = selected_thread_index(widgets) == Some(index);
        widgets.thread_list.select_row(Some(&row));
        focus_thread_row(&row);
        if already_selected {
            select_thread_by_index(options, widgets, state, index, false);
        }
    }
}

fn load_thread_page_containing_index(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    query: &str,
    target_index: usize,
) {
    let visual_anchor_index = visual_selection_anchor_index(widgets, state);
    let target_number = target_index + 1;
    let page_size = options.page_size.max(1);
    let offset = (target_index / page_size) * page_size;
    let page_start = offset + 1;
    let page_end = offset + page_size;
    set_thread_loading_indicator(
        widgets,
        &format!(
            "Loading message {} (page {}-{})…",
            format_count(target_number),
            format_count(page_start),
            format_count(page_end)
        ),
    );

    let (tx, rx) = mpsc::channel::<ThreadPageResponse>();
    let opts = options.clone();
    let query = query.to_string();
    let generation = widgets.search_generation.get().saturating_add(1);
    widgets.search_generation.set(generation);
    thread::spawn(move || {
        let result = execute_search_page(&opts, &query, offset);
        let _ = tx.send(ThreadPageResponse {
            generation,
            target_index,
            visual_anchor_index,
            result,
        });
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(response) => {
            if response.generation == w.search_generation.get() {
                match response.result {
                    Ok(data) => {
                        let keep_visual = response.visual_anchor_index.is_some()
                            && st.borrow().visual_select_mode
                            && st.borrow().current_query == data.query;
                        apply_search_data(&opts, &w, &st, data);
                        if keep_visual {
                            let mut state = st.borrow_mut();
                            state.visual_select_mode = true;
                            state.visual_select_anchor = response.visual_anchor_index;
                        }
                        let local_index = response
                            .target_index
                            .saturating_sub(st.borrow().thread_window_offset);
                        select_thread_index_clamped(&opts, &w, &st, local_index);
                        update_thread_result_label(&w, &st);
                    }
                    Err(err) => apply_search_error(&w, &st, err),
                }
            }
            gtk::glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            apply_search_error(&w, &st, anyhow::anyhow!("thread page load cancelled"));
            gtk::glib::ControlFlow::Break
        }
    });
}

fn set_thread_loading_indicator(widgets: &Widgets, message: &str) {
    widgets.status_label.set_text(message);
    widgets.load_more_button.set_label("Loading…");
    widgets.load_more_button.set_sensitive(false);
    let context = gtk::glib::MainContext::default();
    while context.pending() {
        context.iteration(false);
    }
}

fn visible_thread_row_count(widgets: &Widgets) -> isize {
    let row_height = widgets
        .thread_list
        .selected_row()
        .or_else(|| widgets.thread_list.row_at_index(0))
        .map(|row| row.height().max(1) as f64)
        .unwrap_or(64.0);
    (widgets.thread_scrolled.vadjustment().page_size() / row_height)
        .floor()
        .max(1.0) as isize
}

fn select_thread_page(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    pages: isize,
) {
    let page = (visible_thread_row_count(widgets) / 2).max(1);
    select_relative_thread(options, widgets, state, page * pages);
}

fn focus_thread_row(row: &gtk::ListBoxRow) {
    row.grab_focus();
}

fn key_to_digit(key: gtk::gdk::Key) -> Option<u8> {
    key.to_unicode()
        .and_then(|ch| ch.to_digit(10))
        .map(|digit| digit as u8)
}

fn numeric_prefix_value(prefix: &Rc<RefCell<String>>) -> Option<usize> {
    let prefix = prefix.borrow();
    if prefix.is_empty() {
        return None;
    }
    prefix.parse::<usize>().ok().filter(|value| *value > 0)
}

fn take_numeric_prefix(prefix: &Rc<RefCell<String>>) -> Option<usize> {
    let value = numeric_prefix_value(prefix);
    prefix.borrow_mut().clear();
    value
}

fn clear_numeric_prefix(prefix: &Rc<RefCell<String>>) {
    prefix.borrow_mut().clear();
}

fn button_label(base: &str, binding: &str, state: &SharedState) -> String {
    if state.borrow().input_mode == InputMode::Normal && !binding.is_empty() {
        format!("{base} ({binding})")
    } else {
        base.to_string()
    }
}

fn strip_binding_suffix(label: &str) -> String {
    if label.ends_with(')')
        && let Some(index) = label.rfind(" (")
    {
        return label[..index].to_string();
    }
    label.to_string()
}

fn set_button_label(widget: &gtk::Button, base: &str, binding: &str, state: &SharedState) {
    widget.set_label(&button_label(base, binding, state));
}

fn set_menu_button_label(widget: &gtk::MenuButton, base: &str, binding: &str, state: &SharedState) {
    widget.set_label(&button_label(base, binding, state));
}

fn update_button_binding_labels(widgets: &Widgets, state: &SharedState) {
    set_button_label(&widgets.compose_button, "Compose", "c", state);
    set_button_label(&widgets.debug_button, "Debug", "d", state);
    set_button_label(&widgets.palette_button, "Commands", "Ctrl+K", state);
    set_button_label(&widgets.settings_button, "Settings", ",", state);
    set_button_label(&widgets.help_button, "Help", "?", state);
    set_button_label(&widgets.search_button, "Search", "/", state);
    set_button_label(&widgets.load_more_button, "Load more", "G", state);
    set_button_label(&widgets.archive_button, "Archive", "a", state);
    let read_base = strip_binding_suffix(&widgets.read_toggle_button.label().unwrap_or_default());
    set_button_label(&widgets.read_toggle_button, &read_base, "u", state);
    let flag_base = strip_binding_suffix(&widgets.flag_toggle_button.label().unwrap_or_default());
    set_button_label(&widgets.flag_toggle_button, &flag_base, "f", state);
    set_button_label(&widgets.trash_button, "Trash", "t", state);
    set_button_label(&widgets.spam_button, "Spam", "s", state);
    set_menu_button_label(&widgets.tag_menu_button, "Tag…", "T", state);
    set_button_label(&widgets.add_custom_tag_button, "Add tag", "T t", state);
    set_button_label(
        &widgets.remove_custom_tag_button,
        "Remove tag",
        "T t",
        state,
    );
    set_button_label(&widgets.tag_command_apply_button, "Apply", "T m", state);
    set_menu_button_label(&widgets.undo_tag_button, "Undo", "z", state);
    set_button_label(&widgets.undo_last_tag_button, "Undo last", "z z", state);
    set_button_label(&widgets.undo_list_tag_button, "Undo multiple", "z m", state);
    set_menu_button_label(&widgets.response_menu_button, "Respond", "r", state);
    set_button_label(&widgets.reply_button, "Reply", "r r", state);
    set_button_label(&widgets.reply_all_button, "Reply all", "r a", state);
    set_button_label(&widgets.forward_button, "Forward", "r f", state);
    set_button_label(
        &widgets.forward_attachment_button,
        "Forward attached",
        "r A",
        state,
    );
    set_menu_button_label(&widgets.view_menu_button, "View", "V", state);
    set_button_label(&widgets.view_text_button, "Text", "V t", state);
    set_button_label(&widgets.view_html_button, "Visual HTML", "V v", state);
    set_button_label(&widgets.view_headers_button, "Full headers", "V h", state);
    set_button_label(&widgets.view_raw_button, "Raw source", "V r", state);
    set_button_label(
        &widgets.collapse_quotes_button,
        "Collapse quotes",
        "q",
        state,
    );
    set_menu_button_label(&widgets.copy_menu_button, "Copy", "y", state);
    set_button_label(
        &widgets.copy_message_id_button,
        "Copy message id",
        "y m",
        state,
    );
    set_button_label(
        &widgets.copy_thread_id_button,
        "Copy thread id",
        "y t",
        state,
    );
    set_button_label(
        &widgets.copy_from_email_button,
        "Copy from email",
        "y f",
        state,
    );
    set_button_label(&widgets.copy_to_email_button, "Copy to email", "y o", state);
    set_button_label(&widgets.copy_cc_email_button, "Copy cc email", "y c", state);
    set_button_label(&widgets.copy_subject_button, "Copy subject", "y s", state);
    let image_base = strip_binding_suffix(&widgets.image_policy_button.label().unwrap_or_default());
    set_button_label(&widgets.image_policy_button, &image_base, "I", state);
    set_button_label(
        &widgets.add_attachment_button,
        "Add attachment…",
        "A",
        state,
    );
    set_button_label(&widgets.save_draft_button, "Save draft", "S", state);
    let clear_base = strip_binding_suffix(&widgets.clear_draft_button.label().unwrap_or_default());
    set_button_label(&widgets.clear_draft_button, &clear_base, "x", state);
    set_button_label(
        &widgets.delete_local_draft_button,
        "Delete local draft",
        "D",
        state,
    );
    set_button_label(&widgets.send_button, "Send", "Ctrl+Enter", state);
    update_saved_search_button_labels(widgets, state);
    update_tag_search_button_labels(widgets, state);
}

fn clear_go_prompt_status(widgets: &Widgets) {
    if widgets.status_label.text().starts_with("Go:") {
        widgets.status_label.set_text("Normal mode");
    }
}

fn connect_input_mode_focus(widgets: &Widgets, state: &SharedState) {
    connect_text_focus(
        &widgets.saved_name_entry,
        widgets,
        state,
        ActivePane::Sidebar,
    );
    connect_text_focus(
        &widgets.saved_query_entry,
        widgets,
        state,
        ActivePane::Sidebar,
    );
    connect_text_focus(
        &widgets.custom_tag_entry,
        widgets,
        state,
        ActivePane::Threads,
    );
    connect_text_focus(&widgets.search_entry, widgets, state, ActivePane::Threads);
    connect_text_focus(&widgets.compose_from, widgets, state, ActivePane::Message);
    connect_text_focus(&widgets.compose_to, widgets, state, ActivePane::Message);
    connect_text_focus(&widgets.compose_cc, widgets, state, ActivePane::Message);
    connect_text_focus(&widgets.compose_bcc, widgets, state, ActivePane::Message);
    connect_text_focus(
        &widgets.compose_subject,
        widgets,
        state,
        ActivePane::Message,
    );
    connect_compose_body_focus(&widgets.compose_body, widgets, state);
}

fn connect_text_focus<W>(widget: &W, widgets: &Widgets, state: &SharedState, pane: ActivePane)
where
    W: IsA<gtk::Widget> + Clone + 'static,
{
    let focus = gtk::EventControllerFocus::new();
    let w = widgets.clone();
    let st = state.clone();
    focus.connect_enter(move |_| {
        let Ok(mut state) = st.try_borrow_mut() else {
            return;
        };
        state.active_pane = pane;
        drop(state);
        update_button_binding_labels(&w, &st);
        update_active_pane_visuals(&w, &st);
    });
    widget.add_controller(focus);
}

fn connect_compose_body_focus<W>(widget: &W, widgets: &Widgets, state: &SharedState)
where
    W: IsA<gtk::Widget> + Clone + 'static,
{
    let focus = gtk::EventControllerFocus::new();
    let w = widgets.clone();
    let st = state.clone();
    focus.connect_enter(move |_| {
        let Ok(mut state) = st.try_borrow_mut() else {
            return;
        };
        state.active_pane = ActivePane::Message;
        state.input_mode = InputMode::Insert;
        drop(state);
        update_button_binding_labels(&w, &st);
        update_active_pane_visuals(&w, &st);
        w.status_label
            .set_text("Vim composer: Esc leaves insert/visual, Esc again exits to notm");
    });
    widget.add_controller(focus);
}

fn install_shortcuts(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    saved_store: &SavedSearchStore,
) {
    let capture_controller = gtk::EventControllerKey::new();
    capture_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    capture_controller.connect_key_pressed(move |_, key, _, mods| {
        let ctrl = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK);
        if ctrl && (key == gtk::gdk::Key::k || key == gtk::gdk::Key::K) {
            show_command_palette(&opts, &w, &st, &undo);
            return gtk::glib::Propagation::Stop;
        }
        if ctrl
            && (key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter)
            && compose_view_is_visible(&w)
        {
            send_compose(&opts, &w, &st);
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Escape && st.borrow().visual_select_mode {
            clear_visual_selection(&w, &st);
            return gtk::glib::Propagation::Stop;
        }
        if st.borrow().input_mode == InputMode::Insert {
            if key == gtk::gdk::Key::Tab && complete_focused_recipient(&w, &st) {
                autosave_draft_from_widgets(&w, &st);
                return gtk::glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Escape {
                if tag_editor_insert_mode_active(&w, &st) {
                    close_custom_tag_editor(&w, &st);
                    return gtk::glib::Propagation::Stop;
                }
                if w.compose_body.has_focus() {
                    if ctrl || compose_vim_ready_for_app_escape(&w.compose_vim_context) {
                        enter_normal_mode(&w, &st);
                        return gtk::glib::Propagation::Stop;
                    }
                    return gtk::glib::Propagation::Proceed;
                }
                enter_normal_mode(&w, &st);
                return gtk::glib::Propagation::Stop;
            }
            return gtk::glib::Propagation::Proceed;
        }
        if ctrl && (key == gtk::gdk::Key::h || key == gtk::gdk::Key::H) {
            move_active_pane(&w, &st, -1);
            return gtk::glib::Propagation::Stop;
        }
        if ctrl && (key == gtk::gdk::Key::l || key == gtk::gdk::Key::L) {
            move_active_pane(&w, &st, 1);
            return gtk::glib::Propagation::Stop;
        }
        if ctrl && (key == gtk::gdk::Key::d || key == gtk::gdk::Key::D) {
            if st.borrow().active_pane == ActivePane::Threads {
                select_thread_page(&opts, &w, &st, 1);
            } else if st.borrow().active_pane == ActivePane::Sidebar {
                move_sidebar_focus(&w, 5);
            } else if compose_view_is_visible(&w) {
                move_composer_focus(&w, 5);
            } else {
                vim_scroll_pages(&w, &st, 0.5);
            }
            return gtk::glib::Propagation::Stop;
        }
        if ctrl && (key == gtk::gdk::Key::u || key == gtk::gdk::Key::U) {
            if st.borrow().active_pane == ActivePane::Threads {
                select_thread_page(&opts, &w, &st, -1);
            } else if st.borrow().active_pane == ActivePane::Sidebar {
                move_sidebar_focus(&w, -5);
            } else if compose_view_is_visible(&w) {
                move_composer_focus(&w, -5);
            } else {
                vim_scroll_pages(&w, &st, -0.5);
            }
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter {
            match st.borrow().active_pane {
                ActivePane::Threads => {
                    let idx = selected_thread_index(&w).unwrap_or(0);
                    open_thread_by_index(&opts, &w, &st, idx);
                }
                ActivePane::Sidebar => activate_focused_sidebar_widget(&w, &st),
                ActivePane::Message if compose_view_is_visible(&w) => {
                    enter_insert_mode_for_active_pane(&w, &st);
                }
                ActivePane::Message => {}
            }
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    widgets.window.add_controller(capture_controller);

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    let saved = saved_store.clone();
    let pending_go = Rc::new(RefCell::new(false));
    let pending_custom_search = Rc::new(RefCell::new(false));
    let pending_response = Rc::new(RefCell::new(false));
    let pending_view = Rc::new(RefCell::new(false));
    let pending_copy = Rc::new(RefCell::new(false));
    let pending_tag = Rc::new(RefCell::new(false));
    let pending_undo = Rc::new(RefCell::new(false));
    let numeric_prefix = Rc::new(RefCell::new(String::new()));
    connect_dropdown_sequence_keys(
        &opts,
        &w,
        &st,
        pending_response.clone(),
        pending_view.clone(),
        pending_copy.clone(),
        pending_tag.clone(),
        pending_undo.clone(),
        undo.clone(),
    );
    controller.connect_key_pressed(move |_, key, _, mods| {
        let ctrl = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK);
        if ctrl {
            return gtk::glib::Propagation::Proceed;
        }
        if st.borrow().input_mode == InputMode::Insert {
            return gtk::glib::Propagation::Proceed;
        }
        if key == gtk::gdk::Key::Escape {
            *pending_go.borrow_mut() = false;
            *pending_custom_search.borrow_mut() = false;
            *pending_response.borrow_mut() = false;
            *pending_view.borrow_mut() = false;
            *pending_copy.borrow_mut() = false;
            *pending_tag.borrow_mut() = false;
            *pending_undo.borrow_mut() = false;
            w.response_menu_button.popdown();
            w.view_menu_button.popdown();
            w.copy_menu_button.popdown();
            w.tag_menu_button.popdown();
            w.undo_tag_button.popdown();
            if st.borrow().visual_select_mode {
                clear_visual_selection(&w, &st);
            } else {
                w.status_label.set_text("Normal mode");
            }
            return gtk::glib::Propagation::Stop;
        }
        if *pending_custom_search.borrow() {
            *pending_custom_search.borrow_mut() = false;
            clear_numeric_prefix(&numeric_prefix);
            let handled = open_custom_saved_search_by_key(&opts, &w, &st, &saved, key);
            clear_go_prompt_status(&w);
            return if handled {
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            };
        }
        if *pending_response.borrow() {
            *pending_response.borrow_mut() = false;
            w.response_menu_button.popdown();
            clear_numeric_prefix(&numeric_prefix);
            let handled = if key == gtk::gdk::Key::r {
                reply_selected(&opts, &w, &st, ReplyKind::Sender);
                true
            } else if key == gtk::gdk::Key::a {
                reply_selected(&opts, &w, &st, ReplyKind::All);
                true
            } else if key == gtk::gdk::Key::f {
                forward_selected(&opts, &w, &st);
                true
            } else if key == gtk::gdk::Key::A {
                forward_as_attachment_selected(&opts, &w, &st);
                true
            } else {
                false
            };
            return if handled {
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            };
        }
        if *pending_view.borrow() {
            *pending_view.borrow_mut() = false;
            w.view_menu_button.popdown();
            clear_numeric_prefix(&numeric_prefix);
            let handled = if key == gtk::gdk::Key::t {
                let scroll = current_message_scroll_fraction(&w);
                st.borrow_mut().prefer_html_view = false;
                show_rendered_selected_thread(&opts, &w, &st);
                restore_message_scroll_fraction(&w, scroll);
                true
            } else if key == gtk::gdk::Key::v {
                let scroll = current_message_scroll_fraction(&w);
                st.borrow_mut().prefer_html_view = true;
                show_visual_html_selected_message(&opts, &w, &st);
                restore_message_scroll_fraction(&w, scroll);
                true
            } else if key == gtk::gdk::Key::h {
                show_full_headers(&opts, &w, &st);
                true
            } else if key == gtk::gdk::Key::r {
                show_raw_source(&opts, &w, &st);
                true
            } else {
                false
            };
            return if handled {
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            };
        }
        if *pending_copy.borrow() {
            *pending_copy.borrow_mut() = false;
            w.copy_menu_button.popdown();
            clear_numeric_prefix(&numeric_prefix);
            let handled = if key == gtk::gdk::Key::m {
                copy_selected_message_id(&w, &st);
                true
            } else if key == gtk::gdk::Key::t {
                copy_selected_thread_id(&w, &st);
                true
            } else if key == gtk::gdk::Key::f {
                copy_selected_message_emails(&w, &st, MessageEmailField::From);
                true
            } else if key == gtk::gdk::Key::o {
                copy_selected_message_emails(&w, &st, MessageEmailField::To);
                true
            } else if key == gtk::gdk::Key::c {
                copy_selected_message_emails(&w, &st, MessageEmailField::Cc);
                true
            } else if key == gtk::gdk::Key::s {
                copy_selected_message_subject(&w, &st);
                true
            } else {
                false
            };
            return if handled {
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            };
        }
        if *pending_tag.borrow() {
            *pending_tag.borrow_mut() = false;
            clear_numeric_prefix(&numeric_prefix);
            let handled = if key == gtk::gdk::Key::t {
                open_custom_tag_editor(&w, &st);
                true
            } else if key == gtk::gdk::Key::m {
                open_notmuch_tag_command_editor(&w, &st);
                true
            } else {
                w.tag_menu_button.popdown();
                false
            };
            return if handled {
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            };
        }
        if *pending_undo.borrow() {
            *pending_undo.borrow_mut() = false;
            w.undo_tag_button.popdown();
            clear_numeric_prefix(&numeric_prefix);
            let handled = if key == gtk::gdk::Key::z {
                undo_last_tag(&opts, &w, &st, &undo);
                true
            } else if key == gtk::gdk::Key::m {
                show_undo_tag_actions(&opts, &w, &st, &undo);
                true
            } else {
                false
            };
            return if handled {
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            };
        }
        if *pending_go.borrow() {
            *pending_go.borrow_mut() = false;
            let count = take_numeric_prefix(&numeric_prefix);
            let handled = if key == gtk::gdk::Key::g {
                if let Some(count) = count {
                    select_thread_absolute(&opts, &w, &st, count);
                } else if st.borrow().active_pane == ActivePane::Message {
                    vim_scroll_to_edge(&w, &st, false);
                } else {
                    select_thread_edge(&opts, &w, &st, false);
                }
                true
            } else if key_to_digit(key).is_some()
                && count.is_none()
                && open_visible_tag_by_key(&opts, &w, &st, key)
            {
                true
            } else if key == gtk::gdk::Key::i {
                open_saved_search_name(&opts, &w, &st, "Inbox");
                set_active_pane(&w, &st, ActivePane::Threads);
                true
            } else if key == gtk::gdk::Key::u {
                open_saved_search_name(&opts, &w, &st, "Unread");
                set_active_pane(&w, &st, ActivePane::Threads);
                true
            } else if key == gtk::gdk::Key::f {
                open_saved_search_name(&opts, &w, &st, "Flagged");
                set_active_pane(&w, &st, ActivePane::Threads);
                true
            } else if key == gtk::gdk::Key::s {
                open_saved_search_name(&opts, &w, &st, "Sent");
                set_active_pane(&w, &st, ActivePane::Threads);
                true
            } else if key == gtk::gdk::Key::d {
                open_saved_search_name(&opts, &w, &st, "Drafts");
                set_active_pane(&w, &st, ActivePane::Threads);
                true
            } else if key == gtk::gdk::Key::t {
                open_saved_search_name(&opts, &w, &st, "Trash");
                set_active_pane(&w, &st, ActivePane::Threads);
                true
            } else if key == gtk::gdk::Key::a {
                open_saved_search_name(&opts, &w, &st, "All");
                set_active_pane(&w, &st, ActivePane::Threads);
                true
            } else if key == gtk::gdk::Key::c {
                *pending_custom_search.borrow_mut() = true;
                w.status_label.set_text(&custom_saved_search_prompt(&saved));
                true
            } else {
                false
            };
            return if handled {
                clear_go_prompt_status(&w);
                gtk::glib::Propagation::Stop
            } else {
                clear_go_prompt_status(&w);
                gtk::glib::Propagation::Proceed
            };
        }
        if let Some(digit) = key_to_digit(key)
            && (digit != 0 || !numeric_prefix.borrow().is_empty())
        {
            numeric_prefix.borrow_mut().push(char::from(b'0' + digit));
            w.status_label
                .set_text(&format!("count: {}", numeric_prefix.borrow()));
            return gtk::glib::Propagation::Stop;
        }
        let count = numeric_prefix_value(&numeric_prefix).unwrap_or(1);
        let handled = if key == gtk::gdk::Key::slash {
            clear_numeric_prefix(&numeric_prefix);
            enter_insert_mode_for_search(&w, &st);
            true
        } else if key == gtk::gdk::Key::i {
            clear_numeric_prefix(&numeric_prefix);
            enter_insert_mode_for_active_pane(&w, &st);
            true
        } else if key == gtk::gdk::Key::g {
            *pending_go.borrow_mut() = true;
            w.status_label.set_text(
                "Go: g top/count, 1-9 tags, i inbox, u unread, f flagged, s sent, d drafts, t trash, a all, c custom",
            );
            true
        } else if key == gtk::gdk::Key::j || key == gtk::gdk::Key::Down {
            if st.borrow().active_pane == ActivePane::Threads {
                select_relative_thread(&opts, &w, &st, count as isize);
            } else if st.borrow().active_pane == ActivePane::Sidebar {
                move_sidebar_focus(&w, count as isize);
            } else if compose_view_is_visible(&w) {
                move_composer_focus(&w, count as isize);
            } else {
                vim_scroll_lines(&w, &st, count as f64);
            }
            clear_numeric_prefix(&numeric_prefix);
            true
        } else if key == gtk::gdk::Key::k || key == gtk::gdk::Key::Up {
            if st.borrow().active_pane == ActivePane::Threads {
                select_relative_thread(&opts, &w, &st, -(count as isize));
            } else if st.borrow().active_pane == ActivePane::Sidebar {
                move_sidebar_focus(&w, -(count as isize));
            } else if compose_view_is_visible(&w) {
                move_composer_focus(&w, -(count as isize));
            } else {
                vim_scroll_lines(&w, &st, -(count as f64));
            }
            clear_numeric_prefix(&numeric_prefix);
            true
        } else if key == gtk::gdk::Key::G {
            if !numeric_prefix.borrow().is_empty() {
                select_thread_absolute(&opts, &w, &st, count);
            } else if st.borrow().active_pane == ActivePane::Message {
                vim_scroll_to_edge(&w, &st, true);
            } else {
                select_thread_edge(&opts, &w, &st, true);
            }
            clear_numeric_prefix(&numeric_prefix);
            true
        } else if key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter {
            clear_numeric_prefix(&numeric_prefix);
            if st.borrow().active_pane == ActivePane::Threads {
                let idx = selected_thread_index(&w).unwrap_or(0);
                open_thread_by_index(&opts, &w, &st, idx);
            }
            true
        } else if key == gtk::gdk::Key::a {
            clear_numeric_prefix(&numeric_prefix);
            tag_selected(
                &opts,
                &w,
                &st,
                &undo,
                TagMutation {
                    add: vec![],
                    remove: vec!["inbox".to_string()],
                    sync_maildir_flags: opts.sync_maildir_flags_after_tag_change,
                },
            );
            true
        } else if key == gtk::gdk::Key::u {
            clear_numeric_prefix(&numeric_prefix);
            toggle_unread_selected(&opts, &w, &st, &undo);
            true
        } else if key == gtk::gdk::Key::f {
            clear_numeric_prefix(&numeric_prefix);
            toggle_flagged_selected(&opts, &w, &st, &undo);
            true
        } else if key == gtk::gdk::Key::T {
            clear_numeric_prefix(&numeric_prefix);
            *pending_tag.borrow_mut() = true;
            w.tag_menu_button.popup();
            w.status_label
                .set_text("Tag: t single tag, m multiple tag changes");
            true
        } else if key == gtk::gdk::Key::r {
            clear_numeric_prefix(&numeric_prefix);
            *pending_response.borrow_mut() = true;
            w.response_menu_button.popup();
            w.status_label
                .set_text("Respond: r reply, a reply all, f forward, A forward attached");
            true
        } else if key == gtk::gdk::Key::c {
            clear_numeric_prefix(&numeric_prefix);
            open_compose(&w, &st);
            true
        } else if key == gtk::gdk::Key::t {
            clear_numeric_prefix(&numeric_prefix);
            tag_selected(
                &opts,
                &w,
                &st,
                &undo,
                TagMutation {
                    add: vec!["trash".to_string()],
                    remove: vec!["inbox".to_string(), "spam".to_string()],
                    sync_maildir_flags: opts.sync_maildir_flags_after_tag_change,
                },
            );
            true
        } else if key == gtk::gdk::Key::s {
            clear_numeric_prefix(&numeric_prefix);
            tag_selected(
                &opts,
                &w,
                &st,
                &undo,
                TagMutation {
                    add: vec!["spam".to_string()],
                    remove: vec!["inbox".to_string(), "trash".to_string()],
                    sync_maildir_flags: opts.sync_maildir_flags_after_tag_change,
                },
            );
            true
        } else if key == gtk::gdk::Key::z {
            clear_numeric_prefix(&numeric_prefix);
            *pending_undo.borrow_mut() = true;
            w.undo_tag_button.popup();
            w.status_label
                .set_text("Undo: z last tag change, m choose from list");
            true
        } else if key == gtk::gdk::Key::v {
            clear_numeric_prefix(&numeric_prefix);
            if st.borrow().active_pane == ActivePane::Threads {
                toggle_visual_select_mode(&w, &st);
                true
            } else {
                false
            }
        } else if key == gtk::gdk::Key::V {
            clear_numeric_prefix(&numeric_prefix);
            *pending_view.borrow_mut() = true;
            w.view_menu_button.popup();
            w.status_label
                .set_text("View: t text, v visual HTML, h headers, r raw source");
            true
        } else if key == gtk::gdk::Key::q {
            clear_numeric_prefix(&numeric_prefix);
            toggle_quote_collapse(&opts, &w, &st);
            true
        } else if key == gtk::gdk::Key::y {
            clear_numeric_prefix(&numeric_prefix);
            *pending_copy.borrow_mut() = true;
            w.copy_menu_button.popup();
            w.status_label
                .set_text("Copy: m message id, t thread id, f from, o to, c cc, s subject");
            true
        } else if key == gtk::gdk::Key::I {
            clear_numeric_prefix(&numeric_prefix);
            activate_image_policy_button(&opts, &w, &st);
            true
        } else if key == gtk::gdk::Key::S && compose_view_is_visible(&w) {
            clear_numeric_prefix(&numeric_prefix);
            match save_current_draft(&opts, &w, &st) {
                Ok(_) => w.status_label.set_text("Draft saved"),
                Err(err) => w
                    .status_label
                    .set_text(&format!("Draft save failed: {err}")),
            }
            refresh_draft_list(&w);
            true
        } else if key == gtk::gdk::Key::x && compose_view_is_visible(&w) {
            clear_numeric_prefix(&numeric_prefix);
            clear_draft_widgets(&w, &st);
            let _ = clear_draft_file(&w.draft_path);
            w.status_label.set_text("Composer closed");
            true
        } else if key == gtk::gdk::Key::D && compose_view_is_visible(&w) {
            clear_numeric_prefix(&numeric_prefix);
            delete_active_draft_from_ui(&opts, &w, &st);
            true
        } else if key == gtk::gdk::Key::d {
            clear_numeric_prefix(&numeric_prefix);
            let visible = w.debug_view.is_visible();
            w.debug_view.set_visible(!visible);
            true
        } else if key == gtk::gdk::Key::comma {
            clear_numeric_prefix(&numeric_prefix);
            show_settings(&w, &opts);
            true
        } else if key == gtk::gdk::Key::question {
            clear_numeric_prefix(&numeric_prefix);
            show_shortcuts_overlay(&w);
            true
        } else {
            false
        };
        if handled {
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    widgets.window.add_controller(controller);
}

#[allow(clippy::too_many_arguments)]
fn connect_dropdown_sequence_keys(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    pending_response: Rc<RefCell<bool>>,
    pending_view: Rc<RefCell<bool>>,
    pending_copy: Rc<RefCell<bool>>,
    pending_tag: Rc<RefCell<bool>>,
    pending_undo: Rc<RefCell<bool>>,
    undo_state: UndoState,
) {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let pending = pending_response.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        let handled = if key == gtk::gdk::Key::r {
            reply_selected(&opts, &w, &st, ReplyKind::Sender);
            true
        } else if key == gtk::gdk::Key::a {
            reply_selected(&opts, &w, &st, ReplyKind::All);
            true
        } else if key == gtk::gdk::Key::f {
            forward_selected(&opts, &w, &st);
            true
        } else if key == gtk::gdk::Key::A {
            forward_as_attachment_selected(&opts, &w, &st);
            true
        } else {
            false
        };
        if handled {
            *pending.borrow_mut() = false;
            w.response_menu_button.popdown();
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    widgets.response_menu_box.add_controller(controller);

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let pending = pending_view.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        let handled = if key == gtk::gdk::Key::t {
            let scroll = current_message_scroll_fraction(&w);
            st.borrow_mut().prefer_html_view = false;
            show_rendered_selected_thread(&opts, &w, &st);
            restore_message_scroll_fraction(&w, scroll);
            true
        } else if key == gtk::gdk::Key::v {
            let scroll = current_message_scroll_fraction(&w);
            st.borrow_mut().prefer_html_view = true;
            show_visual_html_selected_message(&opts, &w, &st);
            restore_message_scroll_fraction(&w, scroll);
            true
        } else if key == gtk::gdk::Key::h {
            show_full_headers(&opts, &w, &st);
            true
        } else if key == gtk::gdk::Key::r {
            show_raw_source(&opts, &w, &st);
            true
        } else {
            false
        };
        if handled {
            *pending.borrow_mut() = false;
            w.view_menu_button.popdown();
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    widgets.view_menu_box.add_controller(controller);

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w = widgets.clone();
    let st = state.clone();
    let pending = pending_copy;
    controller.connect_key_pressed(move |_, key, _, _| {
        let handled = if key == gtk::gdk::Key::m {
            copy_selected_message_id(&w, &st);
            true
        } else if key == gtk::gdk::Key::t {
            copy_selected_thread_id(&w, &st);
            true
        } else if key == gtk::gdk::Key::f {
            copy_selected_message_emails(&w, &st, MessageEmailField::From);
            true
        } else if key == gtk::gdk::Key::o {
            copy_selected_message_emails(&w, &st, MessageEmailField::To);
            true
        } else if key == gtk::gdk::Key::c {
            copy_selected_message_emails(&w, &st, MessageEmailField::Cc);
            true
        } else if key == gtk::gdk::Key::s {
            copy_selected_message_subject(&w, &st);
            true
        } else {
            false
        };
        if handled {
            *pending.borrow_mut() = false;
            w.copy_menu_button.popdown();
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    widgets.copy_menu_box.add_controller(controller);

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w = widgets.clone();
    let st = state.clone();
    let pending = pending_tag;
    controller.connect_key_pressed(move |_, key, _, _| {
        if st.borrow().input_mode == InputMode::Insert {
            return gtk::glib::Propagation::Proceed;
        }
        let handled = if key == gtk::gdk::Key::t {
            open_custom_tag_editor(&w, &st);
            true
        } else if key == gtk::gdk::Key::m {
            open_notmuch_tag_command_editor(&w, &st);
            true
        } else {
            false
        };
        if handled {
            *pending.borrow_mut() = false;
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    widgets.tag_menu_box.add_controller(controller);

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state;
    let pending = pending_undo;
    controller.connect_key_pressed(move |_, key, _, _| {
        let handled = if key == gtk::gdk::Key::z {
            undo_last_tag(&opts, &w, &st, &undo);
            true
        } else if key == gtk::gdk::Key::m {
            show_undo_tag_actions(&opts, &w, &st, &undo);
            true
        } else {
            false
        };
        if handled {
            *pending.borrow_mut() = false;
            w.undo_tag_button.popdown();
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    widgets.undo_menu_box.add_controller(controller);
}

fn connect_auto_load_more(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let adjustment = widgets.thread_scrolled.vadjustment();
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let last_auto_offset = Rc::new(Cell::new(usize::MAX));
    let auto_load_scheduled = Rc::new(Cell::new(false));
    adjustment.connect_value_changed(move |adjustment| {
        let upper = adjustment.upper();
        let page = adjustment.page_size();
        let value = adjustment.value();
        let at_bottom = upper <= page + 24.0 || value + page + 24.0 >= upper;
        if !at_bottom {
            return;
        }
        let (can_load_more, offset) = {
            let state = st.borrow();
            (
                state.can_load_more_threads,
                state.thread_window_offset + state.thread_list_items.len(),
            )
        };
        if !can_load_more || last_auto_offset.get() == offset {
            return;
        }
        if auto_load_scheduled.get() {
            return;
        }
        last_auto_offset.set(offset);
        auto_load_scheduled.set(true);
        widgets_set_pending_load_more(&w, &st);
        let opts = opts.clone();
        let w = w.clone();
        let st = st.clone();
        let scheduled = auto_load_scheduled.clone();
        gtk::glib::timeout_add_local_once(Duration::from_millis(120), move || {
            scheduled.set(false);
            load_more_threads(&opts, &w, &st);
        });
    });
}

fn widgets_set_pending_load_more(widgets: &Widgets, state: &SharedState) {
    let status = selected_thread_index(widgets)
        .map(|index| {
            format!(
                "{}; loading more…",
                message_position_status(state, index, "Selected")
            )
        })
        .unwrap_or_else(|| "Bottom reached; loading more messages…".to_string());
    widgets.status_label.set_text(&status);
    widgets.load_more_button.set_label("Loading…");
    widgets.load_more_button.set_sensitive(false);
}

fn selected_thread_index(widgets: &Widgets) -> Option<usize> {
    widgets
        .thread_list
        .selected_row()
        .map(|row| row.index() as usize)
}

fn open_saved_search_name(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    name: &str,
) {
    let query = saved_search_query(name);
    widgets.search_entry.set_text(query);
    state.borrow_mut().visible_saved_search = Some(name.to_string());
    run_search(options, widgets, state, query);
}

fn saved_search_query(name: &str) -> &'static str {
    match name {
        "Unread" => "tag:unread and not tag:trash and not tag:spam",
        "Flagged" => "tag:flagged",
        "Sent" => "tag:sent",
        "Drafts" => "tag:draft",
        "Trash" => "tag:trash",
        "All" => "*",
        _ => "tag:inbox and not tag:trash and not tag:spam",
    }
}

fn select_relative_thread(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    delta: isize,
) {
    let (window_offset, len, total, query) = {
        let state = state.borrow();
        (
            state.thread_window_offset,
            state.thread_list_items.len(),
            state.thread_total_count as usize,
            state.current_query.clone(),
        )
    };
    if len == 0 {
        return;
    }
    let current_local = selected_thread_index(widgets).unwrap_or(0);
    let current_abs = window_offset + current_local;
    let max_abs = if total > 0 {
        total - 1
    } else {
        window_offset + len - 1
    };
    let target_abs = if delta.is_negative() {
        current_abs.saturating_sub(delta.unsigned_abs())
    } else {
        current_abs.saturating_add(delta as usize).min(max_abs)
    };
    if (window_offset..window_offset + len).contains(&target_abs) {
        let next = (target_abs - window_offset) as i32;
        let Some(row) = widgets.thread_list.row_at_index(next) else {
            return;
        };
        let already_selected = selected_thread_index(widgets) == Some(next as usize);
        widgets.thread_list.select_row(Some(&row));
        focus_thread_row(&row);
        if already_selected {
            select_thread_by_index(options, widgets, state, next as usize, false);
        }
    } else {
        load_thread_page_containing_index(options, widgets, state, &query, target_abs);
    }
}

fn toggle_unread_selected(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) {
    let has_unread = tag_targets_any(state, |thread| thread.has_unread);
    let mutation = if has_unread {
        TagMutation {
            add: vec![],
            remove: vec!["unread".to_string()],
            sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
        }
    } else {
        TagMutation {
            add: vec!["unread".to_string()],
            remove: vec![],
            sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
        }
    };
    tag_selected(options, widgets, state, undo_state, mutation);
}

fn toggle_flagged_selected(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) {
    let flagged = tag_targets_any(state, |thread| thread.is_flagged);
    let mutation = if flagged {
        TagMutation {
            add: vec![],
            remove: vec!["flagged".to_string()],
            sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
        }
    } else {
        TagMutation {
            add: vec!["flagged".to_string()],
            remove: vec![],
            sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
        }
    };
    tag_selected(options, widgets, state, undo_state, mutation);
}

fn refresh_address_suggestions_async(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) {
    let (tx, rx) = mpsc::channel::<AddressSuggestionsResponse>();
    let opts = options.clone();
    thread::spawn(move || {
        let result = collect_address_suggestions(&opts);
        let _ = tx.send(AddressSuggestionsResponse { result });
    });

    let w = widgets.clone();
    let st = state.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(response) => {
            apply_address_suggestions_result(&w, &st, response.result);
            gtk::glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            st.borrow_mut().last_error = Some("address cache cancelled".to_string());
            update_debug(&w, &st);
            gtk::glib::ControlFlow::Break
        }
    });
}

fn collect_address_suggestions(options: &LaunchOptions) -> anyhow::Result<Vec<String>> {
    let db = Database::open(&open_config(options), DatabaseMode::ReadOnly)?;
    let opts = QueryOptions {
        limit: 500,
        offset: 0,
        sort: SortOrder::NewestFirst,
        excluded_tags: options.excluded_tags.clone(),
    };
    let messages = db.search_messages("*", &opts)?;
    let mut addrs = Vec::new();
    for msg in messages {
        addrs.extend(parse_address_list(&msg.from));
        addrs.extend(parse_address_list(&msg.to));
        addrs.extend(parse_address_list(&msg.cc));
    }
    let mut own = options
        .other_email
        .iter()
        .map(|s| s.to_lowercase())
        .collect::<BTreeSet<_>>();
    if let Some(email) = &options.primary_email {
        own.insert(email.to_lowercase());
    }
    let mut out = dedupe_addresses(addrs)
        .into_iter()
        .filter(|addr| !own.contains(&addr.email.to_lowercase()))
        .map(|addr| format_address(&addr))
        .collect::<Vec<_>>();
    out.sort_by_key(|s| s.to_lowercase());
    out.truncate(200);
    Ok(out)
}

fn apply_address_suggestions_result(
    widgets: &Widgets,
    state: &SharedState,
    result: anyhow::Result<Vec<String>>,
) {
    match result {
        Ok(suggestions) => {
            state.borrow_mut().address_suggestions = suggestions;
            update_address_suggestions_label(widgets, state, "");
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(format!("address cache failed: {err}"));
            update_debug(widgets, state);
        }
    }
}

fn update_address_suggestions_label(widgets: &Widgets, state: &SharedState, input: &str) {
    let entry = active_address_entry(widgets);
    update_address_suggestions_for_entry(widgets, state, &entry, input);
}

fn update_address_suggestions_for_entry(
    widgets: &Widgets,
    state: &SharedState,
    entry: &gtk::Entry,
    input: &str,
) {
    let suggestions = matching_address_suggestions(input, &state.borrow().address_suggestions, 6);
    if suggestions.is_empty() {
        hide_address_suggestions(widgets);
    } else {
        set_active_address_entry(widgets, entry);
        if let Some(field) = recipient_field_for_entry(widgets, entry) {
            *widgets.address_completion.borrow_mut() = Some(AddressCompletionSession {
                field,
                base: input.to_string(),
                suggestions: suggestions.clone(),
                next_index: 0,
                generated_text: None,
                suppress_next_change: false,
            });
        }
        place_address_suggestions_after_entry(widgets, entry);
        populate_address_suggestions_list(widgets, &suggestions);
        widgets.address_suggestions_list.set_visible(true);
    }
}

fn hide_address_suggestions(widgets: &Widgets) {
    populate_address_suggestions_list(widgets, &[]);
    widgets.address_suggestions_list.set_visible(false);
}

fn reset_address_completion(widgets: &Widgets) {
    *widgets.address_completion.borrow_mut() = None;
}

fn set_active_address_entry(widgets: &Widgets, entry: &gtk::Entry) {
    *widgets.active_address_entry.borrow_mut() = Some(entry.clone());
}

fn active_address_entry(widgets: &Widgets) -> gtk::Entry {
    widgets
        .active_address_entry
        .borrow()
        .clone()
        .unwrap_or_else(|| widgets.compose_to.clone())
}

fn recipient_field_for_entry(widgets: &Widgets, entry: &gtk::Entry) -> Option<RecipientField> {
    if entry == &widgets.compose_to {
        Some(RecipientField::To)
    } else if entry == &widgets.compose_cc {
        Some(RecipientField::Cc)
    } else if entry == &widgets.compose_bcc {
        Some(RecipientField::Bcc)
    } else {
        None
    }
}

fn focused_recipient_entry(widgets: &Widgets) -> Option<(RecipientField, gtk::Entry)> {
    match widgets.active_address_field.get()? {
        RecipientField::To => Some((RecipientField::To, widgets.compose_to.clone())),
        RecipientField::Cc => Some((RecipientField::Cc, widgets.compose_cc.clone())),
        RecipientField::Bcc => Some((RecipientField::Bcc, widgets.compose_bcc.clone())),
    }
}

fn address_completion_current_matches(
    widgets: &Widgets,
    field: Option<RecipientField>,
    text: &str,
) -> bool {
    let Some(field) = field else {
        return false;
    };
    widgets
        .address_completion
        .borrow()
        .as_ref()
        .is_some_and(|session| {
            session.field == field && address_session_matches_current(session, text)
        })
}

fn place_address_suggestions_after_entry(widgets: &Widgets, entry: &gtk::Entry) {
    let Some(parent) = entry.parent() else {
        return;
    };
    let Ok(parent_box) = parent.downcast::<gtk::Box>() else {
        return;
    };
    if let Some(current_parent) = widgets.address_suggestions_list.parent()
        && let Ok(current_box) = current_parent.downcast::<gtk::Box>()
    {
        current_box.remove(&widgets.address_suggestions_list);
    }
    parent_box.insert_child_after(&widgets.address_suggestions_list, Some(entry));
}

fn matching_address_suggestions(input: &str, suggestions: &[String], limit: usize) -> Vec<String> {
    let prefix = current_recipient_prefix(input).to_lowercase();
    if prefix.is_empty() {
        return Vec::new();
    }
    suggestions
        .iter()
        .filter(|suggestion| suggestion.to_lowercase().contains(&prefix))
        .take(limit)
        .cloned()
        .collect()
}

fn current_recipient_prefix(input: &str) -> String {
    input
        .rsplit_once(',')
        .map(|(_, tail)| tail)
        .unwrap_or(input)
        .trim()
        .to_string()
}

fn apply_recipient_completion(entry: &gtk::Entry, state: &SharedState) -> bool {
    let current = entry.text().to_string();
    let Some(suggestion) =
        matching_address_suggestions(&current, &state.borrow().address_suggestions, 1)
            .into_iter()
            .next()
    else {
        return false;
    };
    let next = if let Some((head, _)) = current.rsplit_once(',') {
        format!("{}, {}", head.trim_end(), suggestion)
    } else {
        suggestion
    };
    entry.set_text(&next);
    entry.set_position(-1);
    true
}

fn complete_focused_recipient(widgets: &Widgets, state: &SharedState) -> bool {
    let Some((field, entry)) = focused_recipient_entry(widgets) else {
        return false;
    };
    complete_recipient_entry_for_field(widgets, state, &entry, field)
}

fn complete_recipient_entry(widgets: &Widgets, state: &SharedState, entry: &gtk::Entry) -> bool {
    let Some(field) = recipient_field_for_entry(widgets, entry) else {
        return false;
    };
    complete_recipient_entry_for_field(widgets, state, entry, field)
}

fn complete_recipient_entry_for_field(
    widgets: &Widgets,
    state: &SharedState,
    entry: &gtk::Entry,
    field: RecipientField,
) -> bool {
    set_active_address_entry(widgets, entry);
    place_address_suggestions_after_entry(widgets, entry);

    let current = entry.text().to_string();
    let reuse_session = widgets
        .address_completion
        .borrow()
        .as_ref()
        .is_some_and(|session| {
            session.field == field && address_session_matches_current(session, &current)
        });

    if !reuse_session {
        let suggestions =
            matching_address_suggestions(&current, &state.borrow().address_suggestions, 20);
        if suggestions.is_empty() {
            hide_address_suggestions(widgets);
            return false;
        }
        *widgets.address_completion.borrow_mut() = Some(AddressCompletionSession {
            field,
            base: current.clone(),
            suggestions,
            next_index: 0,
            generated_text: None,
            suppress_next_change: false,
        });
    }

    let (next, index, suggestions) = {
        let mut completion = widgets.address_completion.borrow_mut();
        let Some(session) = completion.as_mut() else {
            return false;
        };
        if session.suggestions.is_empty() {
            *completion = None;
            return false;
        }
        if let Some(current_index) = address_generated_index(session, &current) {
            session.next_index = current_index.saturating_add(1);
        }
        let index = session.next_index % session.suggestions.len();
        let next = recipient_suggestion_text(&session.base, &session.suggestions[index]);
        session.generated_text = Some(next.clone());
        session.suppress_next_change = true;
        session.next_index = index + 1;
        (next, index, session.suggestions.clone())
    };

    entry.set_text(&next);
    entry.set_position(-1);
    populate_address_suggestions_list(widgets, &suggestions);
    if let Some(row) = widgets.address_suggestions_list.row_at_index(index as i32) {
        widgets.address_suggestions_list.select_row(Some(&row));
    }
    widgets.address_suggestions_list.set_visible(true);
    true
}

fn apply_recipient_suggestion(entry: &gtk::Entry, suggestion: &str) {
    let current = entry.text().to_string();
    apply_recipient_suggestion_to_text(entry, &current, suggestion);
}

fn apply_recipient_suggestion_to_text(entry: &gtk::Entry, current: &str, suggestion: &str) {
    let next = recipient_suggestion_text(current, suggestion);
    entry.set_text(&next);
    entry.set_position(-1);
}

fn recipient_suggestion_text(current: &str, suggestion: &str) -> String {
    if let Some((head, _)) = current.rsplit_once(',') {
        format!("{}, {}", head.trim_end(), suggestion)
    } else {
        suggestion.to_string()
    }
}

fn address_session_matches_current(session: &AddressCompletionSession, current: &str) -> bool {
    session.base == current
        || session.generated_text.as_deref() == Some(current)
        || address_generated_index(session, current).is_some()
}

fn address_generated_index(session: &AddressCompletionSession, current: &str) -> Option<usize> {
    session
        .suggestions
        .iter()
        .position(|suggestion| recipient_suggestion_text(&session.base, suggestion) == current)
}

fn populate_address_suggestions_list(widgets: &Widgets, suggestions: &[String]) {
    while let Some(child) = widgets.address_suggestions_list.first_child() {
        widgets.address_suggestions_list.remove(&child);
    }
    for suggestion in suggestions {
        let row = gtk::ListBoxRow::new();
        row.set_widget_name(&format!(
            "notm-address-suggestion-{}",
            widget_token(suggestion)
        ));
        row.set_focusable(false);
        let label = gtk::Label::new(Some(suggestion));
        label.set_xalign(0.0);
        label.set_margin_start(6);
        label.set_margin_end(6);
        label.set_margin_top(3);
        label.set_margin_bottom(3);
        row.set_child(Some(&label));
        widgets.address_suggestions_list.append(&row);
    }
}

fn compose_fields(widgets: &Widgets, state: &SharedState) -> ComposeFields {
    let mut fields = read_compose_fields(widgets);
    let stored = state.borrow().compose_fields.clone();
    fields.attachments = stored.attachments;
    fields.in_reply_to = stored.in_reply_to;
    fields.references = stored.references;
    fields
}

fn autosave_draft_from_widgets(widgets: &Widgets, state: &SharedState) {
    let fields = compose_fields(widgets, state);
    state.borrow_mut().compose_fields = fields.clone();
    update_attachment_label(widgets, &fields.attachments);
    if fields_has_content(&fields) {
        let _ = save_draft_fields(&widgets.draft_path, &fields);
    }
    update_draft_action_buttons(widgets, state);
}

fn update_draft_action_buttons(widgets: &Widgets, state: &SharedState) {
    let active_draft = state.borrow().active_draft.clone();
    if let Some(active_draft) = active_draft {
        let current_fields = compose_fields(widgets, state);
        if current_fields == active_draft.saved_fields {
            widgets.clear_draft_button.set_label("Close draft");
        } else {
            widgets.clear_draft_button.set_label("Discard changes");
        }
        widgets.delete_local_draft_button.set_visible(true);
    } else {
        widgets.clear_draft_button.set_label("Discard draft");
        widgets.delete_local_draft_button.set_visible(false);
    }
    update_button_binding_labels(widgets, state);
}

fn fields_has_content(fields: &ComposeFields) -> bool {
    !fields.to.trim().is_empty()
        || !fields.cc.trim().is_empty()
        || !fields.bcc.trim().is_empty()
        || !fields.subject.trim().is_empty()
        || !fields.body.trim().is_empty()
        || !fields.attachments.is_empty()
}

fn default_draft_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("target/notm-cache"));
    base.join("notm/draft.json")
}

fn default_drafts_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("target/notm-cache"));
    base.join("notm/drafts")
}

fn save_draft_fields(path: &Path, fields: &ComposeFields) -> anyhow::Result<PathBuf> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(fields)?)?;
    Ok(path.to_path_buf())
}

fn save_named_draft_fields(dir: &Path, fields: &ComposeFields) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(fields_has_content(fields), "draft has no content");
    std::fs::create_dir_all(dir)?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let slug = widget_token(&fields.subject);
    let slug = if slug.is_empty() {
        "untitled".to_string()
    } else {
        slug.chars().take(32).collect()
    };
    let path = dir.join(format!("{stamp}-{slug}-{}.json", Uuid::new_v4()));
    std::fs::write(&path, serde_json::to_vec_pretty(fields)?)?;
    Ok(path)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DraftSaveReport {
    local_path: Option<PathBuf>,
    maildir_path: Option<PathBuf>,
    indexed_message_id: Option<String>,
    replaced_path: Option<PathBuf>,
}

fn save_current_draft(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> anyhow::Result<DraftSaveReport> {
    let fields = compose_fields(widgets, state);
    anyhow::ensure!(fields_has_content(&fields), "draft has no content");
    let previous_draft = state.borrow().active_draft.clone();
    let persisted = if options.save_drafts_to_maildir {
        let message = composed_message_from_fields(&fields)?;
        persist_draft_message(options, &message)?
    } else {
        None
    };
    let local_path = if persisted.is_none() {
        Some(save_named_draft_fields(&widgets.drafts_dir, &fields)?)
    } else {
        None
    };
    let active_draft = persisted
        .as_ref()
        .map(|persisted| ActiveDraft {
            path: persisted.path.clone(),
            message_id: persisted.indexed_message_id.clone(),
            indexed: persisted.indexed_message_id.is_some(),
            saved_fields: fields.clone(),
        })
        .or_else(|| {
            local_path.as_ref().map(|path| ActiveDraft {
                path: path.clone(),
                message_id: None,
                indexed: false,
                saved_fields: fields.clone(),
            })
        });
    set_active_draft(widgets, state, active_draft);
    let replaced_path = if let Some(previous) = previous_draft
        && Some(&previous.path)
            != persisted
                .as_ref()
                .map(|persisted| &persisted.path)
                .or(local_path.as_ref())
    {
        delete_draft_source(options, &previous)?;
        Some(previous.path)
    } else {
        None
    };
    Ok(DraftSaveReport {
        local_path,
        maildir_path: persisted.as_ref().map(|persisted| persisted.path.clone()),
        indexed_message_id: persisted.and_then(|persisted| persisted.indexed_message_id),
        replaced_path,
    })
}

fn set_active_draft(widgets: &Widgets, state: &SharedState, active_draft: Option<ActiveDraft>) {
    state.borrow_mut().active_draft = active_draft;
    update_draft_action_buttons(widgets, state);
}

fn delete_draft_source(options: &LaunchOptions, draft: &ActiveDraft) -> anyhow::Result<()> {
    if draft.indexed {
        let db = Database::open(&open_config(options), DatabaseMode::ReadWrite)?;
        db.remove_message_file(&draft.path)?;
    }
    if draft.path.exists() {
        std::fs::remove_file(&draft.path)?;
    }
    Ok(())
}

fn delete_active_draft_from_ui(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let Some(draft) = state.borrow().active_draft.clone() else {
        widgets
            .status_label
            .set_text("No saved local draft to delete");
        return;
    };
    match delete_draft_source(options, &draft) {
        Ok(()) => {
            clear_draft_widgets(widgets, state);
            let _ = clear_draft_file(&widgets.draft_path);
            let current = state.borrow().current_query.clone();
            run_search(options, widgets, state, &current);
            widgets
                .status_label
                .set_text(&format!("Deleted local draft {}", draft.path.display()));
            {
                let mut state = state.borrow_mut();
                state.last_error = None;
                state.last_operation =
                    Some(format!("deleted local draft {}", draft.path.display()));
            }
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Delete local draft failed: {err}"));
            update_debug(widgets, state);
        }
    }
}

fn list_named_drafts(dir: &Path) -> Vec<(PathBuf, ComposeFields)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut drafts = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                return None;
            }
            let fields =
                serde_json::from_slice::<ComposeFields>(&std::fs::read(&path).ok()?).ok()?;
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok();
            Some((modified, path, fields))
        })
        .collect::<Vec<_>>();
    drafts.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    drafts
        .into_iter()
        .map(|(_, path, fields)| (path, fields))
        .collect()
}

fn refresh_draft_list(widgets: &Widgets) {
    while let Some(child) = widgets.draft_list.first_child() {
        widgets.draft_list.remove(&child);
    }
    for (index, (path, fields)) in list_named_drafts(&widgets.drafts_dir)
        .into_iter()
        .enumerate()
    {
        let row = gtk::ListBoxRow::new();
        row.set_widget_name(&format!("notm-draft-row-{index}"));
        let subject = if fields.subject.trim().is_empty() {
            "(no subject)"
        } else {
            fields.subject.trim()
        };
        let to = if fields.to.trim().is_empty() {
            "(no recipients)"
        } else {
            fields.to.trim()
        };
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("draft.json");
        let label = gtk::Label::new(Some(&format!("{subject} → {to}\n{filename}")));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_margin_start(6);
        label.set_margin_end(6);
        label.set_margin_top(3);
        label.set_margin_bottom(3);
        row.set_child(Some(&label));
        widgets.draft_list.append(&row);
    }
}

fn selected_named_draft(widgets: &Widgets) -> anyhow::Result<(PathBuf, ComposeFields)> {
    let index = widgets
        .draft_list
        .selected_row()
        .map(|row| row.index() as usize)
        .unwrap_or(0);
    list_named_drafts(&widgets.drafts_dir)
        .into_iter()
        .nth(index)
        .ok_or_else(|| anyhow::anyhow!("no selected draft"))
}

fn load_selected_named_draft(widgets: &Widgets, state: &SharedState) -> anyhow::Result<()> {
    let (path, fields) = selected_named_draft(widgets)?;
    apply_compose_fields(widgets, state, fields.clone());
    set_active_draft(
        widgets,
        state,
        Some(ActiveDraft {
            path,
            message_id: None,
            indexed: false,
            saved_fields: fields,
        }),
    );
    show_compose_view(widgets);
    Ok(())
}

fn delete_selected_named_draft(widgets: &Widgets) -> anyhow::Result<()> {
    let (path, _) = selected_named_draft(widgets)?;
    std::fs::remove_file(path)?;
    refresh_draft_list(widgets);
    Ok(())
}

fn restore_draft_if_present(widgets: &Widgets, state: &SharedState) {
    let path = &widgets.draft_path;
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(fields) = serde_json::from_slice::<ComposeFields>(&bytes) else {
        return;
    };
    if fields_has_content(&fields) {
        apply_compose_fields(widgets, state, fields);
        show_compose_view(widgets);
        widgets
            .status_label
            .set_text(&format!("Recovered draft from {}", path.display()));
    }
}

fn clear_draft_file(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn clear_draft_widgets(widgets: &Widgets, state: &SharedState) {
    let fields = ComposeFields {
        from: widgets.compose_from.text().to_string(),
        ..ComposeFields::default()
    };
    apply_compose_fields(widgets, state, fields);
    set_active_draft(widgets, state, None);
    widgets.address_suggestions_list.set_visible(false);
    widgets.message_stack.set_visible_child_name("text");
    refresh_thread_attachment_list(widgets, state);
}

fn apply_compose_fields(widgets: &Widgets, state: &SharedState, fields: ComposeFields) {
    widgets.compose_from.set_text(&fields.from);
    widgets.compose_to.set_text(&fields.to);
    widgets.compose_cc.set_text(&fields.cc);
    widgets.compose_bcc.set_text(&fields.bcc);
    widgets.compose_subject.set_text(&fields.subject);
    widgets.compose_body.buffer().set_text(&fields.body);
    move_compose_cursor_to_start(widgets);
    update_attachment_label(widgets, &fields.attachments);
    state.borrow_mut().compose_fields = fields;
    update_draft_action_buttons(widgets, state);
}

fn move_compose_cursor_to_start(widgets: &Widgets) {
    let buffer = widgets.compose_body.buffer();
    let start = buffer.start_iter();
    buffer.place_cursor(&start);
    let compose_body = widgets.compose_body.clone();
    gtk::glib::timeout_add_local_once(Duration::from_millis(0), move || {
        let mut start = compose_body.buffer().start_iter();
        compose_body.scroll_to_iter(&mut start, 0.0, true, 0.0, 0.0);
    });
}

fn add_attachment_path(widgets: &Widgets, state: &SharedState, path: PathBuf) {
    let mut fields = compose_fields(widgets, state);
    let path_text = path.display().to_string();
    if !fields
        .attachments
        .iter()
        .any(|existing| existing == &path_text)
    {
        fields.attachments.push(path_text);
    }
    update_attachment_label(widgets, &fields.attachments);
    state.borrow_mut().compose_fields = fields.clone();
    let _ = save_draft_fields(&widgets.draft_path, &fields);
    update_draft_action_buttons(widgets, state);
    widgets.status_label.set_text("Attachment added to draft");
}

fn update_attachment_label(widgets: &Widgets, attachments: &[String]) {
    if attachments.is_empty() {
        widgets.compose_attachments.set_text("No attachments");
    } else {
        widgets
            .compose_attachments
            .set_text(&format!("Attachments: {}", attachments.join(", ")));
    }
}

fn load_compose_attachments(fields: &ComposeFields) -> anyhow::Result<Vec<AttachmentInput>> {
    fields
        .attachments
        .iter()
        .map(|path| {
            let path = PathBuf::from(path);
            let bytes = std::fs::read(&path)?;
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("attachment.bin")
                .to_string();
            Ok(AttachmentInput {
                filename,
                content_type: attachment_content_type(&path),
                bytes,
                source_path: Some(path),
            })
        })
        .collect()
}

fn composed_message_from_fields(fields: &ComposeFields) -> anyhow::Result<ComposedMessage> {
    let mut message = ComposedMessage::new(
        fields.from.clone(),
        split_recipients(&fields.to),
        fields.subject.clone(),
        fields.body.clone(),
    );
    message.cc = split_recipients(&fields.cc);
    message.bcc = split_recipients(&fields.bcc);
    message.in_reply_to = fields
        .in_reply_to
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    message.references = fields
        .references
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    message.attachments = load_compose_attachments(fields)?;
    Ok(message)
}

fn attachment_content_type(path: &Path) -> String {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("eml") => "message/rfc822",
        Some("txt") | Some("text") => "text/plain",
        Some("html") | Some("htm") => "text/html",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn save_selected_attachment(
    widgets: &Widgets,
    state: &SharedState,
    index: usize,
    dir: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let message = state
        .borrow()
        .selected_message
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no selected message"))?;
    let filename = message
        .filenames
        .first()
        .ok_or_else(|| anyhow::anyhow!("selected message has no file"))?;
    let attachments = extract_attachments_from_file(filename)?;
    let attachment = attachments
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("attachment index {index} not found"))?;
    let target_dir = dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("artifacts/attachments"));
    std::fs::create_dir_all(&target_dir)?;
    let path = target_dir.join(safe_filename(&attachment.filename));
    std::fs::write(&path, &attachment.bytes)?;
    widgets
        .status_label
        .set_text(&format!("Attachment saved to {}", path.display()));
    state.borrow_mut().last_operation = Some(format!(
        "saved attachment {} from {} to {}",
        attachment.filename,
        message.message_id,
        path.display()
    ));
    update_debug(widgets, state);
    Ok(path)
}

fn refresh_thread_attachment_list(widgets: &Widgets, state: &SharedState) {
    while let Some(child) = widgets.attachment_list.first_child() {
        widgets.attachment_list.remove(&child);
    }
    widgets.attachment_items.borrow_mut().clear();
    let messages = state.borrow().messages.clone();
    for (message_index, message) in messages.iter().enumerate() {
        let Some(filename) = message.filenames.first() else {
            continue;
        };
        let Ok(attachments) = extract_attachments_from_file(filename) else {
            continue;
        };
        for (attachment_index, attachment) in attachments.into_iter().enumerate() {
            let item = ThreadAttachmentItem {
                message_index,
                attachment_index,
                message_id: message.message_id.clone(),
                filename: attachment.filename,
                content_type: attachment.content_type,
                size: attachment.bytes.len(),
            };
            let row_index = widgets.attachment_items.borrow().len();
            let row = gtk::ListBoxRow::new();
            row.set_widget_name(&format!("notm-attachment-row-{row_index}"));
            let label = gtk::Label::new(Some(&format!(
                "Message {}: {} ({}, {} bytes)",
                item.message_index + 1,
                item.filename,
                item.content_type,
                item.size
            )));
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_margin_start(6);
            label.set_margin_end(6);
            label.set_margin_top(3);
            label.set_margin_bottom(3);
            row.set_child(Some(&label));
            connect_attachment_context_menu(widgets, state, &row, item.clone());
            widgets.attachment_list.append(&row);
            widgets.attachment_items.borrow_mut().push(item);
        }
    }
    let attachment_count = widgets.attachment_items.borrow().len();
    let has_attachments = attachment_count > 0;
    widgets.attachment_title.set_visible(has_attachments);
    widgets.attachment_scrolled.set_visible(has_attachments);
    if has_attachments {
        widgets.attachment_title.set_text(&format!(
            "{} attachment{} in thread",
            attachment_count,
            if attachment_count == 1 { "" } else { "s" }
        ));
        let visible_rows = attachment_count.min(4) as i32;
        let row_height = 34;
        let height = visible_rows * row_height;
        widgets.attachment_scrolled.set_min_content_height(height);
        widgets.attachment_scrolled.set_max_content_height(height);
    }
    if let Some(row) = widgets.attachment_list.row_at_index(0) {
        widgets.attachment_list.select_row(Some(&row));
    }
}

fn connect_attachment_context_menu(
    widgets: &Widgets,
    state: &SharedState,
    row: &gtk::ListBoxRow,
    item: ThreadAttachmentItem,
) {
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let save_button = gtk::Button::with_label("Save attachment");
    save_button.set_widget_name("notm-attachment-menu-save");
    let open_button = gtk::Button::with_label("Open attachment");
    open_button.set_widget_name("notm-attachment-menu-open");
    menu.append(&save_button);
    menu.append(&open_button);

    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    popover.set_child(Some(&menu));
    popover.set_parent(row);

    let w = widgets.clone();
    let st = state.clone();
    let save_item = item.clone();
    let save_popover = popover.clone();
    save_button.connect_clicked(move |_| {
        save_popover.popdown();
        if let Err(err) = save_thread_attachment(&w, &st, &save_item, None) {
            st.borrow_mut().last_error = Some(err.to_string());
            w.status_label
                .set_text(&format!("Save attachment failed: {err}"));
            update_debug(&w, &st);
        }
    });

    let w = widgets.clone();
    let st = state.clone();
    let open_popover = popover.clone();
    open_button.connect_clicked(move |_| {
        open_popover.popdown();
        match save_thread_attachment(&w, &st, &item, None) {
            Ok(path) => open_saved_attachment_path(&w, &st, path),
            Err(err) => {
                st.borrow_mut().last_error = Some(err.to_string());
                w.status_label
                    .set_text(&format!("Open attachment failed: {err}"));
                update_debug(&w, &st);
            }
        }
    });

    let click = gtk::GestureClick::new();
    click.set_button(3);
    let menu_popover = popover.clone();
    let menu_row = row.clone();
    click.connect_pressed(move |_, _, x, y| {
        if let Some(parent) = menu_row.parent()
            && let Ok(list) = parent.downcast::<gtk::ListBox>()
        {
            list.select_row(Some(&menu_row));
        }
        menu_popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        menu_popover.popup();
    });
    row.add_controller(click);
}

fn selected_thread_attachment(widgets: &Widgets) -> Option<ThreadAttachmentItem> {
    let index = widgets
        .attachment_list
        .selected_row()
        .map(|row| row.index() as usize)
        .unwrap_or(0);
    widgets.attachment_items.borrow().get(index).cloned()
}

fn save_thread_attachment(
    widgets: &Widgets,
    state: &SharedState,
    item: &ThreadAttachmentItem,
    dir: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let message = state
        .borrow()
        .messages
        .get(item.message_index)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("attachment message index not found"))?;
    let filename = message
        .filenames
        .first()
        .ok_or_else(|| anyhow::anyhow!("attachment message has no file"))?;
    let attachments = extract_attachments_from_file(filename)?;
    let attachment = attachments
        .get(item.attachment_index)
        .ok_or_else(|| anyhow::anyhow!("attachment index not found"))?;
    let target_dir = dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("artifacts/attachments"));
    std::fs::create_dir_all(&target_dir)?;
    let path = target_dir.join(safe_filename(&attachment.filename));
    std::fs::write(&path, &attachment.bytes)?;
    widgets
        .status_label
        .set_text(&format!("Attachment saved to {}", path.display()));
    {
        let mut state = state.borrow_mut();
        state.selected_message = Some(message.clone());
        state.last_operation = Some(format!(
            "saved thread attachment {} from message {} to {}",
            attachment.filename,
            message.message_id,
            path.display()
        ));
    }
    update_debug(widgets, state);
    Ok(path)
}

fn show_rendered_selected_thread(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    if state.borrow().selected_message.is_some() {
        show_selected_message_text_view(options, widgets, state);
        return;
    }
    let Some(thread_id) = state
        .borrow()
        .selected_thread
        .as_ref()
        .map(|thread| thread.thread_id.clone())
    else {
        widgets
            .status_label
            .set_text("No selected thread to render");
        return;
    };
    let index = state
        .borrow()
        .thread_list_items
        .iter()
        .position(|thread| thread.thread_id == thread_id)
        .or_else(|| selected_thread_index(widgets));
    if let Some(index) = index {
        open_thread_by_index(options, widgets, state, index);
    } else {
        widgets
            .status_label
            .set_text("Selected thread is not in the visible result list");
    }
}

fn show_preferred_selected_message_view(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) {
    if state.borrow().prefer_html_view && selected_message_has_html(state) {
        show_visual_html_selected_message(options, widgets, state);
    } else {
        show_selected_message_text_view(options, widgets, state);
    }
}

fn show_selected_message_text_view(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) {
    match render_selected_message_text(widgets, state) {
        Ok(rendered) => {
            show_text_message_view(options, widgets, state);
            set_active_message_view(widgets, MessageViewKind::Text);
            widgets.message_view.set_monospace(false);
            widgets.message_view.buffer().set_text(&rendered);
            let index = selected_message_index(state)
                .map(|index| index + 1)
                .unwrap_or(1);
            let total = state.borrow().messages.len().max(1);
            widgets
                .status_label
                .set_text(&format!("Showing message {index} of {total}"));
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Text view failed: {err}"));
        }
    }
    update_active_pane_visuals(widgets, state);
    update_debug(widgets, state);
}

fn render_selected_message_text(widgets: &Widgets, state: &SharedState) -> anyhow::Result<String> {
    let message = state
        .borrow()
        .selected_message
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no selected message"))?;
    let mut rendered = String::new();
    if let Some(path) = message.filenames.first() {
        match parse_file(path) {
            Ok(parsed) => {
                rendered.push_str(&render_body_with_quote_collapse(
                    &parsed.safe_body,
                    widgets.quote_collapse.get(),
                ));
                if !parsed.attachments.is_empty() {
                    rendered.push_str("\n\nAttachments:\n");
                    for att in &parsed.attachments {
                        rendered.push_str(&format!(
                            "- {} ({}, {} bytes)\n",
                            att.filename
                                .clone()
                                .unwrap_or_else(|| "unnamed".to_string()),
                            att.content_type,
                            att.size
                        ));
                    }
                }
                rendered.push_str("\n\nMIME tree:\n");
                for node in parsed.mime_tree {
                    rendered.push_str(&format!("  {node}\n"));
                }
            }
            Err(err) => rendered.push_str(&format!("Could not parse body: {err}\n")),
        }
    }
    Ok(rendered)
}

fn selected_message_index(state: &SharedState) -> Option<usize> {
    let state = state.borrow();
    let selected_id = &state.selected_message.as_ref()?.message_id;
    state
        .messages
        .iter()
        .position(|message| &message.message_id == selected_id)
}

fn update_message_header(widgets: &Widgets, state: &SharedState) {
    let Some(message) = state.borrow().selected_message.clone() else {
        widgets.message_header_label.set_visible(false);
        widgets.message_header_label.set_text("");
        return;
    };
    let index = selected_message_index(state)
        .map(|index| index + 1)
        .unwrap_or(1);
    let total = state.borrow().messages.len().max(1);
    widgets.message_header_label.set_text(&format!(
        "Message {index} of {total}\nFrom: {}\nTo: {}\nCc: {}\nSubject: {}\nDate: {}\nTags: {}\nMessage-ID: {}\nFilenames: {}",
        message.from,
        message.to,
        message.cc,
        message.subject,
        format_message_date(message.date),
        message.tags.join(" "),
        message.message_id,
        message.filenames.join(", ")
    ));
    widgets.message_header_label.set_visible(true);
}

fn format_message_date(timestamp: i64) -> String {
    chrono::DateTime::<Utc>::from_timestamp(timestamp, 0)
        .map(|date| date.to_rfc2822())
        .unwrap_or_else(|| timestamp.to_string())
}

fn set_active_message_view(widgets: &Widgets, active: MessageViewKind) {
    for button in [
        &widgets.view_text_button,
        &widgets.view_html_button,
        &widgets.view_headers_button,
        &widgets.view_raw_button,
    ] {
        button.remove_css_class("suggested-action");
    }
    match active {
        MessageViewKind::Text => widgets.view_text_button.add_css_class("suggested-action"),
        MessageViewKind::Html => widgets.view_html_button.add_css_class("suggested-action"),
        MessageViewKind::Headers => widgets
            .view_headers_button
            .add_css_class("suggested-action"),
        MessageViewKind::Raw => widgets.view_raw_button.add_css_class("suggested-action"),
    }
}

fn toggle_text_visual_view(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let scroll = current_message_scroll_fraction(widgets);
    if html_view_is_visible(widgets) {
        state.borrow_mut().prefer_html_view = false;
        show_rendered_selected_thread(options, widgets, state);
    } else {
        state.borrow_mut().prefer_html_view = true;
        show_visual_html_selected_message(options, widgets, state);
    }
    restore_message_scroll_fraction(widgets, scroll);
}

fn activate_image_policy_button(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    if selected_message_allows_images(options, state) {
        update_message_action_buttons(options, widgets, state);
        return;
    }
    if html_view_is_visible(widgets) && html_view_images_allowed(widgets) {
        show_visual_html_with_image_policy(options, widgets, state, ImagePolicy::TrustSender);
    } else {
        show_visual_html_with_image_policy(options, widgets, state, ImagePolicy::Once);
    }
}

fn update_message_action_buttons(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let html_visible = html_view_is_visible(widgets);
    let has_html = selected_message_has_html(state);
    let (has_message, selected_thread, message_count) = {
        let state = state.borrow();
        (
            state.selected_message.is_some(),
            state.selected_thread.clone(),
            state.messages.len(),
        )
    };
    let has_thread = selected_thread.is_some();
    let multiple_messages = message_count > 1;
    if !has_message {
        widgets.message_header_label.set_visible(false);
    }
    widgets.message_menu_button.set_visible(multiple_messages);
    widgets
        .collapse_quotes_button
        .set_visible(multiple_messages);
    widgets.response_menu_button.set_sensitive(has_message);
    widgets.read_toggle_button.set_sensitive(has_thread);
    widgets.flag_toggle_button.set_sensitive(has_thread);
    let tag_targets = tag_target_threads(state);
    if !tag_targets.is_empty() {
        widgets.read_toggle_button.set_label(
            if tag_targets.iter().any(|thread| thread.has_unread) {
                "Mark read"
            } else {
                "Mark unread"
            },
        );
        widgets.flag_toggle_button.set_label(
            if tag_targets.iter().any(|thread| thread.is_flagged) {
                "Unflag"
            } else {
                "Flag"
            },
        );
    } else {
        widgets.read_toggle_button.set_label("Mark read");
        widgets.flag_toggle_button.set_label("Flag");
    }
    widgets
        .html_policy_row
        .set_visible(html_visible && has_html);
    widgets
        .image_policy_button
        .set_visible(html_visible && has_html);
    widgets.message_menu_button.set_sensitive(has_thread);
    widgets
        .view_menu_button
        .set_sensitive(has_message || has_thread);
    widgets
        .view_text_button
        .set_sensitive(has_message || has_thread);
    widgets.view_html_button.set_visible(has_html);
    widgets.view_html_button.set_sensitive(has_html);
    widgets.view_headers_button.set_sensitive(has_message);
    widgets.view_raw_button.set_sensitive(has_message);
    widgets
        .copy_menu_button
        .set_sensitive(has_message || has_thread);
    widgets.copy_message_id_button.set_sensitive(has_message);
    widgets.copy_thread_id_button.set_sensitive(has_thread);
    widgets.copy_from_email_button.set_sensitive(has_message);
    widgets.copy_to_email_button.set_sensitive(has_message);
    widgets.copy_cc_email_button.set_sensitive(has_message);
    widgets.copy_subject_button.set_sensitive(has_message);
    if html_visible && has_html {
        let image_policy = if html_view_images_allowed(widgets) {
            if selected_message_allows_images(options, state) {
                "remote images allowed"
            } else {
                "remote images loaded for this view"
            }
        } else {
            "remote images blocked"
        };
        widgets.html_policy_label.set_text(&format!(
            "Sanitized HTML view: message JavaScript disabled; {image_policy}; links open externally."
        ));
    }

    if !has_html {
        widgets.image_policy_button.set_label("Load images once");
        widgets.image_policy_button.set_sensitive(false);
        update_button_binding_labels(widgets, state);
        return;
    }

    if selected_message_allows_images(options, state) {
        let sender = selected_sender_email(state);
        let sender_trusted = sender
            .as_deref()
            .is_some_and(|sender| image_sender_is_trusted(state, sender));
        widgets.image_policy_button.set_label(if sender_trusted {
            "Images trusted"
        } else {
            "Images allowed"
        });
        widgets.image_policy_button.set_sensitive(false);
    } else if html_visible && html_view_images_allowed(widgets) {
        widgets.image_policy_button.set_label("Trust sender images");
        widgets
            .image_policy_button
            .set_sensitive(selected_sender_email(state).is_some());
    } else {
        widgets.image_policy_button.set_label("Load images once");
        widgets.image_policy_button.set_sensitive(true);
    }
    update_button_binding_labels(widgets, state);
}

fn html_view_is_visible(widgets: &Widgets) -> bool {
    widgets
        .message_stack
        .visible_child_name()
        .is_some_and(|name| name.as_str() == "html")
}

fn compose_view_is_visible(widgets: &Widgets) -> bool {
    widgets
        .message_stack
        .visible_child_name()
        .is_some_and(|name| name.as_str() == "compose")
}

fn html_view_images_allowed(widgets: &Widgets) -> bool {
    WebViewExt::settings(&widgets.html_view)
        .map(|settings| settings.is_auto_load_images())
        .unwrap_or(false)
}

fn selected_message_has_html(state: &SharedState) -> bool {
    selected_message_filename(state)
        .and_then(parse_file)
        .ok()
        .and_then(|parsed| parsed.html_body)
        .is_some_and(|html| !html.trim().is_empty())
}

fn open_selected_attachment(widgets: &Widgets, state: &SharedState, index: usize) {
    match save_selected_attachment(widgets, state, index, None) {
        Ok(path) => {
            let file = gtk::gio::File::for_path(&path);
            match gtk::gio::AppInfo::launch_default_for_uri(
                &file.uri(),
                None::<&gtk::gio::AppLaunchContext>,
            ) {
                Ok(()) => {
                    widgets
                        .status_label
                        .set_text(&format!("Opened attachment {}", path.display()));
                    state.borrow_mut().last_operation =
                        Some(format!("opened attachment {}", path.display()));
                }
                Err(err) => {
                    widgets
                        .status_label
                        .set_text(&format!("Open attachment failed: {err}"));
                    state.borrow_mut().last_error = Some(err.to_string());
                }
            }
            update_debug(widgets, state);
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Open attachment failed: {err}"));
            update_debug(widgets, state);
        }
    }
}

fn open_saved_attachment_path(widgets: &Widgets, state: &SharedState, path: PathBuf) {
    let file = gtk::gio::File::for_path(&path);
    match gtk::gio::AppInfo::launch_default_for_uri(
        &file.uri(),
        None::<&gtk::gio::AppLaunchContext>,
    ) {
        Ok(()) => {
            widgets
                .status_label
                .set_text(&format!("Opened attachment {}", path.display()));
            state.borrow_mut().last_operation =
                Some(format!("opened attachment {}", path.display()));
        }
        Err(err) => {
            widgets
                .status_label
                .set_text(&format!("Open attachment failed: {err}"));
            state.borrow_mut().last_error = Some(err.to_string());
        }
    }
    update_debug(widgets, state);
}

fn show_raw_source(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let scroll = current_message_scroll_fraction(widgets);
    let result = (|| -> anyhow::Result<String> {
        let filename = selected_message_filename(state)?;
        Ok(std::fs::read_to_string(filename)?)
    })();
    match result {
        Ok(raw) => {
            show_text_message_view(options, widgets, state);
            set_active_message_view(widgets, MessageViewKind::Raw);
            widgets.message_view.set_monospace(true);
            widgets.message_view.buffer().set_text(&raw);
            restore_message_scroll_fraction(widgets, scroll);
            widgets.status_label.set_text("Raw message source shown");
            state.borrow_mut().last_operation = Some("showed raw source".to_string());
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Raw source failed: {err}"));
        }
    }
    update_debug(widgets, state);
}

fn show_full_headers(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let scroll = current_message_scroll_fraction(widgets);
    let result = (|| -> anyhow::Result<String> {
        let filename = selected_message_filename(state)?;
        let raw = std::fs::read_to_string(filename)?;
        Ok(header_block(&raw))
    })();
    match result {
        Ok(headers) => {
            show_text_message_view(options, widgets, state);
            set_active_message_view(widgets, MessageViewKind::Headers);
            widgets.message_view.set_monospace(true);
            widgets.message_view.buffer().set_text(&headers);
            restore_message_scroll_fraction(widgets, scroll);
            widgets.status_label.set_text("Full message headers shown");
            state.borrow_mut().last_operation = Some("showed full headers".to_string());
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Full headers failed: {err}"));
        }
    }
    update_debug(widgets, state);
}

fn header_block(raw: &str) -> String {
    if let Some((headers, _)) = raw.split_once("\r\n\r\n") {
        headers.to_string()
    } else if let Some((headers, _)) = raw.split_once("\n\n") {
        headers.to_string()
    } else {
        raw.to_string()
    }
}

fn toggle_quote_collapse(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let enabled = !widgets.quote_collapse.get();
    widgets.quote_collapse.set(enabled);
    state.borrow_mut().quote_collapse_enabled = enabled;
    show_rendered_selected_thread(options, widgets, state);
    widgets.status_label.set_text(if enabled {
        "Quote collapse enabled"
    } else {
        "Quote collapse disabled"
    });
    update_debug(widgets, state);
}

fn render_body_with_quote_collapse(body: &str, collapse_quotes: bool) -> String {
    if !collapse_quotes {
        return body.to_string();
    }
    let mut out = Vec::new();
    let mut in_quote = false;
    let mut collapsed_count = 0_usize;
    for line in body.lines() {
        if line.trim_start().starts_with('>') {
            if !in_quote {
                out.push("[quoted text collapsed]".to_string());
                in_quote = true;
            }
            collapsed_count += 1;
        } else {
            in_quote = false;
            out.push(line.to_string());
        }
    }
    if collapsed_count == 0 {
        body.to_string()
    } else {
        out.join("\n")
    }
}

fn text_view_text(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string()
}

fn selected_message_filename(state: &SharedState) -> anyhow::Result<String> {
    state
        .borrow()
        .selected_message
        .as_ref()
        .and_then(|message| message.filenames.first().cloned())
        .ok_or_else(|| anyhow::anyhow!("selected message has no file"))
}

fn selected_message_is_draft(options: &LaunchOptions, state: &SharedState) -> bool {
    state
        .borrow()
        .selected_message
        .as_ref()
        .is_some_and(|message| is_draft_message(options, message))
}

fn is_draft_message(options: &LaunchOptions, message: &notm_notmuch::MessageSummary) -> bool {
    let draft_tags = if options.draft_tags.is_empty() {
        vec!["draft".to_string()]
    } else {
        options.draft_tags.clone()
    };
    draft_tags
        .iter()
        .any(|draft_tag| message.tags.iter().any(|tag| tag == draft_tag))
}

fn open_selected_draft_message(widgets: &Widgets, state: &SharedState) -> anyhow::Result<()> {
    let message = state
        .borrow()
        .selected_message
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no selected draft message"))?;
    let filename = message
        .filenames
        .first()
        .ok_or_else(|| anyhow::anyhow!("selected draft has no file"))?;
    let fields = draft_fields_from_message_file(filename)?;
    apply_compose_fields(widgets, state, fields.clone());
    set_active_draft(
        widgets,
        state,
        Some(ActiveDraft {
            path: PathBuf::from(filename),
            message_id: Some(message.message_id.clone()),
            indexed: true,
            saved_fields: fields,
        }),
    );
    show_compose_view(widgets);
    {
        let mut state = state.borrow_mut();
        state.active_pane = ActivePane::Message;
        state.last_operation = Some(format!("opened draft {} for editing", message.message_id));
    }
    if state.borrow().input_mode == InputMode::Insert {
        widgets.compose_to.grab_focus();
    } else {
        focus_active_pane(widgets, state);
    }
    Ok(())
}

fn draft_fields_from_message_file(path: impl AsRef<Path>) -> anyhow::Result<ComposeFields> {
    let path = path.as_ref();
    let parsed = parse_file(path)?;
    let attachment_inputs = extract_attachments_from_file(path)?
        .into_iter()
        .map(|attachment| AttachmentInput {
            filename: attachment.filename,
            content_type: attachment.content_type,
            bytes: attachment.bytes,
            source_path: None,
        })
        .collect::<Vec<_>>();
    let attachments = cache_composer_attachments(&attachment_inputs)?;
    let body = if parsed.text_body.trim().is_empty() {
        parsed.safe_body
    } else {
        parsed.text_body
    };
    Ok(ComposeFields {
        from: parsed.from,
        to: parsed.to,
        cc: parsed.cc,
        bcc: header_value(&parsed.headers, "Bcc"),
        subject: parsed.subject,
        body,
        attachments,
        in_reply_to: nonempty_string(parsed.in_reply_to),
        references: references_from_header(&parsed.references),
    })
}

fn header_value(headers: &BTreeMap<String, String>, name: &str) -> String {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
        .unwrap_or_default()
}

fn nonempty_string(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn references_from_header(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn show_text_message_view(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    widgets.message_stack.set_visible_child_name("text");
    update_message_header(widgets, state);
    refresh_thread_attachment_list(widgets, state);
    update_message_action_buttons(options, widgets, state);
}

fn show_compose_view(widgets: &Widgets) {
    widgets.address_suggestions_list.set_visible(false);
    widgets.html_policy_row.set_visible(false);
    widgets.message_header_label.set_visible(false);
    widgets.attachment_title.set_visible(false);
    widgets.attachment_scrolled.set_visible(false);
    widgets.message_stack.set_visible_child_name("compose");
}

fn configure_html_webview(view: &webkit6::WebView, allow_remote_images: bool) {
    if let Some(settings) = WebViewExt::settings(view) {
        settings.set_enable_javascript(true);
        settings.set_enable_javascript_markup(false);
        settings.set_enable_developer_extras(false);
        settings.set_allow_file_access_from_file_urls(false);
        settings.set_allow_universal_access_from_file_urls(false);
        settings.set_auto_load_images(allow_remote_images);
    }
    view.load_html(
        &visual_html_document(
            "<p class=\"notm-empty-html\">Open an HTML message and choose Visual HTML.</p>",
        ),
        Some("about:blank"),
    );
}

fn connect_html_navigation_policy(view: &webkit6::WebView, status_label: &gtk::Label) {
    let status = status_label.clone();
    view.connect_decide_policy(move |_, decision, decision_type| {
        if matches!(
            decision_type,
            PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction
        ) {
            let uri = navigation_decision_uri(decision);
            if let Some(uri) = uri.as_deref()
                && !uri.is_empty()
                && uri != "about:blank"
            {
                decision.ignore();
                open_html_link_externally(uri, &status);
                return true;
            }
        }
        false
    });
}

fn open_html_link_externally(uri: &str, status_label: &gtk::Label) {
    if !html_link_scheme_is_external_safe(uri) {
        status_label.set_text(&format!("Blocked unsupported HTML link target: {uri}"));
        return;
    }
    match gtk::gio::AppInfo::launch_default_for_uri(uri, None::<&gtk::gio::AppLaunchContext>) {
        Ok(()) => status_label.set_text(&format!("Opened link externally: {uri}")),
        Err(err) => status_label.set_text(&format!("Open link failed: {err}; target: {uri}")),
    }
}

fn html_link_scheme_is_external_safe(uri: &str) -> bool {
    let Some((scheme, _)) = uri.split_once(':') else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto"
    )
}

fn navigation_decision_uri(decision: &webkit6::PolicyDecision) -> Option<String> {
    let navigation = decision.downcast_ref::<NavigationPolicyDecision>()?;
    let action = navigation.navigation_action()?;
    let request = action.request()?;
    request.uri().map(|uri| uri.to_string())
}

#[derive(Debug, Clone, Copy)]
enum ImagePolicy {
    Config,
    Once,
    TrustSender,
}

fn show_visual_html_selected_message(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) {
    show_visual_html_with_image_policy(options, widgets, state, ImagePolicy::Config);
}

fn show_visual_html_with_image_policy(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    image_policy: ImagePolicy,
) {
    let result = (|| -> anyhow::Result<(String, String, bool, Option<String>)> {
        let sender = selected_sender_email(state);
        if matches!(image_policy, ImagePolicy::TrustSender) {
            let sender = sender
                .clone()
                .ok_or_else(|| anyhow::anyhow!("selected message sender could not be parsed"))?;
            trust_image_sender(options, state, &sender)?;
        }
        let allow_remote_images = match image_policy {
            ImagePolicy::Config => selected_message_allows_images(options, state),
            ImagePolicy::Once | ImagePolicy::TrustSender => true,
        };
        let filename = selected_message_filename(state)?;
        let parsed = parse_file(filename)?;
        let html = parsed
            .html_body
            .ok_or_else(|| anyhow::anyhow!("selected message has no HTML body"))?;
        let sanitized = sanitize_html_for_visual(&html, allow_remote_images);
        Ok((
            visual_html_document(&sanitized),
            html,
            allow_remote_images,
            sender,
        ))
    })();
    match result {
        Ok((document, original_html, allow_remote_images, sender)) => {
            set_html_image_loading(&widgets.html_view, allow_remote_images);
            widgets.html_view.load_html(&document, Some("about:blank"));
            widgets.message_stack.set_visible_child_name("html");
            update_message_header(widgets, state);
            set_active_message_view(widgets, MessageViewKind::Html);
            widgets.status_label.set_text(&html_status_text(
                image_policy,
                allow_remote_images,
                sender.as_deref(),
            ));
            {
                let mut s = state.borrow_mut();
                s.last_operation = Some(format!(
                    "showed visual HTML ({} bytes before sanitization, images={})",
                    original_html.len(),
                    if allow_remote_images {
                        "allowed"
                    } else {
                        "blocked"
                    }
                ));
                s.last_error = None;
            }
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Visual HTML failed: {err}"));
        }
    }
    update_message_action_buttons(options, widgets, state);
    update_debug(widgets, state);
}

fn set_html_image_loading(view: &webkit6::WebView, allow_remote_images: bool) {
    if let Some(settings) = WebViewExt::settings(view) {
        settings.set_auto_load_images(allow_remote_images);
    }
}

fn html_status_text(
    policy: ImagePolicy,
    allow_remote_images: bool,
    sender: Option<&str>,
) -> String {
    match policy {
        ImagePolicy::Once if allow_remote_images => {
            "Visual HTML rendered; remote images allowed for this view only".to_string()
        }
        ImagePolicy::TrustSender if allow_remote_images => format!(
            "Visual HTML rendered; remote images always allowed for {}",
            sender.unwrap_or("this sender")
        ),
        ImagePolicy::Config if allow_remote_images => match sender {
            Some(sender) if !sender.is_empty() => {
                format!("Visual HTML rendered; remote images allowed by config/trust for {sender}")
            }
            _ => "Visual HTML rendered; remote images allowed by config".to_string(),
        },
        _ => "Visual HTML rendered; JavaScript and remote images disabled".to_string(),
    }
}

fn selected_message_allows_images(options: &LaunchOptions, state: &SharedState) -> bool {
    options.remote_images
        || selected_sender_email(state)
            .as_deref()
            .is_some_and(|sender| image_sender_is_trusted(state, sender))
}

fn selected_sender_email(state: &SharedState) -> Option<String> {
    state
        .borrow()
        .selected_message
        .as_ref()
        .and_then(|message| sender_email_from_header(&message.from))
}

fn sender_email_from_header(value: &str) -> Option<String> {
    parse_address_list(value)
        .into_iter()
        .next()
        .map(|address| normalize_sender(&address.email))
}

fn normalize_sender(sender: &str) -> String {
    sender.trim().to_ascii_lowercase()
}

fn normalize_sender_list(senders: &[String]) -> Vec<String> {
    let mut senders = senders
        .iter()
        .map(|sender| normalize_sender(sender))
        .filter(|sender| !sender.is_empty())
        .collect::<Vec<_>>();
    senders.sort();
    senders.dedup();
    senders
}

fn image_sender_is_trusted(state: &SharedState, sender: &str) -> bool {
    let sender = normalize_sender(sender);
    state
        .borrow()
        .trusted_image_senders
        .iter()
        .any(|trusted| trusted == &sender)
}

fn trust_image_sender(
    options: &LaunchOptions,
    state: &SharedState,
    sender: &str,
) -> anyhow::Result<()> {
    let sender = normalize_sender(sender);
    anyhow::ensure!(!sender.is_empty(), "sender is empty");
    {
        let mut state = state.borrow_mut();
        if !state.trusted_image_senders.iter().any(|s| s == &sender) {
            state.trusted_image_senders.push(sender.clone());
            state.trusted_image_senders.sort();
        }
    }
    persist_trusted_image_senders(options, &state.borrow().trusted_image_senders)?;
    Ok(())
}

fn sanitize_html_for_visual(html: &str, allow_remote_images: bool) -> String {
    let sanitized = sanitize_html(html);
    if allow_remote_images {
        sanitized
    } else {
        strip_img_tags(&sanitized)
    }
}

fn strip_img_tags(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut pos = 0;
    while let Some(relative_start) = lower[pos..].find("<img") {
        let start = pos + relative_start;
        let next = lower[start + 4..].chars().next();
        let is_img_tag = match next {
            None | Some('>') | Some('/') => true,
            Some(ch) => ch.is_ascii_whitespace(),
        };
        if !is_img_tag {
            out.push_str(&html[pos..start + 4]);
            pos = start + 4;
            continue;
        }
        out.push_str(&html[pos..start]);
        if let Some(relative_end) = lower[start..].find('>') {
            out.push_str("<span class=\"notm-blocked-image\">[image blocked]</span>");
            pos = start + relative_end + 1;
        } else {
            pos = html.len();
            break;
        }
    }
    out.push_str(&html[pos..]);
    out
}

fn visual_html_document(body: &str) -> String {
    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="color-scheme" content="light">
<style>
:root {{
  color-scheme: light;
  font: 15px system-ui, sans-serif;
  background: #ffffff;
  color: #111111;
}}
body {{
  margin: 0;
  padding: 16px;
  line-height: 1.45;
  overflow-wrap: anywhere;
  background: #ffffff;
  color: #111111;
}}
.notm-blocked-image {{
  display: inline;
  margin: 0;
  padding: 0;
  background: transparent;
  color: #666666;
  font-size: 12px;
  font-style: italic;
}}
a {{ color: #1155cc; }}
pre, code {{
  font-family: ui-monospace, monospace;
  white-space: pre-wrap;
}}
blockquote {{
  margin-inline-start: 0.8em;
  padding-inline-start: 0.8em;
  color: #555555;
}}
table {{
  border-collapse: collapse;
  max-width: 100%;
}}
td, th {{
  border: 0;
  padding: 0;
  vertical-align: top;
}}
</style>
</head>
<body>
{body}
</body>
</html>"#
    )
}

fn html_view_state(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> serde_json::Value {
    let visible_child = widgets
        .message_stack
        .visible_child_name()
        .map(|name| name.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let (has_html, html_len, error) = match selected_message_filename(state).and_then(parse_file) {
        Ok(parsed) => (
            parsed
                .html_body
                .as_ref()
                .is_some_and(|html| !html.trim().is_empty()),
            parsed
                .html_body
                .as_ref()
                .map(|html| html.len())
                .unwrap_or(0),
            None,
        ),
        Err(err) => (false, 0, Some(err.to_string())),
    };
    let sender_email = selected_sender_email(state);
    let sender_trusted = sender_email
        .as_deref()
        .is_some_and(|sender| image_sender_is_trusted(state, sender));
    let image_loading_allowed = WebViewExt::settings(&widgets.html_view)
        .map(|settings| settings.is_auto_load_images())
        .unwrap_or(false);
    json!({
        "ok": error.is_none(),
        "visible_child": visible_child,
        "html_visible": visible_child == "html",
        "has_html": has_html,
        "html_bytes": html_len,
        "global_remote_images_allowed": options.remote_images,
        "sender_email": sender_email,
        "sender_trusted": sender_trusted,
        "policy_allows_images": selected_message_allows_images(options, state),
        "image_loading_allowed": image_loading_allowed,
        "remote_images_allowed": image_loading_allowed,
        "error": error,
    })
}

fn copy_selected_message_id(widgets: &Widgets, state: &SharedState) {
    let Some(message_id) = state
        .borrow()
        .selected_message
        .as_ref()
        .map(|message| message.message_id.clone())
    else {
        widgets
            .status_label
            .set_text("No selected message id to copy");
        return;
    };
    copy_to_clipboard(&message_id);
    widgets.status_label.set_text("Copied message id");
    state.borrow_mut().last_operation = Some("copied message id".to_string());
    update_debug(widgets, state);
}

fn copy_selected_thread_id(widgets: &Widgets, state: &SharedState) {
    let Some(thread_id) = state
        .borrow()
        .selected_thread
        .as_ref()
        .map(|thread| thread.thread_id.clone())
    else {
        widgets
            .status_label
            .set_text("No selected thread id to copy");
        return;
    };
    copy_to_clipboard(&thread_id);
    widgets.status_label.set_text("Copied thread id");
    state.borrow_mut().last_operation = Some("copied thread id".to_string());
    update_debug(widgets, state);
}

#[derive(Debug, Clone, Copy)]
enum MessageEmailField {
    From,
    To,
    Cc,
}

fn copy_selected_message_emails(widgets: &Widgets, state: &SharedState, field: MessageEmailField) {
    let value = {
        let state = state.borrow();
        let Some(message) = state.selected_message.as_ref() else {
            widgets
                .status_label
                .set_text("No selected message to copy from");
            return;
        };
        match field {
            MessageEmailField::From => header_emails(&message.from),
            MessageEmailField::To => header_emails(&message.to),
            MessageEmailField::Cc => header_emails(&message.cc),
        }
    };
    if value.trim().is_empty() {
        widgets
            .status_label
            .set_text("Selected message field is empty");
        return;
    }
    copy_to_clipboard(&value);
    let label = match field {
        MessageEmailField::From => "from email",
        MessageEmailField::To => "to email",
        MessageEmailField::Cc => "cc email",
    };
    widgets.status_label.set_text(&format!("Copied {label}"));
    state.borrow_mut().last_operation = Some(format!("copied {label}"));
    update_debug(widgets, state);
}

fn copy_selected_message_subject(widgets: &Widgets, state: &SharedState) {
    let Some(subject) = state
        .borrow()
        .selected_message
        .as_ref()
        .map(|message| message.subject.clone())
        .filter(|subject| !subject.trim().is_empty())
    else {
        widgets
            .status_label
            .set_text("No selected message subject to copy");
        return;
    };
    copy_to_clipboard(&subject);
    widgets.status_label.set_text("Copied subject");
    state.borrow_mut().last_operation = Some("copied subject".to_string());
    update_debug(widgets, state);
}

fn header_emails(value: &str) -> String {
    let emails = parse_address_list(value)
        .into_iter()
        .map(|address| address.email)
        .filter(|email| !email.trim().is_empty())
        .collect::<Vec<_>>();
    if emails.is_empty() {
        value.trim().to_string()
    } else {
        emails.join(", ")
    }
}

fn copy_to_clipboard(text: &str) {
    if let Some(display) = gtk::gdk::Display::default() {
        display.clipboard().set_text(text);
    }
}

fn safe_filename(filename: &str) -> String {
    let cleaned = filename
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | '\0' => '_',
            _ => ch,
        })
        .collect::<String>();
    if cleaned.trim().is_empty() {
        "attachment.bin".to_string()
    } else {
        cleaned
    }
}

#[allow(clippy::too_many_arguments)]
fn connect_actions(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    search_button: &gtk::Button,
    archive_button: &gtk::Button,
    read_button: &gtk::Button,
    flag_button: &gtk::Button,
    trash_button: &gtk::Button,
    spam_button: &gtk::Button,
    undo_last_button: &gtk::Button,
    undo_list_button: &gtk::Button,
    compose_button: &gtk::Button,
    reply_button: &gtk::Button,
    reply_all_button: &gtk::Button,
    forward_button: &gtk::Button,
    forward_attachment_button: &gtk::Button,
    debug_button: &gtk::Button,
    palette_button: &gtk::Button,
    settings_button: &gtk::Button,
    help_button: &gtk::Button,
    send_button: &gtk::Button,
) {
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    search_button.connect_clicked(move |_| {
        let query = w.search_entry.text().to_string();
        run_search(&opts, &w, &st, &query);
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets
        .load_more_button
        .connect_clicked(move |_| load_more_threads(&opts, &w, &st));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets.search_entry.connect_activate(move |entry| {
        run_search(&opts, &w, &st, &entry.text());
    });

    let w = widgets.clone();
    let st = state.clone();
    let opts = options.clone();
    widgets.thread_list.connect_row_activated(move |_, row| {
        open_thread_by_index(&opts, &w, &st, row.index() as usize);
    });

    let w = widgets.clone();
    let st = state.clone();
    let opts = options.clone();
    widgets.thread_list.connect_row_selected(move |_, row| {
        if let Some(row) = row {
            select_thread_by_index(&opts, &w, &st, row.index() as usize, false);
        }
    });

    connect_tag_button(
        archive_button,
        options,
        widgets,
        state,
        undo_state,
        &[],
        &["inbox"],
    );
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    read_button.connect_clicked(move |_| toggle_unread_selected(&opts, &w, &st, &undo));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    flag_button.connect_clicked(move |_| toggle_flagged_selected(&opts, &w, &st, &undo));

    connect_tag_button(
        trash_button,
        options,
        widgets,
        state,
        undo_state,
        &["trash"],
        &["inbox", "spam"],
    );
    connect_tag_button(
        spam_button,
        options,
        widgets,
        state,
        undo_state,
        &["spam"],
        &["inbox", "trash"],
    );

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    undo_last_button.connect_clicked(move |_| undo_last_tag(&opts, &w, &st, &undo));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    undo_list_button.connect_clicked(move |_| show_undo_tag_actions(&opts, &w, &st, &undo));

    let w = widgets.clone();
    let st = state.clone();
    compose_button.connect_clicked(move |_| open_compose(&w, &st));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    reply_button.connect_clicked(move |_| reply_selected(&opts, &w, &st, ReplyKind::Sender));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    reply_all_button.connect_clicked(move |_| reply_selected(&opts, &w, &st, ReplyKind::All));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    forward_button.connect_clicked(move |_| forward_selected(&opts, &w, &st));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    forward_attachment_button.connect_clicked(move |_| {
        forward_as_attachment_selected(&opts, &w, &st);
    });

    let w = widgets.clone();
    let st = state.clone();
    debug_button.connect_clicked(move |_| {
        w.debug_view.set_visible(!w.debug_view.is_visible());
        update_debug(&w, &st);
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    palette_button.connect_clicked(move |_| show_command_palette(&opts, &w, &st, &undo));

    let w = widgets.clone();
    let opts = options.clone();
    settings_button.connect_clicked(move |_| show_settings(&w, &opts));

    let w = widgets.clone();
    help_button.connect_clicked(move |_| show_shortcuts_overlay(&w));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    send_button.connect_clicked(move |_| send_compose(&opts, &w, &st));
}

fn connect_tag_button(
    button: &gtk::Button,
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    add: &[&str],
    remove: &[&str],
) {
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    let add = add.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let remove = remove.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    button.connect_clicked(move |_| {
        let mutation = TagMutation {
            add: add.clone(),
            remove: remove.clone(),
            sync_maildir_flags: opts.sync_maildir_flags_after_tag_change,
        };
        tag_selected(&opts, &w, &st, &undo, mutation);
    });
}

fn run_search(options: &LaunchOptions, widgets: &Widgets, state: &SharedState, query: &str) {
    widgets
        .search_generation
        .set(widgets.search_generation.get().saturating_add(1));
    match execute_search_page(options, query, 0) {
        Ok(data) => apply_search_data(options, widgets, state, data),
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Search failed: {err}"));
            update_debug(widgets, state);
        }
    }
}

fn run_search_async(options: &LaunchOptions, widgets: &Widgets, state: &SharedState, query: &str) {
    let generation = widgets.search_generation.get().saturating_add(1);
    widgets.search_generation.set(generation);
    widgets
        .status_label
        .set_text(&format!("Loading search `{query}`…"));
    widgets.thread_result_label.set_text("Loading search…");
    widgets.load_more_button.set_sensitive(false);

    let (tx, rx) = mpsc::channel::<SearchResponse>();
    let opts = options.clone();
    let query = query.to_string();
    thread::spawn(move || {
        let result = execute_search_page(&opts, &query, 0);
        let _ = tx.send(SearchResponse { generation, result });
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(response) => {
            if response.generation == w.search_generation.get() {
                match response.result {
                    Ok(data) => apply_search_data(&opts, &w, &st, data),
                    Err(err) => apply_search_error(&w, &st, err),
                }
            } else {
                st.borrow_mut().last_operation = Some(format!(
                    "discarded stale search generation {}",
                    response.generation
                ));
            }
            gtk::glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            apply_search_error(&w, &st, anyhow::anyhow!("search cancelled"));
            gtk::glib::ControlFlow::Break
        }
    });
}

fn execute_search(options: &LaunchOptions, query: &str) -> anyhow::Result<SearchData> {
    execute_search_page(options, query, 0)
}

fn execute_search_page(
    options: &LaunchOptions,
    query: &str,
    offset: usize,
) -> anyhow::Result<SearchData> {
    let db = Database::open(&open_config(options), DatabaseMode::ReadOnly)?;
    let revision = db.revision();
    let db_path = db.path();
    let key = search_cache_key(options, query, &db_path, &revision, offset);
    if let Some(mut cached) = SEARCH_CACHE
        .get_or_init(Default::default)
        .lock()
        .expect("search cache lock")
        .get(&key)
        .cloned()
    {
        cached.cached = true;
        return Ok(cached);
    }
    let tags = db.all_tags();
    let opts = QueryOptions {
        limit: options.page_size,
        offset,
        sort: SortOrder::NewestFirst,
        excluded_tags: options.excluded_tags.clone(),
    };
    let threads = db.search_threads(query, &opts)?;
    let count = db
        .count_threads(query, &opts)
        .unwrap_or(threads.len() as u32);
    let details = thread_details_for_threads(&db, &db_path, &revision, &threads);
    let data = SearchData {
        query: query.to_string(),
        threads,
        details,
        count,
        offset,
        limit: options.page_size,
        tags,
        database_path: db_path,
        revision,
        cached: false,
    };
    SEARCH_CACHE
        .get_or_init(Default::default)
        .lock()
        .expect("search cache lock")
        .insert(key, data.clone());
    Ok(data)
}

fn load_more_threads(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let (query, offset, can_load_more) = {
        let state = state.borrow();
        (
            state.current_query.clone(),
            state.thread_window_offset + state.thread_list_items.len(),
            state.can_load_more_threads,
        )
    };
    if !can_load_more {
        widgets
            .status_label
            .set_text("All currently counted threads are already loaded");
        return;
    }
    set_thread_loading_indicator(
        widgets,
        &format!("Loading more messages from {}…", format_count(offset + 1)),
    );
    let (tx, rx) = mpsc::channel::<SearchResponse>();
    let opts = options.clone();
    let generation = widgets.search_generation.get().saturating_add(1);
    widgets.search_generation.set(generation);
    thread::spawn(move || {
        let result = execute_search_page(&opts, &query, offset);
        let _ = tx.send(SearchResponse { generation, result });
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(response) => {
            if response.generation == w.search_generation.get() {
                match response.result {
                    Ok(data) => append_search_data(&opts, &w, &st, data),
                    Err(err) => apply_search_error(&w, &st, err),
                }
            }
            gtk::glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            apply_search_error(&w, &st, anyhow::anyhow!("thread page load cancelled"));
            gtk::glib::ControlFlow::Break
        }
    });
}

fn apply_search_data(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    data: SearchData,
) {
    let query = data.query.clone();
    let count = data.count;
    let offset = data.offset;
    let cached = data.cached;
    {
        let mut s = state.borrow_mut();
        s.current_query = query.clone();
        s.thread_window_offset = offset;
        s.thread_list_items = data.threads;
        s.thread_total_count = count;
        s.thread_loaded_count = s.thread_list_items.len();
        s.thread_page_size = data.limit;
        s.can_load_more_threads =
            s.thread_window_offset + s.thread_list_items.len() < count as usize;
        s.thread_details = data.details;
        s.selected_thread = None;
        s.selected_message = None;
        s.messages.clear();
        s.visual_select_mode = false;
        s.visual_select_anchor = None;
        s.visual_selected_threads.clear();
        s.visual_selection_pending_range = None;
        s.visible_tags = data.tags;
        s.database_path = Some(data.database_path);
        s.database_revision = Some(data.revision);
        s.last_error = None;
        s.last_operation = Some(format!(
            "search `{}` loaded {} of {} thread(s) from offset {}{}",
            query,
            s.thread_list_items.len(),
            count,
            offset,
            if cached { " from cache" } else { "" }
        ));
    }
    populate_thread_list(options, widgets, state);
    update_tag_searches(options, widgets, state);
    refresh_thread_attachment_list(widgets, state);
    update_message_menu(options, widgets, state);
    widgets.status_label.set_text(&format!(
        "{} for {}{}",
        thread_window_status(state),
        query,
        if cached { " (cached)" } else { "" }
    ));
    update_thread_result_label(widgets, state);
    if state.borrow().input_mode == InputMode::Normal {
        focus_active_pane(widgets, state);
    }
    update_debug(widgets, state);
}

fn append_search_data(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    data: SearchData,
) {
    let query = data.query.clone();
    let count = data.count;
    let offset = data.offset;
    let cached = data.cached;
    let selected_thread_id = state
        .borrow()
        .selected_thread
        .as_ref()
        .map(|thread| thread.thread_id.clone());
    let selected_index = selected_thread_index(widgets);
    {
        let mut s = state.borrow_mut();
        s.current_query = query.clone();
        if data.offset != s.thread_window_offset + s.thread_list_items.len() {
            s.thread_window_offset = data.offset;
            s.thread_list_items.clear();
            s.thread_details.clear();
        }
        s.thread_list_items.extend(data.threads);
        s.thread_details.extend(data.details);
        s.thread_total_count = count;
        s.thread_loaded_count = s.thread_list_items.len();
        s.thread_page_size = data.limit;
        s.can_load_more_threads =
            s.thread_window_offset + s.thread_list_items.len() < count as usize;
        s.visible_tags = data.tags;
        s.database_path = Some(data.database_path);
        s.database_revision = Some(data.revision);
        s.last_error = None;
        s.last_operation = Some(format!(
            "loaded page at offset {}: {}{}",
            offset,
            thread_window_status_from_parts(
                s.thread_window_offset,
                s.thread_list_items.len(),
                count as usize,
            ),
            if cached { " from cache" } else { "" }
        ));
    }
    populate_thread_list(options, widgets, state);
    let restored_index =
        restore_thread_selection(widgets, state, selected_thread_id, selected_index);
    update_tag_searches(options, widgets, state);
    if let Some(index) = restored_index {
        widgets
            .status_label
            .set_text(&message_position_status(state, index, "Selected"));
    } else {
        widgets
            .status_label
            .set_text(&format!("Loaded {}", thread_window_status(state)));
    }
    update_thread_result_label(widgets, state);
    update_debug(widgets, state);
}

fn restore_thread_selection(
    widgets: &Widgets,
    state: &SharedState,
    selected_thread_id: Option<String>,
    selected_index: Option<usize>,
) -> Option<usize> {
    let threads = state.borrow().thread_list_items.clone();
    let index = selected_thread_id
        .and_then(|thread_id| {
            threads
                .iter()
                .position(|thread| thread.thread_id == thread_id)
        })
        .or(selected_index)
        .filter(|index| *index < threads.len());
    if let Some(index) = index
        && let Some(row) = widgets.thread_list.row_at_index(index as i32)
    {
        widgets.thread_list.select_row(Some(&row));
        focus_thread_row(&row);
        return Some(index);
    }
    None
}

fn update_thread_result_label(widgets: &Widgets, state: &SharedState) {
    let state_ref = state.borrow();
    let status = thread_window_status_from_parts(
        state_ref.thread_window_offset,
        state_ref.thread_list_items.len(),
        state_ref.thread_total_count as usize,
    );
    widgets.thread_result_label.set_text(&format!(
        "{status} · page size {}",
        state_ref.thread_page_size
    ));
    let can_load_more = state_ref.can_load_more_threads;
    drop(state_ref);
    set_button_label(&widgets.load_more_button, "Load more", "G", state);
    widgets.load_more_button.set_sensitive(can_load_more);
}

fn thread_window_status(state: &SharedState) -> String {
    let state = state.borrow();
    thread_window_status_from_parts(
        state.thread_window_offset,
        state.thread_list_items.len(),
        state.thread_total_count as usize,
    )
}

fn thread_window_status_from_parts(offset: usize, loaded: usize, total: usize) -> String {
    if loaded == 0 {
        return format!("Loaded 0 of {} thread(s)", format_count(total));
    }
    let start = offset + 1;
    let end = offset + loaded;
    if offset == 0 {
        format!(
            "Loaded {} of {} thread(s)",
            format_count(loaded),
            format_count(total.max(loaded))
        )
    } else {
        format!(
            "Showing {}-{} of {} thread(s) ({} loaded)",
            format_count(start),
            format_count(end),
            format_count(total.max(end)),
            format_count(loaded)
        )
    }
}

fn search_cache_key(
    options: &LaunchOptions,
    query: &str,
    db_path: &str,
    revision: &notm_notmuch::Revision,
    offset: usize,
) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        db_path,
        revision.uuid,
        revision.revision,
        options.page_size,
        offset,
        options.excluded_tags.join(","),
        query
    )
}

fn apply_search_error(widgets: &Widgets, state: &SharedState, err: anyhow::Error) {
    state.borrow_mut().last_error = Some(err.to_string());
    widgets
        .status_label
        .set_text(&format!("Search failed: {err}"));
    update_debug(widgets, state);
}

fn populate_thread_list(_options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    while let Some(child) = widgets.thread_list.first_child() {
        widgets.thread_list.remove(&child);
    }
    let (threads, window_offset, details) = {
        let state = state.borrow();
        (
            state.thread_list_items.clone(),
            state.thread_window_offset,
            state.thread_details.clone(),
        )
    };
    for (idx, thread) in threads.iter().enumerate() {
        let row = gtk::ListBoxRow::new();
        row.set_widget_name(&format!("notm-thread-row-{idx}"));
        let detail = details.get(&thread.thread_id).cloned().unwrap_or_default();
        set_thread_row_content(&row, window_offset + idx, thread, &detail);
        widgets.thread_list.append(&row);
    }
    update_visual_selection_rows(widgets, state);
}

fn toggle_visual_select_mode(widgets: &Widgets, state: &SharedState) {
    if state.borrow().visual_select_mode {
        clear_visual_selection(widgets, state);
    } else {
        enter_visual_select_mode(widgets, state);
    }
}

fn enter_visual_select_mode(widgets: &Widgets, state: &SharedState) {
    let Some(index) = selected_thread_index(widgets) else {
        widgets
            .status_label
            .set_text("No thread selected for visual select");
        return;
    };
    {
        let mut state = state.borrow_mut();
        state.active_pane = ActivePane::Threads;
        state.visual_select_mode = true;
        state.visual_select_anchor = Some(state.thread_window_offset + index);
        state.visual_selection_pending_range = None;
    }
    update_visual_selection_to_cursor(widgets, state);
}

fn clear_visual_selection(widgets: &Widgets, state: &SharedState) {
    {
        let mut state = state.borrow_mut();
        state.visual_select_mode = false;
        state.visual_select_anchor = None;
        state.visual_selected_threads.clear();
        state.visual_selection_pending_range = None;
        state.input_mode = InputMode::Normal;
        state.active_pane = ActivePane::Threads;
    }
    update_visual_selection_rows(widgets, state);
    update_button_binding_labels(widgets, state);
    update_active_pane_visuals(widgets, state);
    widgets.status_label.set_text("Normal mode");
}

fn update_visual_selection_to_cursor(widgets: &Widgets, state: &SharedState) {
    let Some(cursor) = selected_thread_index(widgets) else {
        return;
    };
    let (anchor, ids) = {
        let state = state.borrow();
        if !state.visual_select_mode {
            return;
        }
        let cursor = state.thread_window_offset + cursor;
        let anchor = state.visual_select_anchor.unwrap_or(cursor);
        let start = anchor.min(cursor);
        let end = anchor.max(cursor);
        let ids = state
            .thread_list_items
            .iter()
            .enumerate()
            .filter(|(index, _)| (start..=end).contains(&(state.thread_window_offset + *index)))
            .map(|(_, thread)| thread.thread_id.clone())
            .collect::<BTreeSet<_>>();
        (anchor, ids)
    };
    let count = ids.len();
    {
        let mut state = state.borrow_mut();
        state.visual_select_anchor = Some(anchor);
        state.visual_selected_threads = ids;
    }
    update_visual_selection_rows(widgets, state);
    widgets
        .status_label
        .set_text(&format!("Visual select: {count} thread(s) selected"));
}

fn visual_selection_anchor_index(widgets: &Widgets, state: &SharedState) -> Option<usize> {
    let cursor = selected_thread_index(widgets);
    let state = state.borrow();
    if !state.visual_select_mode {
        return None;
    }
    Some(
        state
            .visual_select_anchor
            .or_else(|| cursor.map(|index| state.thread_window_offset + index))
            .unwrap_or(state.thread_window_offset),
    )
}

fn maybe_load_visual_selection_range(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    cursor_index: usize,
) {
    let Some(anchor_index) = visual_selection_anchor_index(widgets, state) else {
        return;
    };
    let (query, window_offset, loaded_len, generation) = {
        let state = state.borrow();
        (
            state.current_query.clone(),
            state.thread_window_offset,
            state.thread_list_items.len(),
            widgets.search_generation.get(),
        )
    };
    let start = anchor_index.min(cursor_index);
    let end = anchor_index.max(cursor_index);
    if loaded_len == 0
        || (window_offset..window_offset + loaded_len).contains(&start)
            && (window_offset..window_offset + loaded_len).contains(&end)
    {
        state.borrow_mut().visual_selection_pending_range = None;
        return;
    }
    state.borrow_mut().visual_selection_pending_range = Some((start, end));
    widgets.status_label.set_text(&format!(
        "Visual select: loading IDs for messages {}-{}…",
        format_count(start + 1),
        format_count(end + 1)
    ));
    let (tx, rx) = mpsc::channel::<ThreadRangeSelectionResponse>();
    let opts = options.clone();
    thread::spawn(move || {
        let result = collect_thread_ids_for_range(&opts, &query, start, end);
        let _ = tx.send(ThreadRangeSelectionResponse {
            generation,
            anchor_index,
            cursor_index,
            result,
        });
    });

    let w = widgets.clone();
    let st = state.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(response) => {
            if response.generation == w.search_generation.get()
                && st.borrow().visual_select_mode
                && st.borrow().visual_select_anchor == Some(response.anchor_index)
                && selected_thread_index(&w).map(|index| st.borrow().thread_window_offset + index)
                    == Some(response.cursor_index)
            {
                match response.result {
                    Ok(ids) => {
                        let count = ids.len();
                        {
                            let mut state = st.borrow_mut();
                            state.visual_selected_threads = ids;
                            state.visual_selection_pending_range = None;
                        }
                        update_visual_selection_rows(&w, &st);
                        w.status_label.set_text(&format!(
                            "Visual select: {} thread(s) selected",
                            format_count(count)
                        ));
                    }
                    Err(err) => {
                        {
                            let mut state = st.borrow_mut();
                            state.visual_selection_pending_range = None;
                            state.last_error = Some(err.to_string());
                        }
                        w.status_label
                            .set_text(&format!("Visual select range load failed: {err}"));
                        update_debug(&w, &st);
                    }
                }
            }
            gtk::glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            st.borrow_mut().visual_selection_pending_range = None;
            apply_search_error(
                &w,
                &st,
                anyhow::anyhow!("visual selection range load cancelled"),
            );
            gtk::glib::ControlFlow::Break
        }
    });
}

fn collect_thread_ids_for_range(
    options: &LaunchOptions,
    query: &str,
    start: usize,
    end: usize,
) -> anyhow::Result<BTreeSet<String>> {
    let db = Database::open(&open_config(options), DatabaseMode::ReadOnly)?;
    let page_size = options.page_size.max(1);
    let mut offset = (start / page_size) * page_size;
    let mut ids = BTreeSet::new();
    while offset <= end {
        let opts = QueryOptions {
            limit: page_size,
            offset,
            sort: SortOrder::NewestFirst,
            excluded_tags: options.excluded_tags.clone(),
        };
        let threads = db.search_threads(query, &opts)?;
        if threads.is_empty() {
            break;
        }
        for (index, thread) in threads.iter().enumerate() {
            let absolute_index = offset + index;
            if (start..=end).contains(&absolute_index) {
                ids.insert(thread.thread_id.clone());
            }
        }
        let next_offset = offset.saturating_add(page_size);
        if next_offset <= offset {
            break;
        }
        offset = next_offset;
    }
    Ok(ids)
}

fn update_visual_selection_rows(widgets: &Widgets, state: &SharedState) {
    let selected = state.borrow().visual_selected_threads.clone();
    for (index, thread) in state.borrow().thread_list_items.iter().enumerate() {
        if let Some(row) = widgets.thread_list.row_at_index(index as i32) {
            if selected.contains(&thread.thread_id) {
                row.add_css_class("notm-visual-selected");
            } else {
                row.remove_css_class("notm-visual-selected");
            }
        }
    }
}

fn set_thread_row_content(
    row: &gtk::ListBoxRow,
    idx: usize,
    thread: &notm_notmuch::ThreadSummary,
    detail: &ThreadUiDetails,
) {
    if thread.has_unread {
        row.add_css_class("unread");
    } else {
        row.remove_css_class("unread");
    }
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 2);
    box_.set_margin_start(6);
    box_.set_margin_end(6);
    box_.set_margin_top(6);
    box_.set_margin_bottom(6);
    let title = gtk::Label::new(Some(&format!(
        "{}{}{}{}{}{}",
        if thread.has_unread { "● " } else { "" },
        if thread.is_flagged { "★ " } else { "" },
        if detail.has_attachment { "📎 " } else { "" },
        if detail.has_encrypted { "🔒 " } else { "" },
        if detail.has_signed { "✍ " } else { "" },
        thread.subject
    )));
    title.set_widget_name(&format!("notm-thread-title-{idx}"));
    title.set_xalign(0.0);
    title.set_wrap(true);
    let meta = gtk::Label::new(Some(&format!(
        "{}  ·  {}/{}  ·  {}",
        thread.authors,
        thread.matched_messages,
        thread.total_messages,
        thread.tags.join(" ")
    )));
    meta.set_widget_name(&format!("notm-thread-meta-{idx}"));
    meta.set_xalign(0.0);
    meta.add_css_class("dim-label");
    meta.set_wrap(true);
    box_.append(&title);
    box_.append(&meta);
    if !detail.preview.is_empty() {
        let preview = gtk::Label::new(Some(&detail.preview));
        preview.set_widget_name(&format!("notm-thread-preview-{idx}"));
        preview.set_xalign(0.0);
        preview.add_css_class("dim-label");
        preview.set_wrap(true);
        box_.append(&preview);
    }
    row.set_child(Some(&box_));
}

fn thread_details_for_threads(
    db: &Database,
    database_path: &str,
    revision: &notm_notmuch::Revision,
    threads: &[notm_notmuch::ThreadSummary],
) -> BTreeMap<String, ThreadUiDetails> {
    let mut out = BTreeMap::new();
    for thread in threads {
        let cache_key = thread_detail_cache_key(database_path, Some(revision), &thread.thread_id);
        if let Some(detail) = THREAD_DETAIL_CACHE
            .get_or_init(Default::default)
            .lock()
            .expect("thread detail cache lock")
            .get(&cache_key)
            .cloned()
        {
            out.insert(thread.thread_id.clone(), detail);
            continue;
        }
        let detail = db
            .thread_messages(&thread.thread_id)
            .map(|messages| compute_thread_detail(&messages))
            .unwrap_or_default();
        THREAD_DETAIL_CACHE
            .get_or_init(Default::default)
            .lock()
            .expect("thread detail cache lock")
            .insert(cache_key, detail.clone());
        out.insert(thread.thread_id.clone(), detail);
    }
    out
}

fn thread_detail_cache_key(
    db_path: &str,
    revision: Option<&notm_notmuch::Revision>,
    thread_id: &str,
) -> String {
    let (rev, uuid) = revision
        .map(|revision| (revision.revision, revision.uuid.as_str()))
        .unwrap_or_default();
    format!("{db_path}\u{1f}{uuid}\u{1f}{rev}\u{1f}{thread_id}")
}

fn compute_thread_detail(messages: &[notm_notmuch::MessageSummary]) -> ThreadUiDetails {
    let mut detail = ThreadUiDetails::default();
    for message in messages {
        for filename in &message.filenames {
            let Ok(bytes) = std::fs::read(filename) else {
                continue;
            };
            let raw_lower = String::from_utf8_lossy(&bytes).to_lowercase();
            detail.has_encrypted |= raw_lower.contains("multipart/encrypted")
                || raw_lower.contains("application/pgp-encrypted");
            detail.has_signed |= raw_lower.contains("multipart/signed")
                || raw_lower.contains("application/pgp-signature")
                || raw_lower.contains("application/pkcs7-signature");
            if let Ok(parsed) = notm_mail::mime::parse_rfc5322(&bytes) {
                detail.has_attachment |= !parsed.attachments.is_empty();
                if detail.preview.is_empty() {
                    detail.preview = body_preview(&parsed.safe_body);
                }
            }
        }
    }
    detail
}

fn body_preview(body: &str) -> String {
    let mut preview = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('>'))
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    if preview.chars().count() > 180 {
        preview = preview.chars().take(177).collect::<String>();
        preview.push('…');
    }
    preview
}

fn format_count(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn truncate_status_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

fn message_position_status(state: &SharedState, index: usize, verb: &str) -> String {
    let state = state.borrow();
    let loaded = state.thread_list_items.len();
    let absolute_index = state.thread_window_offset + index;
    let total = (state.thread_total_count as usize).max(state.thread_window_offset + loaded);
    let number = absolute_index.saturating_add(1).min(total.max(1));
    let position = if total > loaded {
        format!(
            "{} of {} ({} loaded)",
            format_count(number),
            format_count(total),
            format_count(loaded)
        )
    } else {
        format!(
            "{} of {}",
            format_count(number),
            format_count(total.max(loaded))
        )
    };
    let subject = state
        .thread_list_items
        .get(index)
        .map(|thread| truncate_status_text(&thread.subject, 72))
        .unwrap_or_default();
    if subject.is_empty() {
        format!("{verb} message {position}")
    } else {
        format!("{verb} message {position} · {subject}")
    }
}

fn select_thread_by_index(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    index: usize,
    open: bool,
) {
    let Some(thread) = state.borrow().thread_list_items.get(index).cloned() else {
        return;
    };
    if open {
        open_thread_by_index(options, widgets, state, index);
        return;
    }

    let result = (|| -> anyhow::Result<()> {
        let db = Database::open(&open_config(options), DatabaseMode::ReadOnly)?;
        let messages = db.thread_messages(&thread.thread_id)?;
        {
            let mut state = state.borrow_mut();
            state.selected_thread = Some(thread.clone());
            state.selected_message = messages.last().cloned();
            state.messages = messages;
            state.active_pane = ActivePane::Threads;
            state.last_operation = Some(format!("previewed thread {}", thread.thread_id));
            state.last_error = None;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            refresh_thread_attachment_list(widgets, state);
            update_message_menu(options, widgets, state);
            if selected_message_is_draft(options, state) {
                match open_selected_draft_message(widgets, state) {
                    Ok(()) => {
                        state.borrow_mut().active_pane = ActivePane::Threads;
                        focus_active_pane(widgets, state);
                        widgets.status_label.set_text(&message_position_status(
                            state,
                            index,
                            "Selected draft",
                        ));
                    }
                    Err(err) => {
                        state.borrow_mut().last_error = Some(err.to_string());
                        widgets
                            .status_label
                            .set_text(&format!("Preview draft failed: {err}"));
                    }
                }
            } else {
                set_active_draft(widgets, state, None);
                show_preferred_selected_message_view(options, widgets, state);
                state.borrow_mut().active_pane = ActivePane::Threads;
                focus_active_pane(widgets, state);
                widgets
                    .status_label
                    .set_text(&message_position_status(state, index, "Selected"));
            }
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Preview thread failed: {err}"));
            update_debug(widgets, state);
            return;
        }
    }
    if let Some(row) = widgets.thread_list.row_at_index(index as i32) {
        focus_thread_row(&row);
    }
    if state.borrow().visual_select_mode {
        update_visual_selection_to_cursor(widgets, state);
        let absolute_index = state.borrow().thread_window_offset + index;
        maybe_load_visual_selection_range(options, widgets, state, absolute_index);
    }
    update_custom_tag_controls(widgets, state);
    update_message_action_buttons(options, widgets, state);
    update_debug(widgets, state);
}

fn open_thread_by_index(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    index: usize,
) {
    let Some(thread) = state.borrow().thread_list_items.get(index).cloned() else {
        return;
    };
    let result = (|| -> anyhow::Result<()> {
        let db = Database::open(&open_config(options), DatabaseMode::ReadOnly)?;
        let messages = db.thread_messages(&thread.thread_id)?;
        {
            let mut s = state.borrow_mut();
            s.selected_thread = Some(thread.clone());
            s.selected_message = messages.last().cloned();
            s.messages = messages;
            s.active_pane = ActivePane::Message;
            s.last_operation = Some(format!("opened thread {}", thread.thread_id));
            s.last_error = None;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            refresh_thread_attachment_list(widgets, state);
            update_message_menu(options, widgets, state);
            if selected_message_is_draft(options, state) {
                match open_selected_draft_message(widgets, state) {
                    Ok(()) => widgets.status_label.set_text(&message_position_status(
                        state,
                        index,
                        "Opened draft",
                    )),
                    Err(err) => {
                        state.borrow_mut().last_error = Some(err.to_string());
                        widgets
                            .status_label
                            .set_text(&format!("Open draft failed: {err}"));
                    }
                }
            } else {
                set_active_draft(widgets, state, None);
                show_preferred_selected_message_view(options, widgets, state);
                widgets
                    .status_label
                    .set_text(&message_position_status(state, index, "Opened"));
            }
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Open thread failed: {err}"));
        }
    }
    update_custom_tag_controls(widgets, state);
    update_debug(widgets, state);
}

fn push_undo_tag_action(undo_state: &UndoState, action: UndoTagAction) {
    const MAX_UNDO_TAG_ACTIONS: usize = 30;
    let snapshot = {
        let mut actions = undo_state.borrow_mut();
        actions.push(action);
        if actions.len() > MAX_UNDO_TAG_ACTIONS {
            let overflow = actions.len() - MAX_UNDO_TAG_ACTIONS;
            actions.drain(0..overflow);
        }
        actions.clone()
    };
    let _ = persist_undo_tag_actions(&snapshot);
}

fn pop_last_undo_tag_action(undo_state: &UndoState) -> Option<UndoTagAction> {
    let (action, snapshot) = {
        let mut actions = undo_state.borrow_mut();
        let action = actions.pop();
        (action, actions.clone())
    };
    let _ = persist_undo_tag_actions(&snapshot);
    action
}

fn remove_undo_tag_action(undo_state: &UndoState, index: usize) -> Option<UndoTagAction> {
    let (action, snapshot) = {
        let mut actions = undo_state.borrow_mut();
        if index >= actions.len() {
            (None, actions.clone())
        } else {
            (Some(actions.remove(index)), actions.clone())
        }
    };
    let _ = persist_undo_tag_actions(&snapshot);
    action
}

fn default_undo_history_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("notm")
        .join("tag-undo.json")
}

fn load_undo_tag_actions() -> Vec<UndoTagAction> {
    let path = default_undo_history_path();
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<UndoTagAction>>(&text).ok())
        .unwrap_or_default()
}

fn persist_undo_tag_actions(actions: &[UndoTagAction]) -> anyhow::Result<()> {
    let path = default_undo_history_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(actions)?)?;
    Ok(())
}

fn tag_undo_label(
    mutation: &TagMutation,
    target_threads: usize,
    changed_messages: usize,
) -> String {
    let adds = if mutation.add.is_empty() {
        String::new()
    } else {
        format!("+{}", mutation.add.join(" +"))
    };
    let removes = if mutation.remove.is_empty() {
        String::new()
    } else {
        format!("-{}", mutation.remove.join(" -"))
    };
    let ops = [adds, removes]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{ops} on {} ({changed_messages} changed)",
        tag_target_status_label(target_threads)
    )
}

fn tag_selected(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    mutation: TagMutation,
) -> bool {
    if let Some((start, end)) = state.borrow().visual_selection_pending_range {
        widgets.status_label.set_text(&format!(
            "Visual selection {}-{} is still loading; wait for the selected count before tagging",
            format_count(start + 1),
            format_count(end + 1)
        ));
        return false;
    }
    let target_thread_ids = tag_target_thread_ids(state);
    if target_thread_ids.is_empty() {
        widgets
            .status_label
            .set_text("No selected thread for tag operation");
        return false;
    }
    let target_count = target_thread_ids.len();
    let query = tag_query_for_thread_ids(&target_thread_ids);
    let result = (|| -> anyhow::Result<usize> {
        let db = Database::open(&open_config(options), DatabaseMode::ReadWrite)?;
        let report = db.apply_tags_to_query(&query, &mutation)?;
        if report.changed_messages > 0 {
            push_undo_tag_action(
                undo_state,
                UndoTagAction {
                    query: query.clone(),
                    mutation: TagMutation {
                        add: mutation.remove.clone(),
                        remove: mutation.add.clone(),
                        sync_maildir_flags: mutation.sync_maildir_flags,
                    },
                    label: tag_undo_label(&mutation, target_count, report.changed_messages),
                },
            );
        }
        state.borrow_mut().last_operation = Some(format!(
            "tagged {} message(s): +{:?} -{:?}",
            report.changed_messages, report.added, report.removed
        ));
        Ok(report.changed_messages)
    })();
    match result {
        Ok(changed_messages) => {
            apply_local_tag_mutation(widgets, state, &mutation, &target_thread_ids);
            update_message_header(widgets, state);
            update_custom_tag_controls(widgets, state);
            update_message_action_buttons(options, widgets, state);
            let undo_available = !undo_state.borrow().is_empty();
            set_undo_tag_available(widgets, undo_available);
            if changed_messages > 0 {
                widgets.status_label.set_text(&format!(
                    "Tag operation complete for {}; Undo menu shows recent tag actions",
                    tag_target_status_label(target_count)
                ));
            } else {
                widgets
                    .status_label
                    .set_text("Tag operation made no changes");
            }
            true
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Tag operation failed: {err}"));
            update_debug(widgets, state);
            false
        }
    }
}

fn apply_local_tag_mutation(
    widgets: &Widgets,
    state: &SharedState,
    mutation: &TagMutation,
    target_thread_ids: &BTreeSet<String>,
) {
    let row_updates = {
        let mut state = state.borrow_mut();
        let mut updated_thread_indices = Vec::new();
        for (index, thread) in state.thread_list_items.iter_mut().enumerate() {
            if target_thread_ids.contains(&thread.thread_id) {
                apply_tag_mutation_to_thread(thread, mutation);
                updated_thread_indices.push(index);
            }
        }
        if let Some(thread) = &mut state.selected_thread
            && target_thread_ids.contains(&thread.thread_id)
        {
            apply_tag_mutation_to_thread(thread, mutation);
        }
        for message in &mut state.messages {
            if target_thread_ids.contains(&message.thread_id) {
                apply_tag_mutation_to_tags(&mut message.tags, mutation);
            }
        }
        if let Some(message) = &mut state.selected_message
            && target_thread_ids.contains(&message.thread_id)
        {
            apply_tag_mutation_to_tags(&mut message.tags, mutation);
        }
        updated_thread_indices
            .into_iter()
            .filter_map(|index| {
                let thread = state.thread_list_items.get(index)?.clone();
                let detail = state
                    .thread_details
                    .get(&thread.thread_id)
                    .cloned()
                    .unwrap_or_default();
                Some((index, thread, detail))
            })
            .collect::<Vec<_>>()
    };
    for (index, thread, detail) in row_updates {
        if let Some(row) = widgets.thread_list.row_at_index(index as i32) {
            set_thread_row_content(&row, index, &thread, &detail);
        }
    }
    update_visual_selection_rows(widgets, state);
}

fn apply_tag_mutation_to_thread(thread: &mut notm_notmuch::ThreadSummary, mutation: &TagMutation) {
    apply_tag_mutation_to_tags(&mut thread.tags, mutation);
    thread.has_unread = thread.tags.iter().any(|tag| tag == "unread");
    thread.is_flagged = thread.tags.iter().any(|tag| tag == "flagged");
}

fn apply_tag_mutation_to_tags(tags: &mut Vec<String>, mutation: &TagMutation) {
    let mut tag_set = tags.iter().cloned().collect::<BTreeSet<_>>();
    for tag in &mutation.remove {
        tag_set.remove(tag);
    }
    for tag in &mutation.add {
        tag_set.insert(tag.clone());
    }
    *tags = tag_set.into_iter().collect();
}

fn set_undo_tag_available(widgets: &Widgets, available: bool) {
    widgets.undo_tag_button.set_visible(available);
    if available {
        widgets.undo_tag_button.add_css_class("suggested-action");
        widgets
            .undo_tag_button
            .set_tooltip_text(Some("Undo recent tag operations"));
    } else {
        widgets.undo_tag_button.remove_css_class("suggested-action");
    }
}

fn update_message_menu(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    while let Some(child) = widgets.message_menu_box.first_child() {
        widgets.message_menu_box.remove(&child);
    }
    let messages = state.borrow().messages.clone();
    let selected_index = selected_message_index(state);
    let total = messages.len();
    if total == 0 {
        widgets.message_menu_button.set_label("Message");
        widgets.message_menu_button.set_sensitive(false);
        update_message_action_buttons(options, widgets, state);
        return;
    }
    widgets.message_menu_button.set_sensitive(true);
    let label = selected_index
        .map(|index| format!("Message {}/{}", index + 1, total))
        .unwrap_or_else(|| "Message".to_string());
    widgets.message_menu_button.set_label(&label);
    for (index, message) in messages.iter().enumerate() {
        let subject = if message.subject.trim().is_empty() {
            "(no subject)"
        } else {
            message.subject.trim()
        };
        let label = format!("{}: {}", index + 1, subject);
        let button = gtk::Button::with_label(&label);
        button.set_widget_name(&format!("notm-message-select-{}", index + 1));
        if Some(index) == selected_index {
            button.add_css_class("suggested-action");
        }
        let opts = options.clone();
        let w = widgets.clone();
        let st = state.clone();
        button.connect_clicked(move |_| select_message_by_index(&opts, &w, &st, index));
        widgets.message_menu_box.append(&button);
    }
    update_message_action_buttons(options, widgets, state);
}

fn select_message_by_index(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    index: usize,
) {
    let message = state.borrow().messages.get(index).cloned();
    if message.is_none() {
        widgets.status_label.set_text("Message index not found");
        return;
    }
    state.borrow_mut().selected_message = message;
    if let Some((attachment_row, _)) = widgets
        .attachment_items
        .borrow()
        .iter()
        .enumerate()
        .find(|(_, item)| item.message_index == index)
        && let Some(row) = widgets.attachment_list.row_at_index(attachment_row as i32)
    {
        widgets.attachment_list.select_row(Some(&row));
    }
    update_message_menu(options, widgets, state);
    if selected_message_is_draft(options, state) {
        match open_selected_draft_message(widgets, state) {
            Ok(()) => widgets.status_label.set_text("Opened draft for editing"),
            Err(err) => {
                state.borrow_mut().last_error = Some(err.to_string());
                widgets
                    .status_label
                    .set_text(&format!("Open draft failed: {err}"));
            }
        }
    } else {
        set_active_draft(widgets, state, None);
        show_preferred_selected_message_view(options, widgets, state);
    }
}

fn undo_last_tag(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) {
    let Some(action) = pop_last_undo_tag_action(undo_state) else {
        set_undo_tag_available(widgets, false);
        widgets.status_label.set_text("No tag operation to undo");
        return;
    };
    undo_tag_action(options, widgets, state, undo_state, action);
}

fn undo_tag_action(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    action: UndoTagAction,
) {
    let query = action.query.clone();
    let mutation = action.mutation.clone();
    let result = (|| -> anyhow::Result<()> {
        let db = Database::open(&open_config(options), DatabaseMode::ReadWrite)?;
        db.apply_tags_to_query(&query, &mutation)?;
        state.borrow_mut().last_operation = Some(format!("undid tag operation: {}", action.label));
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_undo_tag_available(widgets, !undo_state.borrow().is_empty());
            let target_thread_ids = thread_ids_from_tag_query(&query);
            if !target_thread_ids.is_empty() {
                apply_local_tag_mutation(widgets, state, &mutation, &target_thread_ids);
                update_message_header(widgets, state);
                update_custom_tag_controls(widgets, state);
                update_message_action_buttons(options, widgets, state);
            } else {
                let current = state.borrow().current_query.clone();
                run_search(options, widgets, state, &current);
            }
            widgets
                .status_label
                .set_text(&format!("Undid tag operation: {}", action.label));
        }
        Err(err) => {
            push_undo_tag_action(undo_state, action);
            set_undo_tag_available(widgets, true);
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Undo failed: {err}"));
            update_debug(widgets, state);
        }
    }
}

fn show_undo_tag_actions(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) {
    let actions = undo_state.borrow().clone();
    if actions.is_empty() {
        set_undo_tag_available(widgets, false);
        widgets.status_label.set_text("No tag operation to undo");
        return;
    }

    let dialog = gtk::Dialog::builder()
        .title("Undo tag operations")
        .transient_for(&widgets.window)
        .modal(true)
        .default_width(620)
        .default_height(320)
        .build();
    dialog.set_widget_name("notm-undo-tag-dialog");
    let area = dialog.content_area();
    area.set_spacing(6);

    let help = gtk::Label::new(Some(
        "Newest actions are listed first. Use j/k/gg/G/Ctrl+d/Ctrl+u, Space to select, v for visual selection, Enter to undo selected.",
    ));
    help.set_xalign(0.0);
    help.add_css_class("dim-label");
    area.append(&help);

    let list = gtk::ListBox::new();
    list.set_widget_name("notm-undo-tag-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    list.set_focusable(true);
    for (display_index, action) in actions.iter().rev().enumerate() {
        let row = gtk::ListBoxRow::new();
        row.set_widget_name(&format!("notm-undo-tag-row-{}", display_index + 1));
        row.set_focusable(true);
        let label = gtk::Label::new(Some(&action.label));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_margin_start(8);
        label.set_margin_end(8);
        label.set_margin_top(6);
        label.set_margin_bottom(6);
        row.set_child(Some(&label));
        list.append(&row);
    }
    let selected_rows = Rc::new(RefCell::new(BTreeSet::<usize>::new()));
    let cursor_row = Rc::new(Cell::new(0_usize));
    let visual_anchor = Rc::new(Cell::new(None::<usize>));
    refresh_undo_dialog_selection(&list, &selected_rows.borrow(), cursor_row.get());

    let scrolled = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .min_content_height(180)
        .child(&list)
        .build();
    area.append(&scrolled);

    let w = widgets.clone();
    let selected = selected_rows.clone();
    let cursor = cursor_row.clone();
    let actions_len = actions.len();
    let list_for_rows = list.clone();
    list.connect_row_activated(move |_, row| {
        let row_index = row.index();
        if row_index < 0 {
            return;
        }
        let row_index = row_index as usize;
        cursor.set(row_index);
        toggle_undo_dialog_row(&list_for_rows, &selected, row_index);
        refresh_undo_dialog_selection(&list_for_rows, &selected.borrow(), row_index);
        w.status_label.set_text(&format!(
            "Undo list: {} selected; Enter to apply",
            selected.borrow().len()
        ));
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    let d = dialog.clone();
    let selected = selected_rows.clone();
    let cursor = cursor_row.clone();
    let visual = visual_anchor.clone();
    let list_for_keys = list.clone();
    let numeric_prefix = Rc::new(RefCell::new(String::new()));
    let pending_g = Rc::new(Cell::new(false));
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_key_pressed(move |_, key, _, mods| {
        let ctrl = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK);
        if key == gtk::gdk::Key::Escape {
            d.close();
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter {
            apply_selected_undo_dialog_actions(
                &opts,
                &w,
                &st,
                &undo,
                &d,
                &selected.borrow(),
                cursor.get(),
                actions_len,
            );
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::space {
            visual.set(None);
            toggle_undo_dialog_row(&list_for_keys, &selected, cursor.get());
            refresh_undo_dialog_selection(&list_for_keys, &selected.borrow(), cursor.get());
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::v {
            if visual.get().is_some() {
                visual.set(None);
                w.status_label.set_text("Undo visual selection ended");
            } else {
                let cursor = cursor.get();
                visual.set(Some(cursor));
                selected.borrow_mut().clear();
                selected.borrow_mut().insert(cursor);
                refresh_undo_dialog_selection(&list_for_keys, &selected.borrow(), cursor);
                w.status_label
                    .set_text("Undo visual selection: move with j/k/gg/G, Enter to apply");
            }
            return gtk::glib::Propagation::Stop;
        }
        if let Some(digit) = key_to_digit(key)
            && (digit != 0 || !numeric_prefix.borrow().is_empty())
        {
            numeric_prefix.borrow_mut().push(char::from(b'0' + digit));
            return gtk::glib::Propagation::Stop;
        }
        if pending_g.get() {
            pending_g.set(false);
            let count = take_numeric_prefix(&numeric_prefix);
            if key == gtk::gdk::Key::g {
                let target = count
                    .map(|count| count.saturating_sub(1))
                    .unwrap_or(0)
                    .min(actions_len.saturating_sub(1));
                move_undo_dialog_cursor(
                    &list_for_keys,
                    &selected,
                    &cursor,
                    &visual,
                    target,
                    actions_len,
                );
                return gtk::glib::Propagation::Stop;
            }
            clear_numeric_prefix(&numeric_prefix);
            return gtk::glib::Propagation::Proceed;
        }
        let count = numeric_prefix_value(&numeric_prefix).unwrap_or(1);
        let current = cursor.get();
        let handled = if key == gtk::gdk::Key::g {
            pending_g.set(true);
            true
        } else if key == gtk::gdk::Key::G {
            let target = if !numeric_prefix.borrow().is_empty() {
                count.saturating_sub(1).min(actions_len.saturating_sub(1))
            } else {
                actions_len.saturating_sub(1)
            };
            move_undo_dialog_cursor(
                &list_for_keys,
                &selected,
                &cursor,
                &visual,
                target,
                actions_len,
            );
            true
        } else if key == gtk::gdk::Key::j || key == gtk::gdk::Key::Down {
            let target = current
                .saturating_add(count)
                .min(actions_len.saturating_sub(1));
            move_undo_dialog_cursor(
                &list_for_keys,
                &selected,
                &cursor,
                &visual,
                target,
                actions_len,
            );
            true
        } else if key == gtk::gdk::Key::k || key == gtk::gdk::Key::Up {
            let target = current.saturating_sub(count);
            move_undo_dialog_cursor(
                &list_for_keys,
                &selected,
                &cursor,
                &visual,
                target,
                actions_len,
            );
            true
        } else if ctrl && (key == gtk::gdk::Key::d || key == gtk::gdk::Key::D) {
            let target = current
                .saturating_add(UNDO_DIALOG_HALF_PAGE_ROWS)
                .min(actions_len.saturating_sub(1));
            move_undo_dialog_cursor(
                &list_for_keys,
                &selected,
                &cursor,
                &visual,
                target,
                actions_len,
            );
            true
        } else if ctrl && (key == gtk::gdk::Key::u || key == gtk::gdk::Key::U) {
            let target = current.saturating_sub(UNDO_DIALOG_HALF_PAGE_ROWS);
            move_undo_dialog_cursor(
                &list_for_keys,
                &selected,
                &cursor,
                &visual,
                target,
                actions_len,
            );
            true
        } else {
            false
        };
        if handled {
            if key != gtk::gdk::Key::g {
                clear_numeric_prefix(&numeric_prefix);
            }
            gtk::glib::Propagation::Stop
        } else {
            clear_numeric_prefix(&numeric_prefix);
            gtk::glib::Propagation::Proceed
        }
    });
    list.add_controller(controller);

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    let d = dialog.clone();
    let selected = selected_rows.clone();
    let cursor = cursor_row.clone();
    dialog.add_button("Undo selected", gtk::ResponseType::Accept);
    dialog.add_button("Close", gtk::ResponseType::Close);
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            apply_selected_undo_dialog_actions(
                &opts,
                &w,
                &st,
                &undo,
                &d,
                &selected.borrow(),
                cursor.get(),
                actions_len,
            );
        } else {
            dialog.close();
        }
    });
    dialog.present();
    let list_for_focus = list.clone();
    gtk::glib::idle_add_local_once(move || {
        if let Some(row) = list_for_focus.row_at_index(0) {
            row.grab_focus();
        } else {
            list_for_focus.grab_focus();
        }
    });
}

const UNDO_DIALOG_HALF_PAGE_ROWS: usize = 5;

fn toggle_undo_dialog_row(
    list: &gtk::ListBox,
    selected: &Rc<RefCell<BTreeSet<usize>>>,
    index: usize,
) {
    {
        let mut selected = selected.borrow_mut();
        if !selected.remove(&index) {
            selected.insert(index);
        }
    }
    refresh_undo_dialog_selection(list, &selected.borrow(), index);
}

fn move_undo_dialog_cursor(
    list: &gtk::ListBox,
    selected: &Rc<RefCell<BTreeSet<usize>>>,
    cursor: &Rc<Cell<usize>>,
    visual_anchor: &Rc<Cell<Option<usize>>>,
    target: usize,
    actions_len: usize,
) {
    if actions_len == 0 {
        return;
    }
    let target = target.min(actions_len - 1);
    cursor.set(target);
    if let Some(anchor) = visual_anchor.get() {
        let start = anchor.min(target);
        let end = anchor.max(target);
        let mut selected = selected.borrow_mut();
        selected.clear();
        selected.extend(start..=end);
    }
    refresh_undo_dialog_selection(list, &selected.borrow(), target);
}

fn refresh_undo_dialog_selection(list: &gtk::ListBox, selected: &BTreeSet<usize>, cursor: usize) {
    let mut index = 0_usize;
    while let Some(row) = list.row_at_index(index as i32) {
        if selected.contains(&index) {
            row.add_css_class("notm-undo-selected");
        } else {
            row.remove_css_class("notm-undo-selected");
        }
        if index == cursor {
            row.add_css_class("notm-keyboard-cursor");
            row.grab_focus();
        } else {
            row.remove_css_class("notm-keyboard-cursor");
        }
        index += 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_selected_undo_dialog_actions(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    dialog: &gtk::Dialog,
    selected: &BTreeSet<usize>,
    cursor: usize,
    actions_len: usize,
) {
    if actions_len == 0 {
        widgets.status_label.set_text("No tag operation to undo");
        dialog.close();
        return;
    }
    let display_indices = if selected.is_empty() {
        BTreeSet::from([cursor.min(actions_len - 1)])
    } else {
        selected
            .iter()
            .copied()
            .filter(|index| *index < actions_len)
            .collect::<BTreeSet<_>>()
    };
    let mut removed = Vec::new();
    {
        let original_len = undo_state.borrow().len();
        let mut storage_indices = display_indices
            .iter()
            .filter_map(|display_index| original_len.checked_sub(1 + display_index))
            .collect::<Vec<_>>();
        storage_indices.sort_unstable();
        for storage_index in storage_indices.into_iter().rev() {
            if let Some(action) = remove_undo_tag_action(undo_state, storage_index) {
                let display_index = original_len - 1 - storage_index;
                removed.push((display_index, action));
            }
        }
    }
    removed.sort_by_key(|(display_index, _)| *display_index);
    let count = removed.len();
    for (_, action) in removed {
        undo_tag_action(options, widgets, state, undo_state, action);
    }
    if count == 0 {
        widgets
            .status_label
            .set_text("No selected undo action found");
    } else {
        widgets
            .status_label
            .set_text(&format!("Undid {count} tag operation(s)"));
    }
    dialog.close();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncRunKind {
    Manual,
    Startup,
}

#[derive(Debug, Clone)]
struct SyncCommandSpec {
    name: &'static str,
    command: String,
}

fn run_manual_sync(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    run_sync_commands(options, widgets, state, SyncRunKind::Manual, true);
}

fn run_startup_sync(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    run_sync_commands(options, widgets, state, SyncRunKind::Startup, true);
}

fn run_sync_commands(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    kind: SyncRunKind,
    refresh_after: bool,
) {
    if !options.sync_enabled {
        if kind == SyncRunKind::Manual {
            widgets.status_label.set_text("Manual sync is disabled");
            state.borrow_mut().last_operation = Some("manual sync disabled".to_string());
        }
        update_debug(widgets, state);
        return;
    }
    let commands = sync_command_specs(options, kind);
    if commands.is_empty() {
        if kind == SyncRunKind::Manual {
            widgets.status_label.set_text(
                "Manual sync has no commands to run; enable and define receive and/or database update commands",
            );
            state.borrow_mut().last_operation = Some("manual sync no-op".to_string());
            update_debug(widgets, state);
        } else {
            // No startup sync was requested; keep the normal startup search status/debug context.
        }
        return;
    }
    let label = match kind {
        SyncRunKind::Manual => "Manual sync",
        SyncRunKind::Startup => "Startup sync",
    };
    widgets
        .status_label
        .set_text(&format!("{label}: running {} command(s)…", commands.len()));
    let result = (|| -> anyhow::Result<Vec<String>> {
        let mut reports = Vec::new();
        for spec in commands {
            let output = Command::new("sh").arg("-c").arg(&spec.command).output()?;
            reports.push(format!(
                "{}: status={:?} stdout={} stderr={}",
                spec.name,
                output.status.code(),
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
            anyhow::ensure!(
                output.status.success(),
                "{} command `{}` failed with status {:?}",
                label,
                spec.name,
                output.status.code()
            );
        }
        Ok(reports)
    })();
    match result {
        Ok(reports) => {
            state.borrow_mut().last_operation = Some(format!(
                "{}: {}",
                label.to_ascii_lowercase(),
                reports.join("; ")
            ));
            widgets.status_label.set_text(&format!("{label} completed"));
            if refresh_after {
                let current = state.borrow().current_query.clone();
                run_search(options, widgets, state, &current);
            }
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("{label} failed: {err}"));
            update_debug(widgets, state);
        }
    }
}

fn sync_command_specs(options: &LaunchOptions, kind: SyncRunKind) -> Vec<SyncCommandSpec> {
    let mut commands = Vec::new();
    if options.external_receive_enabled
        && !options.external_receive_command.trim().is_empty()
        && (kind == SyncRunKind::Manual || options.external_receive_on_startup)
    {
        commands.push(SyncCommandSpec {
            name: "receive",
            command: options.external_receive_command.clone(),
        });
    }
    if options.notmuch_database_update_enabled
        && !options.notmuch_database_update_command.trim().is_empty()
        && (kind == SyncRunKind::Manual || options.notmuch_database_update_on_startup)
    {
        commands.push(SyncCommandSpec {
            name: "database_update",
            command: options.notmuch_database_update_command.clone(),
        });
    }
    commands
}

fn open_compose(widgets: &Widgets, state: &SharedState) {
    show_compose_view(widgets);
    set_active_draft(widgets, state, None);
    move_compose_cursor_to_start(widgets);
    {
        let mut state = state.borrow_mut();
        state.active_pane = ActivePane::Message;
        state.compose_fields.in_reply_to = None;
        state.compose_fields.references.clear();
        state.last_operation = Some("opened composer".to_string());
    }
    if state.borrow().input_mode == InputMode::Insert {
        widgets.compose_to.grab_focus();
    } else {
        focus_active_pane(widgets, state);
    }
    update_debug(widgets, state);
}

fn reply_selected(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    kind: ReplyKind,
) {
    let Some(message) = state.borrow().selected_message.clone() else {
        widgets
            .status_label
            .set_text("No selected message to reply to");
        return;
    };
    let Some(path) = message.filenames.first() else {
        widgets
            .status_label
            .set_text("Selected message has no filename");
        return;
    };
    let Some(id) = identity(options) else {
        widgets
            .status_label
            .set_text("No identity configured for reply");
        return;
    };
    match parse_file(path) {
        Ok(parsed) => {
            let mut own = options.other_email.clone();
            if let Some(email) = &options.primary_email {
                own.push(email.clone());
            }
            let reply = build_reply(&parsed, &id, &own, kind);
            fill_composer(widgets, state, reply);
            widgets.status_label.set_text("Reply composer opened");
        }
        Err(err) => widgets
            .status_label
            .set_text(&format!("Reply parse failed: {err}")),
    }
    update_debug(widgets, state);
}

fn forward_selected(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let Some(message) = state.borrow().selected_message.clone() else {
        widgets
            .status_label
            .set_text("No selected message to forward");
        return;
    };
    let Some(path) = message.filenames.first() else {
        widgets
            .status_label
            .set_text("Selected message has no filename");
        return;
    };
    let Some(id) = identity(options) else {
        widgets
            .status_label
            .set_text("No identity configured for forward");
        return;
    };
    match parse_file(path) {
        Ok(parsed) => fill_composer(widgets, state, build_inline_forward(&parsed, &id)),
        Err(err) => widgets
            .status_label
            .set_text(&format!("Forward parse failed: {err}")),
    }
    update_debug(widgets, state);
}

fn forward_as_attachment_selected(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let Some(message) = state.borrow().selected_message.clone() else {
        widgets
            .status_label
            .set_text("No selected message to forward");
        return;
    };
    let Some(path) = message.filenames.first() else {
        widgets
            .status_label
            .set_text("Selected message has no filename");
        return;
    };
    let Some(id) = identity(options) else {
        widgets
            .status_label
            .set_text("No identity configured for forward");
        return;
    };
    let result = (|| -> anyhow::Result<ComposedMessage> {
        let raw = std::fs::read(path)?;
        let parsed = notm_mail::mime::parse_rfc5322(&raw)?;
        Ok(build_attachment_forward(&parsed, &id, raw))
    })();
    match result {
        Ok(message) => {
            fill_composer(widgets, state, message);
            widgets
                .status_label
                .set_text("Forward-as-attachment composer opened");
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Forward-as-attachment failed: {err}"));
        }
    }
    update_debug(widgets, state);
}

fn fill_composer(widgets: &Widgets, state: &SharedState, message: ComposedMessage) {
    show_compose_view(widgets);
    set_active_draft(widgets, state, None);
    widgets.compose_from.set_text(&message.from);
    widgets.compose_to.set_text(&message.to.join(", "));
    widgets.compose_cc.set_text(&message.cc.join(", "));
    widgets.compose_bcc.set_text(&message.bcc.join(", "));
    widgets.compose_subject.set_text(&message.subject);
    widgets.compose_body.buffer().set_text(&message.body);
    move_compose_cursor_to_start(widgets);
    let mut fields = compose_fields(widgets, state);
    fields.in_reply_to = message.in_reply_to;
    fields.references = message.references;
    match cache_composer_attachments(&message.attachments) {
        Ok(paths) => {
            fields.attachments = paths;
            update_attachment_label(widgets, &fields.attachments);
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Attachment cache failed: {err}"));
        }
    }
    {
        let mut state = state.borrow_mut();
        state.compose_fields = fields;
        state.active_pane = ActivePane::Message;
    }
    if state.borrow().input_mode == InputMode::Insert {
        widgets.compose_to.grab_focus();
    } else {
        focus_active_pane(widgets, state);
    }
}

fn cache_composer_attachments(attachments: &[AttachmentInput]) -> anyhow::Result<Vec<String>> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    let dir = default_attachment_cache_dir();
    std::fs::create_dir_all(&dir)?;
    attachments
        .iter()
        .map(|attachment| {
            if let Some(source_path) = &attachment.source_path
                && source_path.exists()
            {
                return Ok(source_path.display().to_string());
            }
            let path = dir.join(format!(
                "{}-{}",
                Uuid::new_v4(),
                safe_filename(&attachment.filename)
            ));
            std::fs::write(&path, &attachment.bytes)?;
            Ok(path.display().to_string())
        })
        .collect()
}

fn default_attachment_cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("target/notm-cache"));
    base.join("notm/compose-attachments")
}

fn send_compose(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let fields = compose_fields(widgets, state);
    state.borrow_mut().compose_fields = fields.clone();
    let message = match composed_message_from_fields(&fields) {
        Ok(message) => message,
        Err(err) => {
            widgets
                .status_label
                .set_text(&format!("Compose message build failed: {err}"));
            state.borrow_mut().last_error = Some(err.to_string());
            update_debug(widgets, state);
            return;
        }
    };
    let message_for_persistence = message.clone();
    let result = send_message_with_config(options, message);
    match result {
        Ok(mut report) => {
            widgets.status_label.set_text(if report.accepted {
                if report.captured_path.is_some() && options.send_command.is_none() {
                    "Fake send captured"
                } else {
                    "Send accepted"
                }
            } else {
                "Send failed"
            });
            if report.accepted {
                match persist_sent_message(options, &message_for_persistence) {
                    Ok(Some(persisted)) => {
                        if report.captured_path.is_none() {
                            report.captured_path = Some(persisted.path.display().to_string());
                        }
                        state.borrow_mut().last_operation = Some(format!(
                            "saved sent message to {}{}",
                            persisted.path.display(),
                            persisted
                                .indexed_message_id
                                .as_deref()
                                .map(|id| format!(" and indexed {id}"))
                                .unwrap_or_default()
                        ));
                    }
                    Ok(None) => {}
                    Err(err) => {
                        state.borrow_mut().last_error = Some(err.to_string());
                        widgets
                            .status_label
                            .set_text(&format!("Send accepted; sent save/index failed: {err}"));
                    }
                }
            }
            state.borrow_mut().last_send_report = Some(report);
            if state
                .borrow()
                .last_send_report
                .as_ref()
                .map(|r| r.accepted)
                .unwrap_or(false)
            {
                if let Some(draft) = state.borrow().active_draft.clone()
                    && let Err(err) = delete_draft_source(options, &draft)
                {
                    state.borrow_mut().last_error = Some(err.to_string());
                    widgets
                        .status_label
                        .set_text(&format!("Send accepted; draft delete failed: {err}"));
                }
                let _ = clear_draft_file(&widgets.draft_path);
                clear_draft_widgets(widgets, state);
            }
        }
        Err(err) => {
            widgets
                .status_label
                .set_text(&format!("Send failed: {err}"));
            state.borrow_mut().last_error = Some(err.to_string());
        }
    }
    update_debug(widgets, state);
}

fn send_message_with_config(
    options: &LaunchOptions,
    message: ComposedMessage,
) -> anyhow::Result<notm_mail::SendReport> {
    if !options.send_enabled {
        anyhow::bail!("send.enabled is false");
    }
    if let Some(command) = &options.send_command {
        let rt = tokio::runtime::Runtime::new()?;
        let transport = ExternalCommandTransport {
            command: command.clone(),
            args: options.send_args.clone(),
            mode: options.send_mode.clone(),
            working_dir: options.send_working_dir.clone(),
            env: options.send_env.clone(),
            timeout: Duration::from_secs(options.send_timeout_seconds),
        };
        return rt.block_on(transport.send(message));
    }
    if let Some(capture_dir) = &options.fake_send_capture_dir {
        let rt = tokio::runtime::Runtime::new()?;
        let transport = FakeSendTransport {
            capture_dir: capture_dir.clone(),
        };
        return rt.block_on(transport.send(message));
    }
    anyhow::bail!(
        "send.command is not configured; refusing to fake-send outside fixture/test mode"
    );
}

#[derive(Debug, Clone)]
struct PersistedMessage {
    path: PathBuf,
    indexed_message_id: Option<String>,
}

fn persist_sent_message(
    options: &LaunchOptions,
    message: &ComposedMessage,
) -> anyhow::Result<Option<PersistedMessage>> {
    if !options.save_sent {
        return Ok(None);
    }
    let maildir = options
        .sent_maildir
        .clone()
        .or_else(|| options.database_path.as_ref().map(|path| path.join("Sent")))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "send.save_sent=true but no sent_maildir or database path is configured"
            )
        })?;
    let path = save_rfc5322_to_maildir(&maildir, message, "S")?;
    let indexed_message_id = if options.index_sent_after_send {
        Some(index_message_file(options, &path, &options.sent_tags)?)
    } else {
        None
    };
    Ok(Some(PersistedMessage {
        path,
        indexed_message_id,
    }))
}

fn persist_draft_message(
    options: &LaunchOptions,
    message: &ComposedMessage,
) -> anyhow::Result<Option<PersistedMessage>> {
    if !options.save_drafts_to_maildir {
        return Ok(None);
    }
    let maildir = options
        .draft_maildir
        .clone()
        .or_else(|| {
            options
                .database_path
                .as_ref()
                .map(|path| path.join("Drafts"))
        })
        .or_else(|| default_database_maildir(options, "Drafts").ok())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "drafts.save_maildir=true but no draft maildir or database path is configured"
            )
        })?;
    let path = save_rfc5322_to_maildir(&maildir, message, "D")?;
    let indexed_message_id = if options.index_draft_after_save {
        Some(index_message_file(options, &path, &options.draft_tags)?)
    } else {
        None
    };
    Ok(Some(PersistedMessage {
        path,
        indexed_message_id,
    }))
}

fn default_database_maildir(options: &LaunchOptions, name: &str) -> anyhow::Result<PathBuf> {
    let db = Database::open(&open_config(options), DatabaseMode::ReadOnly)?;
    Ok(PathBuf::from(db.path()).join(name))
}

fn save_rfc5322_to_maildir(
    maildir: &Path,
    message: &ComposedMessage,
    flags: &str,
) -> anyhow::Result<PathBuf> {
    let tmp = maildir.join("tmp");
    let cur = maildir.join("cur");
    let new = maildir.join("new");
    std::fs::create_dir_all(&tmp)?;
    std::fs::create_dir_all(&cur)?;
    std::fs::create_dir_all(&new)?;
    let unique = format!(
        "{}.{}.{}.notm",
        Utc::now().timestamp(),
        std::process::id(),
        Uuid::new_v4()
    );
    let tmp_path = tmp.join(&unique);
    std::fs::write(&tmp_path, message.to_rfc5322())?;
    let final_path = cur.join(format!("{unique}:2,{flags}"));
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(final_path)
}

fn index_message_file(
    options: &LaunchOptions,
    path: &Path,
    tags: &[String],
) -> anyhow::Result<String> {
    let db = Database::open(&open_config(options), DatabaseMode::ReadWrite)?;
    let tag_refs = tags.iter().map(String::as_str).collect::<Vec<_>>();
    Ok(db.index_file_with_tags(path, &tag_refs)?)
}

fn setup_automation(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    saved_store: &SavedSearchStore,
) {
    let (tx, rx) = mpsc::channel::<AutomationRequest>();
    let socket = options
        .automation_socket
        .clone()
        .unwrap_or_else(automation::default_socket_path);
    let token = options
        .automation_token
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    if let Err(err) = automation::spawn(
        AutomationConfig {
            socket_path: socket.clone(),
            token: token.clone(),
        },
        tx,
    ) {
        state.borrow_mut().last_error = Some(format!("automation failed: {err}"));
    } else {
        eprintln!(
            "notm automation socket={} token={}",
            socket.display(),
            token
        );
        widgets
            .status_label
            .set_text(&format!("Automation: {}", socket.display()));
    }
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    let saved = saved_store.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        while let Ok(req) = rx.try_recv() {
            handle_automation_request(&opts, &w, &st, &undo, &saved, req);
        }
        gtk::glib::ControlFlow::Continue
    });
}

fn handle_automation_request(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    saved_store: &SavedSearchStore,
    req: AutomationRequest,
) {
    let result = match req.command.as_str() {
        "health" => json!({"ok": true, "state": "running"}),
        "app_state" => json!({"ok": true, "state": &*state.borrow()}),
        "screenshot" => {
            let name = req
                .args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("automation.png");
            match screenshot::capture_screenshot(&options.screenshot_dir, name) {
                Ok(path) => {
                    state.borrow_mut().screenshot_path = Some(path.clone());
                    json!({"ok": true, "screenshot_path": path})
                }
                Err(err) => json!({"ok": false, "error": err.to_string()}),
            }
        }
        "focus_search" => {
            widgets.search_entry.grab_focus();
            json!({"ok": true})
        }
        "focus_compose_field" => {
            let field = req
                .args
                .get("field")
                .and_then(|v| v.as_str())
                .unwrap_or("to");
            let entry = match field {
                "from" => &widgets.compose_from,
                "cc" => &widgets.compose_cc,
                "bcc" => &widgets.compose_bcc,
                "subject" => &widgets.compose_subject,
                _ => &widgets.compose_to,
            };
            set_input_mode(
                widgets,
                state,
                InputMode::Insert,
                "Insert mode (automation focus)",
            );
            entry.grab_focus();
            if matches!(field, "to" | "cc" | "bcc") {
                set_active_address_entry(widgets, entry);
                widgets
                    .active_address_field
                    .set(recipient_field_for_entry(widgets, entry));
                place_address_suggestions_after_entry(widgets, entry);
            }
            json!({"ok": true, "field": field})
        }
        "entry_state" => {
            json!({
                "ok": true,
                "search": widgets.search_entry.text().to_string(),
                "custom_tag": widgets.custom_tag_entry.text().to_string(),
                "tag_command": widgets.tag_command_entry.text().to_string(),
                "compose_fields": compose_fields(widgets, state),
                "search_suggestions_visible": widgets.search_suggestions_list.is_visible(),
                "address_suggestions_visible": widgets.address_suggestions_list.is_visible(),
            })
        }
        "set_search_query" => {
            let query = req
                .args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            widgets.search_entry.set_text(query);
            state.borrow_mut().current_query = query.to_string();
            json!({"ok": true, "current_query": query})
        }
        "run_search" => {
            let query = if let Some(q) = req.args.get("query").and_then(|v| v.as_str()) {
                q.to_string()
            } else {
                widgets.search_entry.text().to_string()
            };
            run_search(options, widgets, state, &query);
            json!({"ok": true, "state": &*state.borrow()})
        }
        "load_more_threads" => {
            load_more_threads(options, widgets, state);
            json!({"ok": state.borrow().last_error.is_none(), "state": &*state.borrow()})
        }
        "thread_page_info" => {
            let state = state.borrow();
            json!({
                "ok": true,
                "loaded": state.thread_loaded_count,
                "window_offset": state.thread_window_offset,
                "window_start": if state.thread_list_items.is_empty() { 0 } else { state.thread_window_offset + 1 },
                "window_end": state.thread_window_offset + state.thread_list_items.len(),
                "total": state.thread_total_count,
                "page_size": state.thread_page_size,
                "can_load_more": state.can_load_more_threads,
                "current_query": state.current_query,
            })
        }
        "scroll_thread_list_to_bottom" => {
            let adjustment = widgets.thread_scrolled.vadjustment();
            let before_loaded = state.borrow().thread_loaded_count;
            let target = (adjustment.upper() - adjustment.page_size()).max(0.0);
            adjustment.set_value(target);
            if state.borrow().thread_loaded_count == before_loaded {
                let at_bottom = adjustment.upper() <= adjustment.page_size() + 24.0
                    || adjustment.value() + adjustment.page_size() + 24.0 >= adjustment.upper();
                if at_bottom && state.borrow().can_load_more_threads {
                    load_more_threads(options, widgets, state);
                }
            }
            let state = state.borrow();
            json!({
                "ok": true,
                "loaded": state.thread_loaded_count,
                "total": state.thread_total_count,
                "page_size": state.thread_page_size,
                "can_load_more": state.can_load_more_threads,
                "scroll_value": adjustment.value(),
                "scroll_upper": adjustment.upper(),
                "scroll_page_size": adjustment.page_size(),
            })
        }
        "select_saved_search" => {
            let name = req
                .args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Inbox");
            if let Some(saved) = saved_store
                .borrow()
                .iter()
                .find(|saved| saved.name.eq_ignore_ascii_case(name))
                .cloned()
            {
                widgets.saved_name_entry.set_text(&saved.name);
                widgets.saved_query_entry.set_text(&saved.query);
                widgets.search_entry.set_text(&saved.query);
                state.borrow_mut().visible_saved_search = Some(saved.name.clone());
                run_search(options, widgets, state, &saved.query);
            } else {
                open_saved_search_name(options, widgets, state, name);
            }
            json!({"ok": true, "state": &*state.borrow()})
        }
        "custom_saved_searches" => {
            json!({"ok": true, "custom_saved_searches": &*saved_store.borrow()})
        }
        "save_custom_search" => {
            let name = req
                .args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let query = req
                .args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            widgets.saved_name_entry.set_text(name);
            widgets.saved_query_entry.set_text(query);
            match save_custom_search_from_entries(options, widgets, state, saved_store) {
                Ok(()) => {
                    json!({"ok": true, "custom_saved_searches": &*saved_store.borrow(), "state": &*state.borrow()})
                }
                Err(err) => json!({"ok": false, "error": err.to_string()}),
            }
        }
        "delete_custom_search" => {
            let name = req
                .args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            widgets.saved_name_entry.set_text(name);
            match delete_custom_search_from_entries(options, widgets, state, saved_store) {
                Ok(()) => {
                    json!({"ok": true, "custom_saved_searches": &*saved_store.borrow()})
                }
                Err(err) => json!({"ok": false, "error": err.to_string()}),
            }
        }
        "select_thread_by_index" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if let Some(row) = widgets.thread_list.row_at_index(index as i32) {
                widgets.thread_list.select_row(Some(&row));
            }
            select_thread_by_index(options, widgets, state, index, false);
            json!({"ok": true, "selected_thread": state.borrow().selected_thread})
        }
        "select_message_by_index" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            select_message_by_index(options, widgets, state, index);
            json!({"ok": true, "selected_message": state.borrow().selected_message})
        }
        "open_selected_thread" => {
            let idx = widgets
                .thread_list
                .selected_row()
                .map(|r| r.index())
                .unwrap_or(0) as usize;
            open_thread_by_index(options, widgets, state, idx);
            json!({"ok": true, "state": &*state.borrow()})
        }
        "archive_selected" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec![],
                    remove: vec!["inbox".to_string()],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "mark_read_selected" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec![],
                    remove: vec!["unread".to_string()],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "mark_unread_selected" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["unread".to_string()],
                    remove: vec![],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "flag_selected" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["flagged".to_string()],
                    remove: vec![],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "unflag_selected" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec![],
                    remove: vec!["flagged".to_string()],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "trash_selected" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["trash".to_string()],
                    remove: vec!["inbox".to_string(), "spam".to_string()],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "spam_selected" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["spam".to_string()],
                    remove: vec!["inbox".to_string()],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "set_custom_tag_entry" => {
            let tag = req
                .args
                .get("tag")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            widgets.custom_tag_entry.set_text(tag);
            json!({"ok": true, "tag": tag})
        }
        "add_custom_tag_from_entry" => {
            apply_custom_tag_from_entry(options, widgets, state, undo_state, true);
            json!({"ok": true, "state": &*state.borrow()})
        }
        "remove_custom_tag_from_entry" => {
            apply_custom_tag_from_entry(options, widgets, state, undo_state, false);
            json!({"ok": true, "state": &*state.borrow()})
        }
        "tag_selected" | "add_tag_selected" | "remove_tag_selected" => {
            let mut add = string_array_arg(&req.args, "add");
            let mut remove = string_array_arg(&req.args, "remove");
            if let Some(tag) = req.args.get("tag").and_then(|v| v.as_str()) {
                match req.command.as_str() {
                    "remove_tag_selected" => remove.push(tag.to_string()),
                    _ => add.push(tag.to_string()),
                }
            }
            if req.command == "add_tag_selected" && add.is_empty() {
                add = string_array_arg(&req.args, "tags");
            }
            if req.command == "remove_tag_selected" && remove.is_empty() {
                remove = string_array_arg(&req.args, "tags");
            }
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add,
                    remove,
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "undo_last_tag" => {
            undo_last_tag(options, widgets, state, undo_state);
            json!({"ok": true, "state": &*state.borrow()})
        }
        "run_manual_sync" => {
            run_manual_sync(options, widgets, state);
            json!({"ok": state.borrow().last_error.is_none(), "state": &*state.borrow()})
        }
        "open_compose" => {
            open_compose(widgets, state);
            json!({"ok": true, "compose_fields": state.borrow().compose_fields})
        }
        "compose_set_from"
        | "compose_set_to"
        | "compose_set_cc"
        | "compose_set_bcc"
        | "compose_set_subject"
        | "compose_set_body" => {
            let value = req
                .args
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            match req.command.as_str() {
                "compose_set_from" => widgets.compose_from.set_text(value),
                "compose_set_to" => widgets.compose_to.set_text(value),
                "compose_set_cc" => widgets.compose_cc.set_text(value),
                "compose_set_bcc" => widgets.compose_bcc.set_text(value),
                "compose_set_subject" => widgets.compose_subject.set_text(value),
                "compose_set_body" => {
                    widgets.compose_body.buffer().set_text(value);
                    move_compose_cursor_to_start(widgets);
                }
                _ => {}
            }
            state.borrow_mut().compose_fields = compose_fields(widgets, state);
            json!({"ok": true, "compose_fields": state.borrow().compose_fields})
        }
        "compose_add_attachment" => {
            if let Some(path) = req.args.get("path").and_then(|v| v.as_str()) {
                add_attachment_path(widgets, state, PathBuf::from(path));
                json!({"ok": true, "compose_fields": state.borrow().compose_fields})
            } else {
                json!({"ok": false, "error": "missing attachment path"})
            }
        }
        "get_address_suggestions" => {
            let prefix = req
                .args
                .get("prefix")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            json!({
                "ok": true,
                "suggestions": matching_address_suggestions(prefix, &state.borrow().address_suggestions, 20)
            })
        }
        "select_address_suggestion_by_index" => {
            let input = widgets.compose_to.text().to_string();
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let suggestions =
                matching_address_suggestions(&input, &state.borrow().address_suggestions, 20);
            if let Some(suggestion) = suggestions.get(index) {
                apply_recipient_suggestion(&widgets.compose_to, suggestion);
                state.borrow_mut().compose_fields = compose_fields(widgets, state);
                json!({"ok": true, "suggestion": suggestion, "compose_fields": state.borrow().compose_fields})
            } else {
                json!({"ok": false, "error": "address suggestion index not found"})
            }
        }
        "autocomplete_recipient" => {
            let field = req
                .args
                .get("field")
                .and_then(|v| v.as_str())
                .unwrap_or("to");
            let entry = match field {
                "cc" => &widgets.compose_cc,
                "bcc" => &widgets.compose_bcc,
                "from" => &widgets.compose_from,
                _ => &widgets.compose_to,
            };
            let completed = apply_recipient_completion(entry, state);
            update_address_suggestions_label(widgets, state, &entry.text());
            state.borrow_mut().compose_fields = compose_fields(widgets, state);
            json!({"ok": completed, "compose_fields": state.borrow().compose_fields})
        }
        "save_draft" => match save_current_draft(options, widgets, state) {
            Ok(report) => {
                refresh_draft_list(widgets);
                let destination = report
                    .maildir_path
                    .as_ref()
                    .or(report.local_path.as_ref())
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "draft store".to_string());
                widgets
                    .status_label
                    .set_text(&format!("Draft saved to {destination}"));
                json!({"ok": true, "report": report})
            }
            Err(err) => json!({"ok": false, "error": err.to_string()}),
        },
        "list_drafts" => {
            let drafts = list_named_drafts(&widgets.drafts_dir)
                .into_iter()
                .map(|(path, fields)| json!({"path": path, "fields": fields}))
                .collect::<Vec<_>>();
            json!({"ok": true, "drafts": drafts})
        }
        "select_draft_by_index" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as i32;
            if let Some(row) = widgets.draft_list.row_at_index(index) {
                widgets.draft_list.select_row(Some(&row));
                json!({"ok": true})
            } else {
                json!({"ok": false, "error": "draft index not found"})
            }
        }
        "load_selected_draft" => match load_selected_named_draft(widgets, state) {
            Ok(()) => json!({"ok": true, "compose_fields": state.borrow().compose_fields}),
            Err(err) => json!({"ok": false, "error": err.to_string()}),
        },
        "delete_selected_draft" => match delete_selected_named_draft(widgets) {
            Ok(()) => json!({"ok": true}),
            Err(err) => json!({"ok": false, "error": err.to_string()}),
        },
        "delete_active_draft" | "delete_local_draft" => {
            delete_active_draft_from_ui(options, widgets, state);
            json!({"ok": state.borrow().last_error.is_none(), "compose_fields": state.borrow().compose_fields, "active_draft": state.borrow().active_draft, "last_error": state.borrow().last_error})
        }
        "load_draft" => {
            restore_draft_if_present(widgets, state);
            json!({"ok": true, "compose_fields": state.borrow().compose_fields})
        }
        "clear_draft" => {
            clear_draft_widgets(widgets, state);
            match clear_draft_file(&widgets.draft_path) {
                Ok(()) => json!({"ok": true, "compose_fields": state.borrow().compose_fields}),
                Err(err) => json!({"ok": false, "error": err.to_string()}),
            }
        }
        "compose_send" => {
            send_compose(options, widgets, state);
            json!({"ok": true, "last_send_report": state.borrow().last_send_report, "last_error": state.borrow().last_error})
        }
        "reply_selected" => {
            reply_selected(options, widgets, state, ReplyKind::Sender);
            json!({"ok": true, "compose_fields": state.borrow().compose_fields})
        }
        "reply_all_selected" => {
            reply_selected(options, widgets, state, ReplyKind::All);
            json!({"ok": true, "compose_fields": state.borrow().compose_fields})
        }
        "forward_selected" => {
            forward_selected(options, widgets, state);
            json!({"ok": true, "compose_fields": state.borrow().compose_fields})
        }
        "forward_as_attachment_selected" => {
            forward_as_attachment_selected(options, widgets, state);
            json!({"ok": true, "compose_fields": state.borrow().compose_fields, "last_error": state.borrow().last_error})
        }
        "toggle_debug_panel" => {
            widgets
                .debug_view
                .set_visible(!widgets.debug_view.is_visible());
            update_debug(widgets, state);
            json!({"ok": true, "debug_visible": widgets.debug_view.is_visible()})
        }
        "show_raw_source" | "open_raw_source" => {
            show_raw_source(options, widgets, state);
            json!({"ok": state.borrow().last_error.is_none(), "last_error": state.borrow().last_error})
        }
        "show_full_headers" | "full_headers" => {
            show_full_headers(options, widgets, state);
            json!({"ok": state.borrow().last_error.is_none(), "last_error": state.borrow().last_error})
        }
        "show_rendered_thread" | "show_text_thread" | "text_view" => {
            state.borrow_mut().prefer_html_view = false;
            show_rendered_selected_thread(options, widgets, state);
            json!({"ok": true, "state": &*state.borrow()})
        }
        "toggle_text_visual" | "toggle_visual_html" => {
            toggle_text_visual_view(options, widgets, state);
            json!({
                "ok": state.borrow().last_error.is_none(),
                "html_view": html_view_state(options, widgets, state),
                "last_error": state.borrow().last_error,
            })
        }
        "show_visual_html" | "show_html_visual" | "visual_html" => {
            state.borrow_mut().prefer_html_view = true;
            show_visual_html_selected_message(options, widgets, state);
            json!({
                "ok": state.borrow().last_error.is_none(),
                "html_view": html_view_state(options, widgets, state),
                "last_error": state.borrow().last_error,
            })
        }
        "html_scroll_state" => html_scroll_state(widgets),
        "scroll_html_view_lines" => {
            let lines = req
                .args
                .get("lines")
                .and_then(|value| value.as_f64())
                .unwrap_or(1.0);
            scroll_html_view_lines(widgets, lines);
            spin_main_context_for(Duration::from_millis(150));
            html_scroll_state(widgets)
        }
        "image_policy" => {
            activate_image_policy_button(options, widgets, state);
            json!({
                "ok": state.borrow().last_error.is_none(),
                "html_view": html_view_state(options, widgets, state),
                "trusted_image_senders": state.borrow().trusted_image_senders,
                "last_error": state.borrow().last_error,
            })
        }
        "load_images_once" | "show_visual_html_with_images" => {
            show_visual_html_with_image_policy(options, widgets, state, ImagePolicy::Once);
            json!({
                "ok": state.borrow().last_error.is_none(),
                "html_view": html_view_state(options, widgets, state),
                "last_error": state.borrow().last_error,
            })
        }
        "trust_sender_images" | "always_load_sender_images" => {
            show_visual_html_with_image_policy(options, widgets, state, ImagePolicy::TrustSender);
            json!({
                "ok": state.borrow().last_error.is_none(),
                "html_view": html_view_state(options, widgets, state),
                "trusted_image_senders": state.borrow().trusted_image_senders,
                "last_error": state.borrow().last_error,
            })
        }
        "trusted_image_senders" => {
            json!({"ok": true, "trusted_image_senders": state.borrow().trusted_image_senders})
        }
        "html_view_state" => html_view_state(options, widgets, state),
        "toggle_quote_collapse" => {
            toggle_quote_collapse(options, widgets, state);
            json!({"ok": true, "quote_collapse_enabled": state.borrow().quote_collapse_enabled})
        }
        "message_view_text" => {
            json!({"ok": true, "text": text_view_text(&widgets.message_view)})
        }
        "thread_ui_details" => {
            json!({"ok": true, "thread_details": state.borrow().thread_details})
        }
        "copy_message_id" => {
            copy_selected_message_id(widgets, state);
            json!({"ok": true, "selected_message": state.borrow().selected_message})
        }
        "copy_thread_id" => {
            copy_selected_thread_id(widgets, state);
            json!({"ok": true, "selected_thread": state.borrow().selected_thread})
        }
        "open_command_palette" => {
            show_command_palette(options, widgets, state, undo_state);
            json!({"ok": true})
        }
        "open_shortcuts" | "show_shortcuts" => {
            show_shortcuts_overlay(widgets);
            json!({"ok": true})
        }
        "open_settings" => {
            show_settings(widgets, options);
            json!({"ok": true})
        }
        "save_settings" => {
            let default_query = req
                .args
                .get("default_query")
                .and_then(|v| v.as_str())
                .unwrap_or(&options.default_query);
            let page_size = req
                .args
                .get("page_size")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(options.page_size);
            let send_command = req
                .args
                .get("send_command")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            match persist_basic_settings(options, default_query, page_size, send_command) {
                Ok(()) => json!({"ok": true, "app_config_path": options.app_config_path}),
                Err(err) => json!({"ok": false, "error": err.to_string()}),
            }
        }
        "run_command" => {
            let command = req
                .args
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            run_named_command(command, options, widgets, state, undo_state)
        }
        "attachment_list_items" => {
            json!({"ok": true, "attachments": &*widgets.attachment_items.borrow()})
        }
        "select_attachment_by_index" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as i32;
            if let Some(row) = widgets.attachment_list.row_at_index(index) {
                widgets.attachment_list.select_row(Some(&row));
                json!({"ok": true, "selected": selected_thread_attachment(widgets)})
            } else {
                json!({"ok": false, "error": "attachment index not found"})
            }
        }
        "save_selected_attachment" | "save_attachment" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let dir = req
                .args
                .get("dir")
                .and_then(|v| v.as_str())
                .map(PathBuf::from);
            let result = widgets
                .attachment_items
                .borrow()
                .get(index)
                .cloned()
                .map(|item| save_thread_attachment(widgets, state, &item, dir.as_deref()))
                .unwrap_or_else(|| save_selected_attachment(widgets, state, index, dir.as_deref()));
            match result {
                Ok(path) => json!({"ok": true, "path": path}),
                Err(err) => json!({"ok": false, "error": err.to_string()}),
            }
        }
        "open_selected_attachment" | "open_attachment" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if let Some(item) = widgets.attachment_items.borrow().get(index).cloned() {
                match save_thread_attachment(widgets, state, &item, None) {
                    Ok(path) => open_saved_attachment_path(widgets, state, path),
                    Err(err) => {
                        state.borrow_mut().last_error = Some(err.to_string());
                        widgets
                            .status_label
                            .set_text(&format!("Open attachment failed: {err}"));
                    }
                }
            } else {
                open_selected_attachment(widgets, state, index);
            }
            json!({"ok": true, "last_error": state.borrow().last_error})
        }
        "get_logs" => {
            json!({"ok": true, "recent_error": state.borrow().last_error, "last_operation": state.borrow().last_operation})
        }
        other => json!({"ok": false, "error": format!("unknown automation command: {other}")}),
    };
    let _ = req.response.send(result);
}

fn string_array_arg(args: &serde_json::Value, name: &str) -> Vec<String> {
    match args.get(name) {
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
            .collect(),
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
            vec![value.trim().to_string()]
        }
        _ => Vec::new(),
    }
}

fn run_named_command(
    command: &str,
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) -> serde_json::Value {
    match command {
        "search" => {
            let query = widgets.search_entry.text().to_string();
            run_search(options, widgets, state, &query);
            json!({"ok": true, "state": &*state.borrow()})
        }
        "inbox" => {
            open_saved_search_name(options, widgets, state, "Inbox");
            json!({"ok": true, "state": &*state.borrow()})
        }
        "unread" => {
            open_saved_search_name(options, widgets, state, "Unread");
            json!({"ok": true, "state": &*state.borrow()})
        }
        "flagged" => {
            open_saved_search_name(options, widgets, state, "Flagged");
            json!({"ok": true, "state": &*state.borrow()})
        }
        "sent" => {
            open_saved_search_name(options, widgets, state, "Sent");
            json!({"ok": true, "state": &*state.borrow()})
        }
        "all" => {
            open_saved_search_name(options, widgets, state, "All");
            json!({"ok": true, "state": &*state.borrow()})
        }
        "compose" => {
            open_compose(widgets, state);
            json!({"ok": true, "compose_fields": state.borrow().compose_fields})
        }
        "reply" => {
            reply_selected(options, widgets, state, ReplyKind::Sender);
            json!({"ok": true, "compose_fields": state.borrow().compose_fields})
        }
        "reply_all" | "reply all" => {
            reply_selected(options, widgets, state, ReplyKind::All);
            json!({"ok": true, "compose_fields": state.borrow().compose_fields})
        }
        "forward" => {
            forward_selected(options, widgets, state);
            json!({"ok": true, "compose_fields": state.borrow().compose_fields})
        }
        "forward_attachment" | "forward_as_attachment" => {
            forward_as_attachment_selected(options, widgets, state);
            json!({"ok": true, "compose_fields": state.borrow().compose_fields, "last_error": state.borrow().last_error})
        }
        "visual_select" => {
            enter_visual_select_mode(widgets, state);
            json!({"ok": true, "state": &*state.borrow()})
        }
        "clear_visual_selection" => {
            clear_visual_selection(widgets, state);
            json!({"ok": true, "state": &*state.borrow()})
        }
        "archive" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec![],
                    remove: vec!["inbox".to_string()],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "mark_read" | "mark read" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec![],
                    remove: vec!["unread".to_string()],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "mark_unread" | "mark unread" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["unread".to_string()],
                    remove: vec![],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "flag" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["flagged".to_string()],
                    remove: vec![],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "unflag" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec![],
                    remove: vec!["flagged".to_string()],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "trash" => {
            tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["trash".to_string()],
                    remove: vec!["inbox".to_string(), "spam".to_string()],
                    sync_maildir_flags: options.sync_maildir_flags_after_tag_change,
                },
            );
            json!({"ok": true, "state": &*state.borrow()})
        }
        "toggle_debug_panel" | "debug" => {
            widgets
                .debug_view
                .set_visible(!widgets.debug_view.is_visible());
            update_debug(widgets, state);
            json!({"ok": true, "debug_visible": widgets.debug_view.is_visible()})
        }
        "raw_source" | "open_raw_source" => {
            show_raw_source(options, widgets, state);
            json!({"ok": state.borrow().last_error.is_none(), "last_error": state.borrow().last_error})
        }
        "full_headers" | "show_full_headers" => {
            show_full_headers(options, widgets, state);
            json!({"ok": state.borrow().last_error.is_none(), "last_error": state.borrow().last_error})
        }
        "text" | "rendered" | "show_rendered_thread" | "show_text_thread" => {
            state.borrow_mut().prefer_html_view = false;
            show_rendered_selected_thread(options, widgets, state);
            json!({"ok": true, "state": &*state.borrow()})
        }
        "toggle_text_visual" | "toggle_visual_html" => {
            toggle_text_visual_view(options, widgets, state);
            json!({
                "ok": state.borrow().last_error.is_none(),
                "html_view": html_view_state(options, widgets, state),
                "last_error": state.borrow().last_error,
            })
        }
        "visual_html" | "show_visual_html" | "show_html_visual" => {
            state.borrow_mut().prefer_html_view = true;
            show_visual_html_selected_message(options, widgets, state);
            json!({
                "ok": state.borrow().last_error.is_none(),
                "html_view": html_view_state(options, widgets, state),
                "last_error": state.borrow().last_error,
            })
        }
        "image_policy" => {
            activate_image_policy_button(options, widgets, state);
            json!({
                "ok": state.borrow().last_error.is_none(),
                "html_view": html_view_state(options, widgets, state),
                "trusted_image_senders": state.borrow().trusted_image_senders,
                "last_error": state.borrow().last_error,
            })
        }
        "load_images_once" => {
            show_visual_html_with_image_policy(options, widgets, state, ImagePolicy::Once);
            json!({
                "ok": state.borrow().last_error.is_none(),
                "html_view": html_view_state(options, widgets, state),
                "last_error": state.borrow().last_error,
            })
        }
        "trust_sender_images" | "always_load_sender_images" => {
            show_visual_html_with_image_policy(options, widgets, state, ImagePolicy::TrustSender);
            json!({
                "ok": state.borrow().last_error.is_none(),
                "html_view": html_view_state(options, widgets, state),
                "trusted_image_senders": state.borrow().trusted_image_senders,
                "last_error": state.borrow().last_error,
            })
        }
        "toggle_quote_collapse" | "collapse_quotes" => {
            toggle_quote_collapse(options, widgets, state);
            json!({"ok": true, "quote_collapse_enabled": state.borrow().quote_collapse_enabled})
        }
        "save_attachment" => match save_selected_attachment(widgets, state, 0, None) {
            Ok(path) => json!({"ok": true, "path": path}),
            Err(err) => json!({"ok": false, "error": err.to_string()}),
        },
        "open_attachment" => {
            open_selected_attachment(widgets, state, 0);
            json!({"ok": true, "last_error": state.borrow().last_error})
        }
        "copy_message_id" => {
            copy_selected_message_id(widgets, state);
            json!({"ok": true, "selected_message": state.borrow().selected_message})
        }
        "copy_thread_id" => {
            copy_selected_thread_id(widgets, state);
            json!({"ok": true, "selected_thread": state.borrow().selected_thread})
        }
        "settings" | "open_settings" => {
            show_settings(widgets, options);
            json!({"ok": true})
        }
        "shortcuts" | "show_shortcuts" => {
            show_shortcuts_overlay(widgets);
            json!({"ok": true})
        }
        "undo_last_tag" | "undo" => {
            undo_last_tag(options, widgets, state, undo_state);
            json!({"ok": true, "state": &*state.borrow()})
        }
        "sync" | "manual_sync" | "run_manual_sync" => {
            run_manual_sync(options, widgets, state);
            json!({"ok": state.borrow().last_error.is_none(), "state": &*state.borrow()})
        }
        "" => json!({"ok": false, "error": "missing command"}),
        other => json!({"ok": false, "error": format!("unknown command palette command: {other}")}),
    }
}

fn update_debug(widgets: &Widgets, state: &SharedState) {
    let s = state.borrow();
    let text = format!(
        "query: {}\nselected_thread: {}\nselected_message: {}\ndatabase_path: {}\ndatabase_revision: {}\nlast_operation: {}\nlast_error: {}\nautomation: {}\nlast_send: {}\n",
        s.current_query,
        s.selected_thread
            .as_ref()
            .map(|t| t.thread_id.as_str())
            .unwrap_or(""),
        s.selected_message
            .as_ref()
            .map(|m| m.message_id.as_str())
            .unwrap_or(""),
        s.database_path.as_deref().unwrap_or(""),
        s.database_revision
            .as_ref()
            .map(|r| format!("{} {}", r.revision, r.uuid))
            .unwrap_or_default(),
        s.last_operation.as_deref().unwrap_or(""),
        s.last_error.as_deref().unwrap_or(""),
        s.automation_enabled,
        s.last_send_report
            .as_ref()
            .map(|r| format!("accepted={} status={:?}", r.accepted, r.exit_status))
            .unwrap_or_default(),
    );
    widgets.debug_view.buffer().set_text(&text);
}

fn open_config(options: &LaunchOptions) -> OpenConfig {
    OpenConfig {
        database_path: options.database_path.clone(),
        config_path: options.config_path.clone(),
        profile: options.profile.clone(),
    }
}

fn identity(options: &LaunchOptions) -> Option<Identity> {
    options.primary_email.as_ref().map(|email| Identity {
        name: options.identity_name.clone(),
        email: email.clone(),
    })
}

fn button_flow(spacing: u32) -> gtk::FlowBox {
    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_min_children_per_line(1);
    flow.set_max_children_per_line(24);
    flow.set_column_spacing(spacing);
    flow.set_row_spacing(spacing);
    flow.set_hexpand(true);
    flow.set_valign(gtk::Align::Start);
    flow
}

fn menu_button_with_box(
    label: &str,
    widget_name: &str,
    state: &SharedState,
) -> (gtk::MenuButton, gtk::Box) {
    let button = gtk::MenuButton::new();
    button.set_label(label);
    button.set_widget_name(widget_name);
    let popover = gtk::Popover::new();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    connect_vim_menu_navigation(&menu, state);
    let focus_menu = menu.clone();
    popover.connect_show(move |_| {
        focus_first_menu_child(&focus_menu);
    });
    popover.set_child(Some(&menu));
    button.set_popover(Some(&popover));
    (button, menu)
}

fn connect_vim_menu_navigation(menu: &gtk::Box, state: &SharedState) {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let menu_for_keys = menu.clone();
    let st = state.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        if st.borrow().input_mode == InputMode::Insert {
            return gtk::glib::Propagation::Proceed;
        }
        if key == gtk::gdk::Key::j {
            menu_for_keys.child_focus(gtk::DirectionType::Down);
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::k {
            menu_for_keys.child_focus(gtk::DirectionType::Up);
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    menu.add_controller(controller);
}

fn focus_first_menu_child(menu: &gtk::Box) {
    let mut child = menu.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.is_focusable() {
            widget.grab_focus();
            return;
        }
    }
}

fn entry_with_placeholder(placeholder: &str) -> gtk::Entry {
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.set_widget_name(&format!("notm-compose-{}", placeholder.to_lowercase()));
    entry
}

fn read_compose_fields(widgets: &Widgets) -> ComposeFields {
    let buffer = widgets.compose_body.buffer();
    let body = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string();
    ComposeFields {
        from: widgets.compose_from.text().to_string(),
        to: widgets.compose_to.text().to_string(),
        cc: widgets.compose_cc.text().to_string(),
        bcc: widgets.compose_bcc.text().to_string(),
        subject: widgets.compose_subject.text().to_string(),
        body,
        attachments: Vec::new(),
        in_reply_to: None,
        references: Vec::new(),
    }
}

fn split_recipients(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(crate::theme::css());
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn show_command_palette(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) {
    let dialog = gtk::Dialog::builder()
        .title("notm command palette")
        .transient_for(&widgets.window)
        .modal(true)
        .default_width(560)
        .build();
    dialog.set_widget_name("notm-command-palette");
    let area = dialog.content_area();
    area.set_spacing(6);
    let entry = gtk::Entry::new();
    entry.set_widget_name("notm-command-palette-entry");
    entry.set_placeholder_text(Some(
        "Type a command: inbox, unread, search, compose, reply, forward_as_attachment, raw_source...",
    ));
    area.append(&entry);
    let help = gtk::Label::new(Some("Common commands"));
    help.add_css_class("heading");
    help.set_xalign(0.0);
    area.append(&help);
    for command in command_palette_commands() {
        let label = gtk::Label::new(Some(command));
        label.set_xalign(0.0);
        label.add_css_class("dim-label");
        area.append(&label);
    }
    let shortcut_help = gtk::Label::new(Some("Shortcuts"));
    shortcut_help.add_css_class("heading");
    shortcut_help.set_xalign(0.0);
    area.append(&shortcut_help);
    for (key, desc) in shortcuts::SHORTCUTS {
        let label = gtk::Label::new(Some(&format!("{key:<12} {desc}")));
        label.set_xalign(0.0);
        area.append(&label);
    }
    dialog.add_button("Run", gtk::ResponseType::Accept);
    dialog.add_button("Close", gtk::ResponseType::Close);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    let entry_for_response = entry.clone();
    dialog.connect_response(move |d, response| {
        if response == gtk::ResponseType::Accept {
            let command = entry_for_response.text().to_string();
            let result = run_named_command(command.trim(), &opts, &w, &st, &undo);
            if result
                .get("ok")
                .and_then(|ok| ok.as_bool())
                .unwrap_or(false)
            {
                w.status_label
                    .set_text(&format!("Command `{}` ran", command.trim()));
            } else {
                w.status_label.set_text(&format!(
                    "Command `{}` failed: {}",
                    command.trim(),
                    result
                ));
            }
        }
        d.close();
    });
    dialog.present();
    entry.grab_focus();
}

fn show_shortcuts_overlay(widgets: &Widgets) {
    let dialog = gtk::Dialog::builder()
        .title("notm keyboard shortcuts")
        .transient_for(&widgets.window)
        .modal(true)
        .default_width(520)
        .build();
    dialog.set_widget_name("notm-shortcuts-overlay");
    let area = dialog.content_area();
    area.set_spacing(6);
    let title = gtk::Label::new(Some("Keyboard shortcuts"));
    title.add_css_class("heading");
    title.set_xalign(0.0);
    area.append(&title);
    for (key, desc) in shortcuts::SHORTCUTS {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let key_label = gtk::Label::new(Some(key));
        key_label.set_widget_name(&format!("notm-shortcut-key-{}", widget_token(key)));
        key_label.set_xalign(0.0);
        key_label.set_width_chars(14);
        key_label.add_css_class("monospace");
        let desc_label = gtk::Label::new(Some(desc));
        desc_label.set_xalign(0.0);
        desc_label.set_hexpand(true);
        desc_label.set_wrap(true);
        row.append(&key_label);
        row.append(&desc_label);
        area.append(&row);
    }
    dialog.add_button("Close", gtk::ResponseType::Close);
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.present();
}

fn command_palette_commands() -> &'static [&'static str] {
    &[
        "inbox, unread, flagged, sent, trash, all",
        "search, compose, reply, reply_all, forward, forward_as_attachment",
        "archive, mark_read, mark_unread, flag, unflag, trash, undo",
        "visual_select, clear_visual_selection",
        "raw_source, full_headers, text, visual_html, image_policy, collapse_quotes",
        "save_attachment, open_attachment",
        "copy_message_id, copy_thread_id",
        "debug, settings, shortcuts, manual_sync (if Sync is enabled)",
    ]
}

fn show_settings(widgets: &Widgets, options: &LaunchOptions) {
    let dialog = gtk::Dialog::builder()
        .title("notm settings")
        .transient_for(&widgets.window)
        .modal(true)
        .default_width(820)
        .default_height(720)
        .build();
    dialog.set_widget_name("notm-settings-dialog");
    let area = dialog.content_area();
    area.set_spacing(8);

    let existing = read_settings_toml(options);
    let scrolled = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .min_content_height(560)
        .build();
    let form = gtk::Box::new(gtk::Orientation::Vertical, 10);
    form.set_margin_start(8);
    form.set_margin_end(24);
    form.set_margin_top(8);
    form.set_margin_bottom(8);
    scrolled.set_child(Some(&form));
    area.append(&scrolled);

    settings_section(&form, "Config files");
    settings_readonly_row(
        &form,
        "App config file",
        &options
            .app_config_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "not configured".to_string()),
        "This path is selected before the UI starts. Launch with --config or set the normal app config path to use another file.",
    );

    settings_section(&form, "Notmuch");
    let database_path = settings_path_row(
        &widgets.window,
        &form,
        "Database path",
        &option_path_text(&options.database_path),
        "Notmuch database/mail root. Blank means use libnotmuch/notmuch config resolution.",
        SettingsPathKind::Directory,
    );
    let notmuch_config_path = settings_path_row(
        &widgets.window,
        &form,
        "Notmuch config path",
        &option_path_text(&options.config_path),
        "Path to the notmuch config file. Blank means libnotmuch default.",
        SettingsPathKind::File,
    );
    let notmuch_profile = settings_entry_row(
        &form,
        "Profile",
        options.profile.as_deref().unwrap_or_default(),
        "Optional notmuch profile name. Blank means default profile.",
    );
    let default_query = settings_entry_row(
        &form,
        "Default query",
        &options.default_query,
        "Search run at startup.",
    );
    let excluded_tags = settings_entry_row(
        &form,
        "Excluded tags",
        &join_string_list(&options.excluded_tags),
        "Tags excluded from searches, comma separated.",
    );
    let open_readwrite_only_for_mutations = settings_check_row(
        &form,
        "Keep searches read-only",
        toml_bool(
            &existing,
            "notmuch",
            "open_readwrite_only_for_mutations",
            true,
        ),
        "Searches and message viewing open the database read-only. Notm switches to read/write only for actions that change tags or index saved sent/draft files. Leave this on.",
    );
    let sync_maildir_flags_after_tag_change = settings_check_row(
        &form,
        "Sync Maildir flags",
        options.sync_maildir_flags_after_tag_change,
        "After changing tags like unread or flagged, also update Maildir filename flags so other mail tools see the same read/star state.",
    );

    settings_section(&form, "Identity");
    let identity_name = settings_entry_row(
        &form,
        "Name",
        options.identity_name.as_deref().unwrap_or_default(),
        "Display name used when composing mail.",
    );
    let primary_email = settings_entry_row(
        &form,
        "Primary email",
        options.primary_email.as_deref().unwrap_or_default(),
        "Primary sender identity.",
    );
    let other_email = settings_entry_row(
        &form,
        "Other emails",
        &join_string_list(&options.other_email),
        "Alternate own addresses, comma separated; used for reply-all de-duplication.",
    );

    settings_section(&form, "UI");
    let theme = settings_combo_row(
        &form,
        "Theme preference",
        &[
            ("system", "System/default"),
            ("dark", "Dark"),
            ("light", "Light"),
        ],
        &toml_string(&existing, "ui", "theme").unwrap_or_else(|| "system".to_string()),
        "Stored theme preference. Current UI uses the app theme CSS; relaunch required.",
    );
    let page_size = settings_entry_row(
        &form,
        "Page size",
        &options.page_size.to_string(),
        "Number of threads loaded per search page.",
    );
    let thread_preview_lines = settings_entry_row(
        &form,
        "Thread preview lines",
        &toml_usize(&existing, "ui", "thread_preview_lines", 2).to_string(),
        "Stored preview-line preference. Relaunch required for all effects.",
    );
    let html_mode = settings_combo_row(
        &form,
        "HTML rendering",
        &[
            (
                "sanitize_then_render_text_fallback",
                "Text first; sanitized HTML available",
            ),
            ("visual_html_preferred", "Visual HTML first when available"),
        ],
        &options.html_mode,
        "Visual HTML is sanitized. Message scripts stay blocked; http/https/mailto links open externally.",
    );
    let start_maximized = settings_check_row(
        &form,
        "Start maximized",
        options.start_maximized,
        "Open the main window maximized on launch.",
    );
    let show_debug_panel = settings_check_row(
        &form,
        "Show debug panel",
        options.show_debug_panel,
        "Show the debug text panel by default.",
    );
    let remote_images = settings_check_row(
        &form,
        "Load remote images",
        options.remote_images,
        "If off, HTML mail starts with remote images blocked unless the sender is trusted.",
    );
    let trusted_image_senders = settings_entry_row(
        &form,
        "Trusted image senders",
        &join_string_list(&options.trusted_image_senders),
        "Senders whose remote images may load by default, comma separated.",
    );
    let hidden_tag_searches = settings_entry_row(
        &form,
        "Hidden tag searches",
        &join_string_list(&options.hidden_tag_searches),
        "Tag search buttons hidden from the sidebar, comma separated.",
    );

    settings_section(&form, "Send");
    let send_enabled = settings_check_row(
        &form,
        "Sending enabled",
        options.send_enabled,
        "Config flag for send support. A send command is still required outside fixture mode.",
    );
    let send_transport = settings_combo_row(
        &form,
        "Transport",
        &[("external", "External command")],
        &toml_string(&existing, "send", "transport").unwrap_or_else(|| "external".to_string()),
        "Transport type. Current supported normal value is external.",
    );
    let send_command = settings_path_row(
        &widgets.window,
        &form,
        "Command",
        &option_path_text(&options.send_command),
        "External send helper path, for example msmtp or a gmi wrapper.",
        SettingsPathKind::File,
    );
    let send_args = settings_entry_row(
        &form,
        "Arguments",
        &join_string_list(&options.send_args),
        "Extra send command args, comma separated. In command_template mode, include {file} where the temporary RFC5322 message path should go.",
    );
    let send_mode = settings_combo_row(
        &form,
        "Mode",
        &[
            ("auto", "Auto"),
            ("stdin_rfc5322", "Pipe RFC5322 on stdin"),
            ("file_arg", "Write temp file and pass path"),
            ("command_template", "Template args with {file}"),
        ],
        &transport_mode_name(&options.send_mode),
        "auto/stdin_rfc5322 pipe the RFC5322 message to stdin; file_arg appends a temporary message path; command_template replaces {file} inside args.",
    );
    let send_working_dir = settings_path_row(
        &widgets.window,
        &form,
        "Working directory",
        &option_path_text(&options.send_working_dir),
        "Optional working directory for the external send command.",
        SettingsPathKind::Directory,
    );
    let send_env = settings_entry_row(
        &form,
        "Environment",
        &format_env_map(&options.send_env),
        "Extra environment for the send command as KEY=value pairs, comma or newline separated.",
    );
    let send_timeout_seconds = settings_entry_row(
        &form,
        "Timeout seconds",
        &options.send_timeout_seconds.to_string(),
        "External send command timeout.",
    );
    let save_sent = settings_check_row(
        &form,
        "Save sent locally",
        options.save_sent,
        "Save sent messages into a configured local Maildir after send.",
    );
    let sent_maildir = settings_path_row(
        &widgets.window,
        &form,
        "Sent Maildir",
        &option_path_text(&options.sent_maildir),
        "Optional Maildir used when Save sent locally is enabled.",
        SettingsPathKind::Directory,
    );
    let sent_tags = settings_entry_row(
        &form,
        "Sent tags",
        &join_string_list(&options.sent_tags),
        "Tags applied to locally indexed sent messages, comma separated.",
    );
    let index_sent_after_send = settings_check_row(
        &form,
        "Index sent after send",
        options.index_sent_after_send,
        "Index saved sent messages in notmuch after sending.",
    );
    settings_section(&form, "Drafts");
    let save_drafts_to_maildir = settings_check_row(
        &form,
        "Save drafts to Maildir",
        options.save_drafts_to_maildir,
        "Explicit Save draft writes a local Maildir message tagged as draft.",
    );
    let draft_maildir = settings_path_row(
        &widgets.window,
        &form,
        "Draft Maildir",
        &option_path_text(&options.draft_maildir),
        "Optional local Maildir for saved drafts.",
        SettingsPathKind::Directory,
    );
    let draft_tags = settings_entry_row(
        &form,
        "Draft tags",
        &join_string_list(&options.draft_tags),
        "Tags applied to saved draft messages, comma separated.",
    );
    let index_draft_after_save = settings_check_row(
        &form,
        "Index draft after save",
        options.index_draft_after_save,
        "Index saved drafts in notmuch so tag:draft can find them.",
    );

    settings_section(&form, "Sync");
    settings_note(
        &form,
        "Sync means: run the receive command you define, then run the database update command you define. Notm does not guess lieer/offlineimap/mbsync/notmuch new commands. Leave this off if another service already handles mail sync.",
    );
    let sync_enabled = settings_check_row(
        &form,
        "Enable Sync button",
        options.sync_enabled,
        "Show a Sync button in the sidebar. Pressing it runs the enabled commands below.",
    );
    let manual_sync_label = settings_entry_row(
        &form,
        "Sync button label",
        &options.manual_sync_label,
        "Text shown on the sidebar Sync button.",
    );
    let external_receive_enabled = settings_check_row(
        &form,
        "Run receive command",
        options.external_receive_enabled,
        "When Sync runs, run the receive command below first.",
    );
    let external_receive_on_startup = settings_check_row(
        &form,
        "Run receive at startup",
        options.external_receive_on_startup,
        "Also run the receive command when notm starts, then refresh the current search.",
    );
    let external_receive_command = settings_entry_row(
        &form,
        "Receive command",
        &options.external_receive_command,
        "Shell command to fetch or sync mail, for example a lieer/offlineimap/mbsync wrapper.",
    );
    let notmuch_database_update_enabled = settings_check_row(
        &form,
        "Run update command",
        options.notmuch_database_update_enabled,
        "When Sync runs, run the database update command below after receive.",
    );
    let notmuch_database_update_on_startup = settings_check_row(
        &form,
        "Update at startup",
        options.notmuch_database_update_on_startup,
        "Also run the database update command when notm starts, then refresh the current search.",
    );
    let notmuch_database_update_command = settings_entry_row(
        &form,
        "Database update command",
        &options.notmuch_database_update_command,
        "Shell command to update the local notmuch database, for example `notmuch new` or a wrapper.",
    );

    settings_section(&form, "Automation");
    settings_note(
        &form,
        "Automation is for coding agents and tests to drive the actual notm UI and verify changes without clicking around or using separate GUI tools. It is not a Notmuch CLI replacement; it exercises notm itself. It is local, token-gated, and disabled by default.",
    );
    let automation_enabled = settings_check_row(
        &form,
        "Enable automation",
        options.automation_enabled,
        "Start a local automation socket on launch.",
    );
    let automation_socket = settings_entry_row(
        &form,
        "Socket path",
        &option_path_text(&options.automation_socket),
        "Optional Unix socket path. Blank uses a temporary default.",
    );
    let automation_token = settings_entry_row(
        &form,
        "Token",
        options.automation_token.as_deref().unwrap_or_default(),
        "Token required by automation clients.",
    );
    let screenshot_dir = settings_path_row(
        &widgets.window,
        &form,
        "Screenshots",
        &options.screenshot_dir.display().to_string(),
        "Directory used by automation screenshots.",
        SettingsPathKind::Directory,
    );
    let allow_live_send_test = settings_check_row(
        &form,
        "Allow self-send test",
        toml_bool(&existing, "automation", "allow_live_send_test", true),
        "Only affects the separate live-self-send validation command; normal sending uses the compose Send button and send settings above.",
    );
    let allow_live_tag_test = settings_check_row(
        &form,
        "Allow tag test",
        toml_bool(&existing, "automation", "allow_live_tag_test", false),
        "Safety gate for explicit automation tests that intentionally mutate tags in the real mail database.",
    );

    settings_note(
        &form,
        "Saving writes the app config file. Some changes require relaunch.",
    );

    dialog.add_button("Save", gtk::ResponseType::Accept);
    dialog.add_button("Close", gtk::ResponseType::Close);
    let opts = options.clone();
    let status = widgets.status_label.clone();
    dialog.connect_response(move |d, response| {
        if response == gtk::ResponseType::Accept {
            let values = SettingsValues {
                database_path: database_path.text().to_string(),
                notmuch_config_path: notmuch_config_path.text().to_string(),
                notmuch_profile: notmuch_profile.text().to_string(),
                default_query: default_query.text().to_string(),
                excluded_tags: excluded_tags.text().to_string(),
                open_readwrite_only_for_mutations: open_readwrite_only_for_mutations.is_active(),
                sync_maildir_flags_after_tag_change: sync_maildir_flags_after_tag_change
                    .is_active(),
                identity_name: identity_name.text().to_string(),
                primary_email: primary_email.text().to_string(),
                other_email: other_email.text().to_string(),
                theme: combo_active_id(&theme),
                page_size: page_size.text().parse::<usize>().unwrap_or(opts.page_size),
                thread_preview_lines: thread_preview_lines.text().parse::<usize>().unwrap_or(2),
                html_mode: combo_active_id(&html_mode),
                start_maximized: start_maximized.is_active(),
                show_debug_panel: show_debug_panel.is_active(),
                remote_images: remote_images.is_active(),
                trusted_image_senders: trusted_image_senders.text().to_string(),
                hidden_tag_searches: hidden_tag_searches.text().to_string(),
                send_enabled: send_enabled.is_active(),
                send_transport: combo_active_id(&send_transport),
                send_command: send_command.text().to_string(),
                send_args: send_args.text().to_string(),
                send_mode: combo_active_id(&send_mode),
                send_working_dir: send_working_dir.text().to_string(),
                send_env: send_env.text().to_string(),
                send_timeout_seconds: send_timeout_seconds.text().parse::<u64>().unwrap_or(120),
                save_sent: save_sent.is_active(),
                sent_maildir: sent_maildir.text().to_string(),
                sent_tags: sent_tags.text().to_string(),
                index_sent_after_send: index_sent_after_send.is_active(),
                save_drafts_to_maildir: save_drafts_to_maildir.is_active(),
                draft_maildir: draft_maildir.text().to_string(),
                draft_tags: draft_tags.text().to_string(),
                index_draft_after_save: index_draft_after_save.is_active(),
                sync_enabled: sync_enabled.is_active(),
                manual_sync_label: manual_sync_label.text().to_string(),
                notmuch_database_update_enabled: notmuch_database_update_enabled.is_active(),
                notmuch_database_update_on_startup: notmuch_database_update_on_startup.is_active(),
                notmuch_database_update_command: notmuch_database_update_command.text().to_string(),
                external_receive_enabled: external_receive_enabled.is_active(),
                external_receive_on_startup: external_receive_on_startup.is_active(),
                external_receive_command: external_receive_command.text().to_string(),
                automation_enabled: automation_enabled.is_active(),
                automation_socket: automation_socket.text().to_string(),
                automation_token: automation_token.text().to_string(),
                screenshot_dir: screenshot_dir.text().to_string(),
                allow_live_send_test: allow_live_send_test.is_active(),
                allow_live_tag_test: allow_live_tag_test.is_active(),
            };
            match persist_settings_values(&opts, &values) {
                Ok(()) => {
                    status.set_text("Settings saved to app config; some changes require relaunch")
                }
                Err(err) => status.set_text(&format!("Settings save failed: {err}")),
            }
        }
        d.close();
    });
    dialog.present();
}

struct SettingsValues {
    database_path: String,
    notmuch_config_path: String,
    notmuch_profile: String,
    default_query: String,
    excluded_tags: String,
    open_readwrite_only_for_mutations: bool,
    sync_maildir_flags_after_tag_change: bool,
    identity_name: String,
    primary_email: String,
    other_email: String,
    theme: String,
    page_size: usize,
    thread_preview_lines: usize,
    html_mode: String,
    start_maximized: bool,
    show_debug_panel: bool,
    remote_images: bool,
    trusted_image_senders: String,
    hidden_tag_searches: String,
    send_enabled: bool,
    send_transport: String,
    send_command: String,
    send_args: String,
    send_mode: String,
    send_working_dir: String,
    send_env: String,
    send_timeout_seconds: u64,
    save_sent: bool,
    sent_maildir: String,
    sent_tags: String,
    index_sent_after_send: bool,
    save_drafts_to_maildir: bool,
    draft_maildir: String,
    draft_tags: String,
    index_draft_after_save: bool,
    sync_enabled: bool,
    manual_sync_label: String,
    notmuch_database_update_enabled: bool,
    notmuch_database_update_on_startup: bool,
    notmuch_database_update_command: String,
    external_receive_enabled: bool,
    external_receive_on_startup: bool,
    external_receive_command: String,
    automation_enabled: bool,
    automation_socket: String,
    automation_token: String,
    screenshot_dir: String,
    allow_live_send_test: bool,
    allow_live_tag_test: bool,
}

fn settings_section(container: &gtk::Box, title: &str) {
    if container.first_child().is_some() {
        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        separator.set_margin_top(14);
        separator.set_margin_bottom(6);
        container.append(&separator);
    }
    let label = gtk::Label::new(Some(title));
    label.add_css_class("heading");
    label.add_css_class("notm-settings-section");
    label.set_xalign(0.0);
    label.set_margin_bottom(4);
    container.append(&label);
}

fn settings_note(container: &gtk::Box, text: &str) {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("dim-label");
    label.add_css_class("notm-settings-note");
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_margin_bottom(4);
    container.append(&label);
}

fn settings_label(label_text: &str, tooltip: &str) -> gtk::Label {
    let text = if label_text.ends_with(':') {
        label_text.to_string()
    } else {
        format!("{label_text}:")
    };
    let label = gtk::Label::new(Some(&text));
    label.set_width_chars(24);
    label.set_xalign(1.0);
    label.set_tooltip_text(Some(tooltip));
    label.add_css_class("notm-settings-label");
    label
}

fn settings_entry_row(
    container: &gtk::Box,
    label_text: &str,
    value: &str,
    tooltip: &str,
) -> gtk::Entry {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.set_hexpand(true);
    let label = settings_label(label_text, tooltip);
    let entry = gtk::Entry::new();
    entry.set_hexpand(true);
    entry.set_text(value);
    entry.set_tooltip_text(Some(tooltip));
    row.append(&label);
    row.append(&entry);
    container.append(&row);
    entry
}

#[derive(Debug, Clone, Copy)]
enum SettingsPathKind {
    File,
    Directory,
}

fn settings_path_row(
    parent: &gtk::ApplicationWindow,
    container: &gtk::Box,
    label_text: &str,
    value: &str,
    tooltip: &str,
    kind: SettingsPathKind,
) -> gtk::Entry {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.set_hexpand(true);
    let label = settings_label(label_text, tooltip);
    let field_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    field_box.set_hexpand(true);
    let entry = gtk::Entry::new();
    entry.set_hexpand(true);
    entry.set_text(value);
    entry.set_tooltip_text(Some(tooltip));
    let browse = gtk::Button::with_label("Browse…");
    browse.set_tooltip_text(Some("Choose a path"));
    let parent = parent.clone();
    let entry_for_dialog = entry.clone();
    browse.connect_clicked(move |_| {
        let action = match kind {
            SettingsPathKind::File => gtk::FileChooserAction::Open,
            SettingsPathKind::Directory => gtk::FileChooserAction::SelectFolder,
        };
        let title = match kind {
            SettingsPathKind::File => "Choose file",
            SettingsPathKind::Directory => "Choose directory",
        };
        let dialog = gtk::FileChooserNative::new(
            Some(title),
            Some(&parent),
            action,
            Some("Choose"),
            Some("Cancel"),
        );
        let entry_for_response = entry_for_dialog.clone();
        dialog.connect_response(move |dialog, response| {
            if response == gtk::ResponseType::Accept
                && let Some(file) = dialog.file()
                && let Some(path) = file.path()
            {
                entry_for_response.set_text(&path.display().to_string());
            }
            dialog.destroy();
        });
        dialog.show();
    });
    field_box.append(&entry);
    field_box.append(&browse);
    row.append(&label);
    row.append(&field_box);
    container.append(&row);
    entry
}

fn settings_combo_row(
    container: &gtk::Box,
    label_text: &str,
    options: &[(&str, &str)],
    active: &str,
    tooltip: &str,
) -> gtk::ComboBoxText {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.set_hexpand(true);
    let label = settings_label(label_text, tooltip);
    let combo = gtk::ComboBoxText::new();
    combo.set_hexpand(true);
    combo.set_tooltip_text(Some(tooltip));
    let mut known_active = false;
    for (value, display) in options {
        known_active |= *value == active;
        combo.append(Some(value), display);
    }
    if !known_active && !active.trim().is_empty() {
        combo.append(Some(active), &format!("{active} (custom)"));
    }
    combo.set_active_id(Some(active));
    if combo.active_id().is_none() && !options.is_empty() {
        combo.set_active(Some(0));
    }
    row.append(&label);
    row.append(&combo);
    container.append(&row);
    combo
}

fn combo_active_id(combo: &gtk::ComboBoxText) -> String {
    combo
        .active_id()
        .map(|id| id.to_string())
        .or_else(|| combo.active_text().map(|text| text.to_string()))
        .unwrap_or_default()
}

fn settings_check_row(
    container: &gtk::Box,
    label_text: &str,
    active: bool,
    tooltip: &str,
) -> gtk::CheckButton {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.set_hexpand(true);
    let label = settings_label(label_text, tooltip);
    let check = gtk::CheckButton::new();
    check.set_active(active);
    check.set_tooltip_text(Some(tooltip));
    check.set_halign(gtk::Align::Start);
    row.append(&label);
    row.append(&check);
    if !tooltip.trim().is_empty() {
        let help = gtk::Label::new(Some(tooltip));
        help.set_xalign(0.0);
        help.set_wrap(true);
        help.add_css_class("dim-label");
        help.add_css_class("notm-settings-help");
        help.set_hexpand(true);
        row.append(&help);
    }
    container.append(&row);
    check
}

fn settings_readonly_row(container: &gtk::Box, label_text: &str, value: &str, tooltip: &str) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.set_hexpand(true);
    let label = settings_label(label_text, tooltip);
    let value_label = gtk::Label::new(Some(value));
    value_label.set_xalign(0.0);
    value_label.set_selectable(true);
    value_label.set_wrap(true);
    value_label.set_hexpand(true);
    value_label.set_tooltip_text(Some(tooltip));
    row.append(&label);
    row.append(&value_label);
    container.append(&row);
}

fn read_settings_toml(options: &LaunchOptions) -> toml::Value {
    options
        .app_config_path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| text.parse::<toml::Value>().ok())
        .unwrap_or_else(|| toml::Value::Table(Default::default()))
}

fn toml_section<'a>(
    value: &'a toml::Value,
    section: &str,
) -> Option<&'a toml::map::Map<String, toml::Value>> {
    value.get(section)?.as_table()
}

fn toml_string(value: &toml::Value, section: &str, key: &str) -> Option<String> {
    toml_section(value, section)?
        .get(key)?
        .as_str()
        .map(ToOwned::to_owned)
}

fn toml_bool(value: &toml::Value, section: &str, key: &str, default: bool) -> bool {
    toml_section(value, section)
        .and_then(|table| table.get(key))
        .and_then(toml::Value::as_bool)
        .unwrap_or(default)
}

fn toml_usize(value: &toml::Value, section: &str, key: &str, default: usize) -> usize {
    toml_section(value, section)
        .and_then(|table| table.get(key))
        .and_then(toml::Value::as_integer)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
}

fn option_path_text(value: &Option<PathBuf>) -> String {
    value
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

fn join_string_list(values: &[String]) -> String {
    values.join(", ")
}

fn parse_string_list(value: &str) -> Vec<String> {
    value
        .split([',', '\n'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn format_env_map(values: &BTreeMap<String, String>) -> String {
    values
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_env_map(value: &str) -> BTreeMap<String, String> {
    value
        .split([',', '\n'])
        .filter_map(|item| item.trim().split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .filter(|(key, _)| !key.is_empty())
        .collect()
}

fn transport_mode_name(mode: &TransportMode) -> String {
    match mode {
        TransportMode::Auto => "auto",
        TransportMode::StdinRfc5322 => "stdin_rfc5322",
        TransportMode::FileArg => "file_arg",
        TransportMode::CommandTemplate => "command_template",
    }
    .to_string()
}

fn persist_settings_values(options: &LaunchOptions, values: &SettingsValues) -> anyhow::Result<()> {
    let Some(path) = &options.app_config_path else {
        anyhow::bail!("app config path is not configured");
    };
    let mut value = read_settings_toml(options);
    if !value.is_table() {
        value = toml::Value::Table(Default::default());
    }
    let root = value.as_table_mut().expect("value is table");

    set_optional_string(root, "notmuch", "database_path", &values.database_path);
    set_optional_string(root, "notmuch", "config_path", &values.notmuch_config_path);
    set_optional_string(root, "notmuch", "profile", &values.notmuch_profile);
    set_string(root, "notmuch", "default_query", &values.default_query);
    set_string_array(
        root,
        "notmuch",
        "excluded_tags",
        parse_string_list(&values.excluded_tags),
    );
    set_bool(
        root,
        "notmuch",
        "open_readwrite_only_for_mutations",
        values.open_readwrite_only_for_mutations,
    );
    set_bool(
        root,
        "notmuch",
        "sync_maildir_flags_after_tag_change",
        values.sync_maildir_flags_after_tag_change,
    );

    set_optional_string(root, "identity", "name", &values.identity_name);
    set_optional_string(root, "identity", "primary_email", &values.primary_email);
    set_string_array(
        root,
        "identity",
        "other_email",
        parse_string_list(&values.other_email),
    );

    set_string(root, "ui", "theme", &values.theme);
    set_int(root, "ui", "page_size", values.page_size as i64);
    set_int(
        root,
        "ui",
        "thread_preview_lines",
        values.thread_preview_lines as i64,
    );
    set_string(root, "ui", "html_mode", &values.html_mode);
    set_bool(root, "ui", "start_maximized", values.start_maximized);
    set_bool(root, "ui", "show_debug_panel", values.show_debug_panel);
    set_bool(root, "ui", "remote_images", values.remote_images);
    set_string_array(
        root,
        "ui",
        "trusted_image_senders",
        parse_string_list(&values.trusted_image_senders),
    );
    set_string_array(
        root,
        "ui",
        "hidden_tag_searches",
        parse_string_list(&values.hidden_tag_searches),
    );

    set_bool(root, "send", "enabled", values.send_enabled);
    set_string(root, "send", "transport", &values.send_transport);
    set_optional_string(root, "send", "command", &values.send_command);
    set_string_array(root, "send", "args", parse_string_list(&values.send_args));
    set_string(root, "send", "mode", &values.send_mode);
    set_optional_string(root, "send", "working_dir", &values.send_working_dir);
    set_string_map(root, "send", "env", parse_env_map(&values.send_env));
    set_int(
        root,
        "send",
        "timeout_seconds",
        values.send_timeout_seconds as i64,
    );
    set_bool(root, "send", "save_sent", values.save_sent);
    set_optional_string(root, "send", "sent_maildir", &values.sent_maildir);
    set_string_array(
        root,
        "send",
        "sent_tags",
        parse_string_list(&values.sent_tags),
    );
    set_bool(
        root,
        "send",
        "index_sent_after_send",
        values.index_sent_after_send,
    );
    table_entry(root, "send").remove("one_live_self_test_per_run");
    set_bool(
        root,
        "drafts",
        "save_maildir",
        values.save_drafts_to_maildir,
    );
    set_optional_string(root, "drafts", "maildir", &values.draft_maildir);
    set_string_array(
        root,
        "drafts",
        "tags",
        parse_string_list(&values.draft_tags),
    );
    set_bool(
        root,
        "drafts",
        "index_after_save",
        values.index_draft_after_save,
    );

    set_bool(root, "sync", "enabled", values.sync_enabled);
    set_string(
        root,
        "sync",
        "manual_action_label",
        &values.manual_sync_label,
    );
    table_entry(root, "sync").remove("show_manual_sync_button");
    set_bool(
        root,
        "sync",
        "notmuch_database_update_enabled",
        values.notmuch_database_update_enabled,
    );
    set_bool(
        root,
        "sync",
        "notmuch_database_update_on_startup",
        values.notmuch_database_update_on_startup,
    );
    set_string(
        root,
        "sync",
        "notmuch_database_update_command",
        &values.notmuch_database_update_command,
    );
    set_bool(
        root,
        "sync",
        "external_receive_enabled",
        values.external_receive_enabled,
    );
    set_bool(
        root,
        "sync",
        "external_receive_on_startup",
        values.external_receive_on_startup,
    );
    set_string(
        root,
        "sync",
        "external_receive_command",
        &values.external_receive_command,
    );

    set_bool(root, "automation", "enabled", values.automation_enabled);
    set_optional_string(root, "automation", "socket_path", &values.automation_socket);
    set_optional_string(root, "automation", "token", &values.automation_token);
    set_string(root, "automation", "screenshot_dir", &values.screenshot_dir);
    set_bool(
        root,
        "automation",
        "allow_live_send_test",
        values.allow_live_send_test,
    );
    set_bool(
        root,
        "automation",
        "allow_live_tag_test",
        values.allow_live_tag_test,
    );

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&value)?)?;
    Ok(())
}

fn set_string(
    root: &mut toml::map::Map<String, toml::Value>,
    section: &str,
    key: &str,
    value: &str,
) {
    table_entry(root, section).insert(key.to_string(), toml::Value::String(value.to_string()));
}

fn set_optional_string(
    root: &mut toml::map::Map<String, toml::Value>,
    section: &str,
    key: &str,
    value: &str,
) {
    let table = table_entry(root, section);
    let trimmed = value.trim();
    if trimmed.is_empty() {
        table.remove(key);
    } else {
        table.insert(key.to_string(), toml::Value::String(trimmed.to_string()));
    }
}

fn set_bool(root: &mut toml::map::Map<String, toml::Value>, section: &str, key: &str, value: bool) {
    table_entry(root, section).insert(key.to_string(), toml::Value::Boolean(value));
}

fn set_int(root: &mut toml::map::Map<String, toml::Value>, section: &str, key: &str, value: i64) {
    table_entry(root, section).insert(key.to_string(), toml::Value::Integer(value));
}

fn set_string_array(
    root: &mut toml::map::Map<String, toml::Value>,
    section: &str,
    key: &str,
    values: Vec<String>,
) {
    table_entry(root, section).insert(
        key.to_string(),
        toml::Value::Array(values.into_iter().map(toml::Value::String).collect()),
    );
}

fn set_string_map(
    root: &mut toml::map::Map<String, toml::Value>,
    section: &str,
    key: &str,
    values: BTreeMap<String, String>,
) {
    if values.is_empty() {
        table_entry(root, section).remove(key);
        return;
    }
    table_entry(root, section).insert(
        key.to_string(),
        toml::Value::Table(
            values
                .into_iter()
                .map(|(key, value)| (key, toml::Value::String(value)))
                .collect(),
        ),
    );
}

fn persist_basic_settings(
    options: &LaunchOptions,
    default_query: &str,
    page_size: usize,
    send_command: &str,
) -> anyhow::Result<()> {
    let Some(path) = &options.app_config_path else {
        return Ok(());
    };
    let mut value = if path.exists() {
        std::fs::read_to_string(path)?
            .parse::<toml::Value>()
            .unwrap_or_else(|_| toml::Value::Table(Default::default()))
    } else {
        toml::Value::Table(Default::default())
    };
    if !value.is_table() {
        value = toml::Value::Table(Default::default());
    }
    let root = value.as_table_mut().expect("value is table");
    table_entry(root, "notmuch").insert(
        "default_query".to_string(),
        toml::Value::String(default_query.to_string()),
    );
    table_entry(root, "ui").insert(
        "page_size".to_string(),
        toml::Value::Integer(page_size as i64),
    );
    if !send_command.trim().is_empty() {
        table_entry(root, "send").insert(
            "command".to_string(),
            toml::Value::String(send_command.trim().to_string()),
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(&value)?)?;
    Ok(())
}

fn table_entry<'a>(
    root: &'a mut toml::map::Map<String, toml::Value>,
    key: &str,
) -> &'a mut toml::map::Map<String, toml::Value> {
    let value = root
        .entry(key.to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    if !value.is_table() {
        *value = toml::Value::Table(Default::default());
    }
    value.as_table_mut().expect("table entry is table")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_send_without_command_or_fixture_capture_fails() {
        let options = LaunchOptions::default();
        let message = ComposedMessage::new(
            "sender@example.test".to_string(),
            vec!["recipient@example.test".to_string()],
            "test subject".to_string(),
            "test body".to_string(),
        );
        let err = send_message_with_config(&options, message).unwrap_err();

        assert!(
            err.to_string().contains("refusing to fake-send"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn composed_message_from_fields_preserves_reply_thread_headers() {
        let fields = ComposeFields {
            from: "Me <me@example.test>".to_string(),
            to: "Alice <alice@example.test>".to_string(),
            subject: "Re: Hello".to_string(),
            body: "Reply body".to_string(),
            in_reply_to: Some("<original@example.test>".to_string()),
            references: vec![
                "<older@example.test>".to_string(),
                "<original@example.test>".to_string(),
            ],
            ..ComposeFields::default()
        };

        let message = composed_message_from_fields(&fields).expect("message");
        let rendered = message.to_rfc5322();

        assert_eq!(
            message.in_reply_to.as_deref(),
            Some("<original@example.test>")
        );
        assert_eq!(
            message.references,
            vec![
                "<older@example.test>".to_string(),
                "<original@example.test>".to_string(),
            ]
        );
        assert!(rendered.contains("In-Reply-To: <original@example.test>\r\n"));
        assert!(rendered.contains("References: <older@example.test> <original@example.test>\r\n"));
    }

    #[test]
    fn draft_fields_load_from_saved_rfc5322_message() {
        let path = std::env::temp_dir().join(format!("notm-draft-{}.eml", Uuid::new_v4()));
        let raw = "From: Me <me@example.test>\r\nTo: You <you@example.test>\r\nCc: Other <other@example.test>\r\nBcc: Hidden <hidden@example.test>\r\nSubject: Draft subject\r\nMessage-ID: <draft@example.test>\r\nIn-Reply-To: <parent@example.test>\r\nReferences: <root@example.test> <parent@example.test>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nDraft body.";
        std::fs::write(&path, raw).expect("write draft");

        let fields = draft_fields_from_message_file(&path).expect("draft fields");

        assert_eq!(fields.from, "Me <me@example.test>");
        assert_eq!(fields.to, "You <you@example.test>");
        assert_eq!(fields.cc, "Other <other@example.test>");
        assert_eq!(fields.bcc, "Hidden <hidden@example.test>");
        assert_eq!(fields.subject, "Draft subject");
        assert_eq!(fields.body, "Draft body.");
        assert_eq!(fields.in_reply_to.as_deref(), Some("<parent@example.test>"));
        assert_eq!(
            fields.references,
            vec![
                "<root@example.test>".to_string(),
                "<parent@example.test>".to_string()
            ]
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn visual_html_document_uses_light_default_canvas() {
        let document = visual_html_document("<p>Hello</p>");

        assert!(document.contains(r#"<meta name="color-scheme" content="light">"#));
        assert!(document.contains("background: #ffffff;"));
        assert!(document.contains("color: #111111;"));
        assert!(!document.contains("CanvasText"));
    }

    #[test]
    fn multi_thread_tag_query_round_trips_thread_ids() {
        let ids = BTreeSet::from(["thread-a".to_string(), "thread-b".to_string()]);
        let query = tag_query_for_thread_ids(&ids);

        assert_eq!(query, "thread:thread-a or thread:thread-b");
        assert_eq!(thread_ids_from_tag_query(&query), ids);
    }

    #[test]
    fn sync_command_selection_separates_manual_from_startup() {
        let mut options = LaunchOptions {
            sync_enabled: true,
            external_receive_enabled: true,
            external_receive_command: "lieer-sync".to_string(),
            notmuch_database_update_enabled: true,
            notmuch_database_update_command: "notmuch new".to_string(),
            ..LaunchOptions::default()
        };

        assert_eq!(sync_command_specs(&options, SyncRunKind::Manual).len(), 2);
        assert!(sync_command_specs(&options, SyncRunKind::Startup).is_empty());

        options.external_receive_on_startup = true;
        let startup = sync_command_specs(&options, SyncRunKind::Startup);
        assert_eq!(startup.len(), 1);
        assert_eq!(startup[0].name, "receive");

        options.notmuch_database_update_on_startup = true;
        let startup = sync_command_specs(&options, SyncRunKind::Startup);
        assert_eq!(startup.len(), 2);
        assert_eq!(startup[0].name, "receive");
        assert_eq!(startup[1].name, "database_update");
    }
}
