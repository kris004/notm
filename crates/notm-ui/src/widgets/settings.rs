//! Settings dialog, runtime settings, validation, and raw TOML persistence.
//!
//! The existing behavior-preserving GTK settings UI still uses the GTK 4.10
//! deprecated dialog, combo-box, and chooser APIs. Keep that pre-existing
//! compatibility allowance scoped to this extracted module.

#![allow(deprecated)]

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use gtk::prelude::*;
use gtk4 as gtk;
use notm_mail::TransportMode;
use serde::Serialize;
use uuid::Uuid;

use crate::model::{LayoutPreference, MAX_THREAD_PREVIEW_LINES, ThemePreference};

#[derive(Debug, Clone)]
pub struct RuntimeSettings {
    pub(crate) page_size: usize,
    pub(crate) theme: ThemePreference,
    pub(crate) thread_preview_lines: usize,
    pub(crate) excluded_tags: Vec<String>,
    pub(crate) sync_maildir_flags_after_tag_change: bool,
    pub(crate) remote_images: bool,
    pub(crate) layout_preference: LayoutPreference,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            page_size: 100,
            theme: ThemePreference::System,
            thread_preview_lines: 2,
            excluded_tags: vec!["trash".to_string(), "spam".to_string()],
            sync_maildir_flags_after_tag_change: true,
            remote_images: false,
            layout_preference: LayoutPreference::Auto,
        }
    }
}

pub type RuntimeSettingsStore = Arc<Mutex<RuntimeSettings>>;

pub fn snapshot(store: &RuntimeSettingsStore) -> RuntimeSettings {
    store.lock().expect("runtime settings lock").clone()
}

pub fn update(store: &RuntimeSettingsStore, settings: RuntimeSettings) {
    *store.lock().expect("runtime settings lock") = settings;
}

pub fn page_size(store: &RuntimeSettingsStore) -> usize {
    snapshot(store).page_size.max(1)
}

pub fn theme(store: &RuntimeSettingsStore) -> ThemePreference {
    snapshot(store).theme
}

pub fn thread_preview_lines(store: &RuntimeSettingsStore) -> usize {
    snapshot(store).thread_preview_lines
}

pub fn excluded_tags(store: &RuntimeSettingsStore) -> Vec<String> {
    snapshot(store).excluded_tags
}

pub fn sync_maildir_flags_after_tag_change(store: &RuntimeSettingsStore) -> bool {
    snapshot(store).sync_maildir_flags_after_tag_change
}

pub fn remote_images(store: &RuntimeSettingsStore) -> bool {
    snapshot(store).remote_images
}

pub fn layout_preference(store: &RuntimeSettingsStore) -> LayoutPreference {
    snapshot(store).layout_preference
}

pub fn parse_layout_preference(value: &str) -> LayoutPreference {
    try_parse_layout_preference(value).unwrap_or(LayoutPreference::Auto)
}

pub fn try_parse_layout_preference(value: &str) -> Option<LayoutPreference> {
    match value.trim().replace('-', "_").to_lowercase().as_str() {
        "" | "auto" => Some(LayoutPreference::Auto),
        "three"
        | "three_pane"
        | "threepane"
        | "3pane"
        | "3_pane"
        | "column"
        | "columns"
        | "side_by_side"
        | "sidebyside"
        | "side_by_side_columns" => Some(LayoutPreference::ThreePane),
        "stacked" | "stack" | "top" | "top_stack" | "list_above_message" | "sidebar_list_top" => {
            Some(LayoutPreference::Stacked)
        }
        _ => None,
    }
}

pub fn layout_preference_name(layout: LayoutPreference) -> &'static str {
    match layout {
        LayoutPreference::Auto => "auto",
        LayoutPreference::ThreePane => "three_pane",
        LayoutPreference::Stacked => "stacked",
    }
}

#[derive(Debug, Clone)]
pub struct SettingsDialogSeed {
    pub parent: gtk::ApplicationWindow,
    pub app_config_path: Option<PathBuf>,
    pub database_path: Option<PathBuf>,
    pub notmuch_config_path: Option<PathBuf>,
    pub notmuch_profile: Option<String>,
    pub default_query: String,
    pub runtime: RuntimeSettings,
    pub identity_name: Option<String>,
    pub primary_email: Option<String>,
    pub other_email: Vec<String>,
    pub requested_theme: ThemePreference,
    pub thread_preview_lines: usize,
    pub show_thread_numbers: bool,
    pub show_thread_dates: bool,
    pub show_thread_tags: bool,
    pub show_thread_preview: bool,
    pub show_keybind_hints: bool,
    pub show_sidebar: bool,
    pub show_message_list: bool,
    pub show_message_view: bool,
    pub prefer_html_view: bool,
    pub start_maximized: bool,
    pub show_debug_panel: bool,
    pub trusted_image_senders: Vec<String>,
    pub hidden_tag_searches: Vec<String>,
    pub send_enabled: bool,
    pub send_command: Option<PathBuf>,
    pub send_args: Vec<String>,
    pub send_mode: TransportMode,
    pub send_working_dir: Option<PathBuf>,
    pub send_env: BTreeMap<String, String>,
    pub send_timeout_seconds: u64,
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
    pub automation_enabled: bool,
    pub automation_socket: Option<PathBuf>,
    pub automation_token: Option<String>,
    pub screenshot_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SettingsApplication {
    pub runtime: RuntimeSettings,
    pub show_thread_numbers: bool,
    pub show_thread_dates: bool,
    pub show_thread_tags: bool,
    pub show_thread_preview: bool,
    pub show_keybind_hints: bool,
    pub show_sidebar: bool,
    pub show_message_list: bool,
    pub show_message_view: bool,
    pub prefer_html_view: bool,
    pub show_debug_panel: bool,
    pub trusted_image_senders: Vec<String>,
    pub hidden_tag_searches: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsApplicationOutcome {
    pub search_reload_scheduled: bool,
}

pub type SettingsApplyHandler =
    Rc<dyn Fn(SettingsApplication) -> anyhow::Result<SettingsApplicationOutcome>>;
pub type SettingsStatusHandler = Rc<dyn Fn(String)>;

#[derive(Debug, Clone, Serialize)]
pub struct SettingsDialogTestState {
    pub id: u64,
    pub visible: bool,
    pub theme: String,
    pub thread_preview_lines: String,
    pub show_thread_preview: bool,
}

struct PendingSettingsDialog {
    id: u64,
    dialog: gtk::glib::WeakRef<gtk::Dialog>,
    theme: gtk::ComboBoxText,
    thread_preview_lines: gtk::Entry,
    show_thread_preview: gtk::CheckButton,
}

struct SettingsControllerInner {
    pending: RefCell<Option<PendingSettingsDialog>>,
    next_id: Cell<u64>,
}

#[derive(Clone)]
pub struct SettingsController {
    inner: Rc<SettingsControllerInner>,
}

impl Default for SettingsController {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsController {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(SettingsControllerInner {
                pending: RefCell::new(None),
                next_id: Cell::new(1),
            }),
        }
    }
}
impl SettingsController {
    pub fn show(
        &self,
        seed: SettingsDialogSeed,
        apply: SettingsApplyHandler,
        status: SettingsStatusHandler,
    ) {
        if let Some(dialog) = self
            .inner
            .pending
            .borrow()
            .as_ref()
            .and_then(|pending| pending.dialog.upgrade())
        {
            dialog.present();
            return;
        }
        self.inner.pending.borrow_mut().take();

        let dialog = gtk::Dialog::builder()
            .title("notm settings")
            .transient_for(&seed.parent)
            .modal(true)
            .default_width(820)
            .default_height(720)
            .build();
        dialog.set_widget_name("notm-settings-dialog");
        let area = dialog.content_area();
        area.set_spacing(8);

        let search = gtk::SearchEntry::new();
        search.set_widget_name("notm-settings-search-entry");
        search.set_placeholder_text(Some("Search settings"));
        area.append(&search);

        let existing = read_settings_toml(seed.app_config_path.as_deref());
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
            &seed
                .app_config_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "not configured".to_string()),
            "This path is selected before the UI starts. Launch with --config or set the normal app config path to use another file.",
        );

        settings_section(&form, "Notmuch");
        let database_path = settings_path_row(
            &seed.parent,
            &form,
            "Database path",
            &option_path_text(&seed.database_path),
            "Notmuch database/mail root. Blank means use libnotmuch/notmuch config resolution.",
            SettingsPathKind::Directory,
        );
        let notmuch_config_path = settings_path_row(
            &seed.parent,
            &form,
            "Notmuch config path",
            &option_path_text(&seed.notmuch_config_path),
            "Path to the notmuch config file. Blank means libnotmuch default.",
            SettingsPathKind::File,
        );
        let notmuch_profile = settings_entry_row(
            &form,
            "Profile",
            seed.notmuch_profile.as_deref().unwrap_or_default(),
            "Optional notmuch profile name. Blank means default profile.",
        );
        let default_query = settings_entry_row(
            &form,
            "Default query",
            &seed.default_query,
            "Search run at startup.",
        );
        let excluded_tags = settings_entry_row(
            &form,
            "Excluded tags",
            &join_string_list(&seed.runtime.excluded_tags),
            "Tags excluded from searches, comma separated.",
        );
        settings_readonly_row(
            &form,
            "Keep searches read-only",
            "Always on (required)",
            "Searches and message viewing always open the database read-only. Notm switches to read/write only for actions that change tags or index saved sent/draft files.",
        );
        let sync_maildir_flags_after_tag_change = settings_check_row(
            &form,
            "Sync Maildir flags",
            seed.runtime.sync_maildir_flags_after_tag_change,
            "After changing tags like unread or flagged, also update Maildir filename flags so other mail tools see the same read/star state.",
        );

        settings_section(&form, "Identity");
        let identity_name = settings_entry_row(
            &form,
            "Name",
            seed.identity_name.as_deref().unwrap_or_default(),
            "Display name used when composing mail.",
        );
        let primary_email = settings_entry_row(
            &form,
            "Primary email",
            seed.primary_email.as_deref().unwrap_or_default(),
            "Primary sender identity.",
        );
        let other_email = settings_entry_row(
            &form,
            "Other emails",
            &join_string_list(&seed.other_email),
            "Alternate own addresses, comma separated; used for reply-all de-duplication.",
        );

        settings_section(&form, "UI");
        let theme = settings_combo_row(
            &form,
            "Theme preference",
            &[
                ("system", "System/default"),
                ("light", "Light"),
                ("dark", "Dark"),
            ],
            seed.requested_theme.as_str(),
            "Apply changes this window immediately. System follows the desktop preference; Light and Dark force an application appearance.",
        );
        theme.set_widget_name("notm-settings-theme");
        let page_size = settings_entry_row(
            &form,
            "Page size",
            &seed.runtime.page_size.max(1).to_string(),
            "Positive number of threads loaded per search page.",
        );
        page_size.set_input_purpose(gtk::InputPurpose::Digits);
        let layout = settings_combo_row(
            &form,
            "Layout",
            &[
                ("auto", "Auto based on window width"),
                ("three_pane", "Side-by-side columns"),
                ("stacked", "Sidebar/list above message"),
            ],
            layout_preference_name(seed.runtime.layout_preference),
            "Default window layout. Auto uses columns on wide windows and stacked layout on narrower windows.",
        );
        let thread_preview_lines = settings_entry_row(
            &form,
            "Thread preview lines",
            &seed.thread_preview_lines.to_string(),
            "Visual line limit for wrapped thread previews (1 through 20). Apply changes rendered rows immediately.",
        );
        thread_preview_lines.set_widget_name("notm-settings-thread-preview-lines");
        thread_preview_lines.set_input_purpose(gtk::InputPurpose::Digits);
        let show_thread_numbers = settings_check_row(
            &form,
            "Show thread numbers",
            seed.show_thread_numbers,
            "Show message numbers in the thread list. Runtime command: :nu or :nonu.",
        );
        let show_thread_dates = settings_check_row(
            &form,
            "Show thread dates",
            seed.show_thread_dates,
            "Show newest-message dates in the thread list. Runtime command: :date or :nodate.",
        );
        let show_thread_tags = settings_check_row(
            &form,
            "Show thread tags",
            seed.show_thread_tags,
            "Show tags in the thread list metadata line. Runtime command: :tags or :notags.",
        );
        let show_thread_preview = settings_check_row(
            &form,
            "Show body preview",
            seed.show_thread_preview,
            "Show message body previews in the thread list. Runtime command: :preview or :nopreview.",
        );
        show_thread_preview.set_widget_name("notm-settings-show-thread-preview");
        let show_keybind_hints = settings_check_row(
            &form,
            "Show keybind hints",
            seed.show_keybind_hints,
            "Show shortcut hints in button labels, e.g. Archive (a).",
        );
        let show_sidebar = settings_check_row(
            &form,
            "Show sidebar at startup",
            seed.show_sidebar,
            "Show the saved-search sidebar when notm starts. Runtime toggle: Ctrl+1.",
        );
        let show_message_list = settings_check_row(
            &form,
            "Show message list at startup",
            seed.show_message_list,
            "Show the thread/message list when notm starts. Runtime toggle: Ctrl+2.",
        );
        let show_message_view = settings_check_row(
            &form,
            "Show message view at startup",
            seed.show_message_view,
            "Show the message reading pane when notm starts. Runtime toggle: Ctrl+3.",
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
            if seed.prefer_html_view {
                "visual_html_preferred"
            } else {
                "sanitize_then_render_text_fallback"
            },
            "Visual HTML is sanitized. Message scripts stay blocked; http/https/mailto links open externally.",
        );
        let start_maximized = settings_check_row(
            &form,
            "Start maximized",
            seed.start_maximized,
            "Open the main window maximized on launch.",
        );
        let show_debug_panel = settings_check_row(
            &form,
            "Show debug panel",
            seed.show_debug_panel,
            "Show the debug text panel by default.",
        );
        let remote_images = settings_check_row(
            &form,
            "Load remote images",
            seed.runtime.remote_images,
            "If off, HTML mail starts with remote images blocked unless the sender is trusted.",
        );
        let trusted_image_senders = settings_entry_row(
            &form,
            "Trusted image senders",
            &join_string_list(&seed.trusted_image_senders),
            "Senders whose remote images may load by default, comma separated.",
        );
        let hidden_tag_searches = settings_entry_row(
            &form,
            "Hidden tag searches",
            &join_string_list(&seed.hidden_tag_searches),
            "Tag search buttons hidden from the sidebar, comma separated.",
        );

        settings_section(&form, "Send");
        let send_enabled = settings_check_row(
            &form,
            "Sending enabled",
            seed.send_enabled,
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
            &seed.parent,
            &form,
            "Command",
            &option_path_text(&seed.send_command),
            "External send helper path, for example msmtp or a gmi wrapper.",
            SettingsPathKind::File,
        );
        let send_args = settings_entry_row(
            &form,
            "Arguments",
            &join_string_list(&seed.send_args),
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
            &transport_mode_name(&seed.send_mode),
            "auto/stdin_rfc5322 pipe the RFC5322 message to stdin; file_arg appends a temporary message path; command_template replaces {file} inside args.",
        );
        let send_working_dir = settings_path_row(
            &seed.parent,
            &form,
            "Working directory",
            &option_path_text(&seed.send_working_dir),
            "Optional working directory for the external send command.",
            SettingsPathKind::Directory,
        );
        let send_env = settings_entry_row(
            &form,
            "Environment",
            &format_env_map(&seed.send_env),
            "Extra environment for the send command as KEY=value pairs, comma or newline separated.",
        );
        let send_timeout_seconds = settings_entry_row(
            &form,
            "Timeout seconds",
            &seed.send_timeout_seconds.to_string(),
            "External send command timeout.",
        );
        let save_sent = settings_check_row(
            &form,
            "Save sent locally",
            seed.save_sent,
            "Save sent messages into a configured local Maildir after send.",
        );
        let sent_maildir = settings_path_row(
            &seed.parent,
            &form,
            "Sent Maildir",
            &option_path_text(&seed.sent_maildir),
            "Optional Maildir used when Save sent locally is enabled.",
            SettingsPathKind::Directory,
        );
        let sent_tags = settings_entry_row(
            &form,
            "Sent tags",
            &join_string_list(&seed.sent_tags),
            "Tags applied to locally indexed sent messages, comma separated.",
        );
        let index_sent_after_send = settings_check_row(
            &form,
            "Index sent after send",
            seed.index_sent_after_send,
            "Index saved sent messages in notmuch after sending.",
        );
        settings_section(&form, "Drafts");
        let save_drafts_to_maildir = settings_check_row(
            &form,
            "Save drafts to Maildir",
            seed.save_drafts_to_maildir,
            "Explicit Save draft writes a local Maildir message tagged as draft.",
        );
        let draft_maildir = settings_path_row(
            &seed.parent,
            &form,
            "Draft Maildir",
            &option_path_text(&seed.draft_maildir),
            "Optional local Maildir for saved drafts.",
            SettingsPathKind::Directory,
        );
        let draft_tags = settings_entry_row(
            &form,
            "Draft tags",
            &join_string_list(&seed.draft_tags),
            "Tags applied to saved draft messages, comma separated.",
        );
        let index_draft_after_save = settings_check_row(
            &form,
            "Index draft after save",
            seed.index_draft_after_save,
            "Index saved drafts in notmuch so tag:draft can find them.",
        );

        settings_section(&form, "Sync");
        settings_note(
            &form,
            "Sync runs only commands you define: receive first, then database update. After the selected commands succeed, the current search is refreshed. Enable sync is the master gate for the sidebar action and startup sync. Startup also requires the command's enable toggle, a nonblank command, and its startup toggle. Fixture mode never runs configured external sync commands. Leave sync disabled if another service already handles mail sync.",
        );
        let sync_enabled = settings_check_row(
            &form,
            "Enable sync",
            seed.sync_enabled,
            "Master gate for manual and startup sync. Shows the sidebar Sync action; no sync command runs when this is off.",
        );
        let manual_sync_label = settings_entry_row(
            &form,
            "Sync action label",
            &seed.manual_sync_label,
            "Text shown on the sidebar Sync action.",
        );
        let external_receive_enabled = settings_check_row(
            &form,
            "Enable receive command",
            seed.external_receive_enabled,
            "Make the nonblank receive command eligible for manual Sync and, when its startup toggle is also on, startup sync. It runs before database update.",
        );
        let external_receive_on_startup = settings_check_row(
            &form,
            "Run receive on startup",
            seed.external_receive_on_startup,
            "On a non-fixture launch, run receive at startup only when Enable sync and Enable receive command are on and Receive command is nonblank.",
        );
        let external_receive_command = settings_entry_row(
            &form,
            "Receive command",
            &seed.external_receive_command,
            "Shell command to fetch or sync mail, for example a lieer/offlineimap/mbsync wrapper.",
        );
        let notmuch_database_update_enabled = settings_check_row(
            &form,
            "Enable database update command",
            seed.notmuch_database_update_enabled,
            "Make the nonblank database update command eligible for manual Sync and, when its startup toggle is also on, startup sync. It runs after receive.",
        );
        let notmuch_database_update_on_startup = settings_check_row(
            &form,
            "Run database update on startup",
            seed.notmuch_database_update_on_startup,
            "On a non-fixture launch, run database update at startup only when Enable sync and Enable database update command are on and Database update command is nonblank.",
        );
        let notmuch_database_update_command = settings_entry_row(
            &form,
            "Database update command",
            &seed.notmuch_database_update_command,
            "Shell command to update the local notmuch database, for example `notmuch new` or a wrapper.",
        );

        settings_section(&form, "Developer test harness");
        settings_note(
            &form,
            "The developer test harness lets coding agents and tests drive the actual notm UI without clicking around or using separate GUI tools. It is not mail automation, filtering, or a Notmuch CLI replacement. It is local, token-gated, and disabled by default.",
        );
        let automation_enabled = settings_check_row(
            &form,
            "Enable test harness",
            seed.automation_enabled,
            "Start the local test-harness socket on launch.",
        );
        let automation_socket = settings_entry_row(
            &form,
            "Socket path",
            &option_path_text(&seed.automation_socket),
            "Optional Unix socket path. Blank uses a per-process path under XDG_RUNTIME_DIR, falling back to the system temporary directory.",
        );
        let automation_token = settings_entry_row(
            &form,
            "Token",
            seed.automation_token.as_deref().unwrap_or_default(),
            "Token required by test-harness clients.",
        );
        let screenshot_dir = settings_path_row(
            &seed.parent,
            &form,
            "Screenshots",
            &seed.screenshot_dir.display().to_string(),
            "Directory used by test-harness screenshots.",
            SettingsPathKind::Directory,
        );
        let allow_live_send_test = settings_check_row(
            &form,
            "Allow live send tests",
            toml_bool(&existing, "automation", "allow_live_send_test", false),
            "Permit test-harness sends against a live account and the separate live-self-send validation command. Normal interactive sending is unaffected.",
        );
        let allow_live_tag_test = settings_check_row(
            &form,
            "Allow tag test",
            toml_bool(&existing, "automation", "allow_live_tag_test", false),
            "Safety gate for explicit test-harness checks that intentionally mutate tags in the real mail database.",
        );

        settings_note(
            &form,
            "Apply updates this window without writing the config file. Save writes the app config file and also applies the runtime changes. Theme, thread-preview limit, layout, display, pane, HTML image, page-size, excluded-tag, and tag-sync changes apply immediately. Data-source, identity, send, sync-command, startup, and test-harness changes require relaunch even after saving.",
        );

        let filter_sections = Rc::new(RefCell::new(collect_settings_sections(&form)));
        let sections_for_search = filter_sections.clone();
        search.connect_search_changed(move |entry| {
            apply_settings_search_filter(&sections_for_search, &entry.text());
        });

        dialog.add_button("Apply", gtk::ResponseType::Apply);
        dialog.add_button("Save", gtk::ResponseType::Accept);
        dialog.add_button("Close", gtk::ResponseType::Close);
        if let Some(button) = dialog.widget_for_response(gtk::ResponseType::Apply) {
            button.set_widget_name("notm-settings-apply");
        }
        if let Some(button) = dialog.widget_for_response(gtk::ResponseType::Accept) {
            button.set_widget_name("notm-settings-save");
        }
        let dialog_id = self.inner.next_id.get();
        self.inner.next_id.set(dialog_id.saturating_add(1));
        *self.inner.pending.borrow_mut() = Some(PendingSettingsDialog {
            id: dialog_id,
            dialog: dialog.downgrade(),
            theme: theme.clone(),
            thread_preview_lines: thread_preview_lines.clone(),
            show_thread_preview: show_thread_preview.clone(),
        });
        let app_config_path = seed.app_config_path.clone();
        let pending_for_response = Rc::downgrade(&self.inner);
        let apply_for_response = apply.clone();
        let status_for_response = status.clone();
        dialog.connect_response(move |d, response| {
            if matches!(
                response,
                gtk::ResponseType::Apply | gtk::ResponseType::Accept
            ) {
                let page_size_value = match parse_settings_page_size(&page_size.text()) {
                    Ok(value) => value,
                    Err(err) => {
                        (status_for_response)(format!("Settings validation failed: {err}"));
                        page_size.grab_focus();
                        return;
                    }
                };
                let theme_value = match parse_settings_theme(&combo_active_id(&theme)) {
                    Ok(value) => value,
                    Err(err) => {
                        (status_for_response)(format!("Settings validation failed: {err}"));
                        theme.grab_focus();
                        return;
                    }
                };
                let thread_preview_lines_value =
                    match parse_settings_thread_preview_lines(&thread_preview_lines.text()) {
                        Ok(value) => value,
                        Err(err) => {
                            (status_for_response)(format!("Settings validation failed: {err}"));
                            thread_preview_lines.grab_focus();
                            return;
                        }
                    };
                let values = SettingsValues {
                    database_path: database_path.text().to_string(),
                    notmuch_config_path: notmuch_config_path.text().to_string(),
                    notmuch_profile: notmuch_profile.text().to_string(),
                    default_query: default_query.text().to_string(),
                    excluded_tags: excluded_tags.text().to_string(),
                    sync_maildir_flags_after_tag_change: sync_maildir_flags_after_tag_change
                        .is_active(),
                    identity_name: identity_name.text().to_string(),
                    primary_email: primary_email.text().to_string(),
                    other_email: other_email.text().to_string(),
                    theme: theme_value,
                    page_size: page_size_value,
                    thread_preview_lines: thread_preview_lines_value,
                    show_thread_numbers: show_thread_numbers.is_active(),
                    show_thread_dates: show_thread_dates.is_active(),
                    show_thread_tags: show_thread_tags.is_active(),
                    show_thread_preview: show_thread_preview.is_active(),
                    show_keybind_hints: show_keybind_hints.is_active(),
                    layout: combo_active_id(&layout),
                    show_sidebar: show_sidebar.is_active(),
                    show_message_list: show_message_list.is_active(),
                    show_message_view: show_message_view.is_active(),
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
                    notmuch_database_update_on_startup: notmuch_database_update_on_startup
                        .is_active(),
                    notmuch_database_update_command: notmuch_database_update_command
                        .text()
                        .to_string(),
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

                if response == gtk::ResponseType::Apply {
                    match apply_settings_values(&values)
                        .and_then(|application| (apply_for_response)(application))
                    {
                        Ok(outcome) => (status_for_response)(settings_status_text(
                            "Settings applied where possible",
                            outcome.search_reload_scheduled,
                        )),
                        Err(err) => {
                            (status_for_response)(format!("Settings validation failed: {err}"));
                        }
                    }
                    return;
                }

                match persist_settings_values(app_config_path.as_deref(), &values) {
                    Ok(()) => match apply_settings_values(&values)
                        .and_then(|application| (apply_for_response)(application))
                    {
                        Ok(outcome) => {
                            (status_for_response)(settings_status_text(
                                "Settings saved and applied where possible",
                                outcome.search_reload_scheduled,
                            ));
                            d.close();
                            if let Some(inner) = pending_for_response.upgrade() {
                                clear_pending_settings_dialog(&inner.pending, dialog_id);
                            }
                        }
                        Err(err) => (status_for_response)(format!(
                            "Settings were saved but could not be applied: {err}"
                        )),
                    },
                    Err(err) => (status_for_response)(format!("Settings save failed: {err}")),
                }
                return;
            }
            d.close();
            if let Some(inner) = pending_for_response.upgrade() {
                clear_pending_settings_dialog(&inner.pending, dialog_id);
            }
        });
        let pending_for_destroy = Rc::downgrade(&self.inner);
        dialog.connect_destroy(move |_| {
            if let Some(inner) = pending_for_destroy.upgrade() {
                clear_pending_settings_dialog(&inner.pending, dialog_id);
            }
        });
        dialog.present();
        search.grab_focus();
    }

    pub fn test_dialog_state(&self) -> Option<SettingsDialogTestState> {
        self.inner
            .pending
            .borrow()
            .as_ref()
            .map(|pending| SettingsDialogTestState {
                id: pending.id,
                visible: pending
                    .dialog
                    .upgrade()
                    .is_some_and(|dialog| dialog.is_visible()),
                theme: combo_active_id(&pending.theme),
                thread_preview_lines: pending.thread_preview_lines.text().to_string(),
                show_thread_preview: pending.show_thread_preview.is_active(),
            })
    }

    pub fn respond_test(&self, args: &serde_json::Value) -> anyhow::Result<u64> {
        let (dialog_id, dialog, theme_combo, preview_entry, preview_check) = {
            let pending = self.inner.pending.borrow();
            let pending = pending
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no Settings dialog is pending"))?;
            let requested_id = args
                .get("id")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(pending.id);
            anyhow::ensure!(
                requested_id == pending.id,
                "Settings dialog id {requested_id} is not pending"
            );
            let dialog = pending
                .dialog
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("pending Settings dialog is no longer available"))?;
            (
                pending.id,
                dialog,
                pending.theme.clone(),
                pending.thread_preview_lines.clone(),
                pending.show_thread_preview.clone(),
            )
        };

        if let Some(requested_theme) = args.get("theme").and_then(serde_json::Value::as_str)
            && !theme_combo.set_active_id(Some(requested_theme))
        {
            theme_combo.append(Some(requested_theme), requested_theme);
            theme_combo.set_active_id(Some(requested_theme));
        }
        if let Some(lines) = args.get("thread_preview_lines") {
            let text = match lines {
                serde_json::Value::String(value) => value.clone(),
                serde_json::Value::Number(value) => value.to_string(),
                _ => anyhow::bail!("thread_preview_lines must be a string or whole number"),
            };
            preview_entry.set_text(&text);
        }
        if let Some(visible) = args
            .get("show_thread_preview")
            .and_then(serde_json::Value::as_bool)
        {
            preview_check.set_active(visible);
        }

        let response_name = args
            .get("response")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("apply");
        let response = match response_name {
            "apply" => gtk::ResponseType::Apply,
            "save" => gtk::ResponseType::Accept,
            "close" | "cancel" => gtk::ResponseType::Close,
            _ => anyhow::bail!("response must be apply, save, or close"),
        };
        dialog.response(response);
        Ok(dialog_id)
    }
}

fn clear_pending_settings_dialog(pending: &RefCell<Option<PendingSettingsDialog>>, dialog_id: u64) {
    let is_current = pending
        .borrow()
        .as_ref()
        .is_some_and(|dialog| dialog.id == dialog_id);
    if is_current {
        pending.borrow_mut().take();
    }
}

struct SettingsValues {
    database_path: String,
    notmuch_config_path: String,
    notmuch_profile: String,
    default_query: String,
    excluded_tags: String,
    sync_maildir_flags_after_tag_change: bool,
    identity_name: String,
    primary_email: String,
    other_email: String,
    theme: ThemePreference,
    page_size: usize,
    thread_preview_lines: usize,
    show_thread_numbers: bool,
    show_thread_dates: bool,
    show_thread_tags: bool,
    show_thread_preview: bool,
    show_keybind_hints: bool,
    layout: String,
    show_sidebar: bool,
    show_message_list: bool,
    show_message_view: bool,
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

fn apply_settings_values(values: &SettingsValues) -> anyhow::Result<SettingsApplication> {
    validate_page_size(values.page_size)?;
    validate_thread_preview_lines(values.thread_preview_lines)?;
    Ok(SettingsApplication {
        runtime: RuntimeSettings {
            page_size: values.page_size,
            theme: values.theme,
            thread_preview_lines: values.thread_preview_lines,
            excluded_tags: parse_string_list(&values.excluded_tags),
            sync_maildir_flags_after_tag_change: values.sync_maildir_flags_after_tag_change,
            remote_images: values.remote_images,
            layout_preference: parse_layout_preference(&values.layout),
        },
        show_thread_numbers: values.show_thread_numbers,
        show_thread_dates: values.show_thread_dates,
        show_thread_tags: values.show_thread_tags,
        show_thread_preview: values.show_thread_preview,
        show_keybind_hints: values.show_keybind_hints,
        show_sidebar: values.show_sidebar,
        show_message_list: values.show_message_list,
        show_message_view: values.show_message_view,
        prefer_html_view: values.html_mode == "visual_html_preferred",
        show_debug_panel: values.show_debug_panel,
        trusted_image_senders: parse_string_list(&values.trusted_image_senders),
        hidden_tag_searches: parse_string_list(&values.hidden_tag_searches)
            .into_iter()
            .collect(),
    })
}

#[derive(Clone)]
struct SettingsSectionFilter {
    headers: Vec<gtk::Widget>,
    haystack: String,
    rows: Vec<(gtk::Widget, String)>,
}

fn collect_settings_sections(form: &gtk::Box) -> Vec<SettingsSectionFilter> {
    let mut sections = Vec::new();
    let mut current = None::<SettingsSectionFilter>;
    let mut pending_headers = Vec::<gtk::Widget>::new();
    let mut child = form.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.clone().downcast::<gtk::Separator>().is_ok() {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            pending_headers.push(widget);
            continue;
        }
        if let Ok(label) = widget.clone().downcast::<gtk::Label>()
            && label.has_css_class("notm-settings-section")
        {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            let mut headers = std::mem::take(&mut pending_headers);
            let title = label.text().to_string();
            headers.push(label.upcast::<gtk::Widget>());
            current = Some(SettingsSectionFilter {
                headers,
                haystack: title.to_lowercase(),
                rows: Vec::new(),
            });
            continue;
        }
        if let Some(section) = current.as_mut() {
            let haystack = settings_widget_haystack(&widget);
            section.rows.push((widget, haystack));
        }
    }
    if let Some(section) = current {
        sections.push(section);
    }
    sections
}

fn apply_settings_search_filter(sections: &Rc<RefCell<Vec<SettingsSectionFilter>>>, query: &str) {
    let query = query.trim().to_lowercase();
    for section in sections.borrow().iter() {
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
}

fn settings_widget_haystack(widget: &gtk::Widget) -> String {
    let mut parts = Vec::new();
    collect_settings_widget_text(widget, &mut parts);
    parts.join(" ").to_lowercase()
}

fn collect_settings_widget_text(widget: &gtk::Widget, parts: &mut Vec<String>) {
    if let Some(tooltip) = widget.tooltip_text()
        && !tooltip.trim().is_empty()
    {
        parts.push(tooltip.to_string());
    }
    if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
        let text = label.text();
        if !text.trim().is_empty() {
            parts.push(text.to_string());
        }
    } else if let Ok(entry) = widget.clone().downcast::<gtk::Entry>() {
        let text = entry.text();
        if !text.trim().is_empty() {
            parts.push(text.to_string());
        }
        if let Some(placeholder) = entry.placeholder_text()
            && !placeholder.trim().is_empty()
        {
            parts.push(placeholder.to_string());
        }
    } else if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
        if let Some(label) = button.label()
            && !label.trim().is_empty()
        {
            parts.push(label.to_string());
        }
    } else if let Ok(combo) = widget.clone().downcast::<gtk::ComboBoxText>()
        && let Some(text) = combo.active_text()
        && !text.trim().is_empty()
    {
        parts.push(text.to_string());
    }
    let mut child = widget.first_child();
    while let Some(child_widget) = child {
        child = child_widget.next_sibling();
        collect_settings_widget_text(&child_widget, parts);
    }
}

fn settings_status_text(base: &str, search_reload_scheduled: bool) -> String {
    if search_reload_scheduled {
        format!("{base}; current search reload scheduled")
    } else {
        base.to_string()
    }
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

fn read_settings_toml(path: Option<&Path>) -> toml::Value {
    path.and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| text.parse::<toml::Value>().ok())
        .unwrap_or_else(|| toml::Value::Table(Default::default()))
}

fn read_settings_toml_for_update(path: &Path) -> anyhow::Result<toml::Value> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(toml::Value::Table(Default::default()));
        }
        Err(err) => {
            return Err(anyhow::anyhow!(
                "reading existing app config {}: {err}",
                path.display()
            ));
        }
    };
    let value = text
        .parse::<toml::Value>()
        .map_err(|err| anyhow::anyhow!("parsing existing app config {}: {err}", path.display()))?;
    ensure_settings_root_table(path, value)
}

fn ensure_settings_root_table(path: &Path, value: toml::Value) -> anyhow::Result<toml::Value> {
    anyhow::ensure!(
        value.is_table(),
        "existing app config {} must contain a TOML table",
        path.display()
    );
    Ok(value)
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

fn option_path_text(value: &Option<PathBuf>) -> String {
    value
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}

fn join_string_list(values: &[String]) -> String {
    values.join(", ")
}

fn parse_settings_page_size(value: &str) -> anyhow::Result<usize> {
    let page_size = value
        .trim()
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("page size must be a positive whole number"))?;
    validate_page_size(page_size)
}

fn parse_settings_theme(value: &str) -> anyhow::Result<ThemePreference> {
    value.parse::<ThemePreference>().map_err(|_| {
        anyhow::anyhow!("theme must be exactly one of system, light, or dark; got {value:?}")
    })
}

fn parse_settings_thread_preview_lines(value: &str) -> anyhow::Result<usize> {
    let lines = value
        .trim()
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("thread preview lines must be a whole number from 1 to 20"))?;
    validate_thread_preview_lines(lines)
}

pub fn validate_thread_preview_lines(lines: usize) -> anyhow::Result<usize> {
    anyhow::ensure!(
        (1..=MAX_THREAD_PREVIEW_LINES).contains(&lines),
        "thread preview lines must be between 1 and {MAX_THREAD_PREVIEW_LINES}"
    );
    anyhow::ensure!(
        i64::try_from(lines).is_ok(),
        "thread preview line count is too large to store in configuration"
    );
    Ok(lines)
}

pub fn validate_page_size(page_size: usize) -> anyhow::Result<usize> {
    anyhow::ensure!(page_size > 0, "page size must be greater than zero");
    anyhow::ensure!(
        i64::try_from(page_size).is_ok(),
        "page size is too large to store in configuration"
    );
    Ok(page_size)
}

fn validate_send_settings(mode: &str, args: &[String]) -> anyhow::Result<()> {
    anyhow::ensure!(
        mode != "command_template" || args.iter().any(|arg| arg.contains("{file}")),
        "send arguments must include {{file}} when send mode is command_template"
    );
    Ok(())
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

fn persist_settings_values(path: Option<&Path>, values: &SettingsValues) -> anyhow::Result<()> {
    let page_size = validate_page_size(values.page_size)?;
    let thread_preview_lines = validate_thread_preview_lines(values.thread_preview_lines)?;
    let send_args = parse_string_list(&values.send_args);
    validate_send_settings(&values.send_mode, &send_args)?;
    let Some(path) = path else {
        anyhow::bail!("app config path is not configured");
    };
    let mut value = read_settings_toml_for_update(path)?;
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
    persist_read_only_notmuch_invariant(root);
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

    set_string(root, "ui", "theme", values.theme.as_str());
    set_int(root, "ui", "page_size", page_size as i64);
    set_int(
        root,
        "ui",
        "thread_preview_lines",
        thread_preview_lines as i64,
    );
    set_bool(
        root,
        "ui",
        "show_thread_numbers",
        values.show_thread_numbers,
    );
    set_bool(root, "ui", "show_thread_dates", values.show_thread_dates);
    set_bool(root, "ui", "show_thread_tags", values.show_thread_tags);
    set_bool(
        root,
        "ui",
        "show_thread_preview",
        values.show_thread_preview,
    );
    set_bool(root, "ui", "show_keybind_hints", values.show_keybind_hints);
    set_string(
        root,
        "ui",
        "layout",
        layout_preference_name(parse_layout_preference(&values.layout)),
    );
    set_bool(root, "ui", "show_sidebar", values.show_sidebar);
    set_bool(root, "ui", "show_message_list", values.show_message_list);
    set_bool(root, "ui", "show_message_view", values.show_message_view);
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
    set_string_array(root, "send", "args", send_args);
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

    persist_private_settings_toml(path, &value)
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

fn persist_read_only_notmuch_invariant(root: &mut toml::map::Map<String, toml::Value>) {
    set_bool(root, "notmuch", "open_readwrite_only_for_mutations", true);
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

pub fn persist_basic_settings(
    path: Option<&Path>,
    default_query: &str,
    page_size: usize,
    send_command: &str,
) -> anyhow::Result<()> {
    let page_size = validate_page_size(page_size)?;
    let Some(path) = path else {
        return Ok(());
    };
    let mut value = read_settings_toml_for_update(path)?;
    let root = value.as_table_mut().expect("value is table");
    table_entry(root, "notmuch").insert(
        "default_query".to_string(),
        toml::Value::String(default_query.to_string()),
    );
    persist_read_only_notmuch_invariant(root);
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
    persist_private_settings_toml(path, &value)
}

/// Persist one live UI-domain value while retaining unrelated valid TOML keys.
pub fn persist_ui_value(
    path: Option<&Path>,
    key: &str,
    setting: toml::Value,
) -> anyhow::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let mut value = read_settings_toml_for_update(path)?;
    table_entry(value.as_table_mut().expect("value is table"), "ui")
        .insert(key.to_string(), setting);
    persist_private_settings_toml(path, &value)
}

fn persist_private_settings_toml(path: &Path, value: &toml::Value) -> anyhow::Result<()> {
    let contents = toml::to_string_pretty(value)?;
    atomic_write_private(path, contents.as_bytes())
}

fn atomic_write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let configured_parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let parent = configured_parent.unwrap_or_else(|| Path::new("."));
    if let Some(parent) = configured_parent {
        ensure_private_directory(parent)?;
    }
    let filename = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("notm-config");
    let temporary_path = parent.join(format!(".{filename}.{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> anyhow::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut temporary = options.open(&temporary_path)?;
        #[cfg(unix)]
        temporary.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        temporary.write_all(bytes)?;
        temporary.sync_all()?;
        drop(temporary);
        std::fs::rename(&temporary_path, path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    write_result
        .map_err(|err| anyhow::anyhow!("writing app config {} atomically: {err}", path.display()))
}

fn ensure_private_directory(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)?;
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
    fn settings_status_distinguishes_scheduled_search_reload() {
        assert_eq!(
            settings_status_text("Settings applied where possible", false),
            "Settings applied where possible"
        );
        assert_eq!(
            settings_status_text("Settings saved and applied where possible", true),
            "Settings saved and applied where possible; current search reload scheduled"
        );
    }

    #[test]
    fn page_size_requires_a_positive_whole_number() {
        assert_eq!(parse_settings_page_size(" 25 ").unwrap(), 25);
        assert!(
            parse_settings_page_size("0")
                .unwrap_err()
                .to_string()
                .contains("greater than zero")
        );
        assert!(
            parse_settings_page_size("many")
                .unwrap_err()
                .to_string()
                .contains("positive whole number")
        );
        #[cfg(target_pointer_width = "64")]
        assert!(
            validate_page_size((i64::MAX as usize) + 1)
                .unwrap_err()
                .to_string()
                .contains("too large")
        );
    }

    #[test]
    fn theme_and_preview_lines_require_exact_supported_values() {
        assert_eq!(
            parse_settings_theme("system").unwrap(),
            ThemePreference::System
        );
        assert_eq!(
            parse_settings_theme("light").unwrap(),
            ThemePreference::Light
        );
        assert_eq!(parse_settings_theme("dark").unwrap(), ThemePreference::Dark);
        for invalid in ["", "System", "auto", "dark "] {
            assert!(
                parse_settings_theme(invalid).is_err(),
                "unexpectedly accepted {invalid:?}"
            );
        }

        assert_eq!(parse_settings_thread_preview_lines(" 1 ").unwrap(), 1);
        assert_eq!(
            parse_settings_thread_preview_lines(&MAX_THREAD_PREVIEW_LINES.to_string()).unwrap(),
            MAX_THREAD_PREVIEW_LINES
        );
        for invalid in ["", "zero", "0", "21"] {
            assert!(
                parse_settings_thread_preview_lines(invalid).is_err(),
                "unexpectedly accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn command_template_requires_a_file_argument() {
        assert!(
            validate_send_settings("command_template", &["--message".to_string()])
                .unwrap_err()
                .to_string()
                .contains("{file}")
        );
        validate_send_settings("command_template", &["--message={file}".to_string()])
            .expect("command template should accept a file placeholder");
        validate_send_settings("auto", &[]).expect("other send modes do not need a placeholder");
    }

    #[test]
    fn persistence_forces_the_read_only_notmuch_invariant() {
        let mut root = toml::map::Map::new();
        set_bool(
            &mut root,
            "notmuch",
            "open_readwrite_only_for_mutations",
            false,
        );

        persist_read_only_notmuch_invariant(&mut root);

        assert_eq!(
            root["notmuch"]["open_readwrite_only_for_mutations"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn live_ui_persistence_retains_unrelated_toml_keys() {
        let directory = tempfile::tempdir().expect("temporary settings directory");
        let config_directory = directory.path().join("notm");
        std::fs::create_dir(&config_directory).expect("create app config directory");
        let path = config_directory.join("config.toml");
        std::fs::write(
            &path,
            "[unrelated]\nkeep = \"yes\"\n\n[ui]\nshow_sidebar = true\n",
        )
        .expect("seed settings");
        #[cfg(unix)]
        {
            std::fs::set_permissions(&config_directory, std::fs::Permissions::from_mode(0o755))
                .expect("make config directory non-private");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                .expect("make config file non-private");
        }

        persist_ui_value(
            Some(&path),
            "hidden_tag_searches",
            toml::Value::Array(vec![toml::Value::String("sent".to_string())]),
        )
        .expect("persist live UI value");

        let value = std::fs::read_to_string(&path)
            .expect("read settings")
            .parse::<toml::Value>()
            .expect("parse settings");
        assert_eq!(value["unrelated"]["keep"].as_str(), Some("yes"));
        assert_eq!(value["ui"]["show_sidebar"].as_bool(), Some(true));
        assert_eq!(value["ui"]["hidden_tag_searches"][0].as_str(), Some("sent"));
        #[cfg(unix)]
        {
            assert_eq!(
                std::fs::metadata(&config_directory)
                    .expect("config directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o755,
                "saving an explicitly located config must not change its existing parent directory"
            );
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("config file metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let entries = std::fs::read_dir(&config_directory)
            .expect("list config directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read config directory entries");
        assert_eq!(
            entries.len(),
            1,
            "atomic save must remove its temporary file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn settings_persistence_creates_private_parent_and_file() {
        let directory = tempfile::tempdir().expect("temporary settings directory");
        let config_directory = directory.path().join("notm");
        let path = config_directory.join("config.toml");

        persist_ui_value(Some(&path), "show_sidebar", toml::Value::Boolean(true))
            .expect("persist new app config");

        assert_eq!(
            std::fs::metadata(&config_directory)
                .expect("config directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path)
                .expect("config file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn settings_updates_reject_invalid_existing_content_without_overwriting_it() {
        let directory = tempfile::tempdir().expect("temporary settings directory");
        let path = directory.path().join("config.toml");
        let malformed = b"[ui\nshow_sidebar = true\n";
        std::fs::write(&path, malformed).expect("seed malformed settings");

        let error = persist_ui_value(Some(&path), "show_sidebar", toml::Value::Boolean(false))
            .expect_err("malformed settings must be rejected");

        assert!(error.to_string().contains("parsing existing app config"));
        assert_eq!(
            std::fs::read(&path).expect("read rejected settings"),
            malformed
        );
        let entries = std::fs::read_dir(directory.path())
            .expect("list settings directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read settings directory entries");
        assert_eq!(
            entries.len(),
            1,
            "a rejected save must not leave a temp file"
        );

        let error = ensure_settings_root_table(
            &path,
            toml::Value::String("not a document table".to_string()),
        )
        .expect_err("non-table settings must be rejected");
        assert!(error.to_string().contains("must contain a TOML table"));
    }

    #[test]
    fn layout_preference_parses_config_values() {
        assert_eq!(
            try_parse_layout_preference(""),
            Some(LayoutPreference::Auto)
        );
        assert_eq!(parse_layout_preference("auto"), LayoutPreference::Auto);
        assert_eq!(
            parse_layout_preference("three-pane"),
            LayoutPreference::ThreePane
        );
        assert_eq!(
            parse_layout_preference("columns"),
            LayoutPreference::ThreePane
        );
        assert_eq!(
            parse_layout_preference("side-by-side"),
            LayoutPreference::ThreePane
        );
        assert_eq!(
            parse_layout_preference("stacked"),
            LayoutPreference::Stacked
        );
        assert_eq!(parse_layout_preference("unknown"), LayoutPreference::Auto);
        assert_eq!(try_parse_layout_preference("unknown"), None);
    }
}
