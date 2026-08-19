use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use chrono::Utc;
use gtk::prelude::*;
use gtk4 as gtk;
use notm_mail::{ReplyKind, address::parse_address_list};
use notm_notmuch::MessageSummary;
use serde::Serialize;
use webkit6::prelude::WebViewExt;

use super::link_hints::{LinkHintController, LinkHintOpener, LinkHintSnapshot};

const MESSAGE_HEADER_VALUE_LINES: i32 = 1;
const STATUS_BAR_MAX_WIDTH_CHARS: i32 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StandaloneImagePolicy {
    Config,
    Once,
    TrustSender,
}

#[derive(Debug, Clone)]
pub(crate) struct StandalonePolicySnapshot {
    pub(crate) prefer_html_view: bool,
    pub(crate) collapse_quotes: bool,
    pub(crate) remote_images: bool,
    pub(crate) trusted_image_senders: Vec<String>,
    pub(crate) show_keybind_hints: bool,
    pub(crate) normal_input_mode: bool,
    pub(crate) response_sensitive: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct StandaloneHtmlRender {
    pub(crate) document: String,
    pub(crate) allow_remote_images: bool,
    pub(crate) status: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum StandaloneHtmlScroll {
    Lines(f64),
    Pages(f64),
    Edge(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StandaloneResponseAction {
    Reply(ReplyKind),
    Forward,
    ForwardAttachment,
}

pub(crate) struct StandaloneResponseRequest {
    pub(crate) action: StandaloneResponseAction,
    pub(crate) message: MessageSummary,
    pub(crate) source_status: gtk::Label,
}

pub(crate) type StandalonePolicyProvider = Rc<dyn Fn() -> StandalonePolicySnapshot>;
pub(crate) type StandaloneMessageHasHtml = Rc<dyn Fn(&MessageSummary) -> bool>;
pub(crate) type StandaloneTextRenderer =
    Rc<dyn Fn(&MessageSummary, bool) -> anyhow::Result<String>>;
pub(crate) type StandaloneHtmlRenderer =
    Rc<dyn Fn(&MessageSummary, StandaloneImagePolicy) -> anyhow::Result<StandaloneHtmlRender>>;
pub(crate) type StandaloneHtmlViewInitializer = Rc<dyn Fn(&webkit6::WebView, &gtk::Label, bool)>;
pub(crate) type StandaloneHtmlScrollHandler =
    Rc<dyn Fn(&webkit6::WebView, &gtk::Label, StandaloneHtmlScroll)>;
pub(crate) type StandaloneResponseHandler = Rc<dyn Fn(StandaloneResponseRequest) -> bool>;

pub(crate) struct StandaloneOpenOptions {
    pub(crate) parent: gtk::ApplicationWindow,
    pub(crate) messages: Vec<MessageSummary>,
    pub(crate) selected_index: usize,
    pub(crate) policy: StandalonePolicyProvider,
    pub(crate) message_has_html: StandaloneMessageHasHtml,
    pub(crate) render_text: StandaloneTextRenderer,
    pub(crate) render_html: StandaloneHtmlRenderer,
    pub(crate) initialize_html_view: StandaloneHtmlViewInitializer,
    pub(crate) scroll_html: StandaloneHtmlScrollHandler,
    pub(crate) open_link: LinkHintOpener,
    pub(crate) respond: StandaloneResponseHandler,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StandaloneWindowSnapshot {
    pub(crate) id: u64,
    pub(crate) selected_index: usize,
    pub(crate) message_count: usize,
    pub(crate) selected_message: Option<MessageSummary>,
    pub(crate) message_ids: Vec<String>,
    pub(crate) view: &'static str,
    pub(crate) collapse_quotes: bool,
    pub(crate) image_policy: String,
    pub(crate) title: Option<String>,
    pub(crate) status: String,
    pub(crate) link_hints: LinkHintSnapshot,
}

#[derive(Clone)]
pub(crate) struct StandaloneMessageController {
    windows: Rc<RefCell<Vec<Rc<StandaloneMessageWindow>>>>,
    next_id: Rc<Cell<u64>>,
}

impl Default for StandaloneMessageController {
    fn default() -> Self {
        Self::new()
    }
}

impl StandaloneMessageController {
    pub(crate) fn new() -> Self {
        Self {
            windows: Rc::new(RefCell::new(Vec::new())),
            next_id: Rc::new(Cell::new(1)),
        }
    }

    pub(crate) fn open(&self, options: StandaloneOpenOptions) -> anyhow::Result<()> {
        anyhow::ensure!(!options.messages.is_empty(), "thread has no messages");
        let app = options
            .parent
            .application()
            .ok_or_else(|| anyhow::anyhow!("main window is not attached to an application"))?;
        let selected_index = options.selected_index.min(options.messages.len() - 1);
        let message = options.messages[selected_index].clone();
        let id = self.next_id.get();
        self.next_id.set(id.checked_add(1).unwrap_or(1));

        let window = gtk::ApplicationWindow::builder()
            .application(&app)
            .title(standalone_message_window_title(&message))
            .default_width(900)
            .default_height(760)
            .build();
        window.set_widget_name("notm-message-window");

        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_start(10);
        root.set_margin_end(10);
        root.set_margin_top(10);
        root.set_margin_bottom(10);

        let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        action_row.set_widget_name("notm-message-window-actions");
        action_row.set_halign(gtk::Align::Start);

        let (response_menu_button, response_menu_box) = menu_button_with_box(
            "Respond",
            "notm-message-window-response-menu-button",
            &options.policy,
        );
        let reply_button = gtk::Button::with_label("Reply");
        reply_button.set_widget_name("notm-message-window-reply-button");
        let reply_all_button = gtk::Button::with_label("Reply all");
        reply_all_button.set_widget_name("notm-message-window-reply-all-button");
        let forward_button = gtk::Button::with_label("Forward");
        forward_button.set_widget_name("notm-message-window-forward-button");
        let forward_attachment_button = gtk::Button::with_label("Forward attached");
        forward_attachment_button.set_widget_name("notm-message-window-forward-attachment-button");
        for button in [
            &reply_button,
            &reply_all_button,
            &forward_button,
            &forward_attachment_button,
        ] {
            response_menu_box.append(button);
        }

        let (message_menu_button, message_menu_box) = menu_button_with_box(
            "Message",
            "notm-message-window-message-menu-button",
            &options.policy,
        );
        message_menu_button.set_tooltip_text(Some(
            "Choose a message in this thread. Use J/K for next/previous message.",
        ));
        let (view_menu_button, view_menu_box) = menu_button_with_box(
            "View",
            "notm-message-window-view-menu-button",
            &options.policy,
        );
        let view_text_button = gtk::Button::with_label("Text");
        view_text_button.set_widget_name("notm-message-window-view-text-button");
        let view_html_button = gtk::Button::with_label("Visual HTML");
        view_html_button.set_widget_name("notm-message-window-view-html-button");
        let view_headers_button = gtk::Button::with_label("Full headers");
        view_headers_button.set_widget_name("notm-message-window-view-headers-button");
        let view_raw_button = gtk::Button::with_label("Raw source");
        view_raw_button.set_widget_name("notm-message-window-view-raw-button");
        for button in [
            &view_text_button,
            &view_html_button,
            &view_headers_button,
            &view_raw_button,
        ] {
            view_menu_box.append(button);
        }
        let collapse_quotes_button = gtk::Button::with_label("Collapse quotes");
        collapse_quotes_button.set_widget_name("notm-message-window-collapse-quotes-button");

        let (copy_menu_button, copy_menu_box) = menu_button_with_box(
            "Copy",
            "notm-message-window-copy-menu-button",
            &options.policy,
        );
        let copy_message_id_button = gtk::Button::with_label("Copy message id");
        copy_message_id_button.set_widget_name("notm-message-window-copy-message-id-button");
        let copy_thread_id_button = gtk::Button::with_label("Copy thread id");
        copy_thread_id_button.set_widget_name("notm-message-window-copy-thread-id-button");
        let copy_from_email_button = gtk::Button::with_label("Copy from email");
        copy_from_email_button.set_widget_name("notm-message-window-copy-from-email-button");
        let copy_to_email_button = gtk::Button::with_label("Copy to email");
        copy_to_email_button.set_widget_name("notm-message-window-copy-to-email-button");
        let copy_cc_email_button = gtk::Button::with_label("Copy cc email");
        copy_cc_email_button.set_widget_name("notm-message-window-copy-cc-email-button");
        let copy_subject_button = gtk::Button::with_label("Copy subject");
        copy_subject_button.set_widget_name("notm-message-window-copy-subject-button");
        for button in [
            &copy_message_id_button,
            &copy_thread_id_button,
            &copy_from_email_button,
            &copy_to_email_button,
            &copy_cc_email_button,
            &copy_subject_button,
        ] {
            copy_menu_box.append(button);
        }

        action_row.append(&response_menu_button);
        action_row.append(&message_menu_button);
        action_row.append(&view_menu_button);
        action_row.append(&collapse_quotes_button);
        action_row.append(&copy_menu_button);
        root.append(&action_row);

        let image_policy_button = gtk::Button::with_label("Load images once");
        image_policy_button.set_widget_name("notm-message-window-image-policy-button");
        let html_policy_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        html_policy_row.set_widget_name("notm-message-window-html-policy-row");
        html_policy_row.set_visible(false);
        let html_policy_label = gtk::Label::new(None);
        html_policy_label.set_xalign(0.0);
        html_policy_label.set_wrap(true);
        html_policy_label.set_hexpand(true);
        html_policy_label.add_css_class("dim-label");
        image_policy_button.set_halign(gtk::Align::End);
        html_policy_row.append(&html_policy_label);
        html_policy_row.append(&image_policy_button);
        root.append(&html_policy_row);

        let message_header_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        message_header_box.set_widget_name("notm-message-window-header");
        root.append(&message_header_box);

        let text_view = gtk::TextView::new();
        text_view.set_widget_name("notm-message-window-text");
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        text_view.set_wrap_mode(gtk::WrapMode::WordChar);
        let text_scrolled = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&text_view)
            .build();

        let status_label = gtk::Label::new(Some("Ready"));
        configure_status_label(&status_label);
        let html_view = webkit6::WebView::new();
        html_view.set_widget_name("notm-message-window-html");
        html_view.set_hexpand(true);
        html_view.set_vexpand(true);
        let policy = (options.policy)();
        (options.initialize_html_view)(&html_view, &status_label, policy.remote_images);
        let link_hints =
            LinkHintController::new(&html_view, &status_label, options.open_link.clone());
        let html_scrolled = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&html_view)
            .build();

        let message_stack = gtk::Stack::new();
        message_stack.set_widget_name("notm-message-window-stack");
        message_stack.set_hexpand(true);
        message_stack.set_vexpand(true);
        message_stack.set_hhomogeneous(false);
        message_stack.set_vhomogeneous(false);
        message_stack.add_named(&text_scrolled, Some("text"));
        message_stack.add_named(&html_scrolled, Some("html"));
        message_stack.set_visible_child_name("text");
        root.append(&message_stack);
        root.append(&status_label);
        window.set_child(Some(&root));

        let initial_view = if policy.prefer_html_view && (options.message_has_html)(&message) {
            MessageViewKind::Html
        } else {
            MessageViewKind::Text
        };
        let standalone = Rc::new(StandaloneMessageWindow {
            id,
            window: window.clone(),
            response_menu_button,
            response_menu_box,
            reply_button,
            reply_all_button,
            forward_button,
            forward_attachment_button,
            message_menu_button,
            message_menu_box,
            view_menu_button,
            view_menu_box,
            view_text_button,
            view_html_button,
            view_headers_button,
            view_raw_button,
            html_policy_row,
            html_policy_label,
            image_policy_button,
            collapse_quotes_button,
            message_header_box,
            message_stack,
            text_view,
            text_scrolled,
            html_view,
            link_hints,
            status_label,
            copy_menu_button,
            copy_menu_box,
            copy_message_id_button,
            copy_thread_id_button,
            copy_from_email_button,
            copy_to_email_button,
            copy_cc_email_button,
            copy_subject_button,
            policy: options.policy,
            message_has_html: options.message_has_html,
            render_text: options.render_text,
            render_html: options.render_html,
            scroll_html: options.scroll_html,
            respond: options.respond,
            state: RefCell::new(StandaloneMessageState {
                messages: options.messages,
                selected_index,
                view: initial_view,
                collapse_quotes: policy.collapse_quotes,
                image_policy: StandaloneImagePolicy::Config,
            }),
        });

        self.windows.borrow_mut().push(standalone.clone());
        update_response_controls(&standalone, policy.response_sensitive);
        let windows = self.windows.clone();
        window.connect_close_request(move |_| {
            windows
                .borrow_mut()
                .retain(|standalone| standalone.id != id);
            gtk::glib::Propagation::Proceed
        });
        connect_message_window_actions(&standalone);
        connect_message_window_shortcuts(&standalone);
        populate_message_menu(&standalone);
        show_message_view(&standalone, initial_view);
        window.present();
        Ok(())
    }

    pub(crate) fn set_response_sensitive(&self, sensitive: bool) {
        for standalone in self.windows.borrow().iter() {
            update_response_controls(standalone, sensitive);
        }
    }

    pub(crate) fn snapshots(&self) -> Vec<StandaloneWindowSnapshot> {
        self.windows
            .borrow()
            .iter()
            .map(|window| window.snapshot())
            .collect()
    }

    pub(crate) fn window_snapshot(&self, window_index: usize) -> Option<StandaloneWindowSnapshot> {
        self.windows
            .borrow()
            .get(window_index)
            .map(|window| window.snapshot())
    }

    pub(crate) fn select_message(
        &self,
        window_index: usize,
        message_index: usize,
    ) -> Option<(bool, StandaloneWindowSnapshot)> {
        let standalone = self.windows.borrow().get(window_index).cloned()?;
        let selected = select_message(&standalone, message_index);
        Some((selected, standalone.snapshot()))
    }

    pub(crate) fn respond(
        &self,
        window_index: usize,
        action: StandaloneResponseAction,
    ) -> Option<(bool, StandaloneWindowSnapshot)> {
        let standalone = self.windows.borrow().get(window_index).cloned()?;
        let accepted = run_response_action(&standalone, action);
        Some((accepted, standalone.snapshot()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageViewKind {
    Text,
    Html,
    Headers,
    Raw,
}

#[derive(Debug, Clone)]
struct StandaloneMessageState {
    messages: Vec<MessageSummary>,
    selected_index: usize,
    view: MessageViewKind,
    collapse_quotes: bool,
    image_policy: StandaloneImagePolicy,
}

struct StandaloneMessageWindow {
    id: u64,
    window: gtk::ApplicationWindow,
    response_menu_button: gtk::MenuButton,
    response_menu_box: gtk::Box,
    reply_button: gtk::Button,
    reply_all_button: gtk::Button,
    forward_button: gtk::Button,
    forward_attachment_button: gtk::Button,
    message_menu_button: gtk::MenuButton,
    message_menu_box: gtk::Box,
    view_menu_button: gtk::MenuButton,
    view_menu_box: gtk::Box,
    view_text_button: gtk::Button,
    view_html_button: gtk::Button,
    view_headers_button: gtk::Button,
    view_raw_button: gtk::Button,
    html_policy_row: gtk::Box,
    html_policy_label: gtk::Label,
    image_policy_button: gtk::Button,
    collapse_quotes_button: gtk::Button,
    message_header_box: gtk::Box,
    message_stack: gtk::Stack,
    text_view: gtk::TextView,
    text_scrolled: gtk::ScrolledWindow,
    html_view: webkit6::WebView,
    link_hints: LinkHintController,
    status_label: gtk::Label,
    copy_menu_button: gtk::MenuButton,
    copy_menu_box: gtk::Box,
    copy_message_id_button: gtk::Button,
    copy_thread_id_button: gtk::Button,
    copy_from_email_button: gtk::Button,
    copy_to_email_button: gtk::Button,
    copy_cc_email_button: gtk::Button,
    copy_subject_button: gtk::Button,
    policy: StandalonePolicyProvider,
    message_has_html: StandaloneMessageHasHtml,
    render_text: StandaloneTextRenderer,
    render_html: StandaloneHtmlRenderer,
    scroll_html: StandaloneHtmlScrollHandler,
    respond: StandaloneResponseHandler,
    state: RefCell<StandaloneMessageState>,
}

impl StandaloneMessageWindow {
    fn snapshot(&self) -> StandaloneWindowSnapshot {
        let state = self.state.borrow();
        StandaloneWindowSnapshot {
            id: self.id,
            selected_index: state.selected_index,
            message_count: state.messages.len(),
            selected_message: state.messages.get(state.selected_index).cloned(),
            message_ids: state
                .messages
                .iter()
                .map(|message| message.message_id.clone())
                .collect(),
            view: match state.view {
                MessageViewKind::Text => "text",
                MessageViewKind::Html => "html",
                MessageViewKind::Headers => "headers",
                MessageViewKind::Raw => "raw",
            },
            collapse_quotes: state.collapse_quotes,
            image_policy: format!("{:?}", state.image_policy).to_ascii_lowercase(),
            title: self.window.title().map(|title| title.to_string()),
            status: self.status_label.text().to_string(),
            link_hints: self.link_hints.snapshot(),
        }
    }
}

#[derive(Clone)]
struct StandaloneShortcutState {
    pending_go: Rc<Cell<bool>>,
    pending_response: Rc<Cell<bool>>,
    pending_view: Rc<Cell<bool>>,
    pending_copy: Rc<Cell<bool>>,
}

impl StandaloneShortcutState {
    fn new() -> Self {
        Self {
            pending_go: Rc::new(Cell::new(false)),
            pending_response: Rc::new(Cell::new(false)),
            pending_view: Rc::new(Cell::new(false)),
            pending_copy: Rc::new(Cell::new(false)),
        }
    }

    fn clear(&self) {
        self.pending_go.set(false);
        self.pending_response.set(false);
        self.pending_view.set(false);
        self.pending_copy.set(false);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StandaloneCopyField {
    MessageId,
    ThreadId,
    From,
    To,
    Cc,
    Subject,
}

fn connect_message_window_actions(standalone: &Rc<StandaloneMessageWindow>) {
    for (button, action) in [
        (
            &standalone.reply_button,
            StandaloneResponseAction::Reply(ReplyKind::Sender),
        ),
        (
            &standalone.reply_all_button,
            StandaloneResponseAction::Reply(ReplyKind::All),
        ),
        (
            &standalone.forward_button,
            StandaloneResponseAction::Forward,
        ),
        (
            &standalone.forward_attachment_button,
            StandaloneResponseAction::ForwardAttachment,
        ),
    ] {
        let standalone = Rc::downgrade(standalone);
        button.connect_clicked(move |_| {
            if let Some(standalone) = standalone.upgrade() {
                run_response_action(&standalone, action);
                standalone.response_menu_button.popdown();
            }
        });
    }

    for (button, view) in [
        (&standalone.view_text_button, MessageViewKind::Text),
        (&standalone.view_html_button, MessageViewKind::Html),
        (&standalone.view_headers_button, MessageViewKind::Headers),
        (&standalone.view_raw_button, MessageViewKind::Raw),
    ] {
        let standalone = Rc::downgrade(standalone);
        button.connect_clicked(move |_| {
            if let Some(standalone) = standalone.upgrade() {
                show_message_view(&standalone, view);
                standalone.view_menu_button.popdown();
            }
        });
    }

    let standalone_weak = Rc::downgrade(standalone);
    standalone.image_policy_button.connect_clicked(move |_| {
        if let Some(standalone) = standalone_weak.upgrade() {
            activate_image_policy_button(&standalone);
        }
    });

    let standalone_weak = Rc::downgrade(standalone);
    standalone.collapse_quotes_button.connect_clicked(move |_| {
        if let Some(standalone) = standalone_weak.upgrade() {
            toggle_quote_collapse(&standalone);
        }
    });

    for (button, field) in [
        (
            &standalone.copy_message_id_button,
            StandaloneCopyField::MessageId,
        ),
        (
            &standalone.copy_thread_id_button,
            StandaloneCopyField::ThreadId,
        ),
        (
            &standalone.copy_from_email_button,
            StandaloneCopyField::From,
        ),
        (&standalone.copy_to_email_button, StandaloneCopyField::To),
        (&standalone.copy_cc_email_button, StandaloneCopyField::Cc),
        (
            &standalone.copy_subject_button,
            StandaloneCopyField::Subject,
        ),
    ] {
        let standalone = Rc::downgrade(standalone);
        button.connect_clicked(move |_| {
            if let Some(standalone) = standalone.upgrade() {
                copy_message_field(&standalone, field);
                standalone.copy_menu_button.popdown();
            }
        });
    }
}

fn update_button_binding_labels(standalone: &StandaloneMessageWindow) {
    set_menu_button_label(&standalone.response_menu_button, "Respond", "r", standalone);
    set_button_label(&standalone.reply_button, "Reply", "r r", standalone);
    set_button_label(&standalone.reply_all_button, "Reply all", "r a", standalone);
    set_button_label(&standalone.forward_button, "Forward", "r f", standalone);
    set_button_label(
        &standalone.forward_attachment_button,
        "Forward attached",
        "r A",
        standalone,
    );
    let message_base =
        strip_binding_suffix(&standalone.message_menu_button.label().unwrap_or_default());
    set_menu_button_label(
        &standalone.message_menu_button,
        &message_base,
        "J/K",
        standalone,
    );
    set_menu_button_label(&standalone.view_menu_button, "View", "V", standalone);
    set_button_label(&standalone.view_text_button, "Text", "V t", standalone);
    set_button_label(
        &standalone.view_html_button,
        "Visual HTML",
        "V v",
        standalone,
    );
    set_button_label(
        &standalone.view_headers_button,
        "Full headers",
        "V h",
        standalone,
    );
    set_button_label(&standalone.view_raw_button, "Raw source", "V r", standalone);
    set_button_label(
        &standalone.collapse_quotes_button,
        "Collapse quotes",
        "q",
        standalone,
    );
    set_menu_button_label(&standalone.copy_menu_button, "Copy", "y", standalone);
    set_button_label(
        &standalone.copy_message_id_button,
        "Copy message id",
        "y m",
        standalone,
    );
    set_button_label(
        &standalone.copy_thread_id_button,
        "Copy thread id",
        "y t",
        standalone,
    );
    set_button_label(
        &standalone.copy_from_email_button,
        "Copy from email",
        "y f",
        standalone,
    );
    set_button_label(
        &standalone.copy_to_email_button,
        "Copy to email",
        "y o",
        standalone,
    );
    set_button_label(
        &standalone.copy_cc_email_button,
        "Copy cc email",
        "y c",
        standalone,
    );
    set_button_label(
        &standalone.copy_subject_button,
        "Copy subject",
        "y s",
        standalone,
    );
    let image_base =
        strip_binding_suffix(&standalone.image_policy_button.label().unwrap_or_default());
    set_button_label(
        &standalone.image_policy_button,
        &image_base,
        "I",
        standalone,
    );
}

fn connect_message_window_shortcuts(standalone: &Rc<StandaloneMessageWindow>) {
    let shortcuts = StandaloneShortcutState::new();
    connect_dropdown_sequence_keys(standalone, &shortcuts);
    standalone
        .window
        .add_controller(standalone_key_controller(standalone, &shortcuts));
}

fn standalone_key_controller(
    standalone: &Rc<StandaloneMessageWindow>,
    shortcuts: &StandaloneShortcutState,
) -> gtk::EventControllerKey {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let standalone = Rc::downgrade(standalone);
    let shortcuts = shortcuts.clone();
    controller.connect_key_pressed(move |_, key, _, mods| {
        let Some(standalone) = standalone.upgrade() else {
            return gtk::glib::Propagation::Proceed;
        };
        if standalone.link_hints.handle_key(key, mods) {
            return gtk::glib::Propagation::Stop;
        }
        let ctrl = mods.contains(gtk::gdk::ModifierType::CONTROL_MASK);
        if ctrl && (key == gtk::gdk::Key::d || key == gtk::gdk::Key::D) {
            scroll_message_pages(&standalone, 0.5);
            return gtk::glib::Propagation::Stop;
        }
        if ctrl && (key == gtk::gdk::Key::u || key == gtk::gdk::Key::U) {
            scroll_message_pages(&standalone, -0.5);
            return gtk::glib::Propagation::Stop;
        }
        if ctrl && (key == gtk::gdk::Key::f || key == gtk::gdk::Key::F) {
            scroll_message_pages(&standalone, 1.0);
            return gtk::glib::Propagation::Stop;
        }
        if ctrl && (key == gtk::gdk::Key::b || key == gtk::gdk::Key::B) {
            scroll_message_pages(&standalone, -1.0);
            return gtk::glib::Propagation::Stop;
        }
        if let Some(lines) = super::vim_viewport_scroll_lines(key, mods) {
            scroll_message_lines(&standalone, lines);
            return gtk::glib::Propagation::Stop;
        }
        if ctrl {
            return gtk::glib::Propagation::Proceed;
        }
        if key == gtk::gdk::Key::Escape {
            shortcuts.clear();
            popdown_shortcut_menus(&standalone);
            standalone.status_label.set_text("Normal mode");
            return gtk::glib::Propagation::Stop;
        }
        if shortcuts.pending_response.get() {
            shortcuts.pending_response.set(false);
            standalone.response_menu_button.popdown();
            return propagation_for_handled(run_response_key(&standalone, key));
        }
        if shortcuts.pending_view.get() {
            shortcuts.pending_view.set(false);
            standalone.view_menu_button.popdown();
            return propagation_for_handled(run_view_key(&standalone, key));
        }
        if shortcuts.pending_copy.get() {
            shortcuts.pending_copy.set(false);
            standalone.copy_menu_button.popdown();
            return propagation_for_handled(run_copy_key(&standalone, key));
        }
        if shortcuts.pending_go.get() {
            shortcuts.pending_go.set(false);
            return propagation_for_handled(if key == gtk::gdk::Key::g {
                scroll_message_to_edge(&standalone, false);
                true
            } else {
                false
            });
        }
        let handled = if let Some(delta) = message_navigation_delta(key, mods) {
            select_relative_message(&standalone, delta)
        } else if key == gtk::gdk::Key::j || key == gtk::gdk::Key::Down {
            scroll_message_lines(&standalone, 1.0);
            true
        } else if key == gtk::gdk::Key::k || key == gtk::gdk::Key::Up {
            scroll_message_lines(&standalone, -1.0);
            true
        } else if key == gtk::gdk::Key::g {
            shortcuts.pending_go.set(true);
            standalone.status_label.set_text("Go: g top");
            true
        } else if key == gtk::gdk::Key::G {
            scroll_message_to_edge(&standalone, true);
            true
        } else if key == gtk::gdk::Key::r {
            shortcuts.pending_response.set(true);
            standalone.response_menu_button.popup();
            standalone
                .status_label
                .set_text("Respond: r reply, a reply all, f forward, A forward attached");
            true
        } else if key == gtk::gdk::Key::V {
            shortcuts.pending_view.set(true);
            standalone.view_menu_button.popup();
            standalone
                .status_label
                .set_text("View: t text, v visual HTML, h headers, r raw source");
            true
        } else if key == gtk::gdk::Key::q {
            toggle_quote_collapse(&standalone);
            true
        } else if key == gtk::gdk::Key::y {
            shortcuts.pending_copy.set(true);
            standalone.copy_menu_button.popup();
            standalone
                .status_label
                .set_text("Copy: m message id, t thread id, f from, o to, c cc, s subject");
            true
        } else if key == gtk::gdk::Key::I {
            activate_image_policy_button(&standalone);
            true
        } else if key == gtk::gdk::Key::F
            || (key == gtk::gdk::Key::f && mods.contains(gtk::gdk::ModifierType::SHIFT_MASK))
        {
            start_link_hint_mode(&standalone)
        } else {
            false
        };
        propagation_for_handled(handled)
    });
    controller
}

fn connect_dropdown_sequence_keys(
    standalone: &Rc<StandaloneMessageWindow>,
    shortcuts: &StandaloneShortcutState,
) {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let standalone_weak = Rc::downgrade(standalone);
    let shortcut_state = shortcuts.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        let Some(standalone) = standalone_weak.upgrade() else {
            return gtk::glib::Propagation::Proceed;
        };
        let handled = run_response_key(&standalone, key);
        if handled {
            shortcut_state.pending_response.set(false);
            standalone.response_menu_button.popdown();
        }
        propagation_for_handled(handled)
    });
    standalone.response_menu_box.add_controller(controller);

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let standalone_weak = Rc::downgrade(standalone);
    let shortcut_state = shortcuts.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        let Some(standalone) = standalone_weak.upgrade() else {
            return gtk::glib::Propagation::Proceed;
        };
        let handled = run_view_key(&standalone, key);
        if handled {
            shortcut_state.pending_view.set(false);
            standalone.view_menu_button.popdown();
        }
        propagation_for_handled(handled)
    });
    standalone.view_menu_box.add_controller(controller);

    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let standalone_weak = Rc::downgrade(standalone);
    let shortcut_state = shortcuts.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        let Some(standalone) = standalone_weak.upgrade() else {
            return gtk::glib::Propagation::Proceed;
        };
        let handled = run_copy_key(&standalone, key);
        if handled {
            shortcut_state.pending_copy.set(false);
            standalone.copy_menu_button.popdown();
        }
        propagation_for_handled(handled)
    });
    standalone.copy_menu_box.add_controller(controller);
}

fn propagation_for_handled(handled: bool) -> gtk::glib::Propagation {
    if handled {
        gtk::glib::Propagation::Stop
    } else {
        gtk::glib::Propagation::Proceed
    }
}

fn popdown_shortcut_menus(standalone: &StandaloneMessageWindow) {
    standalone.response_menu_button.popdown();
    standalone.view_menu_button.popdown();
    standalone.copy_menu_button.popdown();
}

fn run_response_key(standalone: &StandaloneMessageWindow, key: gtk::gdk::Key) -> bool {
    let Some(action) = response_sequence_action(key) else {
        return false;
    };
    run_response_action(standalone, action)
}

fn response_sequence_action(key: gtk::gdk::Key) -> Option<StandaloneResponseAction> {
    if key == gtk::gdk::Key::r {
        Some(StandaloneResponseAction::Reply(ReplyKind::Sender))
    } else if key == gtk::gdk::Key::a {
        Some(StandaloneResponseAction::Reply(ReplyKind::All))
    } else if key == gtk::gdk::Key::f {
        Some(StandaloneResponseAction::Forward)
    } else if key == gtk::gdk::Key::A {
        Some(StandaloneResponseAction::ForwardAttachment)
    } else {
        None
    }
}

fn run_view_key(standalone: &StandaloneMessageWindow, key: gtk::gdk::Key) -> bool {
    let Some(view) = view_sequence_action(key) else {
        return false;
    };
    if view == MessageViewKind::Html
        && current_message(standalone)
            .is_none_or(|message| !(standalone.message_has_html)(&message))
    {
        standalone.status_label.set_text("No visual HTML part");
        return true;
    }
    show_message_view(standalone, view);
    true
}

fn view_sequence_action(key: gtk::gdk::Key) -> Option<MessageViewKind> {
    if key == gtk::gdk::Key::t {
        Some(MessageViewKind::Text)
    } else if key == gtk::gdk::Key::v {
        Some(MessageViewKind::Html)
    } else if key == gtk::gdk::Key::h {
        Some(MessageViewKind::Headers)
    } else if key == gtk::gdk::Key::r {
        Some(MessageViewKind::Raw)
    } else {
        None
    }
}

fn run_copy_key(standalone: &StandaloneMessageWindow, key: gtk::gdk::Key) -> bool {
    let Some(field) = copy_sequence_field(key) else {
        return false;
    };
    copy_message_field(standalone, field);
    true
}

fn copy_sequence_field(key: gtk::gdk::Key) -> Option<StandaloneCopyField> {
    if key == gtk::gdk::Key::m {
        Some(StandaloneCopyField::MessageId)
    } else if key == gtk::gdk::Key::t {
        Some(StandaloneCopyField::ThreadId)
    } else if key == gtk::gdk::Key::f {
        Some(StandaloneCopyField::From)
    } else if key == gtk::gdk::Key::o {
        Some(StandaloneCopyField::To)
    } else if key == gtk::gdk::Key::c {
        Some(StandaloneCopyField::Cc)
    } else if key == gtk::gdk::Key::s {
        Some(StandaloneCopyField::Subject)
    } else {
        None
    }
}

fn current_message(standalone: &StandaloneMessageWindow) -> Option<MessageSummary> {
    let state = standalone.state.borrow();
    state.messages.get(state.selected_index).cloned()
}

fn select_message(standalone: &Rc<StandaloneMessageWindow>, index: usize) -> bool {
    let view = {
        let mut state = standalone.state.borrow_mut();
        if index >= state.messages.len() {
            standalone.status_label.set_text("Message index not found");
            return false;
        }
        state.selected_index = index;
        state.image_policy = StandaloneImagePolicy::Config;
        state.view
    };
    show_message_view(standalone, view);
    populate_message_menu(standalone);
    standalone.message_menu_button.popdown();
    true
}

fn message_navigation_delta(key: gtk::gdk::Key, mods: gtk::gdk::ModifierType) -> Option<isize> {
    let shifted = |lowercase, uppercase| {
        key == uppercase || (key == lowercase && mods.contains(gtk::gdk::ModifierType::SHIFT_MASK))
    };
    if shifted(gtk::gdk::Key::j, gtk::gdk::Key::J) {
        Some(1)
    } else if shifted(gtk::gdk::Key::k, gtk::gdk::Key::K) {
        Some(-1)
    } else {
        None
    }
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

fn select_relative_message(standalone: &Rc<StandaloneMessageWindow>, delta: isize) -> bool {
    let (current, total) = {
        let state = standalone.state.borrow();
        (state.selected_index, state.messages.len())
    };
    let Some(target) = relative_message_index(current, total, delta) else {
        standalone.status_label.set_text("Thread has no messages");
        return false;
    };
    if target == current {
        standalone.status_label.set_text(if delta < 0 {
            "Already at the first message in this thread"
        } else {
            "Already at the last message in this thread"
        });
        return true;
    }
    select_message(standalone, target)
}

fn populate_message_menu(standalone: &Rc<StandaloneMessageWindow>) {
    clear_box(&standalone.message_menu_box);
    let (messages, selected_index) = {
        let state = standalone.state.borrow();
        (state.messages.clone(), state.selected_index)
    };
    let total = messages.len();
    standalone.message_menu_button.set_visible(total > 1);
    standalone.collapse_quotes_button.set_visible(total > 1);
    standalone.message_menu_button.set_label(&format!(
        "Message {}/{}",
        selected_index.saturating_add(1),
        total.max(1)
    ));
    for (index, message) in messages.iter().enumerate() {
        let subject = non_empty_or(message.subject.trim(), "(no subject)");
        let button = gtk::Button::with_label(&format!("{}: {}", index + 1, subject));
        if index == selected_index {
            button.add_css_class("suggested-action");
        }
        let standalone_weak = Rc::downgrade(standalone);
        button.connect_clicked(move |_| {
            if let Some(standalone) = standalone_weak.upgrade() {
                select_message(&standalone, index);
            }
        });
        standalone.message_menu_box.append(&button);
    }
}

fn show_message_view(standalone: &StandaloneMessageWindow, view: MessageViewKind) {
    let Some(message) = current_message(standalone) else {
        standalone.status_label.set_text("No selected message");
        return;
    };
    if view == MessageViewKind::Html && !(standalone.message_has_html)(&message) {
        standalone.status_label.set_text("No visual HTML part");
        return;
    }
    standalone
        .window
        .set_title(Some(&standalone_message_window_title(&message)));
    refresh_message_header(standalone, &message);
    match view {
        MessageViewKind::Text => show_text_message(standalone, &message),
        MessageViewKind::Html => show_html_message(standalone, &message),
        MessageViewKind::Headers => show_headers(standalone, &message),
        MessageViewKind::Raw => show_raw(standalone, &message),
    }
    update_message_buttons(standalone, &message);
}

fn start_link_hint_mode(standalone: &StandaloneMessageWindow) -> bool {
    let Some(message) = current_message(standalone) else {
        standalone.status_label.set_text("No message selected");
        return true;
    };
    if !(standalone.message_has_html)(&message) {
        standalone
            .status_label
            .set_text("The selected message has no Visual HTML links");
        return true;
    }
    if !html_view_is_visible(standalone) {
        show_message_view(standalone, MessageViewKind::Html);
    }
    standalone.link_hints.start();
    true
}

fn show_text_message(standalone: &StandaloneMessageWindow, message: &MessageSummary) {
    let collapse_quotes = standalone.state.borrow().collapse_quotes;
    match (standalone.render_text)(message, collapse_quotes) {
        Ok(rendered) => {
            set_active_message_view(standalone, MessageViewKind::Text);
            standalone.text_view.set_monospace(false);
            standalone.text_view.buffer().set_text(&rendered);
            standalone.status_label.set_text("Text message shown");
        }
        Err(err) => standalone
            .status_label
            .set_text(&format!("Text view failed: {err}")),
    }
}

fn show_html_message(standalone: &StandaloneMessageWindow, message: &MessageSummary) {
    let image_policy = standalone.state.borrow().image_policy;
    match (standalone.render_html)(message, image_policy) {
        Ok(rendered) => {
            set_html_image_loading(&standalone.html_view, rendered.allow_remote_images);
            standalone
                .html_view
                .load_html(&rendered.document, Some("about:blank"));
            set_active_message_view(standalone, MessageViewKind::Html);
            standalone.status_label.set_text(&rendered.status);
        }
        Err(err) => standalone
            .status_label
            .set_text(&format!("Visual HTML failed: {err}")),
    }
}

fn show_headers(standalone: &StandaloneMessageWindow, message: &MessageSummary) {
    let result = (|| -> anyhow::Result<String> {
        let filename = message_filename(message)?;
        Ok(header_block(&std::fs::read_to_string(filename)?))
    })();
    match result {
        Ok(headers) => {
            set_active_message_view(standalone, MessageViewKind::Headers);
            standalone.text_view.set_monospace(true);
            standalone.text_view.buffer().set_text(&headers);
            standalone
                .status_label
                .set_text("Full message headers shown");
        }
        Err(err) => standalone
            .status_label
            .set_text(&format!("Full headers failed: {err}")),
    }
}

fn show_raw(standalone: &StandaloneMessageWindow, message: &MessageSummary) {
    let result = (|| -> anyhow::Result<String> {
        let filename = message_filename(message)?;
        Ok(std::fs::read_to_string(filename)?)
    })();
    match result {
        Ok(raw) => {
            set_active_message_view(standalone, MessageViewKind::Raw);
            standalone.text_view.set_monospace(true);
            standalone.text_view.buffer().set_text(&raw);
            standalone.status_label.set_text("Raw message source shown");
        }
        Err(err) => standalone
            .status_label
            .set_text(&format!("Raw source failed: {err}")),
    }
}

fn set_active_message_view(standalone: &StandaloneMessageWindow, active: MessageViewKind) {
    if active != MessageViewKind::Html {
        standalone.link_hints.cancel_silent();
    }
    for button in [
        &standalone.view_text_button,
        &standalone.view_html_button,
        &standalone.view_headers_button,
        &standalone.view_raw_button,
    ] {
        button.remove_css_class("suggested-action");
    }
    match active {
        MessageViewKind::Text => {
            standalone
                .view_text_button
                .add_css_class("suggested-action");
            standalone.message_stack.set_visible_child_name("text");
        }
        MessageViewKind::Html => {
            standalone
                .view_html_button
                .add_css_class("suggested-action");
            standalone.message_stack.set_visible_child_name("html");
        }
        MessageViewKind::Headers => {
            standalone
                .view_headers_button
                .add_css_class("suggested-action");
            standalone.message_stack.set_visible_child_name("text");
        }
        MessageViewKind::Raw => {
            standalone.view_raw_button.add_css_class("suggested-action");
            standalone.message_stack.set_visible_child_name("text");
        }
    }
    standalone.state.borrow_mut().view = active;
}

fn toggle_quote_collapse(standalone: &StandaloneMessageWindow) {
    let enabled = {
        let mut state = standalone.state.borrow_mut();
        state.collapse_quotes = !state.collapse_quotes;
        state.collapse_quotes
    };
    show_message_view(standalone, MessageViewKind::Text);
    standalone.status_label.set_text(if enabled {
        "Quote collapse enabled"
    } else {
        "Quote collapse disabled"
    });
}

fn activate_image_policy_button(standalone: &StandaloneMessageWindow) {
    let Some(message) = current_message(standalone) else {
        standalone.status_label.set_text("No message selected");
        return;
    };
    if message_allows_images(&(standalone.policy)(), &message) {
        update_message_buttons(standalone, &message);
        return;
    }
    let policy = if standalone.state.borrow().view == MessageViewKind::Html
        && webkit_view_images_allowed(&standalone.html_view)
    {
        StandaloneImagePolicy::TrustSender
    } else {
        StandaloneImagePolicy::Once
    };
    standalone.state.borrow_mut().image_policy = policy;
    show_message_view(standalone, MessageViewKind::Html);
}

fn update_message_buttons(standalone: &StandaloneMessageWindow, message: &MessageSummary) {
    let has_html = (standalone.message_has_html)(message);
    let html_visible = standalone.state.borrow().view == MessageViewKind::Html;
    standalone.view_html_button.set_visible(has_html);
    standalone.view_html_button.set_sensitive(has_html);
    standalone
        .html_policy_row
        .set_visible(html_visible && has_html);
    standalone
        .image_policy_button
        .set_visible(html_visible && has_html);
    if !has_html {
        standalone.image_policy_button.set_label("Load images once");
        standalone.image_policy_button.set_sensitive(false);
        update_button_binding_labels(standalone);
        return;
    }
    let policy = (standalone.policy)();
    if html_visible {
        let image_policy = if webkit_view_images_allowed(&standalone.html_view) {
            if message_allows_images(&policy, message) {
                "remote images allowed"
            } else {
                "remote images loaded for this view"
            }
        } else {
            "remote images blocked"
        };
        standalone.html_policy_label.set_text(&format!(
            "Sanitized HTML view: message JavaScript disabled; {image_policy}; links open externally (F shows link hints)."
        ));
    }
    if message_allows_images(&policy, message) {
        let sender = message_sender_email(message);
        let sender_trusted = sender
            .as_deref()
            .is_some_and(|sender| image_sender_is_trusted(&policy, sender));
        standalone.image_policy_button.set_label(if sender_trusted {
            "Images trusted"
        } else {
            "Images allowed"
        });
        standalone.image_policy_button.set_sensitive(false);
    } else if html_visible && webkit_view_images_allowed(&standalone.html_view) {
        standalone
            .image_policy_button
            .set_label("Trust sender images");
        standalone
            .image_policy_button
            .set_sensitive(message_sender_email(message).is_some());
    } else {
        standalone.image_policy_button.set_label("Load images once");
        standalone.image_policy_button.set_sensitive(true);
    }
    update_button_binding_labels(standalone);
}

fn refresh_message_header(standalone: &StandaloneMessageWindow, message: &MessageSummary) {
    clear_box(&standalone.message_header_box);
    let (index, total) = {
        let state = standalone.state.borrow();
        (state.selected_index + 1, state.messages.len().max(1))
    };
    standalone
        .message_header_box
        .append(&standalone_message_header(message, index, total));
    standalone
        .message_header_box
        .set_tooltip_text(Some(&format!(
            "Message-ID: {}\nFiles: {}",
            message.message_id,
            message.filenames.join(", ")
        )));
}

fn standalone_message_window_title(message: &MessageSummary) -> String {
    let subject = non_empty_or(&message.subject, "(no subject)");
    format!("notm: {}", truncate_status_text(subject, 80))
}

fn standalone_message_header(message: &MessageSummary, index: usize, total: usize) -> gtk::Box {
    let header = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let count = gtk::Label::new(Some(&format!("Message {index} of {total}")));
    count.add_css_class("notm-message-header-badge");
    count.set_xalign(0.0);
    header.append(&count);

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
    header.append(&grid);
    header
}

fn copy_message_field(standalone: &StandaloneMessageWindow, field: StandaloneCopyField) {
    let Some(message) = current_message(standalone) else {
        standalone.status_label.set_text("No message to copy from");
        return;
    };
    let (text, label) = match field {
        StandaloneCopyField::MessageId => (message.message_id.clone(), "message id"),
        StandaloneCopyField::ThreadId => (message.thread_id.clone(), "thread id"),
        StandaloneCopyField::From => (header_emails(&message.from), "from email"),
        StandaloneCopyField::To => (header_emails(&message.to), "to email"),
        StandaloneCopyField::Cc => (header_emails(&message.cc), "cc email"),
        StandaloneCopyField::Subject => (message.subject.clone(), "subject"),
    };
    if text.trim().is_empty() {
        standalone
            .status_label
            .set_text(&format!("No {label} to copy"));
    } else {
        copy_to_clipboard(&text);
        standalone.status_label.set_text(&format!("Copied {label}"));
    }
}

fn run_response_action(
    standalone: &StandaloneMessageWindow,
    action: StandaloneResponseAction,
) -> bool {
    let Some(message) = current_message(standalone) else {
        standalone.status_label.set_text("No message selected");
        return false;
    };
    (standalone.respond)(StandaloneResponseRequest {
        action,
        message,
        source_status: standalone.status_label.clone(),
    })
}

fn update_response_controls(standalone: &StandaloneMessageWindow, sensitive: bool) {
    standalone.response_menu_button.set_sensitive(sensitive);
    for button in [
        &standalone.reply_button,
        &standalone.reply_all_button,
        &standalone.forward_button,
        &standalone.forward_attachment_button,
    ] {
        button.set_sensitive(sensitive);
    }
}

fn html_view_is_visible(standalone: &StandaloneMessageWindow) -> bool {
    standalone.state.borrow().view == MessageViewKind::Html
}

fn scroll_message_lines(standalone: &StandaloneMessageWindow, lines: f64) {
    if html_view_is_visible(standalone) {
        (standalone.scroll_html)(
            &standalone.html_view,
            &standalone.status_label,
            StandaloneHtmlScroll::Lines(lines),
        );
    } else {
        scroll_window_lines(&standalone.text_scrolled, lines);
    }
}

fn scroll_message_pages(standalone: &StandaloneMessageWindow, pages: f64) {
    if html_view_is_visible(standalone) {
        (standalone.scroll_html)(
            &standalone.html_view,
            &standalone.status_label,
            StandaloneHtmlScroll::Pages(pages),
        );
    } else {
        scroll_window_pages(&standalone.text_scrolled, pages);
    }
}

fn scroll_message_to_edge(standalone: &StandaloneMessageWindow, bottom: bool) {
    if html_view_is_visible(standalone) {
        (standalone.scroll_html)(
            &standalone.html_view,
            &standalone.status_label,
            StandaloneHtmlScroll::Edge(bottom),
        );
    } else {
        scroll_window_to_edge(&standalone.text_scrolled, bottom);
    }
}

fn menu_button_with_box(
    label: &str,
    widget_name: &str,
    policy: &StandalonePolicyProvider,
) -> (gtk::MenuButton, gtk::Box) {
    let button = gtk::MenuButton::new();
    button.set_label(label);
    button.set_widget_name(widget_name);
    let popover = gtk::Popover::new();
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    connect_vim_menu_navigation(&menu, policy);
    let focus_menu = menu.clone();
    popover.connect_show(move |_| {
        focus_first_menu_child(&focus_menu);
    });
    popover.set_child(Some(&menu));
    button.set_popover(Some(&popover));
    (button, menu)
}

fn connect_vim_menu_navigation(menu: &gtk::Box, policy: &StandalonePolicyProvider) {
    let controller = gtk::EventControllerKey::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let menu_for_keys = menu.clone();
    let policy = policy.clone();
    controller.connect_key_pressed(move |_, key, _, _| {
        if !(policy)().normal_input_mode {
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

fn button_label(base: &str, binding: &str, standalone: &StandaloneMessageWindow) -> String {
    let policy = (standalone.policy)();
    if policy.show_keybind_hints && policy.normal_input_mode && !binding.is_empty() {
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

fn set_button_label(
    widget: &gtk::Button,
    base: &str,
    binding: &str,
    standalone: &StandaloneMessageWindow,
) {
    widget.set_label(&button_label(base, binding, standalone));
}

fn set_menu_button_label(
    widget: &gtk::MenuButton,
    base: &str,
    binding: &str,
    standalone: &StandaloneMessageWindow,
) {
    widget.set_label(&button_label(base, binding, standalone));
}

fn message_allows_images(policy: &StandalonePolicySnapshot, message: &MessageSummary) -> bool {
    policy.remote_images
        || message_sender_email(message)
            .as_deref()
            .is_some_and(|sender| image_sender_is_trusted(policy, sender))
}

fn message_sender_email(message: &MessageSummary) -> Option<String> {
    parse_address_list(&message.from)
        .into_iter()
        .next()
        .map(|address| normalize_sender(&address.email))
}

fn normalize_sender(sender: &str) -> String {
    sender.trim().to_ascii_lowercase()
}

fn image_sender_is_trusted(policy: &StandalonePolicySnapshot, sender: &str) -> bool {
    let sender = normalize_sender(sender);
    policy
        .trusted_image_senders
        .iter()
        .any(|trusted| trusted == &sender)
}

fn set_html_image_loading(view: &webkit6::WebView, allow_remote_images: bool) {
    if let Some(settings) = WebViewExt::settings(view) {
        settings.set_auto_load_images(allow_remote_images);
    }
}

fn webkit_view_images_allowed(view: &webkit6::WebView) -> bool {
    WebViewExt::settings(view)
        .map(|settings| settings.is_auto_load_images())
        .unwrap_or(false)
}

fn configure_status_label(label: &gtk::Label) {
    label.set_hexpand(true);
    label.set_single_line_mode(true);
    label.set_width_chars(1);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(STATUS_BAR_MAX_WIDTH_CHARS);
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

fn message_filename(message: &MessageSummary) -> anyhow::Result<String> {
    message
        .filenames
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("message has no file"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_message_shortcut_sequences_match_their_menu_bindings() {
        let none = gtk::gdk::ModifierType::empty();
        let shift = gtk::gdk::ModifierType::SHIFT_MASK;
        assert_eq!(message_navigation_delta(gtk::gdk::Key::J, none), Some(1));
        assert_eq!(message_navigation_delta(gtk::gdk::Key::K, none), Some(-1));
        assert_eq!(message_navigation_delta(gtk::gdk::Key::j, shift), Some(1));
        assert_eq!(message_navigation_delta(gtk::gdk::Key::k, shift), Some(-1));
        assert_eq!(message_navigation_delta(gtk::gdk::Key::j, none), None);
        assert_eq!(message_navigation_delta(gtk::gdk::Key::k, none), None);
        assert_eq!(relative_message_index(1, 3, 1), Some(2));
        assert_eq!(relative_message_index(1, 3, -1), Some(0));
        assert_eq!(relative_message_index(2, 3, 8), Some(2));
        assert_eq!(relative_message_index(0, 3, -8), Some(0));
        assert_eq!(relative_message_index(0, 0, 1), None);
        assert_eq!(
            response_sequence_action(gtk::gdk::Key::r),
            Some(StandaloneResponseAction::Reply(ReplyKind::Sender))
        );
        assert_eq!(
            response_sequence_action(gtk::gdk::Key::a),
            Some(StandaloneResponseAction::Reply(ReplyKind::All))
        );
        assert_eq!(
            response_sequence_action(gtk::gdk::Key::f),
            Some(StandaloneResponseAction::Forward)
        );
        assert_eq!(
            response_sequence_action(gtk::gdk::Key::A),
            Some(StandaloneResponseAction::ForwardAttachment)
        );
        assert_eq!(
            view_sequence_action(gtk::gdk::Key::t),
            Some(MessageViewKind::Text)
        );
        assert_eq!(
            view_sequence_action(gtk::gdk::Key::v),
            Some(MessageViewKind::Html)
        );
        assert_eq!(
            view_sequence_action(gtk::gdk::Key::h),
            Some(MessageViewKind::Headers)
        );
        assert_eq!(
            view_sequence_action(gtk::gdk::Key::r),
            Some(MessageViewKind::Raw)
        );
        assert_eq!(
            copy_sequence_field(gtk::gdk::Key::m),
            Some(StandaloneCopyField::MessageId)
        );
        assert_eq!(
            copy_sequence_field(gtk::gdk::Key::t),
            Some(StandaloneCopyField::ThreadId)
        );
        assert_eq!(
            copy_sequence_field(gtk::gdk::Key::f),
            Some(StandaloneCopyField::From)
        );
        assert_eq!(
            copy_sequence_field(gtk::gdk::Key::o),
            Some(StandaloneCopyField::To)
        );
        assert_eq!(
            copy_sequence_field(gtk::gdk::Key::c),
            Some(StandaloneCopyField::Cc)
        );
        assert_eq!(
            copy_sequence_field(gtk::gdk::Key::s),
            Some(StandaloneCopyField::Subject)
        );
        assert_eq!(view_sequence_action(gtk::gdk::Key::j), None);
        assert_eq!(copy_sequence_field(gtk::gdk::Key::j), None);
    }
}
