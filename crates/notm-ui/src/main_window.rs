use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use chrono::Utc;
use gtk::glib::variant::{StaticVariantType, ToVariant};
use gtk::prelude::*;
use gtk4 as gtk;
use notm_mail::{
    ComposedMessage, ExternalCommandTransport, FakeSendTransport, MailtoRequest, ReplyKind,
    SendTransport, TransportMode,
    address::{dedupe_addresses, format_address, parse_address_list},
    compose::{AttachmentInput, Identity},
    html_sanitize::sanitize_html,
    mime::parse_file,
    parse_mailto_uri,
};
use notm_notmuch::{
    Database, DatabaseMode, MessageTagMutation, OpenConfig, QueryOptions, SortOrder, TagMutation,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;
use webkit6::{
    NavigationPolicyDecision, PolicyDecisionType,
    prelude::{PolicyDecisionExt, WebViewExt},
};

use crate::{
    automation::{self, AutomationConfig, AutomationRequest},
    model::{
        ActiveDraft, ActivePane, ComposeFields, ContentLayout, InputMode, LayoutPreference,
        MessageViewPreference, ThemePreference, UiState,
    },
    screenshot, theme,
    widgets::attachments::{
        self, AttachmentActionResult, AttachmentController, AttachmentEvent,
        AttachmentEventHandler, AttachmentOpenStore,
    },
    widgets::composer::{
        self, ComposerController, ComposerPaths, ComposerReplacementKind, DRAFT_LIST_MAX_HEIGHT,
        DRAFT_LIST_MIN_HEIGHT, DraftSaveReport, PendingAction, PendingOperation, TransitionHooks,
        composer_requires_confirmation, fields_has_content,
    },
    widgets::link_hints::{LinkHintController, LinkHintOpener, html_link_scheme_is_external_safe},
    widgets::search_bar::{
        self, SearchActivityState, SearchBarController, SearchHarnessPolicy, SearchInputEvent,
        SearchWorkerRequest, begin_search_activity, cancel_search_activity, finish_search_activity,
    },
    widgets::settings::{
        self, SettingsApplication, SettingsApplicationOutcome, SettingsController,
        SettingsDialogSeed, layout_preference_name, parse_layout_preference,
        try_parse_layout_preference,
    },
    widgets::standalone_message::{
        StandaloneHtmlRender, StandaloneHtmlRenderer, StandaloneHtmlScroll,
        StandaloneHtmlScrollHandler, StandaloneHtmlViewFactory, StandaloneHtmlViewInitializer,
        StandaloneImagePolicy, StandaloneMessageController, StandaloneMessageHasHtml,
        StandaloneOpenOptions, StandalonePolicyProvider, StandalonePolicySnapshot,
        StandalonePreferredView, StandaloneRememberView, StandaloneResponseAction,
        StandaloneResponseHandler, StandaloneResponseRequest, StandaloneSenderView,
        StandaloneTextRenderer, StandaloneToggleSenderView,
    },
    widgets::thread_list::{
        self, AppendSearchOutcome, LoadMoreDecision, LocatePagePlan, ReplaceSearchOutcome,
        SearchErrorOutcome, SearchPageCoordinator, SearchPageRequest, SearchPageResponse,
        SearchRuntimeSnapshot, ThreadDisplayToggle, ThreadListController, ThreadListDisplay,
        ThreadModelSnapshot, ThreadModelUpdate, ThreadPagingSnapshot, ThreadRowSnapshot,
        ThreadSearchStateSnapshot, ThreadSearchStateUpdate, format_count, format_thread_list_date,
        thread_window_status as thread_window_status_from_parts,
    },
};

pub use crate::widgets::settings::{RuntimeSettings, RuntimeSettingsStore};

const NORMAL_APPLICATION_ID: &str = "io.github.kris004.notm";
const TEST_HARNESS_APPLICATION_ID_NAMESPACE: &str = "io.github.kris004.notm.test.";
const TEST_HARNESS_APPLICATION_ID_PREFIX: &str = "io.github.kris004.notm.test.t";
const TEST_HARNESS_APPLICATION_ID_ENV: &str = "NOTM_TEST_HARNESS_APPLICATION_ID";
const OPEN_MESSAGE_ID_ACTION: &str = "open-message-id";
const COMPOSE_MAILTO_ACTION: &str = "compose-mailto";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSearch {
    pub name: String,
    pub query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchOptions {
    pub database_path: Option<PathBuf>,
    pub mail_root: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub profile: Option<String>,
    pub default_query: String,
    pub excluded_tags: Vec<String>,
    pub page_size: usize,
    pub theme: ThemePreference,
    pub thread_preview_lines: usize,
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
    pub sync_timeout_seconds: u64,
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
    pub fixture_mode: bool,
    pub allow_live_send_test: bool,
    pub allow_live_tag_test: bool,
    pub show_debug_panel: bool,
    pub start_maximized: bool,
    pub show_sidebar: bool,
    pub show_message_list: bool,
    pub show_message_view: bool,
    pub remote_images: bool,
    pub show_thread_numbers: bool,
    pub show_thread_dates: bool,
    pub show_thread_tags: bool,
    pub show_thread_preview: bool,
    pub show_keybind_hints: bool,
    pub layout: String,
    pub html_mode: String,
    pub message_view_preferences: BTreeMap<String, MessageViewPreference>,
    pub sender_view_preferences: BTreeMap<String, MessageViewPreference>,
    pub hidden_tag_searches: Vec<String>,
    pub sync_maildir_flags_after_tag_change: bool,
    pub draft_path: Option<PathBuf>,
    pub drafts_dir: Option<PathBuf>,
    pub app_config_path: Option<PathBuf>,
    pub custom_saved_searches: Vec<SavedSearch>,
    pub open_message_id: Option<String>,
    pub mailto_uri: Option<String>,
    #[serde(skip)]
    pub runtime_settings: RuntimeSettingsStore,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            database_path: None,
            mail_root: None,
            config_path: None,
            profile: None,
            default_query: "tag:inbox and not tag:trash and not tag:spam".to_string(),
            excluded_tags: vec!["trash".to_string(), "spam".to_string()],
            page_size: 100,
            theme: ThemePreference::System,
            thread_preview_lines: 2,
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
            sync_timeout_seconds: 300,
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
            fixture_mode: false,
            allow_live_send_test: false,
            allow_live_tag_test: false,
            show_debug_panel: false,
            start_maximized: false,
            show_sidebar: true,
            show_message_list: true,
            show_message_view: true,
            remote_images: false,
            show_thread_numbers: true,
            show_thread_dates: true,
            show_thread_tags: true,
            show_thread_preview: true,
            show_keybind_hints: true,
            layout: "auto".to_string(),
            html_mode: "sanitize_then_render_text_fallback".to_string(),
            message_view_preferences: BTreeMap::new(),
            sender_view_preferences: BTreeMap::new(),
            hidden_tag_searches: Vec::new(),
            sync_maildir_flags_after_tag_change: true,
            draft_path: None,
            drafts_dir: None,
            app_config_path: None,
            custom_saved_searches: Vec::new(),
            open_message_id: None,
            mailto_uri: None,
            runtime_settings: Default::default(),
        }
    }
}

pub fn launch(options: LaunchOptions) -> anyhow::Result<()> {
    validate_launch_options(&options)?;
    sync_runtime_settings_from_launch_options(&options);
    let attachment_open_store = AttachmentOpenStore::create()?;
    let attachment_open_dir = attachment_open_store.path().to_path_buf();
    let app_builder = gtk::Application::builder()
        .application_id(application_id_for_launch(&options)?)
        .flags(application_flags_for_launch(&options));
    let app = app_builder.build();
    let main_window = Rc::new(RefCell::new(None::<MainWindowHandle>));

    add_open_message_id_action(&app, &options, &main_window, &attachment_open_dir);
    add_compose_mailto_action(&app, &options, &main_window, &attachment_open_dir);
    let activate_options = options.clone();
    let activate_main_window = main_window.clone();
    let activate_attachment_open_dir = attachment_open_dir.clone();
    app.connect_activate(move |app| {
        open_or_present_main_window(
            app,
            &activate_options,
            &activate_main_window,
            &activate_attachment_open_dir,
            None,
            None,
        );
    });

    if options.open_message_id.is_some() || options.mailto_uri.is_some() {
        app.register(gtk::gio::Cancellable::NONE)?;
        if app.is_remote() {
            if let Some(message_id) = &options.open_message_id {
                anyhow::ensure!(
                    app.has_action(OPEN_MESSAGE_ID_ACTION),
                    "the running notm instance does not support message-id routing; restart it \
                     and try again"
                );
                app.activate_action(OPEN_MESSAGE_ID_ACTION, Some(&message_id.to_variant()));
            } else if let Some(mailto_uri) = &options.mailto_uri {
                anyhow::ensure!(
                    app.has_action(COMPOSE_MAILTO_ACTION),
                    "the running notm instance does not support mailto routing; restart it and \
                     try again"
                );
                app.activate_action(COMPOSE_MAILTO_ACTION, Some(&mailto_uri.to_variant()));
            }
            let connection = app
                .dbus_connection()
                .ok_or_else(|| anyhow::anyhow!("remote notm instance has no D-Bus connection"))?;
            connection.flush_sync(gtk::gio::Cancellable::NONE)?;
            return Ok(());
        }
    }

    app.run_with_args(&["notm"]);
    attachment_open_store.close()?;
    Ok(())
}

fn validate_launch_options(options: &LaunchOptions) -> anyhow::Result<()> {
    settings::validate_thread_preview_lines(options.thread_preview_lines)?;
    anyhow::ensure!(
        options.open_message_id.is_none() || options.mailto_uri.is_none(),
        "message-id and mailto launch targets cannot be combined"
    );
    if let Some(uri) = options.mailto_uri.as_deref() {
        parse_mailto_uri(uri).map_err(|error| anyhow::anyhow!("invalid mailto URI: {error}"))?;
    }
    Ok(())
}

fn application_id_for_launch(options: &LaunchOptions) -> anyhow::Result<String> {
    if options.automation_enabled {
        if let Some(application_id) = std::env::var_os(TEST_HARNESS_APPLICATION_ID_ENV) {
            let application_id = application_id.into_string().map_err(|_| {
                anyhow::anyhow!("{TEST_HARNESS_APPLICATION_ID_ENV} must be valid UTF-8")
            })?;
            anyhow::ensure!(
                application_id.starts_with(TEST_HARNESS_APPLICATION_ID_NAMESPACE),
                "{TEST_HARNESS_APPLICATION_ID_ENV} must use the test-harness namespace \
                 {TEST_HARNESS_APPLICATION_ID_NAMESPACE}"
            );
            anyhow::ensure!(
                gtk::gio::Application::id_is_valid(&application_id),
                "{TEST_HARNESS_APPLICATION_ID_ENV} is not a valid application ID"
            );
            return Ok(application_id);
        }
        Ok(format!(
            "{}{}",
            TEST_HARNESS_APPLICATION_ID_PREFIX,
            Uuid::new_v4().simple()
        ))
    } else {
        Ok(NORMAL_APPLICATION_ID.to_string())
    }
}

fn application_flags_for_launch(_options: &LaunchOptions) -> gtk::gio::ApplicationFlags {
    gtk::gio::ApplicationFlags::empty()
}

fn add_open_message_id_action(
    app: &gtk::Application,
    options: &LaunchOptions,
    main_window: &Rc<RefCell<Option<MainWindowHandle>>>,
    attachment_open_dir: &Path,
) {
    let action =
        gtk::gio::SimpleAction::new(OPEN_MESSAGE_ID_ACTION, Some(&String::static_variant_type()));
    let action_app = app.clone();
    let action_options = options.clone();
    let action_main_window = main_window.clone();
    let action_attachment_open_dir = attachment_open_dir.to_path_buf();
    action.connect_activate(move |_, parameter| {
        let Some(message_id) = parameter.and_then(|value| value.get::<String>()) else {
            return;
        };
        open_or_present_main_window(
            &action_app,
            &action_options,
            &action_main_window,
            &action_attachment_open_dir,
            Some(message_id),
            None,
        );
    });
    app.add_action(&action);
}

fn add_compose_mailto_action(
    app: &gtk::Application,
    options: &LaunchOptions,
    main_window: &Rc<RefCell<Option<MainWindowHandle>>>,
    attachment_open_dir: &Path,
) {
    let action =
        gtk::gio::SimpleAction::new(COMPOSE_MAILTO_ACTION, Some(&String::static_variant_type()));
    let action_app = app.clone();
    let action_options = options.clone();
    let action_main_window = main_window.clone();
    let action_attachment_open_dir = attachment_open_dir.to_path_buf();
    action.connect_activate(move |_, parameter| {
        let Some(mailto_uri) = parameter.and_then(|value| value.get::<String>()) else {
            return;
        };
        if let Err(error) = parse_mailto_uri(&mailto_uri) {
            tracing::warn!(%error, "ignored invalid mailto application action");
            if let Some(handle) = action_main_window.borrow().as_ref() {
                report_mailto_error(&handle.widgets, &handle.state, &error);
            }
            return;
        }
        open_or_present_main_window(
            &action_app,
            &action_options,
            &action_main_window,
            &action_attachment_open_dir,
            None,
            Some(mailto_uri),
        );
    });
    app.add_action(&action);
}

fn resolved_new_window_message_id(
    requested_message_id: Option<String>,
    launch_message_id: Option<&str>,
) -> Option<String> {
    requested_message_id.or_else(|| launch_message_id.map(ToOwned::to_owned))
}

fn resolved_new_window_mailto_uri(
    requested_mailto_uri: Option<String>,
    launch_mailto_uri: Option<&str>,
) -> Option<String> {
    requested_mailto_uri.or_else(|| launch_mailto_uri.map(ToOwned::to_owned))
}

fn open_or_present_main_window(
    app: &gtk::Application,
    options: &LaunchOptions,
    main_window: &Rc<RefCell<Option<MainWindowHandle>>>,
    attachment_open_dir: &Path,
    open_message_id: Option<String>,
    mailto_uri: Option<String>,
) {
    if let Some(handle) = main_window.borrow().as_ref().cloned() {
        handle.widgets.close_when_idle.set(false);
        if let Some(message_id) = open_message_id {
            open_message_id_request(options, &handle.widgets, &handle.state, &message_id);
        }
        if let Some(mailto_uri) = mailto_uri {
            let _ = open_mailto_uri_request(options, &handle.widgets, &handle.state, &mailto_uri);
        }
        handle.window.present();
        return;
    }

    let mut launch_options = options.clone();
    launch_options.open_message_id =
        resolved_new_window_message_id(open_message_id, options.open_message_id.as_deref());
    launch_options.mailto_uri =
        resolved_new_window_mailto_uri(mailto_uri, options.mailto_uri.as_deref());
    let handle = build_ui(app, launch_options, attachment_open_dir.to_path_buf());
    let main_window_weak = Rc::downgrade(main_window);
    handle.window.connect_destroy(move |window| {
        let Some(main_window) = main_window_weak.upgrade() else {
            return;
        };
        let is_current_window = main_window
            .borrow()
            .as_ref()
            .is_some_and(|handle| handle.window == *window);
        if is_current_window {
            main_window.borrow_mut().take();
        }
    });
    *main_window.borrow_mut() = Some(handle);
}

fn sync_runtime_settings_from_launch_options(options: &LaunchOptions) {
    settings::update(
        &options.runtime_settings,
        RuntimeSettings {
            page_size: options.page_size.max(1),
            theme: options.theme,
            thread_preview_lines: options.thread_preview_lines,
            excluded_tags: options.excluded_tags.clone(),
            sync_maildir_flags_after_tag_change: options.sync_maildir_flags_after_tag_change,
            remote_images: options.remote_images,
            layout_preference: parse_layout_preference(&options.layout),
        },
    );
}

#[derive(Clone)]
struct Widgets {
    window: gtk::ApplicationWindow,
    gtk_settings: gtk::Settings,
    css_provider: gtk::CssProvider,
    theme_background_probe: gtk::Label,
    settings: SettingsController,
    overlay: gtk::Overlay,
    outer_paned: gtk::Paned,
    content_paned: gtk::Paned,
    left_pane: gtk::ScrolledWindow,
    thread_pane: gtk::Box,
    message_pane: gtk::Box,
    saved_box: gtk::Box,
    custom_search_menu_button: gtk::MenuButton,
    saved_name_entry: gtk::Entry,
    saved_query_entry: gtk::Entry,
    save_search_button: gtk::Button,
    custom_tag_entry: gtk::Entry,
    search_bar: SearchBarController,
    sync_refresh_generation: Rc<Cell<Option<u64>>>,
    input_mode_generation: Rc<Cell<u64>>,
    hidden_tag_searches: HiddenTagSearchStore,
    thread_list: ThreadListController,
    manual_sync_button: Option<gtk::Button>,
    compose_button: gtk::Button,
    debug_button: gtk::Button,
    palette_button: gtk::Button,
    settings_button: gtk::Button,
    help_button: gtk::Button,
    sidebar_toggle_button: gtk::Button,
    thread_list_toggle_button: gtk::Button,
    message_pane_toggle_button: gtk::Button,
    layout_toggle_button: gtk::Button,
    archive_button: gtk::Button,
    read_toggle_button: gtk::Button,
    flag_toggle_button: gtk::Button,
    trash_button: gtk::Button,
    spam_button: gtk::Button,
    tag_command_entry: gtk::Entry,
    tag_command_button: gtk::Button,
    tag_command_apply_button: gtk::Button,
    tag_menu_button: gtk::MenuButton,
    tag_menu_box: gtk::Box,
    single_tag_button: gtk::Button,
    single_tag_editor_box: gtk::Box,
    single_tag_action_label: gtk::Label,
    single_tag_apply_button: gtk::Button,
    tag_command_editor_box: gtk::Box,
    undo_tag_button: gtk::MenuButton,
    undo_menu_box: gtk::Box,
    undo_last_tag_button: gtk::Button,
    undo_list_tag_button: gtk::Button,
    message_stack: gtk::Stack,
    message_view: gtk::TextView,
    message_scrolled: gtk::ScrolledWindow,
    html_view: webkit6::WebView,
    html_load_generation: Rc<Cell<u64>>,
    html_completed_load_generation: Rc<Cell<u64>>,
    html_scrolled: gtk::ScrolledWindow,
    link_hints: LinkHintController,
    response_menu_button: gtk::MenuButton,
    reply_button: gtk::Button,
    reply_all_button: gtk::Button,
    forward_button: gtk::Button,
    forward_attachment_button: gtk::Button,
    response_menu_box: gtk::Box,
    message_menu_button: gtk::MenuButton,
    message_menu_box: gtk::Box,
    message_tag_menu_button: gtk::MenuButton,
    message_tag_menu_box: gtk::Box,
    message_archive_button: gtk::Button,
    message_read_toggle_button: gtk::Button,
    message_flag_toggle_button: gtk::Button,
    message_trash_button: gtk::Button,
    message_spam_button: gtk::Button,
    message_custom_tag_entry: gtk::Entry,
    message_custom_tag_action_label: gtk::Label,
    message_custom_tag_apply_button: gtk::Button,
    view_menu_button: gtk::MenuButton,
    view_menu_box: gtk::Box,
    view_text_button: gtk::Button,
    view_html_button: gtk::Button,
    view_headers_button: gtk::Button,
    view_raw_button: gtk::Button,
    sender_view_preference_button: gtk::Button,
    active_message_view: Rc<Cell<MessageViewKind>>,
    pending_html_scroll_fraction: Rc<Cell<Option<f64>>>,
    image_policy_button: gtk::Button,
    html_policy_row: gtk::Box,
    html_policy_label: gtk::Label,
    message_header_box: gtk::Box,
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
    attachments: AttachmentController,
    tag_search_box: gtk::Box,
    debug_view: gtk::TextView,
    status_label: gtk::Label,
    composer: ComposerController,
    close_when_idle: Rc<Cell<bool>>,
    standalone_messages: StandaloneMessageController,
}

type SharedState = Rc<RefCell<UiState>>;
type UndoState = Rc<RefCell<Vec<UndoTagAction>>>;
type SavedSearchStore = Rc<RefCell<Vec<SavedSearch>>>;
type HiddenTagSearchStore = Rc<RefCell<BTreeSet<String>>>;

impl SearchActivityState for UiState {
    fn search_generation(&self) -> u64 {
        self.search_generation
    }

    fn set_search_generation(&mut self, generation: u64) {
        self.search_generation = generation;
    }

    fn set_search_loading(&mut self, loading: bool) {
        self.search_loading = loading;
    }

    fn set_pending_search_query(&mut self, query: Option<String>) {
        self.pending_search_query = query;
    }

    fn set_search_error(&mut self, error: Option<String>) {
        self.search_error = error;
    }
}

#[derive(Clone)]
struct MainWindowHandle {
    window: gtk::ApplicationWindow,
    widgets: Widgets,
    state: SharedState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UndoTagAction {
    mutations: Vec<MessageTagMutation>,
    sync_maildir_flags: bool,
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UndoTagHistory {
    version: u8,
    actions: Vec<UndoTagAction>,
}

const UNDO_TAG_HISTORY_VERSION: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageViewKind {
    Text,
    Html,
    Headers,
    Raw,
}

impl MessageViewKind {
    const fn preference(self) -> MessageViewPreference {
        match self {
            Self::Text => MessageViewPreference::Text,
            Self::Html => MessageViewPreference::VisualHtml,
            Self::Headers => MessageViewPreference::FullHeaders,
            Self::Raw => MessageViewPreference::RawSource,
        }
    }

    const fn from_preference(preference: MessageViewPreference) -> Self {
        match preference {
            MessageViewPreference::Text => Self::Text,
            MessageViewPreference::VisualHtml => Self::Html,
            MessageViewPreference::FullHeaders => Self::Headers,
            MessageViewPreference::RawSource => Self::Raw,
        }
    }
}

struct AddressSuggestionsResponse {
    result: anyhow::Result<Vec<String>>,
}

enum PendingTransition {
    ClearComposer,
    ReplaceComposer(PreparedComposerReplacement),
    DeleteActiveDraft(ActiveDraft),
    DeleteNamedDraft(PathBuf),
    SaveDraftReplacement {
        fields: ComposeFields,
        previous: ActiveDraft,
    },
    SendComposer {
        fields: ComposeFields,
        active: ActiveDraft,
        generation: u64,
    },
    ShowSelectedMessage {
        selection: MessageSelectionSnapshot,
        rejection_restore: Option<MessageSelectionSnapshot>,
        status: String,
        active_pane: ActivePane,
        clear_saved_recovery: bool,
    },
    CloseMainWindow,
}

#[derive(Clone, Copy)]
enum PendingTransitionKind {
    ClearComposer,
    ReplaceComposer(ComposerReplacementKind),
    DeleteActiveDraft,
    DeleteNamedDraft,
    SaveDraftReplacement,
    SendComposer,
    ShowSelectedMessage,
    CloseMainWindow,
}

struct PreparedComposerReplacement {
    kind: ComposerReplacementKind,
    payload: ComposerReplacementPayload,
    selection: Option<MessageSelectionSnapshot>,
    rejection_restore: Option<MessageSelectionSnapshot>,
    status: String,
    source_status: Option<gtk::Label>,
    present_main_window: bool,
    show_message_pane: bool,
    active_pane: ActivePane,
}

#[derive(Clone)]
struct MessageSelectionSnapshot {
    selected_thread: Option<notm_notmuch::ThreadSummary>,
    selected_thread_index: Option<usize>,
    selected_message: Option<notm_notmuch::MessageSummary>,
    messages: Vec<notm_notmuch::MessageSummary>,
    active_pane: ActivePane,
    last_operation: Option<String>,
    last_error: Option<String>,
}

enum ComposerReplacementPayload {
    Empty,
    Fields(Box<ComposeFields>),
    Message(Box<ComposedMessage>),
    Draft(Box<PreparedDraftReplacement>),
}

struct PreparedDraftReplacement {
    fields: ComposeFields,
    active_source: Option<PreparedActiveDraft>,
    attachment_inputs: Vec<AttachmentInput>,
}

struct PreparedActiveDraft {
    path: PathBuf,
    message_id: Option<String>,
    indexed: bool,
}

const SIDEBAR_MIN_WIDTH: i32 = 136;
const THREAD_LIST_MIN_WIDTH: i32 = 320;
const MAX_SYNC_REFRESH_DELAY: Duration = Duration::from_secs(5);
// GTK measures the message header at unbounded width during compact pane allocation.
// Reserving multiple lines per metadata row can force the whole message pane taller
// than the available window; full values stay available via selection and tooltip.
const MESSAGE_HEADER_VALUE_LINES: i32 = 1;
const KEYBOARD_CURSOR_CLASS: &str = "notm-keyboard-cursor";
const STATUS_BAR_MAX_WIDTH_CHARS: i32 = 120;
const HTML_LINK_STATUS_URI_MAX_CHARS: usize = 96;
const HTML_DEFAULT_CONTENT_SECURITY_POLICY: &str = "default-src 'none'; img-src http: https:; style-src 'unsafe-inline'; script-src 'none'; connect-src 'none'; frame-src 'none'; font-src 'none'; media-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
// Cache enough normalized content for visual preview limits without keying the
// thread-detail cache by a presentation setting.
const AUTO_STACKED_BELOW_WIDTH: i32 = 1280;
const AUTO_THREE_PANE_ABOVE_WIDTH: i32 = 1360;
const MESSAGE_VIEW_MIN_WIDTH: i32 = 280;
const STACKED_TOP_MIN_HEIGHT: i32 = 260;
const STACKED_MESSAGE_MIN_HEIGHT: i32 = 280;

fn content_layout_name(layout: ContentLayout) -> &'static str {
    match layout {
        ContentLayout::ThreePane => "three_pane",
        ContentLayout::Stacked => "stacked",
    }
}

fn layout_for_preference(
    preference: LayoutPreference,
    width: i32,
    height: i32,
    current: ContentLayout,
) -> ContentLayout {
    match preference {
        LayoutPreference::ThreePane => ContentLayout::ThreePane,
        LayoutPreference::Stacked => ContentLayout::Stacked,
        LayoutPreference::Auto => auto_content_layout(width, height, current),
    }
}

fn auto_content_layout(width: i32, _height: i32, current: ContentLayout) -> ContentLayout {
    if width <= 0 {
        return current;
    }
    if width < AUTO_STACKED_BELOW_WIDTH {
        ContentLayout::Stacked
    } else if width > AUTO_THREE_PANE_ABOVE_WIDTH {
        ContentLayout::ThreePane
    } else {
        current
    }
}

fn build_ui(
    app: &gtk::Application,
    options: LaunchOptions,
    attachment_open_dir: PathBuf,
) -> MainWindowHandle {
    let display = gtk::gdk::Display::default().expect("GTK display must exist while building UI");
    let css_provider = theme::install_css(&display);
    let gtk_settings = gtk::Settings::default().expect("GTK settings must exist for the display");
    theme::apply_theme_preference(
        &gtk_settings,
        &css_provider,
        settings::theme(&options.runtime_settings),
    );
    let layout_preference = settings::layout_preference(&options.runtime_settings);
    let initial_layout =
        layout_for_preference(layout_preference, 1500, 900, ContentLayout::ThreePane);

    let initial_state = UiState {
        current_query: options.default_query.clone(),
        thread_page_size: settings::page_size(&options.runtime_settings),
        automation_enabled: options.automation_enabled,
        database_path: options
            .database_path
            .as_ref()
            .map(|p| p.display().to_string()),
        prefer_html_view: options.html_mode == "visual_html_preferred",
        message_view_preferences: normalize_message_view_preferences(
            &options.message_view_preferences,
        ),
        sender_view_preferences: normalize_sender_view_preferences(
            &options.sender_view_preferences,
        ),
        theme: settings::theme(&options.runtime_settings),
        thread_preview_lines: settings::thread_preview_lines(&options.runtime_settings),
        show_thread_numbers: options.show_thread_numbers,
        show_thread_dates: options.show_thread_dates,
        show_thread_tags: options.show_thread_tags,
        show_thread_preview: options.show_thread_preview,
        show_keybind_hints: options.show_keybind_hints,
        layout_preference,
        content_layout: initial_layout,
        pending_open_message_id: options.open_message_id.clone(),
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
    let sync_refresh_generation = Rc::new(Cell::new(None));
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
    let theme_background_probe = gtk::Label::new(None);
    theme_background_probe.set_widget_name("notm-theme-background-probe");
    theme_background_probe.add_css_class("notm-theme-background-probe");
    theme_background_probe.set_visible(false);
    root.append(&theme_background_probe);
    let top_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top_bar.set_widget_name("notm-top-bar");
    top_bar.set_margin_start(8);
    top_bar.set_margin_end(8);
    top_bar.set_margin_top(8);
    top_bar.set_margin_bottom(8);
    let toolbar = button_flow(8);
    toolbar.set_hexpand(true);
    let view_controls = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    view_controls.set_widget_name("notm-pane-toggle-bar");
    view_controls.set_halign(gtk::Align::End);
    view_controls.set_valign(gtk::Align::Center);

    let compose_button = gtk::Button::with_label("Compose");
    compose_button.set_widget_name("notm-compose-button");
    let debug_button = gtk::Button::with_label("Debug");
    let palette_button = gtk::Button::with_label("Commands");
    let settings_button = gtk::Button::with_label("Settings");
    let help_button = gtk::Button::with_label("Help");
    let sidebar_toggle_button = pane_toggle_button(
        "sidebar-show-symbolic",
        "notm-sidebar-toggle-button",
        "Show/hide sidebar (Ctrl+1)",
    );
    let thread_list_toggle_button = pane_toggle_button(
        "view-list-symbolic",
        "notm-thread-list-toggle-button",
        "Show/hide message list (Ctrl+2)",
    );
    let message_pane_toggle_button = pane_toggle_button(
        "sidebar-show-right-symbolic",
        "notm-message-pane-toggle-button",
        "Show/hide message view (Ctrl+3)",
    );
    let layout_toggle_button = gtk::Button::with_label("Layout");
    layout_toggle_button.set_widget_name("notm-layout-toggle-button");
    layout_toggle_button
        .set_tooltip_text(Some("Cycle auto, columns, and stacked layouts (Ctrl+4)"));
    for b in [
        &compose_button,
        &debug_button,
        &palette_button,
        &settings_button,
        &help_button,
    ] {
        toolbar.insert(b, -1);
    }
    view_controls.append(&sidebar_toggle_button);
    view_controls.append(&thread_list_toggle_button);
    view_controls.append(&message_pane_toggle_button);
    view_controls.append(&layout_toggle_button);
    top_bar.append(&toolbar);
    top_bar.append(&view_controls);
    root.append(&top_bar);

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
    let (custom_search_button, custom_search_box) =
        menu_button_with_box("Save", "notm-custom-search-menu-button", &state);
    custom_search_button.set_tooltip_text(Some(
        "Save the current search as a sidebar shortcut (Ctrl+s).",
    ));
    custom_search_box.set_spacing(6);
    custom_search_box.set_margin_start(6);
    custom_search_box.set_margin_end(6);
    custom_search_box.set_margin_top(6);
    custom_search_box.set_margin_bottom(6);
    let saved_editor_title = gtk::Label::new(Some("Save current search"));
    saved_editor_title.set_xalign(0.0);
    saved_editor_title.add_css_class("dim-label");
    saved_editor_title.set_wrap(true);
    custom_search_box.append(&saved_editor_title);
    let saved_editor_help = gtk::Label::new(Some("Enter a name for the search bar query."));
    saved_editor_help.set_xalign(0.0);
    saved_editor_help.add_css_class("dim-label");
    saved_editor_help.set_wrap(true);
    custom_search_box.append(&saved_editor_help);
    let saved_name_entry = entry_with_placeholder("Name");
    saved_name_entry.set_widget_name("notm-saved-search-name");
    saved_name_entry.set_width_chars(18);
    let saved_query_entry = entry_with_placeholder("Query");
    saved_query_entry.set_widget_name("notm-saved-search-query");
    saved_query_entry.set_visible(false);
    custom_search_box.append(&saved_name_entry);
    let saved_editor_buttons = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let save_search_button = gtk::Button::with_label("Save search");
    save_search_button.set_widget_name("notm-save-search-button");
    saved_editor_buttons.append(&save_search_button);
    custom_search_box.append(&saved_editor_buttons);

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

    let search_bar = SearchBarController::new(&options.default_query, &custom_search_button);
    let search_button = search_bar.button();
    controls_box.append(&search_bar.root());

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
    let tag_choice_label = gtk::Label::new(Some("Choose tag action"));
    tag_choice_label.set_xalign(0.0);
    tag_choice_label.add_css_class("dim-label");
    tag_menu_box.append(&tag_choice_label);
    let single_tag_button = gtk::Button::with_label("Add/remove tag");
    single_tag_button.set_widget_name("notm-single-tag-mode-button");
    single_tag_button.set_hexpand(true);
    single_tag_button.set_halign(gtk::Align::Fill);
    let tag_command_button = gtk::Button::with_label("Tag multiple");
    tag_command_button.set_widget_name("notm-tag-command-mode-button");
    tag_command_button.set_hexpand(true);
    tag_command_button.set_halign(gtk::Align::Fill);
    tag_menu_box.append(&single_tag_button);
    tag_menu_box.append(&tag_command_button);
    let single_tag_editor_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    single_tag_editor_box.set_widget_name("notm-single-tag-editor");
    single_tag_editor_box.set_visible(false);
    let single_tag_action_label = gtk::Label::new(Some("Add/remove tag"));
    single_tag_action_label.set_xalign(0.0);
    single_tag_action_label.add_css_class("dim-label");
    let custom_tag_entry = entry_with_placeholder("tag");
    custom_tag_entry.set_widget_name("notm-custom-tag-entry");
    custom_tag_entry.set_width_chars(18);
    custom_tag_entry.set_hexpand(true);
    let single_tag_apply_button = gtk::Button::with_label("Apply");
    single_tag_apply_button.set_widget_name("notm-apply-single-tag-button");
    single_tag_apply_button.add_css_class("suggested-action");
    single_tag_apply_button.set_size_request(120, -1);
    let single_tag_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    single_tag_row.set_hexpand(true);
    single_tag_row.append(&custom_tag_entry);
    single_tag_row.append(&single_tag_apply_button);
    single_tag_editor_box.append(&single_tag_action_label);
    single_tag_editor_box.append(&single_tag_row);
    let tag_command_editor_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    tag_command_editor_box.set_widget_name("notm-tag-command-editor");
    tag_command_editor_box.set_visible(false);
    let multi_tag_label = gtk::Label::new(Some("Tag multiple"));
    multi_tag_label.set_xalign(0.0);
    multi_tag_label.add_css_class("dim-label");
    let multi_tag_help = gtk::Label::new(Some(
        "Syntax: +tag adds, -tag removes. Example: -inbox +books +flagged",
    ));
    multi_tag_help.set_xalign(0.0);
    multi_tag_help.set_wrap(true);
    multi_tag_help.add_css_class("dim-label");
    let tag_command_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tag_command_row.set_widget_name("notm-tag-command-row");
    tag_command_row.set_hexpand(true);
    let tag_command_entry = entry_with_placeholder("-inbox +books +flagged");
    tag_command_entry.set_widget_name("notm-tag-command-entry");
    tag_command_entry.set_hexpand(true);
    let tag_command_apply_button = gtk::Button::with_label("Apply");
    tag_command_apply_button.set_widget_name("notm-run-tag-command-button");
    tag_command_apply_button.add_css_class("suggested-action");
    tag_command_apply_button.set_size_request(120, -1);
    tag_command_row.append(&tag_command_entry);
    tag_command_row.append(&tag_command_apply_button);
    tag_command_editor_box.append(&multi_tag_label);
    tag_command_editor_box.append(&multi_tag_help);
    tag_command_editor_box.append(&tag_command_row);
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
    controls_box.append(&single_tag_editor_box);
    controls_box.append(&tag_command_editor_box);

    let row_state = state.clone();
    let row_provider = Rc::new(move |index| thread_row_snapshot(&row_state, index));
    let multi_state = state.clone();
    let multi_select =
        Rc::new(move |thread_id: &str| toggle_thread_multi_selection(&multi_state, thread_id));
    let thread_list = ThreadListController::new(row_provider, multi_select);
    middle.append(&thread_list.root());

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
    message_menu_button.set_tooltip_text(Some(
        "Choose a message in this thread. Use J/K for next/previous message.",
    ));
    let (message_tag_menu_button, message_tag_menu_box) =
        menu_button_with_box("Tag message", "notm-message-tag-menu-button", &state);
    message_tag_menu_button
        .set_tooltip_text(Some("Apply tags to the currently displayed message only."));
    message_tag_menu_box.set_spacing(4);
    message_tag_menu_box.set_margin_start(6);
    message_tag_menu_box.set_margin_end(6);
    message_tag_menu_box.set_margin_top(6);
    message_tag_menu_box.set_margin_bottom(6);
    let message_tag_scope_label = gtk::Label::new(Some("Current message only"));
    message_tag_scope_label.set_xalign(0.0);
    message_tag_scope_label.add_css_class("dim-label");
    message_tag_menu_box.append(&message_tag_scope_label);
    let message_archive_button = gtk::Button::with_label("Archive message");
    message_archive_button.set_widget_name("notm-message-archive-button");
    let message_read_toggle_button = gtk::Button::with_label("Mark message read");
    message_read_toggle_button.set_widget_name("notm-message-read-toggle-button");
    let message_flag_toggle_button = gtk::Button::with_label("Flag message");
    message_flag_toggle_button.set_widget_name("notm-message-flag-toggle-button");
    let message_trash_button = gtk::Button::with_label("Move message to trash");
    message_trash_button.set_widget_name("notm-message-trash-button");
    let message_spam_button = gtk::Button::with_label("Mark message as spam");
    message_spam_button.set_widget_name("notm-message-spam-button");
    for button in [
        &message_archive_button,
        &message_read_toggle_button,
        &message_flag_toggle_button,
        &message_trash_button,
        &message_spam_button,
    ] {
        message_tag_menu_box.append(button);
    }
    let message_custom_tag_action_label = gtk::Label::new(Some("Custom tag"));
    message_custom_tag_action_label.set_xalign(0.0);
    message_custom_tag_action_label.add_css_class("dim-label");
    message_tag_menu_box.append(&message_custom_tag_action_label);
    let message_custom_tag_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let message_custom_tag_entry = entry_with_placeholder("message tag");
    message_custom_tag_entry.set_widget_name("notm-message-custom-tag-entry");
    message_custom_tag_entry.set_hexpand(true);
    let message_custom_tag_apply_button = gtk::Button::with_label("Add tag");
    message_custom_tag_apply_button.set_widget_name("notm-message-custom-tag-apply-button");
    message_custom_tag_apply_button.add_css_class("suggested-action");
    message_custom_tag_row.append(&message_custom_tag_entry);
    message_custom_tag_row.append(&message_custom_tag_apply_button);
    message_tag_menu_box.append(&message_custom_tag_row);
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
    let view_preference_separator = gtk::Separator::new(gtk::Orientation::Horizontal);
    view_menu_box.append(&view_preference_separator);
    let sender_view_preference_button = gtk::Button::with_label("Always");
    sender_view_preference_button.set_widget_name("notm-sender-view-preference-button");
    sender_view_preference_button.set_visible(false);
    view_menu_box.append(&sender_view_preference_button);
    let active_message_view = Rc::new(Cell::new(MessageViewKind::Text));
    let pending_html_scroll_fraction = Rc::new(Cell::new(None));
    let image_policy_button = gtk::Button::with_label("Load remote images once");
    image_policy_button.set_widget_name("notm-image-policy-button");
    image_policy_button.set_tooltip_text(Some(
        "Remote images can reveal that you opened a message and expose your network address. This action applies only to the current message and resets when you leave it.",
    ));
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
    message_actions.append(&message_tag_menu_button);
    message_actions.append(&view_menu_button);
    message_actions.append(&collapse_quotes_button);
    message_actions.append(&copy_menu_button);
    right.append(&message_actions);

    let attachments = AttachmentController::new(&window, attachment_open_dir, options.fixture_mode);
    right.append(&attachments.title_widget());
    right.append(&attachments.scrolled_widget());

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

    let message_header_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    message_header_box.set_widget_name("notm-message-header");
    message_header_box.set_hexpand(true);
    message_header_box.set_visible(false);
    right.append(&message_header_box);

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
    let html_view = new_privacy_html_webview();
    html_view.set_widget_name("notm-html-view");
    html_view.set_hexpand(true);
    html_view.set_vexpand(true);
    let html_load_generation = Rc::new(Cell::new(0));
    let html_load_lifecycle = Rc::new(RefCell::new(HtmlLoadLifecycle::default()));
    let html_completed_load_generation = Rc::new(Cell::new(0));
    connect_html_load_completion(
        &html_view,
        &html_load_generation,
        &html_load_lifecycle,
        &html_completed_load_generation,
    );
    configure_html_webview(
        &html_view,
        settings::remote_images(&options.runtime_settings),
    );
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

    let draft_path = options
        .draft_path
        .clone()
        .unwrap_or_else(composer::default_recovery_path);
    let legacy_draft_path = options
        .draft_path
        .is_none()
        .then(composer::legacy_default_recovery_path)
        .filter(|legacy_path| legacy_path != &draft_path);
    let drafts_dir = options
        .drafts_dir
        .clone()
        .unwrap_or_else(composer::default_drafts_dir);
    let legacy_drafts_dir = options
        .drafts_dir
        .is_none()
        .then(composer::legacy_default_drafts_dir)
        .filter(|legacy_dir| legacy_dir != &drafts_dir);
    let composer = ComposerController::new(ComposerPaths {
        recovery: draft_path,
        legacy_recovery: legacy_draft_path,
        drafts: drafts_dir,
        legacy_drafts: legacy_drafts_dir,
    });
    message_stack.add_named(&composer.root(), Some("compose"));

    let debug_view = gtk::TextView::new();
    debug_view.set_widget_name("notm-debug-panel");
    debug_view.set_editable(false);
    debug_view.set_monospace(true);
    debug_view.set_visible(options.show_debug_panel);
    debug_view.set_size_request(-1, 150);
    right.append(&debug_view);

    let content_paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    content_paned.set_widget_name("notm-content-paned");
    content_paned.set_wide_handle(true);
    content_paned.set_position(560);
    content_paned.set_hexpand(true);
    content_paned.set_vexpand(true);

    let outer_paned = gtk::Paned::new(gtk::Orientation::Horizontal);
    outer_paned.set_widget_name("notm-outer-paned");
    outer_paned.set_wide_handle(true);
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
    configure_status_label(&status_label);
    status_label.set_margin_start(8);
    status_label.set_margin_end(8);
    status_label.set_margin_bottom(8);
    root.append(&status_label);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&root));
    window.set_child(Some(&overlay));
    connect_html_navigation_policy(&html_view, &status_label);
    connect_html_hover_status(&html_view, &status_label);
    connect_html_scroll_restore(
        &html_view,
        &status_label,
        &pending_html_scroll_fraction,
        &html_load_generation,
        &html_completed_load_generation,
    );
    let link_opener: LinkHintOpener = Rc::new(open_html_link_externally);
    let link_hints = LinkHintController::new(&html_view, &status_label, link_opener);

    let widgets = Widgets {
        window: window.clone(),
        gtk_settings,
        css_provider,
        theme_background_probe,
        settings: SettingsController::new(),
        overlay: overlay.clone(),
        outer_paned: outer_paned.clone(),
        content_paned: content_paned.clone(),
        left_pane: sidebar_scrolled.clone(),
        thread_pane: middle.clone(),
        message_pane: right.clone(),
        saved_box,
        custom_search_menu_button: custom_search_button.clone(),
        saved_name_entry,
        saved_query_entry,
        save_search_button: save_search_button.clone(),
        custom_tag_entry,
        search_bar,
        sync_refresh_generation,
        input_mode_generation: Rc::new(Cell::new(0)),
        hidden_tag_searches,
        thread_list,
        manual_sync_button: manual_sync_button.clone(),
        compose_button: compose_button.clone(),
        debug_button: debug_button.clone(),
        palette_button: palette_button.clone(),
        settings_button: settings_button.clone(),
        help_button: help_button.clone(),
        sidebar_toggle_button: sidebar_toggle_button.clone(),
        thread_list_toggle_button: thread_list_toggle_button.clone(),
        message_pane_toggle_button: message_pane_toggle_button.clone(),
        layout_toggle_button: layout_toggle_button.clone(),
        archive_button: archive_button.clone(),
        read_toggle_button: read_button.clone(),
        flag_toggle_button: flag_button.clone(),
        trash_button: trash_button.clone(),
        spam_button: spam_button.clone(),
        tag_command_entry: tag_command_entry.clone(),
        tag_command_button: tag_command_button.clone(),
        tag_command_apply_button: tag_command_apply_button.clone(),
        tag_menu_button: tag_menu_button.clone(),
        tag_menu_box: tag_menu_box.clone(),
        single_tag_button: single_tag_button.clone(),
        single_tag_editor_box: single_tag_editor_box.clone(),
        single_tag_action_label: single_tag_action_label.clone(),
        single_tag_apply_button: single_tag_apply_button.clone(),
        tag_command_editor_box: tag_command_editor_box.clone(),
        undo_tag_button: undo_button.clone(),
        undo_menu_box: undo_menu_box.clone(),
        undo_last_tag_button: undo_last_button.clone(),
        undo_list_tag_button: undo_list_button.clone(),
        message_stack,
        message_view,
        message_scrolled: scrolled_message.clone(),
        html_view,
        html_load_generation,
        html_completed_load_generation,
        html_scrolled: scrolled_html.clone(),
        link_hints,
        response_menu_button,
        reply_button: reply_button.clone(),
        reply_all_button: reply_all_button.clone(),
        forward_button: forward_button.clone(),
        forward_attachment_button: forward_attachment_button.clone(),
        response_menu_box: response_menu_box.clone(),
        message_menu_button,
        message_menu_box,
        message_tag_menu_button,
        message_tag_menu_box,
        message_archive_button,
        message_read_toggle_button,
        message_flag_toggle_button,
        message_trash_button,
        message_spam_button,
        message_custom_tag_entry,
        message_custom_tag_action_label,
        message_custom_tag_apply_button,
        view_menu_button,
        view_menu_box: view_menu_box.clone(),
        view_text_button,
        view_html_button,
        view_headers_button,
        view_raw_button,
        sender_view_preference_button,
        active_message_view,
        pending_html_scroll_fraction,
        image_policy_button,
        html_policy_row,
        html_policy_label,
        message_header_box,
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
        attachments,
        tag_search_box,
        debug_view,
        status_label,
        composer,
        close_when_idle: Rc::new(Cell::new(false)),
        standalone_messages: StandaloneMessageController::new(),
    };
    debug_assert!(
        widgets.attachments.open_dir().is_dir(),
        "application attachment-open directory must exist while the UI is running"
    );
    apply_content_layout(&widgets, &state, initial_layout, false);
    apply_initial_pane_visibility(&options, &widgets, &state);
    update_active_pane_visuals(&widgets, &state);
    update_message_action_buttons(&options, &widgets, &state);
    set_undo_tag_available(&widgets, !undo_state.borrow().is_empty());
    if let Some(id) = identity(&options) {
        widgets.composer.sender_entry().set_text(&id.formatted());
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
    connect_custom_tag_editor(&options, &widgets, &state, &undo_state, &single_tag_button);
    connect_notmuch_tag_command_editor(&options, &widgets, &state, &undo_state);
    if let Some(sync_button) = widgets.manual_sync_button.clone() {
        let opts = options.clone();
        let w = widgets.clone();
        let st = state.clone();
        sync_button.connect_clicked(move |_| {
            let _ = run_manual_sync(&opts, &w, &st, Duration::ZERO);
        });
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
        &widgets.composer.send_button(),
    );
    connect_pane_visibility_toggles(&options, &widgets, &state);
    connect_auto_layout(&widgets, &state);
    connect_compose_helpers(
        &options,
        &widgets,
        &state,
        &widgets.composer.add_attachment_button(),
        &widgets.composer.save_draft_button(),
        &widgets.composer.clear_draft_button(),
        &widgets.composer.delete_local_draft_button(),
    );
    connect_draft_list(&options, &widgets, &state);
    connect_compose_vim_context(&options, &widgets, &state);
    connect_message_actions(&options, &widgets, &state, &undo_state);
    connect_recipient_autocomplete(&widgets.composer.to_entry(), &widgets, &state);
    connect_recipient_autocomplete(&widgets.composer.cc_entry(), &widgets, &state);
    connect_recipient_autocomplete(&widgets.composer.bcc_entry(), &widgets, &state);
    connect_address_suggestion_list(&widgets, &state);
    connect_search_bar(&options, &widgets, &state);
    connect_input_mode_focus(&widgets, &state);
    let shortcut_router =
        install_shortcuts(&options, &widgets, &state, &undo_state, &saved_search_store);
    connect_auto_load_more(&options, &widgets, &state);
    {
        let opts = options.clone();
        let w = widgets.clone();
        let st = state.clone();
        window.connect_close_request(move |window| {
            if w.composer.take_allow_close_once() {
                return gtk::glib::Propagation::Proceed;
            }
            if w.composer.has_pending_confirmation() {
                return gtk::glib::Propagation::Stop;
            }
            let background_activity = {
                let state = st.borrow();
                state.send_in_progress || state.sync_in_progress
            };
            if background_activity {
                w.close_when_idle.set(true);
                window.set_visible(false);
                gtk::glib::Propagation::Stop
            } else {
                let fields = compose_fields(&w, &st);
                let active_draft = st.borrow().active_draft.clone();
                if !composer_requires_confirmation(&fields, active_draft.as_ref()) {
                    return gtk::glib::Propagation::Proceed;
                }
                let _ = request_pending_action(&opts, &w, &st, PendingTransition::CloseMainWindow);
                gtk::glib::Propagation::Stop
            }
        });
    }

    if options.automation_enabled {
        setup_automation(
            &options,
            &widgets,
            &state,
            &undo_state,
            &saved_search_store,
            &shortcut_router,
        );
    }

    let recovered_draft = restore_draft_if_present(&options, &widgets, &state);
    let preserve_recovered_composer = recovered_draft
        && composer_requires_confirmation(
            &compose_fields(&widgets, &state),
            state.borrow().active_draft.as_ref(),
        );
    migrate_legacy_named_drafts_from_ui(&widgets, &state);
    widgets.composer.refresh_draft_list();
    window.present();
    let initial_mailto_opened = options
        .mailto_uri
        .as_deref()
        .is_some_and(|uri| open_mailto_uri_request(&options, &widgets, &state, uri));
    let preserve_startup_composer = preserve_recovered_composer || initial_mailto_opened;
    {
        let w = widgets.clone();
        let st = state.clone();
        gtk::glib::idle_add_local_once(move || {
            apply_auto_layout_for_current_size(&w, &st);
            sync_pane_button_classes(&w, &st);
        });
    }
    if !initial_mailto_opened {
        widgets
            .status_label
            .set_text("Starting notm; loading mail…");
    }
    widgets
        .thread_list
        .set_result_label("Loading initial search…");
    show_thread_list_loading(&widgets, "Loading initial search…");
    {
        let opts = options.clone();
        let w = widgets.clone();
        let st = state.clone();
        let query = options.default_query.clone();
        gtk::glib::timeout_add_local_once(Duration::from_millis(0), move || {
            schedule_search(
                &opts,
                &w,
                &st,
                &query,
                !preserve_startup_composer,
                Duration::ZERO,
            );
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
    MainWindowHandle {
        window,
        widgets,
        state,
    }
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
    widgets.search_bar.set_query(query);
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
    if let Some(popover) = widgets.custom_search_menu_button.popover() {
        let w = widgets.clone();
        let st = state.clone();
        let store = saved_store.clone();
        popover.connect_show(move |_| {
            w.saved_name_entry.set_text("");
            w.saved_query_entry
                .set_text(w.search_bar.entry().text().trim());
            update_saved_search_editor_actions(&w, &st, &store);
            set_input_mode(
                &w,
                &st,
                InputMode::Insert,
                "Save current search: enter a name",
            );
            w.saved_name_entry.grab_focus();
        });
    }

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let store = saved_store.clone();
    save_search_button.connect_clicked(move |_| {
        match save_custom_search_from_current_query(&opts, &w, &st, &store) {
            Ok(()) => {
                w.custom_search_menu_button.popdown();
                w.status_label.set_text("Saved custom search");
                enter_normal_mode(&w, &st);
            }
            Err(err) => w
                .status_label
                .set_text(&format!("Save search failed: {err}")),
        }
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let store = saved_store.clone();
    widgets.saved_name_entry.connect_activate(
        move |_| match save_custom_search_from_current_query(&opts, &w, &st, &store) {
            Ok(()) => {
                w.custom_search_menu_button.popdown();
                w.status_label.set_text("Saved custom search");
                enter_normal_mode(&w, &st);
            }
            Err(err) => w
                .status_label
                .set_text(&format!("Save search failed: {err}")),
        },
    );

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
    let query = widgets.search_bar.entry().text().trim().to_string();
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

fn save_custom_search_from_current_query(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    saved_store: &SavedSearchStore,
) -> anyhow::Result<()> {
    let query = widgets.search_bar.entry().text().trim().to_string();
    anyhow::ensure!(!query.is_empty(), "search query is empty");
    widgets.saved_query_entry.set_text(&query);
    save_custom_search_from_entries(options, widgets, state, saved_store)
}

fn open_save_current_search_prompt(
    widgets: &Widgets,
    state: &SharedState,
    saved_store: &SavedSearchStore,
) {
    let query = widgets.search_bar.entry().text().trim().to_string();
    if query.is_empty() {
        widgets.status_label.set_text("Search query is empty");
        return;
    }
    widgets.saved_name_entry.set_text("");
    widgets.saved_query_entry.set_text(&query);
    update_saved_search_editor_actions(widgets, state, saved_store);
    set_input_mode(
        widgets,
        state,
        InputMode::Insert,
        "Save current search: enter a name",
    );
    widgets.custom_search_menu_button.popup();
    let entry = widgets.saved_name_entry.clone();
    gtk::glib::idle_add_local_once(move || {
        entry.grab_focus();
    });
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
    widgets.search_bar.set_query(&query);
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
    settings::persist_ui_value(
        options.app_config_path.as_deref(),
        "custom_saved_searches",
        toml::Value::try_from(searches)?,
    )
}

fn persist_hidden_tag_searches(
    options: &LaunchOptions,
    hidden_tags: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let hidden = hidden_tags
        .iter()
        .cloned()
        .map(toml::Value::String)
        .collect::<Vec<_>>();
    settings::persist_ui_value(
        options.app_config_path.as_deref(),
        "hidden_tag_searches",
        toml::Value::Array(hidden),
    )
}

fn persist_message_view_preferences(
    options: &LaunchOptions,
    preferences: &BTreeMap<String, MessageViewPreference>,
) -> anyhow::Result<()> {
    settings::persist_ui_value(
        options.app_config_path.as_deref(),
        "message_view_preferences",
        toml::Value::try_from(preferences)?,
    )
}

fn persist_sender_view_preferences(
    options: &LaunchOptions,
    preferences: &BTreeMap<String, MessageViewPreference>,
) -> anyhow::Result<()> {
    settings::persist_ui_value(
        options.app_config_path.as_deref(),
        "sender_view_preferences",
        toml::Value::try_from(preferences)?,
    )
}

fn remember_message_view_preference(
    options: &LaunchOptions,
    state: &SharedState,
    message_id: &str,
    preference: MessageViewPreference,
) -> anyhow::Result<()> {
    let message_id = normalize_message_id(message_id);
    anyhow::ensure!(!message_id.is_empty(), "message id is empty");
    let previous = state
        .borrow_mut()
        .message_view_preferences
        .insert(message_id.clone(), preference);
    let snapshot = state.borrow().message_view_preferences.clone();
    if let Err(error) = persist_message_view_preferences(options, &snapshot) {
        let mut state = state.borrow_mut();
        match previous {
            Some(previous) => {
                state.message_view_preferences.insert(message_id, previous);
            }
            None => {
                state.message_view_preferences.remove(&message_id);
            }
        }
        return Err(error);
    }
    Ok(())
}

fn toggle_sender_view_preference(
    options: &LaunchOptions,
    state: &SharedState,
    sender: &str,
    preference: MessageViewPreference,
) -> anyhow::Result<bool> {
    let sender = normalize_sender(sender);
    anyhow::ensure!(!sender.is_empty(), "sender is empty");
    let previous = {
        let mut state = state.borrow_mut();
        let previous = state.sender_view_preferences.get(&sender).copied();
        if previous == Some(preference) {
            state.sender_view_preferences.remove(&sender);
        } else {
            state
                .sender_view_preferences
                .insert(sender.clone(), preference);
        }
        previous
    };
    let enabled = previous != Some(preference);
    let snapshot = state.borrow().sender_view_preferences.clone();
    if let Err(error) = persist_sender_view_preferences(options, &snapshot) {
        let mut state = state.borrow_mut();
        match previous {
            Some(previous) => {
                state.sender_view_preferences.insert(sender, previous);
            }
            None => {
                state.sender_view_preferences.remove(&sender);
            }
        }
        return Err(error);
    }
    Ok(enabled)
}

fn connect_custom_tag_editor(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    single_tag_button: &gtk::Button,
) {
    let w = widgets.clone();
    let st = state.clone();
    single_tag_button.connect_clicked(move |_| {
        open_custom_tag_editor(&w, &st);
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    widgets.single_tag_apply_button.connect_clicked(move |_| {
        let add = !custom_tag_can_remove(&w, &st);
        if apply_custom_tag_from_entry(&opts, &w, &st, &undo, add) {
            prepare_custom_tag_entry_for_next(&w, &st);
        }
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    widgets.custom_tag_entry.connect_activate(move |_| {
        let add = !custom_tag_can_remove(&w, &st);
        if apply_custom_tag_from_entry(&opts, &w, &st, &undo, add) {
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

    widgets.custom_tag_entry.connect_map(|entry| {
        entry.grab_focus();
        entry.select_region(0, -1);
    });

    if let Some(popover) = widgets.tag_menu_button.popover() {
        let w = widgets.clone();
        let st = state.clone();
        popover.connect_closed(move |_| {
            if st.borrow().input_mode != InputMode::Insert {
                return;
            }
            if w.single_tag_editor_box.is_visible() {
                w.custom_tag_entry.grab_focus();
                w.custom_tag_entry.select_region(0, -1);
            } else if w.tag_command_editor_box.is_visible() {
                w.tag_command_entry.grab_focus();
                w.tag_command_entry.select_region(0, -1);
            }
        });
    }

    update_custom_tag_controls(widgets, state);
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
            sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                &options.runtime_settings,
            ),
        }
    } else {
        TagMutation {
            add: Vec::new(),
            remove: vec![tag],
            sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                &options.runtime_settings,
            ),
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
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
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
    let w = widgets.clone();
    let st = state.clone();
    widgets.tag_command_button.connect_clicked(move |_| {
        open_notmuch_tag_command_editor(&w, &st);
    });

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
    widgets.tag_command_entry.connect_map(|entry| {
        entry.grab_focus();
        entry.select_region(0, -1);
    });
}

fn open_notmuch_tag_command_editor(widgets: &Widgets, state: &SharedState) {
    widgets.single_tag_editor_box.set_visible(false);
    widgets.tag_menu_button.popdown();
    set_input_mode(
        widgets,
        state,
        InputMode::Insert,
        "Insert mode: tag multiple (+tag/-tag, Esc for normal)",
    );
    widgets.tag_command_editor_box.set_visible(true);
    widgets.tag_command_entry.grab_focus();
    widgets.tag_command_entry.select_region(0, -1);
}

fn show_tag_sequence_menu(widgets: &Widgets) {
    close_tag_editors(widgets);
    widgets.tag_menu_button.popup();
}

fn handle_tag_sequence_key(widgets: &Widgets, state: &SharedState, key: gtk::gdk::Key) -> bool {
    match tag_sequence_key_action(key) {
        Some(TagSequenceKeyAction::SingleTag) => {
            open_custom_tag_editor(widgets, state);
            true
        }
        Some(TagSequenceKeyAction::TagCommand) => {
            open_notmuch_tag_command_editor(widgets, state);
            true
        }
        None => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagSequenceKeyAction {
    SingleTag,
    TagCommand,
}

fn tag_sequence_key_action(key: gtk::gdk::Key) -> Option<TagSequenceKeyAction> {
    if key == gtk::gdk::Key::t {
        Some(TagSequenceKeyAction::SingleTag)
    } else if key == gtk::gdk::Key::m {
        Some(TagSequenceKeyAction::TagCommand)
    } else {
        None
    }
}

fn is_tag_sequence_prefix(key: gtk::gdk::Key, mods: gtk::gdk::ModifierType) -> bool {
    key == gtk::gdk::Key::T
        || (key == gtk::gdk::Key::t && mods.contains(gtk::gdk::ModifierType::SHIFT_MASK))
}

fn is_tag_menu_navigation_key(key: gtk::gdk::Key) -> bool {
    matches!(
        key,
        gtk::gdk::Key::j
            | gtk::gdk::Key::k
            | gtk::gdk::Key::Up
            | gtk::gdk::Key::Down
            | gtk::gdk::Key::Left
            | gtk::gdk::Key::Right
            | gtk::gdk::Key::Home
            | gtk::gdk::Key::End
            | gtk::gdk::Key::Tab
            | gtk::gdk::Key::ISO_Left_Tab
            | gtk::gdk::Key::Return
            | gtk::gdk::Key::KP_Enter
            | gtk::gdk::Key::space
    )
}

fn close_notmuch_tag_command_editor(widgets: &Widgets, state: &SharedState) {
    close_tag_editors(widgets);
    enter_normal_mode(widgets, state);
}

fn close_tag_editors(widgets: &Widgets) {
    widgets.single_tag_editor_box.set_visible(false);
    widgets.tag_command_editor_box.set_visible(false);
}

fn update_custom_tag_controls(widgets: &Widgets, state: &SharedState) {
    let has_tag = !widgets.custom_tag_entry.text().trim().is_empty();
    let can_remove = custom_tag_can_remove(widgets, state);
    let background_activity = {
        let state = state.borrow();
        state.sync_in_progress || state.send_in_progress
    };
    widgets.single_tag_action_label.set_text(if !has_tag {
        "Add/remove tag: type a tag"
    } else if can_remove {
        "Add/remove tag: this will remove an existing tag"
    } else {
        "Add/remove tag: this will add a tag"
    });
    widgets.custom_tag_entry.set_placeholder_text(Some("tag"));
    widgets
        .single_tag_apply_button
        .set_label(if can_remove { "Remove tag" } else { "Add tag" });
    widgets
        .single_tag_apply_button
        .set_sensitive(has_tag && !background_activity);
}

fn custom_tag_can_remove(widgets: &Widgets, state: &SharedState) -> bool {
    let tag = widgets.custom_tag_entry.text().trim().to_string();
    !tag.is_empty()
        && tag_targets_any(state, |thread| {
            thread.tags.iter().any(|existing| existing == &tag)
        })
}

fn open_custom_tag_editor(widgets: &Widgets, state: &SharedState) {
    widgets.tag_command_editor_box.set_visible(false);
    widgets.tag_menu_button.popdown();
    update_custom_tag_controls(widgets, state);
    set_input_mode(
        widgets,
        state,
        InputMode::Insert,
        "Insert mode: single tag (Esc for normal)",
    );
    widgets.single_tag_editor_box.set_visible(true);
    widgets.custom_tag_entry.grab_focus();
    widgets.custom_tag_entry.select_region(0, -1);
}

fn prepare_custom_tag_entry_for_next(widgets: &Widgets, state: &SharedState) {
    widgets.tag_command_editor_box.set_visible(false);
    widgets.single_tag_editor_box.set_visible(true);
    update_custom_tag_controls(widgets, state);
    set_input_mode(
        widgets,
        state,
        InputMode::Insert,
        "Tag applied; type another tag or Esc for normal",
    );
    widgets.custom_tag_entry.grab_focus();
    widgets.custom_tag_entry.select_region(0, -1);
}

fn visual_selection_range_from_state(state: &UiState) -> Option<(usize, usize)> {
    if !state.visual_select_mode {
        return None;
    }
    let anchor = state.visual_select_anchor?;
    let cursor = state.visual_select_cursor.unwrap_or(anchor);
    Some((anchor.min(cursor), anchor.max(cursor)))
}

fn tag_target_thread_ids(state: &SharedState) -> BTreeSet<String> {
    let state = state.borrow();
    if let Some((start, end)) = visual_selection_range_from_state(&state) {
        state
            .thread_list_items
            .iter()
            .enumerate()
            .filter(|(index, _)| (start..=end).contains(&(state.thread_window_offset + *index)))
            .map(|(_, thread)| thread.thread_id.clone())
            .collect()
    } else if !state.multi_selected_threads.is_empty() {
        state.multi_selected_threads.clone()
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
    let target_ids: BTreeSet<String> =
        if let Some((start, end)) = visual_selection_range_from_state(&state) {
            state
                .thread_list_items
                .iter()
                .enumerate()
                .filter(|(index, _)| (start..=end).contains(&(state.thread_window_offset + *index)))
                .map(|(_, thread)| thread.thread_id.clone())
                .collect()
        } else if !state.multi_selected_threads.is_empty() {
            state.multi_selected_threads.clone()
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

#[cfg(test)]
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
    let was_active = tag_editor_insert_mode_active(widgets, state);
    if let Some(popover) = widgets.tag_menu_button.popover() {
        popover.popdown();
    }
    close_tag_editors(widgets);
    if was_active {
        enter_normal_mode(widgets, state);
    }
}

fn tag_editor_insert_mode_active(widgets: &Widgets, state: &SharedState) -> bool {
    state.borrow().input_mode == InputMode::Insert
        && (widgets.single_tag_editor_box.is_visible()
            || widgets.tag_command_editor_box.is_visible())
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

fn connect_compose_vim_context(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let status = widgets.status_label.clone();
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets.composer.connect_vim(
        Rc::new(move |text| status.set_text(&text)),
        Rc::new(
            move |path| match request_save_current_draft(&opts, &w, &st) {
                Ok(Some(report)) => {
                    w.composer.refresh_draft_list();
                    let destination = report
                        .maildir_path
                        .as_ref()
                        .or(report.local_path.as_ref())
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "draft store".to_string());
                    let suffix = path
                        .map(|requested| format!("; ignored Vim file path {requested}"))
                        .unwrap_or_default();
                    let warning = report
                        .recovery_cleanup_warning
                        .as_ref()
                        .map(|warning| format!("; recovery cleanup failed: {warning}"))
                        .unwrap_or_default();
                    w.status_label.set_text(&format!(
                        "Vim :w saved draft to {destination}{suffix}{warning}"
                    ));
                }
                Ok(None) => {}
                Err(err) => w.status_label.set_text(&format!("Vim :w failed: {err}")),
            },
        ),
    );
}

#[allow(clippy::too_many_arguments)]
// These existing dialog paths still use the pre-GTK-4.10 chooser/dialog API.
// Keep the former file-wide compatibility allowance scoped to those paths.
#[allow(deprecated)]
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
        widgets.composer.sender_entry().clone(),
        widgets.composer.to_entry().clone(),
        widgets.composer.cc_entry().clone(),
        widgets.composer.bcc_entry().clone(),
        widgets.composer.subject_entry().clone(),
    ] {
        let w = widgets.clone();
        let st = state.clone();
        entry.connect_changed(move |_| autosave_draft_from_widgets(&w, &st));
    }
    let w = widgets.clone();
    let st = state.clone();
    widgets
        .composer
        .body()
        .buffer()
        .connect_changed(move |_| autosave_draft_from_widgets(&w, &st));

    let w = widgets.clone();
    let st = state.clone();
    let opts = options.clone();
    save_draft_button.connect_clicked(move |_| {
        save_current_draft_from_ui(&opts, &w, &st);
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    clear_draft_button.connect_clicked(move |_| {
        let _ = clear_current_draft_from_ui(&opts, &w, &st);
    });

    let w = widgets.clone();
    let st = state.clone();
    let opts = options.clone();
    delete_local_draft_button.connect_clicked(move |_| {
        delete_active_draft_from_ui(&opts, &w, &st);
    });

    let w = widgets.clone();
    let st = state.clone();
    add_attachment_button.connect_clicked(move |_| show_add_attachment_dialog(&w, &st));
}

fn save_current_draft_from_ui(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    match request_save_current_draft(options, widgets, state) {
        Ok(Some(_)) => {}
        Ok(None) => {}
        Err(err) => widgets
            .status_label
            .set_text(&format!("Draft save failed: {err}")),
    }
    widgets.composer.refresh_draft_list();
}

#[allow(deprecated)]
fn show_add_attachment_dialog(widgets: &Widgets, state: &SharedState) {
    let dialog = gtk::FileChooserNative::new(
        Some("Add attachment"),
        Some(&widgets.window),
        gtk::FileChooserAction::Open,
        Some("Attach"),
        Some("Cancel"),
    );
    let widgets = widgets.clone();
    let state = state.clone();
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept
            && let Some(file) = dialog.file()
            && let Some(path) = file.path()
        {
            add_attachment_path(&widgets, &state, path);
        }
        dialog.destroy();
    });
    dialog.show();
}

fn connect_draft_list(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let w = widgets.clone();
    let st = state.clone();
    widgets
        .composer
        .draft_list()
        .connect_row_selected(move |_, _| {
            update_draft_action_buttons(&w, &st);
        });

    let w = widgets.clone();
    let st = state.clone();
    let opts = options.clone();
    widgets
        .composer
        .draft_list()
        .connect_row_activated(move |list, row| {
            list.select_row(Some(row));
            match load_selected_named_draft(&opts, &w, &st) {
                Ok((true, _)) => {}
                Ok((false, _)) => {}
                Err(err) => {
                    report_draft_persistence_error(&w, &st, "Saved draft load failed", &err)
                }
            }
        });

    let w = widgets.clone();
    let st = state.clone();
    let opts = options.clone();
    widgets
        .composer
        .delete_selected_draft_button()
        .connect_clicked(move |_| {
            delete_selected_named_draft_from_ui(&opts, &w, &st);
        });
}

fn connect_message_actions(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) {
    // View-mode actions retain composer fields, active-draft identity, and recovery bytes while
    // remembering the selected message's view. Transitions that detach the composer from a
    // selected message are routed through `request_show_selected_message` instead.
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets.view_text_button.connect_clicked(move |_| {
        choose_selected_message_view(&opts, &w, &st, MessageViewKind::Text);
        w.view_menu_button.popdown();
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets.view_html_button.connect_clicked(move |_| {
        choose_selected_message_view(&opts, &w, &st, MessageViewKind::Html);
        w.view_menu_button.popdown();
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets.view_headers_button.connect_clicked(move |_| {
        choose_selected_message_view(&opts, &w, &st, MessageViewKind::Headers);
        w.view_menu_button.popdown();
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets.view_raw_button.connect_clicked(move |_| {
        choose_selected_message_view(&opts, &w, &st, MessageViewKind::Raw);
        w.view_menu_button.popdown();
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets
        .sender_view_preference_button
        .connect_clicked(move |_| {
            activate_sender_view_preference(&opts, &w, &st);
        });

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
    widgets.copy_message_id_button.connect_clicked(move |_| {
        copy_selected_message_id(&w, &st);
        w.copy_menu_button.popdown();
    });

    let w = widgets.clone();
    let st = state.clone();
    widgets.copy_thread_id_button.connect_clicked(move |_| {
        copy_selected_thread_id(&w, &st);
        w.copy_menu_button.popdown();
    });

    let w = widgets.clone();
    let st = state.clone();
    widgets.copy_from_email_button.connect_clicked(move |_| {
        copy_selected_message_emails(&w, &st, MessageEmailField::From);
        w.copy_menu_button.popdown();
    });

    let w = widgets.clone();
    let st = state.clone();
    widgets.copy_to_email_button.connect_clicked(move |_| {
        copy_selected_message_emails(&w, &st, MessageEmailField::To);
        w.copy_menu_button.popdown();
    });

    let w = widgets.clone();
    let st = state.clone();
    widgets.copy_cc_email_button.connect_clicked(move |_| {
        copy_selected_message_emails(&w, &st, MessageEmailField::Cc);
        w.copy_menu_button.popdown();
    });

    let w = widgets.clone();
    let st = state.clone();
    widgets.copy_subject_button.connect_clicked(move |_| {
        copy_selected_message_subject(&w, &st);
        w.copy_menu_button.popdown();
    });

    connect_message_tag_button(
        &widgets.message_archive_button,
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
    widgets
        .message_read_toggle_button
        .connect_clicked(move |_| {
            toggle_selected_message_tag(&opts, &w, &st, &undo, "unread");
            w.message_tag_menu_button.popdown();
        });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    widgets
        .message_flag_toggle_button
        .connect_clicked(move |_| {
            toggle_selected_message_tag(&opts, &w, &st, &undo, "flagged");
            w.message_tag_menu_button.popdown();
        });

    connect_message_tag_button(
        &widgets.message_trash_button,
        options,
        widgets,
        state,
        undo_state,
        &["trash"],
        &["inbox", "spam"],
    );
    connect_message_tag_button(
        &widgets.message_spam_button,
        options,
        widgets,
        state,
        undo_state,
        &["spam"],
        &["inbox", "trash"],
    );

    let w = widgets.clone();
    let st = state.clone();
    widgets
        .message_custom_tag_entry
        .connect_changed(move |_| update_message_tag_controls(&w, &st));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    widgets
        .message_custom_tag_apply_button
        .connect_clicked(move |_| {
            apply_custom_tag_to_selected_message(&opts, &w, &st, &undo);
        });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    widgets.message_custom_tag_entry.connect_activate(move |_| {
        apply_custom_tag_to_selected_message(&opts, &w, &st, &undo);
    });
}

fn activate_sender_view_preference(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) {
    let Some(sender) = selected_sender_email(state) else {
        widgets
            .status_label
            .set_text("The selected message sender could not be parsed");
        widgets.view_menu_button.popdown();
        return;
    };
    let preference = widgets.active_message_view.get().preference();
    match toggle_sender_view_preference(options, state, &sender, preference) {
        Ok(true) => {
            widgets.status_label.set_text(&format!(
                "Messages from {sender} will default to {}",
                preference.label()
            ));
            state.borrow_mut().last_error = None;
        }
        Ok(false) => {
            widgets.status_label.set_text(&format!(
                "Removed the {} default for messages from {sender}",
                preference.label()
            ));
            state.borrow_mut().last_error = None;
        }
        Err(error) => {
            widgets.status_label.set_text(&format!(
                "Sender view preference could not be saved: {error}"
            ));
            state.borrow_mut().last_error = Some(error.to_string());
        }
    }
    update_sender_view_preference_button(widgets, state);
    update_debug(widgets, state);
    widgets.view_menu_button.popdown();
}

fn activate_message_view_sequence_key(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    key: gtk::gdk::Key,
) -> bool {
    if key == gtk::gdk::Key::t {
        choose_selected_message_view(options, widgets, state, MessageViewKind::Text);
    } else if key == gtk::gdk::Key::v {
        choose_selected_message_view(options, widgets, state, MessageViewKind::Html);
    } else if key == gtk::gdk::Key::h {
        choose_selected_message_view(options, widgets, state, MessageViewKind::Headers);
    } else if key == gtk::gdk::Key::r {
        choose_selected_message_view(options, widgets, state, MessageViewKind::Raw);
    } else if key == gtk::gdk::Key::a {
        activate_sender_view_preference(options, widgets, state);
    } else {
        return false;
    }
    true
}

fn connect_message_tag_button(
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
    let add = add.iter().map(|tag| (*tag).to_string()).collect::<Vec<_>>();
    let remove = remove
        .iter()
        .map(|tag| (*tag).to_string())
        .collect::<Vec<_>>();
    button.connect_clicked(move |_| {
        tag_selected_message(
            &opts,
            &w,
            &st,
            &undo,
            TagMutation {
                add: add.clone(),
                remove: remove.clone(),
                sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                    &opts.runtime_settings,
                ),
            },
        );
        w.message_tag_menu_button.popdown();
    });
}

fn connect_recipient_autocomplete(entry: &gtk::Entry, widgets: &Widgets, state: &SharedState) {
    let suggestions_state = state.clone();
    let suggestions = Rc::new(move || suggestions_state.borrow().address_suggestions.clone());
    let w = widgets.clone();
    let st = state.clone();
    let edited = Rc::new(move || autosave_draft_from_widgets(&w, &st));
    widgets
        .composer
        .connect_recipient_autocomplete(entry, suggestions, edited);
}

fn connect_address_suggestion_list(widgets: &Widgets, state: &SharedState) {
    let w = widgets.clone();
    let st = state.clone();
    widgets
        .composer
        .connect_address_suggestion_list(Rc::new(move || autosave_draft_from_widgets(&w, &st)));
}

fn connect_search_bar(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let handler = Rc::new(move |event| match event {
        SearchInputEvent::Cleared => {
            if st.borrow().sync_in_progress
                && w.sync_refresh_generation.get() == Some(st.borrow().search_generation)
            {
                w.status_label.set_text("Sync: refreshing messages…");
                update_debug(&w, &st);
                return;
            }
            let replace_cancelled_search = {
                let state = st.borrow();
                state.sync_in_progress && state.search_loading
            };
            let generation = reserve_search_generation(&w);
            cancel_search_activity(&mut st.borrow_mut(), generation);
            if replace_cancelled_search {
                let refresh_query = st.borrow().current_query.clone();
                let fallback_generation = reserve_search_generation(&w);
                prepare_search_activity_preserving_request(
                    &w,
                    &st,
                    fallback_generation,
                    &refresh_query,
                );
                start_full_search(
                    &opts,
                    &w,
                    &st,
                    SearchWorkerRequest {
                        query: refresh_query,
                        generation: fallback_generation,
                        select_first: true,
                        delay: Duration::ZERO,
                    },
                );
            } else {
                w.status_label.set_text("Search cleared");
                update_thread_result_label(&w, &st);
                update_debug(&w, &st);
            }
        }
        SearchInputEvent::Reserved { query, generation } => {
            prepare_search_activity(&w, &st, generation, &query);
        }
        SearchInputEvent::Dispatch(request) => start_full_search(&opts, &w, &st, request),
    });
    widgets.search_bar.connect_debounce(handler);

    let st = state.clone();
    widgets
        .search_bar
        .connect_autocomplete(Rc::new(move || st.borrow().visible_tags.clone()));
}

fn set_input_mode(widgets: &Widgets, state: &SharedState, mode: InputMode, status: &str) {
    let changed = apply_input_mode(&mut state.borrow_mut(), mode);
    if changed {
        widgets
            .input_mode_generation
            .set(widgets.input_mode_generation.get().wrapping_add(1));
    }
    update_button_binding_labels(widgets, state);
    update_active_pane_visuals(widgets, state);
    widgets.status_label.set_text(status);
}

fn apply_input_mode(state: &mut UiState, mode: InputMode) -> bool {
    let changed = state.input_mode != mode;
    state.input_mode = mode;
    changed
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
    widgets.search_bar.focus();
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
            widgets.search_bar.focus();
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
    ensure_active_pane_visible(widgets, state);
    update_active_pane_visuals(widgets, state);
    match state.borrow().active_pane {
        ActivePane::Sidebar => {
            focus_sidebar_default(widgets);
        }
        ActivePane::Threads => {
            widgets.thread_list.focus();
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
    let pane = if pane_is_visible(widgets, pane) {
        pane
    } else {
        first_visible_pane(widgets).unwrap_or(pane)
    };
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
    let panes = visible_panes(widgets);
    let Some(first) = panes.first().copied() else {
        return;
    };
    let current = state.borrow().active_pane;
    let current_index = panes.iter().position(|pane| *pane == current).unwrap_or(0);
    let next_index = (current_index as isize + delta).clamp(0, panes.len() as isize - 1) as usize;
    set_active_pane(
        widgets,
        state,
        panes.get(next_index).copied().unwrap_or(first),
    );
}

fn connect_pane_visibility_toggles(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) {
    sync_pane_toggle_buttons(widgets);

    {
        let w = widgets.clone();
        let st = state.clone();
        widgets.sidebar_toggle_button.connect_clicked(move |_| {
            toggle_pane_visibility(&w, &st, ActivePane::Sidebar);
        });
    }
    {
        let w = widgets.clone();
        let st = state.clone();
        widgets.thread_list_toggle_button.connect_clicked(move |_| {
            toggle_pane_visibility(&w, &st, ActivePane::Threads);
        });
    }
    {
        let w = widgets.clone();
        let st = state.clone();
        widgets
            .message_pane_toggle_button
            .connect_clicked(move |_| {
                toggle_pane_visibility(&w, &st, ActivePane::Message);
            });
    }
    {
        let opts = options.clone();
        let w = widgets.clone();
        let st = state.clone();
        widgets.layout_toggle_button.connect_clicked(move |_| {
            toggle_layout_preference(&opts, &w, &st);
        });
    }
}

fn connect_auto_layout(widgets: &Widgets, state: &SharedState) {
    let w = widgets.clone();
    let st = state.clone();
    let last_width = Rc::new(Cell::new(0));
    let last_height = Rc::new(Cell::new(0));
    gtk::glib::timeout_add_local(Duration::from_millis(300), move || {
        let width = w.window.width();
        let height = w.window.height();
        if width != last_width.get() || height != last_height.get() {
            last_width.set(width);
            last_height.set(height);
            apply_auto_layout_for_current_size(&w, &st);
        }
        gtk::glib::ControlFlow::Continue
    });
}

fn apply_initial_pane_visibility(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let sidebar = options.show_sidebar;
    let list = options.show_message_list;
    let message = options.show_message_view;
    let any_visible = sidebar || list || message;
    widgets.left_pane.set_visible(sidebar || !any_visible);
    widgets.thread_pane.set_visible(list || !any_visible);
    widgets.message_pane.set_visible(message || !any_visible);
    ensure_active_pane_visible(widgets, state);
    sync_pane_button_classes(widgets, state);
}

fn toggle_layout_preference(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let next_preference = next_layout_preference(state.borrow().layout_preference);
    set_layout_preference(options, widgets, state, next_preference);
    focus_active_pane(widgets, state);
}

fn next_layout_preference(current: LayoutPreference) -> LayoutPreference {
    match current {
        LayoutPreference::ThreePane => LayoutPreference::Stacked,
        LayoutPreference::Stacked => LayoutPreference::Auto,
        LayoutPreference::Auto => LayoutPreference::ThreePane,
    }
}

fn set_layout_preference(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    preference: LayoutPreference,
) {
    update_runtime_layout_preference(options, preference);
    state.borrow_mut().layout_preference = preference;
    apply_layout_preference_for_current_size(widgets, state, preference, true);
}

fn update_runtime_layout_preference(options: &LaunchOptions, preference: LayoutPreference) {
    let mut settings = settings::snapshot(&options.runtime_settings);
    settings.layout_preference = preference;
    settings::update(&options.runtime_settings, settings);
}

fn apply_auto_layout_for_current_size(widgets: &Widgets, state: &SharedState) {
    let preference = state.borrow().layout_preference;
    if preference == LayoutPreference::Auto {
        apply_layout_preference_for_current_size(widgets, state, preference, false);
    }
}

fn apply_layout_preference_for_current_size(
    widgets: &Widgets,
    state: &SharedState,
    preference: LayoutPreference,
    announce: bool,
) {
    let current = state.borrow().content_layout;
    let next = layout_for_preference(
        preference,
        widgets.window.width(),
        widgets.window.height(),
        current,
    );
    apply_content_layout(widgets, state, next, announce);
}

fn apply_content_layout(
    widgets: &Widgets,
    state: &SharedState,
    layout: ContentLayout,
    announce: bool,
) {
    if state.borrow().content_layout == layout
        && widgets.outer_paned.start_child().is_some()
        && widgets.content_paned.start_child().is_some()
    {
        update_layout_toggle_button(widgets, state);
        update_debug(widgets, state);
        if announce {
            widgets.status_label.set_text(&layout_status_text(
                state.borrow().layout_preference,
                layout,
            ));
        }
        return;
    }

    clear_paned_children(&widgets.outer_paned);
    clear_paned_children(&widgets.content_paned);

    match layout {
        ContentLayout::ThreePane => {
            widgets
                .outer_paned
                .set_orientation(gtk::Orientation::Horizontal);
            widgets
                .content_paned
                .set_orientation(gtk::Orientation::Horizontal);
            widgets
                .content_paned
                .set_start_child(Some(&widgets.thread_pane));
            widgets
                .content_paned
                .set_end_child(Some(&widgets.message_pane));
            widgets
                .outer_paned
                .set_start_child(Some(&widgets.left_pane));
            widgets
                .outer_paned
                .set_end_child(Some(&widgets.content_paned));
            configure_paned_allocation(&widgets.outer_paned, false, true, false, true);
            widgets.outer_paned.set_position(SIDEBAR_MIN_WIDTH);
            // In the column layout, the message list is the read-from-left
            // child of this split. Keep it at its requested width and let the
            // message view absorb the narrow-window pressure first.
            configure_paned_allocation(&widgets.content_paned, false, true, false, true);
            widgets
                .content_paned
                .set_position(default_content_split(&widgets.content_paned));
        }
        ContentLayout::Stacked => {
            widgets
                .outer_paned
                .set_orientation(gtk::Orientation::Vertical);
            widgets
                .content_paned
                .set_orientation(gtk::Orientation::Horizontal);
            widgets
                .content_paned
                .set_start_child(Some(&widgets.left_pane));
            widgets
                .content_paned
                .set_end_child(Some(&widgets.thread_pane));
            widgets
                .outer_paned
                .set_start_child(Some(&widgets.content_paned));
            widgets
                .outer_paned
                .set_end_child(Some(&widgets.message_pane));
            configure_paned_allocation(&widgets.outer_paned, false, true, false, true);
            // In the stacked layout, the sidebar is the read-from-left child
            // of the top split. Match the column layout's sidebar policy so a
            // narrow window clips the right side before it eats the sidebar.
            configure_paned_allocation(&widgets.content_paned, false, true, false, true);
            widgets.content_paned.set_position(SIDEBAR_MIN_WIDTH);
            widgets
                .outer_paned
                .set_position(default_stacked_top_split(&widgets.outer_paned));
        }
    }

    state.borrow_mut().content_layout = layout;
    ensure_active_pane_visible(widgets, state);
    sync_pane_button_classes(widgets, state);
    update_layout_toggle_button(widgets, state);
    update_debug(widgets, state);
    if announce {
        widgets.status_label.set_text(&layout_status_text(
            state.borrow().layout_preference,
            layout,
        ));
    }
}

fn layout_status_text(preference: LayoutPreference, layout: ContentLayout) -> String {
    match preference {
        LayoutPreference::Auto => format!("Layout: auto ({})", content_layout_display_name(layout)),
        LayoutPreference::ThreePane | LayoutPreference::Stacked => {
            format!("Layout: {}", content_layout_display_name(layout))
        }
    }
}

fn configure_paned_allocation(
    paned: &gtk::Paned,
    resize_start: bool,
    resize_end: bool,
    shrink_start: bool,
    shrink_end: bool,
) {
    paned.set_resize_start_child(resize_start);
    paned.set_resize_end_child(resize_end);
    paned.set_shrink_start_child(shrink_start);
    paned.set_shrink_end_child(shrink_end);
}

fn clear_paned_children(paned: &gtk::Paned) {
    paned.set_start_child(None::<&gtk::Widget>);
    paned.set_end_child(None::<&gtk::Widget>);
}

fn toggle_pane_visibility(widgets: &Widgets, state: &SharedState, pane: ActivePane) {
    let visible = !pane_is_visible(widgets, pane);
    set_pane_visibility_from_control(widgets, state, pane, visible);
    focus_active_pane(widgets, state);
}

fn set_pane_visibility_from_control(
    widgets: &Widgets,
    state: &SharedState,
    pane: ActivePane,
    visible: bool,
) {
    if !visible && visible_panes(widgets).len() <= 1 {
        widgets
            .status_label
            .set_text("At least one pane must stay visible");
        sync_pane_toggle_buttons(widgets);
        return;
    }
    set_pane_visibility(widgets, state, pane, visible);
    sync_pane_button_classes(widgets, state);
}

fn set_pane_visibility(widgets: &Widgets, state: &SharedState, pane: ActivePane, visible: bool) {
    match pane {
        ActivePane::Sidebar => widgets.left_pane.set_visible(visible),
        ActivePane::Threads => widgets.thread_pane.set_visible(visible),
        ActivePane::Message => widgets.message_pane.set_visible(visible),
    }
    if visible {
        restore_pane_position(widgets, pane);
    } else if state.borrow().active_pane == pane {
        ensure_active_pane_visible(widgets, state);
    }
    update_active_pane_visuals(widgets, state);
    update_button_binding_labels(widgets, state);
    widgets.status_label.set_text(&format!(
        "{} {}",
        pane_display_name(pane),
        if visible { "shown" } else { "hidden" }
    ));
    update_debug(widgets, state);
}

fn sync_pane_toggle_buttons(widgets: &Widgets) {
    set_pane_button_visible_class(
        &widgets.sidebar_toggle_button,
        pane_is_visible(widgets, ActivePane::Sidebar),
    );
    set_pane_button_visible_class(
        &widgets.thread_list_toggle_button,
        pane_is_visible(widgets, ActivePane::Threads),
    );
    set_pane_button_visible_class(
        &widgets.message_pane_toggle_button,
        pane_is_visible(widgets, ActivePane::Message),
    );
}

fn sync_pane_button_classes(widgets: &Widgets, state: &SharedState) {
    sync_pane_toggle_buttons(widgets);
    update_layout_toggle_button(widgets, state);
    update_active_pane_visuals(widgets, state);
}

fn set_pane_button_visible_class<W>(widget: &W, visible: bool)
where
    W: IsA<gtk::Widget>,
{
    if visible {
        widget.add_css_class("notm-pane-visible");
        widget.remove_css_class("notm-pane-hidden");
    } else {
        widget.remove_css_class("notm-pane-visible");
        widget.add_css_class("notm-pane-hidden");
    }
}

fn ensure_active_pane_visible(widgets: &Widgets, state: &SharedState) {
    let active = state.borrow().active_pane;
    if pane_is_visible(widgets, active) {
        return;
    }
    if let Some(pane) = first_visible_pane(widgets) {
        state.borrow_mut().active_pane = pane;
    }
}

fn pane_is_visible(widgets: &Widgets, pane: ActivePane) -> bool {
    match pane {
        ActivePane::Sidebar => widgets.left_pane.get_visible(),
        ActivePane::Threads => widgets.thread_pane.get_visible(),
        ActivePane::Message => widgets.message_pane.get_visible(),
    }
}

fn visible_panes(widgets: &Widgets) -> Vec<ActivePane> {
    [
        ActivePane::Sidebar,
        ActivePane::Threads,
        ActivePane::Message,
    ]
    .into_iter()
    .filter(|pane| pane_is_visible(widgets, *pane))
    .collect()
}

fn first_visible_pane(widgets: &Widgets) -> Option<ActivePane> {
    [
        ActivePane::Threads,
        ActivePane::Message,
        ActivePane::Sidebar,
    ]
    .into_iter()
    .find(|pane| pane_is_visible(widgets, *pane))
}

fn pane_display_name(pane: ActivePane) -> &'static str {
    match pane {
        ActivePane::Sidebar => "Sidebar",
        ActivePane::Threads => "Message list",
        ActivePane::Message => "Message view",
    }
}

fn parse_pane_name(name: &str) -> Option<ActivePane> {
    match name.trim().replace('-', "_").to_lowercase().as_str() {
        "sidebar" | "side_bar" => Some(ActivePane::Sidebar),
        "list" | "message_list" | "thread_list" | "threads" => Some(ActivePane::Threads),
        "message" | "message_view" | "message_pane" => Some(ActivePane::Message),
        _ => None,
    }
}

fn pane_visibility_json(widgets: &Widgets, state: &SharedState) -> serde_json::Value {
    let state = state.borrow();
    json!({
        "ok": true,
        "sidebar": pane_is_visible(widgets, ActivePane::Sidebar),
        "message_list": pane_is_visible(widgets, ActivePane::Threads),
        "message_view": pane_is_visible(widgets, ActivePane::Message),
        "layout_preference": layout_preference_name(state.layout_preference),
        "layout": content_layout_name(state.content_layout),
    })
}

fn layout_state_json(widgets: &Widgets, state: &SharedState) -> serde_json::Value {
    let state = state.borrow();
    json!({
        "ok": true,
        "layout_preference": layout_preference_name(state.layout_preference),
        "layout": content_layout_name(state.content_layout),
        "window_width": widgets.window.width(),
        "window_height": widgets.window.height(),
        "outer_orientation": if widgets.outer_paned.orientation() == gtk::Orientation::Vertical {
            "vertical"
        } else {
            "horizontal"
        },
        "content_orientation": if widgets.content_paned.orientation() == gtk::Orientation::Vertical {
            "vertical"
        } else {
            "horizontal"
        },
        "outer_position": widgets.outer_paned.position(),
        "content_position": widgets.content_paned.position(),
        "outer_resize_start": widgets.outer_paned.resizes_start_child(),
        "outer_resize_end": widgets.outer_paned.resizes_end_child(),
        "outer_shrink_start": widgets.outer_paned.shrinks_start_child(),
        "outer_shrink_end": widgets.outer_paned.shrinks_end_child(),
        "content_resize_start": widgets.content_paned.resizes_start_child(),
        "content_resize_end": widgets.content_paned.resizes_end_child(),
        "content_shrink_start": widgets.content_paned.shrinks_start_child(),
        "content_shrink_end": widgets.content_paned.shrinks_end_child(),
    })
}

fn restore_pane_position(widgets: &Widgets, pane: ActivePane) {
    match pane {
        ActivePane::Sidebar => {
            let paned = sidebar_split_paned(widgets);
            if paned.position() < SIDEBAR_MIN_WIDTH / 2 {
                paned.set_position(SIDEBAR_MIN_WIDTH);
            }
        }
        ActivePane::Threads => {
            let paned = thread_split_paned(widgets);
            if paned.position() < 80 {
                paned.set_position(default_split_for_paned(widgets, &paned));
            }
        }
        ActivePane::Message => {
            let paned = message_split_paned(widgets);
            let extent = if state_layout_for_widgets(widgets) == ContentLayout::Stacked {
                paned.height()
            } else {
                paned.width()
            };
            if extent <= 0 || paned.position() >= extent.saturating_sub(80) {
                paned.set_position(default_split_for_paned(widgets, &paned));
            }
        }
    }
}

fn state_layout_for_widgets(widgets: &Widgets) -> ContentLayout {
    if widgets.outer_paned.orientation() == gtk::Orientation::Vertical {
        ContentLayout::Stacked
    } else {
        ContentLayout::ThreePane
    }
}

fn sidebar_split_paned(widgets: &Widgets) -> gtk::Paned {
    if state_layout_for_widgets(widgets) == ContentLayout::Stacked {
        widgets.content_paned.clone()
    } else {
        widgets.outer_paned.clone()
    }
}

fn thread_split_paned(widgets: &Widgets) -> gtk::Paned {
    widgets.content_paned.clone()
}

fn message_split_paned(widgets: &Widgets) -> gtk::Paned {
    if state_layout_for_widgets(widgets) == ContentLayout::Stacked {
        widgets.outer_paned.clone()
    } else {
        widgets.content_paned.clone()
    }
}

fn default_split_for_paned(widgets: &Widgets, paned: &gtk::Paned) -> i32 {
    if paned == &widgets.outer_paned && state_layout_for_widgets(widgets) == ContentLayout::Stacked
    {
        default_stacked_top_split(paned)
    } else if paned == &widgets.content_paned
        && state_layout_for_widgets(widgets) == ContentLayout::Stacked
    {
        SIDEBAR_MIN_WIDTH
    } else {
        default_content_split(paned)
    }
}

fn default_content_split(paned: &gtk::Paned) -> i32 {
    default_content_split_for_width(paned.width())
}

fn default_content_split_for_width(width: i32) -> i32 {
    if width <= 0 {
        return 560;
    }
    let maximum = width - MESSAGE_VIEW_MIN_WIDTH;
    if maximum >= THREAD_LIST_MIN_WIDTH {
        (width / 2).max(THREAD_LIST_MIN_WIDTH).min(maximum)
    } else {
        THREAD_LIST_MIN_WIDTH.min(width.max(1))
    }
}

fn default_stacked_top_split(paned: &gtk::Paned) -> i32 {
    let height = paned.height();
    if height <= 0 {
        return 360;
    }
    let maximum = (height - STACKED_MESSAGE_MIN_HEIGHT).max(1);
    (height / 2)
        .max(STACKED_TOP_MIN_HEIGHT.min(height))
        .min(maximum)
}

fn update_layout_toggle_button(widgets: &Widgets, state: &SharedState) {
    let state = state.borrow();
    let label = match state.layout_preference {
        LayoutPreference::Auto => {
            format!("Auto: {}", content_layout_short_name(state.content_layout))
        }
        LayoutPreference::ThreePane | LayoutPreference::Stacked => {
            content_layout_short_name(state.content_layout).to_string()
        }
    };
    widgets.layout_toggle_button.set_label(&label);
    widgets.layout_toggle_button.set_tooltip_text(Some(&format!(
        "Current layout: {}. Click to cycle auto, columns, and stacked.",
        content_layout_display_name(state.content_layout)
    )));
    widgets
        .layout_toggle_button
        .remove_css_class("notm-pane-hidden");
    widgets
        .layout_toggle_button
        .add_css_class("notm-pane-visible");
}

fn content_layout_display_name(layout: ContentLayout) -> &'static str {
    match layout {
        ContentLayout::ThreePane => "side-by-side columns",
        ContentLayout::Stacked => "stacked top panes",
    }
}

fn content_layout_short_name(layout: ContentLayout) -> &'static str {
    match layout {
        ContentLayout::ThreePane => "Columns",
        ContentLayout::Stacked => "Stacked",
    }
}

fn update_active_pane_visuals(widgets: &Widgets, state: &SharedState) {
    let active = state.borrow().active_pane;
    if active != ActivePane::Sidebar {
        clear_keyboard_cursor(&widgets.left_pane.clone().upcast());
    }
    set_current_pane_button_class(
        &widgets.sidebar_toggle_button,
        active == ActivePane::Sidebar,
    );
    set_current_pane_button_class(
        &widgets.thread_list_toggle_button,
        active == ActivePane::Threads,
    );
    set_current_pane_button_class(
        &widgets.message_pane_toggle_button,
        active == ActivePane::Message,
    );
}

fn set_current_pane_button_class<W>(widget: &W, current: bool)
where
    W: IsA<gtk::Widget>,
{
    if current {
        widget.add_css_class("notm-current-pane-button");
    } else {
        widget.remove_css_class("notm-current-pane-button");
    }
}

fn composer_has_focus(widgets: &Widgets) -> bool {
    widgets.composer.has_focus()
}

fn focus_first_composer_field(widgets: &Widgets) {
    widgets.composer.focus_first_field();
}

fn focus_composer_insert_target(widgets: &Widgets) {
    widgets.composer.focus_insert_target();
}

fn focus_sidebar_insert_target(widgets: &Widgets) {
    if widget_contains_focus(widgets.saved_name_entry.upcast_ref())
        || widget_contains_focus(widgets.saved_query_entry.upcast_ref())
    {
        return;
    }
    widgets.saved_name_entry.grab_focus();
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
    widgets.composer.move_focus(delta);
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

fn clear_keyboard_cursor(widget: &gtk::Widget) {
    widget.remove_css_class(KEYBOARD_CURSOR_CLASS);
    let mut child = widget.first_child();
    while let Some(child_widget) = child {
        child = child_widget.next_sibling();
        clear_keyboard_cursor(&child_widget);
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
        widgets.composer.scrolled().clone()
    } else if html_view_is_visible(widgets) {
        widgets.html_scrolled.clone()
    } else {
        widgets.message_scrolled.clone()
    }
}

fn scroll_message_view_lines(widgets: &Widgets, lines: f64) {
    if html_view_is_visible(widgets) {
        scroll_html_view_lines(widgets, lines);
    } else {
        scroll_window_lines(&active_message_scrolled(widgets), lines);
    }
}

fn vim_scroll_lines(widgets: &Widgets, state: &SharedState, lines: f64) {
    match state.borrow().active_pane {
        ActivePane::Threads => {}
        ActivePane::Sidebar => scroll_window_lines(&widgets.thread_list.scrolled(), lines),
        ActivePane::Message => scroll_message_view_lines(widgets, lines),
    }
}

fn vim_scroll_pages(widgets: &Widgets, state: &SharedState, pages: f64) {
    match state.borrow().active_pane {
        ActivePane::Threads => {}
        ActivePane::Sidebar => scroll_window_pages(&widgets.thread_list.scrolled(), pages),
        ActivePane::Message if html_view_is_visible(widgets) => {
            scroll_html_view_pages(widgets, pages)
        }
        ActivePane::Message => scroll_window_pages(&active_message_scrolled(widgets), pages),
    }
}

fn vim_scroll_to_edge(widgets: &Widgets, state: &SharedState, bottom: bool) {
    match state.borrow().active_pane {
        ActivePane::Threads => {}
        ActivePane::Sidebar => scroll_window_to_edge(&widgets.thread_list.scrolled(), bottom),
        ActivePane::Message if html_view_is_visible(widgets) => {
            scroll_html_view_to_edge(widgets, bottom)
        }
        ActivePane::Message => scroll_window_to_edge(&active_message_scrolled(widgets), bottom),
    }
}

fn scroll_html_view_lines(widgets: &Widgets, lines: f64) {
    scroll_web_view_lines(&widgets.html_view, &widgets.status_label, lines);
}

fn scroll_html_view_pages(widgets: &Widgets, pages: f64) {
    scroll_web_view_pages(&widgets.html_view, &widgets.status_label, pages);
}

fn scroll_html_view_to_edge(widgets: &Widgets, bottom: bool) {
    scroll_web_view_to_edge(&widgets.html_view, &widgets.status_label, bottom);
}

fn scroll_web_view_lines(view: &webkit6::WebView, status_label: &gtk::Label, lines: f64) {
    evaluate_web_view_scroll_script(
        view,
        status_label,
        &format!(
            "const e = document.scrollingElement || document.documentElement || document.body; \
             e.scrollBy(0, {}); \
             JSON.stringify({{y:e.scrollTop,h:e.scrollHeight,c:e.clientHeight}});",
            (lines * 40.0).round()
        ),
    );
}

fn scroll_web_view_pages(view: &webkit6::WebView, status_label: &gtk::Label, pages: f64) {
    evaluate_web_view_scroll_script(
        view,
        status_label,
        &format!(
            "const e = document.scrollingElement || document.documentElement || document.body; \
             e.scrollBy(0, Math.round(window.innerHeight * {})); \
             JSON.stringify({{y:e.scrollTop,h:e.scrollHeight,c:e.clientHeight}});",
            pages
        ),
    );
}

fn scroll_web_view_to_edge(view: &webkit6::WebView, status_label: &gtk::Label, bottom: bool) {
    let target = if bottom { "e.scrollHeight" } else { "0" };
    evaluate_web_view_scroll_script(
        view,
        status_label,
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
    widgets
        .pending_html_scroll_fraction
        .set(Some(fraction.clamp(0.0, 1.0)));
}

fn connect_html_scroll_restore(
    view: &webkit6::WebView,
    status_label: &gtk::Label,
    pending_fraction: &Rc<Cell<Option<f64>>>,
    requested_generation: &Rc<Cell<u64>>,
    completed_generation: &Rc<Cell<u64>>,
) {
    let pending_fraction = pending_fraction.clone();
    let requested_generation = requested_generation.clone();
    let completed_generation = completed_generation.clone();
    let status = status_label.clone();
    view.connect_is_loading_notify(move |view| {
        if view.is_loading() || completed_generation.get() != requested_generation.get() {
            return;
        }
        let Some(fraction) = pending_fraction.take() else {
            return;
        };
        evaluate_web_view_scroll_script(
            view,
            &status,
            &format!(
                "const e = document.scrollingElement || document.documentElement || document.body; \
                 const max = Math.max(0, e.scrollHeight - e.clientHeight); \
                 e.scrollTo(0, max * {});",
                fraction.clamp(0.0, 1.0)
            ),
        );
    });
}

fn connect_html_load_completion(
    view: &webkit6::WebView,
    requested_generation: &Rc<Cell<u64>>,
    lifecycle: &Rc<RefCell<HtmlLoadLifecycle>>,
    completed_generation: &Rc<Cell<u64>>,
) {
    let requested_generation_for_load = requested_generation.clone();
    let requested_generation_for_resource = requested_generation.clone();
    let lifecycle_for_load = lifecycle.clone();
    view.connect_load_changed(move |_, event| {
        lifecycle_for_load
            .borrow_mut()
            .load_changed(event, requested_generation_for_load.get());
    });

    let lifecycle_for_resource = lifecycle.clone();
    let completed_generation = completed_generation.clone();
    view.connect_resource_load_started(move |view, resource, _| {
        let is_main_resource = view
            .main_resource()
            .is_some_and(|main_resource| main_resource == resource.clone());
        if !is_main_resource {
            return;
        }

        let Some(generation) = lifecycle_for_resource.borrow_mut().bind_main_resource() else {
            return;
        };
        let lifecycle = lifecycle_for_resource.clone();
        let requested_generation = requested_generation_for_resource.clone();
        let completed_generation = completed_generation.clone();
        // WebView Finished events have no load identity. The main WebResource
        // object does, so only its captured generation may advance completion;
        // callers additionally require WebView::is_loading() to be false.
        resource.connect_finished(move |_| {
            if let Some(generation) = lifecycle
                .borrow_mut()
                .finish_main_resource(generation, requested_generation.get())
            {
                completed_generation.set(generation);
            }
        });
    });
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HtmlLoadLifecycle {
    active_generation: Option<u64>,
    awaiting_main_resource_generation: Option<u64>,
}

impl HtmlLoadLifecycle {
    fn load_changed(&mut self, event: webkit6::LoadEvent, requested_generation: u64) {
        if event == webkit6::LoadEvent::Started {
            self.start(requested_generation);
        }
    }

    fn start(&mut self, generation: u64) {
        self.active_generation = Some(generation);
        self.awaiting_main_resource_generation = Some(generation);
    }

    fn bind_main_resource(&mut self) -> Option<u64> {
        let generation = self.awaiting_main_resource_generation.take()?;
        (self.active_generation == Some(generation)).then_some(generation)
    }

    fn finish_main_resource(&mut self, generation: u64, requested_generation: u64) -> Option<u64> {
        if self.active_generation != Some(generation) || generation != requested_generation {
            return None;
        }
        self.active_generation = None;
        self.awaiting_main_resource_generation = None;
        Some(generation)
    }
}

fn evaluate_web_view_scroll_script(
    view: &webkit6::WebView,
    status_label: &gtk::Label,
    script: &str,
) {
    let status = status_label.clone();
    view.evaluate_javascript(
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
    let target = if bottom {
        if total > 0 {
            total - 1
        } else {
            window_offset + len - 1
        }
    } else {
        0
    };
    if (window_offset..window_offset + len).contains(&target) {
        select_thread_index_clamped(options, widgets, state, target - window_offset);
    } else {
        load_thread_page_containing_index(options, widgets, state, &query, target);
    }
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
    if index >= widgets.thread_list.model_len() {
        return;
    }
    let already_selected = selected_thread_index(widgets) == Some(index);
    select_thread_index_in_list(widgets, index);
    if already_selected {
        select_thread_by_index(options, widgets, state, index, false);
    }
}

fn load_thread_page_containing_index(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    query: &str,
    target_index: usize,
) {
    if state.borrow().search_loading {
        widgets
            .status_label
            .set_text("Wait for the current search before loading another page");
        return;
    }
    let plan = LocatePagePlan::new(
        query,
        target_index,
        settings::page_size(&options.runtime_settings),
        visual_selection_anchor_index(widgets, state),
    );
    set_thread_loading_indicator(widgets, &plan.loading_status());

    let generation = reserve_search_generation(widgets);
    begin_search_activity(&mut state.borrow_mut(), generation, &plan.query);
    widgets.thread_list.set_result_label("Loading thread page…");
    let request = SearchPageRequest {
        query: plan.query.clone(),
        generation,
        offset: plan.offset,
        select_first: false,
        delay: Duration::ZERO,
    };
    let coordinator = search_page_coordinator(options);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    coordinator.launch(request, "thread page load cancelled", move |response| {
        if !accept_search_page_response(&w, &st, &response) {
            return;
        }
        match response.result {
            Ok(data) => {
                let outcome = thread_list::reduce_replace_search(data);
                let keep_visual = plan.visual_anchor_index.is_some()
                    && st.borrow().visual_select_mode
                    && st.borrow().current_query == outcome.update.current_query;
                finish_replaced_search(&opts, &w, &st, outcome, false);
                if keep_visual {
                    let mut state = st.borrow_mut();
                    state.visual_select_mode = true;
                    state.visual_select_anchor = plan.visual_anchor_index;
                }
                let local_index = plan
                    .target_index
                    .saturating_sub(st.borrow().thread_window_offset);
                select_thread_index_clamped(&opts, &w, &st, local_index);
                update_thread_result_label(&w, &st);
            }
            Err(err) => {
                let has_threads = !st.borrow().thread_list_items.is_empty();
                finish_search_error(&w, &st, thread_list::reduce_search_error(err, has_threads));
            }
        }
    });
}

fn set_thread_loading_indicator(widgets: &Widgets, message: &str) {
    widgets.status_label.set_text(message);
    widgets.thread_list.set_load_more_state("Loading…", false);
}

fn thread_list_display(state: &UiState) -> ThreadListDisplay {
    ThreadListDisplay {
        numbers: state.show_thread_numbers,
        dates: state.show_thread_dates,
        tags: state.show_thread_tags,
        preview: state.show_thread_preview,
        preview_lines: state.thread_preview_lines,
    }
}

fn thread_row_snapshot(state: &SharedState, index: usize) -> Option<ThreadRowSnapshot> {
    let state = state.borrow();
    let thread = state.thread_list_items.get(index)?.clone();
    let detail = state
        .thread_details
        .get(&thread.thread_id)
        .cloned()
        .unwrap_or_default();
    let absolute_index = state.thread_window_offset + index;
    let visual_selected = visual_selection_range_from_state(&state)
        .is_some_and(|(start, end)| (start..=end).contains(&absolute_index))
        || state.multi_selected_threads.contains(&thread.thread_id);
    Some(ThreadRowSnapshot {
        thread,
        detail,
        absolute_index,
        display: thread_list_display(&state),
        visual_selected,
    })
}

fn thread_model_snapshot(state: &SharedState) -> ThreadModelSnapshot {
    let state = state.borrow();
    let range = visual_selection_range_from_state(&state);
    let marked_indices = state
        .thread_list_items
        .iter()
        .enumerate()
        .filter_map(|(index, thread)| {
            let absolute = state.thread_window_offset + index;
            (range.is_some_and(|(start, end)| (start..=end).contains(&absolute))
                || state.multi_selected_threads.contains(&thread.thread_id))
            .then_some(index)
        })
        .collect();
    ThreadModelSnapshot {
        len: state.thread_list_items.len(),
        display: thread_list_display(&state),
        marked_indices,
    }
}

fn toggle_thread_multi_selection(state: &SharedState, thread_id: &str) -> bool {
    let mut state = state.borrow_mut();
    state.visual_select_mode = false;
    state.visual_select_anchor = None;
    state.visual_select_cursor = None;
    state.visual_selected_threads.clear();
    state.visual_selection_pending_range = None;
    if state.multi_selected_threads.contains(thread_id) {
        state.multi_selected_threads.remove(thread_id);
        false
    } else {
        state.multi_selected_threads.insert(thread_id.to_string());
        true
    }
}

fn show_thread_list_loading(widgets: &Widgets, message: &str) {
    widgets.thread_list.show_loading(message);
}

fn show_thread_list_message(widgets: &Widgets, message: &str) {
    widgets.thread_list.show_message(message);
}

fn visible_thread_row_count(widgets: &Widgets) -> isize {
    widgets.thread_list.visible_row_count()
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

fn focus_thread_list(widgets: &Widgets) {
    widgets.thread_list.focus();
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
    let state = state.borrow();
    if state.show_keybind_hints && state.input_mode == InputMode::Normal && !binding.is_empty() {
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

fn message_pane_shortcuts_available(widgets: &Widgets) -> bool {
    pane_is_visible(widgets, ActivePane::Message)
}

fn visible_binding(visible: bool, binding: &'static str) -> &'static str {
    if visible { binding } else { "" }
}

fn update_button_binding_labels(widgets: &Widgets, state: &SharedState) {
    let message_bindings = message_pane_shortcuts_available(widgets);
    set_button_label(&widgets.compose_button, "Compose", "c", state);
    set_button_label(&widgets.debug_button, "Debug", "d", state);
    set_button_label(&widgets.palette_button, "Commands", "Ctrl+K", state);
    set_button_label(&widgets.settings_button, "Settings", ",", state);
    set_button_label(&widgets.help_button, "Help", "?", state);
    update_layout_toggle_button(widgets, state);
    set_button_label(&widgets.search_bar.button(), "Search", "/", state);
    set_button_label(
        &widgets.thread_list.load_more_button(),
        "Load more",
        "Ctrl+f",
        state,
    );
    set_button_label(&widgets.archive_button, "Archive", "a", state);
    let read_base = strip_binding_suffix(&widgets.read_toggle_button.label().unwrap_or_default());
    set_button_label(&widgets.read_toggle_button, &read_base, "u", state);
    let flag_base = strip_binding_suffix(&widgets.flag_toggle_button.label().unwrap_or_default());
    set_button_label(&widgets.flag_toggle_button, &flag_base, "f", state);
    set_button_label(&widgets.trash_button, "Trash", "t", state);
    set_button_label(&widgets.spam_button, "Spam", "s", state);
    set_menu_button_label(&widgets.tag_menu_button, "Tag…", "T", state);
    set_button_label(&widgets.single_tag_button, "Add/remove tag", "T t", state);
    set_button_label(&widgets.tag_command_button, "Tag multiple", "T m", state);
    widgets
        .tag_command_apply_button
        .set_label(&button_label("Apply", "", state));
    set_menu_button_label(&widgets.undo_tag_button, "Undo", "z", state);
    set_button_label(&widgets.undo_last_tag_button, "Undo last", "z z", state);
    set_button_label(&widgets.undo_list_tag_button, "Undo multiple", "z m", state);
    set_menu_button_label(
        &widgets.response_menu_button,
        "Respond",
        visible_binding(message_bindings, "r"),
        state,
    );
    set_button_label(
        &widgets.reply_button,
        "Reply",
        visible_binding(message_bindings, "r r"),
        state,
    );
    set_button_label(
        &widgets.reply_all_button,
        "Reply all",
        visible_binding(message_bindings, "r a"),
        state,
    );
    set_button_label(
        &widgets.forward_button,
        "Forward",
        visible_binding(message_bindings, "r f"),
        state,
    );
    set_button_label(
        &widgets.forward_attachment_button,
        "Forward attached",
        visible_binding(message_bindings, "r A"),
        state,
    );
    let message_base =
        strip_binding_suffix(&widgets.message_menu_button.label().unwrap_or_default());
    set_menu_button_label(
        &widgets.message_menu_button,
        &message_base,
        visible_binding(message_bindings, "J/K"),
        state,
    );
    set_menu_button_label(
        &widgets.message_tag_menu_button,
        "Tag message",
        visible_binding(message_bindings, "M"),
        state,
    );
    set_button_label(
        &widgets.message_archive_button,
        "Archive message",
        visible_binding(message_bindings, "M a"),
        state,
    );
    let message_read_base = strip_binding_suffix(
        &widgets
            .message_read_toggle_button
            .label()
            .unwrap_or_default(),
    );
    set_button_label(
        &widgets.message_read_toggle_button,
        &message_read_base,
        visible_binding(message_bindings, "M u"),
        state,
    );
    let message_flag_base = strip_binding_suffix(
        &widgets
            .message_flag_toggle_button
            .label()
            .unwrap_or_default(),
    );
    set_button_label(
        &widgets.message_flag_toggle_button,
        &message_flag_base,
        visible_binding(message_bindings, "M f"),
        state,
    );
    set_button_label(
        &widgets.message_trash_button,
        "Move message to trash",
        visible_binding(message_bindings, "M t"),
        state,
    );
    set_button_label(
        &widgets.message_spam_button,
        "Mark message as spam",
        visible_binding(message_bindings, "M s"),
        state,
    );
    let message_custom_tag_base = strip_binding_suffix(
        &widgets
            .message_custom_tag_apply_button
            .label()
            .unwrap_or_default(),
    );
    set_button_label(
        &widgets.message_custom_tag_apply_button,
        &message_custom_tag_base,
        visible_binding(message_bindings, "M T"),
        state,
    );
    set_menu_button_label(
        &widgets.view_menu_button,
        "View",
        visible_binding(message_bindings, "V"),
        state,
    );
    set_button_label(
        &widgets.view_text_button,
        "Text",
        visible_binding(message_bindings, "V t"),
        state,
    );
    set_button_label(
        &widgets.view_html_button,
        "Visual HTML",
        visible_binding(message_bindings, "V v"),
        state,
    );
    set_button_label(
        &widgets.view_headers_button,
        "Full headers",
        visible_binding(message_bindings, "V h"),
        state,
    );
    set_button_label(
        &widgets.view_raw_button,
        "Raw source",
        visible_binding(message_bindings, "V r"),
        state,
    );
    update_sender_view_preference_button(widgets, state);
    set_button_label(
        &widgets.collapse_quotes_button,
        "Collapse quotes",
        visible_binding(message_bindings, "q"),
        state,
    );
    set_menu_button_label(
        &widgets.copy_menu_button,
        "Copy",
        visible_binding(message_bindings, "y"),
        state,
    );
    set_button_label(
        &widgets.copy_message_id_button,
        "Copy message id",
        visible_binding(message_bindings, "y m"),
        state,
    );
    set_button_label(
        &widgets.copy_thread_id_button,
        "Copy thread id",
        visible_binding(message_bindings, "y t"),
        state,
    );
    set_button_label(
        &widgets.copy_from_email_button,
        "Copy from email",
        visible_binding(message_bindings, "y f"),
        state,
    );
    set_button_label(
        &widgets.copy_to_email_button,
        "Copy to email",
        visible_binding(message_bindings, "y o"),
        state,
    );
    set_button_label(
        &widgets.copy_cc_email_button,
        "Copy cc email",
        visible_binding(message_bindings, "y c"),
        state,
    );
    set_button_label(
        &widgets.copy_subject_button,
        "Copy subject",
        visible_binding(message_bindings, "y s"),
        state,
    );
    let image_base = strip_binding_suffix(&widgets.image_policy_button.label().unwrap_or_default());
    set_button_label(
        &widgets.image_policy_button,
        &image_base,
        visible_binding(message_bindings, "I"),
        state,
    );
    set_button_label(
        &widgets.composer.add_attachment_button(),
        "Add attachment…",
        "A",
        state,
    );
    set_button_label(
        &widgets.composer.save_draft_button(),
        "Save draft",
        "S",
        state,
    );
    let clear_base = strip_binding_suffix(
        &widgets
            .composer
            .clear_draft_button()
            .label()
            .unwrap_or_default(),
    );
    set_button_label(
        &widgets.composer.clear_draft_button(),
        &clear_base,
        "x",
        state,
    );
    set_button_label(
        &widgets.composer.delete_local_draft_button(),
        "Delete local draft",
        "D",
        state,
    );
    set_button_label(&widgets.composer.send_button(), "Send", "Ctrl+Enter", state);
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
    connect_text_focus(
        &widgets.message_custom_tag_entry,
        widgets,
        state,
        ActivePane::Message,
    );
    connect_text_focus(
        &widgets.search_bar.entry(),
        widgets,
        state,
        ActivePane::Threads,
    );
    connect_text_focus(
        &widgets.composer.sender_entry(),
        widgets,
        state,
        ActivePane::Message,
    );
    connect_text_focus(
        &widgets.composer.to_entry(),
        widgets,
        state,
        ActivePane::Message,
    );
    connect_text_focus(
        &widgets.composer.cc_entry(),
        widgets,
        state,
        ActivePane::Message,
    );
    connect_text_focus(
        &widgets.composer.bcc_entry(),
        widgets,
        state,
        ActivePane::Message,
    );
    connect_text_focus(
        &widgets.composer.subject_entry(),
        widgets,
        state,
        ActivePane::Message,
    );
    connect_compose_body_focus(&widgets.composer.body(), widgets, state);
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
        let normal_mode = state.input_mode == InputMode::Normal;
        drop(state);
        update_button_binding_labels(&w, &st);
        update_active_pane_visuals(&w, &st);
        if normal_mode {
            w.status_label
                .set_text("Normal mode: press Enter or i to edit this field");
        }
    });
    widget.add_controller(focus);
}

fn main_text_entry_has_focus(widgets: &Widgets) -> bool {
    [
        &widgets.saved_name_entry,
        &widgets.saved_query_entry,
        &widgets.custom_tag_entry,
        &widgets.tag_command_entry,
        &widgets.message_custom_tag_entry,
        &widgets.search_bar.entry(),
        &widgets.composer.sender_entry(),
        &widgets.composer.to_entry(),
        &widgets.composer.cc_entry(),
        &widgets.composer.bcc_entry(),
        &widgets.composer.subject_entry(),
    ]
    .into_iter()
    .any(|entry| widget_contains_focus(entry.upcast_ref()))
}

fn main_shortcut_controller_count(widgets: &Widgets) -> u32 {
    let controllers = widgets.window.observe_controllers();
    (0..controllers.n_items())
        .filter(|index| {
            controllers
                .item(*index)
                .and_then(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
                .is_some_and(|controller| {
                    controller.name().as_deref() == Some(MAIN_SHORTCUT_CONTROLLER_NAME)
                })
        })
        .count() as u32
}

fn normal_text_focus_blocks_key(key: gtk::gdk::Key) -> bool {
    key_to_digit(key).is_none()
        && key.to_unicode().is_some()
        && !matches!(
            key,
            gtk::gdk::Key::h
                | gtk::gdk::Key::H
                | gtk::gdk::Key::j
                | gtk::gdk::Key::k
                | gtk::gdk::Key::l
                | gtk::gdk::Key::L
                | gtk::gdk::Key::g
                | gtk::gdk::Key::G
                | gtk::gdk::Key::i
                | gtk::gdk::Key::slash
                | gtk::gdk::Key::colon
                | gtk::gdk::Key::comma
                | gtk::gdk::Key::question
        )
}

fn normal_text_focus_starts_insert(key: gtk::gdk::Key, mods: gtk::gdk::ModifierType) -> bool {
    let ctrl = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK);
    let alt = mods.contains(gtk::gdk::ModifierType::ALT_MASK);
    let super_key = mods.contains(gtk::gdk::ModifierType::SUPER_MASK);
    if alt || super_key {
        return false;
    }
    if ctrl {
        matches!(
            key,
            gtk::gdk::Key::a
                | gtk::gdk::Key::c
                | gtk::gdk::Key::v
                | gtk::gdk::Key::x
                | gtk::gdk::Key::y
                | gtk::gdk::Key::z
                | gtk::gdk::Key::Z
        )
    } else {
        matches!(
            key,
            gtk::gdk::Key::BackSpace
                | gtk::gdk::Key::Delete
                | gtk::gdk::Key::Left
                | gtk::gdk::Key::Right
                | gtk::gdk::Key::Home
                | gtk::gdk::Key::End
        )
    }
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
        drop(state);
        set_input_mode(
            &w,
            &st,
            InputMode::Insert,
            "Vim composer: Esc leaves insert/visual, Esc again exits to notm",
        );
    });
    widget.add_controller(focus);
}

type MainShortcutHandler = dyn Fn(gtk::gdk::Key, gtk::gdk::ModifierType) -> gtk::glib::Propagation;
const MAIN_SHORTCUT_CONTROLLER_NAME: &str = "notm-main-shortcut-router";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComposerShortcutAction {
    AddAttachment,
    SaveDraft,
    ClearDraft,
    DeleteLocalDraft,
}

fn composer_shortcut_action(
    key: gtk::gdk::Key,
    modifiers: gtk::gdk::ModifierType,
) -> Option<ComposerShortcutAction> {
    if modifiers.intersects(
        gtk::gdk::ModifierType::CONTROL_MASK
            | gtk::gdk::ModifierType::ALT_MASK
            | gtk::gdk::ModifierType::SUPER_MASK,
    ) {
        return None;
    }
    if shifted_shortcut_key(key, modifiers, gtk::gdk::Key::a, gtk::gdk::Key::A) {
        Some(ComposerShortcutAction::AddAttachment)
    } else if shifted_shortcut_key(key, modifiers, gtk::gdk::Key::s, gtk::gdk::Key::S) {
        Some(ComposerShortcutAction::SaveDraft)
    } else if key == gtk::gdk::Key::x && !modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
        Some(ComposerShortcutAction::ClearDraft)
    } else if shifted_shortcut_key(key, modifiers, gtk::gdk::Key::d, gtk::gdk::Key::D) {
        Some(ComposerShortcutAction::DeleteLocalDraft)
    } else {
        None
    }
}

fn activate_composer_shortcut(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    action: ComposerShortcutAction,
) {
    match action {
        ComposerShortcutAction::AddAttachment => show_add_attachment_dialog(widgets, state),
        ComposerShortcutAction::SaveDraft => save_current_draft_from_ui(options, widgets, state),
        ComposerShortcutAction::ClearDraft => {
            let _ = clear_current_draft_from_ui(options, widgets, state);
        }
        ComposerShortcutAction::DeleteLocalDraft => {
            delete_active_draft_from_ui(options, widgets, state);
        }
    }
}

#[derive(Clone)]
struct MainShortcutRouter {
    handler: Rc<MainShortcutHandler>,
}

impl MainShortcutRouter {
    fn handle_key(
        &self,
        key: gtk::gdk::Key,
        modifiers: gtk::gdk::ModifierType,
    ) -> gtk::glib::Propagation {
        (self.handler)(key, modifiers)
    }
}

fn install_shortcuts(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    saved_store: &SavedSearchStore,
) -> MainShortcutRouter {
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    let saved_for_capture = saved_store.clone();
    let fallback_handler: Rc<MainShortcutHandler> = Rc::new(move |key, mods| {
        let ctrl = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK);
        let normal_mode = st.borrow().input_mode == InputMode::Normal;
        if normal_mode
            && main_text_entry_has_focus(&w)
            && normal_text_focus_starts_insert(key, mods)
        {
            set_input_mode(&w, &st, InputMode::Insert, "Insert mode (Esc for normal)");
            return gtk::glib::Propagation::Proceed;
        }
        if ctrl && (key == gtk::gdk::Key::k || key == gtk::gdk::Key::K) {
            show_command_palette(&opts, &w, &st, &undo);
            return gtk::glib::Propagation::Stop;
        }
        if ctrl && (key == gtk::gdk::Key::s || key == gtk::gdk::Key::S) {
            if !compose_view_is_visible(&w) {
                open_save_current_search_prompt(&w, &st, &saved_for_capture);
                return gtk::glib::Propagation::Stop;
            }
            return gtk::glib::Propagation::Proceed;
        }
        if ctrl && let Some(digit) = key_to_digit(key) {
            match digit {
                1 => {
                    toggle_pane_visibility(&w, &st, ActivePane::Sidebar);
                    return gtk::glib::Propagation::Stop;
                }
                2 => {
                    toggle_pane_visibility(&w, &st, ActivePane::Threads);
                    return gtk::glib::Propagation::Stop;
                }
                3 => {
                    toggle_pane_visibility(&w, &st, ActivePane::Message);
                    return gtk::glib::Propagation::Stop;
                }
                4 => {
                    toggle_layout_preference(&opts, &w, &st);
                    return gtk::glib::Propagation::Stop;
                }
                _ => {}
            }
        }
        if key == gtk::gdk::Key::Escape && close_command_palette(&w, &st) {
            return gtk::glib::Propagation::Stop;
        }
        if ctrl
            && (key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter)
            && compose_view_is_visible(&w)
        {
            let _ = send_compose(&opts, &w, &st);
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
                if w.composer.body().has_focus() {
                    if ctrl || w.composer.vim_ready_for_app_escape() {
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
        if ctrl && (key == gtk::gdk::Key::f || key == gtk::gdk::Key::F) {
            if st.borrow().active_pane == ActivePane::Threads {
                load_more_threads(&opts, &w, &st, true);
                return gtk::glib::Propagation::Stop;
            }
            return gtk::glib::Propagation::Proceed;
        }
        if let Some(lines) = crate::widgets::vim_viewport_scroll_lines(key, mods) {
            if pane_is_visible(&w, ActivePane::Threads)
                && !compose_view_is_visible(&w)
                && !main_text_entry_has_focus(&w)
            {
                scroll_window_lines(&w.thread_list.scrolled(), lines);
                return gtk::glib::Propagation::Stop;
            }
            return gtk::glib::Propagation::Proceed;
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
        if ctrl && (key == gtk::gdk::Key::b || key == gtk::gdk::Key::B) {
            if st.borrow().active_pane == ActivePane::Threads {
                select_thread_page(&opts, &w, &st, -1);
            } else if st.borrow().active_pane == ActivePane::Sidebar {
                move_sidebar_focus(&w, -10);
            } else if compose_view_is_visible(&w) {
                move_composer_focus(&w, -10);
            } else {
                vim_scroll_pages(&w, &st, -1.0);
            }
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter {
            if main_text_entry_has_focus(&w) {
                return gtk::glib::Propagation::Proceed;
            }
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
    let pending_message_tag = Rc::new(RefCell::new(false));
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
        pending_message_tag.clone(),
        pending_tag.clone(),
        pending_undo.clone(),
        undo.clone(),
    );
    let observed_input_mode_generation = Cell::new(w.input_mode_generation.get());
    let normal_handler: Rc<MainShortcutHandler> = Rc::new(move |key, mods| {
        let cancel_pending_sequences = || {
            *pending_go.borrow_mut() = false;
            *pending_custom_search.borrow_mut() = false;
            *pending_response.borrow_mut() = false;
            *pending_view.borrow_mut() = false;
            *pending_copy.borrow_mut() = false;
            *pending_message_tag.borrow_mut() = false;
            *pending_tag.borrow_mut() = false;
            *pending_undo.borrow_mut() = false;
            clear_numeric_prefix(&numeric_prefix);
            w.response_menu_button.popdown();
            w.view_menu_button.popdown();
            w.copy_menu_button.popdown();
            w.tag_menu_button.popdown();
            w.message_tag_menu_button.popdown();
            w.undo_tag_button.popdown();
        };
        let input_mode_generation = w.input_mode_generation.get();
        if observed_input_mode_generation.get() != input_mode_generation {
            cancel_pending_sequences();
            observed_input_mode_generation.set(input_mode_generation);
        }
        let ctrl = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK);
        if ctrl {
            return gtk::glib::Propagation::Proceed;
        }
        if st.borrow().input_mode == InputMode::Insert {
            return gtk::glib::Propagation::Proceed;
        }
        if compose_view_is_visible(&w)
            && let Some(action) = composer_shortcut_action(key, mods)
        {
            cancel_pending_sequences();
            clear_numeric_prefix(&numeric_prefix);
            activate_composer_shortcut(&opts, &w, &st, action);
            return gtk::glib::Propagation::Stop;
        }
        if *pending_custom_search.borrow() {
            *pending_custom_search.borrow_mut() = false;
            clear_numeric_prefix(&numeric_prefix);
            let handled = open_custom_saved_search_by_key(&opts, &w, &st, &saved, key);
            clear_go_prompt_status(&w);
            return if handled {
                gtk::glib::Propagation::Stop
            } else if main_text_entry_has_focus(&w) && normal_text_focus_blocks_key(key) {
                w.status_label
                    .set_text("Normal mode: press Enter or i to edit this field");
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
            } else if main_text_entry_has_focus(&w) && normal_text_focus_blocks_key(key) {
                w.status_label
                    .set_text("Normal mode: press Enter or i to edit this field");
                gtk::glib::Propagation::Stop
            } else {
                clear_go_prompt_status(&w);
                gtk::glib::Propagation::Proceed
            };
        }
        if (key == gtk::gdk::Key::Return || key == gtk::gdk::Key::KP_Enter)
            && main_text_entry_has_focus(&w)
        {
            cancel_pending_sequences();
            set_input_mode(&w, &st, InputMode::Insert, "Insert mode (Esc for normal)");
            return gtk::glib::Propagation::Stop;
        }
        if main_text_entry_has_focus(&w) && normal_text_focus_blocks_key(key) {
            cancel_pending_sequences();
            w.status_label
                .set_text("Normal mode: press Enter or i to edit this field");
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::space && st.borrow().active_pane == ActivePane::Threads {
            clear_numeric_prefix(&numeric_prefix);
            toggle_multi_selected_thread(&w, &st);
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::h || key == gtk::gdk::Key::H {
            clear_numeric_prefix(&numeric_prefix);
            move_active_pane(&w, &st, -1);
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::l || key == gtk::gdk::Key::L {
            clear_numeric_prefix(&numeric_prefix);
            move_active_pane(&w, &st, 1);
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Escape {
            cancel_pending_sequences();
            if st.borrow().visual_select_mode {
                clear_visual_selection(&w, &st);
            } else if !st.borrow().multi_selected_threads.is_empty() {
                clear_multi_selection(&w, &st);
            } else {
                w.status_label.set_text("Normal mode");
            }
            return gtk::glib::Propagation::Stop;
        }
        if *pending_response.borrow() {
            if !message_pane_shortcuts_available(&w) {
                *pending_response.borrow_mut() = false;
                w.response_menu_button.popdown();
                return gtk::glib::Propagation::Proceed;
            }
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
            if !message_pane_shortcuts_available(&w) {
                *pending_view.borrow_mut() = false;
                w.view_menu_button.popdown();
                return gtk::glib::Propagation::Proceed;
            }
            *pending_view.borrow_mut() = false;
            w.view_menu_button.popdown();
            clear_numeric_prefix(&numeric_prefix);
            let handled = activate_message_view_sequence_key(&opts, &w, &st, key);
            return if handled {
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            };
        }
        if *pending_copy.borrow() {
            if !message_pane_shortcuts_available(&w) {
                *pending_copy.borrow_mut() = false;
                w.copy_menu_button.popdown();
                return gtk::glib::Propagation::Proceed;
            }
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
        if *pending_message_tag.borrow() {
            if !message_pane_shortcuts_available(&w) || compose_view_is_visible(&w) {
                *pending_message_tag.borrow_mut() = false;
                w.message_tag_menu_button.popdown();
                return gtk::glib::Propagation::Proceed;
            }
            *pending_message_tag.borrow_mut() = false;
            clear_numeric_prefix(&numeric_prefix);
            return match activate_message_tag_sequence_key(&w, &st, key, mods) {
                MessageTagSequenceOutcome::CloseMenu => {
                    w.message_tag_menu_button.popdown();
                    gtk::glib::Propagation::Stop
                }
                MessageTagSequenceOutcome::KeepMenuOpen => gtk::glib::Propagation::Stop,
                MessageTagSequenceOutcome::Unhandled => {
                    w.message_tag_menu_button.popdown();
                    gtk::glib::Propagation::Proceed
                }
            };
        }
        if *pending_tag.borrow() {
            *pending_tag.borrow_mut() = false;
            clear_numeric_prefix(&numeric_prefix);
            let handled = handle_tag_sequence_key(&w, &st, key);
            if !handled {
                w.tag_menu_button.popdown();
            }
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
        } else if key == gtk::gdk::Key::colon {
            clear_numeric_prefix(&numeric_prefix);
            show_command_palette(&opts, &w, &st, &undo);
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
        } else if let Some(delta) = message_navigation_delta(key, mods) {
            let available = message_pane_shortcuts_available(&w) && !compose_view_is_visible(&w);
            clear_numeric_prefix(&numeric_prefix);
            available && select_relative_message(&opts, &w, &st, delta * count as isize)
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
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &opts.runtime_settings,
                    ),
                },
            );
            true
        } else if key == gtk::gdk::Key::u {
            clear_numeric_prefix(&numeric_prefix);
            toggle_unread_selected(&opts, &w, &st, &undo);
            true
        } else if shifted_shortcut_key(key, mods, gtk::gdk::Key::f, gtk::gdk::Key::F) {
            clear_numeric_prefix(&numeric_prefix);
            start_link_hint_mode(&opts, &w, &st)
        } else if key == gtk::gdk::Key::f {
            clear_numeric_prefix(&numeric_prefix);
            toggle_flagged_selected(&opts, &w, &st, &undo);
            true
        } else if is_message_tag_menu_key(key, mods)
            && message_pane_shortcuts_available(&w)
            && !compose_view_is_visible(&w)
        {
            clear_numeric_prefix(&numeric_prefix);
            *pending_message_tag.borrow_mut() = true;
            w.message_tag_menu_button.popup();
            w.status_label.set_text(
                "Current message: a archive, u read/unread, f flag, t trash, s spam, T custom tag; j/k choose",
            );
            true
        } else if is_tag_sequence_prefix(key, mods) {
            clear_numeric_prefix(&numeric_prefix);
            *pending_tag.borrow_mut() = true;
            show_tag_sequence_menu(&w);
            w.status_label
                .set_text("Tag: t add/remove tag, m tag multiple");
            true
        } else if key == gtk::gdk::Key::r && message_pane_shortcuts_available(&w) {
            clear_numeric_prefix(&numeric_prefix);
            *pending_response.borrow_mut() = true;
            w.response_menu_button.popup();
            w.status_label
                .set_text("Respond: r reply, a reply all, f forward, A forward attached");
            true
        } else if key == gtk::gdk::Key::c {
            clear_numeric_prefix(&numeric_prefix);
            open_compose(&opts, &w, &st);
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
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &opts.runtime_settings,
                    ),
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
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &opts.runtime_settings,
                    ),
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
        } else if shifted_shortcut_key(key, mods, gtk::gdk::Key::v, gtk::gdk::Key::V)
            && message_pane_shortcuts_available(&w)
        {
            clear_numeric_prefix(&numeric_prefix);
            *pending_view.borrow_mut() = true;
            w.view_menu_button.popup();
            w.status_label
                .set_text("View: t text, v visual HTML, h headers, r raw source, a sender default");
            true
        } else if key == gtk::gdk::Key::v {
            clear_numeric_prefix(&numeric_prefix);
            if st.borrow().active_pane == ActivePane::Threads {
                toggle_visual_select_mode(&w, &st);
                true
            } else {
                false
            }
        } else if key == gtk::gdk::Key::q && message_pane_shortcuts_available(&w) {
            clear_numeric_prefix(&numeric_prefix);
            toggle_quote_collapse(&opts, &w, &st);
            true
        } else if key == gtk::gdk::Key::y && message_pane_shortcuts_available(&w) {
            clear_numeric_prefix(&numeric_prefix);
            *pending_copy.borrow_mut() = true;
            w.copy_menu_button.popup();
            w.status_label
                .set_text("Copy: m message id, t thread id, f from, o to, c cc, s subject");
            true
        } else if key == gtk::gdk::Key::I && message_pane_shortcuts_available(&w) {
            clear_numeric_prefix(&numeric_prefix);
            activate_image_policy_button(&opts, &w, &st);
            true
        } else if key == gtk::gdk::Key::d {
            clear_numeric_prefix(&numeric_prefix);
            let visible = w.debug_view.is_visible();
            w.debug_view.set_visible(!visible);
            true
        } else if key == gtk::gdk::Key::comma {
            clear_numeric_prefix(&numeric_prefix);
            show_settings(&w, &st, &opts);
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
    let link_widgets = widgets.clone();
    let handler = Rc::new(move |key, modifiers| {
        if link_widgets.link_hints.handle_key(key, modifiers) {
            return gtk::glib::Propagation::Stop;
        }
        match normal_handler(key, modifiers) {
            gtk::glib::Propagation::Stop => gtk::glib::Propagation::Stop,
            gtk::glib::Propagation::Proceed => fallback_handler(key, modifiers),
        }
    });
    let router = MainShortcutRouter { handler };
    let controller = gtk::EventControllerKey::new();
    controller.set_name(Some(MAIN_SHORTCUT_CONTROLLER_NAME));
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let router_for_controller = router.clone();
    controller.connect_key_pressed(move |_, key, _, modifiers| {
        router_for_controller.handle_key(key, modifiers)
    });
    widgets.window.add_controller(controller);
    router
}

#[allow(clippy::too_many_arguments)]
fn connect_dropdown_sequence_keys(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    pending_response: Rc<RefCell<bool>>,
    pending_view: Rc<RefCell<bool>>,
    pending_copy: Rc<RefCell<bool>>,
    pending_message_tag: Rc<RefCell<bool>>,
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
        if !message_pane_shortcuts_available(&w) {
            *pending.borrow_mut() = false;
            w.response_menu_button.popdown();
            return gtk::glib::Propagation::Proceed;
        }
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
        if !message_pane_shortcuts_available(&w) {
            *pending.borrow_mut() = false;
            w.view_menu_button.popdown();
            return gtk::glib::Propagation::Proceed;
        }
        let handled = activate_message_view_sequence_key(&opts, &w, &st, key);
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
        if !message_pane_shortcuts_available(&w) {
            *pending.borrow_mut() = false;
            w.copy_menu_button.popdown();
            return gtk::glib::Propagation::Proceed;
        }
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
    let pending = pending_message_tag;
    controller.connect_key_pressed(move |_, key, _, mods| {
        if st.borrow().input_mode == InputMode::Insert {
            return gtk::glib::Propagation::Proceed;
        }
        if !message_pane_shortcuts_available(&w) || compose_view_is_visible(&w) {
            *pending.borrow_mut() = false;
            w.message_tag_menu_button.popdown();
            return gtk::glib::Propagation::Proceed;
        }
        if is_tag_menu_navigation_key(key) {
            return gtk::glib::Propagation::Proceed;
        }
        match activate_message_tag_sequence_key(&w, &st, key, mods) {
            MessageTagSequenceOutcome::CloseMenu => {
                *pending.borrow_mut() = false;
                w.message_tag_menu_button.popdown();
                gtk::glib::Propagation::Stop
            }
            MessageTagSequenceOutcome::KeepMenuOpen => {
                *pending.borrow_mut() = false;
                gtk::glib::Propagation::Stop
            }
            MessageTagSequenceOutcome::Unhandled => gtk::glib::Propagation::Proceed,
        }
    });
    widgets.message_tag_menu_box.add_controller(controller);

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w = widgets.clone();
    let st = state.clone();
    let pending = pending_tag;
    controller.connect_key_pressed(move |_, key, _, _| {
        if st.borrow().input_mode == InputMode::Insert {
            return gtk::glib::Propagation::Proceed;
        }
        if is_tag_menu_navigation_key(key) {
            return gtk::glib::Propagation::Proceed;
        }
        let handled = handle_tag_sequence_key(&w, &st, key);
        if handled {
            *pending.borrow_mut() = false;
            gtk::glib::Propagation::Stop
        } else {
            *pending.borrow_mut() = false;
            w.tag_menu_button.popdown();
            gtk::glib::Propagation::Stop
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
    let state_for_check = state.clone();
    let search = widgets.search_bar.clone();
    let pending_widgets = widgets.clone();
    let pending_state = state.clone();
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets.thread_list.connect_auto_load_more(
        move || {
            let state = state_for_check.borrow();
            (
                state.can_load_more_threads && !state.search_loading,
                search.current_generation(),
                state.thread_window_offset + state.thread_list_items.len(),
            )
        },
        move || widgets_set_pending_load_more(&pending_widgets, &pending_state),
        move || {
            load_more_threads(&opts, &w, &st, false);
        },
    );
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
    widgets.thread_list.set_load_more_state("Loading…", false);
}

fn selected_thread_index(widgets: &Widgets) -> Option<usize> {
    widgets.thread_list.selected_index()
}

fn select_thread_index_in_list(widgets: &Widgets, index: usize) {
    widgets.thread_list.select(index);
}

fn select_thread_index_for_open_message(widgets: &Widgets, index: usize) {
    widgets.thread_list.select_silently(index);
}

fn scroll_thread_index_into_view(widgets: &Widgets, index: usize) {
    widgets.thread_list.scroll_into_view(index);
}

fn find_widget_by_name(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    thread_list::find_widget_by_name(root, name)
}

fn thread_selection_view_state(widgets: &Widgets, state: &SharedState) -> serde_json::Value {
    widgets
        .thread_list
        .selection_view_state(state.borrow().thread_window_offset)
}

fn thread_row_layout_state(widgets: &Widgets, index: usize) -> serde_json::Value {
    widgets.thread_list.row_layout_state(index)
}

fn open_saved_search_name(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    name: &str,
) {
    let query = saved_search_query(name);
    widgets.search_bar.set_query(query);
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
        let next = target_abs - window_offset;
        if next >= widgets.thread_list.model_len() {
            return;
        }
        let already_selected = selected_thread_index(widgets) == Some(next);
        select_thread_index_in_list(widgets, next);
        if already_selected {
            select_thread_by_index(options, widgets, state, next, false);
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
            sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                &options.runtime_settings,
            ),
        }
    } else {
        TagMutation {
            add: vec!["unread".to_string()],
            remove: vec![],
            sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                &options.runtime_settings,
            ),
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
            sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                &options.runtime_settings,
            ),
        }
    } else {
        TagMutation {
            add: vec!["flagged".to_string()],
            remove: vec![],
            sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                &options.runtime_settings,
            ),
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
        excluded_tags: settings::excluded_tags(&options.runtime_settings),
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
    let suggestions = state.borrow().address_suggestions.clone();
    widgets
        .composer
        .update_address_suggestions_for_active(input, &suggestions);
}

fn complete_focused_recipient(widgets: &Widgets, state: &SharedState) -> bool {
    let suggestions = state.borrow().address_suggestions.clone();
    widgets.composer.complete_focused_recipient(&suggestions)
}

fn compose_fields(widgets: &Widgets, state: &SharedState) -> ComposeFields {
    let stored = state.borrow().compose_fields.clone();
    widgets.composer.read_fields(&stored)
}

fn record_compose_edit(state: &SharedState, fields: ComposeFields) {
    let mut state = state.borrow_mut();
    state.compose_fields = fields;
    state.compose_generation = state.compose_generation.saturating_add(1);
}

fn autosave_draft_from_widgets(widgets: &Widgets, state: &SharedState) {
    if widgets.composer.autosave_suppressed() {
        return;
    }
    let fields = compose_fields(widgets, state);
    record_compose_edit(state, fields.clone());
    update_attachment_label(widgets, &fields.attachments);
    persist_recovery_draft_from_ui(widgets, state, &fields);
    update_draft_action_buttons(widgets, state);
}

fn persist_recovery_draft_from_ui(
    widgets: &Widgets,
    state: &SharedState,
    fields: &ComposeFields,
) -> bool {
    let clearing_saved_copy = active_draft_matches_fields(state, fields);
    match reconcile_recovery_draft(widgets, state, fields) {
        Ok(()) => {
            let mut state = state.borrow_mut();
            if composer::clear_transient_autosave_error(&mut state.last_error) {
                widgets.status_label.set_text("Draft autosave recovered");
            }
            true
        }
        Err(err) => {
            let action = if clearing_saved_copy {
                "Saved draft recovery cleanup failed"
            } else {
                "Draft autosave failed"
            };
            report_draft_persistence_error(widgets, state, action, &err);
            false
        }
    }
}

fn active_draft_matches_fields(state: &SharedState, fields: &ComposeFields) -> bool {
    state
        .borrow()
        .active_draft
        .as_ref()
        .is_some_and(|active| active.saved_fields == *fields)
}

fn reconcile_recovery_draft(
    widgets: &Widgets,
    state: &SharedState,
    fields: &ComposeFields,
) -> anyhow::Result<()> {
    if active_draft_matches_fields(state, fields) {
        composer::clear_recovery_draft_files(
            widgets.composer.recovery_path(),
            widgets.composer.legacy_recovery_path(),
        )
    } else {
        composer::persist_recovery_draft(
            widgets.composer.recovery_path(),
            widgets.composer.legacy_recovery_path(),
            fields,
        )
    }
}

fn report_draft_persistence_error(
    widgets: &Widgets,
    state: &SharedState,
    action: &str,
    err: &anyhow::Error,
) {
    let message = format!("{action}: {err}");
    state.borrow_mut().last_error = Some(message.clone());
    widgets.status_label.set_text(&message);
    update_debug(widgets, state);
}

fn update_draft_action_buttons(widgets: &Widgets, state: &SharedState) {
    let (active_draft, background_activity, send_in_progress) = {
        let state = state.borrow();
        (
            state.active_draft.clone(),
            state.sync_in_progress || state.send_in_progress,
            state.send_in_progress,
        )
    };
    if let Some(active_draft) = active_draft {
        let current_fields = compose_fields(widgets, state);
        if current_fields == active_draft.saved_fields {
            widgets
                .composer
                .clear_draft_button()
                .set_label("Close draft");
        } else {
            widgets
                .composer
                .clear_draft_button()
                .set_label("Discard changes");
        }
        widgets
            .composer
            .delete_local_draft_button()
            .set_visible(true);
    } else {
        widgets
            .composer
            .clear_draft_button()
            .set_label("Discard draft");
        widgets
            .composer
            .delete_local_draft_button()
            .set_visible(false);
    }
    widgets
        .composer
        .save_draft_button()
        .set_sensitive(!background_activity);
    widgets
        .composer
        .clear_draft_button()
        .set_sensitive(!send_in_progress);
    widgets
        .composer
        .delete_local_draft_button()
        .set_sensitive(!background_activity);
    widgets
        .composer
        .delete_selected_draft_button()
        .set_sensitive(
            !background_activity && widgets.composer.draft_list().selected_row().is_some(),
        );
    widgets
        .composer
        .draft_list()
        .set_sensitive(!send_in_progress);
    update_button_binding_labels(widgets, state);
}

impl PendingTransition {
    fn operation(&self) -> UserOperation {
        match self {
            Self::ClearComposer => UserOperation::DraftClear,
            Self::ReplaceComposer(replacement) => match replacement.kind {
                ComposerReplacementKind::NamedDraft
                | ComposerReplacementKind::RecoveryDraft
                | ComposerReplacementKind::IndexedDraft => UserOperation::DraftLoad,
                _ => UserOperation::ComposeReplace,
            },
            Self::DeleteActiveDraft(_) | Self::DeleteNamedDraft(_) => UserOperation::DraftDelete,
            Self::SaveDraftReplacement { .. } => UserOperation::DraftSave,
            Self::SendComposer { .. } => UserOperation::Send,
            Self::ShowSelectedMessage { .. } | Self::CloseMainWindow => {
                UserOperation::ComposeReplace
            }
        }
    }

    fn rollback_preparation(
        &self,
        options: &LaunchOptions,
        widgets: &Widgets,
        state: &SharedState,
    ) {
        let restore = match self {
            Self::ReplaceComposer(replacement) => replacement.rejection_restore.clone(),
            Self::ShowSelectedMessage {
                rejection_restore, ..
            } => rejection_restore.clone(),
            _ => None,
        };
        if let Some(restore) = restore {
            apply_message_selection_snapshot(options, widgets, state, restore);
        }
    }

    fn into_confirmation_action(
        self,
        options: &LaunchOptions,
        widgets: &Widgets,
        state: &SharedState,
    ) -> PendingAction {
        let kind = match &self {
            Self::ClearComposer => PendingTransitionKind::ClearComposer,
            Self::ReplaceComposer(replacement) => {
                PendingTransitionKind::ReplaceComposer(replacement.kind)
            }
            Self::DeleteActiveDraft(_) => PendingTransitionKind::DeleteActiveDraft,
            Self::DeleteNamedDraft(_) => PendingTransitionKind::DeleteNamedDraft,
            Self::SaveDraftReplacement { .. } => PendingTransitionKind::SaveDraftReplacement,
            Self::SendComposer { .. } => PendingTransitionKind::SendComposer,
            Self::ShowSelectedMessage { .. } => PendingTransitionKind::ShowSelectedMessage,
            Self::CloseMainWindow => PendingTransitionKind::CloseMainWindow,
        };
        let transition = Rc::new(RefCell::new(Some(self)));
        let accept_transition = transition.clone();
        let opts = options.clone();
        let w = widgets.clone();
        let st = state.clone();
        let accept = move || {
            let Some(transition) = accept_transition.borrow_mut().take() else {
                return false;
            };
            execute_pending_action(&opts, &w, &st, transition)
        };
        let reject_transition = transition;
        let opts = options.clone();
        let w = widgets.clone();
        let st = state.clone();
        let reject = move || {
            if let Some(transition) = reject_transition.borrow_mut().take() {
                transition.rollback_preparation(&opts, &w, &st);
            }
        };
        let hooks = TransitionHooks::new(accept, reject);
        match kind {
            PendingTransitionKind::ClearComposer => PendingAction::ClearComposer(hooks),
            PendingTransitionKind::ReplaceComposer(kind) => {
                PendingAction::ReplaceComposer { kind, hooks }
            }
            PendingTransitionKind::DeleteActiveDraft => PendingAction::DeleteActiveDraft(hooks),
            PendingTransitionKind::DeleteNamedDraft => PendingAction::DeleteNamedDraft(hooks),
            PendingTransitionKind::SaveDraftReplacement => {
                PendingAction::SaveDraftReplacement(hooks)
            }
            PendingTransitionKind::SendComposer => PendingAction::SendComposer(hooks),
            PendingTransitionKind::ShowSelectedMessage => PendingAction::ShowSelectedMessage(hooks),
            PendingTransitionKind::CloseMainWindow => PendingAction::CloseMainWindow(hooks),
        }
    }
}

fn pending_operation(operation: PendingOperation) -> UserOperation {
    match operation {
        PendingOperation::DraftClear => UserOperation::DraftClear,
        PendingOperation::DraftLoad => UserOperation::DraftLoad,
        PendingOperation::ComposeReplace => UserOperation::ComposeReplace,
        PendingOperation::DraftDelete => UserOperation::DraftDelete,
        PendingOperation::DraftSave => UserOperation::DraftSave,
        PendingOperation::Send => UserOperation::Send,
    }
}

fn request_pending_action(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    action: PendingTransition,
) -> bool {
    if let Err(err) = ensure_user_operation_allowed(widgets, state, action.operation()) {
        action.rollback_preparation(options, widgets, state);
        widgets.status_label.set_text(&err.to_string());
        return false;
    }
    let fields = compose_fields(widgets, state);
    let active = state.borrow().active_draft.clone();
    let action = action.into_confirmation_action(options, widgets, state);
    let w = widgets.clone();
    let st = state.clone();
    let response_handler = Rc::new(move |id, accepted| {
        complete_pending_confirmation(&w, &st, id, accepted);
    });
    match widgets.composer.request_confirmation(
        &fields,
        active.as_ref(),
        action,
        confirmation_presenter(&widgets.window),
        response_handler,
    ) {
        Ok(composer::ConfirmationDisposition::Immediate(action)) => action.accept(),
        Ok(composer::ConfirmationDisposition::Pending { title, .. }) => {
            widgets
                .status_label
                .set_text(&format!("Confirmation required: {title}"));
            true
        }
        Err(action) => {
            action.reject();
            let message = "Another confirmation is already pending";
            widgets.status_label.set_text(message);
            state.borrow_mut().last_error = Some(message.to_string());
            update_debug(widgets, state);
            false
        }
    }
}

#[allow(deprecated)]
fn confirmation_presenter(window: &gtk::ApplicationWindow) -> composer::ConfirmationPresenter {
    let parent = window.downgrade();
    Rc::new(move |prompt, response_handler| {
        let Some(parent) = parent.upgrade() else {
            return gtk::glib::WeakRef::new();
        };
        let dialog = gtk::Dialog::builder()
            .title(prompt.title)
            .transient_for(&parent)
            .modal(true)
            .default_width(480)
            .build();
        dialog.set_widget_name("notm-confirmation-dialog");
        dialog.add_button("Cancel", gtk::ResponseType::Cancel);
        let confirm = dialog.add_button(prompt.confirm_label, gtk::ResponseType::Accept);
        confirm.add_css_class("destructive-action");
        dialog.set_default_response(gtk::ResponseType::Cancel);
        let area = dialog.content_area();
        area.set_spacing(8);
        area.set_margin_start(16);
        area.set_margin_end(16);
        area.set_margin_top(12);
        area.set_margin_bottom(12);
        let label = gtk::Label::new(Some(prompt.detail));
        label.set_xalign(0.0);
        label.set_wrap(true);
        area.append(&label);
        dialog.connect_response(move |dialog, response| {
            response_handler(prompt.id, response == gtk::ResponseType::Accept);
            dialog.destroy();
        });
        dialog.present();
        dialog.upcast::<gtk::Widget>().downgrade()
    })
}

fn complete_pending_confirmation(
    widgets: &Widgets,
    state: &SharedState,
    id: u64,
    accepted: bool,
) -> bool {
    let Some(action) = widgets.composer.take_confirmation_action(id) else {
        return false;
    };
    if !accepted {
        action.reject();
        widgets.status_label.set_text("Action cancelled");
        update_debug(widgets, state);
        widgets
            .composer
            .record_confirmation_completion(id, false, true);
        return true;
    }
    if let Err(err) =
        ensure_user_operation_allowed(widgets, state, pending_operation(action.operation()))
    {
        action.reject();
        let message = err.to_string();
        widgets.status_label.set_text(&message);
        {
            let mut state = state.borrow_mut();
            state.last_error = Some(message.clone());
            state.last_operation = Some(message);
        }
        update_debug(widgets, state);
        widgets
            .composer
            .record_confirmation_completion(id, true, false);
        return false;
    }
    let succeeded = action.accept();
    widgets
        .composer
        .record_confirmation_completion(id, true, succeeded);
    succeeded
}

fn execute_pending_action(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    action: PendingTransition,
) -> bool {
    match action {
        PendingTransition::ClearComposer => {
            clear_current_draft_immediately(options, widgets, state)
        }
        PendingTransition::ReplaceComposer(replacement) => {
            apply_prepared_composer_replacement(options, widgets, state, replacement)
        }
        PendingTransition::DeleteActiveDraft(draft) => {
            delete_captured_active_draft(options, widgets, state, draft)
        }
        PendingTransition::DeleteNamedDraft(path) => {
            delete_captured_named_draft(widgets, state, path)
        }
        PendingTransition::SaveDraftReplacement { fields, previous } => {
            match finish_captured_draft_save(options, widgets, state, fields, Some(previous)) {
                Ok(_) => true,
                Err(err) => {
                    report_draft_persistence_error(widgets, state, "Draft save failed", &err);
                    false
                }
            }
        }
        PendingTransition::SendComposer {
            fields,
            active,
            generation,
        } => match start_captured_send(options, widgets, state, fields, Some(active), generation) {
            Ok(()) => true,
            Err(err) => {
                let message = format!("Send failed to start: {err}");
                widgets.status_label.set_text(&message);
                let mut state = state.borrow_mut();
                state.last_error = Some(err.to_string());
                state.last_operation = Some(message);
                false
            }
        },
        PendingTransition::ShowSelectedMessage {
            selection,
            status,
            active_pane,
            clear_saved_recovery,
            ..
        } => {
            apply_message_selection_snapshot(options, widgets, state, selection);
            let recovery_error = if clear_saved_recovery {
                composer::clear_recovery_draft_files(
                    widgets.composer.recovery_path(),
                    widgets.composer.legacy_recovery_path(),
                )
                .err()
            } else {
                None
            };
            reset_composer_fields(widgets, state);
            show_preferred_selected_message_view(options, widgets, state);
            state.borrow_mut().active_pane = active_pane;
            focus_active_pane(widgets, state);
            if let Some(err) = recovery_error {
                let message = format!("{status}; saved-draft recovery cleanup failed: {err}");
                state.borrow_mut().last_error = Some(message.clone());
                widgets.status_label.set_text(&message);
            } else {
                widgets.status_label.set_text(&status);
            }
            true
        }
        PendingTransition::CloseMainWindow => {
            widgets.composer.allow_close_once();
            widgets.window.close();
            true
        }
    }
}

fn request_show_selected_message(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    active_pane: ActivePane,
    status: String,
    rejection_restore: Option<MessageSelectionSnapshot>,
) -> bool {
    let selection = capture_message_selection_snapshot(state);
    if state.borrow().send_in_progress {
        apply_message_selection_snapshot(options, widgets, state, selection);
        show_preferred_selected_message_view(options, widgets, state);
        state.borrow_mut().active_pane = active_pane;
        focus_active_pane(widgets, state);
        widgets.status_label.set_text(&status);
        return true;
    }
    let fields = compose_fields(widgets, state);
    let clear_saved_recovery = state
        .borrow()
        .active_draft
        .as_ref()
        .is_some_and(|active| fields == active.saved_fields);
    request_pending_action(
        options,
        widgets,
        state,
        PendingTransition::ShowSelectedMessage {
            selection,
            rejection_restore,
            status,
            active_pane,
            clear_saved_recovery,
        },
    )
}

fn capture_message_selection_snapshot(state: &SharedState) -> MessageSelectionSnapshot {
    let state = state.borrow();
    let selected_thread_index = state.selected_thread.as_ref().and_then(|selected| {
        state
            .thread_list_items
            .iter()
            .position(|thread| thread.thread_id == selected.thread_id)
    });
    MessageSelectionSnapshot {
        selected_thread: state.selected_thread.clone(),
        selected_thread_index,
        selected_message: state.selected_message.clone(),
        messages: state.messages.clone(),
        active_pane: state.active_pane,
        last_operation: state.last_operation.clone(),
        last_error: state.last_error.clone(),
    }
}

fn apply_message_selection_snapshot(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    selection: MessageSelectionSnapshot,
) {
    let selected_thread_index = selection.selected_thread_index;
    {
        let mut state = state.borrow_mut();
        state.selected_thread = selection.selected_thread;
        state.selected_message = selection.selected_message;
        state.messages = selection.messages;
        state.active_pane = selection.active_pane;
        state.last_operation = selection.last_operation;
        state.last_error = selection.last_error;
    }
    if let Some(index) = selected_thread_index {
        select_thread_index_for_open_message(widgets, index);
    } else {
        widgets.thread_list.clear_selection_silently();
    }
    refresh_thread_attachment_list(widgets, state);
    update_message_menu(options, widgets, state);
    update_custom_tag_controls(widgets, state);
    update_active_pane_visuals(widgets, state);
    update_message_action_buttons(options, widgets, state);
    update_debug(widgets, state);
}

fn apply_prepared_composer_replacement(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    replacement: PreparedComposerReplacement,
) -> bool {
    state.borrow_mut().last_error = None;
    if let Some(selection) = replacement.selection {
        apply_message_selection_snapshot(options, widgets, state, selection);
    }
    match replacement.payload {
        ComposerReplacementPayload::Empty => {
            let fields = ComposeFields {
                from: widgets.composer.sender_entry().text().to_string(),
                ..ComposeFields::default()
            };
            apply_compose_fields(widgets, state, fields);
            set_active_draft(widgets, state, None);
            show_compose_view(widgets);
        }
        ComposerReplacementPayload::Fields(fields) => {
            apply_compose_fields(widgets, state, *fields);
            set_active_draft(widgets, state, None);
            show_compose_view(widgets);
        }
        ComposerReplacementPayload::Message(message) => fill_composer(widgets, state, *message),
        ComposerReplacementPayload::Draft(draft) => {
            let PreparedDraftReplacement {
                mut fields,
                active_source,
                attachment_inputs,
            } = *draft;
            if !attachment_inputs.is_empty() {
                let attachments = match cache_composer_attachments(&attachment_inputs) {
                    Ok(attachments) => attachments,
                    Err(err) => {
                        report_draft_persistence_error(
                            widgets,
                            state,
                            "Draft attachment load failed",
                            &err,
                        );
                        return false;
                    }
                };
                fields.attachments = attachments;
            }
            let active_draft = active_source.map(|source| ActiveDraft {
                path: source.path,
                message_id: source.message_id,
                indexed: source.indexed,
                saved_fields: fields.clone(),
            });
            apply_compose_fields(widgets, state, fields);
            set_active_draft(widgets, state, active_draft);
            let fields = compose_fields(widgets, state);
            persist_recovery_draft_from_ui(widgets, state, &fields);
            show_compose_view(widgets);
        }
    }
    if state.borrow().last_error.is_some() {
        update_debug(widgets, state);
        return false;
    }
    if replacement.show_message_pane {
        set_pane_visibility(widgets, state, ActivePane::Message, true);
    }
    state.borrow_mut().active_pane = replacement.active_pane;
    if state.borrow().input_mode == InputMode::Insert {
        widgets.composer.to_entry().grab_focus();
    } else {
        focus_active_pane(widgets, state);
    }
    widgets.status_label.set_text(&replacement.status);
    if replacement.present_main_window {
        widgets.window.present();
    }
    if let Some(source_status) = replacement.source_status {
        source_status.set_text(&replacement.status);
    }
    update_debug(widgets, state);
    true
}

fn request_save_current_draft(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> anyhow::Result<Option<DraftSaveReport>> {
    ensure_user_operation_allowed(widgets, state, UserOperation::DraftSave)?;
    let fields = compose_fields(widgets, state);
    anyhow::ensure!(fields_has_content(&fields), "draft has no content");
    let previous_draft = state.borrow().active_draft.clone();
    if let Some(previous) = previous_draft.as_ref()
        && fields == previous.saved_fields
    {
        let recovery_cleanup_warning = reconcile_recovery_draft(widgets, state, &fields)
            .err()
            .map(|err| err.to_string());
        let report = DraftSaveReport {
            local_path: (!previous.indexed).then(|| previous.path.clone()),
            maildir_path: previous.indexed.then(|| previous.path.clone()),
            indexed_message_id: previous.message_id.clone(),
            replaced_path: None,
            recovery_cleanup_warning,
        };
        announce_draft_save(widgets, state, &report);
        return Ok(Some(report));
    }
    if let Some(previous) = previous_draft {
        let requested = request_pending_action(
            options,
            widgets,
            state,
            PendingTransition::SaveDraftReplacement { fields, previous },
        );
        anyhow::ensure!(
            requested,
            "draft replacement confirmation could not be opened"
        );
        return Ok(None);
    }
    finish_captured_draft_save(options, widgets, state, fields, None).map(Some)
}

fn finish_captured_draft_save(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    fields: ComposeFields,
    previous_draft: Option<ActiveDraft>,
) -> anyhow::Result<DraftSaveReport> {
    anyhow::ensure!(fields_has_content(&fields), "draft has no content");
    let persisted = if options.save_drafts_to_maildir {
        let message = composer::composed_message_from_fields(&fields)?;
        persist_draft_message(options, &message)?
    } else {
        None
    };
    let local_path = if persisted.is_none() {
        Some(composer::save_named_draft_fields(
            widgets.composer.drafts_dir(),
            &fields,
        )?)
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
    let active_draft = active_draft.ok_or_else(|| anyhow::anyhow!("draft was not persisted"))?;
    let replaced_path = if let Some(previous) = previous_draft
        && previous.path != active_draft.path
    {
        if let Err(delete_error) = delete_draft_source(options, &previous) {
            let rollback_error = delete_draft_source(options, &active_draft).err();
            let rollback_detail = rollback_error
                .map(|err| format!("; replacement cleanup also failed: {err}"))
                .unwrap_or_default();
            anyhow::bail!(
                "deleting previous saved draft {}: {delete_error}{rollback_detail}",
                previous.path.display()
            );
        }
        Some(previous.path)
    } else {
        None
    };
    set_active_draft(widgets, state, Some(active_draft));
    let recovery_cleanup_warning = reconcile_recovery_draft(widgets, state, &fields)
        .err()
        .map(|err| err.to_string());
    let report = DraftSaveReport {
        local_path,
        maildir_path: persisted.as_ref().map(|persisted| persisted.path.clone()),
        indexed_message_id: persisted.and_then(|persisted| persisted.indexed_message_id),
        replaced_path,
        recovery_cleanup_warning,
    };
    widgets.composer.refresh_draft_list();
    announce_draft_save(widgets, state, &report);
    if report.indexed_message_id.is_some() && report.recovery_cleanup_warning.is_none() {
        let current = state.borrow().current_query.clone();
        // Refresh indexed results without selecting a background message and
        // making a successful draft save look like the composer disappeared.
        schedule_search(options, widgets, state, &current, false, Duration::ZERO);
    }
    Ok(report)
}

fn announce_draft_save(widgets: &Widgets, state: &SharedState, report: &DraftSaveReport) {
    let destination = report
        .maildir_path
        .as_ref()
        .or(report.local_path.as_ref())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "draft store".to_string());
    let mut message = format!("Draft saved to {destination}");
    if let Some(warning) = &report.recovery_cleanup_warning {
        message.push_str(&format!("; recovery cleanup failed: {warning}"));
    }
    {
        let mut state = state.borrow_mut();
        state.last_error = report.recovery_cleanup_warning.clone();
        state.last_operation = Some(message.clone());
    }
    widgets.status_label.set_text(&message);
    update_debug(widgets, state);
}

fn set_active_draft(widgets: &Widgets, state: &SharedState, active_draft: Option<ActiveDraft>) {
    state.borrow_mut().active_draft = active_draft;
    update_draft_action_buttons(widgets, state);
}

fn delete_draft_source(options: &LaunchOptions, draft: &ActiveDraft) -> anyhow::Result<()> {
    if draft.indexed {
        {
            let db = Database::open(&open_config(options), DatabaseMode::ReadWrite)?;
            db.remove_message_file(&draft.path)?;
        }
        // Notmuch's committed revision can remain unchanged when a document is
        // deleted while other messages remain. Revision-keyed search pages
        // therefore need explicit invalidation before the post-delete refresh.
        thread_list::invalidate_search_caches();
    }
    if draft.path.exists() {
        std::fs::remove_file(&draft.path)?;
    }
    Ok(())
}

fn delete_active_draft_from_ui(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> bool {
    let Some(draft) = state.borrow().active_draft.clone() else {
        widgets
            .status_label
            .set_text("No saved local draft to delete");
        return false;
    };
    request_pending_action(
        options,
        widgets,
        state,
        PendingTransition::DeleteActiveDraft(draft),
    )
}

fn delete_captured_active_draft(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    draft: ActiveDraft,
) -> bool {
    match delete_draft_source(options, &draft) {
        Ok(()) => {
            if state.borrow().active_draft.as_ref() == Some(&draft) {
                detach_deleted_indexed_draft_selection(widgets, state, &draft);
                clear_draft_widgets(options, widgets, state);
                if let Err(err) = composer::clear_recovery_draft_files(
                    widgets.composer.recovery_path(),
                    widgets.composer.legacy_recovery_path(),
                ) {
                    report_draft_persistence_error(
                        widgets,
                        state,
                        "Draft recovery clear failed",
                        &err,
                    );
                    return false;
                }
            }
            let current = state.borrow().current_query.clone();
            if draft.indexed {
                show_thread_list_loading(widgets, "Reloading after draft deletion…");
            }
            run_search(options, widgets, state, &current);
            widgets.status_label.set_text(&format!(
                "Deleted local draft {}; reloading search…",
                draft.path.display()
            ));
            {
                let mut state = state.borrow_mut();
                state.last_error = None;
                state.last_operation =
                    Some(format!("deleted local draft {}", draft.path.display()));
            }
            true
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Delete local draft failed: {err}"));
            update_debug(widgets, state);
            false
        }
    }
}

fn detach_deleted_indexed_draft_selection(
    widgets: &Widgets,
    state: &SharedState,
    draft: &ActiveDraft,
) {
    if !draft.indexed {
        return;
    }
    let selected_is_deleted = state
        .borrow()
        .selected_message
        .as_ref()
        .is_some_and(|message| {
            draft
                .message_id
                .as_ref()
                .is_some_and(|message_id| message.message_id == *message_id)
                || message
                    .filenames
                    .iter()
                    .any(|filename| Path::new(filename) == draft.path)
        });
    if !selected_is_deleted {
        return;
    }
    let removed_thread = {
        let mut state = state.borrow_mut();
        state.messages.retain(|message| {
            !draft
                .message_id
                .as_ref()
                .is_some_and(|message_id| message.message_id == *message_id)
                && !message
                    .filenames
                    .iter()
                    .any(|filename| Path::new(filename) == draft.path)
        });
        state.selected_message = state.messages.last().cloned();
        if state.selected_message.is_some() {
            false
        } else {
            let selected_thread_id = state.selected_thread.take().map(|thread| thread.thread_id);
            if let Some(thread_id) = selected_thread_id {
                let before = state.thread_list_items.len();
                state
                    .thread_list_items
                    .retain(|thread| thread.thread_id != thread_id);
                state.thread_details.remove(&thread_id);
                state.multi_selected_threads.remove(&thread_id);
                state.visual_selected_threads.remove(&thread_id);
                state.thread_loaded_count = state.thread_list_items.len();
                if state.thread_list_items.len() < before {
                    state.thread_total_count = state.thread_total_count.saturating_sub(1);
                }
                state.can_load_more_threads = state.thread_window_offset
                    + state.thread_loaded_count
                    < state.thread_total_count as usize;
            }
            true
        }
    };
    if removed_thread {
        widgets.thread_list.clear_selection_silently();
        widgets
            .thread_list
            .apply_model_update(&thread_model_snapshot(state), ThreadModelUpdate::Replace);
        update_thread_result_label(widgets, state);
    }
}

fn migrate_legacy_named_drafts_from_ui(widgets: &Widgets, state: &SharedState) {
    let Some(legacy_dir) = widgets.composer.legacy_drafts_dir() else {
        return;
    };
    match composer::migrate_legacy_named_drafts(widgets.composer.drafts_dir(), legacy_dir) {
        Ok(0) => {}
        Ok(count) => {
            let message = format!("Migrated {count} legacy named draft(s) to persistent state");
            state.borrow_mut().last_operation = Some(message.clone());
            widgets.status_label.set_text(&message);
        }
        Err(err) => {
            report_draft_persistence_error(widgets, state, "Named draft migration failed", &err)
        }
    }
}

fn load_selected_named_draft(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> anyhow::Result<(bool, PathBuf)> {
    let (path, fields) = widgets.composer.selected_named_draft()?;
    let active_source = PreparedActiveDraft {
        path: path.clone(),
        message_id: None,
        indexed: false,
    };
    let requested = request_pending_action(
        options,
        widgets,
        state,
        PendingTransition::ReplaceComposer(PreparedComposerReplacement {
            kind: ComposerReplacementKind::NamedDraft,
            payload: ComposerReplacementPayload::Draft(Box::new(PreparedDraftReplacement {
                fields,
                active_source: Some(active_source),
                attachment_inputs: Vec::new(),
            })),
            selection: None,
            rejection_restore: None,
            status: format!("Loaded saved draft {}", path.display()),
            source_status: None,
            present_main_window: false,
            show_message_pane: false,
            active_pane: ActivePane::Message,
        }),
    );
    Ok((requested, path))
}

fn delete_selected_named_draft_from_ui(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> bool {
    let path = match widgets.composer.selected_named_draft() {
        Ok((path, _)) => path,
        Err(err) => {
            report_draft_persistence_error(widgets, state, "Saved draft delete failed", &err);
            return false;
        }
    };
    request_pending_action(
        options,
        widgets,
        state,
        PendingTransition::DeleteNamedDraft(path),
    )
}

fn delete_captured_named_draft(widgets: &Widgets, state: &SharedState, path: PathBuf) -> bool {
    match std::fs::remove_file(&path)
        .map_err(|err| anyhow::anyhow!("removing saved draft {}: {err}", path.display()))
    {
        Ok(()) => {
            if composer::active_draft_matches_path(state.borrow().active_draft.as_ref(), &path) {
                set_active_draft(widgets, state, None);
            }
            widgets.composer.refresh_draft_list();
            let message = format!("Deleted saved draft {}", path.display());
            {
                let mut state = state.borrow_mut();
                state.last_error = None;
                state.last_operation = Some(message.clone());
            }
            widgets.status_label.set_text(&message);
            update_debug(widgets, state);
            true
        }
        Err(err) => {
            report_draft_persistence_error(widgets, state, "Saved draft delete failed", &err);
            false
        }
    }
}

fn restore_draft_if_present(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> bool {
    let source_path = if widgets.composer.recovery_path().exists() {
        Some(widgets.composer.recovery_path())
    } else {
        widgets
            .composer
            .legacy_recovery_path()
            .filter(|legacy_path| legacy_path.exists())
    };
    let Some(source_path) = source_path else {
        return true;
    };
    let fields = match std::fs::read(source_path)
        .map_err(anyhow::Error::from)
        .and_then(|bytes| serde_json::from_slice::<ComposeFields>(&bytes).map_err(Into::into))
    {
        Ok(fields) => fields,
        Err(err) => {
            report_draft_persistence_error(widgets, state, "Draft recovery failed", &err);
            return false;
        }
    };
    if !fields_has_content(&fields) {
        return true;
    }
    let status = if source_path == widgets.composer.recovery_path() {
        format!("Recovered draft from {}", source_path.display())
    } else {
        format!(
            "Recovered draft from {} (migrated from legacy cache)",
            widgets.composer.recovery_path().display()
        )
    };
    request_pending_action(
        options,
        widgets,
        state,
        PendingTransition::ReplaceComposer(PreparedComposerReplacement {
            kind: ComposerReplacementKind::RecoveryDraft,
            payload: ComposerReplacementPayload::Draft(Box::new(PreparedDraftReplacement {
                fields,
                active_source: None,
                attachment_inputs: Vec::new(),
            })),
            selection: None,
            rejection_restore: None,
            status,
            source_status: None,
            present_main_window: false,
            show_message_pane: false,
            active_pane: ActivePane::Message,
        }),
    )
}

fn clear_current_draft_from_ui(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> bool {
    request_pending_action(options, widgets, state, PendingTransition::ClearComposer)
}

fn clear_current_draft_immediately(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> bool {
    if let Err(err) = composer::clear_recovery_draft_files(
        widgets.composer.recovery_path(),
        widgets.composer.legacy_recovery_path(),
    ) {
        report_draft_persistence_error(widgets, state, "Draft recovery clear failed", &err);
        return false;
    }
    clear_draft_widgets(options, widgets, state);
    widgets.status_label.set_text("Composer closed");
    state.borrow_mut().last_error = None;
    true
}

fn clear_draft_widgets(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let fields = ComposeFields {
        from: widgets.composer.sender_entry().text().to_string(),
        ..ComposeFields::default()
    };
    apply_compose_fields(widgets, state, fields);
    set_active_draft(widgets, state, None);
    widgets.composer.hide_address_suggestions();
    restore_message_view_after_compose(options, widgets, state);
}

fn restore_message_view_after_compose(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) {
    if state.borrow().selected_message.is_some() {
        let status = widgets.status_label.text().to_string();
        show_preferred_selected_message_view(options, widgets, state);
        widgets.status_label.set_text(&status);
    } else {
        widgets.message_stack.set_visible_child_name("text");
        widgets.message_view.buffer().set_text("");
        widgets.message_header_box.set_visible(false);
        widgets.attachments.hide();
        update_message_menu(options, widgets, state);
        update_message_action_buttons(options, widgets, state);
    }
}

fn apply_compose_fields(widgets: &Widgets, state: &SharedState, fields: ComposeFields) {
    widgets.composer.apply_fields(&fields);
    update_attachment_label(widgets, &fields.attachments);
    record_compose_edit(state, fields.clone());
    persist_recovery_draft_from_ui(widgets, state, &fields);
    update_draft_action_buttons(widgets, state);
}

fn move_compose_cursor_to_start(widgets: &Widgets) {
    widgets.composer.move_cursor_to_start();
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
    record_compose_edit(state, fields.clone());
    let saved = persist_recovery_draft_from_ui(widgets, state, &fields);
    update_draft_action_buttons(widgets, state);
    if saved {
        widgets.status_label.set_text("Attachment added to draft");
    }
}

fn update_attachment_label(widgets: &Widgets, attachments: &[String]) {
    attachments::set_compose_attachment_label(&widgets.composer.attachments_label(), attachments);
}

fn attachment_event_handler(widgets: &Widgets, state: &SharedState) -> AttachmentEventHandler {
    let status_label = widgets.status_label.downgrade();
    let debug_view = widgets.debug_view.downgrade();
    let state = Rc::downgrade(state);
    Rc::new(move |event| {
        let (Some(status_label), Some(debug_view), Some(state)) = (
            status_label.upgrade(),
            debug_view.upgrade(),
            state.upgrade(),
        ) else {
            return;
        };
        match event {
            AttachmentEvent::Completed(result) => {
                status_label.set_text(&result.status);
                record_attachment_action_result(
                    &mut state.borrow_mut(),
                    &result.message_id,
                    result.operation,
                );
                update_debug_view(&debug_view, &state);
            }
            AttachmentEvent::Failed { action, error } => {
                state.borrow_mut().last_error = Some(error.to_string());
                status_label.set_text(&format!("{action} failed: {error}"));
                update_debug_view(&debug_view, &state);
            }
        }
    })
}

fn apply_attachment_action_result(
    widgets: &Widgets,
    state: &SharedState,
    result: AttachmentActionResult,
) {
    widgets.status_label.set_text(&result.status);
    record_attachment_action_result(
        &mut state.borrow_mut(),
        &result.message_id,
        result.operation,
    );
    update_debug(widgets, state);
}

fn record_attachment_action_result(state: &mut UiState, message_id: &str, operation: String) {
    let current_message = state
        .messages
        .iter()
        .find(|message| message.message_id == message_id)
        .cloned()
        .or_else(|| {
            state
                .selected_message
                .as_ref()
                .filter(|message| message.message_id == message_id)
                .cloned()
        });
    if let Some(message) = current_message {
        state.selected_message = Some(message);
    }
    state.last_operation = Some(operation);
    state.last_error = None;
}

fn report_attachment_error(
    widgets: &Widgets,
    state: &SharedState,
    action: &str,
    error: &anyhow::Error,
) {
    state.borrow_mut().last_error = Some(error.to_string());
    widgets
        .status_label
        .set_text(&format!("{action} failed: {error}"));
    update_debug(widgets, state);
}

fn refresh_thread_attachment_list(widgets: &Widgets, state: &SharedState) {
    let messages = state.borrow().messages.clone();
    widgets
        .attachments
        .refresh(&messages, attachment_event_handler(widgets, state));
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
    let preference = {
        let state = state.borrow();
        state
            .selected_message
            .as_ref()
            .map(|message| message_view_preference(&state, message))
    };
    match preference.map(MessageViewKind::from_preference) {
        Some(MessageViewKind::Html) => show_visual_html_selected_message(options, widgets, state),
        Some(MessageViewKind::Headers) => show_full_headers(options, widgets, state),
        Some(MessageViewKind::Raw) => show_raw_source(options, widgets, state),
        Some(MessageViewKind::Text) | None => {
            show_selected_message_text_view(options, widgets, state)
        }
    }
}

fn show_selected_message_text_view(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) {
    match render_selected_message_text(widgets, state) {
        Ok(rendered) => {
            set_active_message_view(widgets, MessageViewKind::Text);
            show_text_message_view(options, widgets, state);
            widgets.message_view.set_monospace(false);
            widgets.message_view.buffer().set_text(&rendered);
            let index = selected_message_index(state)
                .map(|index| index + 1)
                .unwrap_or(1);
            let total = state.borrow().messages.len().max(1);
            widgets
                .status_label
                .set_text(&format!("Showing message {index} of {total}"));
            state.borrow_mut().last_error = None;
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
    render_message_text(&message, widgets.quote_collapse.get())
}

fn render_message_text(
    message: &notm_notmuch::MessageSummary,
    collapse_quotes: bool,
) -> anyhow::Result<String> {
    let mut rendered = String::new();
    if let Some(path) = message.filenames.first() {
        match parse_file(path) {
            Ok(parsed) => {
                rendered.push_str(&render_body_with_quote_collapse(
                    &parsed.safe_body,
                    collapse_quotes,
                ));
                if !parsed.decode_warnings.is_empty() {
                    if !rendered.is_empty() {
                        rendered.push_str("\n\n");
                    }
                    rendered.push_str("MIME decode warnings:\n");
                    for warning in &parsed.decode_warnings {
                        rendered.push_str(&format!("- {warning}\n"));
                    }
                }
                if !parsed.attachments.is_empty() {
                    rendered.push_str("\n\nAttachments:\n");
                    for att in &parsed.attachments {
                        let filename = att.filename.as_deref().unwrap_or("unnamed");
                        match &att.decode_error {
                            Some(error) => rendered.push_str(&format!(
                                "- {filename} ({}, decode failed: {error})\n",
                                att.content_type
                            )),
                            None if att.decode_warnings.is_empty() => rendered.push_str(&format!(
                                "- {filename} ({}, {} bytes)\n",
                                att.content_type, att.size
                            )),
                            None => rendered.push_str(&format!(
                                "- {filename} ({}, {} bytes; decoded with warning)\n",
                                att.content_type, att.size
                            )),
                        }
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
    clear_box(&widgets.message_header_box);
    let Some(message) = state.borrow().selected_message.clone() else {
        widgets.message_header_box.set_visible(false);
        widgets.message_header_box.set_tooltip_text(None);
        return;
    };
    let index = selected_message_index(state)
        .map(|index| index + 1)
        .unwrap_or(1);
    let total = state.borrow().messages.len().max(1);
    widgets.message_header_box.set_tooltip_text(Some(&format!(
        "Message-ID: {}\nFiles: {}",
        message.message_id,
        message.filenames.join(", ")
    )));

    let summary_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    summary_row.set_hexpand(true);
    let count = gtk::Label::new(Some(&format!("Message {index} of {total}")));
    count.add_css_class("notm-message-header-badge");
    count.set_xalign(0.0);
    summary_row.append(&count);
    widgets.message_header_box.append(&summary_row);

    let grid = gtk::Grid::new();
    grid.set_column_spacing(10);
    grid.set_row_spacing(4);
    grid.set_hexpand(true);
    let mut row = 0;
    append_message_header_field(&grid, &mut row, "Subject", &message.subject);
    append_message_header_field(&grid, &mut row, "Date", &format_message_date(message.date));
    append_message_header_field(&grid, &mut row, "From", &message.from);
    append_message_header_field(&grid, &mut row, "To", &message.to);
    if !message.cc.trim().is_empty() {
        append_message_header_field(&grid, &mut row, "Cc", &message.cc);
    }
    append_message_header_field(&grid, &mut row, "Tags", &message.tags.join(" "));
    widgets.message_header_box.append(&grid);
    widgets.message_header_box.set_visible(true);
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn append_message_header_field(grid: &gtk::Grid, row: &mut i32, key: &str, value: &str) {
    let key_label = gtk::Label::new(Some(key));
    key_label.add_css_class("notm-message-header-key");
    key_label.set_width_chars(10);
    key_label.set_xalign(1.0);
    key_label.set_valign(gtk::Align::Start);
    let value_label = gtk::Label::new(Some(non_empty_or(value, "—")));
    value_label.add_css_class("notm-message-header-value");
    value_label.set_xalign(0.0);
    value_label.set_wrap(true);
    value_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    value_label.set_lines(MESSAGE_HEADER_VALUE_LINES);
    value_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    value_label.set_max_width_chars(44);
    value_label.set_selectable(true);
    value_label.set_tooltip_text(Some(value));
    value_label.set_hexpand(true);
    grid.attach(&key_label, 0, *row, 1, 1);
    grid.attach(&value_label, 1, *row, 1, 1);
    *row += 1;
}

fn non_empty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

fn format_message_date(timestamp: i64) -> String {
    chrono::DateTime::<Utc>::from_timestamp(timestamp, 0)
        .map(|date| date.to_rfc2822())
        .unwrap_or_else(|| timestamp.to_string())
}

fn set_active_message_view(widgets: &Widgets, active: MessageViewKind) {
    widgets.active_message_view.set(active);
    if active != MessageViewKind::Html {
        widgets.link_hints.cancel_silent();
        widgets.html_view.stop_loading();
        set_html_image_loading(&widgets.html_view, false);
    }
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

fn update_sender_view_preference_button(widgets: &Widgets, state: &SharedState) {
    let Some(sender) = selected_sender_email(state) else {
        widgets.sender_view_preference_button.set_visible(false);
        widgets.sender_view_preference_button.set_tooltip_text(None);
        widgets
            .sender_view_preference_button
            .remove_css_class("suggested-action");
        return;
    };
    let preference = widgets.active_message_view.get().preference();
    let enabled = state
        .borrow()
        .sender_view_preferences
        .get(&sender)
        .is_some_and(|saved| *saved == preference);
    let label = format!("Always: {}", preference.label());
    set_button_label(
        &widgets.sender_view_preference_button,
        &label,
        visible_binding(message_pane_shortcuts_available(widgets), "V a"),
        state,
    );
    if enabled {
        widgets
            .sender_view_preference_button
            .add_css_class("suggested-action");
    } else {
        widgets
            .sender_view_preference_button
            .remove_css_class("suggested-action");
    }
    let action = if enabled {
        "Activate to remove this sender default."
    } else {
        "Activate to use this view by default for this sender."
    };
    widgets
        .sender_view_preference_button
        .set_tooltip_text(Some(&format!(
            "Sender: {sender}. {action} A per-message view choice still takes precedence."
        )));
    widgets.sender_view_preference_button.set_visible(true);
    widgets.sender_view_preference_button.set_sensitive(true);
}

fn toggle_text_visual_view(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> bool {
    if html_view_is_visible(widgets) {
        choose_selected_message_view(options, widgets, state, MessageViewKind::Text)
    } else {
        choose_selected_message_view(options, widgets, state, MessageViewKind::Html)
    }
}

fn choose_selected_message_view(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    view: MessageViewKind,
) -> bool {
    let Some(message) = state.borrow().selected_message.clone() else {
        widgets.status_label.set_text("No selected message");
        return false;
    };
    if view == MessageViewKind::Html && !message_has_html(&message) {
        widgets.status_label.set_text("No visual HTML part");
        return false;
    }
    let scroll = current_message_scroll_fraction(widgets);
    state.borrow_mut().last_error = None;
    match view {
        MessageViewKind::Text => show_selected_message_text_view(options, widgets, state),
        MessageViewKind::Html => show_visual_html_selected_message(options, widgets, state),
        MessageViewKind::Headers => show_full_headers(options, widgets, state),
        MessageViewKind::Raw => show_raw_source(options, widgets, state),
    }
    restore_message_scroll_fraction(widgets, scroll);
    if state.borrow().last_error.is_some() {
        return false;
    }
    if let Err(error) =
        remember_message_view_preference(options, state, &message.message_id, view.preference())
    {
        let status = format!(
            "{} shown, but its message preference could not be saved: {error}",
            view.preference().label()
        );
        widgets.status_label.set_text(&status);
        {
            let mut state_ref = state.borrow_mut();
            state_ref.last_error = Some(error.to_string());
            state_ref.last_operation = Some(status);
        }
        update_debug(widgets, state);
        return false;
    }
    update_sender_view_preference_button(widgets, state);
    update_debug(widgets, state);
    true
}

fn activate_image_policy_button(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    if settings::remote_images(&options.runtime_settings)
        || (html_view_is_visible(widgets) && html_view_images_allowed(widgets))
    {
        state.borrow_mut().last_error = None;
        update_message_action_buttons(options, widgets, state);
        return;
    }
    show_visual_html_with_image_policy(options, widgets, state, ImagePolicy::Once);
}

fn reject_persistent_sender_image_trust(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> serde_json::Value {
    let error = "Persistent sender image trust is unavailable because email From headers are not authenticated; use Load remote images once for the current message";
    widgets.status_label.set_text(error);
    {
        let mut state = state.borrow_mut();
        state.last_error = Some(error.to_string());
        state.last_operation = Some("rejected unsafe persistent sender image trust".to_string());
    }
    update_debug(widgets, state);
    json!({
        "ok": false,
        "error": error,
        "html_view": html_view_state(options, widgets, state),
    })
}

fn update_message_tag_controls(widgets: &Widgets, state: &SharedState) {
    let (selected, background_activity) = {
        let state = state.borrow();
        (
            state.selected_message.clone(),
            state.sync_in_progress || state.send_in_progress,
        )
    };
    let has_message = selected.is_some() && !compose_view_is_visible(widgets);
    let can_mutate = has_message && !background_activity;
    widgets.message_tag_menu_button.set_visible(has_message);
    widgets.message_tag_menu_button.set_sensitive(can_mutate);
    for button in [
        &widgets.message_archive_button,
        &widgets.message_read_toggle_button,
        &widgets.message_flag_toggle_button,
        &widgets.message_trash_button,
        &widgets.message_spam_button,
    ] {
        button.set_sensitive(can_mutate);
    }

    let selected_tags = selected
        .as_ref()
        .map(|message| message.tags.as_slice())
        .unwrap_or_default();
    let message_bindings = message_pane_shortcuts_available(widgets);
    set_button_label(
        &widgets.message_read_toggle_button,
        if selected_tags.iter().any(|tag| tag == "unread") {
            "Mark message read"
        } else {
            "Mark message unread"
        },
        visible_binding(message_bindings, "M u"),
        state,
    );
    set_button_label(
        &widgets.message_flag_toggle_button,
        if selected_tags.iter().any(|tag| tag == "flagged") {
            "Unflag message"
        } else {
            "Flag message"
        },
        visible_binding(message_bindings, "M f"),
        state,
    );

    let custom_tag = widgets.message_custom_tag_entry.text().trim().to_string();
    let has_custom_tag =
        !custom_tag.is_empty() && selected_tags.iter().any(|existing| existing == &custom_tag);
    widgets
        .message_custom_tag_action_label
        .set_text(if custom_tag.is_empty() {
            "Custom tag for current message"
        } else if has_custom_tag {
            "Current message has this tag"
        } else {
            "Current message does not have this tag"
        });
    set_button_label(
        &widgets.message_custom_tag_apply_button,
        if has_custom_tag {
            "Remove tag"
        } else {
            "Add tag"
        },
        visible_binding(message_bindings, "M T"),
        state,
    );
    widgets.message_custom_tag_entry.set_sensitive(can_mutate);
    widgets
        .message_custom_tag_apply_button
        .set_sensitive(can_mutate && !custom_tag.is_empty());
}

fn update_message_action_buttons(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let html_visible = html_view_is_visible(widgets);
    let has_html = selected_message_has_html(state);
    let (has_message, selected_thread, message_count, background_activity, send_in_progress) = {
        let state = state.borrow();
        (
            state.selected_message.is_some(),
            state.selected_thread.clone(),
            state.messages.len(),
            state.sync_in_progress || state.send_in_progress,
            state.send_in_progress,
        )
    };
    let has_thread = selected_thread.is_some();
    let multiple_messages = message_count > 1;
    if !has_message {
        widgets.message_header_box.set_visible(false);
    }
    widgets.message_menu_button.set_visible(multiple_messages);
    widgets
        .collapse_quotes_button
        .set_visible(multiple_messages);
    widgets
        .response_menu_button
        .set_sensitive(has_message && !send_in_progress);
    let tag_targets = tag_target_threads(state);
    let can_mutate_tags = !background_activity && !tag_targets.is_empty();
    for button in [
        &widgets.archive_button,
        &widgets.read_toggle_button,
        &widgets.flag_toggle_button,
        &widgets.trash_button,
        &widgets.spam_button,
        &widgets.single_tag_button,
        &widgets.tag_command_button,
    ] {
        button.set_sensitive(can_mutate_tags);
    }
    widgets.tag_menu_button.set_sensitive(can_mutate_tags);
    widgets
        .tag_command_apply_button
        .set_sensitive(can_mutate_tags);
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
    update_sender_view_preference_button(widgets, state);
    widgets
        .copy_menu_button
        .set_sensitive(has_message || has_thread);
    widgets.copy_message_id_button.set_sensitive(has_message);
    widgets.copy_thread_id_button.set_sensitive(has_thread);
    widgets.copy_from_email_button.set_sensitive(has_message);
    widgets.copy_to_email_button.set_sensitive(has_message);
    widgets.copy_cc_email_button.set_sensitive(has_message);
    widgets.copy_subject_button.set_sensitive(has_message);
    update_message_tag_controls(widgets, state);
    if html_visible && has_html {
        let image_policy = if html_view_images_allowed(widgets) {
            if settings::remote_images(&options.runtime_settings) {
                "remote images allowed for all messages by settings"
            } else {
                "remote images loaded once for this message"
            }
        } else {
            "remote content blocked"
        };
        widgets.html_policy_label.set_text(&format!(
            "Privacy-protected HTML: {image_policy}; message scripts and in-app navigation are blocked; links open externally (F shows link hints)."
        ));
    }

    if !has_html {
        widgets
            .image_policy_button
            .set_label("Load remote images once");
        widgets.image_policy_button.set_sensitive(false);
        update_button_binding_labels(widgets, state);
        return;
    }

    if settings::remote_images(&options.runtime_settings) {
        widgets
            .image_policy_button
            .set_label("Images allowed for all messages");
        widgets.image_policy_button.set_sensitive(false);
    } else if html_visible && html_view_images_allowed(widgets) {
        widgets
            .image_policy_button
            .set_label("Images loaded once for this message");
        widgets.image_policy_button.set_sensitive(false);
    } else {
        widgets
            .image_policy_button
            .set_label("Load remote images once");
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

fn start_link_hint_mode(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) -> bool {
    if !message_pane_shortcuts_available(widgets) {
        widgets
            .status_label
            .set_text("Show the message pane before opening link hints");
        return true;
    }
    if compose_view_is_visible(widgets) {
        widgets
            .status_label
            .set_text("Link hints are unavailable while composing");
        return true;
    }
    if !selected_message_has_html(state) {
        widgets
            .status_label
            .set_text("The selected message has no Visual HTML links");
        return true;
    }
    if !html_view_is_visible(widgets) {
        show_visual_html_selected_message(options, widgets, state);
        if state.borrow().last_error.is_some() {
            return true;
        }
    }
    widgets.link_hints.start();
    true
}

fn compose_view_is_visible(widgets: &Widgets) -> bool {
    widgets
        .message_stack
        .visible_child_name()
        .is_some_and(|name| name.as_str() == "compose")
}

fn html_view_images_allowed(widgets: &Widgets) -> bool {
    webkit_view_images_allowed(&widgets.html_view)
}

fn webkit_view_images_allowed(view: &webkit6::WebView) -> bool {
    WebViewExt::settings(view)
        .map(|settings| settings.is_auto_load_images())
        .unwrap_or(false)
}

fn selected_message_has_html(state: &SharedState) -> bool {
    state
        .borrow()
        .selected_message
        .as_ref()
        .is_some_and(message_has_html)
}

fn message_has_html(message: &notm_notmuch::MessageSummary) -> bool {
    message_filename(message)
        .and_then(parse_file)
        .ok()
        .and_then(|parsed| parsed.html_body)
        .is_some_and(|html| !html.trim().is_empty())
}

fn show_raw_source(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let scroll = current_message_scroll_fraction(widgets);
    let result = (|| -> anyhow::Result<String> {
        let filename = selected_message_filename(state)?;
        Ok(std::fs::read_to_string(filename)?)
    })();
    match result {
        Ok(raw) => {
            set_active_message_view(widgets, MessageViewKind::Raw);
            show_text_message_view(options, widgets, state);
            widgets.message_view.set_monospace(true);
            widgets.message_view.buffer().set_text(&raw);
            restore_message_scroll_fraction(widgets, scroll);
            widgets.status_label.set_text("Raw message source shown");
            let mut state = state.borrow_mut();
            state.last_operation = Some("showed raw source".to_string());
            state.last_error = None;
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
            set_active_message_view(widgets, MessageViewKind::Headers);
            show_text_message_view(options, widgets, state);
            widgets.message_view.set_monospace(true);
            widgets.message_view.buffer().set_text(&headers);
            restore_message_scroll_fraction(widgets, scroll);
            widgets.status_label.set_text("Full message headers shown");
            let mut state = state.borrow_mut();
            state.last_operation = Some("showed full headers".to_string());
            state.last_error = None;
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

fn message_filename(message: &notm_notmuch::MessageSummary) -> anyhow::Result<String> {
    message
        .filenames
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("message has no file"))
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

fn open_selected_draft_message(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    active_pane: ActivePane,
    status: String,
    rejection_restore: Option<MessageSelectionSnapshot>,
) -> anyhow::Result<bool> {
    let message = state
        .borrow()
        .selected_message
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no selected draft message"))?;
    let filename = message
        .filenames
        .first()
        .ok_or_else(|| anyhow::anyhow!("selected draft has no file"))?;
    let (fields, attachment_inputs) = composer::prepare_draft_fields_from_message_file(filename)?;
    let active_source = PreparedActiveDraft {
        path: PathBuf::from(filename),
        message_id: Some(message.message_id.clone()),
        indexed: true,
    };
    Ok(request_pending_action(
        options,
        widgets,
        state,
        PendingTransition::ReplaceComposer(PreparedComposerReplacement {
            kind: ComposerReplacementKind::IndexedDraft,
            payload: ComposerReplacementPayload::Draft(Box::new(PreparedDraftReplacement {
                fields,
                active_source: Some(active_source),
                attachment_inputs,
            })),
            selection: Some(capture_message_selection_snapshot(state)),
            rejection_restore,
            status,
            source_status: None,
            present_main_window: false,
            show_message_pane: false,
            active_pane,
        }),
    ))
}

fn show_text_message_view(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    widgets.message_stack.set_visible_child_name("text");
    update_message_header(widgets, state);
    refresh_thread_attachment_list(widgets, state);
    update_message_action_buttons(options, widgets, state);
}

fn show_compose_view(widgets: &Widgets) {
    widgets.composer.hide_address_suggestions();
    widgets.html_policy_row.set_visible(false);
    widgets.message_header_box.set_visible(false);
    widgets.message_tag_menu_button.set_visible(false);
    widgets.attachments.hide();
    widgets.message_stack.set_visible_child_name("compose");
}

fn configure_status_label(label: &gtk::Label) {
    label.set_hexpand(true);
    label.set_single_line_mode(true);
    label.set_width_chars(1);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(STATUS_BAR_MAX_WIDTH_CHARS);
}

fn new_privacy_html_webview() -> webkit6::WebView {
    let network_session = webkit6::NetworkSession::new_ephemeral();
    if let Some(cookie_manager) = network_session.cookie_manager() {
        cookie_manager.set_accept_policy(webkit6::CookieAcceptPolicy::Never);
    }
    webkit6::WebView::builder()
        .network_session(&network_session)
        .default_content_security_policy(HTML_DEFAULT_CONTENT_SECURITY_POLICY)
        .build()
}

fn configure_html_webview(view: &webkit6::WebView, allow_remote_images: bool) {
    if let Some(settings) = WebViewExt::settings(view) {
        settings.set_enable_javascript(true);
        settings.set_enable_javascript_markup(false);
        settings.set_enable_developer_extras(false);
        settings.set_enable_dns_prefetching(false);
        settings.set_enable_hyperlink_auditing(false);
        settings.set_load_icons_ignoring_image_load_setting(false);
        settings.set_allow_file_access_from_file_urls(false);
        settings.set_allow_universal_access_from_file_urls(false);
        settings.set_auto_load_images(allow_remote_images);
    }
    view.load_html(
        &visual_html_document(
            "<p class=\"notm-empty-html\">Open an HTML message and choose Visual HTML.</p>",
            false,
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

fn connect_html_hover_status(view: &webkit6::WebView, status_label: &gtk::Label) {
    let status = status_label.clone();
    let previous_status = Rc::new(RefCell::new(None::<String>));
    view.connect_mouse_target_changed(move |_, hit_test, _| {
        if let Some(uri) = html_hover_link_uri(hit_test) {
            if previous_status.borrow().is_none() && !status.text().as_str().starts_with("Link: ") {
                *previous_status.borrow_mut() = Some(status.text().to_string());
            }
            status.set_text(&html_link_hover_status(&uri));
            status.set_tooltip_text(Some(&uri));
        } else {
            let previous = previous_status.borrow_mut().take();
            if status.text().as_str().starts_with("Link: ") {
                status.set_text(previous.as_deref().unwrap_or("Ready"));
            }
            status.set_tooltip_text(None);
        }
    });
}

fn html_hover_link_uri(hit_test: &webkit6::HitTestResult) -> Option<String> {
    if !hit_test.context_is_link() {
        return None;
    }
    hit_test
        .link_uri()
        .map(|uri| uri.to_string())
        .filter(|uri| !uri.is_empty())
}

fn open_html_link_externally(uri: &str, status_label: &gtk::Label) {
    if !html_link_scheme_is_external_safe(uri) {
        status_label.set_text(&html_link_blocked_status(uri));
        return;
    }
    match gtk::gio::AppInfo::launch_default_for_uri(uri, None::<&gtk::gio::AppLaunchContext>) {
        Ok(()) => status_label.set_text(&html_link_opened_status(uri)),
        Err(err) => status_label.set_text(&html_link_failed_status(uri, &err.to_string())),
    }
}

fn html_link_opened_status(uri: &str) -> String {
    format!("Opened link externally: {}", html_link_status_uri(uri))
}

fn html_link_hover_status(uri: &str) -> String {
    format!("Link: {}", html_link_status_uri(uri))
}

fn html_link_blocked_status(uri: &str) -> String {
    format!(
        "Blocked unsupported HTML link target: {}",
        html_link_status_uri(uri)
    )
}

fn html_link_failed_status(uri: &str, error: &str) -> String {
    format!(
        "Open link failed: {error}; target: {}",
        html_link_status_uri(uri)
    )
}

fn html_link_status_uri(uri: &str) -> String {
    truncate_status_text(uri, HTML_LINK_STATUS_URI_MAX_CHARS)
}

fn navigation_decision_uri(decision: &webkit6::PolicyDecision) -> Option<String> {
    let navigation = decision.downcast_ref::<NavigationPolicyDecision>()?;
    let action = navigation.navigation_action()?;
    let request = action.request()?;
    request.uri().map(|uri| uri.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImagePolicy {
    Config,
    Once,
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
    let result = {
        let message = state.borrow().selected_message.clone();
        (|| -> anyhow::Result<(String, String, bool, usize)> {
            let message = message.ok_or_else(|| anyhow::anyhow!("no selected message"))?;
            render_visual_html_for_message(options, &message, image_policy)
        })()
    };
    match result {
        Ok((document, original_html, allow_remote_images, decode_warning_count)) => {
            widgets.html_view.stop_loading();
            set_html_image_loading(&widgets.html_view, allow_remote_images);
            widgets
                .html_load_generation
                .set(widgets.html_load_generation.get().saturating_add(1));
            widgets.html_view.load_html(&document, Some("about:blank"));
            widgets.message_stack.set_visible_child_name("html");
            update_message_header(widgets, state);
            set_active_message_view(widgets, MessageViewKind::Html);
            widgets.status_label.set_text(&html_status_text(
                image_policy,
                allow_remote_images,
                decode_warning_count,
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

fn render_visual_html_for_message(
    options: &LaunchOptions,
    message: &notm_notmuch::MessageSummary,
    image_policy: ImagePolicy,
) -> anyhow::Result<(String, String, bool, usize)> {
    let allow_remote_images = match image_policy {
        ImagePolicy::Config => settings::remote_images(&options.runtime_settings),
        ImagePolicy::Once => true,
    };
    let filename = message_filename(message)?;
    let parsed = parse_file(filename)?;
    let decode_warning_count = parsed.decode_warnings.len();
    let html = parsed
        .html_body
        .ok_or_else(|| anyhow::anyhow!("selected message has no HTML body"))?;
    let sanitized = sanitize_html_for_visual(&html, allow_remote_images);
    Ok((
        visual_html_document(&sanitized, allow_remote_images),
        html,
        allow_remote_images,
        decode_warning_count,
    ))
}

fn set_html_image_loading(view: &webkit6::WebView, allow_remote_images: bool) {
    if let Some(settings) = WebViewExt::settings(view) {
        settings.set_auto_load_images(allow_remote_images);
    }
}

fn html_status_text(
    policy: ImagePolicy,
    allow_remote_images: bool,
    decode_warning_count: usize,
) -> String {
    let status = match policy {
        ImagePolicy::Once if allow_remote_images => {
            "Visual HTML rendered; remote images loaded once for this message".to_string()
        }
        ImagePolicy::Config if allow_remote_images => {
            "Visual HTML rendered; remote images allowed for all messages by settings".to_string()
        }
        _ => "Visual HTML rendered; remote content and message scripts blocked".to_string(),
    };
    if decode_warning_count == 0 {
        status
    } else {
        format!(
            "{status}; {decode_warning_count} MIME decode warning{}",
            if decode_warning_count == 1 { "" } else { "s" }
        )
    }
}

fn selected_sender_email(state: &SharedState) -> Option<String> {
    state
        .borrow()
        .selected_message
        .as_ref()
        .and_then(message_sender_email)
}

fn message_sender_email(message: &notm_notmuch::MessageSummary) -> Option<String> {
    sender_email_from_header(&message.from)
}

fn sender_email_from_header(value: &str) -> Option<String> {
    parse_address_list(value)
        .into_iter()
        .next()
        .map(|address| normalize_sender(&address.email))
}

fn normalize_message_id(message_id: &str) -> String {
    message_id.trim().to_string()
}

fn normalize_message_view_preferences(
    preferences: &BTreeMap<String, MessageViewPreference>,
) -> BTreeMap<String, MessageViewPreference> {
    preferences
        .iter()
        .filter_map(|(message_id, preference)| {
            let message_id = normalize_message_id(message_id);
            (!message_id.is_empty()).then_some((message_id, *preference))
        })
        .collect()
}

fn normalize_sender_view_preferences(
    preferences: &BTreeMap<String, MessageViewPreference>,
) -> BTreeMap<String, MessageViewPreference> {
    preferences
        .iter()
        .filter_map(|(sender, preference)| {
            let sender = normalize_sender(sender);
            (!sender.is_empty()).then_some((sender, *preference))
        })
        .collect()
}

fn resolve_message_view_preference(
    prefer_html_view: bool,
    message_preferences: &BTreeMap<String, MessageViewPreference>,
    sender_preferences: &BTreeMap<String, MessageViewPreference>,
    message_id: &str,
    sender: Option<&str>,
    has_html: bool,
) -> MessageViewPreference {
    let message_id = normalize_message_id(message_id);
    let sender = sender.map(normalize_sender);
    let preference = message_preferences
        .get(&message_id)
        .copied()
        .or_else(|| {
            sender
                .as_deref()
                .and_then(|sender| sender_preferences.get(sender).copied())
        })
        .unwrap_or(if prefer_html_view {
            MessageViewPreference::VisualHtml
        } else {
            MessageViewPreference::Text
        });
    if preference == MessageViewPreference::VisualHtml && !has_html {
        MessageViewPreference::Text
    } else {
        preference
    }
}

fn message_view_preference(
    state: &UiState,
    message: &notm_notmuch::MessageSummary,
) -> MessageViewPreference {
    resolve_message_view_preference(
        state.prefer_html_view,
        &state.message_view_preferences,
        &state.sender_view_preferences,
        &message.message_id,
        message_sender_email(message).as_deref(),
        message_has_html(message),
    )
}

fn normalize_sender(sender: &str) -> String {
    sender.trim().to_ascii_lowercase()
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

fn visual_html_document(body: &str, allow_remote_images: bool) -> String {
    let image_sources = if allow_remote_images {
        "http: https:"
    } else {
        "'none'"
    };
    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src {image_sources}; style-src 'unsafe-inline'; script-src 'none'; connect-src 'none'; frame-src 'none'; font-src 'none'; media-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'">
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
    let (has_html, html_len, decode_warning_count, error) =
        match selected_message_filename(state).and_then(parse_file) {
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
                parsed.decode_warnings.len(),
                None,
            ),
            Err(err) => (false, 0, 0, Some(err.to_string())),
        };
    let sender_email = selected_sender_email(state);
    let global_remote_images_allowed = settings::remote_images(&options.runtime_settings);
    let image_loading_allowed = WebViewExt::settings(&widgets.html_view)
        .map(|settings| settings.is_auto_load_images())
        .unwrap_or(false);
    let image_permission = if global_remote_images_allowed {
        "all_messages"
    } else if visible_child == "html" && image_loading_allowed {
        "message_once"
    } else {
        "blocked"
    };
    let network_session_ephemeral = widgets
        .html_view
        .network_session()
        .is_some_and(|session| session.is_ephemeral());
    json!({
        "ok": error.is_none(),
        "visible_child": visible_child,
        "html_visible": visible_child == "html",
        "has_html": has_html,
        "html_bytes": html_len,
        "decode_warning_count": decode_warning_count,
        "status_text": widgets.status_label.text().to_string(),
        "loading": widgets.html_view.is_loading(),
        "load_generation": widgets.html_load_generation.get(),
        "completed_load_generation": widgets.html_completed_load_generation.get(),
        "global_remote_images_allowed": global_remote_images_allowed,
        "sender_email": sender_email,
        "sender_identity_authenticated": false,
        "image_permission": image_permission,
        "image_loading_allowed": image_loading_allowed,
        "remote_images_allowed": image_loading_allowed,
        "network_session_ephemeral": network_session_ephemeral,
        "default_content_security_policy": widgets
            .html_view
            .default_content_security_policy()
            .map(|policy| policy.to_string()),
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
        let query = w.search_bar.entry().text().to_string();
        run_search(&opts, &w, &st, &query);
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets.thread_list.connect_load_more(move || {
        load_more_threads(&opts, &w, &st, true);
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    widgets.search_bar.entry().connect_activate(move |entry| {
        run_search(&opts, &w, &st, &entry.text());
    });

    let w = widgets.clone();
    let st = state.clone();
    let opts = options.clone();
    widgets.thread_list.connect_activate(move |index| {
        open_thread_by_index(&opts, &w, &st, index);
    });

    let w = widgets.clone();
    let st = state.clone();
    let opts = options.clone();
    widgets
        .thread_list
        .connect_selection_changed(move |selected| {
            if let Some(index) = selected {
                select_thread_by_index(&opts, &w, &st, index, false);
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
    undo_last_button.connect_clicked(move |_| {
        undo_last_tag(&opts, &w, &st, &undo);
        w.undo_tag_button.popdown();
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    undo_list_button.connect_clicked(move |_| {
        show_undo_tag_actions(&opts, &w, &st, &undo);
        w.undo_tag_button.popdown();
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    compose_button.connect_clicked(move |_| {
        let _ = open_compose(&opts, &w, &st);
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    reply_button.connect_clicked(move |_| {
        reply_selected(&opts, &w, &st, ReplyKind::Sender);
        w.response_menu_button.popdown();
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    reply_all_button.connect_clicked(move |_| {
        reply_selected(&opts, &w, &st, ReplyKind::All);
        w.response_menu_button.popdown();
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    forward_button.connect_clicked(move |_| {
        forward_selected(&opts, &w, &st);
        w.response_menu_button.popdown();
    });

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    forward_attachment_button.connect_clicked(move |_| {
        forward_as_attachment_selected(&opts, &w, &st);
        w.response_menu_button.popdown();
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
    let st = state.clone();
    let opts = options.clone();
    settings_button.connect_clicked(move |_| show_settings(&w, &st, &opts));

    let w = widgets.clone();
    help_button.connect_clicked(move |_| show_shortcuts_overlay(&w));

    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    send_button.connect_clicked(move |_| {
        let _ = send_compose(&opts, &w, &st);
    });
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
            sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                &opts.runtime_settings,
            ),
        };
        tag_selected(&opts, &w, &st, &undo, mutation);
    });
}

fn search_page_coordinator(options: &LaunchOptions) -> SearchPageCoordinator {
    let runtime_settings = options.runtime_settings.clone();
    SearchPageCoordinator::new(
        open_config(options),
        Arc::new(move || {
            let runtime = settings::snapshot(&runtime_settings);
            SearchRuntimeSnapshot {
                page_size: runtime.page_size,
                excluded_tags: runtime.excluded_tags,
            }
        }),
    )
}

fn thread_paging_snapshot(state: &SharedState) -> ThreadPagingSnapshot {
    let state = state.borrow();
    ThreadPagingSnapshot {
        search_loading: state.search_loading,
        current_query: state.current_query.clone(),
        window_offset: state.thread_window_offset,
        loaded_count: state.thread_list_items.len(),
        can_load_more: state.can_load_more_threads,
    }
}

fn thread_search_state_snapshot(
    widgets: &Widgets,
    state: &SharedState,
) -> ThreadSearchStateSnapshot {
    let state = state.borrow();
    ThreadSearchStateSnapshot {
        window_offset: state.thread_window_offset,
        threads: state.thread_list_items.clone(),
        details: state.thread_details.clone(),
        selected_thread_id: state
            .selected_thread
            .as_ref()
            .map(|thread| thread.thread_id.clone()),
        selected_index: selected_thread_index(widgets),
    }
}

fn apply_thread_search_state_update(state: &SharedState, update: ThreadSearchStateUpdate) {
    let mut state = state.borrow_mut();
    state.current_query = update.current_query;
    state.thread_window_offset = update.window_offset;
    state.thread_list_items = update.threads;
    state.thread_total_count = update.total_count;
    state.thread_loaded_count = update.loaded_count;
    state.thread_page_size = update.page_size;
    state.can_load_more_threads = update.can_load_more;
    state.thread_details = update.details;
    state.visible_tags = update.visible_tags;
    state.database_path = Some(update.database_path);
    state.database_revision = Some(update.revision);
    state.last_operation = Some(update.operation);
    state.last_error = None;
    state.search_error = None;
}

fn accept_search_page_response(
    widgets: &Widgets,
    state: &SharedState,
    response: &SearchPageResponse,
) -> bool {
    widgets.search_bar.current_generation() == response.generation
        && finish_search_activity(&mut state.borrow_mut(), response.generation)
}

fn run_search(options: &LaunchOptions, widgets: &Widgets, state: &SharedState, query: &str) -> u64 {
    schedule_search(options, widgets, state, query, true, Duration::ZERO)
}

fn schedule_search(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    query: &str,
    select_first: bool,
    worker_delay: Duration,
) -> u64 {
    let generation = reserve_search_generation(widgets);
    prepare_search_activity(widgets, state, generation, query);
    start_full_search(
        options,
        widgets,
        state,
        SearchWorkerRequest {
            query: query.to_string(),
            generation,
            select_first,
            delay: worker_delay,
        },
    );
    generation
}

fn reserve_search_generation(widgets: &Widgets) -> u64 {
    let generation = widgets.search_bar.current_generation().saturating_add(1);
    widgets.search_bar.set_generation(generation);
    generation
}

fn prepare_search_activity(widgets: &Widgets, state: &SharedState, generation: u64, query: &str) {
    widgets.search_bar.set_requested_query(query);
    prepare_search_activity_preserving_request(widgets, state, generation, query);
}

fn prepare_search_activity_preserving_request(
    widgets: &Widgets,
    state: &SharedState,
    generation: u64,
    query: &str,
) {
    begin_search_activity(&mut state.borrow_mut(), generation, query);
    widgets
        .status_label
        .set_text(&format!("Loading search `{query}`…"));
    widgets.thread_list.set_result_label("Loading search…");
    widgets.thread_list.set_load_more_sensitive(false);
}

fn start_full_search(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    request: SearchWorkerRequest,
) {
    let coordinator = search_page_coordinator(options);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    coordinator.launch(
        SearchPageRequest {
            query: request.query,
            generation: request.generation,
            offset: 0,
            select_first: request.select_first,
            delay: request.delay,
        },
        "search cancelled",
        move |response| {
            if accept_search_page_response(&w, &st, &response) {
                match response.result {
                    Ok(data) => finish_replaced_search(
                        &opts,
                        &w,
                        &st,
                        thread_list::reduce_replace_search(data),
                        response.select_first,
                    ),
                    Err(err) => {
                        let has_threads = !st.borrow().thread_list_items.is_empty();
                        finish_search_error(
                            &w,
                            &st,
                            thread_list::reduce_search_error(err, has_threads),
                        );
                    }
                }
                record_full_search_outcome(&st, response.generation);
            }
        },
    );
}

fn record_full_search_outcome(state: &SharedState, generation: u64) {
    let mut state = state.borrow_mut();
    state.full_search_outcome_generation = generation;
    state.full_search_outcome_error = state.search_error.clone();
}

fn load_more_threads(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    select_last_loaded: bool,
) -> bool {
    let (query, offset) = match thread_list::plan_load_more(&thread_paging_snapshot(state)) {
        LoadMoreDecision::Busy => return false,
        LoadMoreDecision::Exhausted => {
            widgets
                .status_label
                .set_text("All currently counted threads are already loaded");
            return false;
        }
        LoadMoreDecision::Ready { query, offset } => (query, offset),
    };
    set_thread_loading_indicator(
        widgets,
        &format!("Loading more messages from {}…", format_count(offset + 1)),
    );
    let generation = reserve_search_generation(widgets);
    begin_search_activity(&mut state.borrow_mut(), generation, &query);
    widgets.thread_list.set_result_label("Loading more…");
    let request = SearchPageRequest {
        query,
        generation,
        offset,
        select_first: false,
        delay: Duration::ZERO,
    };
    let coordinator = search_page_coordinator(options);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    coordinator.launch(request, "thread page load cancelled", move |response| {
        if accept_search_page_response(&w, &st, &response) {
            match response.result {
                Ok(data) => {
                    let snapshot = thread_search_state_snapshot(&w, &st);
                    finish_appended_search(
                        &opts,
                        &w,
                        &st,
                        thread_list::reduce_append_search(snapshot, data, select_last_loaded),
                    );
                }
                Err(err) => {
                    let has_threads = !st.borrow().thread_list_items.is_empty();
                    finish_search_error(
                        &w,
                        &st,
                        thread_list::reduce_search_error(err, has_threads),
                    );
                }
            }
        }
    });
    true
}

fn finish_replaced_search(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    outcome: ReplaceSearchOutcome,
    select_first: bool,
) {
    let query = outcome.update.current_query.clone();
    let cached = outcome.cached;
    let preserve_search_focus = widgets.search_bar.has_focus();
    apply_thread_search_state_update(state, outcome.update);
    {
        let mut state = state.borrow_mut();
        state.selected_thread = None;
        state.selected_message = None;
        state.messages.clear();
        state.visual_select_mode = false;
        state.visual_select_anchor = None;
        state.visual_select_cursor = None;
        state.visual_selected_threads.clear();
        state.visual_selection_pending_range = None;
        state.multi_selected_threads.clear();
    }
    widgets
        .thread_list
        .apply_model_update(&thread_model_snapshot(state), ThreadModelUpdate::Replace);
    update_tag_searches(options, widgets, state);

    let pending_open_message_id = { state.borrow().pending_open_message_id.clone() };
    if let Some(message_id) = pending_open_message_id {
        let has_loaded_threads = !state.borrow().thread_list_items.is_empty();
        if has_loaded_threads && open_loaded_thread_at_message(options, widgets, state, &message_id)
        {
            state.borrow_mut().pending_open_message_id = None;
            update_thread_result_label(widgets, state);
            update_debug(widgets, state);
            return;
        }

        let fallback_query = message_id_query(&message_id);
        if query != fallback_query {
            widgets.status_label.set_text(&format!(
                "Message id not in loaded startup results: {message_id}; opening direct match"
            ));
            widgets.search_bar.set_query(&fallback_query);
            run_search(options, widgets, state, &fallback_query);
            return;
        }

        state.borrow_mut().pending_open_message_id = None;
        widgets
            .status_label
            .set_text(&format!("Message id not found: {message_id}"));
        refresh_thread_attachment_list(widgets, state);
        update_message_menu(options, widgets, state);
        update_thread_result_label(widgets, state);
        update_debug(widgets, state);
        return;
    }

    if select_first && !state.borrow().thread_list_items.is_empty() {
        select_thread_index_clamped(options, widgets, state, 0);
    } else {
        refresh_thread_attachment_list(widgets, state);
        update_message_menu(options, widgets, state);
        widgets.status_label.set_text(&format!(
            "{} for {}{}",
            thread_window_status(state),
            query,
            if cached { " (cached)" } else { "" }
        ));
    }
    update_thread_result_label(widgets, state);
    if !preserve_search_focus && state.borrow().input_mode == InputMode::Normal {
        focus_active_pane(widgets, state);
    }
    update_debug(widgets, state);
}

fn finish_appended_search(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    outcome: AppendSearchOutcome,
) {
    let model_update = outcome.model_update;
    let selected_index = outcome.selected_index;
    apply_thread_search_state_update(state, outcome.update);
    widgets
        .thread_list
        .apply_model_update(&thread_model_snapshot(state), model_update);
    update_tag_searches(options, widgets, state);
    if let Some(index) = selected_index {
        select_thread_index_clamped(options, widgets, state, index);
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

fn finish_search_error(widgets: &Widgets, state: &SharedState, outcome: SearchErrorOutcome) {
    {
        let mut state = state.borrow_mut();
        state.last_error = Some(outcome.error.clone());
        state.search_error = Some(outcome.error);
        if outcome.clear_empty_counts {
            state.thread_loaded_count = 0;
            state.thread_total_count = 0;
            state.can_load_more_threads = false;
        }
    }
    widgets.status_label.set_text(&outcome.message);
    if outcome.clear_empty_counts {
        show_thread_list_message(widgets, &outcome.message);
    }
    update_thread_result_label(widgets, state);
    update_debug(widgets, state);
}

fn update_thread_result_label(widgets: &Widgets, state: &SharedState) {
    let state_ref = state.borrow();
    if state_ref.search_loading {
        drop(state_ref);
        widgets
            .thread_list
            .set_result("Loading search…", "Loading…", false);
        return;
    }
    let status = thread_window_status_from_parts(
        state_ref.thread_window_offset,
        state_ref.thread_list_items.len(),
        state_ref.thread_total_count as usize,
    );
    let result = format!("{status} · page size {}", state_ref.thread_page_size);
    let can_load_more = state_ref.can_load_more_threads;
    drop(state_ref);
    let label = button_label("Load more", "Ctrl+f", state);
    widgets
        .thread_list
        .set_result(&result, &label, can_load_more);
}

fn thread_window_status(state: &SharedState) -> String {
    let state = state.borrow();
    thread_window_status_from_parts(
        state.thread_window_offset,
        state.thread_list_items.len(),
        state.thread_total_count as usize,
    )
}

fn refresh_thread_model_rows(widgets: &Widgets, state: &SharedState, indices: &[usize]) {
    widgets
        .thread_list
        .refresh_rows(&thread_model_snapshot(state), indices, true);
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
        state.visual_select_cursor = Some(state.thread_window_offset + index);
        state.visual_selection_pending_range = None;
    }
    update_visual_selection_to_cursor(widgets, state);
}

fn clear_visual_selection(widgets: &Widgets, state: &SharedState) {
    {
        let mut state = state.borrow_mut();
        state.visual_select_mode = false;
        state.visual_select_anchor = None;
        state.visual_select_cursor = None;
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

fn toggle_multi_selected_thread(widgets: &Widgets, state: &SharedState) {
    let Some(index) = selected_thread_index(widgets) else {
        widgets.status_label.set_text("No thread selected");
        return;
    };
    toggle_multi_selected_thread_index(widgets, state, index);
}

fn toggle_multi_selected_thread_index(widgets: &Widgets, state: &SharedState, index: usize) {
    let (thread_id, count, selected) = {
        let mut state = state.borrow_mut();
        let Some(thread_id) = state
            .thread_list_items
            .get(index)
            .map(|thread| thread.thread_id.clone())
        else {
            widgets
                .status_label
                .set_text("Thread row is not selectable");
            return;
        };
        state.visual_select_mode = false;
        state.visual_select_anchor = None;
        state.visual_select_cursor = None;
        state.visual_selected_threads.clear();
        state.visual_selection_pending_range = None;
        let selected = if state.multi_selected_threads.contains(&thread_id) {
            state.multi_selected_threads.remove(&thread_id);
            false
        } else {
            state.multi_selected_threads.insert(thread_id.clone());
            true
        };
        (thread_id, state.multi_selected_threads.len(), selected)
    };
    refresh_thread_model_rows(widgets, state, &[index]);
    widgets.status_label.set_text(&format!(
        "{} thread `{}`; {} selected",
        if selected { "Selected" } else { "Unselected" },
        thread_id,
        format_count(count)
    ));
}

fn clear_multi_selection(widgets: &Widgets, state: &SharedState) {
    {
        let mut state = state.borrow_mut();
        state.multi_selected_threads.clear();
    }
    update_visual_selection_rows_with_force(widgets, state, true);
    widgets.status_label.set_text("Multi-selection cleared");
}

fn update_visual_selection_to_cursor(widgets: &Widgets, state: &SharedState) {
    let Some(cursor) = selected_thread_index(widgets) else {
        return;
    };
    let (anchor, cursor, count) = {
        let state = state.borrow();
        if !state.visual_select_mode {
            return;
        }
        let cursor = state.thread_window_offset + cursor;
        let anchor = state.visual_select_anchor.unwrap_or(cursor);
        let start = anchor.min(cursor);
        let end = anchor.max(cursor);
        (anchor, cursor, end.saturating_sub(start).saturating_add(1))
    };
    {
        let mut state = state.borrow_mut();
        state.visual_select_anchor = Some(anchor);
        state.visual_select_cursor = Some(cursor);
        state.visual_selected_threads.clear();
        state.visual_selection_pending_range = None;
    }
    update_visual_selection_rows(widgets, state);
    widgets.status_label.set_text(&format!(
        "Visual select: {} thread(s) selected",
        format_count(count)
    ));
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

fn update_visual_selection_rows(widgets: &Widgets, state: &SharedState) {
    update_visual_selection_rows_with_force(widgets, state, false);
}

fn update_visual_selection_rows_with_force(widgets: &Widgets, state: &SharedState, force: bool) {
    let snapshot = thread_model_snapshot(state);
    let indices = (0..snapshot.len).collect::<Vec<_>>();
    widgets.thread_list.refresh_rows(&snapshot, &indices, force);
}

fn set_thread_numbers_visible(widgets: &Widgets, state: &SharedState, visible: bool) {
    set_thread_display_visible(widgets, state, ThreadDisplayToggle::Numbers, visible);
}

fn set_thread_display_visible(
    widgets: &Widgets,
    state: &SharedState,
    toggle: ThreadDisplayToggle,
    visible: bool,
) {
    {
        let mut state = state.borrow_mut();
        match toggle {
            ThreadDisplayToggle::Numbers => state.show_thread_numbers = visible,
            ThreadDisplayToggle::Dates => state.show_thread_dates = visible,
            ThreadDisplayToggle::Tags => state.show_thread_tags = visible,
            ThreadDisplayToggle::Preview => state.show_thread_preview = visible,
        }
    }
    update_visual_selection_rows(widgets, state);
    widgets.status_label.set_text(&format!(
        "{} {}",
        toggle.label(),
        if visible { "on" } else { "off" }
    ));
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
    let rejection_restore = capture_message_selection_snapshot(state);
    let preserve_search_focus = widget_contains_focus(widgets.search_bar.entry().upcast_ref());

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
                let status = message_position_status(state, index, "Selected draft");
                match open_selected_draft_message(
                    options,
                    widgets,
                    state,
                    ActivePane::Threads,
                    status,
                    Some(rejection_restore.clone()),
                ) {
                    Ok(true) => {}
                    Ok(false) => {}
                    Err(err) => {
                        state.borrow_mut().last_error = Some(err.to_string());
                        widgets
                            .status_label
                            .set_text(&format!("Preview draft failed: {err}"));
                    }
                }
            } else {
                let status = message_position_status(state, index, "Selected");
                let _ = request_show_selected_message(
                    options,
                    widgets,
                    state,
                    ActivePane::Threads,
                    status,
                    Some(rejection_restore.clone()),
                );
                if !widgets.composer.has_pending_confirmation() {
                    focus_after_thread_preview(widgets, state, preserve_search_focus);
                }
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
    scroll_thread_index_into_view(widgets, index);
    if !preserve_search_focus {
        focus_thread_list(widgets);
    }
    if state.borrow().visual_select_mode {
        update_visual_selection_to_cursor(widgets, state);
    }
    update_custom_tag_controls(widgets, state);
    update_message_action_buttons(options, widgets, state);
    update_debug(widgets, state);
}

fn focus_after_thread_preview(widgets: &Widgets, state: &SharedState, preserve_search_focus: bool) {
    if !preserve_search_focus {
        focus_active_pane(widgets, state);
    }
}

fn message_id_query(message_id: &str) -> String {
    format!("id:{}", search_bar::quote_notmuch_value(message_id))
}

fn open_message_id_request(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    message_id: &str,
) {
    state.borrow_mut().pending_open_message_id = Some(message_id.to_string());
    if open_loaded_thread_at_message(options, widgets, state, message_id) {
        state.borrow_mut().pending_open_message_id = None;
        update_thread_result_label(widgets, state);
        update_debug(widgets, state);
        return;
    }

    widgets.search_bar.set_query(&options.default_query);
    run_search(options, widgets, state, &options.default_query);
}

fn open_loaded_thread_at_message(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    message_id: &str,
) -> bool {
    let rejection_restore = capture_message_selection_snapshot(state);
    let threads = state.borrow().thread_list_items.clone();
    for (index, thread) in threads.iter().enumerate() {
        let result = (|| -> anyhow::Result<Option<usize>> {
            let db = Database::open(&open_config(options), DatabaseMode::ReadOnly)?;
            let messages = db.thread_messages(&thread.thread_id)?;
            let Some(message_index) = messages
                .iter()
                .position(|message| message.message_id == message_id)
            else {
                return Ok(None);
            };
            {
                let mut s = state.borrow_mut();
                s.selected_thread = Some(thread.clone());
                s.selected_message = messages.get(message_index).cloned();
                s.messages = messages;
                s.active_pane = ActivePane::Message;
                s.last_operation = Some(format!("opened message {message_id}"));
                s.last_error = None;
            }
            Ok(Some(message_index))
        })();

        match result {
            Ok(Some(message_index)) => {
                select_thread_index_for_open_message(widgets, index);
                refresh_thread_attachment_list(widgets, state);
                update_message_menu(options, widgets, state);
                if selected_message_is_draft(options, state) {
                    match open_selected_draft_message(
                        options,
                        widgets,
                        state,
                        ActivePane::Message,
                        format!("Opened draft message {message_id}"),
                        Some(rejection_restore.clone()),
                    ) {
                        Ok(true) => {}
                        Ok(false) => {}
                        Err(err) => {
                            state.borrow_mut().last_error = Some(err.to_string());
                            widgets
                                .status_label
                                .set_text(&format!("Open draft failed: {err}"));
                        }
                    }
                } else {
                    let status = format!(
                        "Opened message {} ({}/{})",
                        message_id,
                        message_index + 1,
                        state.borrow().messages.len()
                    );
                    let _ = request_show_selected_message(
                        options,
                        widgets,
                        state,
                        ActivePane::Message,
                        status,
                        Some(rejection_restore.clone()),
                    );
                }
                scroll_thread_index_into_view(widgets, index);
                update_custom_tag_controls(widgets, state);
                update_active_pane_visuals(widgets, state);
                update_message_action_buttons(options, widgets, state);
                focus_active_pane(widgets, state);
                return true;
            }
            Ok(None) => {}
            Err(err) => {
                state.borrow_mut().last_error = Some(err.to_string());
                widgets
                    .status_label
                    .set_text(&format!("Open message failed: {err}"));
                update_debug(widgets, state);
                return false;
            }
        }
    }

    widgets.status_label.set_text(&format!(
        "Message id not found in loaded results: {message_id}"
    ));
    false
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
    let rejection_restore = capture_message_selection_snapshot(state);
    let destination = thread_open_destination(pane_is_visible(widgets, ActivePane::Message));
    let result = (|| -> anyhow::Result<(Vec<notm_notmuch::MessageSummary>, usize)> {
        let db = Database::open(&open_config(options), DatabaseMode::ReadOnly)?;
        let messages = db.thread_messages(&thread.thread_id)?;
        let selected_index = messages.len().saturating_sub(1);
        {
            let mut s = state.borrow_mut();
            s.selected_thread = Some(thread.clone());
            s.selected_message = messages.last().cloned();
            s.messages = messages.clone();
            s.active_pane = match destination {
                ThreadOpenDestination::InlinePane => ActivePane::Message,
                ThreadOpenDestination::StandaloneWindow => ActivePane::Threads,
            };
            s.last_operation = Some(format!("opened thread {}", thread.thread_id));
            s.last_error = None;
        }
        Ok((messages, selected_index))
    })();
    match result {
        Ok((messages, selected_index)) => {
            refresh_thread_attachment_list(widgets, state);
            update_message_menu(options, widgets, state);
            if destination == ThreadOpenDestination::StandaloneWindow {
                match open_standalone_message_window(
                    options,
                    widgets,
                    state,
                    messages,
                    selected_index,
                ) {
                    Ok(()) => widgets.status_label.set_text(&message_position_status(
                        state,
                        index,
                        "Opened in new window",
                    )),
                    Err(err) => {
                        state.borrow_mut().last_error = Some(err.to_string());
                        widgets
                            .status_label
                            .set_text(&format!("Open message window failed: {err}"));
                    }
                }
            } else if selected_message_is_draft(options, state) {
                let status = message_position_status(state, index, "Opened draft");
                match open_selected_draft_message(
                    options,
                    widgets,
                    state,
                    ActivePane::Message,
                    status,
                    Some(rejection_restore.clone()),
                ) {
                    Ok(true) => {}
                    Ok(false) => {}
                    Err(err) => {
                        state.borrow_mut().last_error = Some(err.to_string());
                        widgets
                            .status_label
                            .set_text(&format!("Open draft failed: {err}"));
                    }
                }
            } else {
                let status = message_position_status(state, index, "Opened");
                let _ = request_show_selected_message(
                    options,
                    widgets,
                    state,
                    ActivePane::Message,
                    status,
                    Some(rejection_restore.clone()),
                );
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
    update_active_pane_visuals(widgets, state);
    update_debug(widgets, state);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThreadOpenDestination {
    InlinePane,
    StandaloneWindow,
}

fn thread_open_destination(message_pane_visible: bool) -> ThreadOpenDestination {
    if message_pane_visible {
        ThreadOpenDestination::InlinePane
    } else {
        ThreadOpenDestination::StandaloneWindow
    }
}

fn open_standalone_message_window(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    messages: Vec<notm_notmuch::MessageSummary>,
    selected_index: usize,
) -> anyhow::Result<()> {
    let policy_options = options.clone();
    let policy_state = state.clone();
    let policy_quote_collapse = widgets.quote_collapse.clone();
    let policy: StandalonePolicyProvider = Rc::new(move || {
        let state = policy_state.borrow();
        StandalonePolicySnapshot {
            collapse_quotes: policy_quote_collapse.get(),
            remote_images: settings::remote_images(&policy_options.runtime_settings),
            show_keybind_hints: state.show_keybind_hints,
            normal_input_mode: state.input_mode == InputMode::Normal,
            response_sensitive: !state.send_in_progress,
        }
    });
    let message_has_html: StandaloneMessageHasHtml = Rc::new(message_has_html);
    let render_text: StandaloneTextRenderer = Rc::new(render_message_text);
    let render_options = options.clone();
    let render_html: StandaloneHtmlRenderer = Rc::new(move |message, policy| {
        let policy = match policy {
            StandaloneImagePolicy::Config => ImagePolicy::Config,
            StandaloneImagePolicy::Once => ImagePolicy::Once,
        };
        let (document, _, allow_remote_images, decode_warning_count) =
            render_visual_html_for_message(&render_options, message, policy)?;
        Ok(StandaloneHtmlRender {
            document,
            allow_remote_images,
            status: html_status_text(policy, allow_remote_images, decode_warning_count),
        })
    });
    let create_html_view: StandaloneHtmlViewFactory = Rc::new(new_privacy_html_webview);
    let initialize_html_view: StandaloneHtmlViewInitializer =
        Rc::new(move |view, status_label, allow_remote_images| {
            configure_html_webview(view, allow_remote_images);
            connect_html_navigation_policy(view, status_label);
            connect_html_hover_status(view, status_label);
        });
    let scroll_html: StandaloneHtmlScrollHandler =
        Rc::new(move |view, status_label, request| match request {
            StandaloneHtmlScroll::Lines(lines) => {
                scroll_web_view_lines(view, status_label, lines);
            }
            StandaloneHtmlScroll::Pages(pages) => {
                scroll_web_view_pages(view, status_label, pages);
            }
            StandaloneHtmlScroll::Edge(bottom) => {
                scroll_web_view_to_edge(view, status_label, bottom);
            }
        });
    let open_link: LinkHintOpener = Rc::new(open_html_link_externally);
    let preferred_state = state.clone();
    let preferred_view: StandalonePreferredView =
        Rc::new(move |message| message_view_preference(&preferred_state.borrow(), message));
    let remember_options = options.clone();
    let remember_state = state.clone();
    let remember_view: StandaloneRememberView = Rc::new(move |message, preference| {
        remember_message_view_preference(
            &remember_options,
            &remember_state,
            &message.message_id,
            preference,
        )
    });
    let sender_state = state.clone();
    let sender_view: StandaloneSenderView = Rc::new(move |message| {
        let sender = message_sender_email(message)?;
        sender_state
            .borrow()
            .sender_view_preferences
            .get(&sender)
            .copied()
    });
    let toggle_options = options.clone();
    let toggle_state = state.clone();
    let toggle_sender_view: StandaloneToggleSenderView = Rc::new(move |message, preference| {
        let sender = message_sender_email(message)
            .ok_or_else(|| anyhow::anyhow!("selected message sender could not be parsed"))?;
        toggle_sender_view_preference(&toggle_options, &toggle_state, &sender, preference)
    });
    let response_options = options.clone();
    let response_widgets = widgets.clone();
    let response_state = state.clone();
    let respond: StandaloneResponseHandler = Rc::new(move |request| {
        run_standalone_response_request(
            &response_options,
            &response_widgets,
            &response_state,
            request,
        )
    });

    widgets.standalone_messages.open(StandaloneOpenOptions {
        parent: widgets.window.clone(),
        messages,
        selected_index,
        policy,
        message_has_html,
        render_text,
        render_html,
        create_html_view,
        initialize_html_view,
        scroll_html,
        open_link,
        respond,
        preferred_view,
        remember_view,
        sender_view,
        toggle_sender_view,
    })
}

fn run_standalone_response_request(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    request: StandaloneResponseRequest,
) -> bool {
    let StandaloneResponseRequest {
        action,
        message,
        source_status,
    } = request;
    let result = match action {
        StandaloneResponseAction::Reply(kind) => {
            composed_reply_for_message(options, &message, kind)
                .map(|message| (message, "Reply composer opened"))
        }
        StandaloneResponseAction::Forward => composed_inline_forward_for_message(options, &message)
            .map(|message| (message, "Forward composer opened")),
        StandaloneResponseAction::ForwardAttachment => {
            composed_attachment_forward_for_message(options, &message)
                .map(|message| (message, "Forward-as-attachment composer opened"))
        }
    };
    match result {
        Ok((message, status)) => {
            let kind = match action {
                StandaloneResponseAction::Reply(ReplyKind::Sender) => {
                    ComposerReplacementKind::StandaloneReply
                }
                StandaloneResponseAction::Reply(ReplyKind::All) => {
                    ComposerReplacementKind::StandaloneReplyAll
                }
                StandaloneResponseAction::Forward => ComposerReplacementKind::StandaloneForward,
                StandaloneResponseAction::ForwardAttachment => {
                    ComposerReplacementKind::StandaloneForwardAttachment
                }
            };
            request_pending_action(
                options,
                widgets,
                state,
                PendingTransition::ReplaceComposer(PreparedComposerReplacement {
                    kind,
                    payload: ComposerReplacementPayload::Message(Box::new(message)),
                    selection: None,
                    rejection_restore: None,
                    status: status.to_string(),
                    source_status: Some(source_status),
                    present_main_window: true,
                    show_message_pane: true,
                    active_pane: ActivePane::Message,
                }),
            )
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            source_status.set_text(&format!("Response failed: {err}"));
            false
        }
    }
}

fn composed_reply_for_message(
    options: &LaunchOptions,
    message: &notm_notmuch::MessageSummary,
    kind: ReplyKind,
) -> anyhow::Result<ComposedMessage> {
    let path = message_filename(message)?;
    let identity =
        identity(options).ok_or_else(|| anyhow::anyhow!("No identity configured for reply"))?;
    let mut own = options.other_email.clone();
    if let Some(email) = &options.primary_email {
        own.push(email.clone());
    }
    composer::composed_reply_from_file(path, &identity, &own, kind)
}

fn composed_inline_forward_for_message(
    options: &LaunchOptions,
    message: &notm_notmuch::MessageSummary,
) -> anyhow::Result<ComposedMessage> {
    let path = message_filename(message)?;
    let identity =
        identity(options).ok_or_else(|| anyhow::anyhow!("No identity configured for forward"))?;
    composer::composed_inline_forward_from_file(path, &identity)
}

fn composed_attachment_forward_for_message(
    options: &LaunchOptions,
    message: &notm_notmuch::MessageSummary,
) -> anyhow::Result<ComposedMessage> {
    let path = message_filename(message)?;
    let identity =
        identity(options).ok_or_else(|| anyhow::anyhow!("No identity configured for forward"))?;
    composer::composed_attachment_forward_from_file(path, &identity)
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
    composer::default_state_home()
        .join("notm")
        .join("tag-undo.json")
}

fn load_undo_tag_actions() -> Vec<UndoTagAction> {
    let path = default_undo_history_path();
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<UndoTagHistory>(&text).ok())
        .filter(|history| history.version == UNDO_TAG_HISTORY_VERSION)
        .map(|history| history.actions)
        .unwrap_or_default()
}

fn persist_undo_tag_actions(actions: &[UndoTagAction]) -> anyhow::Result<()> {
    let path = default_undo_history_path();
    persist_undo_tag_actions_to_path(&path, actions)
}

fn persist_undo_tag_actions_to_path(path: &Path, actions: &[UndoTagAction]) -> anyhow::Result<()> {
    let history = UndoTagHistory {
        version: UNDO_TAG_HISTORY_VERSION,
        actions: actions.to_vec(),
    };
    composer::atomic_write_durable(path, serde_json::to_string_pretty(&history)?.as_bytes())
}

fn tag_undo_label(
    mutation: &TagMutation,
    target_threads: usize,
    changed_messages: usize,
) -> String {
    let ops = tag_mutation_label(mutation);
    format!(
        "{ops} on {} ({changed_messages} changed)",
        tag_target_status_label(target_threads)
    )
}

fn message_tag_undo_label(mutation: &TagMutation, changed_messages: usize) -> String {
    let ops = tag_mutation_label(mutation);
    format!("{ops} on 1 message ({changed_messages} changed)")
}

fn tag_mutation_label(mutation: &TagMutation) -> String {
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
    [adds, removes]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn undo_detail_for_visual_range(query: &str, start: usize, end: usize) -> String {
    format!(
        "Range {}-{} in query: {}",
        format_count(start.saturating_add(1)),
        format_count(end.saturating_add(1)),
        truncate_status_text(query, 180)
    )
}

fn undo_detail_for_thread_targets(state: &SharedState, target_threads: usize) -> Option<String> {
    let state = state.borrow();
    if target_threads == 1 {
        if let Some(message) = &state.selected_message {
            return Some(undo_detail_for_message(message));
        }
        if let Some(thread) = &state.selected_thread {
            return Some(undo_detail_for_thread(thread));
        }
    }
    (target_threads > 1).then(|| {
        format!(
            "{} in query: {}",
            tag_target_status_label(target_threads),
            truncate_status_text(&state.current_query, 180)
        )
    })
}

fn undo_detail_for_message(message: &notm_notmuch::MessageSummary) -> String {
    let subject = if message.subject.trim().is_empty() {
        "(no subject)"
    } else {
        message.subject.trim()
    };
    format!(
        "Message: {} · From: {} · Date: {} · ID: {}",
        truncate_status_text(subject, 80),
        truncate_status_text(&message.from, 80),
        format_message_date(message.date),
        message.message_id
    )
}

fn undo_detail_for_thread(thread: &notm_notmuch::ThreadSummary) -> String {
    let subject = if thread.subject.trim().is_empty() {
        "(no subject)"
    } else {
        thread.subject.trim()
    };
    format!(
        "Thread: {} · Authors: {} · Newest: {} · ID: {}",
        truncate_status_text(subject, 80),
        truncate_status_text(&thread.authors, 80),
        format_message_date(thread.newest_date),
        thread.thread_id
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserOperation {
    ComposeEdit,
    Tag,
    DraftSave,
    DraftDelete,
    DraftLoad,
    DraftClear,
    ComposeReplace,
    Send,
    Sync,
}

fn background_activity_block_reason(
    sync_in_progress: bool,
    send_in_progress: bool,
    operation: UserOperation,
) -> Option<&'static str> {
    if operation == UserOperation::ComposeEdit {
        return None;
    }
    if sync_in_progress {
        return match operation {
            UserOperation::ComposeEdit
            | UserOperation::DraftLoad
            | UserOperation::DraftClear
            | UserOperation::ComposeReplace => None,
            UserOperation::Tag => Some("tag changes are unavailable while sync is in progress"),
            UserOperation::DraftSave => {
                Some("draft saving is unavailable while sync is in progress")
            }
            UserOperation::DraftDelete => {
                Some("draft deletion is unavailable while sync is in progress")
            }
            UserOperation::Send => Some("sending is unavailable while sync is in progress"),
            UserOperation::Sync => Some("sync is already running"),
        };
    }
    if send_in_progress {
        return Some(match operation {
            UserOperation::ComposeEdit => unreachable!(),
            UserOperation::Tag => "tag changes are unavailable while send is in progress",
            UserOperation::DraftSave => "draft saving is unavailable while send is in progress",
            UserOperation::DraftDelete => "draft deletion is unavailable while send is in progress",
            UserOperation::DraftLoad => "draft loading is unavailable while send is in progress",
            UserOperation::DraftClear => "draft clearing is unavailable while send is in progress",
            UserOperation::ComposeReplace => {
                "replacing the composer is unavailable while send is in progress"
            }
            UserOperation::Send => "send is already in progress",
            UserOperation::Sync => "sync is unavailable while send is in progress",
        });
    }
    None
}

fn ensure_user_operation_allowed(
    widgets: &Widgets,
    state: &SharedState,
    operation: UserOperation,
) -> anyhow::Result<()> {
    if operation == UserOperation::Send && widgets.composer.has_pending_confirmation() {
        let message = "sending is unavailable while a confirmation is pending";
        widgets.status_label.set_text(message);
        state.borrow_mut().last_operation = Some(message.to_string());
        update_debug(widgets, state);
        anyhow::bail!(message);
    }
    let message = {
        let state = state.borrow();
        background_activity_block_reason(state.sync_in_progress, state.send_in_progress, operation)
    };
    let Some(message) = message else {
        return Ok(());
    };
    widgets.status_label.set_text(message);
    state.borrow_mut().last_operation = Some(message.to_string());
    update_debug(widgets, state);
    anyhow::bail!(message)
}

fn selected_message_has_tag(state: &SharedState, tag: &str) -> bool {
    state
        .borrow()
        .selected_message
        .as_ref()
        .is_some_and(|message| message.tags.iter().any(|existing| existing == tag))
}

fn toggle_selected_message_tag(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    tag: &str,
) -> bool {
    let remove = selected_message_has_tag(state, tag);
    tag_selected_message(
        options,
        widgets,
        state,
        undo_state,
        TagMutation {
            add: (!remove).then(|| tag.to_string()).into_iter().collect(),
            remove: remove.then(|| tag.to_string()).into_iter().collect(),
            sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                &options.runtime_settings,
            ),
        },
    )
}

fn apply_custom_tag_to_selected_message(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) -> bool {
    let tag = widgets.message_custom_tag_entry.text().trim().to_string();
    if tag.is_empty() {
        widgets.status_label.set_text("Message tag name is empty");
        return false;
    }
    let applied = toggle_selected_message_tag(options, widgets, state, undo_state, &tag);
    update_message_tag_controls(widgets, state);
    if applied {
        widgets.message_custom_tag_entry.grab_focus();
        widgets.message_custom_tag_entry.select_region(0, -1);
    }
    applied
}

fn tag_selected_message(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    mutation: TagMutation,
) -> bool {
    if ensure_user_operation_allowed(widgets, state, UserOperation::Tag).is_err() {
        return false;
    }
    let Some(message) = state.borrow().selected_message.clone() else {
        widgets
            .status_label
            .set_text("No selected message for tag operation");
        return false;
    };
    let message_id = message.message_id.clone();
    let result = (|| -> anyhow::Result<usize> {
        let db = Database::open(&open_config(options), DatabaseMode::ReadWrite)?;
        let changes = db.apply_tags_to_messages(
            &[MessageTagMutation {
                message_id: message_id.clone(),
                add: mutation.add.clone(),
                remove: mutation.remove.clone(),
            }],
            mutation.sync_maildir_flags,
        )?;
        if !changes.is_empty() {
            push_undo_tag_action(
                undo_state,
                UndoTagAction {
                    mutations: changes.iter().map(|change| change.inverse()).collect(),
                    sync_maildir_flags: mutation.sync_maildir_flags,
                    label: message_tag_undo_label(&mutation, changes.len()),
                    detail: Some(undo_detail_for_message(&message)),
                },
            );
        }
        let mut state = state.borrow_mut();
        state.last_operation = Some(format!(
            "tagged current message {}: +{:?} -{:?}",
            message_id, mutation.add, mutation.remove
        ));
        state.last_error = None;
        Ok(changes.len())
    })();
    match result {
        Ok(changed_messages) => {
            if changed_messages > 0 {
                apply_local_message_tag_mutation(widgets, state, &message_id, &mutation);
            }
            update_message_header(widgets, state);
            update_message_tag_controls(widgets, state);
            update_message_action_buttons(options, widgets, state);
            set_undo_tag_available(widgets, !undo_state.borrow().is_empty());
            if changed_messages > 0 {
                widgets.status_label.set_text(
                    "Tag operation complete for current message; Undo menu shows recent tag actions",
                );
            } else {
                widgets
                    .status_label
                    .set_text("Message tag operation made no changes");
            }
            update_debug(widgets, state);
            true
        }
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Message tag operation failed: {err}"));
            update_debug(widgets, state);
            false
        }
    }
}

fn tag_selected(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    mutation: TagMutation,
) -> bool {
    if ensure_user_operation_allowed(widgets, state, UserOperation::Tag).is_err() {
        return false;
    }
    let visual_target = {
        let state = state.borrow();
        visual_selection_range_from_state(&state).map(|(start, end)| {
            (
                start,
                end,
                state.current_query.clone(),
                end.saturating_sub(start).saturating_add(1),
            )
        })
    };
    if let Some((start, end, query, target_count)) = visual_target {
        let result = (|| -> anyhow::Result<notm_notmuch::ThreadRangeTagReport> {
            let db = Database::open(&open_config(options), DatabaseMode::ReadWrite)?;
            let opts = QueryOptions {
                limit: usize::MAX,
                offset: 0,
                sort: SortOrder::NewestFirst,
                excluded_tags: settings::excluded_tags(&options.runtime_settings),
            };
            let report = db.apply_tags_to_thread_range(&query, &opts, start, end, &mutation)?;
            if !report.changes.is_empty() {
                push_undo_tag_action(
                    undo_state,
                    UndoTagAction {
                        mutations: report
                            .changes
                            .iter()
                            .map(|change| change.inverse())
                            .collect(),
                        sync_maildir_flags: mutation.sync_maildir_flags,
                        label: tag_undo_label(&mutation, target_count, report.changed_messages),
                        detail: Some(undo_detail_for_visual_range(&query, start, end)),
                    },
                );
            }
            state.borrow_mut().last_operation = Some(format!(
                "tagged {} message(s) across {} selected thread(s): +{:?} -{:?}",
                report.changed_messages, report.changed_threads, report.added, report.removed
            ));
            Ok(report)
        })();
        match result {
            Ok(report) => {
                apply_local_tag_mutation_to_visual_range(widgets, state, &mutation, start, end);
                update_message_header(widgets, state);
                update_custom_tag_controls(widgets, state);
                update_message_action_buttons(options, widgets, state);
                let undo_available = !undo_state.borrow().is_empty();
                set_undo_tag_available(widgets, undo_available);
                if report.changed_messages > 0 {
                    widgets.status_label.set_text(&format!(
                        "Tag operation complete for {}; {} message(s) changed",
                        tag_target_status_label(target_count),
                        format_count(report.changed_messages)
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
    } else {
        tag_selected_threads(options, widgets, state, undo_state, mutation)
    }
}

fn tag_selected_threads(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    mutation: TagMutation,
) -> bool {
    let target_thread_ids = tag_target_thread_ids(state);
    if target_thread_ids.is_empty() {
        widgets
            .status_label
            .set_text("No selected thread for tag operation");
        return false;
    }
    let target_count = target_thread_ids.len();
    let undo_detail = undo_detail_for_thread_targets(state, target_count);
    let query = tag_query_for_thread_ids(&target_thread_ids);
    let result = (|| -> anyhow::Result<usize> {
        let db = Database::open(&open_config(options), DatabaseMode::ReadWrite)?;
        let report = db.apply_tags_to_query(&query, &mutation)?;
        if !report.changes.is_empty() {
            push_undo_tag_action(
                undo_state,
                UndoTagAction {
                    mutations: report
                        .changes
                        .iter()
                        .map(|change| change.inverse())
                        .collect(),
                    sync_maildir_flags: mutation.sync_maildir_flags,
                    label: tag_undo_label(&mutation, target_count, report.changed_messages),
                    detail: undo_detail,
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

fn apply_local_tag_mutation_to_visual_range(
    widgets: &Widgets,
    state: &SharedState,
    mutation: &TagMutation,
    start: usize,
    end: usize,
) {
    let target_thread_ids = {
        let state = state.borrow();
        state
            .thread_list_items
            .iter()
            .enumerate()
            .filter(|(index, _)| (start..=end).contains(&(state.thread_window_offset + *index)))
            .map(|(_, thread)| thread.thread_id.clone())
            .collect::<BTreeSet<_>>()
    };
    apply_local_tag_mutation(widgets, state, mutation, &target_thread_ids);
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
    };
    refresh_thread_model_rows(widgets, state, &row_updates);
    update_visual_selection_rows(widgets, state);
}

fn apply_local_message_tag_mutation(
    widgets: &Widgets,
    state: &SharedState,
    message_id: &str,
    mutation: &TagMutation,
) {
    let row_updates = {
        let mut state = state.borrow_mut();
        let Some(thread_id) = state
            .messages
            .iter()
            .find(|message| message.message_id == message_id)
            .map(|message| message.thread_id.clone())
        else {
            return;
        };
        for message in &mut state.messages {
            if message.message_id == message_id {
                apply_tag_mutation_to_tags(&mut message.tags, mutation);
            }
        }
        if let Some(message) = &mut state.selected_message
            && message.message_id == message_id
        {
            apply_tag_mutation_to_tags(&mut message.tags, mutation);
        }

        let aggregate_tags = aggregate_thread_tags(&state.messages, &thread_id);
        let mut updated_thread_indices = Vec::new();
        for (index, thread) in state.thread_list_items.iter_mut().enumerate() {
            if thread.thread_id == thread_id {
                set_thread_tags(thread, &aggregate_tags);
                updated_thread_indices.push(index);
            }
        }
        if let Some(thread) = &mut state.selected_thread
            && thread.thread_id == thread_id
        {
            set_thread_tags(thread, &aggregate_tags);
        }
        updated_thread_indices
    };
    refresh_thread_model_rows(widgets, state, &row_updates);
    update_visual_selection_rows(widgets, state);
}

fn aggregate_thread_tags(
    messages: &[notm_notmuch::MessageSummary],
    thread_id: &str,
) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.thread_id == thread_id)
        .flat_map(|message| message.tags.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn set_thread_tags(thread: &mut notm_notmuch::ThreadSummary, tags: &[String]) {
    thread.tags = tags.to_vec();
    thread.has_unread = thread.tags.iter().any(|tag| tag == "unread");
    thread.is_flagged = thread.tags.iter().any(|tag| tag == "flagged");
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

fn update_background_activity_controls(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) {
    let (background_activity, send_in_progress) = {
        let state = state.borrow();
        (
            state.sync_in_progress || state.send_in_progress,
            state.send_in_progress,
        )
    };
    if let Some(button) = &widgets.manual_sync_button {
        button.set_sensitive(!background_activity);
    }
    widgets
        .composer
        .send_button()
        .set_sensitive(!background_activity);
    widgets.compose_button.set_sensitive(!send_in_progress);
    if send_in_progress {
        widgets.response_menu_button.popdown();
    }
    for button in [
        &widgets.reply_button,
        &widgets.reply_all_button,
        &widgets.forward_button,
        &widgets.forward_attachment_button,
    ] {
        button.set_sensitive(!send_in_progress);
    }
    widgets
        .standalone_messages
        .set_response_sensitive(!send_in_progress);
    widgets.undo_tag_button.set_sensitive(!background_activity);
    widgets
        .undo_last_tag_button
        .set_sensitive(!background_activity);
    widgets
        .undo_list_tag_button
        .set_sensitive(!background_activity);
    update_message_action_buttons(options, widgets, state);
    update_custom_tag_controls(widgets, state);
    update_draft_action_buttons(widgets, state);
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
        button.connect_clicked(move |_| {
            select_message_by_index(&opts, &w, &st, index);
            w.message_menu_button.popdown();
        });
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
    let rejection_restore = capture_message_selection_snapshot(state);
    let message = state.borrow().messages.get(index).cloned();
    if message.is_none() {
        widgets.status_label.set_text("Message index not found");
        return;
    }
    state.borrow_mut().selected_message = message;
    widgets.attachments.select_first_for_message(index);
    update_message_menu(options, widgets, state);
    if selected_message_is_draft(options, state) {
        match open_selected_draft_message(
            options,
            widgets,
            state,
            ActivePane::Message,
            "Opened draft for editing".to_string(),
            Some(rejection_restore.clone()),
        ) {
            Ok(true) => {}
            Ok(false) => {}
            Err(err) => {
                state.borrow_mut().last_error = Some(err.to_string());
                widgets
                    .status_label
                    .set_text(&format!("Open draft failed: {err}"));
            }
        }
    } else {
        let _ = request_show_selected_message(
            options,
            widgets,
            state,
            ActivePane::Message,
            "Opened selected message".to_string(),
            Some(rejection_restore),
        );
    }
}

fn shifted_shortcut_key(
    key: gtk::gdk::Key,
    mods: gtk::gdk::ModifierType,
    lowercase: gtk::gdk::Key,
    uppercase: gtk::gdk::Key,
) -> bool {
    key == uppercase || (key == lowercase && mods.contains(gtk::gdk::ModifierType::SHIFT_MASK))
}

fn message_navigation_delta(key: gtk::gdk::Key, mods: gtk::gdk::ModifierType) -> Option<isize> {
    if shifted_shortcut_key(key, mods, gtk::gdk::Key::j, gtk::gdk::Key::J) {
        Some(1)
    } else if shifted_shortcut_key(key, mods, gtk::gdk::Key::k, gtk::gdk::Key::K) {
        Some(-1)
    } else {
        None
    }
}

fn is_message_tag_menu_key(key: gtk::gdk::Key, mods: gtk::gdk::ModifierType) -> bool {
    shifted_shortcut_key(key, mods, gtk::gdk::Key::m, gtk::gdk::Key::M)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageTagSequenceKeyAction {
    Archive,
    ToggleRead,
    ToggleFlag,
    Trash,
    Spam,
    CustomTag,
}

fn message_tag_sequence_key_action(
    key: gtk::gdk::Key,
    mods: gtk::gdk::ModifierType,
) -> Option<MessageTagSequenceKeyAction> {
    if shifted_shortcut_key(key, mods, gtk::gdk::Key::t, gtk::gdk::Key::T) {
        return Some(MessageTagSequenceKeyAction::CustomTag);
    }
    match key {
        gtk::gdk::Key::a => Some(MessageTagSequenceKeyAction::Archive),
        gtk::gdk::Key::u => Some(MessageTagSequenceKeyAction::ToggleRead),
        gtk::gdk::Key::f => Some(MessageTagSequenceKeyAction::ToggleFlag),
        gtk::gdk::Key::t => Some(MessageTagSequenceKeyAction::Trash),
        gtk::gdk::Key::s => Some(MessageTagSequenceKeyAction::Spam),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageTagSequenceOutcome {
    Unhandled,
    CloseMenu,
    KeepMenuOpen,
}

fn activate_message_tag_sequence_key(
    widgets: &Widgets,
    state: &SharedState,
    key: gtk::gdk::Key,
    mods: gtk::gdk::ModifierType,
) -> MessageTagSequenceOutcome {
    let Some(action) = message_tag_sequence_key_action(key, mods) else {
        return MessageTagSequenceOutcome::Unhandled;
    };
    if action == MessageTagSequenceKeyAction::CustomTag {
        if widgets.message_custom_tag_entry.is_sensitive() {
            set_input_mode(
                widgets,
                state,
                InputMode::Insert,
                "Insert mode: current-message tag (Esc for normal)",
            );
            widgets.message_custom_tag_entry.grab_focus();
            widgets.message_custom_tag_entry.select_region(0, -1);
            return MessageTagSequenceOutcome::KeepMenuOpen;
        }
        widgets
            .status_label
            .set_text("Current message action is unavailable");
        return MessageTagSequenceOutcome::CloseMenu;
    }
    let button = match action {
        MessageTagSequenceKeyAction::Archive => &widgets.message_archive_button,
        MessageTagSequenceKeyAction::ToggleRead => &widgets.message_read_toggle_button,
        MessageTagSequenceKeyAction::ToggleFlag => &widgets.message_flag_toggle_button,
        MessageTagSequenceKeyAction::Trash => &widgets.message_trash_button,
        MessageTagSequenceKeyAction::Spam => &widgets.message_spam_button,
        MessageTagSequenceKeyAction::CustomTag => unreachable!("handled above"),
    };
    if button.is_sensitive() {
        button.emit_clicked();
    } else {
        widgets
            .status_label
            .set_text("Current message action is unavailable");
    }
    MessageTagSequenceOutcome::CloseMenu
}

fn relative_message_index(current: usize, total: usize, delta: isize) -> Option<usize> {
    if total == 0 {
        return None;
    }
    Some(if delta >= 0 {
        current
            .saturating_add(delta as usize)
            .min(total.saturating_sub(1))
    } else {
        current.saturating_sub(delta.unsigned_abs())
    })
}

fn select_relative_message(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    delta: isize,
) -> bool {
    let (current, total) = {
        let state = state.borrow();
        let Some(selected_id) = state
            .selected_message
            .as_ref()
            .map(|message| message.message_id.as_str())
        else {
            widgets.status_label.set_text("No selected message");
            return false;
        };
        let Some(current) = state
            .messages
            .iter()
            .position(|message| message.message_id == selected_id)
        else {
            widgets
                .status_label
                .set_text("Selected message is not in its thread");
            return false;
        };
        (current, state.messages.len())
    };
    let Some(target) = relative_message_index(current, total, delta) else {
        widgets.status_label.set_text("Thread has no messages");
        return false;
    };
    if target == current {
        widgets.status_label.set_text(if delta < 0 {
            "Already at the first message in this thread"
        } else {
            "Already at the last message in this thread"
        });
        return true;
    }
    select_message_by_index(options, widgets, state, target);
    true
}

fn undo_last_tag(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) -> bool {
    if ensure_user_operation_allowed(widgets, state, UserOperation::Tag).is_err() {
        return false;
    }
    let Some(action) = pop_last_undo_tag_action(undo_state) else {
        set_undo_tag_available(widgets, false);
        widgets.status_label.set_text("No tag operation to undo");
        return false;
    };
    undo_tag_action(options, widgets, state, undo_state, action)
}

fn undo_tag_action(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    action: UndoTagAction,
) -> bool {
    if ensure_user_operation_allowed(widgets, state, UserOperation::Tag).is_err() {
        push_undo_tag_action(undo_state, action);
        set_undo_tag_available(widgets, true);
        return false;
    }
    let mutations = action.mutations.clone();
    let result = (|| -> anyhow::Result<()> {
        let db = Database::open(&open_config(options), DatabaseMode::ReadWrite)?;
        db.apply_tags_to_messages(&mutations, action.sync_maildir_flags)?;
        state.borrow_mut().last_operation = Some(format!("undid tag operation: {}", action.label));
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_undo_tag_available(widgets, !undo_state.borrow().is_empty());
            let current = state.borrow().current_query.clone();
            run_search(options, widgets, state, &current);
            widgets.status_label.set_text(&format!(
                "Undid tag operation: {}; reloading search…",
                action.label
            ));
            true
        }
        Err(err) => {
            push_undo_tag_action(undo_state, action);
            set_undo_tag_available(widgets, true);
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Undo failed: {err}"));
            update_debug(widgets, state);
            false
        }
    }
}

#[allow(deprecated)]
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
        let content = gtk::Box::new(gtk::Orientation::Vertical, 3);
        content.set_margin_start(8);
        content.set_margin_end(8);
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        let label = gtk::Label::new(Some(&action.label));
        label.set_xalign(0.0);
        label.set_wrap(true);
        content.append(&label);
        if let Some(detail) = action.detail.as_deref().filter(|detail| !detail.is_empty()) {
            let detail_label = gtk::Label::new(Some(detail));
            detail_label
                .set_widget_name(&format!("notm-undo-tag-row-{}-detail", display_index + 1));
            detail_label.set_xalign(0.0);
            detail_label.set_wrap(true);
            detail_label.add_css_class("dim-label");
            content.append(&detail_label);
        }
        row.set_child(Some(&content));
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

#[allow(clippy::too_many_arguments, deprecated)]
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
    if ensure_user_operation_allowed(widgets, state, UserOperation::Tag).is_err() {
        return;
    }
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
        let _ = undo_tag_action(options, widgets, state, undo_state, action);
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

struct SyncResponse {
    result: anyhow::Result<Vec<String>>,
}

#[derive(Debug, Clone)]
struct SyncExecutionContext {
    database_path: Option<PathBuf>,
    config_path: Option<PathBuf>,
    profile: Option<String>,
    timeout: Duration,
}

impl From<&LaunchOptions> for SyncExecutionContext {
    fn from(options: &LaunchOptions) -> Self {
        Self {
            database_path: options.database_path.clone(),
            config_path: options.config_path.clone(),
            profile: options.profile.clone(),
            timeout: Duration::from_secs(options.sync_timeout_seconds),
        }
    }
}

fn run_manual_sync(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    refresh_delay: Duration,
) -> anyhow::Result<()> {
    run_sync_commands(
        options,
        widgets,
        state,
        SyncRunKind::Manual,
        true,
        refresh_delay,
    )
}

fn run_startup_sync(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    let _ = run_sync_commands(
        options,
        widgets,
        state,
        SyncRunKind::Startup,
        true,
        Duration::ZERO,
    );
}

fn run_sync_commands(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    kind: SyncRunKind,
    refresh_after: bool,
    refresh_delay: Duration,
) -> anyhow::Result<()> {
    if options.fixture_mode {
        if kind == SyncRunKind::Manual {
            widgets
                .status_label
                .set_text("External sync is disabled in fixture mode");
            state.borrow_mut().last_operation = Some("fixture sync blocked".to_string());
        }
        update_debug(widgets, state);
        anyhow::bail!("external sync is disabled in fixture mode");
    }
    if !options.sync_enabled {
        if kind == SyncRunKind::Manual {
            widgets.status_label.set_text("Manual sync is disabled");
            state.borrow_mut().last_operation = Some("manual sync disabled".to_string());
        }
        update_debug(widgets, state);
        anyhow::bail!("manual sync is disabled");
    }
    ensure_user_operation_allowed(widgets, state, UserOperation::Sync)?;
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
        anyhow::bail!("manual sync has no commands to run");
    }
    let label = match kind {
        SyncRunKind::Manual => "Manual sync",
        SyncRunKind::Startup => "Startup sync",
    };
    let application_hold = widgets
        .window
        .application()
        .ok_or_else(|| anyhow::anyhow!("main window is not attached to the GTK application"))?
        .hold();
    widgets
        .status_label
        .set_text(&format!("{label}: running {} command(s)…", commands.len()));
    {
        let mut state = state.borrow_mut();
        state.sync_in_progress = true;
        state.last_error = None;
        state.last_operation = Some(format!("{} started", label.to_ascii_lowercase()));
    }
    update_background_activity_controls(options, widgets, state);
    update_debug(widgets, state);

    let rx = spawn_sync_commands(label, commands, SyncExecutionContext::from(options));
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let mut application_hold = Some(application_hold);
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        let _keep_application_alive = application_hold.as_ref();
        match rx.try_recv() {
            Ok(response) => {
                match response.result {
                    Ok(reports) if refresh_after => {
                        st.borrow_mut().last_operation = Some(format!(
                            "{} commands completed; refreshing messages",
                            label.to_ascii_lowercase()
                        ));
                        w.status_label
                            .set_text(&format!("{label}: refreshing messages…"));
                        let query = sync_refresh_query(&w, &st);
                        let select_first = !composer_requires_confirmation(
                            &compose_fields(&w, &st),
                            st.borrow().active_draft.as_ref(),
                        );
                        let generation =
                            schedule_search(&opts, &w, &st, &query, select_first, refresh_delay);
                        w.sync_refresh_generation.set(Some(generation));
                        let application_hold = application_hold
                            .take()
                            .expect("sync application hold should still be owned");
                        wait_for_sync_refresh(SyncRefreshWait {
                            options: opts.clone(),
                            widgets: w.clone(),
                            state: st.clone(),
                            label,
                            reports,
                            generation,
                            application_hold,
                        });
                    }
                    Ok(reports) => {
                        apply_sync_success(&w, &st, label, &reports);
                        finish_sync_activity(&opts, &w, &st);
                        drop(application_hold.take());
                    }
                    Err(err) => {
                        apply_sync_error(&w, &st, label, err);
                        finish_sync_activity(&opts, &w, &st);
                        drop(application_hold.take());
                    }
                }
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                apply_sync_error(&w, &st, label, anyhow::anyhow!("sync worker disconnected"));
                finish_sync_activity(&opts, &w, &st);
                drop(application_hold.take());
                gtk::glib::ControlFlow::Break
            }
        }
    });
    Ok(())
}

fn sync_refresh_query(widgets: &Widgets, state: &SharedState) -> String {
    let requested_query = widgets.search_bar.requested_query();
    if requested_query.trim().is_empty() {
        state.borrow().current_query.clone()
    } else {
        requested_query
    }
}

struct SyncRefreshWait {
    options: LaunchOptions,
    widgets: Widgets,
    state: SharedState,
    label: &'static str,
    reports: Vec<String>,
    generation: u64,
    application_hold: gtk::gio::ApplicationHoldGuard,
}

fn wait_for_sync_refresh(wait: SyncRefreshWait) {
    let SyncRefreshWait {
        options,
        widgets,
        state,
        label,
        reports,
        generation,
        application_hold,
    } = wait;
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        let _keep_application_alive = &application_hold;
        let (search_loading, full_search_outcome) = {
            let state = state.borrow();
            (
                state.search_loading,
                full_search_outcome_at_or_after(&state, generation),
            )
        };
        if search_loading || full_search_outcome.is_none() {
            return gtk::glib::ControlFlow::Continue;
        }
        if let Some(Err(error)) = full_search_outcome {
            apply_sync_error(
                &widgets,
                &state,
                label,
                anyhow::anyhow!("message refresh failed: {error}"),
            );
        } else {
            apply_sync_success(&widgets, &state, label, &reports);
        }
        finish_sync_activity(&options, &widgets, &state);
        gtk::glib::ControlFlow::Break
    });
}

fn full_search_outcome_at_or_after(state: &UiState, generation: u64) -> Option<Result<(), String>> {
    (state.full_search_outcome_generation >= generation)
        .then(|| state.full_search_outcome_error.clone().map_or(Ok(()), Err))
}

fn apply_sync_success(widgets: &Widgets, state: &SharedState, label: &str, reports: &[String]) {
    let mut state = state.borrow_mut();
    state.last_error = None;
    state.last_operation = Some(format!(
        "{}: {}",
        label.to_ascii_lowercase(),
        reports.join("; ")
    ));
    drop(state);
    widgets.status_label.set_text(&format!("{label} completed"));
}

fn finish_sync_activity(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    state.borrow_mut().sync_in_progress = false;
    widgets.sync_refresh_generation.set(None);
    update_background_activity_controls(options, widgets, state);
    update_debug(widgets, state);
    close_main_window_after_background_activity(widgets, state);
}

fn close_main_window_after_background_activity(widgets: &Widgets, state: &SharedState) {
    let background_activity = {
        let state = state.borrow();
        state.send_in_progress || state.sync_in_progress
    };
    if !background_activity && widgets.close_when_idle.replace(false) {
        widgets.window.close();
    }
}

fn spawn_sync_commands(
    label: &'static str,
    commands: Vec<SyncCommandSpec>,
    context: SyncExecutionContext,
) -> mpsc::Receiver<SyncResponse> {
    let (tx, rx) = mpsc::channel();
    let worker_tx = tx.clone();
    let spawn_result = thread::Builder::new()
        .name("notm-sync".to_string())
        .spawn(move || {
            let result = execute_sync_commands(label, commands, &context);
            let _ = worker_tx.send(SyncResponse { result });
        });
    if let Err(err) = spawn_result {
        let _ = tx.send(SyncResponse {
            result: Err(anyhow::anyhow!("starting sync worker: {err}")),
        });
    }
    rx
}

fn execute_sync_commands(
    label: &str,
    commands: Vec<SyncCommandSpec>,
    context: &SyncExecutionContext,
) -> anyhow::Result<Vec<String>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut reports = Vec::new();
    for spec in commands {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg(&spec.command);
        if let Some(path) = &context.config_path {
            command.env("NOTMUCH_CONFIG", path);
        }
        if let Some(path) = &context.database_path {
            command.env("NOTMUCH_DATABASE", path);
        }
        if let Some(profile) = &context.profile {
            command.env("NOTMUCH_PROFILE", profile);
        }
        let output = runtime.block_on(notm_mail::run_external_command(
            spec.name,
            command,
            None,
            context.timeout,
        ))?;
        let report = sync_command_report(&spec, &output);
        if !output.status.success() {
            let details = report
                .split_once(": ")
                .map_or(report.as_str(), |(_, details)| details);
            anyhow::bail!(
                "{label} {} command failed with {details}",
                spec.name.replace('_', " ")
            );
        }
        reports.push(report);
    }
    Ok(reports)
}

const SYNC_UI_OUTPUT_LIMIT: usize = 4 * 1024;

fn sync_command_report(spec: &SyncCommandSpec, output: &std::process::Output) -> String {
    let status = output
        .status
        .code()
        .map_or_else(|| "signal".to_string(), |code| code.to_string());
    let mut report = format!("{}: status={status}", spec.name);
    for (name, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        let value = bounded_sync_output(bytes);
        if !value.is_empty() {
            report.push_str(&format!(" {name}={value}"));
        }
    }
    report
}

fn bounded_sync_output(bytes: &[u8]) -> String {
    let value = String::from_utf8_lossy(bytes);
    let value = value.trim();
    if value.len() <= SYNC_UI_OUTPUT_LIMIT {
        return value.to_string();
    }
    let mut boundary = SYNC_UI_OUTPUT_LIMIT;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}… [truncated]", &value[..boundary])
}

fn apply_sync_error(widgets: &Widgets, state: &SharedState, label: &str, err: anyhow::Error) {
    let message = err.to_string();
    {
        let mut state = state.borrow_mut();
        state.last_error = Some(message.clone());
        state.last_operation = Some(format!("{} failed: {message}", label.to_ascii_lowercase()));
    }
    widgets
        .status_label
        .set_text(&format!("{label} failed: {message}"));
}

fn manual_sync_response(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    args: &serde_json::Value,
) -> serde_json::Value {
    let refresh_delay = match sync_refresh_worker_delay(options, args) {
        Ok(delay) => delay,
        Err(err) => {
            return json!({
                "ok": false,
                "pending": false,
                "error": err.to_string(),
                "state": &*state.borrow(),
            });
        }
    };
    match run_manual_sync(options, widgets, state, refresh_delay) {
        Ok(()) => json!({"ok": true, "pending": true, "state": &*state.borrow()}),
        Err(err) => json!({
            "ok": false,
            "pending": false,
            "error": err.to_string(),
            "state": &*state.borrow(),
        }),
    }
}

fn sync_refresh_worker_delay(
    options: &LaunchOptions,
    args: &serde_json::Value,
) -> anyhow::Result<Duration> {
    let Some(value) = args.get("test_refresh_delay_ms") else {
        return Ok(Duration::ZERO);
    };
    anyhow::ensure!(
        options.automation_enabled && !options.fixture_mode,
        "test_refresh_delay_ms is available only for non-fixture test-harness syncs"
    );
    let milliseconds = value.as_u64().ok_or_else(|| {
        anyhow::anyhow!("test_refresh_delay_ms must be a non-negative whole number")
    })?;
    let delay = Duration::from_millis(milliseconds);
    anyhow::ensure!(
        delay <= MAX_SYNC_REFRESH_DELAY,
        "test_refresh_delay_ms must not exceed {}",
        MAX_SYNC_REFRESH_DELAY.as_millis()
    );
    Ok(delay)
}

fn sync_command_specs(options: &LaunchOptions, kind: SyncRunKind) -> Vec<SyncCommandSpec> {
    if options.fixture_mode {
        return Vec::new();
    }
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

fn open_compose(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) -> bool {
    request_pending_action(
        options,
        widgets,
        state,
        PendingTransition::ReplaceComposer(PreparedComposerReplacement {
            kind: ComposerReplacementKind::New,
            payload: ComposerReplacementPayload::Empty,
            selection: None,
            rejection_restore: None,
            status: "New composer opened".to_string(),
            source_status: None,
            present_main_window: false,
            show_message_pane: false,
            active_pane: ActivePane::Message,
        }),
    )
}

fn open_mailto_uri_request(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    uri: &str,
) -> bool {
    let request = match parse_mailto_uri(uri) {
        Ok(request) => request,
        Err(error) => {
            report_mailto_error(widgets, state, &error);
            return false;
        }
    };
    let fields =
        compose_fields_from_mailto(widgets.composer.sender_entry().text().to_string(), request);
    request_pending_action(
        options,
        widgets,
        state,
        PendingTransition::ReplaceComposer(PreparedComposerReplacement {
            kind: ComposerReplacementKind::Mailto,
            payload: ComposerReplacementPayload::Fields(Box::new(fields)),
            selection: None,
            rejection_restore: None,
            status: "Mailto composer opened".to_string(),
            source_status: None,
            present_main_window: true,
            show_message_pane: true,
            active_pane: ActivePane::Message,
        }),
    )
}

fn compose_fields_from_mailto(sender: String, request: MailtoRequest) -> ComposeFields {
    ComposeFields {
        from: sender,
        to: request.to.join(", "),
        cc: request.cc.join(", "),
        bcc: request.bcc.join(", "),
        subject: request.subject,
        body: request.body,
        ..ComposeFields::default()
    }
}

fn report_mailto_error(widgets: &Widgets, state: &SharedState, error: &anyhow::Error) {
    let message = format!("Could not open mailto URI: {error}");
    widgets.status_label.set_text(&message);
    {
        let mut state = state.borrow_mut();
        state.last_error = Some(message.clone());
        state.last_operation = Some("mailto URI rejected".to_string());
    }
    update_debug(widgets, state);
}

fn reply_selected(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    kind: ReplyKind,
) -> bool {
    let Some(message) = state.borrow().selected_message.clone() else {
        widgets
            .status_label
            .set_text("No selected message to reply to");
        return false;
    };
    let replied = match composed_reply_for_message(options, &message, kind) {
        Ok(message) => request_pending_action(
            options,
            widgets,
            state,
            PendingTransition::ReplaceComposer(PreparedComposerReplacement {
                kind: if kind == ReplyKind::All {
                    ComposerReplacementKind::ReplyAll
                } else {
                    ComposerReplacementKind::Reply
                },
                payload: ComposerReplacementPayload::Message(Box::new(message)),
                selection: None,
                rejection_restore: None,
                status: "Reply composer opened".to_string(),
                source_status: None,
                present_main_window: false,
                show_message_pane: false,
                active_pane: ActivePane::Message,
            }),
        ),
        Err(err) => {
            widgets
                .status_label
                .set_text(&format!("Reply failed: {err}"));
            false
        }
    };
    update_debug(widgets, state);
    replied
}

fn forward_selected(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) -> bool {
    let Some(message) = state.borrow().selected_message.clone() else {
        widgets
            .status_label
            .set_text("No selected message to forward");
        return false;
    };
    let forwarded = match composed_inline_forward_for_message(options, &message) {
        Ok(message) => request_pending_action(
            options,
            widgets,
            state,
            PendingTransition::ReplaceComposer(PreparedComposerReplacement {
                kind: ComposerReplacementKind::Forward,
                payload: ComposerReplacementPayload::Message(Box::new(message)),
                selection: None,
                rejection_restore: None,
                status: "Forward composer opened".to_string(),
                source_status: None,
                present_main_window: false,
                show_message_pane: false,
                active_pane: ActivePane::Message,
            }),
        ),
        Err(err) => {
            widgets
                .status_label
                .set_text(&format!("Forward failed: {err}"));
            false
        }
    };
    update_debug(widgets, state);
    forwarded
}

fn forward_as_attachment_selected(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> bool {
    let Some(message) = state.borrow().selected_message.clone() else {
        widgets
            .status_label
            .set_text("No selected message to forward");
        return false;
    };
    let forwarded = match composed_attachment_forward_for_message(options, &message) {
        Ok(message) => request_pending_action(
            options,
            widgets,
            state,
            PendingTransition::ReplaceComposer(PreparedComposerReplacement {
                kind: ComposerReplacementKind::ForwardAttachment,
                payload: ComposerReplacementPayload::Message(Box::new(message)),
                selection: None,
                rejection_restore: None,
                status: "Forward-as-attachment composer opened".to_string(),
                source_status: None,
                present_main_window: false,
                show_message_pane: false,
                active_pane: ActivePane::Message,
            }),
        ),
        Err(err) => {
            state.borrow_mut().last_error = Some(err.to_string());
            widgets
                .status_label
                .set_text(&format!("Forward-as-attachment failed: {err}"));
            false
        }
    };
    update_debug(widgets, state);
    forwarded
}

fn fill_composer(widgets: &Widgets, state: &SharedState, message: ComposedMessage) {
    show_compose_view(widgets);
    set_active_draft(widgets, state, None);
    widgets.composer.apply_message_fields(&message);
    let mut fields = compose_fields(widgets, state);
    fields.in_reply_to = message.in_reply_to;
    fields.references = message.references;
    fields.text_reply_quote = message.text_reply_quote;
    fields.html_reply_quote = message.html_reply_quote;
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
    record_compose_edit(state, fields.clone());
    state.borrow_mut().active_pane = ActivePane::Message;
    persist_recovery_draft_from_ui(widgets, state, &fields);
    if state.borrow().input_mode == InputMode::Insert {
        widgets.composer.to_entry().grab_focus();
    } else {
        focus_active_pane(widgets, state);
    }
}

fn cache_composer_attachments(attachments: &[AttachmentInput]) -> anyhow::Result<Vec<String>> {
    attachments::cache_composer_attachments(
        attachments,
        &composer::default_attachment_cache_dir(),
        composer::atomic_write_durable,
    )
}

#[derive(Debug)]
enum SendFailureStage {
    Compose,
    Transport,
}

#[derive(Debug)]
struct SendFailure {
    stage: SendFailureStage,
    error: anyhow::Error,
}

struct SendSuccess {
    report: notm_mail::SendReport,
    persisted: Option<PersistedMessage>,
    issues: Vec<composer::SendCleanupIssue>,
}

struct SendResponse {
    result: Result<SendSuccess, SendFailure>,
}

struct PendingSend {
    options: LaunchOptions,
    widgets: Widgets,
    state: SharedState,
    sent_generation: u64,
    sent_draft: Option<ActiveDraft>,
    application_hold: gtk::gio::ApplicationHoldGuard,
}

enum SendStart {
    Started,
    ConfirmationPending,
}

fn send_compose(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> anyhow::Result<SendStart> {
    ensure_user_operation_allowed(widgets, state, UserOperation::Send)?;
    let snapshot = {
        let state = state.borrow();
        widgets.composer.capture_send(
            &state.compose_fields,
            state.compose_generation,
            state.active_draft.clone(),
        )
    };
    let composer::SendSnapshot {
        fields: sent_fields,
        generation: sent_generation,
        active_draft: sent_draft,
    } = snapshot;
    if let Some(active) = sent_draft {
        let requested = request_pending_action(
            options,
            widgets,
            state,
            PendingTransition::SendComposer {
                fields: sent_fields,
                active,
                generation: sent_generation,
            },
        );
        if requested {
            return Ok(SendStart::ConfirmationPending);
        }
        anyhow::bail!(widgets.status_label.text().to_string());
    }
    start_captured_send(options, widgets, state, sent_fields, None, sent_generation)?;
    Ok(SendStart::Started)
}

fn start_captured_send(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    sent_fields: ComposeFields,
    sent_draft: Option<ActiveDraft>,
    sent_generation: u64,
) -> anyhow::Result<()> {
    let application_hold = widgets
        .window
        .application()
        .ok_or_else(|| anyhow::anyhow!("main window is not attached to the GTK application"))?
        .hold();
    let rx = spawn_send(options.clone(), sent_fields.clone())?;
    {
        let mut state = state.borrow_mut();
        if state.compose_generation == sent_generation {
            state.compose_fields = sent_fields.clone();
        }
        state.send_in_progress = true;
        state.last_send_report = None;
        state.last_error = None;
        state.last_operation = Some("send started".to_string());
    }
    widgets.status_label.set_text("Sending…");
    update_background_activity_controls(options, widgets, state);
    update_debug(widgets, state);

    let pending = PendingSend {
        options: options.clone(),
        widgets: widgets.clone(),
        state: state.clone(),
        sent_generation,
        sent_draft,
        application_hold,
    };
    let mut pending = Some(pending);
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        let _keep_application_alive = pending.as_ref().map(|pending| &pending.application_hold);
        match rx.try_recv() {
            Ok(response) => {
                continue_send_completion(
                    pending.take().expect("pending send should still be owned"),
                    response,
                );
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                finish_send_failure(
                    pending.take().expect("pending send should still be owned"),
                    "Send failed",
                    "send worker disconnected",
                );
                gtk::glib::ControlFlow::Break
            }
        }
    });
    Ok(())
}

fn send_start_response(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> serde_json::Value {
    match send_compose(options, widgets, state) {
        Ok(SendStart::Started) => json!({
            "ok": true,
            "pending": true,
            "pending_confirmation": false,
            "state": &*state.borrow(),
        }),
        Ok(SendStart::ConfirmationPending) => json!({
            "ok": true,
            "pending": false,
            "pending_confirmation": true,
            "state": &*state.borrow(),
        }),
        Err(err) => json!({
            "ok": false,
            "pending": false,
            "pending_confirmation": widgets.composer.has_pending_confirmation(),
            "error": err.to_string(),
            "state": &*state.borrow(),
        }),
    }
}

fn spawn_send(
    options: LaunchOptions,
    fields: ComposeFields,
) -> anyhow::Result<mpsc::Receiver<SendResponse>> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("notm-send".to_string())
        .spawn(move || {
            let result = execute_send(&options, &fields);
            let _ = tx.send(SendResponse { result });
        })?;
    Ok(rx)
}

fn execute_send(
    options: &LaunchOptions,
    fields: &ComposeFields,
) -> Result<SendSuccess, SendFailure> {
    let message = composer::composed_message_from_fields(fields).map_err(|error| SendFailure {
        stage: SendFailureStage::Compose,
        error,
    })?;
    let message_for_persistence = message.clone();
    let mut report = send_message_with_config(options, message).map_err(|error| SendFailure {
        stage: SendFailureStage::Transport,
        error,
    })?;
    let mut persisted = None;
    let mut issues = Vec::new();
    if report.accepted {
        match persist_sent_message(options, &message_for_persistence) {
            Ok(saved) => {
                if report.captured_path.is_none()
                    && let Some(saved) = &saved
                {
                    report.captured_path = Some(saved.path.display().to_string());
                }
                persisted = saved;
            }
            Err(err) => issues.push(composer::SendCleanupIssue::new(
                composer::SendCleanupStage::SentPersistence,
                err,
            )),
        }
    }
    Ok(SendSuccess {
        report,
        persisted,
        issues,
    })
}

fn continue_send_completion(pending: PendingSend, response: SendResponse) {
    match response.result {
        Err(failure) => {
            let prefix = match failure.stage {
                SendFailureStage::Compose => "Compose message build failed",
                SendFailureStage::Transport => "Send failed",
            };
            finish_send_failure(pending, prefix, &failure.error.to_string());
        }
        Ok(success) if success.report.accepted => {
            begin_accepted_send_cleanup(pending, success);
        }
        Ok(success) => finish_send_success(pending, success, false),
    }
}

fn begin_accepted_send_cleanup(pending: PendingSend, mut success: SendSuccess) {
    let Some(draft) = pending.sent_draft.clone() else {
        finish_send_success(pending, success, false);
        return;
    };
    if pending.state.borrow().active_draft.as_ref() != Some(&draft) {
        success.issues.push(composer::SendCleanupIssue::new(
            composer::SendCleanupStage::DraftIdentity,
            "captured draft is no longer active",
        ));
        finish_send_success(pending, success, false);
        return;
    }
    let rx = match spawn_sent_draft_delete(pending.options.clone(), draft) {
        Ok(rx) => rx,
        Err(err) => {
            success.issues.push(composer::SendCleanupIssue::new(
                composer::SendCleanupStage::DraftDelete,
                err,
            ));
            finish_send_success(pending, success, false);
            return;
        }
    };
    let mut pending = Some(pending);
    let mut success = Some(success);
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        let _keep_application_alive = pending.as_ref().map(|pending| &pending.application_hold);
        match rx.try_recv() {
            Ok(result) => {
                let mut success = success.take().expect("send success should still be owned");
                let draft_deleted = match result {
                    Ok(()) => true,
                    Err(err) => {
                        success.issues.push(composer::SendCleanupIssue::new(
                            composer::SendCleanupStage::DraftDelete,
                            err,
                        ));
                        false
                    }
                };
                finish_send_success(
                    pending.take().expect("pending send should still be owned"),
                    success,
                    draft_deleted,
                );
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                let mut success = success.take().expect("send success should still be owned");
                success.issues.push(composer::SendCleanupIssue::new(
                    composer::SendCleanupStage::DraftDelete,
                    "draft delete worker disconnected",
                ));
                finish_send_success(
                    pending.take().expect("pending send should still be owned"),
                    success,
                    false,
                );
                gtk::glib::ControlFlow::Break
            }
        }
    });
}

fn spawn_sent_draft_delete(
    options: LaunchOptions,
    draft: ActiveDraft,
) -> anyhow::Result<mpsc::Receiver<anyhow::Result<()>>> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("notm-sent-draft-delete".to_string())
        .spawn(move || {
            let _ = tx.send(delete_draft_source(&options, &draft));
        })?;
    Ok(rx)
}

fn finish_send_failure(pending: PendingSend, prefix: &str, error: &str) {
    let message = format!("{prefix}: {error}");
    {
        let mut state = pending.state.borrow_mut();
        state.last_error = Some(error.to_string());
        state.last_operation = Some(message.clone());
    }
    pending.widgets.status_label.set_text(&message);
    finish_send_activity(&pending);
}

fn finish_send_success(pending: PendingSend, mut success: SendSuccess, draft_deleted: bool) {
    let accepted = success.report.accepted;
    let pending_autosave_error = pending
        .state
        .borrow()
        .last_error
        .as_deref()
        .filter(|error| error.starts_with("Draft autosave failed:"))
        .map(ToOwned::to_owned);
    let newer_composer_changes =
        accepted && finish_accepted_send_cleanup(&pending, &mut success, draft_deleted);
    if newer_composer_changes && let Some(error) = pending_autosave_error {
        success.issues.push(composer::SendCleanupIssue::new(
            composer::SendCleanupStage::NewerDraftAutosave,
            error,
        ));
    }
    let issue_summary = composer::format_send_cleanup_issues(&success.issues);
    let operation = send_operation_summary(&success, issue_summary.as_deref());
    let status = if accepted {
        if let Some(summary) = &issue_summary {
            format!("Send accepted; cleanup issues: {summary}")
        } else if newer_composer_changes {
            "Send accepted; newer composer changes kept".to_string()
        } else if success.report.captured_path.is_some() && pending.options.send_command.is_none() {
            "Fake send captured".to_string()
        } else {
            "Send accepted".to_string()
        }
    } else {
        send_rejection_message(&success.report)
    };
    {
        let mut state = pending.state.borrow_mut();
        state.last_send_report = Some(success.report);
        state.last_error = if accepted {
            issue_summary
        } else {
            Some(status.clone())
        };
        state.last_operation = Some(operation);
    }
    pending.widgets.status_label.set_text(&status);
    pending.widgets.composer.refresh_draft_list();
    finish_send_activity(&pending);
}

fn finish_accepted_send_cleanup(
    pending: &PendingSend,
    success: &mut SendSuccess,
    draft_deleted: bool,
) -> bool {
    let plan = {
        let state = pending.state.borrow();
        composer::plan_accepted_send_cleanup(
            pending.sent_generation,
            state.compose_generation,
            pending.sent_draft.as_ref(),
            state.active_draft.as_ref(),
            draft_deleted,
        )
    };
    if plan.clear_active_draft {
        set_active_draft(&pending.widgets, &pending.state, None);
    } else if plan.draft_identity_changed {
        success.issues.push(composer::SendCleanupIssue::new(
            composer::SendCleanupStage::DraftIdentity,
            "captured draft changed before cleanup completed",
        ));
    }

    let recovery_cleared = if plan.clear_recovery {
        match composer::clear_recovery_draft_files(
            pending.widgets.composer.recovery_path(),
            pending.widgets.composer.legacy_recovery_path(),
        ) {
            Ok(()) => true,
            Err(err) => {
                success.issues.push(composer::SendCleanupIssue::new(
                    composer::SendCleanupStage::RecoveryClear,
                    err,
                ));
                false
            }
        }
    } else {
        false
    };
    if plan.reset_composer(recovery_cleared) {
        reset_composer_after_send(&pending.options, &pending.widgets, &pending.state);
    }
    plan.newer_composer_changes
}

fn reset_composer_after_send(options: &LaunchOptions, widgets: &Widgets, state: &SharedState) {
    reset_composer_fields(widgets, state);
    restore_message_view_after_compose(options, widgets, state);
}

fn reset_composer_fields(widgets: &Widgets, state: &SharedState) {
    let fields = widgets.composer.reset_fields();
    update_attachment_label(widgets, &[]);
    {
        let mut state = state.borrow_mut();
        state.compose_fields = fields;
        state.compose_generation = state.compose_generation.saturating_add(1);
        state.active_draft = None;
    }
    update_draft_action_buttons(widgets, state);
}

fn send_operation_summary(success: &SendSuccess, issue_summary: Option<&str>) -> String {
    if !success.report.accepted {
        return send_rejection_message(&success.report);
    }
    let mut details = vec!["send accepted".to_string()];
    if let Some(persisted) = &success.persisted {
        details.push(format!(
            "saved sent message to {}{}",
            persisted.path.display(),
            persisted
                .indexed_message_id
                .as_deref()
                .map(|id| format!(" and indexed {id}"))
                .unwrap_or_default()
        ));
    }
    if let Some(summary) = issue_summary {
        details.push(format!("cleanup issues: {summary}"));
    }
    details.join("; ")
}

fn send_rejection_message(report: &notm_mail::SendReport) -> String {
    let mut message = "Send was not accepted".to_string();
    if let Some(status) = report.exit_status {
        message.push_str(&format!(" (status {status})"));
    }
    if !report.stderr.trim().is_empty() {
        message.push_str(&format!(": {}", report.stderr.trim()));
    }
    message
}

fn finish_send_activity(pending: &PendingSend) {
    pending.state.borrow_mut().send_in_progress = false;
    update_background_activity_controls(&pending.options, &pending.widgets, &pending.state);
    update_debug(&pending.widgets, &pending.state);
    close_main_window_after_background_activity(&pending.widgets, &pending.state);
}

fn send_message_with_config(
    options: &LaunchOptions,
    message: ComposedMessage,
) -> anyhow::Result<notm_mail::SendReport> {
    if !options.send_enabled {
        anyhow::bail!("send.enabled is false");
    }
    if options.fixture_mode {
        let capture_dir = options.fake_send_capture_dir.as_ref().ok_or_else(|| {
            anyhow::anyhow!("fixture mode requires a disposable fake-send capture directory")
        })?;
        let rt = tokio::runtime::Runtime::new()?;
        let transport = FakeSendTransport {
            capture_dir: capture_dir.clone(),
        };
        return rt.block_on(transport.send(message));
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
        .or_else(|| options.mail_root.as_ref().map(|path| path.join("Sent")))
        .or_else(|| options.database_path.as_ref().map(|path| path.join("Sent")))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "send.save_sent=true but no sent_maildir or Notmuch mail root/database path is available"
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
        .or_else(|| options.mail_root.as_ref().map(|path| path.join("Drafts")))
        .or_else(|| {
            options
                .database_path
                .as_ref()
                .map(|path| path.join("Drafts"))
        })
        .or_else(|| default_database_maildir(options, "Drafts").ok())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "drafts.save_maildir=true but no draft maildir or Notmuch mail root/database path is available"
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
    let root = db
        .config_value("database.mail_root")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(db.path()));
    Ok(root.join(name))
}

fn save_rfc5322_to_maildir(
    maildir: &Path,
    message: &ComposedMessage,
    flags: &str,
) -> anyhow::Result<PathBuf> {
    let tmp = maildir.join("tmp");
    let cur = maildir.join("cur");
    let new = maildir.join("new");
    create_private_directory(&tmp)?;
    create_private_directory(&cur)?;
    create_private_directory(&new)?;
    let unique = format!(
        "{}.{}.{}.notm",
        Utc::now().timestamp(),
        std::process::id(),
        Uuid::new_v4()
    );
    let tmp_path = tmp.join(&unique);
    write_private_new_file(&tmp_path, message.to_rfc5322().as_bytes())?;
    let final_path = cur.join(format!("{unique}:2,{flags}"));
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(final_path)
}

fn create_private_directory(path: &Path) -> anyhow::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}

fn write_private_new_file(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(contents)?;
    file.flush()?;
    Ok(())
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
    shortcut_router: &MainShortcutRouter,
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
        state.borrow_mut().last_error = Some(format!("test harness failed: {err}"));
    } else {
        eprintln!(
            "notm test harness socket={} token={}",
            socket.display(),
            token
        );
        widgets
            .status_label
            .set_text(&format!("Test harness: {}", socket.display()));
    }
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    let saved = saved_store.clone();
    let shortcuts = shortcut_router.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        while let Ok(req) = rx.try_recv() {
            handle_automation_request(&opts, &w, &st, &undo, &saved, &shortcuts, req);
        }
        gtk::glib::ControlFlow::Continue
    });
}

fn search_status_json(state: &SharedState) -> serde_json::Value {
    let state = state.borrow();
    json!({
        "ok": true,
        "loading": state.search_loading,
        "generation": state.search_generation,
        "pending_query": state.pending_search_query,
        "error": state.search_error,
        "current_query": state.current_query,
    })
}

fn fixture_search_worker_delay(
    options: &LaunchOptions,
    args: &serde_json::Value,
) -> anyhow::Result<Duration> {
    search_bar::fixture_search_worker_delay(
        SearchHarnessPolicy {
            fixture_mode: options.fixture_mode,
            automation_enabled: options.automation_enabled,
        },
        args,
    )
}

fn handle_automation_request(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
    saved_store: &SavedSearchStore,
    shortcut_router: &MainShortcutRouter,
    req: AutomationRequest,
) {
    let pending_block = widgets
        .composer
        .pending_confirmation_snapshot()
        .and_then(|pending| {
            (!automation_command_allowed_while_confirmation_pending(&req.command)).then(|| {
                json!({
                    "ok": false,
                    "error": "automation mutation is unavailable while a confirmation is pending",
                    "pending_confirmation": true,
                    "pending": {
                        "id": pending.id,
                        "kind": pending.kind,
                    },
                })
            })
        });
    if let Some(response) = pending_block {
        let _ = req.response.send(response);
        return;
    }
    if let Err(err) = ensure_automation_request_allowed(options, &req.command, &req.args) {
        let _ = req
            .response
            .send(json!({"ok": false, "error": err.to_string()}));
        return;
    }
    let confirmation_control = ensure_confirmation_control_allowed(
        options,
        widgets.composer.pending_confirmation_is_saved_send(),
        req.command.as_str(),
    );
    if let Err(err) = confirmation_control {
        let _ = req
            .response
            .send(json!({"ok": false, "error": err.to_string()}));
        return;
    }
    let result = match req.command.as_str() {
        "health" => json!({"ok": true, "state": "running"}),
        "close_main_window" => {
            let window = widgets.window.clone();
            let response_written = req.response_written;
            gtk::glib::timeout_add_local(Duration::from_millis(10), move || match response_written
                .try_recv()
            {
                Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
                    window.close();
                    gtk::glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            });
            json!({"ok": true})
        }
        "app_state" => json!({"ok": true, "state": &*state.borrow()}),
        "search_status" => search_status_json(state),
        "screenshot" => {
            let name = req
                .args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("test-harness.png");
            match screenshot::capture_screenshot(&options.screenshot_dir, name) {
                Ok(path) => {
                    state.borrow_mut().screenshot_path = Some(path.clone());
                    json!({"ok": true, "screenshot_path": path})
                }
                Err(err) => json!({"ok": false, "error": err.to_string()}),
            }
        }
        "focus_search" => {
            widgets.search_bar.focus();
            json!({"ok": true})
        }
        "focus_compose_field" => {
            let field = req
                .args
                .get("field")
                .and_then(|v| v.as_str())
                .unwrap_or("to");
            let entry = match field {
                "from" => &widgets.composer.sender_entry(),
                "cc" => &widgets.composer.cc_entry(),
                "bcc" => &widgets.composer.bcc_entry(),
                "subject" => &widgets.composer.subject_entry(),
                _ => &widgets.composer.to_entry(),
            };
            entry.grab_focus();
            if matches!(field, "to" | "cc" | "bcc") {
                widgets.composer.activate_address_entry(entry);
            }
            json!({"ok": true, "field": field})
        }
        "entry_state" => {
            let search_selection_bounds = widgets
                .search_bar
                .entry()
                .selection_bounds()
                .map(|(start, end)| json!({"start": start, "end": end}));
            json!({
                "ok": true,
                "search": widgets.search_bar.entry().text().to_string(),
                "search_has_focus": widget_contains_focus(widgets.search_bar.entry().upcast_ref()),
                "search_selection_bounds": search_selection_bounds,
                "custom_tag": widgets.custom_tag_entry.text().to_string(),
                "custom_tag_has_focus": widget_contains_focus(widgets.custom_tag_entry.upcast_ref()),
                "message_custom_tag": widgets.message_custom_tag_entry.text().to_string(),
                "message_custom_tag_has_focus": widget_contains_focus(
                    widgets.message_custom_tag_entry.upcast_ref()
                ),
                "tag_command": widgets.tag_command_entry.text().to_string(),
                "tag_command_has_focus": widget_contains_focus(widgets.tag_command_entry.upcast_ref()),
                "status": widgets.status_label.text().to_string(),
                "tag_menu_visible": widgets
                    .tag_menu_button
                    .popover()
                    .is_some_and(|popover| popover.is_visible()),
                "single_tag_editor_visible": widgets.single_tag_editor_box.is_visible(),
                "tag_command_editor_visible": widgets.tag_command_editor_box.is_visible(),
                "single_tag_action": widgets.single_tag_action_label.text().to_string(),
                "single_tag_apply_label": widgets
                    .single_tag_apply_button
                    .label()
                    .unwrap_or_default()
                    .to_string(),
                "compose_fields": compose_fields(widgets, state),
                "input_mode": format!("{:?}", state.borrow().input_mode),
                "active_pane": format!("{:?}", state.borrow().active_pane),
                "main_shortcut_controller_count": main_shortcut_controller_count(widgets),
                "search_suggestions_visible": widgets.search_bar.suggestions_visible(),
                "address_suggestions_visible": widgets.composer.address_suggestions().is_visible(),
            })
        }
        "send_key" => match injected_shortcut(&req.args) {
            Ok((key, modifiers)) => {
                let propagation = shortcut_router.handle_key(key, modifiers);
                spin_main_context_for(Duration::from_millis(25));
                json!({
                    "ok": true,
                    "handled": propagation == gtk::glib::Propagation::Stop,
                    "propagation": match propagation {
                        gtk::glib::Propagation::Stop => "stop",
                        gtk::glib::Propagation::Proceed => "proceed",
                    },
                    "key": key.name().map(|name| name.to_string()),
                    "modifiers": shortcut_modifier_names(modifiers),
                    "input_mode": format!("{:?}", state.borrow().input_mode),
                    "active_pane": format!("{:?}", state.borrow().active_pane),
                    "status_text": widgets.status_label.text().to_string(),
                })
            }
            Err(err) => json!({"ok": false, "error": err.to_string()}),
        },
        "set_search_query" => {
            let query = req
                .args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            widgets.search_bar.set_query(query);
            widgets.search_bar.entry().set_position(-1);
            json!({"ok": true, "current_query": query, "state": &*state.borrow()})
        }
        "run_search" => {
            let query = if let Some(q) = req.args.get("query").and_then(|v| v.as_str()) {
                q.to_string()
            } else {
                widgets.search_bar.entry().text().to_string()
            };
            match fixture_search_worker_delay(options, &req.args) {
                Ok(worker_delay) => {
                    let generation =
                        schedule_search(options, widgets, state, &query, true, worker_delay);
                    json!({
                        "ok": true,
                        "scheduled": true,
                        "generation": generation,
                        "state": &*state.borrow(),
                    })
                }
                Err(err) => json!({"ok": false, "error": err.to_string()}),
            }
        }
        "load_more_threads" => {
            let select_last = req
                .args
                .get("select_last")
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            let scheduled = load_more_threads(options, widgets, state, select_last);
            json!({"ok": true, "scheduled": scheduled, "state": &*state.borrow()})
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
                "search_loading": state.search_loading,
                "search_generation": state.search_generation,
                "pending_search_query": state.pending_search_query,
                "search_error": state.search_error,
            })
        }
        "thread_selection_view_state" | "selection_view_state" => {
            spin_main_context_for(Duration::from_millis(150));
            thread_selection_view_state(widgets, state)
        }
        "thread_row_layout" => {
            let index = req
                .args
                .get("index")
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok())
                .or_else(|| selected_thread_index(widgets))
                .unwrap_or(0);
            spin_main_context_for(Duration::from_millis(150));
            thread_row_layout_state(widgets, index)
        }
        "scroll_thread_list_to_bottom" => {
            let adjustment = widgets.thread_list.scrolled().vadjustment();
            let before_loaded = state.borrow().thread_loaded_count;
            let target = (adjustment.upper() - adjustment.page_size()).max(0.0);
            adjustment.set_value(target);
            if state.borrow().thread_loaded_count == before_loaded {
                let at_bottom = adjustment.upper() <= adjustment.page_size() + 24.0
                    || adjustment.value() + adjustment.page_size() + 24.0 >= adjustment.upper();
                if at_bottom && state.borrow().can_load_more_threads {
                    load_more_threads(options, widgets, state, false);
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
        "resize_window" => {
            let width = req
                .args
                .get("width")
                .and_then(|value| value.as_i64())
                .unwrap_or(900)
                .clamp(360, 4000) as i32;
            let height = req
                .args
                .get("height")
                .and_then(|value| value.as_i64())
                .unwrap_or(500)
                .clamp(240, 3000) as i32;
            widgets.window.set_default_size(width, height);
            widgets.window.present();
            spin_main_context_for(Duration::from_millis(250));
            apply_auto_layout_for_current_size(widgets, state);
            json!({"ok": true, "width": widgets.window.width(), "height": widgets.window.height(), "layout": layout_state_json(widgets, state)})
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
                widgets.search_bar.set_query(&saved.query);
                state.borrow_mut().visible_saved_search = Some(saved.name.clone());
                run_search(options, widgets, state, &saved.query);
            } else {
                open_saved_search_name(options, widgets, state, name);
            }
            json!({
                "ok": true,
                "scheduled": state.borrow().search_loading,
                "generation": state.borrow().search_generation,
                "state": &*state.borrow(),
            })
        }
        "custom_saved_searches" => {
            json!({"ok": true, "custom_saved_searches": &*saved_store.borrow()})
        }
        "save_current_search" => {
            let name = req
                .args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            widgets.saved_name_entry.set_text(name);
            match save_custom_search_from_current_query(options, widgets, state, saved_store) {
                Ok(()) => {
                    json!({"ok": true, "custom_saved_searches": &*saved_store.borrow(), "state": &*state.borrow()})
                }
                Err(err) => json!({"ok": false, "error": err.to_string()}),
            }
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
        "pane_visibility" => pane_visibility_json(widgets, state),
        "message_action_labels" => json!({
            "ok": true,
            "respond": widgets.response_menu_button.label().map(|label| label.to_string()),
            "reply": widgets.reply_button.label().map(|label| label.to_string()),
            "message": widgets.message_menu_button.label().map(|label| label.to_string()),
            "message_tag": widgets.message_tag_menu_button.label().map(|label| label.to_string()),
            "view": widgets.view_menu_button.label().map(|label| label.to_string()),
            "collapse_quotes": widgets.collapse_quotes_button.label().map(|label| label.to_string()),
            "copy": widgets.copy_menu_button.label().map(|label| label.to_string()),
            "image_policy": widgets.image_policy_button.label().map(|label| label.to_string()),
            "archive": widgets.archive_button.label().map(|label| label.to_string()),
        }),
        "layout_state" => layout_state_json(widgets, state),
        "toggle_layout" => {
            toggle_layout_preference(options, widgets, state);
            json!({"ok": true, "layout": layout_state_json(widgets, state)})
        }
        "set_layout" => {
            let layout = req
                .args
                .get("layout")
                .or_else(|| req.args.get("mode"))
                .and_then(|v| v.as_str())
                .unwrap_or("auto");
            match try_parse_layout_preference(layout) {
                Some(preference) => {
                    set_layout_preference(options, widgets, state, preference);
                    json!({"ok": true, "layout": layout_state_json(widgets, state)})
                }
                None => json!({
                    "ok": false,
                    "error": format!(
                        "unknown layout {layout:?}; expected auto, columns, three_pane, or stacked"
                    ),
                    "layout": layout_state_json(widgets, state),
                }),
            }
        }
        "set_pane_visibility" => {
            let pane = req
                .args
                .get("pane")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let visible = req
                .args
                .get("visible")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            match parse_pane_name(pane) {
                Some(pane)
                    if !visible
                        && pane_is_visible(widgets, pane)
                        && visible_panes(widgets).len() <= 1 =>
                {
                    json!({"ok": false, "error": "at least one pane must stay visible", "visibility": pane_visibility_json(widgets, state)})
                }
                Some(pane) => {
                    set_pane_visibility(widgets, state, pane, visible);
                    sync_pane_toggle_buttons(widgets);
                    json!({"ok": true, "visibility": pane_visibility_json(widgets, state)})
                }
                None => json!({"ok": false, "error": format!("unknown pane: {pane}")}),
            }
        }
        "select_thread_by_index" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            select_thread_index_in_list(widgets, index);
            select_thread_by_index(options, widgets, state, index, false);
            json!({"ok": true, "selected_thread_index": selected_thread_index(widgets), "selected_thread": state.borrow().selected_thread})
        }
        "toggle_multi_select_thread" => {
            let index = req
                .args
                .get("index")
                .and_then(|v| v.as_u64())
                .map(|value| value as usize)
                .or_else(|| selected_thread_index(widgets));
            if let Some(index) = index {
                select_thread_index_in_list(widgets, index);
                select_thread_by_index(options, widgets, state, index, false);
                toggle_multi_selected_thread_index(widgets, state, index);
            }
            json!({"ok": index.is_some(), "selected_thread_index": selected_thread_index(widgets), "multi_selected_threads": state.borrow().multi_selected_threads})
        }
        "clear_multi_selection" => {
            clear_multi_selection(widgets, state);
            json!({"ok": true, "multi_selected_threads": state.borrow().multi_selected_threads})
        }
        "select_relative_thread" => {
            let delta = req.args.get("delta").and_then(|v| v.as_i64()).unwrap_or(0) as isize;
            select_relative_thread(options, widgets, state, delta);
            json!({"ok": true, "selected_thread_index": selected_thread_index(widgets), "state": &*state.borrow()})
        }
        "select_thread_edge" => {
            let bottom = req
                .args
                .get("bottom")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            select_thread_edge(options, widgets, state, bottom);
            json!({"ok": true, "selected_thread_index": selected_thread_index(widgets), "state": &*state.borrow()})
        }
        "select_message_by_index" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            select_message_by_index(options, widgets, state, index);
            json!({"ok": true, "selected_message": state.borrow().selected_message})
        }
        "select_relative_message" => {
            let delta = req
                .args
                .get("delta")
                .and_then(|value| value.as_i64())
                .unwrap_or(0) as isize;
            let ok = select_relative_message(options, widgets, state, delta);
            json!({
                "ok": ok,
                "selected_index": selected_message_index(state),
                "selected_message": state.borrow().selected_message,
            })
        }
        "open_selected_thread" => {
            let idx = selected_thread_index(widgets).unwrap_or(0);
            open_thread_by_index(options, widgets, state, idx);
            json!({"ok": true, "state": &*state.borrow()})
        }
        "standalone_message_windows" => {
            spin_main_context_for(Duration::from_millis(100));
            standalone_message_windows_json(widgets, state)
        }
        "standalone_select_message" => {
            let window_index = req
                .args
                .get("window_index")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize;
            let message_index = req
                .args
                .get("message_index")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize;
            match widgets
                .standalone_messages
                .select_message(window_index, message_index)
            {
                Some((ok, window)) => {
                    json!({
                        "ok": ok,
                        "window": window,
                        "main_selected_message": state.borrow().selected_message,
                    })
                }
                None => json!({"ok": false, "error": "standalone window index not found"}),
            }
        }
        "standalone_respond" => {
            let window_index = req
                .args
                .get("window_index")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize;
            let action_name = req
                .args
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("reply");
            let action = match action_name {
                "reply" => Some(StandaloneResponseAction::Reply(ReplyKind::Sender)),
                "reply_all" => Some(StandaloneResponseAction::Reply(ReplyKind::All)),
                "forward" => Some(StandaloneResponseAction::Forward),
                "forward_attachment" => Some(StandaloneResponseAction::ForwardAttachment),
                _ => None,
            };
            let window_exists = widgets
                .standalone_messages
                .window_snapshot(window_index)
                .is_some();
            match action {
                Some(action) => match widgets.standalone_messages.respond(window_index, action) {
                    Some((ok, window)) => {
                        let status = window.status.clone();
                        json!({
                            "ok": ok,
                            "window": window,
                            "compose_fields": state.borrow().compose_fields,
                            "main_selected_message": state.borrow().selected_message,
                            "status": status,
                        })
                    }
                    None => {
                        json!({"ok": false, "error": "standalone window index not found"})
                    }
                },
                None if !window_exists => {
                    json!({"ok": false, "error": "standalone window index not found"})
                }
                None => {
                    json!({"ok": false, "error": format!("unknown standalone response action: {action_name}")})
                }
            }
        }
        "message_tag_state" => {
            let selected = state.borrow().selected_message.clone();
            json!({
                "ok": true,
                "selected_message": selected,
                "menu_visible": widgets.message_tag_menu_button.is_visible(),
                "menu_sensitive": widgets.message_tag_menu_button.is_sensitive(),
                "menu_popup_visible": widgets
                    .message_tag_menu_button
                    .popover()
                    .is_some_and(|popover| popover.is_visible()),
                "menu_label": widgets.message_tag_menu_button.label().map(|label| label.to_string()),
                "archive_label": widgets.message_archive_button.label().map(|label| label.to_string()),
                "read_label": widgets.message_read_toggle_button.label().map(|label| label.to_string()),
                "flag_label": widgets.message_flag_toggle_button.label().map(|label| label.to_string()),
                "trash_label": widgets.message_trash_button.label().map(|label| label.to_string()),
                "spam_label": widgets.message_spam_button.label().map(|label| label.to_string()),
                "custom_tag": widgets.message_custom_tag_entry.text().to_string(),
                "custom_action": widgets.message_custom_tag_action_label.text().to_string(),
                "custom_apply_label": widgets.message_custom_tag_apply_button.label().map(|label| label.to_string()),
                "custom_apply_sensitive": widgets.message_custom_tag_apply_button.is_sensitive(),
                "status": widgets.status_label.text().to_string(),
            })
        }
        "set_message_tag_entry" => {
            let tag = req
                .args
                .get("tag")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            widgets.message_custom_tag_entry.set_text(tag);
            json!({"ok": true, "tag": tag, "state": &*state.borrow()})
        }
        "click_message_tag_action" => {
            let action = req
                .args
                .get("action")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if action == "custom"
                && let Some(tag) = req.args.get("tag").and_then(|value| value.as_str())
            {
                widgets.message_custom_tag_entry.set_text(tag);
            }
            let button = match action {
                "archive" => Some(&widgets.message_archive_button),
                "read" | "toggle_read" => Some(&widgets.message_read_toggle_button),
                "flag" | "toggle_flag" => Some(&widgets.message_flag_toggle_button),
                "trash" => Some(&widgets.message_trash_button),
                "spam" => Some(&widgets.message_spam_button),
                "custom" => Some(&widgets.message_custom_tag_apply_button),
                _ => None,
            };
            match button {
                Some(button) if button.is_sensitive() => {
                    button.emit_clicked();
                    let ok = state.borrow().last_error.is_none();
                    automation_mutation_response(ok, widgets, state)
                }
                Some(_) => json!({
                    "ok": false,
                    "error": "message tag action is unavailable",
                    "state": &*state.borrow(),
                }),
                None => json!({
                    "ok": false,
                    "error": format!("unknown message tag action: {action}"),
                    "state": &*state.borrow(),
                }),
            }
        }
        "archive_selected" => {
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec![],
                    remove: vec!["inbox".to_string()],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "mark_read_selected" => {
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec![],
                    remove: vec!["unread".to_string()],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "mark_unread_selected" => {
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["unread".to_string()],
                    remove: vec![],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "flag_selected" => {
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["flagged".to_string()],
                    remove: vec![],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "unflag_selected" => {
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec![],
                    remove: vec!["flagged".to_string()],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "trash_selected" => {
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["trash".to_string()],
                    remove: vec!["inbox".to_string(), "spam".to_string()],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "spam_selected" => {
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["spam".to_string()],
                    remove: vec!["inbox".to_string()],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
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
        ADD_CUSTOM_TAG_FROM_ENTRY_COMMAND => {
            let ok = apply_custom_tag_from_entry(options, widgets, state, undo_state, true);
            automation_mutation_response(ok, widgets, state)
        }
        REMOVE_CUSTOM_TAG_FROM_ENTRY_COMMAND => {
            let ok = apply_custom_tag_from_entry(options, widgets, state, undo_state, false);
            automation_mutation_response(ok, widgets, state)
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
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add,
                    remove,
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "undo_last_tag" => {
            let ok = undo_last_tag(options, widgets, state, undo_state);
            automation_mutation_response(ok, widgets, state)
        }
        "undo_tag_actions" => {
            let actions = undo_state
                .borrow()
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>();
            json!({"ok": true, "actions": actions})
        }
        "run_manual_sync" => manual_sync_response(options, widgets, state, &req.args),
        "open_compose" => {
            let opened = open_compose(options, widgets, state);
            automation_reply_response(opened, widgets, state)
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
                "compose_set_from" => widgets.composer.sender_entry().set_text(value),
                "compose_set_to" => widgets.composer.to_entry().set_text(value),
                "compose_set_cc" => widgets.composer.cc_entry().set_text(value),
                "compose_set_bcc" => widgets.composer.bcc_entry().set_text(value),
                "compose_set_subject" => widgets.composer.subject_entry().set_text(value),
                "compose_set_body" => {
                    widgets.composer.body().buffer().set_text(value);
                    move_compose_cursor_to_start(widgets);
                }
                _ => {}
            }
            record_compose_edit(state, compose_fields(widgets, state));
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
                "suggestions": composer::matching_address_suggestions(prefix, &state.borrow().address_suggestions, 20)
            })
        }
        "select_address_suggestion_by_index" => {
            let input = widgets.composer.to_entry().text().to_string();
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let suggestions = composer::matching_address_suggestions(
                &input,
                &state.borrow().address_suggestions,
                20,
            );
            if let Some(suggestion) = suggestions.get(index) {
                widgets
                    .composer
                    .apply_recipient_suggestion(&widgets.composer.to_entry(), suggestion);
                record_compose_edit(state, compose_fields(widgets, state));
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
                "cc" => &widgets.composer.cc_entry(),
                "bcc" => &widgets.composer.bcc_entry(),
                "from" => &widgets.composer.sender_entry(),
                _ => &widgets.composer.to_entry(),
            };
            let suggestions = state.borrow().address_suggestions.clone();
            let completed = widgets
                .composer
                .apply_first_recipient_completion(entry, &suggestions);
            widgets.composer.update_address_suggestions_for_entry(
                entry,
                &entry.text(),
                &suggestions,
                6,
            );
            record_compose_edit(state, compose_fields(widgets, state));
            json!({"ok": completed, "compose_fields": state.borrow().compose_fields})
        }
        "save_draft" => match request_save_current_draft(options, widgets, state) {
            Ok(Some(report)) => {
                widgets.composer.refresh_draft_list();
                json!({"ok": true, "report": report})
            }
            Ok(None) => json!({
                "ok": true,
                "pending_confirmation": true,
                "state": &*state.borrow(),
            }),
            Err(err) => json!({"ok": false, "error": err.to_string()}),
        },
        "list_drafts" => {
            let drafts = composer::list_named_drafts(
                widgets.composer.drafts_dir(),
                widgets.composer.legacy_drafts_dir(),
            )
            .into_iter()
            .map(|(path, fields)| json!({"path": path, "fields": fields}))
            .collect::<Vec<_>>();
            json!({"ok": true, "drafts": drafts})
        }
        "select_draft_by_index" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as i32;
            if let Some(row) = widgets.composer.draft_list().row_at_index(index) {
                widgets.composer.draft_list().select_row(Some(&row));
                json!({"ok": true})
            } else {
                json!({"ok": false, "error": "draft index not found"})
            }
        }
        "draft_list_state" => {
            spin_main_context_for(Duration::from_millis(75));
            draft_list_state_json(widgets, state)
        }
        "pending_confirmation" => {
            spin_main_context_for(Duration::from_millis(25));
            pending_confirmation_state_json(widgets, state)
        }
        "respond_confirmation" => match respond_pending_confirmation(widgets, state, &req.args) {
            Ok(response) => response,
            Err(err) => json!({"ok": false, "error": err.to_string()}),
        },
        "activate_draft_by_index" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as i32;
            if let Some(row) = widgets.composer.draft_list().row_at_index(index) {
                widgets.composer.draft_list().select_row(Some(&row));
                widgets
                    .composer
                    .draft_list()
                    .emit_by_name::<()>("row-activated", &[&row]);
                spin_main_context_for(Duration::from_millis(25));
                draft_list_state_json(widgets, state)
            } else {
                json!({"ok": false, "error": "draft index not found"})
            }
        }
        "click_delete_selected_draft" => {
            let selected = widgets
                .composer
                .selected_named_draft()
                .map(|(path, _)| path);
            match selected {
                Ok(path) => {
                    let existed_before = path.exists();
                    widgets
                        .composer
                        .delete_selected_draft_button()
                        .emit_clicked();
                    spin_main_context_for(Duration::from_millis(25));
                    let deleted = existed_before && !path.exists();
                    let mut response = draft_list_state_json(widgets, state);
                    response["ok"] = json!(true);
                    response["deleted"] = json!(deleted);
                    response["pending_confirmation"] =
                        json!(widgets.composer.has_pending_confirmation());
                    response["path"] = json!(path);
                    response
                }
                Err(err) => json!({"ok": false, "error": err.to_string()}),
            }
        }
        "load_selected_draft" => match load_selected_named_draft(options, widgets, state) {
            Ok((requested, path)) => {
                let error = (!requested).then(|| widgets.status_label.text().to_string());
                json!({"ok": requested, "path": path, "pending_confirmation": widgets.composer.has_pending_confirmation(), "error": error, "compose_fields": state.borrow().compose_fields})
            }
            Err(err) => json!({"ok": false, "error": err.to_string()}),
        },
        "delete_selected_draft" => {
            let ok = delete_selected_named_draft_from_ui(options, widgets, state);
            let error = (!ok).then(|| widgets.status_label.text().to_string());
            json!({"ok": ok, "pending_confirmation": widgets.composer.has_pending_confirmation(), "error": error, "compose_fields": state.borrow().compose_fields, "active_draft": state.borrow().active_draft, "last_error": state.borrow().last_error})
        }
        "delete_active_draft" | "delete_local_draft" => {
            let ok = delete_active_draft_from_ui(options, widgets, state);
            let error = (!ok).then(|| widgets.status_label.text().to_string());
            json!({"ok": ok, "pending_confirmation": widgets.composer.has_pending_confirmation(), "error": error, "compose_fields": state.borrow().compose_fields, "active_draft": state.borrow().active_draft, "last_error": state.borrow().last_error})
        }
        "load_draft" => {
            let loaded = restore_draft_if_present(options, widgets, state);
            automation_reply_response(loaded, widgets, state)
        }
        "clear_draft" => {
            let ok = clear_current_draft_from_ui(options, widgets, state);
            let error = (!ok).then(|| widgets.status_label.text().to_string());
            json!({"ok": ok, "pending_confirmation": widgets.composer.has_pending_confirmation(), "error": error, "compose_fields": state.borrow().compose_fields, "active_draft": state.borrow().active_draft})
        }
        "compose_send" => send_start_response(options, widgets, state),
        "reply_selected" => automation_reply_response(
            reply_selected(options, widgets, state, ReplyKind::Sender),
            widgets,
            state,
        ),
        "reply_all_selected" => automation_reply_response(
            reply_selected(options, widgets, state, ReplyKind::All),
            widgets,
            state,
        ),
        "forward_selected" => {
            let forwarded = forward_selected(options, widgets, state);
            automation_reply_response(forwarded, widgets, state)
        }
        "forward_as_attachment_selected" => {
            let forwarded = forward_as_attachment_selected(options, widgets, state);
            automation_reply_response(forwarded, widgets, state)
        }
        "toggle_debug_panel" => {
            widgets
                .debug_view
                .set_visible(!widgets.debug_view.is_visible());
            update_debug(widgets, state);
            json!({"ok": true, "debug_visible": widgets.debug_view.is_visible()})
        }
        "show_raw_source" | "open_raw_source" => {
            let ok = choose_selected_message_view(options, widgets, state, MessageViewKind::Raw);
            json!({"ok": ok, "last_error": state.borrow().last_error})
        }
        "show_full_headers" | "full_headers" => {
            let ok =
                choose_selected_message_view(options, widgets, state, MessageViewKind::Headers);
            json!({"ok": ok, "last_error": state.borrow().last_error})
        }
        "show_rendered_thread" | "show_text_thread" | "text_view" => {
            let ok = choose_selected_message_view(options, widgets, state, MessageViewKind::Text);
            json!({"ok": ok, "state": &*state.borrow()})
        }
        "toggle_text_visual" | "toggle_visual_html" => {
            let ok = toggle_text_visual_view(options, widgets, state);
            json!({
                "ok": ok,
                "html_view": html_view_state(options, widgets, state),
                "last_error": state.borrow().last_error,
            })
        }
        "show_visual_html" | "show_html_visual" | "visual_html" => {
            let ok = choose_selected_message_view(options, widgets, state, MessageViewKind::Html);
            json!({
                "ok": ok,
                "html_view": html_view_state(options, widgets, state),
                "last_error": state.borrow().last_error,
            })
        }
        "start_link_hints" => {
            let ok = start_link_hint_mode(options, widgets, state);
            json!({
                "ok": ok,
                "link_hints": widgets.link_hints.snapshot(),
                "status_text": widgets.status_label.text().to_string(),
            })
        }
        "link_hint_state" => link_hint_state_json(widgets),
        "input_link_hint" => {
            let input = req
                .args
                .get("key")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| {
                    let mut chars = value.chars();
                    let first = chars.next()?;
                    chars.next().is_none().then_some(first)
                });
            match input {
                Some(input) => {
                    widgets.link_hints.input_char(input);
                    link_hint_state_json(widgets)
                }
                None => json!({"ok": false, "error": "key must contain exactly one character"}),
            }
        }
        "cancel_link_hints" => {
            widgets.link_hints.cancel();
            link_hint_state_json(widgets)
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
            reject_persistent_sender_image_trust(options, widgets, state)
        }
        "trusted_image_senders" => {
            json!({
                "ok": true,
                "trusted_image_senders": [],
                "retired": true,
                "reason": "raw From headers are not authenticated",
            })
        }
        "html_view_state" => html_view_state(options, widgets, state),
        "view_preference_state" => view_preference_state_json(widgets, state),
        "click_sender_view_preference" => {
            update_sender_view_preference_button(widgets, state);
            widgets.view_menu_button.popup();
            spin_main_context_for(Duration::from_millis(50));
            let button_was_visible = widgets.sender_view_preference_button.is_visible();
            if widgets.sender_view_preference_button.is_sensitive()
                && selected_sender_email(state).is_some()
            {
                widgets.sender_view_preference_button.emit_clicked();
                let mut result = view_preference_state_json(widgets, state);
                result["sender_button_was_visible"] = json!(button_was_visible);
                result
            } else {
                json!({"ok": false, "error": "sender view preference is unavailable"})
            }
        }
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
        "thread_list_rows" => {
            let state = state.borrow();
            let rows = state
                .thread_list_items
                .iter()
                .enumerate()
                .map(|(index, thread)| {
                    json!({
                        "index": index,
                        "absolute": state.thread_window_offset + index,
                        "display_number": state.thread_window_offset + index + 1,
                        "show_thread_numbers": state.show_thread_numbers,
                        "thread_id": &thread.thread_id,
                        "subject": &thread.subject,
                        "authors": &thread.authors,
                        "date": format_thread_list_date(thread.newest_date),
                        "matched_messages": thread.matched_messages,
                        "total_messages": thread.total_messages,
                        "tags": &thread.tags,
                        "show_thread_dates": state.show_thread_dates,
                        "show_thread_tags": state.show_thread_tags,
                        "show_thread_preview": state.show_thread_preview,
                    })
                })
                .collect::<Vec<_>>();
            json!({"ok": true, "rows": rows})
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
        "command_completion" => {
            let input = req
                .args
                .get("input")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            json!({
                "ok": true,
                "input": input,
                "completion": command_completion(input),
                "matches": command_completion_matches(input),
            })
        }
        "open_shortcuts" | "show_shortcuts" => {
            show_shortcuts_overlay(widgets);
            json!({"ok": true})
        }
        "help_search" => {
            let query = req
                .args
                .get("query")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            json!({"ok": true, "query": query, "results": help_search_results(query)})
        }
        "open_settings" => {
            show_settings(widgets, state, options);
            json!({"ok": true})
        }
        "settings_test_state" => {
            spin_main_context_for(Duration::from_millis(75));
            settings_test_state_json(options, widgets, state)
        }
        "respond_settings" => match respond_settings_dialog(options, widgets, state, &req.args) {
            Ok(response) => response,
            Err(err) => json!({"ok": false, "error": err.to_string()}),
        },
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
                .unwrap_or_else(|| settings::page_size(&options.runtime_settings));
            let send_command = req
                .args
                .get("send_command")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            match settings::persist_basic_settings(
                options.app_config_path.as_deref(),
                default_query,
                page_size,
                send_command,
            ) {
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
            json!({"ok": true, "attachments": widgets.attachments.items()})
        }
        "select_attachment_by_index" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            match widgets.attachments.select_index(index) {
                Some(selected) => json!({"ok": true, "selected": selected}),
                None => json!({"ok": false, "error": "attachment index not found"}),
            }
        }
        "save_selected_attachment" | "save_attachment" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let dir = req
                .args
                .get("dir")
                .and_then(|v| v.as_str())
                .map(PathBuf::from);
            let selected_message = state.borrow().selected_message.clone();
            match widgets
                .attachments
                .payload_at_index(selected_message, index)
            {
                Ok(payload) => {
                    let result = match dir.as_deref() {
                        Some(dir) => widgets.attachments.save_to_directory(&payload, dir).map(
                            |result| {
                                let path = result.path.clone();
                                apply_attachment_action_result(widgets, state, result);
                                json!({"ok": true, "pending": false, "path": path})
                            },
                        ),
                        None => widgets
                            .attachments
                            .request_save(payload, attachment_event_handler(widgets, state))
                            .map(|chooser_id| {
                                json!({"ok": true, "pending": true, "chooser_id": chooser_id})
                            }),
                    };
                    match result {
                        Ok(response) => response,
                        Err(err) => {
                            report_attachment_error(widgets, state, "Save attachment", &err);
                            json!({"ok": false, "error": err.to_string()})
                        }
                    }
                }
                Err(err) => {
                    report_attachment_error(widgets, state, "Save attachment", &err);
                    json!({"ok": false, "error": err.to_string()})
                }
            }
        }
        "open_selected_attachment" | "open_attachment" => {
            let index = req.args.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let selected_message = state.borrow().selected_message.clone();
            let result = widgets
                .attachments
                .payload_at_index(selected_message, index)
                .and_then(|payload| widgets.attachments.open(&payload));
            match result {
                Ok(result) => {
                    let path = result.path.clone();
                    apply_attachment_action_result(widgets, state, result);
                    json!({"ok": true, "path": path})
                }
                Err(err) => {
                    report_attachment_error(widgets, state, "Open attachment", &err);
                    json!({"ok": false, "error": err.to_string()})
                }
            }
        }
        "attachment_test_state" => widgets
            .attachments
            .test_state_json(&widgets.status_label.text()),
        "respond_attachment_save" => {
            let response = req
                .args
                .get("response")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let chooser_id = req
                .args
                .get("id")
                .and_then(|value| value.as_u64())
                .or_else(|| widgets.attachments.pending_save_id());
            let result = (|| -> anyhow::Result<(bool, Option<PathBuf>)> {
                let chooser_id = chooser_id
                    .ok_or_else(|| anyhow::anyhow!("no attachment save chooser is pending"))?;
                match response {
                    "accept" => {
                        let target = req
                            .args
                            .get("path")
                            .and_then(|value| value.as_str())
                            .map(Path::new)
                            .ok_or_else(|| {
                                anyhow::anyhow!("respond_attachment_save accept requires a path")
                            })?;
                        let result = widgets.attachments.complete_pending_save(
                            chooser_id,
                            true,
                            Some(target),
                        )?;
                        let path = result.as_ref().map(|result| result.path.clone());
                        if let Some(result) = result {
                            apply_attachment_action_result(widgets, state, result);
                        }
                        Ok((true, path))
                    }
                    "cancel" => {
                        let result = widgets
                            .attachments
                            .complete_pending_save(chooser_id, false, None)?;
                        let path = result.as_ref().map(|result| result.path.clone());
                        if let Some(result) = result {
                            apply_attachment_action_result(widgets, state, result);
                        }
                        Ok((false, path))
                    }
                    _ => anyhow::bail!("respond_attachment_save response must be accept or cancel"),
                }
            })();
            match result {
                Ok((accepted, path)) => json!({
                    "ok": true,
                    "accepted": accepted,
                    "pending": false,
                    "path": path,
                }),
                Err(err) => {
                    report_attachment_error(widgets, state, "Save attachment", &err);
                    json!({"ok": false, "error": err.to_string()})
                }
            }
        }
        "get_logs" => {
            json!({"ok": true, "recent_error": state.borrow().last_error, "last_operation": state.borrow().last_operation})
        }
        other => json!({"ok": false, "error": format!("unknown test-harness command: {other}")}),
    };
    let _ = req.response.send(result);
}

fn automation_command_allowed_while_confirmation_pending(command: &str) -> bool {
    matches!(
        command,
        "health"
            | "app_state"
            | "search_status"
            | "entry_state"
            | "thread_page_info"
            | "thread_selection_view_state"
            | "selection_view_state"
            | "thread_row_layout"
            | "custom_saved_searches"
            | "pane_visibility"
            | "message_action_labels"
            | "message_tag_state"
            | "layout_state"
            | "standalone_message_windows"
            | "undo_tag_actions"
            | "get_address_suggestions"
            | "list_drafts"
            | "draft_list_state"
            | "pending_confirmation"
            | "respond_confirmation"
            | "html_scroll_state"
            | "trusted_image_senders"
            | "html_view_state"
            | "link_hint_state"
            | "message_view_text"
            | "thread_ui_details"
            | "thread_list_rows"
            | "command_completion"
            | "settings_test_state"
            | "attachment_list_items"
            | "attachment_test_state"
            | "get_logs"
    )
}

fn automation_mutation_response(
    ok: bool,
    widgets: &Widgets,
    state: &SharedState,
) -> serde_json::Value {
    let error = (!ok).then(|| widgets.status_label.text().to_string());
    json!({"ok": ok, "error": error, "state": &*state.borrow()})
}

fn draft_list_state_json(widgets: &Widgets, state: &SharedState) -> serde_json::Value {
    let selected_index = widgets
        .composer
        .draft_list()
        .selected_row()
        .map(|row| row.index());
    let mut rows = Vec::new();
    let mut index = 0;
    while let Some(row) = widgets.composer.draft_list().row_at_index(index) {
        let text = row
            .child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
            .map(|label| label.text().to_string())
            .unwrap_or_default();
        rows.push(json!({
            "index": index,
            "widget_name": row.widget_name().to_string(),
            "visible": row.is_visible(),
            "mapped": row.is_mapped(),
            "text": text,
        }));
        index += 1;
    }
    let adjustment = widgets.composer.draft_scrolled().vadjustment();
    let state = state.borrow();
    json!({
        "ok": true,
        "section": {
            "visible": widgets.composer.draft_section().is_visible(),
            "mapped": widgets.composer.draft_section().is_mapped(),
        },
        "empty_state": {
            "text": widgets.composer.draft_empty_label().text().to_string(),
            "visible": widgets.composer.draft_empty_label().is_visible(),
            "mapped": widgets.composer.draft_empty_label().is_mapped(),
        },
        "scroller": {
            "visible": widgets.composer.draft_scrolled().is_visible(),
            "mapped": widgets.composer.draft_scrolled().is_mapped(),
            "min_content_height": DRAFT_LIST_MIN_HEIGHT,
            "max_content_height": DRAFT_LIST_MAX_HEIGHT,
            "scroll_upper": adjustment.upper(),
            "scroll_page_size": adjustment.page_size(),
        },
        "list": {
            "visible": widgets.composer.draft_list().is_visible(),
            "mapped": widgets.composer.draft_list().is_mapped(),
            "selected_index": selected_index,
            "rows": rows,
        },
        "delete_button": {
            "label": widgets.composer.delete_selected_draft_button()
                .label()
                .map(|label| label.to_string()),
            "visible": widgets.composer.delete_selected_draft_button().is_visible(),
            "mapped": widgets.composer.delete_selected_draft_button().is_mapped(),
            "sensitive": widgets.composer.delete_selected_draft_button().is_sensitive(),
        },
        "compose_fields": &state.compose_fields,
        "active_draft": &state.active_draft,
        "recovery_path": widgets.composer.recovery_path(),
        "drafts_dir": widgets.composer.drafts_dir(),
        "last_error": &state.last_error,
        "last_operation": &state.last_operation,
        "status_text": widgets.status_label.text().to_string(),
    })
}

fn pending_confirmation_state_json(widgets: &Widgets, state: &SharedState) -> serde_json::Value {
    let pending = widgets
        .composer
        .pending_confirmation_snapshot()
        .map(|pending| {
            json!({
                "id": pending.id,
                "kind": pending.kind,
                "title": pending.title,
                "confirm_label": pending.confirm_label,
                "visible": pending.visible,
            })
        });
    let completion = widgets
        .composer
        .last_confirmation_completion()
        .map(|completion| {
            json!({
                "id": completion.id,
                "accepted": completion.accepted,
                "succeeded": completion.succeeded,
            })
        });
    let state = state.borrow();
    json!({
        "ok": true,
        "pending": pending,
        "last_completion": completion,
        "compose_fields": &state.compose_fields,
        "active_draft": &state.active_draft,
        "recovery_path": widgets.composer.recovery_path(),
        "drafts_dir": widgets.composer.drafts_dir(),
        "last_error": &state.last_error,
        "last_operation": &state.last_operation,
        "status_text": widgets.status_label.text().to_string(),
    })
}

fn respond_pending_confirmation(
    widgets: &Widgets,
    state: &SharedState,
    args: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let response_name = args
        .get("response")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("reject");
    let response = match response_name {
        "accept" => gtk::ResponseType::Accept,
        "reject" | "cancel" => gtk::ResponseType::Cancel,
        _ => anyhow::bail!("response must be accept or reject"),
    };
    let pending_id = widgets
        .composer
        .respond_confirmation(args.get("id").and_then(serde_json::Value::as_u64), response)?;
    spin_main_context_for(Duration::from_millis(75));
    let mut result = pending_confirmation_state_json(widgets, state);
    let succeeded = widgets
        .composer
        .last_confirmation_completion()
        .is_some_and(|completion| completion.id == pending_id && completion.succeeded);
    result["ok"] = json!(succeeded);
    result["response"] = json!(response_name);
    Ok(result)
}

fn rendered_thread_preview_json(widgets: &Widgets, state: &SharedState) -> serde_json::Value {
    let root = widgets.thread_list.list().upcast::<gtk::Widget>();
    let rendered = (0..state.borrow().thread_list_items.len()).find_map(|index| {
        let name = format!("notm-thread-preview-{index}");
        let label = find_widget_by_name(&root, &name)?
            .downcast::<gtk::Label>()
            .ok()?;
        Some(json!({
            "index": index,
            "widget_name": name,
            "visible": label.is_visible(),
            "lines": label.lines(),
            "wrap": label.wraps(),
            "text": label.text().to_string(),
        }))
    });
    let state = state.borrow();
    json!({
        "show_thread_preview": state.show_thread_preview,
        "configured_lines": state.thread_preview_lines,
        "rendered": rendered,
    })
}

fn settings_test_state_json(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> serde_json::Value {
    let requested = state.borrow().theme;
    let theme_state = theme::theme_state(
        &widgets.theme_background_probe,
        &widgets.gtk_settings,
        &widgets.css_provider,
        requested,
    );
    json!({
        "ok": true,
        "dialog": widgets.settings.test_dialog_state(),
        "theme": theme_state,
        "preview": rendered_thread_preview_json(widgets, state),
        "remote_images": settings::remote_images(&options.runtime_settings),
        "app_config_path": options.app_config_path,
        "status_text": widgets.status_label.text().to_string(),
    })
}

fn respond_settings_dialog(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    args: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let dialog_id = widgets.settings.respond_test(args)?;
    let response_name = args
        .get("response")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("apply");
    spin_main_context_for(Duration::from_millis(75));

    let state_json = settings_test_state_json(options, widgets, state);
    let status = widgets.status_label.text().to_string();
    let accepted = !status.starts_with("Settings validation failed:")
        && !status.starts_with("Settings save failed:")
        && !status.starts_with("Settings were saved but could not be applied:");
    Ok(json!({
        "ok": accepted,
        "dialog_id": dialog_id,
        "response": response_name,
        "error": (!accepted).then_some(status),
        "state": state_json,
    }))
}

fn standalone_message_windows_json(widgets: &Widgets, state: &SharedState) -> serde_json::Value {
    let windows = widgets.standalone_messages.snapshots();
    json!({
        "ok": true,
        "windows": windows,
        "main_selected_thread": state.borrow().selected_thread,
        "main_selected_message": state.borrow().selected_message,
    })
}

fn link_hint_state_json(widgets: &Widgets) -> serde_json::Value {
    json!({
        "ok": true,
        "link_hints": widgets.link_hints.snapshot(),
        "status_text": widgets.status_label.text().to_string(),
        "html_visible": html_view_is_visible(widgets),
    })
}

fn view_preference_state_json(widgets: &Widgets, state: &SharedState) -> serde_json::Value {
    let state_ref = state.borrow();
    let selected_message = state_ref.selected_message.clone();
    let resolved = selected_message
        .as_ref()
        .map(|message| message_view_preference(&state_ref, message));
    let selected_sender = selected_message.as_ref().and_then(message_sender_email);
    json!({
        "ok": true,
        "active_view": widgets.active_message_view.get().preference(),
        "resolved_view": resolved,
        "selected_message": selected_message,
        "selected_sender": selected_sender,
        "message_view_preferences": state_ref.message_view_preferences,
        "sender_view_preferences": state_ref.sender_view_preferences,
        "sender_button": {
            "visible": widgets.sender_view_preference_button.is_visible(),
            "sensitive": widgets.sender_view_preference_button.is_sensitive(),
            "active": widgets.sender_view_preference_button.has_css_class("suggested-action"),
            "label": widgets.sender_view_preference_button.label().map(|label| label.to_string()),
            "tooltip": widgets.sender_view_preference_button.tooltip_text().map(|text| text.to_string()),
        },
    })
}

fn injected_shortcut(
    args: &serde_json::Value,
) -> anyhow::Result<(gtk::gdk::Key, gtk::gdk::ModifierType)> {
    let key_name = args
        .get("key")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow::anyhow!("key must be a non-empty GDK key name"))?;
    let key = match key_name {
        " " | "Space" => Some(gtk::gdk::Key::space),
        "Enter" => Some(gtk::gdk::Key::Return),
        "Esc" => Some(gtk::gdk::Key::Escape),
        _ => gtk::gdk::Key::from_name(key_name),
    }
    .ok_or_else(|| anyhow::anyhow!("unknown GDK key name: {key_name}"))?;

    let modifier_names = match args.get("modifiers") {
        None => Vec::new(),
        Some(serde_json::Value::String(name)) => vec![name.as_str()],
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("modifiers must contain only strings"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
        Some(_) => anyhow::bail!("modifiers must be a string or array of strings"),
    };
    let mut modifiers = gtk::gdk::ModifierType::empty();
    for name in modifier_names {
        modifiers |= match name.to_ascii_lowercase().as_str() {
            "shift" => gtk::gdk::ModifierType::SHIFT_MASK,
            "control" | "ctrl" => gtk::gdk::ModifierType::CONTROL_MASK,
            "alt" => gtk::gdk::ModifierType::ALT_MASK,
            "super" => gtk::gdk::ModifierType::SUPER_MASK,
            _ => anyhow::bail!("unknown key modifier: {name}"),
        };
    }
    Ok((key, modifiers))
}

fn shortcut_modifier_names(modifiers: gtk::gdk::ModifierType) -> Vec<&'static str> {
    [
        (gtk::gdk::ModifierType::SHIFT_MASK, "shift"),
        (gtk::gdk::ModifierType::CONTROL_MASK, "control"),
        (gtk::gdk::ModifierType::ALT_MASK, "alt"),
        (gtk::gdk::ModifierType::SUPER_MASK, "super"),
    ]
    .into_iter()
    .filter_map(|(mask, name)| modifiers.contains(mask).then_some(name))
    .collect()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutomationOperation {
    Send,
    Tag,
    ExternalSync,
    FixtureOnly,
    ConfirmationControl,
}

const ADD_CUSTOM_TAG_FROM_ENTRY_COMMAND: &str = "add_custom_tag_from_entry";
const REMOVE_CUSTOM_TAG_FROM_ENTRY_COMMAND: &str = "remove_custom_tag_from_entry";

fn ensure_automation_request_allowed(
    options: &LaunchOptions,
    command: &str,
    args: &serde_json::Value,
) -> anyhow::Result<()> {
    let operation = match command {
        "compose_send" => Some(AutomationOperation::Send),
        "archive_selected"
        | "mark_read_selected"
        | "mark_unread_selected"
        | "flag_selected"
        | "unflag_selected"
        | "trash_selected"
        | "spam_selected"
        | "click_message_tag_action"
        | ADD_CUSTOM_TAG_FROM_ENTRY_COMMAND
        | REMOVE_CUSTOM_TAG_FROM_ENTRY_COMMAND
        | "tag_selected"
        | "add_tag_selected"
        | "remove_tag_selected"
        | "undo_last_tag" => Some(AutomationOperation::Tag),
        "run_manual_sync" => Some(AutomationOperation::ExternalSync),
        "attachment_test_state"
        | "respond_attachment_save"
        | "draft_list_state"
        | "activate_draft_by_index"
        | "click_delete_selected_draft"
        | "settings_test_state"
        | "respond_settings"
        | "view_preference_state"
        | "click_sender_view_preference"
        | "send_key" => Some(AutomationOperation::FixtureOnly),
        "pending_confirmation" | "respond_confirmation" => {
            Some(AutomationOperation::ConfirmationControl)
        }
        "run_command" => args
            .get("command")
            .and_then(serde_json::Value::as_str)
            .and_then(automation_named_command_operation),
        _ => None,
    };
    let Some(operation) = operation else {
        return Ok(());
    };
    if operation == AutomationOperation::FixtureOnly {
        anyhow::ensure!(
            options.fixture_mode,
            "fixture UI controls are available only in fixture mode"
        );
        return Ok(());
    }
    if operation == AutomationOperation::ConfirmationControl {
        anyhow::ensure!(
            options.fixture_mode || options.allow_live_send_test,
            "confirmation controls require fixture mode or automation.allow_live_send_test=true"
        );
        return Ok(());
    }
    if options.fixture_mode {
        anyhow::ensure!(
            operation != AutomationOperation::ExternalSync,
            "external sync is disabled in fixture mode"
        );
        return Ok(());
    }
    match operation {
        AutomationOperation::Send => anyhow::ensure!(
            options.allow_live_send_test,
            "live test-harness send is disabled; set automation.allow_live_send_test=true"
        ),
        AutomationOperation::Tag => anyhow::ensure!(
            options.allow_live_tag_test,
            "live test-harness tag changes are disabled; set automation.allow_live_tag_test=true"
        ),
        AutomationOperation::ExternalSync => {}
        AutomationOperation::FixtureOnly => unreachable!("fixture-only operation handled above"),
        AutomationOperation::ConfirmationControl => {
            unreachable!("confirmation control handled above")
        }
    }
    Ok(())
}

fn ensure_confirmation_control_allowed(
    options: &LaunchOptions,
    pending_saved_send: bool,
    command: &str,
) -> anyhow::Result<()> {
    if !matches!(command, "pending_confirmation" | "respond_confirmation") || options.fixture_mode {
        return Ok(());
    }
    anyhow::ensure!(
        options.allow_live_send_test,
        "confirmation controls require automation.allow_live_send_test=true"
    );
    anyhow::ensure!(
        pending_saved_send,
        "live confirmation controls are available only for a pending saved-draft Send"
    );
    Ok(())
}

fn automation_named_command_operation(command: &str) -> Option<AutomationOperation> {
    match normalize_command_input(command).as_str() {
        "archive" | "mark_read" | "mark read" | "mark_unread" | "mark unread" | "flag"
        | "unflag" | "trash" | "undo_last_tag" | "undo" => Some(AutomationOperation::Tag),
        "sync" | "manual_sync" | "run_manual_sync" => Some(AutomationOperation::ExternalSync),
        _ => None,
    }
}

fn automation_reply_response(
    replied: bool,
    widgets: &Widgets,
    state: &SharedState,
) -> serde_json::Value {
    if replied {
        json!({
            "ok": true,
            "pending_confirmation": widgets.composer.has_pending_confirmation(),
            "compose_fields": state.borrow().compose_fields,
        })
    } else {
        json!({
            "ok": false,
            "error": widgets.status_label.text().to_string(),
            "compose_fields": state.borrow().compose_fields,
        })
    }
}

fn run_named_command(
    command: &str,
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) -> serde_json::Value {
    let command = normalize_command_input(command);
    match command.as_str() {
        "search" => {
            let query = widgets.search_bar.entry().text().to_string();
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
        "drafts" => {
            open_saved_search_name(options, widgets, state, "Drafts");
            json!({"ok": true, "state": &*state.borrow()})
        }
        "all" => {
            open_saved_search_name(options, widgets, state, "All");
            json!({"ok": true, "state": &*state.borrow()})
        }
        "compose" => {
            let opened = open_compose(options, widgets, state);
            automation_reply_response(opened, widgets, state)
        }
        "reply" => automation_reply_response(
            reply_selected(options, widgets, state, ReplyKind::Sender),
            widgets,
            state,
        ),
        "reply_all" | "reply all" => automation_reply_response(
            reply_selected(options, widgets, state, ReplyKind::All),
            widgets,
            state,
        ),
        "forward" => {
            let forwarded = forward_selected(options, widgets, state);
            automation_reply_response(forwarded, widgets, state)
        }
        "forward_attachment" | "forward_as_attachment" => {
            let forwarded = forward_as_attachment_selected(options, widgets, state);
            automation_reply_response(forwarded, widgets, state)
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
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec![],
                    remove: vec!["inbox".to_string()],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "mark_read" | "mark read" => {
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec![],
                    remove: vec!["unread".to_string()],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "mark_unread" | "mark unread" => {
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["unread".to_string()],
                    remove: vec![],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "flag" => {
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["flagged".to_string()],
                    remove: vec![],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "unflag" => {
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec![],
                    remove: vec!["flagged".to_string()],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "trash" => {
            let ok = tag_selected(
                options,
                widgets,
                state,
                undo_state,
                TagMutation {
                    add: vec!["trash".to_string()],
                    remove: vec!["inbox".to_string(), "spam".to_string()],
                    sync_maildir_flags: settings::sync_maildir_flags_after_tag_change(
                        &options.runtime_settings,
                    ),
                },
            );
            automation_mutation_response(ok, widgets, state)
        }
        "toggle_debug_panel" | "debug" => {
            widgets
                .debug_view
                .set_visible(!widgets.debug_view.is_visible());
            update_debug(widgets, state);
            json!({"ok": true, "debug_visible": widgets.debug_view.is_visible()})
        }
        "layout" | "toggle_layout" => {
            toggle_layout_preference(options, widgets, state);
            json!({"ok": true, "layout": layout_state_json(widgets, state)})
        }
        "layout_auto" | "auto_layout" => {
            set_layout_preference(options, widgets, state, LayoutPreference::Auto);
            json!({"ok": true, "layout": layout_state_json(widgets, state)})
        }
        "layout_columns" | "columns" | "layout_three_pane" | "three_pane" => {
            set_layout_preference(options, widgets, state, LayoutPreference::ThreePane);
            json!({"ok": true, "layout": layout_state_json(widgets, state)})
        }
        "layout_stacked" | "stacked_layout" => {
            set_layout_preference(options, widgets, state, LayoutPreference::Stacked);
            json!({"ok": true, "layout": layout_state_json(widgets, state)})
        }
        "raw_source" | "open_raw_source" => {
            let ok = choose_selected_message_view(options, widgets, state, MessageViewKind::Raw);
            json!({"ok": ok, "last_error": state.borrow().last_error})
        }
        "full_headers" | "show_full_headers" => {
            let ok =
                choose_selected_message_view(options, widgets, state, MessageViewKind::Headers);
            json!({"ok": ok, "last_error": state.borrow().last_error})
        }
        "text" | "rendered" | "show_rendered_thread" | "show_text_thread" => {
            let ok = choose_selected_message_view(options, widgets, state, MessageViewKind::Text);
            json!({"ok": ok, "state": &*state.borrow()})
        }
        "toggle_text_visual" | "toggle_visual_html" => {
            let ok = toggle_text_visual_view(options, widgets, state);
            json!({
                "ok": ok,
                "html_view": html_view_state(options, widgets, state),
                "last_error": state.borrow().last_error,
            })
        }
        "visual_html" | "show_visual_html" | "show_html_visual" => {
            let ok = choose_selected_message_view(options, widgets, state, MessageViewKind::Html);
            json!({
                "ok": ok,
                "html_view": html_view_state(options, widgets, state),
                "last_error": state.borrow().last_error,
            })
        }
        "link_hints" | "links" => {
            let ok = start_link_hint_mode(options, widgets, state);
            json!({
                "ok": ok,
                "link_hints": widgets.link_hints.snapshot(),
                "status_text": widgets.status_label.text().to_string(),
            })
        }
        "image_policy" => {
            activate_image_policy_button(options, widgets, state);
            json!({
                "ok": state.borrow().last_error.is_none(),
                "html_view": html_view_state(options, widgets, state),
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
            reject_persistent_sender_image_trust(options, widgets, state)
        }
        "toggle_quote_collapse" | "collapse_quotes" => {
            toggle_quote_collapse(options, widgets, state);
            json!({"ok": true, "quote_collapse_enabled": state.borrow().quote_collapse_enabled})
        }
        "nu" | "number" => {
            set_thread_numbers_visible(widgets, state, true);
            json!({"ok": true, "show_thread_numbers": state.borrow().show_thread_numbers})
        }
        "nonu" | "nonumber" => {
            set_thread_numbers_visible(widgets, state, false);
            json!({"ok": true, "show_thread_numbers": state.borrow().show_thread_numbers})
        }
        "date" | "dates" => {
            set_thread_display_visible(widgets, state, ThreadDisplayToggle::Dates, true);
            json!({"ok": true, "show_thread_dates": state.borrow().show_thread_dates})
        }
        "nodate" | "nodates" => {
            set_thread_display_visible(widgets, state, ThreadDisplayToggle::Dates, false);
            json!({"ok": true, "show_thread_dates": state.borrow().show_thread_dates})
        }
        "tags" => {
            set_thread_display_visible(widgets, state, ThreadDisplayToggle::Tags, true);
            json!({"ok": true, "show_thread_tags": state.borrow().show_thread_tags})
        }
        "notags" => {
            set_thread_display_visible(widgets, state, ThreadDisplayToggle::Tags, false);
            json!({"ok": true, "show_thread_tags": state.borrow().show_thread_tags})
        }
        "preview" => {
            set_thread_display_visible(widgets, state, ThreadDisplayToggle::Preview, true);
            json!({"ok": true, "show_thread_preview": state.borrow().show_thread_preview})
        }
        "nopreview" => {
            set_thread_display_visible(widgets, state, ThreadDisplayToggle::Preview, false);
            json!({"ok": true, "show_thread_preview": state.borrow().show_thread_preview})
        }
        "save_attachment" => {
            let selected_message = state.borrow().selected_message.clone();
            let result = widgets
                .attachments
                .active_payload(selected_message)
                .and_then(|payload| {
                    widgets
                        .attachments
                        .request_save(payload, attachment_event_handler(widgets, state))
                });
            match result {
                Ok(chooser_id) => {
                    json!({"ok": true, "pending": true, "chooser_id": chooser_id})
                }
                Err(err) => {
                    report_attachment_error(widgets, state, "Save attachment", &err);
                    json!({"ok": false, "error": err.to_string()})
                }
            }
        }
        "open_attachment" => {
            let selected_message = state.borrow().selected_message.clone();
            let result = widgets
                .attachments
                .active_payload(selected_message)
                .and_then(|payload| widgets.attachments.open(&payload));
            match result {
                Ok(result) => {
                    let path = result.path.clone();
                    apply_attachment_action_result(widgets, state, result);
                    json!({"ok": true, "path": path})
                }
                Err(err) => {
                    report_attachment_error(widgets, state, "Open attachment", &err);
                    json!({"ok": false, "error": err.to_string()})
                }
            }
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
            show_settings(widgets, state, options);
            json!({"ok": true})
        }
        "shortcuts" | "show_shortcuts" => {
            show_shortcuts_overlay(widgets);
            json!({"ok": true})
        }
        "help" | "commands" => {
            show_shortcuts_overlay(widgets);
            json!({"ok": true})
        }
        "undo_last_tag" | "undo" => {
            let ok = undo_last_tag(options, widgets, state, undo_state);
            automation_mutation_response(ok, widgets, state)
        }
        "sync" | "manual_sync" | "run_manual_sync" => {
            manual_sync_response(options, widgets, state, &serde_json::Value::Null)
        }
        "" => json!({"ok": false, "error": "missing command"}),
        other => json!({"ok": false, "error": format!("unknown command palette command: {other}")}),
    }
}

fn update_debug(widgets: &Widgets, state: &SharedState) {
    update_debug_view(&widgets.debug_view, state);
}

fn update_debug_view(debug_view: &gtk::TextView, state: &SharedState) {
    let s = state.borrow();
    let text = format!(
        "query: {}\nselected_thread: {}\nselected_message: {}\nlayout: {} ({})\ndatabase_path: {}\ndatabase_revision: {}\nlast_operation: {}\nlast_error: {}\ntest_harness: {}\nsend_in_progress: {}\nsync_in_progress: {}\nlast_send: {}\n",
        s.current_query,
        s.selected_thread
            .as_ref()
            .map(|t| t.thread_id.as_str())
            .unwrap_or(""),
        s.selected_message
            .as_ref()
            .map(|m| m.message_id.as_str())
            .unwrap_or(""),
        content_layout_name(s.content_layout),
        layout_preference_name(s.layout_preference),
        s.database_path.as_deref().unwrap_or(""),
        s.database_revision
            .as_ref()
            .map(|r| format!("{} {}", r.revision, r.uuid))
            .unwrap_or_default(),
        s.last_operation.as_deref().unwrap_or(""),
        s.last_error.as_deref().unwrap_or(""),
        s.automation_enabled,
        s.send_in_progress,
        s.sync_in_progress,
        s.last_send_report
            .as_ref()
            .map(|r| format!("accepted={} status={:?}", r.accepted, r.exit_status))
            .unwrap_or_default(),
    );
    debug_view.buffer().set_text(&text);
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

fn pane_toggle_button(icon_name: &str, widget_name: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_widget_name(widget_name);
    button.set_tooltip_text(Some(tooltip));
    button.add_css_class("notm-pane-visible");
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(16);
    button.set_child(Some(&icon));
    button
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

fn command_name_candidates() -> &'static [&'static str] {
    &[
        "inbox",
        "unread",
        "flagged",
        "sent",
        "drafts",
        "trash",
        "all",
        "search",
        "compose",
        "reply",
        "reply_all",
        "forward",
        "forward_as_attachment",
        "archive",
        "mark_read",
        "mark_unread",
        "flag",
        "unflag",
        "visual_select",
        "clear_visual_selection",
        "raw_source",
        "full_headers",
        "text",
        "visual_html",
        "link_hints",
        "image_policy",
        "load_images_once",
        "collapse_quotes",
        "nu",
        "nonu",
        "number",
        "nonumber",
        "date",
        "nodate",
        "dates",
        "nodates",
        "tags",
        "notags",
        "preview",
        "nopreview",
        "save_attachment",
        "open_attachment",
        "copy_message_id",
        "copy_thread_id",
        "undo",
        "undo_last_tag",
        "sync",
        "manual_sync",
        "debug",
        "layout",
        "layout_auto",
        "layout_columns",
        "layout_stacked",
        "settings",
        "shortcuts",
        "help",
        "commands",
    ]
}

fn normalize_command_input(input: &str) -> String {
    input
        .trim()
        .trim_start_matches(':')
        .trim()
        .replace(' ', "_")
        .to_lowercase()
}

fn command_completion_matches(input: &str) -> Vec<&'static str> {
    let prefix = normalize_command_input(input);
    command_name_candidates()
        .iter()
        .copied()
        .filter(|command| prefix.is_empty() || command.starts_with(&prefix))
        .collect()
}

fn command_completion(input: &str) -> Option<String> {
    let prefix = normalize_command_input(input);
    let matches = command_completion_matches(input);
    if matches.is_empty() {
        return None;
    }
    let common = common_prefix(&matches);
    if common.len() > prefix.len() {
        Some(common)
    } else {
        matches.first().map(|command| (*command).to_string())
    }
}

fn common_prefix(values: &[&str]) -> String {
    let Some(first) = values.first() else {
        return String::new();
    };
    let mut prefix = (*first).to_string();
    for value in &values[1..] {
        while !value.starts_with(&prefix) {
            if prefix.pop().is_none() {
                return String::new();
            }
        }
    }
    prefix
}

fn apply_command_completion(entry: &gtk::Entry) -> bool {
    let Some(completion) = command_completion(&entry.text()) else {
        return false;
    };
    entry.set_text(&completion);
    entry.set_position(-1);
    true
}

fn remove_named_overlay(overlay: &gtk::Overlay, widget_name: &str) -> bool {
    let mut removed = false;
    let mut child = overlay.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.widget_name() == widget_name {
            overlay.remove_overlay(&widget);
            removed = true;
        }
    }
    removed
}

fn close_command_palette(widgets: &Widgets, state: &SharedState) -> bool {
    if remove_named_overlay(&widgets.overlay, "notm-command-palette") {
        enter_normal_mode(widgets, state);
        true
    } else {
        false
    }
}

fn show_command_palette(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    undo_state: &UndoState,
) {
    remove_named_overlay(&widgets.overlay, "notm-command-palette");
    set_input_mode(
        widgets,
        state,
        InputMode::Insert,
        "Command mode (Esc to close)",
    );
    let entry = gtk::Entry::new();
    entry.set_widget_name("notm-command-palette-entry");
    entry.set_placeholder_text(Some(":command"));
    entry.set_width_chars(36);
    entry.set_hexpand(true);

    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.set_widget_name("notm-command-palette");
    panel.set_halign(gtk::Align::Center);
    panel.set_valign(gtk::Align::Center);
    panel.set_width_request(420);
    panel.append(&entry);
    widgets.overlay.add_overlay(&panel);

    let entry_key_controller = gtk::EventControllerKey::new();
    entry_key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let entry_for_keys = entry.clone();
    let w_for_keys = widgets.clone();
    let st_for_keys = state.clone();
    entry_key_controller.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            close_command_palette(&w_for_keys, &st_for_keys);
            return gtk::glib::Propagation::Stop;
        }
        if key == gtk::gdk::Key::Tab {
            return if apply_command_completion(&entry_for_keys) {
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            };
        }
        gtk::glib::Propagation::Proceed
    });
    entry.add_controller(entry_key_controller);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let undo = undo_state.clone();
    entry.connect_activate(move |entry| {
        let command = normalize_command_input(&entry.text());
        close_command_palette(&w, &st);
        let result = run_named_command(&command, &opts, &w, &st, &undo);
        if w.composer.has_pending_confirmation() {
            return;
        }
        if result
            .get("ok")
            .and_then(|ok| ok.as_bool())
            .unwrap_or(false)
        {
            w.status_label.set_text(&format!("Command `{command}` ran"));
        } else {
            w.status_label
                .set_text(&format!("Command `{command}` failed: {result}"));
        }
    });
    entry.grab_focus();
}

#[derive(Debug, Clone, Copy)]
struct HelpEntry {
    section: &'static str,
    key: &'static str,
    description: &'static str,
}

#[allow(deprecated)]
fn show_shortcuts_overlay(widgets: &Widgets) {
    let dialog = gtk::Dialog::builder()
        .title("notm help")
        .transient_for(&widgets.window)
        .modal(true)
        .default_width(820)
        .default_height(720)
        .build();
    dialog.set_widget_name("notm-shortcuts-overlay");
    let area = dialog.content_area();
    area.set_spacing(8);

    let search = gtk::SearchEntry::new();
    search.set_widget_name("notm-help-search-entry");
    search.set_placeholder_text(Some("Search help"));
    area.append(&search);

    let scrolled = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(560)
        .build();
    let form = gtk::Box::new(gtk::Orientation::Vertical, 10);
    form.set_margin_start(8);
    form.set_margin_end(24);
    form.set_margin_top(8);
    form.set_margin_bottom(8);
    scrolled.set_child(Some(&form));
    area.append(&scrolled);

    let sections = Rc::new(RefCell::new(Vec::<HelpSectionFilter>::new()));
    append_help_sections(&form, &sections, shortcut_help_entries());
    append_help_sections(&form, &sections, command_help_entries());

    let sections_for_search = sections.clone();
    search.connect_search_changed(move |entry| {
        let query = entry.text().trim().to_lowercase();
        for section in sections_for_search.borrow().iter() {
            let section_matches = !query.is_empty() && section.haystack.contains(&query);
            let mut any_visible = false;
            for (row, haystack) in &section.rows {
                let visible = query.is_empty() || section_matches || haystack.contains(&query);
                row.set_visible(visible);
                any_visible |= visible;
            }
            for header in &section.headers {
                header.set_visible(query.is_empty() || section_matches || any_visible);
            }
        }
    });

    dialog.add_button("Close", gtk::ResponseType::Close);
    let key_controller = gtk::EventControllerKey::new();
    key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let dialog_for_keys = dialog.clone();
    key_controller.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            dialog_for_keys.close();
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    dialog.add_controller(key_controller);
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.present();
    search.grab_focus();
}

#[derive(Clone)]
struct HelpSectionFilter {
    headers: Vec<gtk::Widget>,
    haystack: String,
    rows: Vec<(gtk::Widget, String)>,
}

fn append_help_sections(
    form: &gtk::Box,
    sections: &Rc<RefCell<Vec<HelpSectionFilter>>>,
    entries: &'static [HelpEntry],
) {
    let mut start = 0;
    while start < entries.len() {
        let title = entries[start].section;
        let mut end = start + 1;
        while end < entries.len() && entries[end].section == title {
            end += 1;
        }
        append_help_section(form, sections, title, &entries[start..end]);
        start = end;
    }
}

fn append_help_section(
    form: &gtk::Box,
    sections: &Rc<RefCell<Vec<HelpSectionFilter>>>,
    title: &'static str,
    entries: &[HelpEntry],
) {
    let headers = help_section_header(form, title);
    let mut rows = Vec::new();
    for entry in entries {
        let row = help_row(entry.key, entry.description);
        let haystack =
            format!("{} {} {}", entry.section, entry.key, entry.description).to_lowercase();
        form.append(&row);
        rows.push((row.upcast::<gtk::Widget>(), haystack));
    }
    sections.borrow_mut().push(HelpSectionFilter {
        headers,
        haystack: title.to_lowercase(),
        rows,
    });
}

fn help_section_header(form: &gtk::Box, title: &str) -> Vec<gtk::Widget> {
    let mut headers = Vec::new();
    if form.first_child().is_some() {
        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        separator.set_margin_top(14);
        separator.set_margin_bottom(6);
        form.append(&separator);
        headers.push(separator.upcast::<gtk::Widget>());
    }
    let label = gtk::Label::new(Some(title));
    label.add_css_class("heading");
    label.add_css_class("notm-settings-section");
    label.set_xalign(0.0);
    label.set_margin_bottom(4);
    form.append(&label);
    headers.push(label.upcast::<gtk::Widget>());
    headers
}

fn help_row(key: &str, description: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_hexpand(true);
    let key_label = gtk::Label::new(Some(key));
    key_label.set_widget_name(&format!("notm-help-key-{}", widget_token(key)));
    key_label.set_width_chars(24);
    key_label.set_xalign(1.0);
    key_label.set_valign(gtk::Align::Start);
    key_label.add_css_class("monospace");
    key_label.add_css_class("notm-help-key");
    key_label.add_css_class("notm-settings-label");
    let desc_label = gtk::Label::new(Some(description));
    desc_label.set_xalign(0.0);
    desc_label.set_hexpand(true);
    desc_label.set_wrap(true);
    row.append(&key_label);
    row.append(&desc_label);
    row
}

fn help_search_results(query: &str) -> Vec<serde_json::Value> {
    let query = query.trim().to_lowercase();
    shortcut_help_entries()
        .iter()
        .chain(command_help_entries().iter())
        .filter(|entry| {
            query.is_empty()
                || format!("{} {} {}", entry.section, entry.key, entry.description)
                    .to_lowercase()
                    .contains(&query)
        })
        .map(|entry| {
            json!({
                "section": entry.section,
                "key": entry.key,
                "description": entry.description,
            })
        })
        .collect()
}

fn shortcut_help_entries() -> &'static [HelpEntry] {
    &[
        HelpEntry {
            section: "Basics",
            key: "Esc",
            description: "Close dialogs, cancel prompts, or return to normal mode from an input.",
        },
        HelpEntry {
            section: "Basics",
            key: "i",
            description: "Focus the nearest input for the active pane.",
        },
        HelpEntry {
            section: "Basics",
            key: "/",
            description: "Focus the search field.",
        },
        HelpEntry {
            section: "Basics",
            key: ":",
            description: "Open the run-command prompt. Tab completes commands; Enter runs them.",
        },
        HelpEntry {
            section: "Basics",
            key: "?",
            description: "Open this help window.",
        },
        HelpEntry {
            section: "Pane navigation",
            key: "h / l",
            description: "Move the active pane left or right.",
        },
        HelpEntry {
            section: "Pane navigation",
            key: "Ctrl+h / Ctrl+l",
            description: "Also move the active pane left or right.",
        },
        HelpEntry {
            section: "Pane navigation",
            key: "Ctrl+1 / Ctrl+2 / Ctrl+3",
            description: "Show or hide the sidebar, message list, or message view.",
        },
        HelpEntry {
            section: "Pane navigation",
            key: "Ctrl+4",
            description: "Cycle among auto, columns, and stacked layouts.",
        },
        HelpEntry {
            section: "Thread navigation",
            key: "j / k",
            description: "Move the selected thread down or up; in other panes, scroll the active pane.",
        },
        HelpEntry {
            section: "Thread navigation",
            key: "<count>j / <count>k",
            description: "Move the selected thread by count.",
        },
        HelpEntry {
            section: "Thread navigation",
            key: "gg / G",
            description: "Go to the actual top or bottom of the current thread result set.",
        },
        HelpEntry {
            section: "Thread navigation",
            key: "<count>gg",
            description: "Load/select an absolute thread number, for example 25gg.",
        },
        HelpEntry {
            section: "Thread navigation",
            key: "Ctrl+d / Ctrl+u",
            description: "Move half a page down or up.",
        },
        HelpEntry {
            section: "Thread navigation",
            key: "Ctrl+b",
            description: "Move back one full page.",
        },
        HelpEntry {
            section: "Thread navigation",
            key: "Ctrl+f",
            description: "Load the next page of thread results and select the last loaded row.",
        },
        HelpEntry {
            section: "Thread navigation",
            key: "Enter",
            description: "Open the selected thread from the thread pane.",
        },
        HelpEntry {
            section: "Thread actions",
            key: "a",
            description: "Archive selected thread(s).",
        },
        HelpEntry {
            section: "Thread actions",
            key: "u",
            description: "Toggle unread on selected thread(s).",
        },
        HelpEntry {
            section: "Thread actions",
            key: "f",
            description: "Toggle flagged on selected thread(s).",
        },
        HelpEntry {
            section: "Thread actions",
            key: "t",
            description: "Move selected thread(s) to trash.",
        },
        HelpEntry {
            section: "Thread actions",
            key: "s",
            description: "Mark selected thread(s) as spam.",
        },
        HelpEntry {
            section: "Thread actions",
            key: "v",
            description: "Start or clear visual thread selection.",
        },
        HelpEntry {
            section: "Thread actions",
            key: "Space",
            description: "Toggle the selected thread in the multi-selection.",
        },
        HelpEntry {
            section: "Thread actions",
            key: "Ctrl+click",
            description: "Toggle the clicked thread in the multi-selection.",
        },
        HelpEntry {
            section: "Thread actions",
            key: "T t",
            description: "Open an add/remove tag input for the selected thread(s).",
        },
        HelpEntry {
            section: "Thread actions",
            key: "T m",
            description: "Open a tag-multiple input, for example -inbox +books.",
        },
        HelpEntry {
            section: "Thread actions",
            key: "z z",
            description: "Undo the last tag operation.",
        },
        HelpEntry {
            section: "Thread actions",
            key: "z m",
            description: "Open the undoable tag-operation list.",
        },
        HelpEntry {
            section: "Saved searches",
            key: "g i/u/f/s/d/t/a",
            description: "Open Inbox, Unread, Flagged, Sent, Drafts, Trash, or All.",
        },
        HelpEntry {
            section: "Saved searches",
            key: "g c 1-9",
            description: "Open a numbered custom saved search.",
        },
        HelpEntry {
            section: "Saved searches",
            key: "Ctrl+s",
            description: "Save the current search-bar query as a custom saved search.",
        },
        HelpEntry {
            section: "Saved searches",
            key: "g 1-9",
            description: "Open a numbered found-tag search or tag-path dropdown.",
        },
        HelpEntry {
            section: "Message actions",
            key: "J / K",
            description: "Select the next or previous message in the current thread; lowercase j/k still scroll.",
        },
        HelpEntry {
            section: "Navigation",
            key: "Ctrl+e / Ctrl+y",
            description: "Scroll the message-list viewport down or up one line without changing the selection.",
        },
        HelpEntry {
            section: "Message actions",
            key: "M",
            description: "Open tag actions for the currently displayed message only.",
        },
        HelpEntry {
            section: "Message actions",
            key: "M a/u/f/t/s",
            description: "Archive, toggle read or flagged, trash, or spam the current message.",
        },
        HelpEntry {
            section: "Message actions",
            key: "M T",
            description: "Focus the custom-tag field for the current message.",
        },
        HelpEntry {
            section: "Message actions",
            key: "r r",
            description: "Reply to the selected message.",
        },
        HelpEntry {
            section: "Message actions",
            key: "r a",
            description: "Reply all.",
        },
        HelpEntry {
            section: "Message actions",
            key: "r f",
            description: "Forward inline.",
        },
        HelpEntry {
            section: "Message actions",
            key: "r A",
            description: "Forward as attachment.",
        },
        HelpEntry {
            section: "Message actions",
            key: "V t / V v / V h / V r",
            description: "Show text, visual HTML, full headers, or raw source.",
        },
        HelpEntry {
            section: "Message actions",
            key: "V a",
            description: "Toggle the current view as this sender's default.",
        },
        HelpEntry {
            section: "Message actions",
            key: "q",
            description: "Toggle quote collapse.",
        },
        HelpEntry {
            section: "Message actions",
            key: "I",
            description: "Load remote images once for the current HTML message.",
        },
        HelpEntry {
            section: "Message actions",
            key: "F",
            description: "Label visible links in Visual HTML; type a label to open that link externally.",
        },
        HelpEntry {
            section: "Message actions",
            key: "y m/t/f/o/c/s",
            description: "Copy message id, thread id, from, to, cc, or subject.",
        },
        HelpEntry {
            section: "Compose",
            key: "c",
            description: "Compose a new message.",
        },
        HelpEntry {
            section: "Compose",
            key: "Ctrl+Enter",
            description: "Send compose.",
        },
        HelpEntry {
            section: "Compose",
            key: "A",
            description: "Add an attachment in compose.",
        },
        HelpEntry {
            section: "Compose",
            key: "S",
            description: "Save draft in compose.",
        },
        HelpEntry {
            section: "Compose",
            key: "x",
            description: "Discard draft or changes in compose.",
        },
        HelpEntry {
            section: "Compose",
            key: "D",
            description: "Delete the opened local draft in compose.",
        },
        HelpEntry {
            section: "Menus",
            key: "Message menu",
            description: "Choose which message in a thread is selected.",
        },
        HelpEntry {
            section: "Menus",
            key: "Tag message menu",
            description: "Archive, mark, or add/remove a tag on the current message without changing the rest of its thread.",
        },
        HelpEntry {
            section: "Menus",
            key: "View menu",
            description: "Switch between text, HTML, headers, and raw source.",
        },
        HelpEntry {
            section: "Menus",
            key: "Copy menu",
            description: "Copy message/thread IDs and selected message fields.",
        },
        HelpEntry {
            section: "Menus",
            key: "Attachment double-click / right-click",
            description: "Open a thread attachment, or right-click to save or open it.",
        },
    ]
}

fn command_help_entries() -> &'static [HelpEntry] {
    &[
        HelpEntry {
            section: "Search commands",
            key: ":search",
            description: "Run the query currently typed in the search field.",
        },
        HelpEntry {
            section: "Search commands",
            key: ":inbox",
            description: "Open the Inbox saved search.",
        },
        HelpEntry {
            section: "Search commands",
            key: ":unread",
            description: "Open the Unread saved search.",
        },
        HelpEntry {
            section: "Search commands",
            key: ":flagged",
            description: "Open the Flagged saved search.",
        },
        HelpEntry {
            section: "Search commands",
            key: ":sent",
            description: "Open the Sent saved search.",
        },
        HelpEntry {
            section: "Search commands",
            key: ":drafts",
            description: "Open the Drafts saved search.",
        },
        HelpEntry {
            section: "Search commands",
            key: ":all",
            description: "Open the All saved search.",
        },
        HelpEntry {
            section: "Search commands",
            key: ":manual_sync",
            description: "Run the configured manual Sync action when Sync is enabled.",
        },
        HelpEntry {
            section: "Compose and response commands",
            key: ":compose",
            description: "Compose a new message.",
        },
        HelpEntry {
            section: "Compose and response commands",
            key: ":reply",
            description: "Reply to the selected message.",
        },
        HelpEntry {
            section: "Compose and response commands",
            key: ":reply_all",
            description: "Reply to all recipients on the selected message.",
        },
        HelpEntry {
            section: "Compose and response commands",
            key: ":forward",
            description: "Forward the selected message inline.",
        },
        HelpEntry {
            section: "Compose and response commands",
            key: ":forward_as_attachment",
            description: "Forward the selected message as a message/rfc822 attachment.",
        },
        HelpEntry {
            section: "Thread action commands",
            key: ":archive",
            description: "Remove the inbox tag from the selected thread(s).",
        },
        HelpEntry {
            section: "Thread action commands",
            key: ":mark_read",
            description: "Remove the unread tag from the selected thread(s).",
        },
        HelpEntry {
            section: "Thread action commands",
            key: ":mark_unread",
            description: "Add the unread tag to the selected thread(s).",
        },
        HelpEntry {
            section: "Thread action commands",
            key: ":flag",
            description: "Add the flagged tag to the selected thread(s).",
        },
        HelpEntry {
            section: "Thread action commands",
            key: ":unflag",
            description: "Remove the flagged tag from the selected thread(s).",
        },
        HelpEntry {
            section: "Thread action commands",
            key: ":trash",
            description: "Move the selected thread(s) to trash.",
        },
        HelpEntry {
            section: "Thread action commands",
            key: ":undo",
            description: "Undo the last tag operation.",
        },
        HelpEntry {
            section: "Thread action commands",
            key: ":visual_select",
            description: "Start visual thread selection from the selected row.",
        },
        HelpEntry {
            section: "Thread action commands",
            key: ":clear_visual_selection",
            description: "Clear the current visual thread selection.",
        },
        HelpEntry {
            section: "Message view commands",
            key: ":text",
            description: "Show the rendered text view for the selected message.",
        },
        HelpEntry {
            section: "Message view commands",
            key: ":visual_html",
            description: "Show the sanitized visual HTML view for the selected message.",
        },
        HelpEntry {
            section: "Message view commands",
            key: ":full_headers",
            description: "Show full message headers.",
        },
        HelpEntry {
            section: "Message view commands",
            key: ":raw_source",
            description: "Show the raw message source.",
        },
        HelpEntry {
            section: "Message view commands",
            key: ":image_policy",
            description: "Load remote images once for the current HTML message.",
        },
        HelpEntry {
            section: "Message view commands",
            key: ":load_images_once",
            description: "Load remote images once for the current HTML message.",
        },
        HelpEntry {
            section: "Message view commands",
            key: ":collapse_quotes",
            description: "Toggle collapsed quoted text.",
        },
        HelpEntry {
            section: "Message view commands",
            key: ":link_hints",
            description: "Label visible links and wait for a label key to open one externally.",
        },
        HelpEntry {
            section: "Thread list display commands",
            key: ":nu / :number",
            description: "Show thread numbers.",
        },
        HelpEntry {
            section: "Thread list display commands",
            key: ":nonu / :nonumber",
            description: "Hide thread numbers.",
        },
        HelpEntry {
            section: "Thread list display commands",
            key: ":date / :dates",
            description: "Show dates in the thread list.",
        },
        HelpEntry {
            section: "Thread list display commands",
            key: ":nodate / :nodates",
            description: "Hide dates in the thread list.",
        },
        HelpEntry {
            section: "Thread list display commands",
            key: ":tags",
            description: "Show tags in the thread list.",
        },
        HelpEntry {
            section: "Thread list display commands",
            key: ":notags",
            description: "Hide tags in the thread list.",
        },
        HelpEntry {
            section: "Thread list display commands",
            key: ":preview",
            description: "Show body previews in the thread list.",
        },
        HelpEntry {
            section: "Thread list display commands",
            key: ":nopreview",
            description: "Hide body previews in the thread list.",
        },
        HelpEntry {
            section: "Attachment and copy commands",
            key: ":save_attachment",
            description: "Save the selected attachment.",
        },
        HelpEntry {
            section: "Attachment and copy commands",
            key: ":open_attachment",
            description: "Open the selected attachment.",
        },
        HelpEntry {
            section: "Attachment and copy commands",
            key: ":copy_message_id",
            description: "Copy the selected message id.",
        },
        HelpEntry {
            section: "Attachment and copy commands",
            key: ":copy_thread_id",
            description: "Copy the selected thread id.",
        },
        HelpEntry {
            section: "Application commands",
            key: ":debug",
            description: "Toggle the debug panel.",
        },
        HelpEntry {
            section: "Application commands",
            key: ":layout / :layout_auto / :layout_columns / :layout_stacked",
            description: "Switch or select the window layout.",
        },
        HelpEntry {
            section: "Application commands",
            key: ":settings",
            description: "Open Settings.",
        },
        HelpEntry {
            section: "Application commands",
            key: ":help / :shortcuts / :commands",
            description: "Open this help window.",
        },
    ]
}

fn show_settings(widgets: &Widgets, state: &SharedState, options: &LaunchOptions) {
    let seed = settings_dialog_seed(options, widgets, state);
    let opts = options.clone();
    let w = widgets.clone();
    let st = state.clone();
    let apply = Rc::new(move |application| apply_settings_application(&opts, &w, &st, application));
    let status_label = widgets.status_label.clone();
    let status = Rc::new(move |text: String| status_label.set_text(&text));
    widgets.settings.show(seed, apply, status);
}

fn settings_dialog_seed(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
) -> SettingsDialogSeed {
    let state = state.borrow();
    SettingsDialogSeed {
        parent: widgets.window.clone(),
        app_config_path: options.app_config_path.clone(),
        database_path: options.database_path.clone(),
        notmuch_config_path: options.config_path.clone(),
        notmuch_profile: options.profile.clone(),
        default_query: options.default_query.clone(),
        runtime: settings::snapshot(&options.runtime_settings),
        identity_name: options.identity_name.clone(),
        primary_email: options.primary_email.clone(),
        other_email: options.other_email.clone(),
        requested_theme: state.theme,
        thread_preview_lines: state.thread_preview_lines,
        show_thread_numbers: state.show_thread_numbers,
        show_thread_dates: state.show_thread_dates,
        show_thread_tags: state.show_thread_tags,
        show_thread_preview: state.show_thread_preview,
        show_keybind_hints: state.show_keybind_hints,
        show_sidebar: pane_is_visible(widgets, ActivePane::Sidebar),
        show_message_list: pane_is_visible(widgets, ActivePane::Threads),
        show_message_view: pane_is_visible(widgets, ActivePane::Message),
        prefer_html_view: state.prefer_html_view,
        start_maximized: options.start_maximized,
        show_debug_panel: widgets.debug_view.is_visible(),
        hidden_tag_searches: widgets
            .hidden_tag_searches
            .borrow()
            .iter()
            .cloned()
            .collect(),
        send_enabled: options.send_enabled,
        send_command: options.send_command.clone(),
        send_args: options.send_args.clone(),
        send_mode: options.send_mode.clone(),
        send_working_dir: options.send_working_dir.clone(),
        send_env: options.send_env.clone(),
        send_timeout_seconds: options.send_timeout_seconds,
        save_sent: options.save_sent,
        sent_maildir: options.sent_maildir.clone(),
        sent_tags: options.sent_tags.clone(),
        index_sent_after_send: options.index_sent_after_send,
        save_drafts_to_maildir: options.save_drafts_to_maildir,
        draft_maildir: options.draft_maildir.clone(),
        draft_tags: options.draft_tags.clone(),
        index_draft_after_save: options.index_draft_after_save,
        sync_enabled: options.sync_enabled,
        manual_sync_label: options.manual_sync_label.clone(),
        notmuch_database_update_enabled: options.notmuch_database_update_enabled,
        notmuch_database_update_on_startup: options.notmuch_database_update_on_startup,
        notmuch_database_update_command: options.notmuch_database_update_command.clone(),
        external_receive_enabled: options.external_receive_enabled,
        external_receive_on_startup: options.external_receive_on_startup,
        external_receive_command: options.external_receive_command.clone(),
        automation_enabled: options.automation_enabled,
        automation_socket: options.automation_socket.clone(),
        automation_token: options.automation_token.clone(),
        screenshot_dir: options.screenshot_dir.clone(),
    }
}

fn apply_settings_application(
    options: &LaunchOptions,
    widgets: &Widgets,
    state: &SharedState,
    application: SettingsApplication,
) -> anyhow::Result<SettingsApplicationOutcome> {
    let previous_runtime = settings::snapshot(&options.runtime_settings);
    let next_runtime = application.runtime;
    let next_page_size = next_runtime.page_size;
    let next_theme = next_runtime.theme;
    let next_thread_preview_lines = next_runtime.thread_preview_lines;
    let next_layout_preference = next_runtime.layout_preference;
    let next_excluded_tags = next_runtime.excluded_tags.clone();
    let next_remote_images = next_runtime.remote_images;
    settings::update(&options.runtime_settings, next_runtime);

    {
        let mut state = state.borrow_mut();
        state.thread_page_size = next_page_size;
        state.theme = next_theme;
        state.thread_preview_lines = next_thread_preview_lines;
        state.show_thread_numbers = application.show_thread_numbers;
        state.show_thread_dates = application.show_thread_dates;
        state.show_thread_tags = application.show_thread_tags;
        state.show_thread_preview = application.show_thread_preview;
        state.show_keybind_hints = application.show_keybind_hints;
        state.layout_preference = next_layout_preference;
        state.prefer_html_view = application.prefer_html_view;
    }
    theme::apply_theme_preference(&widgets.gtk_settings, &widgets.css_provider, next_theme);
    widgets.theme_background_probe.queue_draw();
    *widgets.hidden_tag_searches.borrow_mut() = application.hidden_tag_searches;

    apply_pane_visibility_values(
        widgets,
        state,
        application.show_sidebar,
        application.show_message_list,
        application.show_message_view,
    );
    apply_layout_preference_for_current_size(widgets, state, next_layout_preference, false);
    widgets.debug_view.set_visible(application.show_debug_panel);
    widgets
        .thread_list
        .apply_model_update(&thread_model_snapshot(state), ThreadModelUpdate::Replace);
    update_tag_searches(options, widgets, state);
    update_thread_result_label(widgets, state);
    update_button_binding_labels(widgets, state);
    update_message_action_buttons(options, widgets, state);
    widgets
        .standalone_messages
        .refresh_remote_image_policy(previous_runtime.remote_images, next_remote_images);
    if html_view_is_visible(widgets) {
        let scroll = current_message_scroll_fraction(widgets);
        show_visual_html_selected_message(options, widgets, state);
        restore_message_scroll_fraction(widgets, scroll);
    } else {
        set_html_image_loading(
            &widgets.html_view,
            settings::remote_images(&options.runtime_settings),
        );
    }

    let search_reload_scheduled = previous_runtime.page_size != next_page_size
        || previous_runtime.excluded_tags != next_excluded_tags;
    if search_reload_scheduled {
        let query = state.borrow().current_query.clone();
        widgets.search_bar.set_query(&query);
        run_search(options, widgets, state, &query);
    }
    sync_pane_button_classes(widgets, state);
    update_active_pane_visuals(widgets, state);
    update_debug(widgets, state);
    Ok(SettingsApplicationOutcome {
        search_reload_scheduled,
    })
}

fn apply_pane_visibility_values(
    widgets: &Widgets,
    state: &SharedState,
    sidebar: bool,
    message_list: bool,
    message_view: bool,
) {
    let any_visible = sidebar || message_list || message_view;
    widgets.left_pane.set_visible(sidebar || !any_visible);
    widgets
        .thread_pane
        .set_visible(message_list || !any_visible);
    widgets
        .message_pane
        .set_visible(message_view || !any_visible);
    ensure_active_pane_visible(widgets, state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_load_completion_ignores_replaced_generations() {
        let mut lifecycle = HtmlLoadLifecycle::default();

        lifecycle.load_changed(webkit6::LoadEvent::Started, 0);
        assert_eq!(lifecycle.bind_main_resource(), Some(0));
        assert_eq!(lifecycle.finish_main_resource(0, 1), None);

        lifecycle.load_changed(webkit6::LoadEvent::Started, 1);
        assert_eq!(lifecycle.bind_main_resource(), Some(1));
        lifecycle.load_changed(webkit6::LoadEvent::Started, 2);
        assert_eq!(lifecycle.bind_main_resource(), Some(2));

        // Concrete replacement race: Started(g1), request g2, Started(g2),
        // then the old resource and WebView emit their stale Finished events.
        assert_eq!(lifecycle.finish_main_resource(1, 2), None);
        let before_stale_finish = lifecycle.clone();
        lifecycle.load_changed(webkit6::LoadEvent::Finished, 2);
        assert_eq!(lifecycle, before_stale_finish);

        assert_eq!(lifecycle.finish_main_resource(2, 2), Some(2));
        let after_current_resource = lifecycle.clone();
        lifecycle.load_changed(webkit6::LoadEvent::Finished, 2);
        assert_eq!(lifecycle, after_current_resource);
    }

    #[test]
    fn mailto_requests_map_to_editable_composer_fields() {
        let fields = compose_fields_from_mailto(
            "Fixture User <fixture@example.test>".to_string(),
            MailtoRequest {
                to: vec![
                    "one@example.test".to_string(),
                    "two@example.test".to_string(),
                ],
                cc: vec!["copy@example.test".to_string()],
                bcc: vec!["hidden@example.test".to_string()],
                subject: "Hello".to_string(),
                body: "Message body".to_string(),
            },
        );

        assert_eq!(
            fields,
            ComposeFields {
                from: "Fixture User <fixture@example.test>".to_string(),
                to: "one@example.test, two@example.test".to_string(),
                cc: "copy@example.test".to_string(),
                bcc: "hidden@example.test".to_string(),
                subject: "Hello".to_string(),
                body: "Message body".to_string(),
                ..ComposeFields::default()
            }
        );
    }

    #[test]
    fn launch_validation_rejects_invalid_or_conflicting_open_targets() {
        let invalid_mailto = LaunchOptions {
            mailto_uri: Some("https://example.test".to_string()),
            ..LaunchOptions::default()
        };
        assert!(
            validate_launch_options(&invalid_mailto)
                .expect_err("non-mailto URI should be rejected")
                .to_string()
                .contains("invalid mailto URI")
        );

        let conflicting = LaunchOptions {
            open_message_id: Some("message@example.test".to_string()),
            mailto_uri: Some("mailto:person@example.test".to_string()),
            ..LaunchOptions::default()
        };
        assert!(
            validate_launch_options(&conflicting)
                .expect_err("two launch targets should be rejected")
                .to_string()
                .contains("cannot be combined")
        );
    }

    #[test]
    fn message_view_preferences_use_message_then_sender_then_global_precedence() {
        let mut by_message = BTreeMap::new();
        let mut by_sender = BTreeMap::new();
        by_sender.insert(
            "sender@example.test".to_string(),
            MessageViewPreference::FullHeaders,
        );

        assert_eq!(
            resolve_message_view_preference(
                true,
                &by_message,
                &by_sender,
                "message@example.test",
                Some("Sender@Example.Test"),
                true,
            ),
            MessageViewPreference::FullHeaders
        );

        by_message.insert(
            "message@example.test".to_string(),
            MessageViewPreference::RawSource,
        );
        assert_eq!(
            resolve_message_view_preference(
                false,
                &by_message,
                &by_sender,
                " message@example.test ",
                Some("sender@example.test"),
                true,
            ),
            MessageViewPreference::RawSource
        );

        assert_eq!(
            resolve_message_view_preference(
                true,
                &BTreeMap::new(),
                &BTreeMap::new(),
                "new@example.test",
                None,
                true,
            ),
            MessageViewPreference::VisualHtml
        );
    }

    #[test]
    fn visual_preference_falls_back_to_text_for_plain_messages() {
        let by_message = BTreeMap::from([(
            "plain@example.test".to_string(),
            MessageViewPreference::VisualHtml,
        )]);
        assert_eq!(
            resolve_message_view_preference(
                true,
                &by_message,
                &BTreeMap::new(),
                "plain@example.test",
                None,
                false,
            ),
            MessageViewPreference::Text
        );
    }

    #[test]
    fn message_header_values_stay_single_line_for_compact_pane_height() {
        assert_eq!(MESSAGE_HEADER_VALUE_LINES, 1);
    }

    #[test]
    fn uppercase_message_navigation_preserves_lowercase_scroll_keys() {
        let none = gtk::gdk::ModifierType::empty();
        let shift = gtk::gdk::ModifierType::SHIFT_MASK;
        assert_eq!(message_navigation_delta(gtk::gdk::Key::J, none), Some(1));
        assert_eq!(message_navigation_delta(gtk::gdk::Key::K, none), Some(-1));
        assert_eq!(message_navigation_delta(gtk::gdk::Key::j, shift), Some(1));
        assert_eq!(message_navigation_delta(gtk::gdk::Key::k, shift), Some(-1));
        assert_eq!(message_navigation_delta(gtk::gdk::Key::j, none), None);
        assert_eq!(message_navigation_delta(gtk::gdk::Key::k, none), None);
        assert!(is_message_tag_menu_key(gtk::gdk::Key::M, none));
        assert!(is_message_tag_menu_key(gtk::gdk::Key::m, shift));
        assert!(!is_message_tag_menu_key(gtk::gdk::Key::m, none));
        assert_eq!(
            message_tag_sequence_key_action(gtk::gdk::Key::a, none),
            Some(MessageTagSequenceKeyAction::Archive)
        );
        assert_eq!(
            message_tag_sequence_key_action(gtk::gdk::Key::u, none),
            Some(MessageTagSequenceKeyAction::ToggleRead)
        );
        assert_eq!(
            message_tag_sequence_key_action(gtk::gdk::Key::f, none),
            Some(MessageTagSequenceKeyAction::ToggleFlag)
        );
        assert_eq!(
            message_tag_sequence_key_action(gtk::gdk::Key::t, none),
            Some(MessageTagSequenceKeyAction::Trash)
        );
        assert_eq!(
            message_tag_sequence_key_action(gtk::gdk::Key::s, none),
            Some(MessageTagSequenceKeyAction::Spam)
        );
        assert_eq!(
            message_tag_sequence_key_action(gtk::gdk::Key::T, none),
            Some(MessageTagSequenceKeyAction::CustomTag)
        );
        assert_eq!(
            message_tag_sequence_key_action(gtk::gdk::Key::t, shift),
            Some(MessageTagSequenceKeyAction::CustomTag)
        );
        for key in [gtk::gdk::Key::m, gtk::gdk::Key::x, gtk::gdk::Key::A] {
            assert_eq!(message_tag_sequence_key_action(key, none), None);
        }
        assert_eq!(relative_message_index(1, 3, 1), Some(2));
        assert_eq!(relative_message_index(1, 3, -1), Some(0));
        assert_eq!(relative_message_index(2, 3, 8), Some(2));
        assert_eq!(relative_message_index(0, 3, -8), Some(0));
        assert_eq!(relative_message_index(0, 0, 1), None);
    }

    #[test]
    fn composer_shortcuts_accept_physical_shifted_and_uppercase_keyvals() {
        let none = gtk::gdk::ModifierType::empty();
        let shift = gtk::gdk::ModifierType::SHIFT_MASK;
        for (lowercase, uppercase, action) in [
            (
                gtk::gdk::Key::a,
                gtk::gdk::Key::A,
                ComposerShortcutAction::AddAttachment,
            ),
            (
                gtk::gdk::Key::s,
                gtk::gdk::Key::S,
                ComposerShortcutAction::SaveDraft,
            ),
            (
                gtk::gdk::Key::d,
                gtk::gdk::Key::D,
                ComposerShortcutAction::DeleteLocalDraft,
            ),
        ] {
            assert_eq!(composer_shortcut_action(uppercase, none), Some(action));
            assert_eq!(composer_shortcut_action(lowercase, shift), Some(action));
            assert_eq!(composer_shortcut_action(lowercase, none), None);
        }
        assert_eq!(
            composer_shortcut_action(gtk::gdk::Key::x, none),
            Some(ComposerShortcutAction::ClearDraft)
        );
        assert_eq!(composer_shortcut_action(gtk::gdk::Key::x, shift), None);
        assert_eq!(
            composer_shortcut_action(
                gtk::gdk::Key::s,
                shift | gtk::gdk::ModifierType::CONTROL_MASK,
            ),
            None
        );
    }

    #[test]
    fn injected_shortcuts_parse_gdk_names_and_reject_ambiguous_input() {
        let (key, modifiers) = injected_shortcut(&json!({
            "key": "J",
            "modifiers": ["shift", "ctrl"],
        }))
        .expect("named shortcut");
        assert_eq!(key, gtk::gdk::Key::J);
        assert!(modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK));
        assert!(modifiers.contains(gtk::gdk::ModifierType::CONTROL_MASK));
        assert_eq!(shortcut_modifier_names(modifiers), vec!["shift", "control"]);

        assert_eq!(
            injected_shortcut(&json!({"key": "Enter"}))
                .expect("common Enter alias")
                .0,
            gtk::gdk::Key::Return
        );
        assert!(
            injected_shortcut(&json!({"key": "j", "modifiers": [42]}))
                .expect_err("non-string modifier")
                .to_string()
                .contains("only strings")
        );
        assert!(
            injected_shortcut(&json!({"key": "j", "modifiers": ["hyper"]}))
                .expect_err("unknown modifier")
                .to_string()
                .contains("unknown key modifier")
        );
        assert!(injected_shortcut(&json!({})).is_err());
    }

    #[test]
    fn current_message_tag_projection_keeps_other_message_tags_in_thread_summary() {
        let message = |id: &str, thread_id: &str, tags: &[&str]| notm_notmuch::MessageSummary {
            message_id: id.to_string(),
            thread_id: thread_id.to_string(),
            date: 0,
            from: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            filenames: Vec::new(),
        };
        let messages = vec![
            message("one", "thread", &["inbox", "unread"]),
            message("two", "thread", &["flagged"]),
            message("other", "other-thread", &["spam"]),
        ];

        assert_eq!(
            aggregate_thread_tags(&messages, "thread"),
            vec![
                "flagged".to_string(),
                "inbox".to_string(),
                "unread".to_string()
            ]
        );
    }

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

    #[cfg(unix)]
    #[test]
    fn maildir_persistence_creates_private_directories_and_message() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temporary parent");
        let maildir = temp.path().join("Sent");
        let message = ComposedMessage::new(
            "sender@example.test".to_string(),
            vec!["recipient@example.test".to_string()],
            "private persistence".to_string(),
            "body".to_string(),
        );

        let path =
            save_rfc5322_to_maildir(&maildir, &message, "S").expect("persist message to Maildir");

        for directory in [
            &maildir,
            &maildir.join("tmp"),
            &maildir.join("cur"),
            &maildir.join("new"),
        ] {
            let mode = std::fs::metadata(directory)
                .expect("private Maildir metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{directory:?} exposed group/other bits");
        }
        let mode = std::fs::metadata(&path)
            .expect("private message metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o177, 0, "{path:?} was not a regular mode-0600 file");
    }

    #[cfg(unix)]
    #[test]
    fn undo_history_is_replaced_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("temporary state root");
        let state_directory = temp.path().join("notm");
        std::fs::create_dir(&state_directory).expect("create state directory");
        std::fs::set_permissions(&state_directory, std::fs::Permissions::from_mode(0o755))
            .expect("seed non-private state directory");
        let path = state_directory.join("tag-undo.json");
        std::fs::write(&path, "old history").expect("seed undo history");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("seed non-private undo history");

        persist_undo_tag_actions_to_path(&path, &[]).expect("replace undo history");

        assert_eq!(
            std::fs::metadata(&state_directory)
                .expect("state directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path)
                .expect("undo history metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let stored: UndoTagHistory =
            serde_json::from_slice(&std::fs::read(path).expect("read undo history"))
                .expect("parse undo history");
        assert!(stored.actions.is_empty());
    }

    #[test]
    fn sent_persistence_defaults_to_mail_root_before_database_path() {
        let temp = tempfile::tempdir().expect("temporary parent");
        let mail_root = temp.path().join("mail-root");
        let database_path = temp.path().join("separate-index");
        let options = LaunchOptions {
            database_path: Some(database_path.clone()),
            mail_root: Some(mail_root.clone()),
            save_sent: true,
            ..LaunchOptions::default()
        };
        let message = ComposedMessage::new(
            "sender@example.test".to_string(),
            vec!["recipient@example.test".to_string()],
            "split database".to_string(),
            "body".to_string(),
        );

        let persisted = persist_sent_message(&options, &message)
            .expect("persist sent message")
            .expect("sent persistence enabled");

        assert!(persisted.path.starts_with(mail_root.join("Sent/cur")));
        assert!(!persisted.path.starts_with(database_path));
    }

    #[test]
    fn test_harness_live_mutations_require_explicit_gates() {
        let mut options = LaunchOptions::default();

        let send_error = ensure_automation_request_allowed(&options, "compose_send", &json!({}))
            .expect_err("live harness send should be gated");
        assert!(send_error.to_string().contains("allow_live_send_test=true"));

        let tag_error = ensure_automation_request_allowed(&options, "archive_selected", &json!({}))
            .expect_err("live harness tag should be gated");
        assert!(tag_error.to_string().contains("allow_live_tag_test=true"));

        let message_tag_error = ensure_automation_request_allowed(
            &options,
            "click_message_tag_action",
            &json!({"action": "archive"}),
        )
        .expect_err("live harness current-message tag should be gated");
        assert!(
            message_tag_error
                .to_string()
                .contains("allow_live_tag_test=true")
        );

        let nested_error = ensure_automation_request_allowed(
            &options,
            "run_command",
            &json!({"command": ":archive"}),
        )
        .expect_err("nested tag command should not bypass the gate");
        assert!(
            nested_error
                .to_string()
                .contains("allow_live_tag_test=true")
        );

        options.allow_live_send_test = true;
        options.allow_live_tag_test = true;
        ensure_automation_request_allowed(&options, "compose_send", &json!({}))
            .expect("explicit send gate");
        ensure_automation_request_allowed(&options, "archive_selected", &json!({}))
            .expect("explicit tag gate");
        ensure_automation_request_allowed(&options, "run_command", &json!({"command": ":archive"}))
            .expect("explicit tag gate should cover nested command");
    }

    #[test]
    fn fixture_harness_allows_disposable_mutations_but_never_sync() {
        let options = LaunchOptions {
            fixture_mode: true,
            ..LaunchOptions::default()
        };

        ensure_automation_request_allowed(&options, "compose_send", &json!({}))
            .expect("fixture fake send");
        ensure_automation_request_allowed(&options, "archive_selected", &json!({}))
            .expect("fixture tag mutation");
        let direct = ensure_automation_request_allowed(&options, "run_manual_sync", &json!({}))
            .expect_err("fixture sync should be blocked");
        assert!(direct.to_string().contains("disabled in fixture mode"));
        let nested = ensure_automation_request_allowed(
            &options,
            "run_command",
            &json!({"command": ":sync"}),
        )
        .expect_err("nested fixture sync should be blocked");
        assert!(nested.to_string().contains("disabled in fixture mode"));
    }

    #[test]
    fn injected_shortcuts_are_fixture_only() {
        let live_options = LaunchOptions {
            allow_live_send_test: true,
            allow_live_tag_test: true,
            ..LaunchOptions::default()
        };
        let error = ensure_automation_request_allowed(&live_options, "send_key", &json!({}))
            .expect_err("arbitrary live shortcut routing must stay disabled");
        assert!(error.to_string().contains("available only in fixture mode"));

        let fixture_options = LaunchOptions {
            fixture_mode: true,
            ..LaunchOptions::default()
        };
        ensure_automation_request_allowed(&fixture_options, "send_key", &json!({}))
            .expect("fixture shortcut routing");
    }

    #[test]
    fn custom_tag_entry_harness_commands_keep_their_stable_names_and_tag_gate() {
        assert_eq!(
            [
                ADD_CUSTOM_TAG_FROM_ENTRY_COMMAND,
                REMOVE_CUSTOM_TAG_FROM_ENTRY_COMMAND,
            ],
            ["add_custom_tag_from_entry", "remove_custom_tag_from_entry"]
        );

        let live_options = LaunchOptions::default();
        for command in [
            ADD_CUSTOM_TAG_FROM_ENTRY_COMMAND,
            REMOVE_CUSTOM_TAG_FROM_ENTRY_COMMAND,
        ] {
            let error = ensure_automation_request_allowed(&live_options, command, &json!({}))
                .expect_err("stable custom-tag command must remain a gated tag mutation");
            assert!(error.to_string().contains("allow_live_tag_test=true"));
        }
    }

    #[test]
    fn attachment_dialog_test_controls_are_fixture_only() {
        let live_options = LaunchOptions::default();
        let error =
            ensure_automation_request_allowed(&live_options, "attachment_test_state", &json!({}))
                .expect_err("live harness must not expose attachment dialog internals");
        assert!(error.to_string().contains("available only in fixture mode"));

        let fixture_options = LaunchOptions {
            fixture_mode: true,
            ..LaunchOptions::default()
        };
        ensure_automation_request_allowed(&fixture_options, "attachment_test_state", &json!({}))
            .expect("fixture state inspection");
        ensure_automation_request_allowed(&fixture_options, "respond_attachment_save", &json!({}))
            .expect("fixture chooser response");
    }

    #[test]
    fn attachment_result_preserves_current_tags_after_local_mutation() {
        let message = notm_notmuch::MessageSummary {
            message_id: "attachment-message@example.test".to_string(),
            thread_id: "attachment-thread".to_string(),
            date: 0,
            from: "Sender <sender@example.test>".to_string(),
            to: "Recipient <recipient@example.test>".to_string(),
            cc: String::new(),
            subject: "Attachment".to_string(),
            tags: vec!["inbox".to_string(), "unread".to_string()],
            filenames: vec!["/tmp/attachment-message.eml".to_string()],
        };
        let result = AttachmentActionResult {
            message_id: message.message_id.clone(),
            path: PathBuf::from("/tmp/saved-attachment.txt"),
            status: "Attachment saved".to_string(),
            operation: "saved attachment".to_string(),
        };
        let mut state = UiState {
            messages: vec![message.clone()],
            selected_message: Some(message),
            ..UiState::default()
        };
        let mutation = TagMutation {
            add: vec!["flagged".to_string()],
            remove: vec!["unread".to_string()],
            sync_maildir_flags: false,
        };
        apply_tag_mutation_to_tags(&mut state.messages[0].tags, &mutation);
        apply_tag_mutation_to_tags(
            &mut state
                .selected_message
                .as_mut()
                .expect("selected attachment message")
                .tags,
            &mutation,
        );
        let expected_tags = state.messages[0].tags.clone();

        record_attachment_action_result(&mut state, &result.message_id, result.operation);

        assert_eq!(
            state
                .selected_message
                .as_ref()
                .expect("attachment action keeps the selected message")
                .tags,
            expected_tags
        );
        assert_eq!(state.last_error, None);
        assert_eq!(state.last_operation.as_deref(), Some("saved attachment"));
    }

    #[test]
    fn draft_list_and_confirmation_test_controls_are_narrowly_gated() {
        let live_options = LaunchOptions::default();
        for command in [
            "draft_list_state",
            "activate_draft_by_index",
            "click_delete_selected_draft",
        ] {
            let error = ensure_automation_request_allowed(&live_options, command, &json!({}))
                .expect_err("live harness must not expose draft-list widget controls");
            assert!(error.to_string().contains("available only in fixture mode"));
        }
        for command in ["pending_confirmation", "respond_confirmation"] {
            let error = ensure_automation_request_allowed(&live_options, command, &json!({}))
                .expect_err("live confirmation controls must require the send gate");
            assert!(error.to_string().contains("allow_live_send_test=true"));
        }

        let live_send_options = LaunchOptions {
            allow_live_send_test: true,
            ..LaunchOptions::default()
        };
        for command in ["pending_confirmation", "respond_confirmation"] {
            ensure_automation_request_allowed(&live_send_options, command, &json!({}))
                .expect("the dispatch layer performs the exact pending-Send check");
        }
        for command in ["pending_confirmation", "respond_confirmation"] {
            ensure_confirmation_control_allowed(&live_send_options, true, command)
                .expect("gated live controls should drive the exact pending Send");
            let no_pending =
                ensure_confirmation_control_allowed(&live_send_options, false, command)
                    .expect_err("gated controls must not inspect an absent live confirmation");
            assert!(no_pending.to_string().contains("pending saved-draft Send"));
            let wrong_action =
                ensure_confirmation_control_allowed(&live_send_options, false, command)
                    .expect_err("gated controls must not drive a non-Send confirmation");
            assert!(
                wrong_action
                    .to_string()
                    .contains("pending saved-draft Send")
            );
        }

        let fixture_options = LaunchOptions {
            fixture_mode: true,
            ..LaunchOptions::default()
        };
        for command in [
            "draft_list_state",
            "activate_draft_by_index",
            "click_delete_selected_draft",
            "pending_confirmation",
            "respond_confirmation",
        ] {
            ensure_automation_request_allowed(&fixture_options, command, &json!({}))
                .expect("fixture draft-list UI control");
        }
    }

    #[test]
    fn pending_saved_send_revalidates_new_background_activity() {
        let action = PendingTransition::SendComposer {
            fields: ComposeFields::default(),
            active: ActiveDraft {
                path: PathBuf::from("/tmp/gated-send-draft.json"),
                message_id: None,
                indexed: false,
                saved_fields: ComposeFields::default(),
            },
            generation: 7,
        };
        assert_eq!(action.operation(), UserOperation::Send);
        assert!(
            background_activity_block_reason(true, false, action.operation()).is_some(),
            "accepting a pending saved-draft Send must revalidate a newly started sync"
        );
    }

    #[test]
    fn pending_confirmation_automation_gate_is_read_only_except_for_its_response() {
        for command in [
            "health",
            "app_state",
            "draft_list_state",
            "pending_confirmation",
            "respond_confirmation",
            "html_view_state",
            "attachment_test_state",
        ] {
            assert!(
                automation_command_allowed_while_confirmation_pending(command),
                "{command} should remain available while inspecting the modal"
            );
        }
        for command in [
            "compose_set_subject",
            "compose_add_attachment",
            "save_draft",
            "load_draft",
            "delete_active_draft",
            "clear_draft",
            "compose_send",
            "reply_selected",
            "tag_selected",
            "run_manual_sync",
            "respond_settings",
            "send_key",
            "close_main_window",
            "run_command",
        ] {
            assert!(
                !automation_command_allowed_while_confirmation_pending(command),
                "{command} must not bypass GTK modality through the harness"
            );
        }
    }

    #[test]
    fn full_search_outcome_is_not_replaced_by_an_unrelated_page_error() {
        let mut state = UiState {
            full_search_outcome_generation: 5,
            full_search_outcome_error: Some("full search failed".to_string()),
            ..UiState::default()
        };
        assert_eq!(
            full_search_outcome_at_or_after(&state, 4),
            Some(Err("full search failed".to_string()))
        );
        assert_eq!(full_search_outcome_at_or_after(&state, 6), None);
        assert_eq!(state.pending_search_query, None);

        state.search_error = Some("unrelated page load failed".to_string());
        state.full_search_outcome_error = None;
        assert_eq!(
            full_search_outcome_at_or_after(&state, 5),
            Some(Ok(())),
            "an unrelated mutable search error replaced the recorded full-search outcome"
        );
    }

    #[test]
    fn delayed_sync_refresh_work_is_scoped_to_non_fixture_harnesses() {
        let harness = LaunchOptions {
            automation_enabled: true,
            ..LaunchOptions::default()
        };
        assert_eq!(
            sync_refresh_worker_delay(&harness, &json!({"test_refresh_delay_ms": 250}))
                .expect("non-fixture harness delay"),
            Duration::from_millis(250)
        );
        for options in [
            LaunchOptions::default(),
            LaunchOptions {
                automation_enabled: true,
                fixture_mode: true,
                ..LaunchOptions::default()
            },
        ] {
            assert!(
                sync_refresh_worker_delay(&options, &json!({"test_refresh_delay_ms": 1}))
                    .unwrap_err()
                    .to_string()
                    .contains("non-fixture test-harness syncs")
            );
        }
        assert!(
            sync_refresh_worker_delay(&harness, &json!({"test_refresh_delay_ms": 5001}))
                .unwrap_err()
                .to_string()
                .contains("must not exceed")
        );
    }

    #[test]
    fn fixture_send_uses_fake_capture_even_if_an_external_command_is_present() {
        let capture_dir = std::env::temp_dir().join(format!(
            "notm-fixture-send-policy-{}",
            Uuid::new_v4().simple()
        ));
        let options = LaunchOptions {
            fixture_mode: true,
            fake_send_capture_dir: Some(capture_dir.clone()),
            send_command: Some(PathBuf::from("notm-command-that-must-not-run")),
            ..LaunchOptions::default()
        };
        let message = ComposedMessage::new(
            "sender@example.test".to_string(),
            vec!["recipient@example.test".to_string()],
            "fixture policy".to_string(),
            "body".to_string(),
        );

        let report = send_message_with_config(&options, message).expect("fixture fake send");

        assert!(report.accepted);
        assert!(
            report
                .captured_path
                .as_ref()
                .is_some_and(|path| Path::new(path).is_file())
        );
        let _ = std::fs::remove_dir_all(capture_dir);
    }

    #[test]
    fn fixture_mode_produces_no_external_sync_commands() {
        let options = LaunchOptions {
            fixture_mode: true,
            sync_enabled: true,
            external_receive_enabled: true,
            external_receive_on_startup: true,
            external_receive_command: "must-not-run".to_string(),
            notmuch_database_update_enabled: true,
            notmuch_database_update_on_startup: true,
            notmuch_database_update_command: "must-not-run".to_string(),
            ..LaunchOptions::default()
        };

        assert!(sync_command_specs(&options, SyncRunKind::Manual).is_empty());
        assert!(sync_command_specs(&options, SyncRunKind::Startup).is_empty());
    }

    #[test]
    fn visual_html_document_uses_light_default_canvas() {
        let document = visual_html_document("<p>Hello</p>", false);

        assert!(document.contains(r#"<meta name="color-scheme" content="light">"#));
        assert!(document.contains("default-src 'none'; img-src 'none'"));
        assert!(document.contains("background: #ffffff;"));
        assert!(document.contains("color: #111111;"));
        assert!(!document.contains("CanvasText"));
    }

    #[test]
    fn visual_html_document_only_opens_http_images_for_explicit_image_loading() {
        let blocked = visual_html_document("<p>Blocked</p>", false);
        let allowed = visual_html_document("<p>Allowed once</p>", true);

        assert!(blocked.contains("img-src 'none'"));
        assert!(!blocked.contains("img-src http: https:"));
        assert!(allowed.contains("img-src http: https:"));
        for document in [&blocked, &allowed] {
            assert!(document.contains("script-src 'none'"));
            assert!(document.contains("connect-src 'none'"));
            assert!(document.contains("frame-src 'none'"));
            assert!(document.contains("object-src 'none'"));
            assert!(document.contains("base-uri 'none'"));
            assert!(document.contains("form-action 'none'"));
        }
    }

    #[test]
    fn blocked_visual_html_removes_direct_and_alternate_remote_resource_markup() {
        let html = r#"
            <IMG SRC="https://tracker.test/direct" SRCSET="https://tracker.test/srcset 2x">
            <div style="background:url(https://tracker.test/css-inline)">inline</div>
            <style>@import url(https://tracker.test/css-import)</style>
            <picture><source srcset="https://tracker.test/source"><img src="https://tracker.test/nested-img"></picture>
            <iframe src="https://tracker.test/frame" srcdoc="<img src='https://tracker.test/srcdoc'>"></iframe>
            <object data="https://tracker.test/object"></object>
            <embed src="https://tracker.test/embed">
            <link rel="stylesheet" href="https://tracker.test/stylesheet">
            <meta http-equiv="refresh" content="0;url=https://tracker.test/refresh">
            <svg><image href="https://tracker.test/svg"></image></svg>
        "#;

        let sanitized = sanitize_html_for_visual(html, false);

        assert_eq!(sanitized.matches("[image blocked]").count(), 2);
        for forbidden in [
            "tracker.test/direct",
            "tracker.test/srcset",
            "tracker.test/css-inline",
            "tracker.test/css-import",
            "tracker.test/source",
            "tracker.test/nested-img",
            "tracker.test/frame",
            "tracker.test/srcdoc",
            "tracker.test/object",
            "tracker.test/embed",
            "tracker.test/stylesheet",
            "tracker.test/refresh",
            "tracker.test/svg",
        ] {
            assert!(
                !sanitized.contains(forbidden),
                "blocked HTML retained remote resource URL {forbidden}: {sanitized}"
            );
        }
    }

    #[test]
    fn one_shot_visual_html_keeps_only_sanitized_direct_image_sources() {
        let html = r#"
            <img src="https://tracker.test/direct" srcset="https://tracker.test/srcset 2x">
            <div style="background-image:url(https://tracker.test/css)">body</div>
            <iframe src="https://tracker.test/frame"></iframe>
        "#;

        let sanitized = sanitize_html_for_visual(html, true);

        assert!(sanitized.contains("https://tracker.test/direct"));
        assert!(!sanitized.contains("tracker.test/srcset"));
        assert!(!sanitized.contains("tracker.test/css"));
        assert!(!sanitized.contains("tracker.test/frame"));
    }

    #[test]
    fn html_link_status_keeps_short_uri_visible() {
        let uri = "https://example.test/message";

        assert_eq!(
            html_link_opened_status(uri),
            "Opened link externally: https://example.test/message"
        );
        assert_eq!(
            html_link_hover_status(uri),
            "Link: https://example.test/message"
        );
    }

    #[test]
    fn html_link_status_truncates_long_tracking_uri() {
        let uri = format!("https://example.test/{}", "tracking".repeat(40));
        let status = html_link_opened_status(&uri);

        assert!(status.starts_with("Opened link externally: https://example.test/"));
        assert!(status.ends_with('…'));
        assert!(status.chars().count() < uri.chars().count());
    }

    #[test]
    fn normal_launch_uses_stable_single_instance_application_id() {
        let options = LaunchOptions::default();

        assert_eq!(
            application_id_for_launch(&options).expect("application ID"),
            NORMAL_APPLICATION_ID
        );
        assert!(application_flags_for_launch(&options).is_empty());
        assert!(
            !application_flags_for_launch(&options)
                .contains(gtk::gio::ApplicationFlags::NON_UNIQUE)
        );
    }

    #[test]
    fn test_harness_launch_uses_per_process_valid_application_id() {
        let options = LaunchOptions {
            automation_enabled: true,
            ..LaunchOptions::default()
        };
        let app_id = application_id_for_launch(&options).expect("application ID");

        assert_ne!(app_id, NORMAL_APPLICATION_ID);
        assert!(app_id.starts_with(TEST_HARNESS_APPLICATION_ID_PREFIX));
        assert!(gtk::gio::Application::id_is_valid(&app_id));
        assert!(application_flags_for_launch(&options).is_empty());
    }

    #[test]
    fn message_id_launch_keeps_stable_single_instance_application_id() {
        let options = LaunchOptions {
            open_message_id: Some("abc@example.test".to_string()),
            ..LaunchOptions::default()
        };

        assert_eq!(
            application_id_for_launch(&options).expect("application ID"),
            NORMAL_APPLICATION_ID
        );
        assert!(application_flags_for_launch(&options).is_empty());
        assert!(
            !application_flags_for_launch(&options)
                .contains(gtk::gio::ApplicationFlags::NON_UNIQUE)
        );
    }

    #[test]
    fn new_window_message_id_prefers_request_and_preserves_cold_launch_target() {
        assert_eq!(
            resolved_new_window_message_id(None, Some("cold@example.test")),
            Some("cold@example.test".to_string())
        );
        assert_eq!(
            resolved_new_window_message_id(
                Some("request@example.test".to_string()),
                Some("cold@example.test")
            ),
            Some("request@example.test".to_string())
        );
        assert_eq!(resolved_new_window_message_id(None, None), None);
    }

    #[test]
    fn message_id_query_uses_notmuch_id_term() {
        assert_eq!(message_id_query("abc@example.test"), "id:abc@example.test");
    }

    #[test]
    fn message_id_query_quotes_special_values() {
        assert_eq!(
            message_id_query("abc+reply@example.test"),
            "id:\"abc+reply@example.test\""
        );
    }

    #[test]
    fn launch_theme_and_preview_lines_propagate_to_runtime_settings() {
        let options = LaunchOptions {
            theme: ThemePreference::Dark,
            thread_preview_lines: 7,
            ..LaunchOptions::default()
        };

        validate_launch_options(&options).expect("valid preview limit");
        sync_runtime_settings_from_launch_options(&options);

        assert_eq!(
            settings::theme(&options.runtime_settings),
            ThemePreference::Dark
        );
        assert_eq!(settings::thread_preview_lines(&options.runtime_settings), 7);
    }

    #[test]
    fn launch_rejects_out_of_range_thread_preview_lines_before_gtk_startup() {
        for thread_preview_lines in [0, crate::model::MAX_THREAD_PREVIEW_LINES + 1] {
            let options = LaunchOptions {
                thread_preview_lines,
                ..LaunchOptions::default()
            };
            let error = launch(options).expect_err("invalid launch options must fail");
            assert!(
                error.to_string().contains("thread preview lines"),
                "unexpected error: {error:#}"
            );
        }
    }

    #[test]
    fn auto_layout_uses_width_thresholds() {
        assert_eq!(
            layout_for_preference(
                LayoutPreference::Auto,
                AUTO_STACKED_BELOW_WIDTH - 1,
                900,
                ContentLayout::ThreePane,
            ),
            ContentLayout::Stacked
        );
        assert_eq!(
            layout_for_preference(
                LayoutPreference::Auto,
                AUTO_THREE_PANE_ABOVE_WIDTH + 1,
                900,
                ContentLayout::Stacked,
            ),
            ContentLayout::ThreePane
        );
    }

    #[test]
    fn layout_toggle_cycles_through_explicit_and_auto_preferences() {
        assert_eq!(
            next_layout_preference(LayoutPreference::ThreePane),
            LayoutPreference::Stacked
        );
        assert_eq!(
            next_layout_preference(LayoutPreference::Stacked),
            LayoutPreference::Auto
        );
        assert_eq!(
            next_layout_preference(LayoutPreference::Auto),
            LayoutPreference::ThreePane
        );
    }

    #[test]
    fn layout_status_names_auto_when_preference_does_not_change_visible_layout() {
        assert_eq!(
            layout_status_text(LayoutPreference::Auto, ContentLayout::ThreePane),
            "Layout: auto (side-by-side columns)"
        );
        assert_eq!(
            layout_status_text(LayoutPreference::Stacked, ContentLayout::Stacked),
            "Layout: stacked top panes"
        );
    }

    #[test]
    fn default_content_split_preserves_thread_list_before_message_view() {
        assert_eq!(
            default_content_split_for_width(THREAD_LIST_MIN_WIDTH + MESSAGE_VIEW_MIN_WIDTH + 80),
            THREAD_LIST_MIN_WIDTH + 20
        );
        assert_eq!(
            default_content_split_for_width(THREAD_LIST_MIN_WIDTH + MESSAGE_VIEW_MIN_WIDTH - 40),
            THREAD_LIST_MIN_WIDTH
        );
        assert_eq!(
            default_content_split_for_width(THREAD_LIST_MIN_WIDTH - 20),
            THREAD_LIST_MIN_WIDTH - 20
        );
    }

    #[test]
    fn button_label_shows_keybind_hint_by_default_in_normal_mode() {
        let state = Rc::new(RefCell::new(UiState::default()));

        assert_eq!(button_label("Archive", "a", &state), "Archive (a)");
    }

    #[test]
    fn button_label_hides_keybind_hint_when_setting_is_off() {
        let state = Rc::new(RefCell::new(UiState {
            show_keybind_hints: false,
            ..UiState::default()
        }));

        assert_eq!(button_label("Archive", "a", &state), "Archive");
    }

    #[test]
    fn button_label_hides_keybind_hint_outside_normal_mode() {
        let state = Rc::new(RefCell::new(UiState {
            input_mode: InputMode::Insert,
            ..UiState::default()
        }));

        assert_eq!(button_label("Archive", "a", &state), "Archive");
    }

    #[test]
    fn input_mode_transition_reports_only_real_changes() {
        let mut state = UiState::default();

        assert!(apply_input_mode(&mut state, InputMode::Insert));
        assert_eq!(state.input_mode, InputMode::Insert);
        assert!(!apply_input_mode(&mut state, InputMode::Insert));
        assert!(apply_input_mode(&mut state, InputMode::Normal));
        assert_eq!(state.input_mode, InputMode::Normal);
    }

    #[test]
    fn normal_text_focus_blocks_mail_actions_but_keeps_modal_navigation() {
        for key in [gtk::gdk::Key::a, gtk::gdk::Key::t, gtk::gdk::Key::s] {
            assert!(normal_text_focus_blocks_key(key));
        }
        for key in [
            gtk::gdk::Key::h,
            gtk::gdk::Key::j,
            gtk::gdk::Key::k,
            gtk::gdk::Key::l,
            gtk::gdk::Key::i,
            gtk::gdk::Key::slash,
        ] {
            assert!(!normal_text_focus_blocks_key(key));
        }
    }

    #[test]
    fn text_editing_keys_enter_insert_mode_before_reaching_an_entry() {
        let ctrl = gtk::gdk::ModifierType::CONTROL_MASK;

        assert!(normal_text_focus_starts_insert(gtk::gdk::Key::v, ctrl));
        assert!(normal_text_focus_starts_insert(
            gtk::gdk::Key::BackSpace,
            gtk::gdk::ModifierType::empty(),
        ));
        assert!(!normal_text_focus_starts_insert(gtk::gdk::Key::k, ctrl));
        assert!(!normal_text_focus_starts_insert(
            gtk::gdk::Key::j,
            gtk::gdk::ModifierType::empty(),
        ));
    }

    #[test]
    fn tag_sequence_requires_an_explicit_editor_choice() {
        assert!(is_tag_sequence_prefix(
            gtk::gdk::Key::T,
            gtk::gdk::ModifierType::empty()
        ));
        assert!(is_tag_sequence_prefix(
            gtk::gdk::Key::t,
            gtk::gdk::ModifierType::SHIFT_MASK
        ));
        assert!(!is_tag_sequence_prefix(
            gtk::gdk::Key::t,
            gtk::gdk::ModifierType::empty()
        ));
        for key in [
            gtk::gdk::Key::j,
            gtk::gdk::Key::k,
            gtk::gdk::Key::Tab,
            gtk::gdk::Key::Return,
            gtk::gdk::Key::space,
        ] {
            assert!(is_tag_menu_navigation_key(key));
        }
        assert!(!is_tag_menu_navigation_key(gtk::gdk::Key::x));
        assert_eq!(
            tag_sequence_key_action(gtk::gdk::Key::t),
            Some(TagSequenceKeyAction::SingleTag)
        );
        assert_eq!(
            tag_sequence_key_action(gtk::gdk::Key::m),
            Some(TagSequenceKeyAction::TagCommand)
        );
        for key in [
            gtk::gdk::Key::i,
            gtk::gdk::Key::a,
            gtk::gdk::Key::r,
            gtk::gdk::Key::s,
            gtk::gdk::Key::j,
            gtk::gdk::Key::k,
        ] {
            assert_eq!(tag_sequence_key_action(key), None);
        }
    }

    #[test]
    fn multi_thread_tag_query_round_trips_thread_ids() {
        let ids = BTreeSet::from(["thread-a".to_string(), "thread-b".to_string()]);
        let query = tag_query_for_thread_ids(&ids);

        assert_eq!(query, "thread:thread-a or thread:thread-b");
        assert_eq!(thread_ids_from_tag_query(&query), ids);
    }

    #[test]
    fn background_activity_rules_keep_edits_safe_and_serialize_writers() {
        let operations = [
            UserOperation::ComposeEdit,
            UserOperation::Tag,
            UserOperation::DraftSave,
            UserOperation::DraftDelete,
            UserOperation::DraftLoad,
            UserOperation::DraftClear,
            UserOperation::ComposeReplace,
            UserOperation::Send,
            UserOperation::Sync,
        ];
        for operation in operations {
            assert_eq!(
                background_activity_block_reason(false, false, operation),
                None,
                "idle operation was blocked: {operation:?}"
            );
        }
        for operation in [
            UserOperation::Tag,
            UserOperation::DraftSave,
            UserOperation::DraftDelete,
            UserOperation::Send,
            UserOperation::Sync,
        ] {
            assert!(
                background_activity_block_reason(true, false, operation).is_some(),
                "sync did not block {operation:?}"
            );
        }
        for operation in [
            UserOperation::ComposeEdit,
            UserOperation::DraftLoad,
            UserOperation::DraftClear,
            UserOperation::ComposeReplace,
        ] {
            assert_eq!(
                background_activity_block_reason(true, false, operation),
                None,
                "sync unnecessarily blocked {operation:?}"
            );
        }
        for operation in operations
            .into_iter()
            .filter(|operation| *operation != UserOperation::ComposeEdit)
        {
            assert!(
                background_activity_block_reason(false, true, operation)
                    .is_some_and(|message| message.contains("send is")),
                "send did not block {operation:?}"
            );
        }
        assert_eq!(
            background_activity_block_reason(false, true, UserOperation::ComposeEdit),
            None
        );
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

        options.external_receive_enabled = false;
        assert_eq!(sync_command_specs(&options, SyncRunKind::Manual).len(), 1);
        let startup = sync_command_specs(&options, SyncRunKind::Startup);
        assert_eq!(startup.len(), 1);
        assert_eq!(startup[0].name, "database_update");

        options.external_receive_enabled = true;
        options.external_receive_command = "   ".to_string();
        assert_eq!(sync_command_specs(&options, SyncRunKind::Manual).len(), 1);
        let startup = sync_command_specs(&options, SyncRunKind::Startup);
        assert_eq!(startup.len(), 1);
        assert_eq!(startup[0].name, "database_update");

        options.external_receive_command = "lieer-sync".to_string();
        options.notmuch_database_update_enabled = false;
        assert_eq!(sync_command_specs(&options, SyncRunKind::Manual).len(), 1);
        let startup = sync_command_specs(&options, SyncRunKind::Startup);
        assert_eq!(startup.len(), 1);
        assert_eq!(startup[0].name, "receive");

        options.notmuch_database_update_enabled = true;
        options.notmuch_database_update_command = "\t".to_string();
        assert_eq!(sync_command_specs(&options, SyncRunKind::Manual).len(), 1);
        let startup = sync_command_specs(&options, SyncRunKind::Startup);
        assert_eq!(startup.len(), 1);
        assert_eq!(startup[0].name, "receive");
    }

    #[test]
    fn hidden_message_pane_opens_threads_in_standalone_windows() {
        assert_eq!(
            thread_open_destination(true),
            ThreadOpenDestination::InlinePane
        );
        assert_eq!(
            thread_open_destination(false),
            ThreadOpenDestination::StandaloneWindow
        );
    }

    #[test]
    fn hidden_message_pane_suppresses_pane_only_binding_hints() {
        assert_eq!(visible_binding(true, "r"), "r");
        assert_eq!(visible_binding(false, "r"), "");
    }

    #[test]
    fn sync_worker_returns_before_a_slow_command_finishes() {
        let started = Instant::now();
        let receiver = spawn_sync_commands(
            "Test sync",
            vec![SyncCommandSpec {
                name: "receive",
                command: "sleep 1; printf done".to_string(),
            }],
            SyncExecutionContext {
                database_path: None,
                config_path: None,
                profile: None,
                timeout: Duration::from_secs(3),
            },
        );

        assert!(
            started.elapsed() < Duration::from_millis(300),
            "starting a background sync waited for its command"
        );
        let response = receiver
            .recv_timeout(Duration::from_secs(3))
            .expect("background sync response");
        let reports = response.result.expect("slow sync command should succeed");
        assert_eq!(reports.len(), 1);
        assert!(reports[0].contains("stdout=done"));
    }

    #[test]
    fn send_worker_returns_before_a_slow_transport_finishes() {
        let options = LaunchOptions {
            send_enabled: true,
            send_command: Some(PathBuf::from("/bin/sh")),
            send_args: vec!["-c".to_string(), "sleep 1; cat >/dev/null".to_string()],
            send_mode: notm_mail::transport::TransportMode::StdinRfc5322,
            save_sent: false,
            ..LaunchOptions::default()
        };
        let fields = ComposeFields {
            from: "Sender <sender@example.test>".to_string(),
            to: "recipient@example.test".to_string(),
            subject: "Slow worker".to_string(),
            body: "Slow worker body".to_string(),
            ..ComposeFields::default()
        };
        let started = Instant::now();
        let receiver = spawn_send(options, fields).expect("spawn send worker");

        assert!(
            started.elapsed() < Duration::from_millis(300),
            "starting a background send waited for its transport"
        );
        let response = receiver
            .recv_timeout(Duration::from_secs(3))
            .expect("background send response");
        let success = response.result.expect("slow send should succeed");
        assert!(success.report.accepted);
    }

    #[test]
    fn sync_command_failure_stops_later_commands() {
        let directory = tempfile::tempdir().expect("temporary sync directory");
        let marker = directory.path().join("later-command-ran");
        let commands = vec![
            SyncCommandSpec {
                name: "receive",
                command: "exit 7".to_string(),
            },
            SyncCommandSpec {
                name: "database_update",
                command: format!("printf ran > {}", marker.display()),
            },
        ];

        let error = execute_sync_commands(
            "Test sync",
            commands,
            &SyncExecutionContext {
                database_path: None,
                config_path: None,
                profile: None,
                timeout: Duration::from_secs(3),
            },
        )
        .expect_err("failing receive command should fail the sync");
        assert!(error.to_string().contains("failed with status=7"));
        assert!(!marker.exists(), "a command after the failure was executed");
    }

    #[test]
    fn sync_commands_receive_notmuch_context_and_run_in_order() {
        let directory = tempfile::tempdir().expect("temporary sync directory");
        let marker = directory.path().join("order");
        let first = format!("printf 1 > {:?}", marker);
        let second = format!(
            "test \"$(cat {:?})\" = 1 && printf 2 >> {:?}; \
             printf '%s|%s|%s' \"$NOTMUCH_CONFIG\" \"$NOTMUCH_DATABASE\" \"$NOTMUCH_PROFILE\"",
            marker, marker
        );
        let context = SyncExecutionContext {
            database_path: Some(PathBuf::from("/tmp/notm-test-database")),
            config_path: Some(PathBuf::from("/tmp/notm-test-config")),
            profile: Some("work".to_string()),
            timeout: Duration::from_secs(3),
        };

        let reports = execute_sync_commands(
            "Test sync",
            vec![
                SyncCommandSpec {
                    name: "receive",
                    command: first,
                },
                SyncCommandSpec {
                    name: "database_update",
                    command: second,
                },
            ],
            &context,
        )
        .expect("ordered sync commands");

        assert_eq!(std::fs::read_to_string(marker).expect("order marker"), "12");
        assert!(reports[1].contains("stdout=/tmp/notm-test-config|/tmp/notm-test-database|work"));
    }

    #[test]
    fn sync_timeout_and_diagnostics_are_bounded_and_actionable() {
        let timeout = execute_sync_commands(
            "Test sync",
            vec![SyncCommandSpec {
                name: "receive",
                command: "sleep 2".to_string(),
            }],
            &SyncExecutionContext {
                database_path: None,
                config_path: None,
                profile: None,
                timeout: Duration::from_millis(100),
            },
        )
        .expect_err("slow sync should time out");
        assert!(timeout.to_string().contains("receive command timed out"));

        let failure = execute_sync_commands(
            "Test sync",
            vec![SyncCommandSpec {
                name: "receive",
                command: "head -c 10000 /dev/zero | tr '\\0' x >&2; exit 9".to_string(),
            }],
            &SyncExecutionContext {
                database_path: None,
                config_path: None,
                profile: None,
                timeout: Duration::from_secs(3),
            },
        )
        .expect_err("failing sync should include diagnostics");
        let failure = failure.to_string();
        assert!(failure.contains("failed with status=9"));
        assert!(failure.contains("stderr="));
        assert!(failure.contains("[truncated]"));
        assert!(failure.len() < SYNC_UI_OUTPUT_LIMIT + 256);
    }
}
