use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc,
    time::Duration,
};

#[cfg(all(test, unix))]
use std::os::unix::fs::DirBuilderExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use gtk::prelude::*;
use gtk4 as gtk;
#[cfg(test)]
use notm_mail::mime::extract_attachments;
use notm_mail::{attachments::sanitize_attachment_filename, compose::AttachmentInput};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(test)]
use uuid::Uuid;

use crate::{
    attachment_io::{
        self, AttachmentIoAction, AttachmentIoCoordinator, AttachmentIoRequest,
        AttachmentIoResponse, AttachmentIoSource, AttachmentIoToken, MAX_FIXTURE_DELAY,
    },
    model::ComposeFields,
    thread_loader::PreparedAttachment,
};

const ATTACHMENT_ROWS_PER_UPDATE: usize = 24;
const ATTACHMENT_WORKER_POLL_INTERVAL: Duration = Duration::from_millis(20);
type AttachmentPayloadMap = BTreeMap<(String, usize), AttachmentIoSource>;

pub(crate) struct AttachmentOpenStore {
    directory: tempfile::TempDir,
}

impl AttachmentOpenStore {
    pub(crate) fn create() -> io::Result<Self> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("notm-open-attachments-");
        #[cfg(unix)]
        builder.permissions(std::fs::Permissions::from_mode(0o700));
        let directory = builder.tempdir()?;
        #[cfg(unix)]
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        Ok(Self { directory })
    }

    pub(crate) fn path(&self) -> &Path {
        self.directory.path()
    }

    pub(crate) fn close(self) -> io::Result<()> {
        self.directory.close()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ThreadAttachmentItem {
    pub(crate) message_index: usize,
    /// Stable depth-first attachment MIME-part index within the message.
    pub(crate) attachment_index: usize,
    pub(crate) message_id: String,
    pub(crate) filename: String,
    pub(crate) content_type: String,
    pub(crate) size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachmentOrigin {
    Thread,
}

#[derive(Debug, Clone)]
pub(crate) struct AttachmentPayload {
    message_id: String,
    filename: String,
    source: AttachmentIoSource,
    origin: AttachmentOrigin,
}

#[derive(Debug)]
struct AttachmentActionContext {
    message_id: String,
    filename: String,
    origin: AttachmentOrigin,
}

impl AttachmentPayload {
    fn into_parts(self) -> (AttachmentActionContext, AttachmentIoSource) {
        (
            AttachmentActionContext {
                message_id: self.message_id,
                filename: self.filename,
                origin: self.origin,
            },
            self.source,
        )
    }
}

#[allow(
    deprecated,
    reason = "preserve the existing native attachment chooser during extraction"
)]
struct PendingAttachmentSave {
    id: u64,
    token: AttachmentIoToken,
    suggested_name: String,
    payload: AttachmentPayload,
    dialog: gtk::FileChooserNative,
}

#[derive(Debug, Clone)]
struct ActiveAttachmentIo {
    token: AttachmentIoToken,
    action: AttachmentIoAction,
    phase: &'static str,
}

#[derive(Debug, Clone)]
struct AttachmentIoCompletion {
    token: AttachmentIoToken,
    action: AttachmentIoAction,
    applied: bool,
    path: Option<PathBuf>,
    error: Option<String>,
}

#[derive(Debug, Default)]
struct AttachmentIoRuntime {
    active: Option<ActiveAttachmentIo>,
    in_flight: usize,
    completion_count: u64,
    stale_completion_count: u64,
    last_completion: Option<AttachmentIoCompletion>,
}

#[derive(Clone)]
struct AttachmentIoLauncher {
    application: gtk::Application,
    fixture_mode: bool,
    fail_next_fixture_write: Rc<Cell<bool>>,
    coordinator: Rc<RefCell<AttachmentIoCoordinator>>,
    runtime: Rc<RefCell<AttachmentIoRuntime>>,
    fixture_delay: Rc<Cell<Duration>>,
    opener: AttachmentOpener,
}

#[derive(Clone)]
enum AttachmentOpener {
    System,
    Fixture(Rc<RefCell<Vec<PathBuf>>>),
}

impl AttachmentOpener {
    fn open(&self, path: &Path) -> anyhow::Result<()> {
        match self {
            Self::System => {
                let file = gtk::gio::File::for_path(path);
                gtk::gio::AppInfo::launch_default_for_uri(
                    &file.uri(),
                    None::<&gtk::gio::AppLaunchContext>,
                )?;
            }
            Self::Fixture(calls) => calls.borrow_mut().push(path.to_path_buf()),
        }
        Ok(())
    }

    fn fixture_calls(&self) -> Option<Vec<PathBuf>> {
        match self {
            Self::System => None,
            Self::Fixture(calls) => Some(calls.borrow().clone()),
        }
    }
}

pub(crate) struct AttachmentActionResult {
    pub(crate) message_id: String,
    pub(crate) status: String,
    pub(crate) operation: String,
}

pub(crate) enum AttachmentEvent {
    Completed(Box<AttachmentActionResult>),
    Failed {
        action: &'static str,
        error: anyhow::Error,
    },
}

pub(crate) type AttachmentEventHandler = Rc<dyn Fn(AttachmentEvent)>;

#[derive(Clone)]
pub(crate) struct AttachmentController {
    window: gtk::glib::WeakRef<gtk::ApplicationWindow>,
    title: gtk::Label,
    scrolled: gtk::ScrolledWindow,
    list: gtk::ListBox,
    items: Rc<RefCell<Vec<ThreadAttachmentItem>>>,
    payloads: Rc<RefCell<AttachmentPayloadMap>>,
    open_dir: PathBuf,
    pending_save: Rc<RefCell<Option<PendingAttachmentSave>>>,
    next_save_id: Rc<Cell<u64>>,
    render_generation: Rc<Cell<u64>>,
    io: AttachmentIoLauncher,
    actions_sensitive: Rc<Cell<bool>>,
}

impl AttachmentController {
    pub(crate) fn new(
        window: &gtk::ApplicationWindow,
        open_dir: PathBuf,
        fixture_mode: bool,
    ) -> Self {
        let title = gtk::Label::new(Some("Attachments in thread"));
        title.set_xalign(0.0);
        title.add_css_class("dim-label");
        title.set_visible(false);

        let list = gtk::ListBox::new();
        list.set_widget_name("notm-attachment-list");
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("boxed-list");
        let scrolled = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(false)
            .child(&list)
            .build();
        scrolled.set_visible(false);

        Self {
            window: window.downgrade(),
            title,
            scrolled,
            list,
            items: Rc::new(RefCell::new(Vec::new())),
            payloads: Rc::new(RefCell::new(BTreeMap::new())),
            open_dir,
            pending_save: Rc::new(RefCell::new(None)),
            next_save_id: Rc::new(Cell::new(1)),
            render_generation: Rc::new(Cell::new(0)),
            io: AttachmentIoLauncher {
                application: window
                    .application()
                    .expect("attachment controller window must belong to an application"),
                fixture_mode,
                fail_next_fixture_write: Rc::new(Cell::new(false)),
                coordinator: Rc::new(RefCell::new(AttachmentIoCoordinator::default())),
                runtime: Rc::new(RefCell::new(AttachmentIoRuntime::default())),
                fixture_delay: Rc::new(Cell::new(Duration::ZERO)),
                opener: if fixture_mode {
                    AttachmentOpener::Fixture(Rc::new(RefCell::new(Vec::new())))
                } else {
                    AttachmentOpener::System
                },
            },
            actions_sensitive: Rc::new(Cell::new(true)),
        }
    }

    pub(crate) fn title_widget(&self) -> gtk::Label {
        self.title.clone()
    }

    pub(crate) fn scrolled_widget(&self) -> gtk::ScrolledWindow {
        self.scrolled.clone()
    }

    pub(crate) fn open_dir(&self) -> &Path {
        &self.open_dir
    }

    pub(crate) fn hide(&self) {
        self.invalidate_content();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.title.set_visible(false);
        self.scrolled.set_visible(false);
    }

    pub(crate) fn set_actions_sensitive(&self, sensitive: bool) {
        self.actions_sensitive.set(sensitive);
        self.list.set_sensitive(sensitive);
    }

    /// Prepared attachment metadata and MIME-part ordering do not change when
    /// tags change. Lazy payload sources resolve stale cached paths by
    /// Message-ID when read, so no retained summary needs replacement here.
    pub(crate) fn apply_authoritative_messages(&self, _messages: &[notm_notmuch::MessageSummary]) {}

    pub(crate) fn refresh_prepared(
        &self,
        attachments: &[PreparedAttachment],
        event_handler: AttachmentEventHandler,
    ) {
        let items = attachments
            .iter()
            .map(|attachment| ThreadAttachmentItem {
                message_index: attachment.message_index,
                attachment_index: attachment.attachment_index,
                message_id: attachment.message_id.clone(),
                filename: attachment.filename.clone(),
                content_type: attachment.content_type.clone(),
                size: attachment.size,
            })
            .collect();
        let payloads = attachments
            .iter()
            .map(|attachment| {
                (
                    (attachment.message_id.clone(), attachment.attachment_index),
                    AttachmentIoSource::mime_part(
                        attachment.source.clone(),
                        attachment.attachment_index,
                    ),
                )
            })
            .collect();
        self.replace_items(items, payloads, event_handler);
    }

    fn replace_items(
        &self,
        items: Vec<ThreadAttachmentItem>,
        payloads: AttachmentPayloadMap,
        event_handler: AttachmentEventHandler,
    ) {
        self.invalidate_content();
        self.payloads.replace(payloads);
        let generation = self.render_generation.get();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.items.replace(items);
        self.title.set_visible(false);
        self.scrolled.set_visible(false);
        self.append_row_chunk(generation, 0, event_handler);
    }

    fn invalidate_content(&self) {
        self.render_generation
            .set(self.render_generation.get().saturating_add(1));
        self.payloads.borrow_mut().clear();
        self.items.borrow_mut().clear();
        self.io.coordinator.borrow_mut().cancel();
        self.io.runtime.borrow_mut().active = None;
    }

    fn append_row_chunk(
        &self,
        generation: u64,
        start: usize,
        event_handler: AttachmentEventHandler,
    ) {
        if self.render_generation.get() != generation {
            return;
        }
        let end = (start + ATTACHMENT_ROWS_PER_UPDATE).min(self.items.borrow().len());
        let items = self.items.borrow()[start..end].to_vec();
        for (offset, item) in items.into_iter().enumerate() {
            let row_index = start + offset;
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
            self.connect_context_menu(&row, item, event_handler.clone());
            self.list.append(&row);
        }

        if end < self.items.borrow().len() {
            let controller = self.clone();
            gtk::glib::idle_add_local_once(move || {
                controller.append_row_chunk(generation, end, event_handler);
            });
            return;
        }

        let attachment_count = self.items.borrow().len();
        let has_attachments = attachment_count > 0;
        self.title.set_visible(has_attachments);
        self.scrolled.set_visible(has_attachments);
        if has_attachments {
            self.title.set_text(&format!(
                "{} attachment{} in thread",
                attachment_count,
                if attachment_count == 1 { "" } else { "s" }
            ));
            let visible_rows = attachment_count.min(4) as i32;
            let height = visible_rows * 34;
            self.scrolled.set_min_content_height(height);
            self.scrolled.set_max_content_height(height);
        }
        if let Some(row) = self.list.row_at_index(0) {
            self.list.select_row(Some(&row));
        }
    }

    pub(crate) fn items(&self) -> Vec<ThreadAttachmentItem> {
        self.items.borrow().clone()
    }

    pub(crate) fn select_index(&self, index: usize) -> Option<ThreadAttachmentItem> {
        let row = self.list.row_at_index(index as i32)?;
        self.list.select_row(Some(&row));
        self.items.borrow().get(index).cloned()
    }

    pub(crate) fn select_first_for_message(&self, message_index: usize) -> bool {
        let row_index = self
            .items
            .borrow()
            .iter()
            .position(|item| item.message_index == message_index);
        let Some(row) = row_index.and_then(|index| self.list.row_at_index(index as i32)) else {
            return false;
        };
        self.list.select_row(Some(&row));
        true
    }

    pub(crate) fn payload_at_index(&self, index: usize) -> anyhow::Result<AttachmentPayload> {
        anyhow::ensure!(
            self.actions_sensitive.get(),
            "attachment actions are unavailable while tags are changing"
        );
        match self.items.borrow().get(index).cloned() {
            Some(item) => self.thread_payload(&item),
            None => anyhow::bail!("attachment index {index} is not prepared"),
        }
    }

    pub(crate) fn active_payload(&self) -> anyhow::Result<AttachmentPayload> {
        anyhow::ensure!(
            self.actions_sensitive.get(),
            "attachment actions are unavailable while tags are changing"
        );
        match self.selected_thread_attachment() {
            Some(item) => self.thread_payload(&item),
            None => anyhow::bail!("no prepared attachment is selected"),
        }
    }

    #[allow(
        deprecated,
        reason = "preserve the existing native attachment chooser during extraction"
    )]
    pub(crate) fn request_save(
        &self,
        payload: AttachmentPayload,
        event_handler: AttachmentEventHandler,
    ) -> anyhow::Result<u64> {
        anyhow::ensure!(
            self.pending_save.borrow().is_none(),
            "an attachment save chooser is already open"
        );
        let chooser_id = self.next_save_id.get();
        let next_id = chooser_id
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("attachment save chooser id overflowed"))?;
        self.next_save_id.set(next_id);
        let token = self.begin_action(AttachmentIoAction::SaveToTarget, "chooser");

        let suggested_name = sanitize_attachment_filename(&payload.filename);
        let parent = self.window.upgrade();
        let dialog = gtk::FileChooserNative::new(
            Some("Save attachment"),
            parent.as_ref(),
            gtk::FileChooserAction::Save,
            Some("Save"),
            Some("Cancel"),
        );
        dialog.set_current_name(&suggested_name);
        self.pending_save.replace(Some(PendingAttachmentSave {
            id: chooser_id,
            token,
            suggested_name,
            payload,
            dialog: dialog.clone(),
        }));

        let pending_save = Rc::downgrade(&self.pending_save);
        let io = self.io.clone();
        dialog.connect_response(move |dialog, response| {
            let Some(pending_save) = pending_save.upgrade() else {
                return;
            };
            let is_current = pending_save
                .borrow()
                .as_ref()
                .is_some_and(|pending| pending.id == chooser_id);
            if !is_current {
                return;
            }
            let accepted = response == gtk::ResponseType::Accept;
            let target = accepted
                .then(|| dialog.file().and_then(|file| file.path()))
                .flatten();
            if let Err(error) = complete_pending_attachment_save(
                pending_save.as_ref(),
                chooser_id,
                accepted,
                target.as_deref(),
                &io,
                event_handler.clone(),
            ) {
                event_handler(AttachmentEvent::Failed {
                    action: "Save attachment",
                    error,
                });
            }
        });
        dialog.show();
        Ok(chooser_id)
    }

    pub(crate) fn complete_pending_save(
        &self,
        chooser_id: u64,
        accepted: bool,
        target: Option<&Path>,
        event_handler: AttachmentEventHandler,
    ) -> anyhow::Result<Option<AttachmentIoToken>> {
        complete_pending_attachment_save(
            self.pending_save.as_ref(),
            chooser_id,
            accepted,
            target,
            &self.io,
            event_handler,
        )
    }

    pub(crate) fn save_to_directory(
        &self,
        payload: AttachmentPayload,
        target_dir: PathBuf,
        event_handler: AttachmentEventHandler,
    ) -> AttachmentIoToken {
        let token = self.begin_action(AttachmentIoAction::SaveToDirectory, "writing");
        let (context, source) = payload.into_parts();
        let request = AttachmentIoRequest::save_to_directory(
            token,
            target_dir,
            context.filename.clone(),
            source,
        )
        .with_fixture_delay(self.io.fixture_delay.get());
        launch_attachment_worker(request, context, &self.io, event_handler);
        token
    }

    pub(crate) fn open(
        &self,
        payload: AttachmentPayload,
        event_handler: AttachmentEventHandler,
    ) -> AttachmentIoToken {
        let token = self.begin_action(AttachmentIoAction::PrepareOpen, "writing");
        let (context, source) = payload.into_parts();
        let request = AttachmentIoRequest::prepare_open(
            token,
            self.open_dir.clone(),
            context.filename.clone(),
            source,
        )
        .with_fixture_delay(self.io.fixture_delay.get());
        launch_attachment_worker(request, context, &self.io, event_handler);
        token
    }

    fn begin_action(&self, action: AttachmentIoAction, phase: &'static str) -> AttachmentIoToken {
        let token = self.io.coordinator.borrow_mut().begin();
        self.io.runtime.borrow_mut().active = Some(ActiveAttachmentIo {
            token,
            action,
            phase,
        });
        token
    }

    pub(crate) fn set_fixture_io_delay(&self, delay: Duration) -> Duration {
        let delay = delay.min(MAX_FIXTURE_DELAY);
        self.io.fixture_delay.set(delay);
        delay
    }

    pub(crate) fn fixture_io_delay(&self) -> Duration {
        self.io.fixture_delay.get()
    }

    pub(crate) fn fail_next_fixture_write(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.io.fixture_mode,
            "attachment write failure injection is available only in fixture mode"
        );
        self.io.fail_next_fixture_write.set(true);
        Ok(())
    }

    pub(crate) fn io_status_json(&self) -> serde_json::Value {
        attachment_io_status_json(
            &self.io.coordinator.borrow(),
            &self.io.runtime.borrow(),
            self.io.fixture_delay.get(),
            self.io.fail_next_fixture_write.get(),
        )
    }

    pub(crate) fn pending_save_id(&self) -> Option<u64> {
        self.pending_save
            .borrow()
            .as_ref()
            .map(|pending| pending.id)
    }

    pub(crate) fn test_state_json(&self, status_text: &str) -> serde_json::Value {
        let mut row_count = 0_usize;
        while self.list.row_at_index(row_count as i32).is_some() {
            row_count += 1;
        }
        let save_chooser = self.pending_save.borrow().as_ref().map(|pending| {
            json!({
                "id": pending.id,
                "suggested_name": pending.suggested_name,
                "visible": pending.dialog.is_visible(),
            })
        });
        let fake_opener_calls = self.io.opener.fixture_calls();
        json!({
            "ok": true,
            "save_chooser": save_chooser,
            "status_text": status_text,
            "open_temp_dir": self.open_dir,
            "fake_opener": fake_opener_calls.is_some(),
            "fake_opener_calls": fake_opener_calls.unwrap_or_default(),
            "row_count": row_count,
            "io": self.io_status_json(),
        })
    }

    fn connect_context_menu(
        &self,
        row: &gtk::ListBoxRow,
        item: ThreadAttachmentItem,
        event_handler: AttachmentEventHandler,
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

        let controller = self.clone();
        let save_item = item.clone();
        let save_popover = popover.clone();
        let save_handler = event_handler.clone();
        save_button.connect_clicked(move |_| {
            save_popover.popdown();
            let result = controller
                .thread_payload(&save_item)
                .and_then(|payload| controller.request_save(payload, save_handler.clone()));
            if let Err(error) = result {
                save_handler(AttachmentEvent::Failed {
                    action: "Save attachment",
                    error,
                });
            }
        });

        let controller = self.clone();
        let open_popover = popover.clone();
        let open_item = item.clone();
        let open_handler = event_handler.clone();
        open_button.connect_clicked(move |_| {
            open_popover.popdown();
            if let Err(error) = controller.open_thread_attachment(&open_item, open_handler.clone())
            {
                open_handler(AttachmentEvent::Failed {
                    action: "Open attachment",
                    error,
                });
            }
        });

        let open_click = gtk::GestureClick::new();
        open_click.set_button(1);
        let controller = self.clone();
        let open_item = item.clone();
        let open_row = row.clone();
        let double_click_handler = event_handler;
        open_click.connect_pressed(move |_, n_press, _, _| {
            if n_press != 2 {
                return;
            }
            if let Some(parent) = open_row.parent()
                && let Ok(list) = parent.downcast::<gtk::ListBox>()
            {
                list.select_row(Some(&open_row));
            }
            if let Err(error) =
                controller.open_thread_attachment(&open_item, double_click_handler.clone())
            {
                double_click_handler(AttachmentEvent::Failed {
                    action: "Open attachment",
                    error,
                });
            }
        });
        row.add_controller(open_click);

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

    fn thread_payload(&self, item: &ThreadAttachmentItem) -> anyhow::Result<AttachmentPayload> {
        anyhow::ensure!(
            self.actions_sensitive.get(),
            "attachment actions are unavailable while tags are changing"
        );
        let source = self
            .payloads
            .borrow()
            .get(&(item.message_id.clone(), item.attachment_index))
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("attachment payload is still loading"))?;
        Ok(AttachmentPayload {
            message_id: item.message_id.clone(),
            filename: item.filename.clone(),
            source,
            origin: AttachmentOrigin::Thread,
        })
    }

    fn selected_thread_attachment(&self) -> Option<ThreadAttachmentItem> {
        let index = self
            .list
            .selected_row()
            .map(|row| row.index() as usize)
            .unwrap_or(0);
        self.items.borrow().get(index).cloned()
    }

    fn open_thread_attachment(
        &self,
        item: &ThreadAttachmentItem,
        event_handler: AttachmentEventHandler,
    ) -> anyhow::Result<AttachmentIoToken> {
        let payload = self.thread_payload(item)?;
        Ok(self.open(payload, event_handler))
    }
}

fn complete_pending_attachment_save(
    pending_save: &RefCell<Option<PendingAttachmentSave>>,
    chooser_id: u64,
    accepted: bool,
    target: Option<&Path>,
    io: &AttachmentIoLauncher,
    event_handler: AttachmentEventHandler,
) -> anyhow::Result<Option<AttachmentIoToken>> {
    let pending = {
        let mut slot = pending_save.borrow_mut();
        let pending = slot
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no attachment save chooser is pending"))?;
        anyhow::ensure!(
            pending.id == chooser_id,
            "attachment save chooser id does not match the pending chooser"
        );
        slot.take().expect("pending chooser checked above")
    };
    pending.dialog.hide();
    pending.dialog.destroy();

    if !accepted {
        finish_attachment_action_without_worker(io, pending.token);
        return Ok(None);
    }

    let Some(target) = target else {
        finish_attachment_action_without_worker(io, pending.token);
        anyhow::bail!("the attachment save chooser did not return a local target path");
    };
    let token = pending.token;
    let (context, source) = pending.payload.into_parts();
    let request = AttachmentIoRequest::save_to_target(token, target.to_path_buf(), source)
        .with_fixture_delay(io.fixture_delay.get());
    launch_attachment_worker(request, context, io, event_handler);
    Ok(Some(token))
}

fn finish_attachment_action_without_worker(io: &AttachmentIoLauncher, token: AttachmentIoToken) {
    if io.coordinator.borrow_mut().finish(token) {
        io.runtime.borrow_mut().active = None;
    }
}

fn launch_attachment_worker(
    request: AttachmentIoRequest,
    context: AttachmentActionContext,
    io: &AttachmentIoLauncher,
    event_handler: AttachmentEventHandler,
) {
    let request = request.with_fixture_fail_before_publish(
        io.fixture_mode && io.fail_next_fixture_write.replace(false),
    );
    let token = request.token();
    let action = request.action();
    if io.coordinator.borrow().accepts(token)
        && let Some(active) = io.runtime.borrow_mut().active.as_mut()
        && active.token == token
    {
        active.phase = "writing";
    }
    {
        let mut runtime = io.runtime.borrow_mut();
        runtime.in_flight = runtime.in_flight.saturating_add(1);
    }
    // The last window may close while a delayed or large attachment is still
    // being written. Keep the application running until the worker response
    // has been observed so process teardown cannot interrupt the write.
    let application_hold = io.application.hold();
    let receiver = attachment_io::spawn(request);
    let io = io.clone();
    let mut context = Some(context);
    let mut application_hold = Some(application_hold);
    gtk::glib::timeout_add_local(ATTACHMENT_WORKER_POLL_INTERVAL, move || {
        let _keep_application_alive = application_hold.as_ref();
        match receiver.try_recv() {
            Ok(response) => {
                complete_attachment_worker(
                    response,
                    context.take().expect("attachment worker completes once"),
                    action,
                    &io.coordinator,
                    &io.runtime,
                    &io.opener,
                    &event_handler,
                );
                drop(application_hold.take());
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                complete_disconnected_attachment_worker(
                    token,
                    action,
                    &io.coordinator,
                    &io.runtime,
                    &event_handler,
                );
                drop(application_hold.take());
                gtk::glib::ControlFlow::Break
            }
        }
    });
}

fn complete_attachment_worker(
    response: AttachmentIoResponse,
    context: AttachmentActionContext,
    action: AttachmentIoAction,
    io_coordinator: &Rc<RefCell<AttachmentIoCoordinator>>,
    io_runtime: &Rc<RefCell<AttachmentIoRuntime>>,
    opener: &AttachmentOpener,
    event_handler: &AttachmentEventHandler,
) {
    let applied = io_coordinator.borrow_mut().finish(response.token);
    {
        let mut runtime = io_runtime.borrow_mut();
        runtime.in_flight = runtime.in_flight.saturating_sub(1);
        runtime.completion_count = runtime.completion_count.saturating_add(1);
        if applied {
            runtime.active = None;
        } else {
            runtime.stale_completion_count = runtime.stale_completion_count.saturating_add(1);
        }
    }

    if !applied {
        let (path, error) = attachment_response_snapshot(&response);
        io_runtime.borrow_mut().last_completion = Some(AttachmentIoCompletion {
            token: response.token,
            action,
            applied: false,
            path,
            error,
        });
        return;
    }

    match response.result {
        Ok(completed) => {
            debug_assert_eq!(completed.action, action);
            let path = completed.path;
            let action_result = if action == AttachmentIoAction::PrepareOpen {
                opener
                    .open(&path)
                    .map(|()| opened_action(&context, path.clone()))
            } else {
                Ok(saved_action(&context, path.clone()))
            };
            match action_result {
                Ok(result) => {
                    io_runtime.borrow_mut().last_completion = Some(AttachmentIoCompletion {
                        token: response.token,
                        action,
                        applied: true,
                        path: Some(path),
                        error: None,
                    });
                    event_handler(AttachmentEvent::Completed(Box::new(result)));
                }
                Err(error) => {
                    let error_text = error.to_string();
                    io_runtime.borrow_mut().last_completion = Some(AttachmentIoCompletion {
                        token: response.token,
                        action,
                        applied: true,
                        path: Some(path),
                        error: Some(error_text),
                    });
                    event_handler(AttachmentEvent::Failed {
                        action: attachment_action_label(action),
                        error,
                    });
                }
            }
        }
        Err(error) => {
            let error_text = error.to_string();
            io_runtime.borrow_mut().last_completion = Some(AttachmentIoCompletion {
                token: response.token,
                action,
                applied: true,
                path: None,
                error: Some(error_text),
            });
            event_handler(AttachmentEvent::Failed {
                action: attachment_action_label(action),
                error: anyhow::Error::new(error),
            });
        }
    }
}

fn complete_disconnected_attachment_worker(
    token: AttachmentIoToken,
    action: AttachmentIoAction,
    io_coordinator: &Rc<RefCell<AttachmentIoCoordinator>>,
    io_runtime: &Rc<RefCell<AttachmentIoRuntime>>,
    event_handler: &AttachmentEventHandler,
) {
    let error = "attachment worker stopped without returning a result".to_string();
    let applied = io_coordinator.borrow_mut().finish(token);
    {
        let mut runtime = io_runtime.borrow_mut();
        runtime.in_flight = runtime.in_flight.saturating_sub(1);
        runtime.completion_count = runtime.completion_count.saturating_add(1);
        if applied {
            runtime.active = None;
        } else {
            runtime.stale_completion_count = runtime.stale_completion_count.saturating_add(1);
        }
        runtime.last_completion = Some(AttachmentIoCompletion {
            token,
            action,
            applied,
            path: None,
            error: Some(error.clone()),
        });
    }
    if applied {
        event_handler(AttachmentEvent::Failed {
            action: attachment_action_label(action),
            error: anyhow::anyhow!(error),
        });
    }
}

fn attachment_response_snapshot(
    response: &AttachmentIoResponse,
) -> (Option<PathBuf>, Option<String>) {
    match &response.result {
        Ok(completed) => (Some(completed.path.clone()), None),
        Err(error) => (None, Some(error.to_string())),
    }
}

fn attachment_action_label(action: AttachmentIoAction) -> &'static str {
    match action {
        AttachmentIoAction::SaveToDirectory | AttachmentIoAction::SaveToTarget => "Save attachment",
        AttachmentIoAction::PrepareOpen => "Open attachment",
    }
}

fn attachment_io_status_json(
    coordinator: &AttachmentIoCoordinator,
    runtime: &AttachmentIoRuntime,
    fixture_delay: Duration,
    fail_next_fixture_write: bool,
) -> serde_json::Value {
    let active = runtime.active.as_ref().map(|active| {
        json!({
            "generation": active.token.generation,
            "request_id": active.token.request_id,
            "action": active.action.as_str(),
            "phase": active.phase,
        })
    });
    let last_completion = runtime.last_completion.as_ref().map(|completion| {
        json!({
            "generation": completion.token.generation,
            "request_id": completion.token.request_id,
            "action": completion.action.as_str(),
            "applied": completion.applied,
            "path": completion.path,
            "error": completion.error,
        })
    });
    json!({
        "ok": true,
        "busy": runtime.in_flight > 0,
        "in_flight": runtime.in_flight,
        "pending": active.is_some(),
        "active": active,
        "active_token": coordinator.active_token().map(|token| json!({
            "generation": token.generation,
            "request_id": token.request_id,
        })),
        "completion_count": runtime.completion_count,
        "stale_completion_count": runtime.stale_completion_count,
        "last_completion": last_completion,
        "fixture_delay_ms": u64::try_from(fixture_delay.as_millis()).unwrap_or(u64::MAX),
        "fail_next_fixture_write": fail_next_fixture_write,
    })
}

fn saved_action(context: &AttachmentActionContext, path: PathBuf) -> AttachmentActionResult {
    let operation = match context.origin {
        AttachmentOrigin::Thread => format!(
            "saved thread attachment {} from message {} to {}",
            context.filename,
            context.message_id,
            path.display()
        ),
    };
    AttachmentActionResult {
        message_id: context.message_id.clone(),
        status: format!("Attachment saved to {}", path.display()),
        operation,
    }
}

fn opened_action(context: &AttachmentActionContext, path: PathBuf) -> AttachmentActionResult {
    AttachmentActionResult {
        message_id: context.message_id.clone(),
        status: format!("Opened attachment {}", path.display()),
        operation: format!("opened attachment {}", path.display()),
    }
}

pub(crate) fn set_compose_attachment_label(label: &gtk::Label, attachments: &[String]) {
    if attachments.is_empty() {
        label.set_text("No attachments");
    } else {
        label.set_text(&format!("Attachments: {}", attachments.join(", ")));
    }
}

pub(crate) fn load_compose_attachments(
    fields: &ComposeFields,
) -> anyhow::Result<Vec<AttachmentInput>> {
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

#[cfg(test)]
pub(crate) fn attachment_inputs_from_bytes(bytes: &[u8]) -> anyhow::Result<Vec<AttachmentInput>> {
    Ok(extract_attachments(bytes)?
        .into_iter()
        .map(|attachment| AttachmentInput {
            filename: attachment.filename,
            content_type: attachment.content_type,
            bytes: attachment.bytes,
            source_path: None,
        })
        .collect())
}

#[cfg(test)]
pub(crate) fn cache_composer_attachments(
    attachments: &[AttachmentInput],
    directory: &Path,
    write: impl Fn(&Path, &[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<Vec<String>> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }
    ensure_private_directory(directory)?;
    attachments
        .iter()
        .map(|attachment| {
            if let Some(source_path) = &attachment.source_path
                && source_path.exists()
            {
                return Ok(source_path.display().to_string());
            }
            // Isolate cached files in unique private directories instead of
            // prefixing their basenames. The composer derives the outgoing
            // attachment filename from this path, so changing the basename
            // would leak cache bookkeeping into saved/reopened messages.
            let attachment_directory = directory.join(Uuid::new_v4().to_string());
            ensure_private_directory(&attachment_directory)?;
            let path =
                attachment_directory.join(sanitize_attachment_filename(&attachment.filename));
            write(&path, &attachment.bytes)?;
            #[cfg(unix)]
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok(path.display().to_string())
        })
        .collect()
}

#[cfg(test)]
fn ensure_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_open_store_is_removed_when_owner_drops() {
        let store = AttachmentOpenStore::create().expect("attachment-open temp directory");
        let path = store.path().to_path_buf();
        std::fs::write(path.join("attachment.txt"), b"attachment")
            .expect("write temporary attachment");

        drop(store);

        assert!(
            !path.exists(),
            "dropping the application-owned store must remove {}",
            path.display()
        );
    }

    #[cfg(unix)]
    #[test]
    fn attachment_open_store_has_private_unix_permissions() {
        let store = AttachmentOpenStore::create().expect("attachment-open temp directory");
        let mode = std::fs::metadata(store.path())
            .expect("attachment-open directory metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(mode, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn cached_compose_attachments_have_private_unix_permissions() {
        let root = tempfile::tempdir().expect("temporary attachment cache root");
        let directory = root.path().join("notm/compose-attachments");
        std::fs::create_dir_all(&directory).expect("create non-private cache directory");
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755))
            .expect("make cache directory non-private");
        let attachment = AttachmentInput {
            filename: "private.txt".to_string(),
            content_type: "text/plain".to_string(),
            bytes: b"private attachment".to_vec(),
            source_path: None,
        };

        let cached = cache_composer_attachments(&[attachment], &directory, |path, bytes| {
            std::fs::write(path, bytes)?;
            Ok(())
        })
        .expect("cache compose attachment");

        assert_eq!(cached.len(), 1);
        assert_eq!(
            Path::new(&cached[0])
                .file_name()
                .and_then(|name| name.to_str()),
            Some("private.txt"),
            "cache bookkeeping changed the outgoing attachment filename"
        );
        assert_eq!(
            std::fs::metadata(&directory)
                .expect("cache directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(
                Path::new(&cached[0])
                    .parent()
                    .expect("cached attachment directory"),
            )
            .expect("cached attachment directory metadata")
            .permissions()
            .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&cached[0])
                .expect("cached attachment metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        for unsafe_name in [".", ".."] {
            let attachment = AttachmentInput {
                filename: unsafe_name.to_string(),
                content_type: "application/octet-stream".to_string(),
                bytes: b"unusual attachment".to_vec(),
                source_path: None,
            };
            let cached = cache_composer_attachments(&[attachment], &directory, |path, bytes| {
                std::fs::write(path, bytes)?;
                Ok(())
            })
            .expect("cache dot attachment filename");
            let cached_path = Path::new(&cached[0]);
            assert_eq!(
                cached_path.file_name().and_then(|name| name.to_str()),
                Some("attachment.bin")
            );
            assert_eq!(
                cached_path.parent().and_then(Path::parent),
                Some(directory.as_path()),
                "dot attachment escaped its unique cache directory"
            );
        }
    }
}
