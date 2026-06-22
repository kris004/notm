use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::{Mutex, OnceLock, mpsc},
    thread,
    time::Duration,
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
    pub send_command: Option<PathBuf>,
    pub send_args: Vec<String>,
    pub send_mode: TransportMode,
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
    pub notmuch_database_update_command: String,
    pub external_receive_enabled: bool,
    pub external_receive_command: String,
    pub show_manual_sync_button: bool,
    pub screenshot_dir: PathBuf,
    pub automation_enabled: bool,
    pub automation_socket: Option<PathBuf>,
    pub automation_token: Option<String>,
    pub show_debug_panel: bool,
    pub start_maximized: bool,
    pub remote_images: bool,
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
            send_command: None,
            send_args: Vec::new(),
            send_mode: TransportMode::Auto,
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
            notmuch_database_update_command: String::new(),
            external_receive_enabled: false,
            external_receive_command: String::new(),
            show_manual_sync_button: false,
            screenshot_dir: PathBuf::from("artifacts/screenshots"),
            automation_enabled: false,
            automation_socket: None,
            automation_token: None,
            show_debug_panel: false,
            start_maximized: false,
            remote_images: false,
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
    left_pane: gtk::Box,
    message_pane: gtk::Box,
    saved_box: gtk::Box,
    saved_name_entry: gtk::Entry,
    saved_query_entry: gtk::Entry,
    custom_tag_entry: gtk::Entry,
    search_entry: gtk::Entry,
    search_button: gtk::Button,
    search_generation: Rc<Cell<u64>>,
    hidden_tag_searches: HiddenTagSearchStore,
    thread_list: gtk::ListBox,
    thread_result_label: gtk::Label,
    load_more_button: gtk::Button,
    thread_scrolled: gtk::ScrolledWindow,
    compose_button: gtk::Button,
    debug_button: gtk::Button,
    palette_button: gtk::Button,
    settings_button: gtk::Button,
    archive_button: gtk::Button,
    read_toggle_button: gtk::Button,
    flag_toggle_button: gtk::Button,
    trash_button: gtk::Button,
    spam_button: gtk::Button,
    tag_menu_button: gtk::MenuButton,
    undo_tag_button: gtk::Button,
    message_stack: gtk::Stack,
    message_view: gtk::TextView,
    message_scrolled: gtk::ScrolledWindow,
    html_view: webkit6::WebView,
    html_scrolled: gtk::ScrolledWindow,
    response_menu_button: gtk::MenuButton,
    message_menu_button: gtk::MenuButton,
    message_menu_box: gtk::Box,
    view_menu_button: gtk::MenuButton,
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
    compose_body: gtk::TextView,
    compose_scrolled: gtk::ScrolledWindow,
    compose_attachments: gtk::Label,
    add_attachment_button: gtk::Button,
    save_draft_button: gtk::Button,
    clear_draft_button: gtk::Button,
    delete_local_draft_button: gtk::Button,
    send_button: gtk::Button,
    address_suggestions_popover: gtk::Popover,
    address_suggestions_list: gtk::ListBox,
    draft_list: gtk::ListBox,
    drafts_dir: PathBuf,
}

type SharedState = Rc<RefCell<UiState>>;
type UndoState = Rc<RefCell<Option<(String, TagMutation)>>>;
type SavedSearchStore = Rc<RefCell<Vec<SavedSearch>>>;
type HiddenTagSearchStore = Rc<RefCell<BTreeSet<String>>>;

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

static SEARCH_CACHE: OnceLock<Mutex<BTreeMap<String, SearchData>>> = OnceLock::new();
static THREAD_DETAIL_CACHE: OnceLock<Mutex<BTreeMap<String, ThreadUiDetails>>> = OnceLock::new();

const SIDEBAR_MIN_WIDTH: i32 = 112;
const SIDEBAR_INITIAL_WIDTH: i32 = 136;
const THREAD_LIST_MIN_WIDTH: i32 = 320;
const COMPOSE_BODY_MIN_HEIGHT: i32 = 160;
const COMPOSE_BODY_NATURAL_HEIGHT: i32 = 260;

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
    let undo_state: UndoState = Rc::new(RefCell::new(None));
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
    for b in [
        &compose_button,
        &debug_button,
        &palette_button,
        &settings_button,
    ] {
        toolbar.insert(b, -1);
    }
    root.append(&toolbar);

    let left = gtk::Box::new(gtk::Orientation::Vertical, 6);
    left.set_widget_name("notm-left-sidebar");
    left.set_size_request(SIDEBAR_MIN_WIDTH, -1);
    left.set_focusable(true);
    left.set_margin_start(8);
    left.set_margin_end(8);
    left.set_margin_top(8);
    left.set_margin_bottom(8);

    let sidebar_title = gtk::Label::new(Some("Saved searches"));
    sidebar_title.add_css_class("heading");
    sidebar_title.set_xalign(0.0);
    left.append(&sidebar_title);
    let saved_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    saved_box.set_widget_name("notm-saved-searches");
    left.append(&saved_box);
    let saved_editor_title = gtk::Label::new(Some("Custom saved search"));
    saved_editor_title.set_xalign(0.0);
    saved_editor_title.add_css_class("dim-label");
    saved_editor_title.set_wrap(true);
    left.append(&saved_editor_title);
    let saved_name_entry = entry_with_placeholder("Name");
    saved_name_entry.set_widget_name("notm-saved-search-name");
    saved_name_entry.set_width_chars(10);
    saved_name_entry.set_max_width_chars(10);
    let saved_query_entry = entry_with_placeholder("Query");
    saved_query_entry.set_widget_name("notm-saved-search-query");
    saved_query_entry.set_width_chars(10);
    saved_query_entry.set_max_width_chars(10);
    left.append(&saved_name_entry);
    left.append(&saved_query_entry);
    let saved_editor_buttons = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let save_search_button = gtk::Button::with_label("Save");
    save_search_button.set_widget_name("notm-save-search-button");
    let delete_search_button = gtk::Button::with_label("Delete");
    delete_search_button.set_widget_name("notm-delete-search-button");
    saved_editor_buttons.append(&save_search_button);
    saved_editor_buttons.append(&delete_search_button);
    left.append(&saved_editor_buttons);

    let tag_title = gtk::Label::new(Some("Tags"));
    tag_title.set_xalign(0.0);
    tag_title.add_css_class("heading");
    left.append(&tag_title);
    let tag_search_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    tag_search_box.set_widget_name("notm-tag-searches");
    left.append(&tag_search_box);
    let manual_sync_button = if options.sync_enabled && options.show_manual_sync_button {
        let sync_button = gtk::Button::with_label(&options.manual_sync_label);
        sync_button.set_widget_name("notm-manual-sync-button");
        left.append(&sync_button);
        Some(sync_button)
    } else {
        None
    };

    let middle = gtk::Box::new(gtk::Orientation::Vertical, 6);
    middle.set_margin_start(8);
    middle.set_margin_end(8);
    middle.set_margin_top(8);
    middle.set_margin_bottom(8);
    middle.set_size_request(THREAD_LIST_MIN_WIDTH, -1);
    middle.set_focusable(true);

    let search_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let search_entry = gtk::Entry::new();
    search_entry.set_widget_name("notm-search-entry");
    search_entry.set_hexpand(true);
    search_entry.set_text(&options.default_query);
    search_entry.set_placeholder_text(Some(
        "Notmuch query, e.g. tag:inbox and not tag:trash and not tag:spam",
    ));
    let search_button = gtk::Button::with_label("Search");
    search_button.set_widget_name("notm-search-button");
    search_row.append(&search_entry);
    search_row.append(&search_button);
    middle.append(&search_row);
    let helper = gtk::Label::new(Some(
        "Syntax: tag:inbox, from:alice, subject:report, thread:<id>, *",
    ));
    helper.set_xalign(0.0);
    helper.add_css_class("dim-label");
    middle.append(&helper);

    let action_outer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    action_outer.set_hexpand(true);
    let action_row = button_flow(4);
    let archive_button = gtk::Button::with_label("Archive");
    let read_button = gtk::Button::with_label("Mark read");
    read_button.set_widget_name("notm-read-toggle-button");
    let flag_button = gtk::Button::with_label("Flag");
    flag_button.set_widget_name("notm-flag-toggle-button");
    let trash_button = gtk::Button::with_label("Trash");
    let spam_button = gtk::Button::with_label("Spam");
    let undo_button = gtk::Button::with_label("Undo tag");
    undo_button.set_widget_name("notm-undo-tag-button");
    undo_button.add_css_class("suggested-action");
    undo_button.set_halign(gtk::Align::End);
    undo_button.set_visible(false);
    undo_button.set_tooltip_text(Some(
        "Undo only reverses the most recent tag operation from this session.",
    ));
    let (tag_menu_button, tag_menu_box) =
        menu_button_with_box("Tag…", "notm-custom-tag-menu-button");
    tag_menu_box.set_spacing(6);
    tag_menu_box.set_margin_start(6);
    tag_menu_box.set_margin_end(6);
    tag_menu_box.set_margin_top(6);
    tag_menu_box.set_margin_bottom(6);
    let custom_tag_entry = entry_with_placeholder("tag");
    custom_tag_entry.set_widget_name("notm-custom-tag-entry");
    custom_tag_entry.set_width_chars(18);
    let tag_button_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let add_tag_button = gtk::Button::with_label("Add tag");
    add_tag_button.set_widget_name("notm-add-custom-tag-button");
    let remove_tag_button = gtk::Button::with_label("Remove tag");
    remove_tag_button.set_widget_name("notm-remove-custom-tag-button");
    tag_button_row.append(&add_tag_button);
    tag_button_row.append(&remove_tag_button);
    tag_menu_box.append(&custom_tag_entry);
    tag_menu_box.append(&tag_button_row);
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
    action_outer.append(&action_row);
    action_outer.append(&undo_button);
    middle.append(&action_outer);

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
        menu_button_with_box("Respond", "notm-response-menu-button");
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
        menu_button_with_box("Message", "notm-message-menu-button");
    let (view_menu_button, view_menu_box) = menu_button_with_box("View", "notm-view-menu-button");
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
    let (copy_menu_button, copy_menu_box) = menu_button_with_box("Copy", "notm-copy-menu-button");
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
    let compose_body = gtk::TextView::new();
    compose_body.set_widget_name("notm-compose-body");
    compose_body.set_hexpand(true);
    compose_body.set_wrap_mode(gtk::WrapMode::WordChar);
    compose_body.set_vexpand(true);
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
    address_suggestions_list.set_size_request(360, -1);
    address_suggestions_list.set_focusable(false);
    let address_suggestions_popover = gtk::Popover::new();
    address_suggestions_popover.set_widget_name("notm-address-suggestions");
    address_suggestions_popover.set_has_arrow(false);
    // Address suggestions are informational while the recipient entry keeps
    // keyboard focus.  The default modal/autohide popover grabs keyboard input,
    // which makes typing stop as soon as suggestions appear.
    address_suggestions_popover.set_autohide(false);
    address_suggestions_popover.set_child(Some(&address_suggestions_list));
    address_suggestions_popover.set_parent(&compose_to);
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
    for w in [
        &compose_from,
        &compose_to,
        &compose_cc,
        &compose_bcc,
        &compose_subject,
    ] {
        composer_box.append(w);
    }
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
    outer_paned.set_start_child(Some(&left));
    outer_paned.set_end_child(Some(&content_paned));
    outer_paned.set_position(SIDEBAR_INITIAL_WIDTH);
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
        left_pane: left.clone(),
        message_pane: right.clone(),
        saved_box,
        saved_name_entry,
        saved_query_entry,
        custom_tag_entry,
        search_entry,
        search_button: search_button.clone(),
        search_generation,
        hidden_tag_searches,
        thread_list,
        thread_result_label,
        load_more_button,
        thread_scrolled: scrolled_threads,
        compose_button: compose_button.clone(),
        debug_button: debug_button.clone(),
        palette_button: palette_button.clone(),
        settings_button: settings_button.clone(),
        archive_button: archive_button.clone(),
        read_toggle_button: read_button.clone(),
        flag_toggle_button: flag_button.clone(),
        trash_button: trash_button.clone(),
        spam_button: spam_button.clone(),
        tag_menu_button: tag_menu_button.clone(),
        undo_tag_button: undo_button.clone(),
        message_stack,
        message_view,
        message_scrolled: scrolled_message.clone(),
        html_view,
        html_scrolled: scrolled_html.clone(),
        response_menu_button,
        message_menu_button,
        message_menu_box,
        view_menu_button,
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
        compose_scrolled: scrolled_compose_body.clone(),
        compose_attachments,
        add_attachment_button: add_attachment_button.clone(),
        save_draft_button: save_draft_button.clone(),
        clear_draft_button: clear_draft_button.clone(),
        delete_local_draft_button: delete_local_draft_button.clone(),
        send_button: send_button.clone(),
        address_suggestions_popover,
        address_suggestions_list,
        draft_list,
        drafts_dir: options
            .drafts_dir
            .clone()
            .unwrap_or_else(default_drafts_dir),
    };
    update_message_action_buttons(&options, &widgets, &state);
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
        &delete_search_button,
    );
    connect_custom_tag_editor(
        &options,
        &widgets,
        &state,
        &undo_state,
        &add_tag_button,
        &remove_tag_button,
    );
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
        &undo_button,
        &compose_button,
        &reply_button,
        &reply_all_button,
        &forward_button,
        &forward_attachment_button,
        &debug_button,
        &palette_button,
        &settings_button,
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
    connect_message_actions(&options, &widgets, &state);
    connect_recipient_autocomplete(&widgets.compose_to, &widgets, &state);
    connect_address_suggestion_list(&widgets, &state);
    connect_search_debounce(&options, &widgets, &state);
    connect_input_mode_focus(&widgets, &state);
    install_shortcuts(&options, &widgets, &state, &undo_state);
    connect_auto_load_more(&options, &widgets, &state);

    if options.automation_enabled {
        setup_automation(&options, &widgets, &state, &undo_state, &saved_search_store);
    }

    restore_draft_if_present(&widgets, &state);
    refresh_draft_list(&widgets);
    window.present();
    run_search(&options, &widgets, &state, &options.default_query);
    refresh_address_suggestions(&options, &widgets, &state);
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
        "All" => Some("g a"),
        _ => None,
    }
}

fn update_saved_search_button_labels(widgets: &Widgets, state: &SharedState) {
    let mut child = widgets.saved_box.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        let Ok(button) = widget.downcast::<gtk::Button>() else {
            continue;
        };
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
    let mut searches = built_in_saved_searches();
    searches.extend(saved_store.borrow().iter().cloned());
    for saved in searches {
        let btn = gtk::Button::with_label(&saved.name);
        btn.set_widget_name(&format!("notm-saved-search-{}", widget_token(&saved.name)));
        btn.set_tooltip_text(Some(&saved.name));
        let st = state.clone();
        let w = widgets.clone();
        let opts = options.clone();
        btn.connect_clicked(move |_| {
            activate_saved_search(&opts, &w, &st, &saved.name, &saved.query);
        });
        widgets.saved_box.append(&btn);
    }
    update_saved_search_button_labels(widgets, state);
    update_tag_searches(options, widgets, state);
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
        let (button, menu) =
            menu_button_with_box(&root, &format!("notm-tag-group-{}", widget_token(&root)));
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
    button.connect_clicked(move |_| activate_saved_search(&opts, &w, &st, &tag, &query));
    container.append(&button);
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
    collect_tag_button_targets_from_widget(&widgets.tag_search_box.clone().upcast(), &mut targets);
    targets
}

fn collect_tag_button_targets_from_widget(widget: &gtk::Widget, targets: &mut Vec<String>) {
    if let Ok(button) = widget.clone().downcast::<gtk::Button>()
        && let Some(tag) = button.tooltip_text()
    {
        targets.push(tag.to_string());
    }
    if let Ok(menu_button) = widget.clone().downcast::<gtk::MenuButton>()
        && let Some(popover) = menu_button.popover()
        && let Some(child) = popover.child()
    {
        collect_tag_button_targets_from_widget(&child, targets);
    }
    let mut child = widget.first_child();
    while let Some(child_widget) = child {
        child = child_widget.next_sibling();
        collect_tag_button_targets_from_widget(&child_widget, targets);
    }
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
    }
    if let Ok(menu_button) = widget.clone().downcast::<gtk::MenuButton>()
        && let Some(popover) = menu_button.popover()
        && let Some(child) = popover.child()
    {
        update_tag_search_button_labels_in_widget(&child, targets, state);
    }
    let mut child = widget.first_child();
    while let Some(child_widget) = child {
        child = child_widget.next_sibling();
        update_tag_search_button_labels_in_widget(&child_widget, targets, state);
    }
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
    activate_saved_search(options, widgets, state, &tag, &tag_query(&tag));
    set_active_pane(widgets, state, ActivePane::Threads);
    true
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
    delete_search_button: &gtk::Button,
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

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let store = saved_store.clone();
    delete_search_button.connect_clicked(move |_| {
        match delete_custom_search_from_entries(&opts, &w, &st, &store) {
            Ok(()) => w.status_label.set_text("Deleted custom search"),
            Err(err) => w
                .status_label
                .set_text(&format!("Delete search failed: {err}")),
        }
    });
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
    add_tag_button
        .connect_clicked(move |_| apply_custom_tag_from_entry(&opts, &w, &st, &undo, true));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    remove_tag_button
        .connect_clicked(move |_| apply_custom_tag_from_entry(&opts, &w, &st, &undo, false));
}

fn apply_custom_tag_from_entry(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    add: bool,
) {
    let tag = widgets.custom_tag_entry.text().trim().to_string();
    if tag.is_empty() {
        widgets.status_label.set_text("Tag name is empty");
        return;
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
    tag_selected(options, widgets, state, undo_state, mutation);
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
    widgets
        .view_text_button
        .connect_clicked(move |_| show_rendered_selected_thread(&opts, &w, &st));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets
        .view_html_button
        .connect_clicked(move |_| show_visual_html_selected_message(&opts, &w, &st));

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
    entry.connect_changed(move |entry| {
        update_address_suggestions_label(&w, &st, &entry.text());
        autosave_draft_from_widgets(&w, &st);
    });
    let controller = gtk::EventControllerKey::new();
    let entry_clone = entry.clone();
    let w = widgets.clone();
    let st = state.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Tab && apply_recipient_completion(&entry_clone, &st) {
            w.address_suggestions_popover.popdown();
            autosave_draft_from_widgets(&w, &st);
            return gtk::glib::Propagation::Stop;
        } else if key == gtk::gdk::Key::Escape {
            w.address_suggestions_popover.popdown();
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    entry.add_controller(controller);

    let w = widgets.clone();
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(move |_| w.address_suggestions_popover.popdown());
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
            apply_recipient_suggestion(&w.compose_to, &label.text());
            w.address_suggestions_popover.popdown();
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

fn set_input_mode(widgets: &Widgets, state: &SharedState, mode: InputMode, status: &str) {
    state.borrow_mut().input_mode = mode;
    update_button_binding_labels(widgets, state);
    widgets.status_label.set_text(status);
}

fn enter_normal_mode(widgets: &Widgets, state: &SharedState) {
    set_input_mode(widgets, state, InputMode::Normal, "Normal mode");
    focus_active_pane(widgets, state);
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
        ActivePane::Sidebar => widgets.saved_query_entry.grab_focus(),
        ActivePane::Threads => widgets.search_entry.grab_focus(),
        ActivePane::Message if compose_view_is_visible(widgets) => widgets.compose_to.grab_focus(),
        ActivePane::Message => widgets.message_view.grab_focus(),
    };
}

fn focus_active_pane(widgets: &Widgets, state: &SharedState) {
    match state.borrow().active_pane {
        ActivePane::Sidebar => {
            widgets.left_pane.grab_focus();
        }
        ActivePane::Threads => {
            widgets.thread_list.grab_focus();
        }
        ActivePane::Message => {
            if compose_view_is_visible(widgets) {
                widgets.message_pane.grab_focus();
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
        ActivePane::Message => "message view",
    };
    widgets
        .status_label
        .set_text(&format!("Active pane: {name}"));
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
        ActivePane::Message => scroll_window_lines(&active_message_scrolled(widgets), lines),
    }
}

fn vim_scroll_pages(widgets: &Widgets, state: &SharedState, pages: f64) {
    match state.borrow().active_pane {
        ActivePane::Threads => {}
        ActivePane::Sidebar => scroll_window_pages(&widgets.thread_scrolled, pages),
        ActivePane::Message => scroll_window_pages(&active_message_scrolled(widgets), pages),
    }
}

fn vim_scroll_to_edge(widgets: &Widgets, state: &SharedState, bottom: bool) {
    match state.borrow().active_pane {
        ActivePane::Threads => {}
        ActivePane::Sidebar => scroll_window_to_edge(&widgets.thread_scrolled, bottom),
        ActivePane::Message => scroll_window_to_edge(&active_message_scrolled(widgets), bottom),
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
    let target = one_based.saturating_sub(1);
    let len = state.borrow().thread_list_items.len();
    if len == 0 {
        return;
    }
    if target >= len && state.borrow().can_load_more_threads {
        load_until_thread_index(options, widgets, state, target);
        return;
    }
    select_thread_index_clamped(options, widgets, state, target);
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

fn load_until_thread_index(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    target_index: usize,
) {
    let target_number = target_index + 1;
    set_thread_loading_indicator(widgets, &format!("Loading messages up to {target_number}…"));
    loop {
        let (query, offset, can_load_more) = {
            let state = state.borrow();
            (
                state.current_query.clone(),
                state.thread_list_items.len(),
                state.can_load_more_threads,
            )
        };
        if offset > target_index || !can_load_more {
            break;
        }
        match execute_search_page(options, &query, offset) {
            Ok(data) => {
                set_thread_loading_indicator(
                    widgets,
                    &format!("Loading messages up to {target_number}… loaded {}", offset),
                );
                append_search_data(options, widgets, state, data);
            }
            Err(err) => {
                apply_search_error(widgets, state, err);
                return;
            }
        }
    }
    let len = state.borrow().thread_list_items.len();
    if len == 0 {
        update_thread_result_label(widgets, state);
        return;
    }
    select_thread_index_clamped(options, widgets, state, target_index);
    update_thread_result_label(widgets, state);
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
    set_button_label(&widgets.undo_tag_button, "Undo tag", "z", state);
    set_menu_button_label(&widgets.response_menu_button, "Respond", "r/R/F", state);
    set_menu_button_label(&widgets.view_menu_button, "View", "v", state);
    set_button_label(
        &widgets.collapse_quotes_button,
        "Collapse quotes",
        "q",
        state,
    );
    set_menu_button_label(&widgets.copy_menu_button, "Copy", "y/Y", state);
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

fn connect_input_mode_focus(widgets: &Widgets, state: &SharedState) {
    connect_insert_focus(&widgets.saved_name_entry, widgets, state);
    connect_insert_focus(&widgets.saved_query_entry, widgets, state);
    connect_insert_focus(&widgets.custom_tag_entry, widgets, state);
    connect_insert_focus(&widgets.search_entry, widgets, state);
    connect_insert_focus(&widgets.compose_from, widgets, state);
    connect_insert_focus(&widgets.compose_to, widgets, state);
    connect_insert_focus(&widgets.compose_cc, widgets, state);
    connect_insert_focus(&widgets.compose_bcc, widgets, state);
    connect_insert_focus(&widgets.compose_subject, widgets, state);
    connect_insert_focus(&widgets.compose_body, widgets, state);
}

fn connect_insert_focus<W>(widget: &W, widgets: &Widgets, state: &SharedState)
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
        state.input_mode = InputMode::Insert;
        drop(state);
        update_button_binding_labels(&w, &st);
    });
    widget.add_controller(focus);
}

fn install_shortcuts(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
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
        if st.borrow().input_mode == InputMode::Insert {
            if key == gtk::gdk::Key::Escape {
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
            } else {
                vim_scroll_pages(&w, &st, 0.5);
            }
            return gtk::glib::Propagation::Stop;
        }
        if ctrl && (key == gtk::gdk::Key::u || key == gtk::gdk::Key::U) {
            if st.borrow().active_pane == ActivePane::Threads {
                select_thread_page(&opts, &w, &st, -1);
            } else {
                vim_scroll_pages(&w, &st, -0.5);
            }
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter {
            if st.borrow().active_pane == ActivePane::Threads {
                let idx = selected_thread_index(&w).unwrap_or(0);
                open_thread_by_index(&opts, &w, &st, idx);
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
    let pending_go = Rc::new(RefCell::new(false));
    let numeric_prefix = Rc::new(RefCell::new(String::new()));
    controller.connect_key_pressed(move |_, key, _, mods| {
        let ctrl = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK);
        if ctrl {
            return gtk::glib::Propagation::Proceed;
        }
        if st.borrow().input_mode == InputMode::Insert {
            return gtk::glib::Propagation::Proceed;
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
            } else if key == gtk::gdk::Key::a {
                open_saved_search_name(&opts, &w, &st, "All");
                set_active_pane(&w, &st, ActivePane::Threads);
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
                "Go: g top/count, 1-9 tags, i inbox, u unread, f flagged, s sent, d drafts, a all",
            );
            true
        } else if key == gtk::gdk::Key::j || key == gtk::gdk::Key::Down {
            if st.borrow().active_pane == ActivePane::Threads {
                select_relative_thread(&opts, &w, &st, count as isize);
            } else {
                vim_scroll_lines(&w, &st, count as f64);
            }
            clear_numeric_prefix(&numeric_prefix);
            true
        } else if key == gtk::gdk::Key::k || key == gtk::gdk::Key::Up {
            if st.borrow().active_pane == ActivePane::Threads {
                select_relative_thread(&opts, &w, &st, -(count as isize));
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
        } else if key == gtk::gdk::Key::r {
            clear_numeric_prefix(&numeric_prefix);
            reply_selected(&opts, &w, &st, ReplyKind::Sender);
            true
        } else if key == gtk::gdk::Key::R {
            clear_numeric_prefix(&numeric_prefix);
            reply_selected(&opts, &w, &st, ReplyKind::All);
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
            undo_last_tag(&opts, &w, &st, &undo);
            true
        } else if key == gtk::gdk::Key::F {
            clear_numeric_prefix(&numeric_prefix);
            forward_selected(&opts, &w, &st);
            true
        } else if key == gtk::gdk::Key::v {
            clear_numeric_prefix(&numeric_prefix);
            toggle_text_visual_view(&opts, &w, &st);
            true
        } else if key == gtk::gdk::Key::q {
            clear_numeric_prefix(&numeric_prefix);
            toggle_quote_collapse(&opts, &w, &st);
            true
        } else if key == gtk::gdk::Key::y {
            clear_numeric_prefix(&numeric_prefix);
            copy_selected_message_id(&w, &st);
            true
        } else if key == gtk::gdk::Key::Y {
            clear_numeric_prefix(&numeric_prefix);
            copy_selected_thread_id(&w, &st);
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
            (state.can_load_more_threads, state.thread_list_items.len())
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
    let len = state.borrow().thread_list_items.len();
    if len == 0 {
        return;
    }
    let current = selected_thread_index(widgets).unwrap_or(0) as isize;
    let next = (current + delta).clamp(0, len.saturating_sub(1) as isize) as i32;
    if let Some(row) = widgets.thread_list.row_at_index(next) {
        let already_selected = selected_thread_index(widgets) == Some(next as usize);
        widgets.thread_list.select_row(Some(&row));
        focus_thread_row(&row);
        if already_selected {
            select_thread_by_index(options, widgets, state, next as usize, false);
        }
    }
}

fn toggle_unread_selected(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) {
    let has_unread = state
        .borrow()
        .selected_thread
        .as_ref()
        .map(|thread| thread.has_unread)
        .unwrap_or(false);
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
    let flagged = state
        .borrow()
        .selected_thread
        .as_ref()
        .map(|thread| thread.is_flagged)
        .unwrap_or(false);
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

fn refresh_address_suggestions(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let result = (|| -> anyhow::Result<Vec<String>> {
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
    })();
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
    let suggestions = matching_address_suggestions(input, &state.borrow().address_suggestions, 6);
    if suggestions.is_empty() {
        populate_address_suggestions_list(widgets, &[]);
        widgets.address_suggestions_popover.popdown();
    } else {
        populate_address_suggestions_list(widgets, &suggestions);
        widgets.address_suggestions_popover.popup();
    }
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

fn apply_recipient_suggestion(entry: &gtk::Entry, suggestion: &str) {
    let current = entry.text().to_string();
    let next = if let Some((head, _)) = current.rsplit_once(',') {
        format!("{}, {}", head.trim_end(), suggestion)
    } else {
        suggestion.to_string()
    };
    entry.set_text(&next);
    entry.set_position(-1);
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
    widgets.address_suggestions_popover.popdown();
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
    update_attachment_label(widgets, &fields.attachments);
    state.borrow_mut().compose_fields = fields;
    update_draft_action_buttons(widgets, state);
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
    if html_view_is_visible(widgets) {
        show_rendered_selected_thread(options, widgets, state);
    } else {
        show_visual_html_selected_message(options, widgets, state);
    }
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
    if let Some(thread) = selected_thread {
        widgets.read_toggle_button.set_label(if thread.has_unread {
            "Mark read"
        } else {
            "Mark unread"
        });
        widgets
            .flag_toggle_button
            .set_label(if thread.is_flagged { "Unflag" } else { "Flag" });
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
            "Sanitized HTML view: JavaScript disabled; {image_policy}; link navigation blocked in-app."
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
    widgets.address_suggestions_popover.popdown();
    widgets.html_policy_row.set_visible(false);
    widgets.message_header_label.set_visible(false);
    widgets.attachment_title.set_visible(false);
    widgets.attachment_scrolled.set_visible(false);
    widgets.message_stack.set_visible_child_name("compose");
}

fn configure_html_webview(view: &webkit6::WebView, allow_remote_images: bool) {
    if let Some(settings) = WebViewExt::settings(view) {
        settings.set_enable_javascript(false);
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
                status.set_text(&format!(
                    "Blocked HTML navigation; copy/open manually after checking target: {uri}"
                ));
                return true;
            }
        }
        false
    });
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
    undo_button: &gtk::Button,
    compose_button: &gtk::Button,
    reply_button: &gtk::Button,
    reply_all_button: &gtk::Button,
    forward_button: &gtk::Button,
    forward_attachment_button: &gtk::Button,
    debug_button: &gtk::Button,
    palette_button: &gtk::Button,
    settings_button: &gtk::Button,
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
    undo_button.connect_clicked(move |_| undo_last_tag(&opts, &w, &st, &undo));

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
    let data = SearchData {
        query: query.to_string(),
        threads,
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
            state.thread_list_items.len(),
            state.can_load_more_threads,
        )
    };
    if !can_load_more {
        widgets
            .status_label
            .set_text("All currently counted threads are already loaded");
        return;
    }
    set_thread_loading_indicator(widgets, "Loading more messages…");
    match execute_search_page(options, &query, offset) {
        Ok(data) => append_search_data(options, widgets, state, data),
        Err(err) => apply_search_error(widgets, state, err),
    }
}

fn apply_search_data(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    data: SearchData,
) {
    {
        let mut s = state.borrow_mut();
        s.current_query = data.query.clone();
        s.thread_list_items = data.threads;
        s.thread_total_count = data.count;
        s.thread_loaded_count = s.thread_list_items.len();
        s.thread_page_size = data.limit;
        s.can_load_more_threads = s.thread_list_items.len() < data.count as usize;
        s.thread_details.clear();
        s.selected_thread = None;
        s.selected_message = None;
        s.messages.clear();
        s.visible_tags = data.tags;
        s.database_path = Some(data.database_path);
        s.database_revision = Some(data.revision);
        s.last_error = None;
        s.last_operation = Some(format!(
            "search `{}` loaded {} of {} thread(s){}",
            data.query,
            s.thread_list_items.len(),
            data.count,
            if data.cached { " from cache" } else { "" }
        ));
    }
    populate_thread_list(options, widgets, state);
    update_tag_searches(options, widgets, state);
    refresh_thread_attachment_list(widgets, state);
    update_message_menu(options, widgets, state);
    widgets.status_label.set_text(&format!(
        "{} of {} thread(s) for {}{}",
        state.borrow().thread_loaded_count,
        state.borrow().thread_total_count,
        data.query,
        if data.cached { " (cached)" } else { "" }
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
    let selected_thread_id = state
        .borrow()
        .selected_thread
        .as_ref()
        .map(|thread| thread.thread_id.clone());
    let selected_index = selected_thread_index(widgets);
    {
        let mut s = state.borrow_mut();
        s.current_query = data.query.clone();
        s.thread_list_items.extend(data.threads);
        s.thread_total_count = data.count;
        s.thread_loaded_count = s.thread_list_items.len();
        s.thread_page_size = data.limit;
        s.can_load_more_threads = s.thread_list_items.len() < data.count as usize;
        s.visible_tags = data.tags;
        s.database_path = Some(data.database_path);
        s.database_revision = Some(data.revision);
        s.last_error = None;
        s.last_operation = Some(format!(
            "loaded page at offset {}: {} of {} thread(s){}",
            data.offset,
            s.thread_list_items.len(),
            data.count,
            if data.cached { " from cache" } else { "" }
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
        widgets.status_label.set_text(&format!(
            "Loaded {} of {} thread(s)",
            state.borrow().thread_loaded_count,
            state.borrow().thread_total_count
        ));
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
    widgets.thread_result_label.set_text(&format!(
        "Loaded {} of {} thread(s) · page size {}",
        state_ref.thread_loaded_count, state_ref.thread_total_count, state_ref.thread_page_size
    ));
    let can_load_more = state_ref.can_load_more_threads;
    drop(state_ref);
    set_button_label(&widgets.load_more_button, "Load more", "G", state);
    widgets.load_more_button.set_sensitive(can_load_more);
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

fn populate_thread_list(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    while let Some(child) = widgets.thread_list.first_child() {
        widgets.thread_list.remove(&child);
    }
    let threads = state.borrow().thread_list_items.clone();
    let details = visible_thread_details(options, state, &threads);
    state.borrow_mut().thread_details = details.clone();
    for (idx, thread) in threads.iter().enumerate() {
        let row = gtk::ListBoxRow::new();
        row.set_widget_name(&format!("notm-thread-row-{idx}"));
        if thread.has_unread {
            row.add_css_class("unread");
        }
        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 2);
        box_.set_margin_start(6);
        box_.set_margin_end(6);
        box_.set_margin_top(6);
        box_.set_margin_bottom(6);
        let detail = details.get(&thread.thread_id).cloned().unwrap_or_default();
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
        widgets.thread_list.append(&row);
    }
}

fn visible_thread_details(
    options: &LaunchOptions,
    state: &SharedState,
    threads: &[notm_notmuch::ThreadSummary],
) -> BTreeMap<String, ThreadUiDetails> {
    let (database_path, revision) = {
        let state = state.borrow();
        (state.database_path.clone(), state.database_revision.clone())
    };
    let mut out = BTreeMap::new();
    let Ok(db) = Database::open(&open_config(options), DatabaseMode::ReadOnly) else {
        return out;
    };
    for thread in threads {
        let cache_key = thread_detail_cache_key(
            database_path.as_deref().unwrap_or(""),
            revision.as_ref(),
            &thread.thread_id,
        );
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
    let total = (state.thread_total_count as usize).max(loaded);
    let number = index.saturating_add(1).min(total.max(1));
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
                show_selected_message_text_view(options, widgets, state);
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
                show_selected_message_text_view(options, widgets, state);
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
    update_debug(widgets, state);
}

fn tag_selected(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    mutation: TagMutation,
) {
    let Some(thread) = state.borrow().selected_thread.clone() else {
        widgets
            .status_label
            .set_text("No selected thread for tag operation");
        return;
    };
    let query = format!("thread:{}", thread.thread_id);
    let result = (|| -> anyhow::Result<usize> {
        let db = Database::open(&open_config(options), DatabaseMode::ReadWrite)?;
        let report = db.apply_tags_to_query(&query, &mutation)?;
        if report.changed_messages == 0 {
            undo_state.borrow_mut().take();
        } else {
            *undo_state.borrow_mut() = Some((
                query.clone(),
                TagMutation {
                    add: mutation.remove.clone(),
                    remove: mutation.add.clone(),
                    sync_maildir_flags: mutation.sync_maildir_flags,
                },
            ));
        }
        state.borrow_mut().last_operation = Some(format!(
            "tagged {} message(s): +{:?} -{:?}",
            report.changed_messages, report.added, report.removed
        ));
        Ok(report.changed_messages)
    })();
    match result {
        Ok(changed_messages) => {
            let current = state.borrow().current_query.clone();
            run_search(options, widgets, state, &current);
            let undo_available = changed_messages > 0;
            set_undo_tag_available(widgets, undo_available);
            if undo_available {
                widgets
                    .status_label
                    .set_text("Tag operation complete; Undo tag reverses this change");
            } else {
                widgets
                    .status_label
                    .set_text("Tag operation made no changes");
            }
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Tag operation failed: {err}"));
            update_debug(widgets, state);
        }
    }
}

fn set_undo_tag_available(widgets: &Widgets, available: bool) {
    widgets.undo_tag_button.set_visible(available);
    if available {
        widgets.undo_tag_button.add_css_class("suggested-action");
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
        show_selected_message_text_view(options, widgets, state);
    }
}

fn undo_last_tag(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) {
    let Some((query, mutation)) = undo_state.borrow().clone() else {
        set_undo_tag_available(widgets, false);
        widgets.status_label.set_text("No tag operation to undo");
        return;
    };
    let result = (|| -> anyhow::Result<()> {
        let db = Database::open(&open_config(options), DatabaseMode::ReadWrite)?;
        db.apply_tags_to_query(&query, &mutation)?;
        state.borrow_mut().last_operation = Some("undid last tag operation".to_string());
        Ok(())
    })();
    match result {
        Ok(()) => {
            undo_state.borrow_mut().take();
            set_undo_tag_available(widgets, false);
            let current = state.borrow().current_query.clone();
            run_search(options, widgets, state, &current);
            widgets.status_label.set_text("Undid last tag operation");
        }
        Err(err) => {
            set_undo_tag_available(widgets, true);
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Undo failed: {err}"));
            update_debug(widgets, state);
        }
    }
}

fn run_manual_sync(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    if !options.sync_enabled {
        widgets.status_label.set_text("Manual sync is disabled");
        state.borrow_mut().last_operation = Some("manual sync disabled".to_string());
        update_debug(widgets, state);
        return;
    }
    let mut commands = Vec::new();
    if options.external_receive_enabled && !options.external_receive_command.trim().is_empty() {
        commands.push(("external_receive", options.external_receive_command.clone()));
    }
    if options.notmuch_database_update_enabled
        && !options.notmuch_database_update_command.trim().is_empty()
    {
        commands.push((
            "notmuch_database_update",
            options.notmuch_database_update_command.clone(),
        ));
    }
    if commands.is_empty() {
        widgets
            .status_label
            .set_text("Manual sync enabled but no sync commands configured");
        state.borrow_mut().last_operation = Some("manual sync no-op".to_string());
        update_debug(widgets, state);
        return;
    }
    let result = (|| -> anyhow::Result<Vec<String>> {
        let mut reports = Vec::new();
        for (name, command) in commands {
            let output = Command::new("sh").arg("-c").arg(&command).output()?;
            reports.push(format!(
                "{name}: status={:?} stdout={} stderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
            anyhow::ensure!(
                output.status.success(),
                "manual sync command `{name}` failed with status {:?}",
                output.status.code()
            );
        }
        Ok(reports)
    })();
    match result {
        Ok(reports) => {
            state.borrow_mut().last_operation =
                Some(format!("manual sync: {}", reports.join("; ")));
            widgets.status_label.set_text("Manual sync completed");
            let current = state.borrow().current_query.clone();
            run_search(options, widgets, state, &current);
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Manual sync failed: {err}"));
            update_debug(widgets, state);
        }
    }
}

fn open_compose(widgets: &Widgets, state: &SharedState) {
    show_compose_view(widgets);
    set_active_draft(widgets, state, None);
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
    if let Some(command) = &options.send_command {
        let rt = tokio::runtime::Runtime::new()?;
        let transport = ExternalCommandTransport {
            command: command.clone(),
            args: options.send_args.clone(),
            mode: options.send_mode.clone(),
            working_dir: None,
            env: Default::default(),
            timeout: Duration::from_secs(120),
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
                "compose_set_body" => widgets.compose_body.buffer().set_text(value),
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

fn menu_button_with_box(label: &str, widget_name: &str) -> (gtk::MenuButton, gtk::Box) {
    let button = gtk::MenuButton::new();
    button.set_label(label);
    button.set_widget_name(widget_name);
    let popover = gtk::Popover::new();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    popover.set_child(Some(&menu));
    button.set_popover(Some(&popover));
    (button, menu)
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
        "inbox, unread, flagged, sent, all",
        "search, compose, reply, reply_all, forward, forward_as_attachment",
        "archive, mark_read, mark_unread, flag, unflag, trash, undo",
        "raw_source, full_headers, text, visual_html, image_policy, collapse_quotes",
        "save_attachment, open_attachment",
        "copy_message_id, copy_thread_id",
        "debug, settings, shortcuts, manual_sync (only if explicitly enabled)",
    ]
}

fn show_settings(widgets: &Widgets, options: &LaunchOptions) {
    let dialog = gtk::Dialog::builder()
        .title("notm settings")
        .transient_for(&widgets.window)
        .modal(true)
        .default_width(600)
        .build();
    dialog.set_widget_name("notm-settings-dialog");
    let area = dialog.content_area();
    area.set_spacing(6);
    let text = format!(
        "Database: {}\nNotmuch config: {}\nApp config: {}\nProfile: {}\nSync: disabled by default; no startup sync is implemented.\nRemote images: {}.\nTrusted image senders: {}\nAutomation: {}\n",
        options
            .database_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "libnotmuch default".to_string()),
        options
            .config_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "libnotmuch default".to_string()),
        options
            .app_config_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not configured".to_string()),
        options.profile.as_deref().unwrap_or("default"),
        if options.remote_images {
            "enabled"
        } else {
            "disabled"
        },
        options.trusted_image_senders.join(", "),
        options.automation_enabled,
    );
    let label = gtk::Label::new(Some(&text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    area.append(&label);
    let default_query = entry_with_placeholder("Default query");
    default_query.set_widget_name("notm-settings-default-query");
    default_query.set_text(&options.default_query);
    let page_size = entry_with_placeholder("Page size");
    page_size.set_widget_name("notm-settings-page-size");
    page_size.set_text(&options.page_size.to_string());
    let send_command = entry_with_placeholder("Send command");
    send_command.set_widget_name("notm-settings-send-command");
    send_command.set_text(
        &options
            .send_command
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    );
    area.append(&gtk::Label::new(Some(
        "Editable settings below are written to the app config file. Restart or relaunch for all settings to take full effect.",
    )));
    area.append(&default_query);
    area.append(&page_size);
    area.append(&send_command);
    dialog.add_button("Save", gtk::ResponseType::Accept);
    dialog.add_button("Close", gtk::ResponseType::Close);
    let opts = options.clone();
    let status = widgets.status_label.clone();
    dialog.connect_response(move |d, response| {
        if response == gtk::ResponseType::Accept {
            let page_size_value = page_size.text().parse::<usize>().unwrap_or(opts.page_size);
            match persist_basic_settings(
                &opts,
                &default_query.text(),
                page_size_value,
                &send_command.text(),
            ) {
                Ok(()) => status.set_text("Settings saved to app config"),
                Err(err) => status.set_text(&format!("Settings save failed: {err}")),
            }
        }
        d.close();
    });
    dialog.present();
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
}
