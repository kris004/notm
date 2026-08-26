use std::{
    cell::{Cell, RefCell},
    io,
    path::{Path, PathBuf},
    rc::Rc,
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

use gtk::prelude::*;
use gtk4 as gtk;
use notm_mail::{
    attachments::{
        sanitize_attachment_filename, save_attachment_to_target_without_overwrite,
        save_attachment_without_overwrite,
    },
    compose::AttachmentInput,
    mime::{extract_attachments, extract_attachments_from_reader_detailed},
};
use notm_notmuch::{Database, DatabaseMode, MessageSummary, OpenConfig};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::model::ComposeFields;

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
    message_index: usize,
    /// Stable depth-first attachment MIME-part index within the message.
    attachment_index: usize,
    message_id: String,
    filename: String,
    content_type: String,
    size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachmentOrigin {
    SelectedMessage,
    Thread,
}

#[derive(Debug, Clone)]
pub(crate) struct AttachmentPayload {
    message: MessageSummary,
    filename: String,
    bytes: Vec<u8>,
    origin: AttachmentOrigin,
}

#[allow(
    deprecated,
    reason = "preserve the existing native attachment chooser during extraction"
)]
struct PendingAttachmentSave {
    id: u64,
    suggested_name: String,
    payload: AttachmentPayload,
    dialog: gtk::FileChooserNative,
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
    pub(crate) path: PathBuf,
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
    messages: Rc<RefCell<Vec<MessageSummary>>>,
    open_config: OpenConfig,
    open_dir: PathBuf,
    pending_save: Rc<RefCell<Option<PendingAttachmentSave>>>,
    next_save_id: Rc<Cell<u64>>,
    opener: AttachmentOpener,
    actions_sensitive: Rc<Cell<bool>>,
}

impl AttachmentController {
    pub(crate) fn new(
        window: &gtk::ApplicationWindow,
        open_dir: PathBuf,
        fixture_mode: bool,
        open_config: OpenConfig,
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
            messages: Rc::new(RefCell::new(Vec::new())),
            open_config,
            open_dir,
            pending_save: Rc::new(RefCell::new(None)),
            next_save_id: Rc::new(Cell::new(1)),
            opener: if fixture_mode {
                AttachmentOpener::Fixture(Rc::new(RefCell::new(Vec::new())))
            } else {
                AttachmentOpener::System
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
        self.title.set_visible(false);
        self.scrolled.set_visible(false);
    }

    pub(crate) fn set_actions_sensitive(&self, sensitive: bool) {
        self.actions_sensitive.set(sensitive);
        self.list.set_sensitive(sensitive);
    }

    /// Replace retained message paths/tags without reparsing MIME bodies.
    /// Existing attachment rows remain valid because tag mutations do not
    /// change message content or MIME part ordering.
    pub(crate) fn apply_authoritative_messages(&self, messages: &[MessageSummary]) {
        self.messages.replace(messages.to_vec());
    }

    pub(crate) fn refresh(
        &self,
        messages: &[MessageSummary],
        event_handler: AttachmentEventHandler,
    ) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.items.borrow_mut().clear();
        self.messages.replace(messages.to_vec());
        let database = Database::open(&self.open_config, DatabaseMode::ReadOnly).ok();

        for (message_index, message) in messages.iter().enumerate() {
            let Some(database) = database.as_ref() else {
                continue;
            };
            let Ok(source) = database.open_message_file(message) else {
                continue;
            };
            let Ok(report) = extract_attachments_from_reader_detailed(source) else {
                continue;
            };
            for attachment in report.attachments {
                let item = ThreadAttachmentItem {
                    message_index,
                    attachment_index: attachment.part_index,
                    message_id: message.message_id.clone(),
                    filename: attachment.filename,
                    content_type: attachment.content_type,
                    size: attachment.bytes.len(),
                };
                let row_index = self.items.borrow().len();
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
                self.connect_context_menu(&row, item.clone(), event_handler.clone());
                self.list.append(&row);
                self.items.borrow_mut().push(item);
            }
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

    pub(crate) fn payload_at_index(
        &self,
        selected_message: Option<MessageSummary>,
        index: usize,
    ) -> anyhow::Result<AttachmentPayload> {
        anyhow::ensure!(
            self.actions_sensitive.get(),
            "attachment actions are unavailable while tags are changing"
        );
        match self.items.borrow().get(index).cloned() {
            Some(item) => self.thread_payload(&item),
            None => selected_attachment_payload(&self.open_config, selected_message, index),
        }
    }

    pub(crate) fn active_payload(
        &self,
        selected_message: Option<MessageSummary>,
    ) -> anyhow::Result<AttachmentPayload> {
        anyhow::ensure!(
            self.actions_sensitive.get(),
            "attachment actions are unavailable while tags are changing"
        );
        match self.selected_thread_attachment() {
            Some(item) => self.thread_payload(&item),
            None => selected_attachment_payload(&self.open_config, selected_message, 0),
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
            suggested_name,
            payload,
            dialog: dialog.clone(),
        }));

        let pending_save = Rc::downgrade(&self.pending_save);
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
            emit_result(
                &event_handler,
                "Save attachment",
                complete_pending_attachment_save(
                    pending_save.as_ref(),
                    chooser_id,
                    accepted,
                    target.as_deref(),
                ),
            );
        });
        dialog.show();
        Ok(chooser_id)
    }

    pub(crate) fn complete_pending_save(
        &self,
        chooser_id: u64,
        accepted: bool,
        target: Option<&Path>,
    ) -> anyhow::Result<Option<AttachmentActionResult>> {
        complete_pending_attachment_save(self.pending_save.as_ref(), chooser_id, accepted, target)
    }

    pub(crate) fn save_to_directory(
        &self,
        payload: &AttachmentPayload,
        target_dir: &Path,
    ) -> anyhow::Result<AttachmentActionResult> {
        let path =
            save_attachment_without_overwrite(target_dir, &payload.filename, &payload.bytes)?;
        Ok(saved_action(payload, path))
    }

    pub(crate) fn open(
        &self,
        payload: &AttachmentPayload,
    ) -> anyhow::Result<AttachmentActionResult> {
        let path =
            save_attachment_without_overwrite(&self.open_dir, &payload.filename, &payload.bytes)?;
        self.opener.open(&path)?;
        Ok(AttachmentActionResult {
            message_id: payload.message.message_id.clone(),
            status: format!("Opened attachment {}", path.display()),
            operation: format!("opened attachment {}", path.display()),
            path,
        })
    }

    pub(crate) fn pending_save_id(&self) -> Option<u64> {
        self.pending_save
            .borrow()
            .as_ref()
            .map(|pending| pending.id)
    }

    pub(crate) fn test_state_json(&self, status_text: &str) -> serde_json::Value {
        let save_chooser = self.pending_save.borrow().as_ref().map(|pending| {
            json!({
                "id": pending.id,
                "suggested_name": pending.suggested_name,
                "visible": pending.dialog.is_visible(),
            })
        });
        let fake_opener_calls = self.opener.fixture_calls();
        json!({
            "ok": true,
            "save_chooser": save_chooser,
            "status_text": status_text,
            "open_temp_dir": self.open_dir,
            "fake_opener": fake_opener_calls.is_some(),
            "fake_opener_calls": fake_opener_calls.unwrap_or_default(),
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
            emit_result(
                &open_handler,
                "Open attachment",
                controller.open_thread_attachment(&open_item).map(Some),
            );
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
            emit_result(
                &double_click_handler,
                "Open attachment",
                controller.open_thread_attachment(&open_item).map(Some),
            );
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
        let message = self
            .messages
            .borrow()
            .get(item.message_index)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("attachment message index not found"))?;
        let database = Database::open(&self.open_config, DatabaseMode::ReadOnly)?;
        let source = database.open_message_file(&message)?;
        let report = extract_attachments_from_reader_detailed(source)?;
        let attachment = report
            .attachments
            .into_iter()
            .find(|attachment| attachment.part_index == item.attachment_index)
            .ok_or_else(|| anyhow::anyhow!("attachment MIME part not found"))?;
        Ok(AttachmentPayload {
            message,
            filename: attachment.filename,
            bytes: attachment.bytes,
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
    ) -> anyhow::Result<AttachmentActionResult> {
        let payload = self.thread_payload(item)?;
        self.open(&payload)
    }
}

fn complete_pending_attachment_save(
    pending_save: &RefCell<Option<PendingAttachmentSave>>,
    chooser_id: u64,
    accepted: bool,
    target: Option<&Path>,
) -> anyhow::Result<Option<AttachmentActionResult>> {
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

    let result = if accepted {
        let target = target.ok_or_else(|| {
            anyhow::anyhow!("the attachment save chooser did not return a local target path")
        });
        target.and_then(|target| {
            let path = save_attachment_to_target_without_overwrite(target, &pending.payload.bytes)?;
            Ok(Some(saved_action(&pending.payload, path)))
        })
    } else {
        Ok(None)
    };

    pending.dialog.hide();
    pending.dialog.destroy();
    result
}

fn emit_result(
    handler: &AttachmentEventHandler,
    action: &'static str,
    result: anyhow::Result<Option<AttachmentActionResult>>,
) {
    match result {
        Ok(Some(result)) => handler(AttachmentEvent::Completed(Box::new(result))),
        Ok(None) => {}
        Err(error) => handler(AttachmentEvent::Failed { action, error }),
    }
}

fn selected_attachment_payload(
    open_config: &OpenConfig,
    selected_message: Option<MessageSummary>,
    index: usize,
) -> anyhow::Result<AttachmentPayload> {
    let message = selected_message.ok_or_else(|| anyhow::anyhow!("no selected message"))?;
    let database = Database::open(open_config, DatabaseMode::ReadOnly)?;
    let source = database.open_message_file(&message)?;
    let report = extract_attachments_from_reader_detailed(source)?;
    let attachment = report
        .attachments
        .get(index)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("attachment index {index} not found"))?;
    Ok(AttachmentPayload {
        message,
        filename: attachment.filename,
        bytes: attachment.bytes,
        origin: AttachmentOrigin::SelectedMessage,
    })
}

fn saved_action(payload: &AttachmentPayload, path: PathBuf) -> AttachmentActionResult {
    let operation = match payload.origin {
        AttachmentOrigin::SelectedMessage => format!(
            "saved attachment {} from {} to {}",
            payload.filename,
            payload.message.message_id,
            path.display()
        ),
        AttachmentOrigin::Thread => format!(
            "saved thread attachment {} from message {} to {}",
            payload.filename,
            payload.message.message_id,
            path.display()
        ),
    };
    AttachmentActionResult {
        message_id: payload.message.message_id.clone(),
        status: format!("Attachment saved to {}", path.display()),
        operation,
        path,
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
