use std::{
    cell::{Cell, RefCell},
    ffi::OsStr,
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use chrono::Utc;
use gtk::glib::translate::IntoGlib;
use gtk::prelude::*;
use gtk4 as gtk;
#[cfg(test)]
use notm_mail::mime::parse_file;
use notm_mail::{
    ComposedMessage, ParsedMessage,
    address::{format_address, parse_address_list_checked, parse_one_checked},
    compose::AttachmentInput,
};
use serde::{Deserialize, Serialize};
use sourceview5::{Buffer as SourceBuffer, View as SourceView, VimIMContext};
use uuid::Uuid;

use crate::{
    draft_io::ensure_named_draft_save_fits,
    draft_recovery::MAX_RECOVERY_BYTES,
    model::{ActiveDraft, ComposeFields},
};

use super::attachments;

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

const COMPOSE_BODY_MIN_HEIGHT: i32 = 96;
const COMPOSE_BODY_NATURAL_HEIGHT: i32 = 260;
const KEYBOARD_CURSOR_CLASS: &str = "notm-keyboard-cursor";
pub(crate) const DRAFT_LIST_MIN_HEIGHT: i32 = 72;
pub(crate) const DRAFT_LIST_MAX_HEIGHT: i32 = 160;

#[derive(Debug, Clone)]
pub(crate) struct ComposerPaths {
    pub(crate) recovery: PathBuf,
    pub(crate) legacy_recovery: Option<PathBuf>,
    pub(crate) drafts: PathBuf,
    pub(crate) legacy_drafts: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecipientField {
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

pub(crate) type AddressSuggestionsProvider = Rc<dyn Fn() -> Vec<String>>;
pub(crate) type ComposerEditedHandler = Rc<dyn Fn()>;
pub(crate) type ComposerStatusHandler = Rc<dyn Fn(String)>;
pub(crate) type ComposerVimWriteHandler = Rc<dyn Fn(Option<String>)>;

struct ConfirmationController {
    next_id: u64,
    pending: Option<PendingConfirmation>,
    last_completion: Option<ConfirmationCompletion>,
    allow_close_once: bool,
}

struct ConfirmationCompletion {
    id: u64,
    accepted: bool,
    succeeded: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingConfirmationSnapshot {
    pub(crate) id: u64,
    pub(crate) kind: &'static str,
    pub(crate) title: &'static str,
    pub(crate) confirm_label: &'static str,
    pub(crate) visible: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ConfirmationCompletionSnapshot {
    pub(crate) id: u64,
    pub(crate) accepted: bool,
    pub(crate) succeeded: bool,
}

pub(crate) enum ConfirmationDisposition {
    Immediate(PendingAction),
    Pending { title: &'static str },
}

pub(crate) type ConfirmationResponseHandler = Rc<dyn Fn(u64, bool)>;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ConfirmationPrompt {
    pub(crate) id: u64,
    pub(crate) title: &'static str,
    pub(crate) detail: &'static str,
    pub(crate) confirm_label: &'static str,
}

pub(crate) type ConfirmationPresenter =
    Rc<dyn Fn(ConfirmationPrompt, ConfirmationResponseHandler) -> gtk::glib::WeakRef<gtk::Widget>>;

struct PendingConfirmation {
    id: u64,
    action: PendingAction,
    dialog: gtk::glib::WeakRef<gtk::Widget>,
}

pub(crate) enum PendingAction {
    ClearComposer(TransitionHooks),
    ReplaceComposer {
        kind: ComposerReplacementKind,
        hooks: TransitionHooks,
    },
    DeleteActiveDraft(TransitionHooks),
    DeleteNamedDraft(TransitionHooks),
    SaveDraftReplacement(TransitionHooks),
    SendComposer(TransitionHooks),
    ShowSelectedMessage(TransitionHooks),
    CloseMainWindow(TransitionHooks),
}

pub(crate) struct TransitionHooks {
    accept: Box<dyn FnOnce() -> bool>,
    reject: Box<dyn FnOnce()>,
}

impl TransitionHooks {
    pub(crate) fn new(
        accept: impl FnOnce() -> bool + 'static,
        reject: impl FnOnce() + 'static,
    ) -> Self {
        Self {
            accept: Box::new(accept),
            reject: Box::new(reject),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingOperation {
    DraftClear,
    DraftLoad,
    ComposeReplace,
    DraftDelete,
    DraftSave,
    Send,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposerReplacementKind {
    New,
    Mailto,
    Reply,
    ReplyAll,
    Forward,
    ForwardAttachment,
    StandaloneReply,
    StandaloneReplyAll,
    StandaloneForward,
    StandaloneForwardAttachment,
    NamedDraft,
    RecoveryDraft,
    IndexedDraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistedDraftDeletion {
    ExplicitActive,
    ExplicitNamed,
    SaveReplacement,
    AcceptedSendCleanup,
}

#[derive(Debug, Clone)]
pub(crate) struct SendSnapshot {
    pub(crate) fields: ComposeFields,
    pub(crate) generation: u64,
    pub(crate) active_draft: Option<ActiveDraft>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendCleanupStage {
    SentPersistence,
    DraftDelete,
    RecoveryClear,
    NewerDraftAutosave,
    DraftIdentity,
}

#[derive(Debug)]
pub(crate) struct SendCleanupIssue {
    pub(crate) stage: SendCleanupStage,
    pub(crate) error: String,
}

impl SendCleanupIssue {
    pub(crate) fn new(stage: SendCleanupStage, error: impl ToString) -> Self {
        Self {
            stage,
            error: error.to_string(),
        }
    }

    fn description(&self) -> &'static str {
        match self.stage {
            SendCleanupStage::SentPersistence => "sent save/index failed",
            SendCleanupStage::DraftDelete => "draft delete failed",
            SendCleanupStage::RecoveryClear => "draft recovery clear failed",
            SendCleanupStage::NewerDraftAutosave => "newer draft autosave failed",
            SendCleanupStage::DraftIdentity => "active draft changed unexpectedly",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AcceptedSendCleanupPlan {
    pub(crate) clear_active_draft: bool,
    pub(crate) draft_identity_changed: bool,
    pub(crate) clear_recovery: bool,
    pub(crate) newer_composer_changes: bool,
}

impl AcceptedSendCleanupPlan {
    pub(crate) fn reset_composer(self, recovery_cleared: bool) -> bool {
        self.clear_recovery && recovery_cleared
    }
}

pub(crate) fn plan_accepted_send_cleanup(
    sent_generation: u64,
    current_generation: u64,
    sent_draft: Option<&ActiveDraft>,
    current_active_draft: Option<&ActiveDraft>,
    draft_deleted: bool,
) -> AcceptedSendCleanupPlan {
    let same_active_draft = current_active_draft == sent_draft;
    let clear_active_draft = sent_draft.is_some() && draft_deleted && same_active_draft;
    let draft_identity_changed = sent_draft.is_some() && draft_deleted && !same_active_draft;
    let composer_unchanged = current_generation == sent_generation;
    let source_cleanup_allows_reset = sent_draft.is_none() || draft_deleted;
    AcceptedSendCleanupPlan {
        clear_active_draft,
        draft_identity_changed,
        clear_recovery: composer_unchanged && source_cleanup_allows_reset,
        newer_composer_changes: !composer_unchanged,
    }
}

pub(crate) fn format_send_cleanup_issues(issues: &[SendCleanupIssue]) -> Option<String> {
    (!issues.is_empty()).then(|| {
        issues
            .iter()
            .map(|issue| format!("{}: {}", issue.description(), issue.error))
            .collect::<Vec<_>>()
            .join("; ")
    })
}

pub(crate) fn composed_message_from_fields(
    fields: &ComposeFields,
) -> anyhow::Result<ComposedMessage> {
    let from = parse_one_checked(&fields.from)
        .map(|address| format_address(&address))
        .map_err(|err| anyhow::anyhow!("invalid From address: {err}"))?;
    let to = checked_recipient_field("To", &fields.to)?;
    let cc = checked_recipient_field("Cc", &fields.cc)?;
    let bcc = checked_recipient_field("Bcc", &fields.bcc)?;
    anyhow::ensure!(
        !to.is_empty() || !cc.is_empty() || !bcc.is_empty(),
        "at least one To, Cc, or Bcc recipient is required"
    );
    let mut message = ComposedMessage::new(from, to, fields.subject.clone(), fields.body.clone());
    message.cc = cc;
    message.bcc = bcc;
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
    message.text_reply_quote = fields.text_reply_quote.clone();
    message.html_reply_quote = fields.html_reply_quote.clone();
    message.attachments = attachments::load_compose_attachments(fields)?;
    Ok(message)
}

fn checked_recipient_field(label: &str, value: &str) -> anyhow::Result<Vec<String>> {
    parse_address_list_checked(value)
        .map(|addresses| addresses.iter().map(format_address).collect::<Vec<_>>())
        .map_err(|err| anyhow::anyhow!("invalid {label} recipients: {err}"))
}

pub(crate) fn prepare_draft_fields_from_message(
    parsed: &ParsedMessage,
    attachment_inputs: Vec<AttachmentInput>,
) -> (ComposeFields, Vec<AttachmentInput>) {
    let body = if parsed.text_body.trim().is_empty() {
        parsed.safe_body.clone()
    } else {
        parsed.text_body.clone()
    };
    (
        ComposeFields {
            from: parsed.from.clone(),
            to: parsed.to.clone(),
            cc: parsed.cc.clone(),
            bcc: header_value(&parsed.headers, "Bcc"),
            subject: parsed.subject.clone(),
            body,
            attachments: Vec::new(),
            in_reply_to: nonempty_string(parsed.in_reply_to.clone()),
            references: references_from_header(&parsed.references),
            text_reply_quote: None,
            html_reply_quote: None,
        },
        attachment_inputs,
    )
}

#[cfg(test)]
fn draft_fields_from_message_file(path: impl AsRef<Path>) -> anyhow::Result<ComposeFields> {
    let path = path.as_ref();
    let parsed = parse_file(path)?;
    let attachment_inputs = attachments::attachment_inputs_from_file(path)?;
    let (mut fields, attachment_inputs) =
        prepare_draft_fields_from_message(&parsed, attachment_inputs);
    fields.attachments = attachments::cache_composer_attachments(
        &attachment_inputs,
        &default_attachment_cache_dir(),
        atomic_write_durable,
    )?;
    Ok(fields)
}

fn header_value(headers: &std::collections::BTreeMap<String, String>, name: &str) -> String {
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

fn persisted_draft_deletion_requires_confirmation(_deletion: PersistedDraftDeletion) -> bool {
    true
}

impl ComposerReplacementKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Mailto => "mailto",
            Self::Reply => "reply",
            Self::ReplyAll => "reply_all",
            Self::Forward => "forward",
            Self::ForwardAttachment => "forward_attachment",
            Self::StandaloneReply => "standalone_reply",
            Self::StandaloneReplyAll => "standalone_reply_all",
            Self::StandaloneForward => "standalone_forward",
            Self::StandaloneForwardAttachment => "standalone_forward_attachment",
            Self::NamedDraft => "named_draft",
            Self::RecoveryDraft => "recovery_draft",
            Self::IndexedDraft => "indexed_draft",
        }
    }
}

impl PendingAction {
    pub(crate) fn kind_name(&self) -> &'static str {
        match self {
            Self::ClearComposer(_) => "clear_composer",
            Self::ReplaceComposer { kind, .. } => kind.name(),
            Self::DeleteActiveDraft(_) => "delete_active_draft",
            Self::DeleteNamedDraft(_) => "delete_named_draft",
            Self::SaveDraftReplacement(_) => "save_draft_replacement",
            Self::SendComposer(_) => "send_composer",
            Self::ShowSelectedMessage(_) => "show_selected_message",
            Self::CloseMainWindow(_) => "close_main_window",
        }
    }

    pub(crate) fn operation(&self) -> PendingOperation {
        match self {
            Self::ClearComposer(_) => PendingOperation::DraftClear,
            Self::ReplaceComposer { kind, .. } => match kind {
                ComposerReplacementKind::NamedDraft
                | ComposerReplacementKind::RecoveryDraft
                | ComposerReplacementKind::IndexedDraft => PendingOperation::DraftLoad,
                _ => PendingOperation::ComposeReplace,
            },
            Self::DeleteActiveDraft(_) | Self::DeleteNamedDraft(_) => PendingOperation::DraftDelete,
            Self::SaveDraftReplacement(_) => PendingOperation::DraftSave,
            Self::SendComposer(_) => PendingOperation::Send,
            Self::ShowSelectedMessage(_) | Self::CloseMainWindow(_) => {
                PendingOperation::ComposeReplace
            }
        }
    }

    pub(crate) fn accept(self) -> bool {
        (self.into_hooks().accept)()
    }

    pub(crate) fn reject(self) {
        (self.into_hooks().reject)();
    }

    fn into_hooks(self) -> TransitionHooks {
        match self {
            Self::ClearComposer(hooks)
            | Self::DeleteActiveDraft(hooks)
            | Self::DeleteNamedDraft(hooks)
            | Self::SaveDraftReplacement(hooks)
            | Self::SendComposer(hooks)
            | Self::ShowSelectedMessage(hooks)
            | Self::CloseMainWindow(hooks)
            | Self::ReplaceComposer { hooks, .. } => hooks,
        }
    }

    pub(crate) fn always_requires_confirmation(&self) -> bool {
        let deletion = match self {
            Self::DeleteActiveDraft(_) => Some(PersistedDraftDeletion::ExplicitActive),
            Self::DeleteNamedDraft(_) => Some(PersistedDraftDeletion::ExplicitNamed),
            Self::SaveDraftReplacement(_) => Some(PersistedDraftDeletion::SaveReplacement),
            Self::SendComposer(_) => Some(PersistedDraftDeletion::AcceptedSendCleanup),
            _ => None,
        };
        deletion.is_some_and(persisted_draft_deletion_requires_confirmation)
    }

    pub(crate) fn prompt(&self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::DeleteActiveDraft(_) => (
                "Delete saved draft?",
                "This permanently deletes the active saved draft.",
                "Delete",
            ),
            Self::DeleteNamedDraft(_) => (
                "Delete selected draft?",
                "This permanently deletes the selected saved draft.",
                "Delete",
            ),
            Self::SaveDraftReplacement(_) => (
                "Replace saved draft?",
                "Saving these changes creates a replacement and permanently deletes the previous saved draft.",
                "Replace",
            ),
            Self::SendComposer(_) => (
                "Send and delete saved draft?",
                "If sending succeeds, this permanently deletes the active saved draft.",
                "Send",
            ),
            Self::ClearComposer(_) => (
                "Discard composer changes?",
                "The current composer has unsaved changes that will be discarded.",
                "Discard",
            ),
            Self::ReplaceComposer { .. } => (
                "Replace composer contents?",
                "The current composer has unsaved changes that will be replaced.",
                "Replace",
            ),
            Self::ShowSelectedMessage(_) => (
                "Close composer and show message?",
                "The current composer has unsaved changes. Showing the message detaches its active draft context; recovery data remains available.",
                "Show message",
            ),
            Self::CloseMainWindow(_) => (
                "Close notm with composer changes?",
                "The current composer has unsaved changes. Recovery data remains available on the next launch.",
                "Close",
            ),
        }
    }
}

pub(crate) fn fields_has_content(fields: &ComposeFields) -> bool {
    !fields.to.trim().is_empty()
        || !fields.cc.trim().is_empty()
        || !fields.bcc.trim().is_empty()
        || !fields.subject.trim().is_empty()
        || !fields.body.trim().is_empty()
        || !fields.attachments.is_empty()
}

pub(crate) fn composer_requires_confirmation(
    fields: &ComposeFields,
    active: Option<&ActiveDraft>,
) -> bool {
    match active {
        Some(active) => fields != &active.saved_fields,
        None => fields_has_content(fields),
    }
}

fn pending_action_requires_confirmation(
    action: &PendingAction,
    fields: &ComposeFields,
    active: Option<&ActiveDraft>,
) -> bool {
    action.always_requires_confirmation() || composer_requires_confirmation(fields, active)
}

fn xdg_home_path(
    configured_home: Option<&OsStr>,
    home: Option<&OsStr>,
    home_suffix: &str,
    fallback: &str,
) -> PathBuf {
    configured_home
        .filter(|path| !path.is_empty() && Path::new(path).is_absolute())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|path| !path.is_empty() && Path::new(path).is_absolute())
                .map(|path| PathBuf::from(path).join(home_suffix))
        })
        .unwrap_or_else(|| PathBuf::from(fallback))
}

pub(crate) fn default_state_home() -> PathBuf {
    let configured_home = std::env::var_os("XDG_STATE_HOME");
    let home = std::env::var_os("HOME");
    xdg_home_path(
        configured_home.as_deref(),
        home.as_deref(),
        ".local/state",
        ".local/state",
    )
}

fn legacy_default_cache_home() -> PathBuf {
    let configured_home = std::env::var_os("XDG_CACHE_HOME");
    let home = std::env::var_os("HOME");
    xdg_home_path(
        configured_home.as_deref(),
        home.as_deref(),
        ".cache",
        ".cache",
    )
}

fn compose_state_path(state_home: &Path, relative_path: &str) -> PathBuf {
    state_home.join("notm").join(relative_path)
}

pub(crate) fn default_recovery_path() -> PathBuf {
    compose_state_path(&default_state_home(), "draft.json")
}

pub(crate) fn legacy_default_recovery_path() -> PathBuf {
    legacy_default_cache_home().join("notm/draft.json")
}

pub(crate) fn default_drafts_dir() -> PathBuf {
    compose_state_path(&default_state_home(), "drafts")
}

pub(crate) fn legacy_default_drafts_dir() -> PathBuf {
    legacy_default_cache_home().join("notm/drafts")
}

pub(crate) fn default_attachment_cache_dir() -> PathBuf {
    compose_state_path(&default_state_home(), "compose-attachments")
}

pub(crate) fn persist_recovery_draft(
    path: &Path,
    legacy_path: Option<&Path>,
    fields: &ComposeFields,
) -> anyhow::Result<()> {
    if fields_has_content(fields) {
        let bytes = serde_json::to_vec_pretty(fields)?;
        anyhow::ensure!(
            bytes.len() <= MAX_RECOVERY_BYTES,
            "recovery draft serializes to {} bytes; limit is {MAX_RECOVERY_BYTES}",
            bytes.len()
        );
        atomic_write_durable(path, &bytes)?;
        if let Some(legacy_path) = legacy_path {
            remove_file_if_present(legacy_path)?;
        }
    } else {
        clear_recovery_draft_files(path, legacy_path)?;
    }
    Ok(())
}

pub(crate) fn atomic_write_durable(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    atomic_write_with_sync(path, bytes, true)
}

fn atomic_write_with_sync(path: &Path, bytes: &[u8], sync_to_disk: bool) -> anyhow::Result<()> {
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
        .unwrap_or("notm-state");
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
        if sync_to_disk {
            temporary.sync_all()?;
        }
        drop(temporary);
        std::fs::rename(&temporary_path, path)?;
        if sync_to_disk {
            sync_directory(parent)?;
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    write_result.map_err(|err| anyhow::anyhow!("writing {} atomically: {err}", path.display()))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> anyhow::Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

pub(crate) fn ensure_private_directory(path: &Path) -> anyhow::Result<()> {
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

pub(crate) fn save_named_draft_fields(
    dir: &Path,
    fields: &ComposeFields,
    replacement: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(fields_has_content(fields), "draft has no content");
    let bytes = serde_json::to_vec_pretty(fields)?;
    ensure_named_draft_save_fits(dir, replacement, bytes.len())?;
    ensure_private_directory(dir)?;
    let path = if let Some(replacement) = replacement {
        replacement.to_path_buf()
    } else {
        let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
        let slug = widget_token(&fields.subject);
        let slug = if slug.is_empty() {
            "untitled".to_string()
        } else {
            slug.chars().take(32).collect()
        };
        dir.join(format!("{stamp}-{slug}-{}.json", Uuid::new_v4()))
    };
    atomic_write_durable(&path, &bytes)?;
    Ok(path)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DraftSaveReport {
    pub(crate) local_path: Option<PathBuf>,
    pub(crate) maildir_path: Option<PathBuf>,
    pub(crate) indexed_message_id: Option<String>,
    pub(crate) replaced_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) recovery_cleanup_warning: Option<String>,
}

#[cfg(test)]
pub(crate) fn migrate_legacy_named_drafts(dir: &Path, legacy_dir: &Path) -> anyhow::Result<usize> {
    let entries = match std::fs::read_dir(legacy_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err.into()),
    };
    let mut migrated = 0;
    for entry in entries {
        let entry = entry?;
        let legacy_path = entry.path();
        if legacy_path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let bytes = std::fs::read(&legacy_path)?;
        ensure_private_directory(dir)?;
        let filename = entry.file_name();
        let mut destination = dir.join(&filename);
        if destination.exists() {
            if std::fs::read(&destination)? == bytes {
                remove_file_if_present(&legacy_path)?;
                continue;
            }
            destination = dir.join(format!(
                "legacy-{}-{}",
                Uuid::new_v4(),
                filename.to_string_lossy()
            ));
        }
        atomic_write_durable(&destination, &bytes)?;
        remove_file_if_present(&legacy_path)?;
        migrated += 1;
    }
    Ok(migrated)
}

#[cfg(test)]
pub(crate) fn list_named_drafts(
    dir: &Path,
    legacy_dir: Option<&Path>,
) -> Vec<(PathBuf, ComposeFields)> {
    let mut drafts: Vec<(Option<std::time::SystemTime>, PathBuf, ComposeFields)> = Vec::new();
    for dir in std::iter::once(dir).chain(legacy_dir) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(fields) = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<ComposeFields>(&bytes).ok())
            else {
                continue;
            };
            let duplicate = drafts.iter().any(|(_, existing_path, existing_fields)| {
                existing_path.file_name() == path.file_name() && existing_fields == &fields
            });
            if duplicate {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok();
            drafts.push((modified, path, fields));
        }
    }
    drafts.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    drafts
        .into_iter()
        .map(|(_, path, fields)| (path, fields))
        .collect()
}

#[cfg(test)]
pub(crate) fn migrate_legacy_recovery_draft(
    path: &Path,
    legacy_path: &Path,
) -> anyhow::Result<bool> {
    if path.exists() || !legacy_path.exists() {
        return Ok(false);
    }
    let bytes = std::fs::read(legacy_path)?;
    atomic_write_durable(path, &bytes)?;
    remove_file_if_present(legacy_path)?;
    Ok(true)
}

pub(crate) fn remove_file_if_present(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(anyhow::anyhow!("removing {}: {err}", path.display())),
    }
}

pub(crate) fn clear_recovery_draft_files(
    path: &Path,
    legacy_path: Option<&Path>,
) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    for path in std::iter::once(path).chain(legacy_path) {
        if let Err(err) = remove_file_if_present(path) {
            errors.push(format!("{}: {err}", path.display()));
        } else if let Some(parent) = path.parent().filter(|parent| parent.exists())
            && let Err(err) = sync_directory(parent)
        {
            errors.push(format!("syncing {}: {err}", parent.display()));
        }
    }
    if !errors.is_empty() {
        anyhow::bail!("could not remove recovery draft: {}", errors.join("; "));
    }
    Ok(())
}

pub(crate) fn active_draft_matches_path(active_draft: Option<&ActiveDraft>, path: &Path) -> bool {
    active_draft.is_some_and(|draft| draft.path == path)
}

pub(crate) fn clear_transient_autosave_error(last_error: &mut Option<String>) -> bool {
    if last_error
        .as_deref()
        .is_some_and(|error| error.starts_with("Draft autosave failed:"))
    {
        *last_error = None;
        true
    } else {
        false
    }
}

#[derive(Debug, Clone)]
pub(crate) struct NamedDraftEntry {
    pub(crate) modified: Option<std::time::SystemTime>,
    pub(crate) path: PathBuf,
    pub(crate) fields: ComposeFields,
}

#[derive(Clone)]
pub(crate) struct ComposerController {
    root: gtk::Box,
    from: gtk::Entry,
    to: gtk::Entry,
    cc: gtk::Entry,
    bcc: gtk::Entry,
    subject: gtk::Entry,
    body: SourceView,
    autosave_suppressed: Rc<Cell<bool>>,
    vim_context: VimIMContext,
    scrolled: gtk::ScrolledWindow,
    attachments: gtk::Label,
    add_attachment: gtk::Button,
    save_draft: gtk::Button,
    clear_draft: gtk::Button,
    delete_local_draft: gtk::Button,
    send: gtk::Button,
    address_suggestions: gtk::ListBox,
    active_address_entry: Rc<RefCell<Option<gtk::Entry>>>,
    active_address_field: Rc<Cell<Option<RecipientField>>>,
    address_completion: Rc<RefCell<Option<AddressCompletionSession>>>,
    draft_section: gtk::Box,
    draft_empty: gtk::Label,
    draft_scrolled: gtk::ScrolledWindow,
    draft_list: gtk::ListBox,
    named_drafts: Rc<RefCell<Vec<NamedDraftEntry>>>,
    delete_selected_draft: gtk::Button,
    paths: ComposerPaths,
    confirmation: Rc<RefCell<ConfirmationController>>,
}

impl ComposerController {
    pub(crate) fn new(paths: ComposerPaths) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.set_widget_name("notm-composer");
        root.set_hexpand(true);
        root.set_vexpand(true);
        let from = entry_with_placeholder("From");
        let to = entry_with_placeholder("To");
        let cc = entry_with_placeholder("Cc");
        let bcc = entry_with_placeholder("Bcc");
        let subject = entry_with_placeholder("Subject");
        let body_buffer = SourceBuffer::builder()
            .highlight_matching_brackets(true)
            .highlight_syntax(false)
            .build();
        let body = SourceView::builder()
            .buffer(&body_buffer)
            .highlight_current_line(false)
            .hexpand(true)
            .monospace(true)
            .vexpand(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .build();
        body.set_widget_name("notm-compose-body");
        let vim_context = attach_vim_context(&body);
        let scrolled = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_width(false)
            .propagate_natural_height(false)
            .min_content_width(240)
            .min_content_height(COMPOSE_BODY_MIN_HEIGHT)
            .max_content_height(COMPOSE_BODY_NATURAL_HEIGHT)
            .child(&body)
            .build();

        let address_suggestions = gtk::ListBox::new();
        address_suggestions.set_widget_name("notm-address-suggestions-list");
        address_suggestions.set_selection_mode(gtk::SelectionMode::Single);
        address_suggestions.add_css_class("boxed-list");
        address_suggestions.set_hexpand(true);
        address_suggestions.set_focusable(false);
        address_suggestions.set_visible(false);

        let attachments = gtk::Label::new(Some("No attachments"));
        attachments.set_widget_name("notm-compose-attachments");
        attachments.set_xalign(0.0);
        attachments.set_wrap(true);
        attachments.add_css_class("dim-label");

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        actions.set_hexpand(true);
        let left_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let add_attachment = gtk::Button::with_label("Add attachment…");
        let save_draft = gtk::Button::with_label("Save draft");
        save_draft.set_widget_name("notm-save-draft-button");
        let clear_draft = gtk::Button::with_label("Discard draft");
        let delete_local_draft = gtk::Button::with_label("Delete local draft");
        delete_local_draft.set_widget_name("notm-delete-local-draft-button");
        delete_local_draft.add_css_class("destructive-action");
        delete_local_draft.set_visible(false);
        let send = gtk::Button::with_label("Send");
        send.set_widget_name("notm-send-button");
        for button in [&add_attachment, &save_draft, &clear_draft, &send] {
            left_actions.append(button);
        }
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        actions.append(&left_actions);
        actions.append(&spacer);
        actions.append(&delete_local_draft);

        for widget in [
            from.clone().upcast::<gtk::Widget>(),
            to.clone().upcast(),
            address_suggestions.clone().upcast(),
            cc.clone().upcast(),
            bcc.clone().upcast(),
            subject.clone().upcast(),
            scrolled.clone().upcast(),
            attachments.clone().upcast(),
        ] {
            root.append(&widget);
        }

        let draft_section = gtk::Box::new(gtk::Orientation::Vertical, 4);
        draft_section.set_widget_name("notm-saved-drafts-section");
        draft_section.set_hexpand(true);
        let draft_header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        draft_header.set_hexpand(true);
        let draft_title = gtk::Label::new(Some("Saved drafts"));
        draft_title.set_widget_name("notm-saved-drafts-title");
        draft_title.set_xalign(0.0);
        draft_title.set_hexpand(true);
        draft_title.add_css_class("heading");
        let delete_selected_draft = gtk::Button::with_label("Delete selected draft");
        delete_selected_draft.set_widget_name("notm-delete-selected-draft-button");
        delete_selected_draft.add_css_class("destructive-action");
        delete_selected_draft.set_sensitive(false);
        draft_header.append(&draft_title);
        draft_header.append(&delete_selected_draft);
        draft_section.append(&draft_header);
        let draft_empty = gtk::Label::new(Some("No saved drafts"));
        draft_empty.set_widget_name("notm-saved-drafts-empty");
        draft_empty.set_xalign(0.0);
        draft_empty.add_css_class("dim-label");
        draft_empty.set_margin_start(6);
        draft_empty.set_margin_end(6);
        draft_empty.set_margin_top(6);
        draft_empty.set_margin_bottom(6);
        draft_section.append(&draft_empty);
        let draft_list = gtk::ListBox::new();
        draft_list.set_widget_name("notm-draft-list");
        draft_list.set_selection_mode(gtk::SelectionMode::Single);
        draft_list.set_activate_on_single_click(false);
        draft_list.add_css_class("boxed-list");
        let draft_scrolled = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(false)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .propagate_natural_height(false)
            .min_content_height(DRAFT_LIST_MIN_HEIGHT)
            .max_content_height(DRAFT_LIST_MAX_HEIGHT)
            .child(&draft_list)
            .build();
        draft_scrolled.set_widget_name("notm-saved-drafts-scrolled");
        draft_section.append(&draft_scrolled);
        root.append(&draft_section);
        root.append(&actions);

        Self {
            root,
            from,
            to,
            cc,
            bcc,
            subject,
            body,
            autosave_suppressed: Rc::new(Cell::new(false)),
            vim_context,
            scrolled,
            attachments,
            add_attachment,
            save_draft,
            clear_draft,
            delete_local_draft,
            send,
            address_suggestions,
            active_address_entry: Rc::new(RefCell::new(None)),
            active_address_field: Rc::new(Cell::new(None)),
            address_completion: Rc::new(RefCell::new(None)),
            draft_section,
            draft_empty,
            draft_scrolled,
            draft_list,
            named_drafts: Rc::new(RefCell::new(Vec::new())),
            delete_selected_draft,
            paths,
            confirmation: Rc::new(RefCell::new(ConfirmationController {
                next_id: 1,
                pending: None,
                last_completion: None,
                allow_close_once: false,
            })),
        }
    }

    pub(crate) fn root(&self) -> gtk::Box {
        self.root.clone()
    }

    pub(crate) fn sender_entry(&self) -> gtk::Entry {
        self.from.clone()
    }

    pub(crate) fn to_entry(&self) -> gtk::Entry {
        self.to.clone()
    }

    pub(crate) fn cc_entry(&self) -> gtk::Entry {
        self.cc.clone()
    }

    pub(crate) fn bcc_entry(&self) -> gtk::Entry {
        self.bcc.clone()
    }

    pub(crate) fn subject_entry(&self) -> gtk::Entry {
        self.subject.clone()
    }

    pub(crate) fn body(&self) -> SourceView {
        self.body.clone()
    }

    pub(crate) fn scrolled(&self) -> gtk::ScrolledWindow {
        self.scrolled.clone()
    }

    pub(crate) fn connect_vim(
        &self,
        status_handler: ComposerStatusHandler,
        write_handler: ComposerVimWriteHandler,
    ) {
        let body = self.body.downgrade();
        let status = status_handler.clone();
        self.vim_context
            .connect_command_bar_text_notify(move |context| {
                update_vim_status(body.upgrade().as_ref(), &status, context);
            });
        let body = self.body.downgrade();
        self.vim_context
            .connect_command_text_notify(move |context| {
                update_vim_status(body.upgrade().as_ref(), &status_handler, context);
            });
        self.vim_context.connect_write(move |_, _, path| {
            write_handler(path.map(ToString::to_string));
        });
    }

    pub(crate) fn vim_ready_for_app_escape(&self) -> bool {
        self.vim_context.command_bar_text().is_empty() && self.vim_context.command_text().is_empty()
    }

    pub(crate) fn add_attachment_button(&self) -> gtk::Button {
        self.add_attachment.clone()
    }

    pub(crate) fn attachments_label(&self) -> gtk::Label {
        self.attachments.clone()
    }

    pub(crate) fn save_draft_button(&self) -> gtk::Button {
        self.save_draft.clone()
    }

    pub(crate) fn clear_draft_button(&self) -> gtk::Button {
        self.clear_draft.clone()
    }

    pub(crate) fn delete_local_draft_button(&self) -> gtk::Button {
        self.delete_local_draft.clone()
    }

    pub(crate) fn send_button(&self) -> gtk::Button {
        self.send.clone()
    }

    pub(crate) fn address_suggestions(&self) -> gtk::ListBox {
        self.address_suggestions.clone()
    }

    pub(crate) fn draft_section(&self) -> gtk::Box {
        self.draft_section.clone()
    }

    pub(crate) fn draft_empty_label(&self) -> gtk::Label {
        self.draft_empty.clone()
    }

    pub(crate) fn draft_scrolled(&self) -> gtk::ScrolledWindow {
        self.draft_scrolled.clone()
    }

    pub(crate) fn draft_list(&self) -> gtk::ListBox {
        self.draft_list.clone()
    }

    pub(crate) fn delete_selected_draft_button(&self) -> gtk::Button {
        self.delete_selected_draft.clone()
    }

    pub(crate) fn has_pending_confirmation(&self) -> bool {
        self.confirmation.borrow().pending.is_some()
    }

    pub(crate) fn pending_confirmation_is_saved_send(&self) -> bool {
        self.confirmation
            .borrow()
            .pending
            .as_ref()
            .is_some_and(|pending| matches!(pending.action, PendingAction::SendComposer(_)))
    }

    pub(crate) fn pending_confirmation_is_close_main_window(&self) -> bool {
        self.confirmation
            .borrow()
            .pending
            .as_ref()
            .is_some_and(|pending| matches!(pending.action, PendingAction::CloseMainWindow(_)))
    }

    pub(crate) fn take_allow_close_once(&self) -> bool {
        let mut confirmation = self.confirmation.borrow_mut();
        let allowed = confirmation.allow_close_once;
        confirmation.allow_close_once = false;
        allowed
    }

    pub(crate) fn allow_close_once(&self) {
        self.confirmation.borrow_mut().allow_close_once = true;
    }

    pub(crate) fn request_confirmation(
        &self,
        fields: &ComposeFields,
        active: Option<&ActiveDraft>,
        action: PendingAction,
        presenter: ConfirmationPresenter,
        response_handler: ConfirmationResponseHandler,
    ) -> Result<ConfirmationDisposition, PendingAction> {
        if self.has_pending_confirmation() {
            return Err(action);
        }
        if !pending_action_requires_confirmation(&action, fields, active) {
            return Ok(ConfirmationDisposition::Immediate(action));
        }

        let (title, detail, confirm_label) = action.prompt();
        let id = {
            let mut confirmation = self.confirmation.borrow_mut();
            let id = confirmation.next_id;
            confirmation.next_id = confirmation.next_id.checked_add(1).unwrap_or(1);
            confirmation.last_completion = None;
            confirmation.pending = Some(PendingConfirmation {
                id,
                action,
                dialog: gtk::glib::WeakRef::new(),
            });
            id
        };
        let dialog = presenter(
            ConfirmationPrompt {
                id,
                title,
                detail,
                confirm_label,
            },
            response_handler,
        );
        if let Some(pending) = self.confirmation.borrow_mut().pending.as_mut()
            && pending.id == id
        {
            pending.dialog = dialog;
        }
        Ok(ConfirmationDisposition::Pending { title })
    }

    pub(crate) fn take_confirmation_action(&self, id: u64) -> Option<PendingAction> {
        let mut confirmation = self.confirmation.borrow_mut();
        if confirmation.pending.as_ref()?.id != id {
            return None;
        }
        Some(
            confirmation
                .pending
                .take()
                .expect("checked pending confirmation")
                .action,
        )
    }

    pub(crate) fn record_confirmation_completion(&self, id: u64, accepted: bool, succeeded: bool) {
        self.confirmation.borrow_mut().last_completion = Some(ConfirmationCompletion {
            id,
            accepted,
            succeeded,
        });
    }

    pub(crate) fn pending_confirmation_snapshot(&self) -> Option<PendingConfirmationSnapshot> {
        let confirmation = self.confirmation.borrow();
        confirmation.pending.as_ref().map(|pending| {
            let (title, _, confirm_label) = pending.action.prompt();
            PendingConfirmationSnapshot {
                id: pending.id,
                kind: pending.action.kind_name(),
                title,
                confirm_label,
                visible: pending
                    .dialog
                    .upgrade()
                    .is_some_and(|dialog| dialog.is_visible()),
            }
        })
    }

    pub(crate) fn last_confirmation_completion(&self) -> Option<ConfirmationCompletionSnapshot> {
        self.confirmation
            .borrow()
            .last_completion
            .as_ref()
            .map(|completion| ConfirmationCompletionSnapshot {
                id: completion.id,
                accepted: completion.accepted,
                succeeded: completion.succeeded,
            })
    }

    pub(crate) fn respond_confirmation(
        &self,
        requested_id: Option<u64>,
        response: gtk::ResponseType,
    ) -> anyhow::Result<u64> {
        let (id, dialog) = {
            let confirmation = self.confirmation.borrow();
            let pending = confirmation
                .pending
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no confirmation is pending"))?;
            let id = requested_id.unwrap_or(pending.id);
            anyhow::ensure!(id == pending.id, "confirmation id does not match");
            let dialog = pending
                .dialog
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("pending confirmation dialog is unavailable"))?;
            (id, dialog)
        };
        let response_id = response.into_glib();
        dialog.emit_by_name::<()>("response", &[&response_id]);
        Ok(id)
    }

    pub(crate) fn recovery_path(&self) -> &Path {
        &self.paths.recovery
    }

    pub(crate) fn legacy_recovery_path(&self) -> Option<&Path> {
        self.paths.legacy_recovery.as_deref()
    }

    pub(crate) fn drafts_dir(&self) -> &Path {
        &self.paths.drafts
    }

    pub(crate) fn legacy_drafts_dir(&self) -> Option<&Path> {
        self.paths.legacy_drafts.as_deref()
    }

    pub(crate) fn connect_recipient_autocomplete(
        &self,
        entry: &gtk::Entry,
        suggestions: AddressSuggestionsProvider,
        edited: ComposerEditedHandler,
    ) {
        let Some(field) = self.recipient_field_for_entry(entry) else {
            return;
        };
        let entry_weak = entry.downgrade();
        let active_entry = self.active_address_entry.clone();
        let active_field = self.active_address_field.clone();
        let completion = self.address_completion.clone();
        let list_weak = self.address_suggestions.downgrade();
        let suggestions_for_change = suggestions.clone();
        let edited_for_change = edited.clone();
        entry.connect_changed(move |entry| {
            let text = entry.text().to_string();
            {
                let mut completion = completion.borrow_mut();
                if let Some(session) = completion.as_mut()
                    && session.field == field
                    && session.suppress_next_change
                {
                    if session.generated_text.as_deref() == Some(text.as_str()) {
                        session.suppress_next_change = false;
                        edited_for_change();
                        return;
                    }
                    if text.is_empty() && session.generated_text.is_some() {
                        return;
                    }
                }
            }
            if completion.borrow().as_ref().is_some_and(|session| {
                session.field == field && address_session_matches_current(session, &text)
            }) {
                edited_for_change();
                return;
            }
            *completion.borrow_mut() = None;
            let Some(entry) = entry_weak.upgrade() else {
                return;
            };
            *active_entry.borrow_mut() = Some(entry.clone());
            if active_field.get() == Some(field) {
                if let Some(list) = list_weak.upgrade() {
                    update_address_suggestions(
                        AddressCompletionView {
                            list: &list,
                            active_entry: &active_entry,
                            completion: &completion,
                        },
                        field,
                        &entry,
                        &text,
                        &(suggestions_for_change)(),
                        6,
                    );
                }
            } else if let Some(list) = list_weak.upgrade() {
                hide_address_suggestions_list(&list);
            }
            edited_for_change();
        });

        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let entry_weak = entry.downgrade();
        let active_entry_for_key = self.active_address_entry.clone();
        let active_field_for_key = self.active_address_field.clone();
        let completion_for_key = self.address_completion.clone();
        let list_weak = self.address_suggestions.downgrade();
        let suggestions_for_key = suggestions;
        controller.connect_key_pressed(move |_, key, _, _| {
            let Some(entry) = entry_weak.upgrade() else {
                return gtk::glib::Propagation::Proceed;
            };
            *active_entry_for_key.borrow_mut() = Some(entry.clone());
            active_field_for_key.set(Some(field));
            if key == gtk::gdk::Key::Tab
                && list_weak.upgrade().is_some_and(|list| {
                    complete_recipient(
                        &list,
                        &active_entry_for_key,
                        &completion_for_key,
                        field,
                        &entry,
                        &(suggestions_for_key)(),
                    )
                })
            {
                return gtk::glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Escape {
                *completion_for_key.borrow_mut() = None;
                if let Some(list) = list_weak.upgrade() {
                    hide_address_suggestions_list(&list);
                }
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        entry.add_controller(controller);

        let focus = gtk::EventControllerFocus::new();
        let entry_weak = entry.downgrade();
        let active_entry = self.active_address_entry.clone();
        let active_field = self.active_address_field.clone();
        let list_weak = self.address_suggestions.downgrade();
        focus.connect_enter(move |_| {
            let (Some(entry), Some(list)) = (entry_weak.upgrade(), list_weak.upgrade()) else {
                return;
            };
            *active_entry.borrow_mut() = Some(entry.clone());
            active_field.set(Some(field));
            place_address_suggestions_after_entry(&list, &entry);
            hide_address_suggestions_list(&list);
        });
        let active_field = self.active_address_field.clone();
        let list_weak = self.address_suggestions.downgrade();
        focus.connect_leave(move |_| {
            let active_field = active_field.clone();
            let list_weak = list_weak.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(150), move || {
                if active_field.get() == Some(field) {
                    active_field.set(None);
                    if let Some(list) = list_weak.upgrade() {
                        hide_address_suggestions_list(&list);
                    }
                }
            });
        });
        entry.add_controller(focus);
    }

    pub(crate) fn connect_address_suggestion_list(&self) {
        let active_entry = self.active_address_entry.clone();
        let list_weak = self.address_suggestions.downgrade();
        self.address_suggestions
            .connect_row_activated(move |_, row| {
                let Some(child) = row.child() else {
                    return;
                };
                let Ok(label) = child.downcast::<gtk::Label>() else {
                    return;
                };
                let Some(entry) = active_entry.borrow().clone() else {
                    return;
                };
                apply_recipient_suggestion_to_entry(&entry, &label.text());
                if let Some(list) = list_weak.upgrade() {
                    hide_address_suggestions_list(&list);
                }
            });
    }

    pub(crate) fn activate_address_entry(&self, entry: &gtk::Entry) {
        let Some(field) = self.recipient_field_for_entry(entry) else {
            return;
        };
        *self.active_address_entry.borrow_mut() = Some(entry.clone());
        self.active_address_field.set(Some(field));
        place_address_suggestions_after_entry(&self.address_suggestions, entry);
    }

    pub(crate) fn update_address_suggestions_for_active(
        &self,
        input: &str,
        suggestions: &[String],
    ) {
        let entry = self
            .active_address_entry
            .borrow()
            .clone()
            .unwrap_or_else(|| self.to.clone());
        self.update_address_suggestions_for_entry(&entry, input, suggestions, 6);
    }

    pub(crate) fn update_address_suggestions_for_entry(
        &self,
        entry: &gtk::Entry,
        input: &str,
        suggestions: &[String],
        limit: usize,
    ) {
        let Some(field) = self.recipient_field_for_entry(entry) else {
            self.hide_address_suggestions();
            return;
        };
        update_address_suggestions(
            AddressCompletionView {
                list: &self.address_suggestions,
                active_entry: &self.active_address_entry,
                completion: &self.address_completion,
            },
            field,
            entry,
            input,
            suggestions,
            limit,
        );
    }

    pub(crate) fn hide_address_suggestions(&self) {
        hide_address_suggestions_list(&self.address_suggestions);
    }

    pub(crate) fn complete_focused_recipient(&self, suggestions: &[String]) -> bool {
        let Some(field) = self.active_address_field.get() else {
            return false;
        };
        let entry = match field {
            RecipientField::To => self.to.clone(),
            RecipientField::Cc => self.cc.clone(),
            RecipientField::Bcc => self.bcc.clone(),
        };
        complete_recipient(
            &self.address_suggestions,
            &self.active_address_entry,
            &self.address_completion,
            field,
            &entry,
            suggestions,
        )
    }

    pub(crate) fn apply_first_recipient_completion(
        &self,
        entry: &gtk::Entry,
        suggestions: &[String],
    ) -> bool {
        let current = entry.text().to_string();
        let Some(suggestion) = matching_address_suggestions(&current, suggestions, 1)
            .into_iter()
            .next()
        else {
            return false;
        };
        apply_recipient_suggestion_to_entry(entry, &suggestion);
        true
    }

    pub(crate) fn apply_recipient_suggestion(&self, entry: &gtk::Entry, suggestion: &str) {
        apply_recipient_suggestion_to_entry(entry, suggestion);
    }

    fn recipient_field_for_entry(&self, entry: &gtk::Entry) -> Option<RecipientField> {
        if entry == &self.to {
            Some(RecipientField::To)
        } else if entry == &self.cc {
            Some(RecipientField::Cc)
        } else if entry == &self.bcc {
            Some(RecipientField::Bcc)
        } else {
            None
        }
    }

    pub(crate) fn autosave_suppressed(&self) -> bool {
        self.autosave_suppressed.get()
    }

    pub(crate) fn read_fields(&self, stored: &ComposeFields) -> ComposeFields {
        ComposeFields {
            from: self.from.text().to_string(),
            to: self.to.text().to_string(),
            cc: self.cc.text().to_string(),
            bcc: self.bcc.text().to_string(),
            subject: self.subject.text().to_string(),
            body: self
                .body
                .buffer()
                .text(
                    &self.body.buffer().start_iter(),
                    &self.body.buffer().end_iter(),
                    true,
                )
                .to_string(),
            attachments: stored.attachments.clone(),
            in_reply_to: stored.in_reply_to.clone(),
            references: stored.references.clone(),
            text_reply_quote: stored.text_reply_quote.clone(),
            html_reply_quote: stored.html_reply_quote.clone(),
        }
    }

    pub(crate) fn capture_send(
        &self,
        stored: &ComposeFields,
        generation: u64,
        active_draft: Option<ActiveDraft>,
    ) -> SendSnapshot {
        SendSnapshot {
            fields: self.read_fields(stored),
            generation,
            active_draft,
        }
    }

    pub(crate) fn apply_fields(&self, fields: &ComposeFields) {
        self.autosave_suppressed.set(true);
        self.from.set_text(&fields.from);
        self.to.set_text(&fields.to);
        self.cc.set_text(&fields.cc);
        self.bcc.set_text(&fields.bcc);
        self.subject.set_text(&fields.subject);
        self.body.buffer().set_text(&fields.body);
        self.autosave_suppressed.set(false);
        self.move_cursor_to_start();
    }

    pub(crate) fn apply_message_fields(&self, message: &ComposedMessage) {
        self.autosave_suppressed.set(true);
        self.from.set_text(&message.from);
        self.to.set_text(&message.to.join(", "));
        self.cc.set_text(&message.cc.join(", "));
        self.bcc.set_text(&message.bcc.join(", "));
        self.subject.set_text(&message.subject);
        self.body.buffer().set_text(&message.body);
        self.autosave_suppressed.set(false);
        self.move_cursor_to_start();
    }

    pub(crate) fn reset_fields(&self) -> ComposeFields {
        let fields = ComposeFields {
            from: self.from.text().to_string(),
            ..ComposeFields::default()
        };
        self.autosave_suppressed.set(true);
        self.from.set_text(&fields.from);
        self.to.set_text("");
        self.cc.set_text("");
        self.bcc.set_text("");
        self.subject.set_text("");
        self.body.buffer().set_text("");
        self.autosave_suppressed.set(false);
        self.hide_address_suggestions();
        fields
    }

    pub(crate) fn move_cursor_to_start(&self) {
        let buffer = self.body.buffer();
        let start = buffer.start_iter();
        buffer.place_cursor(&start);
        let body = self.body.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::ZERO, move || {
            let mut start = body.buffer().start_iter();
            body.scroll_to_iter(&mut start, 0.0, true, 0.0, 0.0);
        });
    }

    pub(crate) fn has_focus(&self) -> bool {
        self.focus_targets().iter().any(widget_contains_focus)
    }

    pub(crate) fn focus_first_field(&self) {
        focus_widget_at(&self.focus_targets(), 0);
    }

    pub(crate) fn focus_insert_target(&self) {
        if !self.has_focus() {
            self.focus_first_field();
        }
    }

    pub(crate) fn focus_targets(&self) -> Vec<gtk::Widget> {
        [
            self.from.clone().upcast::<gtk::Widget>(),
            self.to.clone().upcast(),
            self.cc.clone().upcast(),
            self.bcc.clone().upcast(),
            self.subject.clone().upcast(),
            self.body.clone().upcast(),
        ]
        .into_iter()
        .filter(|widget| widget.is_visible() && widget.is_sensitive())
        .collect()
    }

    pub(crate) fn move_focus(&self, delta: isize) {
        move_focus_in_targets(&self.focus_targets(), delta);
    }

    pub(crate) fn replace_named_drafts(&self, drafts: Vec<NamedDraftEntry>) {
        *self.named_drafts.borrow_mut() = drafts;
        self.refresh_draft_list();
    }

    pub(crate) fn named_drafts(&self) -> Vec<NamedDraftEntry> {
        self.named_drafts.borrow().clone()
    }

    pub(crate) fn upsert_named_draft(&self, path: PathBuf, fields: ComposeFields) {
        let mut drafts = self.named_drafts.borrow_mut();
        drafts.retain(|draft| draft.path != path);
        drafts.insert(
            0,
            NamedDraftEntry {
                modified: Some(std::time::SystemTime::now()),
                path,
                fields,
            },
        );
        drop(drafts);
        self.refresh_draft_list();
    }

    pub(crate) fn remove_named_draft(&self, path: &Path) {
        self.named_drafts
            .borrow_mut()
            .retain(|draft| draft.path != path);
        self.refresh_draft_list();
    }

    pub(crate) fn refresh_draft_list(&self) {
        while let Some(child) = self.draft_list.first_child() {
            self.draft_list.remove(&child);
        }
        let drafts = self.named_drafts.borrow();
        let is_empty = drafts.is_empty();
        for (index, draft) in drafts.iter().enumerate() {
            let path = &draft.path;
            let fields = &draft.fields;
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
            self.draft_list.append(&row);
        }
        self.draft_empty.set_visible(is_empty);
        self.draft_scrolled.set_visible(!is_empty);
        self.delete_selected_draft.set_sensitive(false);
    }

    pub(crate) fn selected_named_draft(&self) -> anyhow::Result<(PathBuf, ComposeFields)> {
        let index = self
            .draft_list
            .selected_row()
            .map(|row| row.index() as usize)
            .unwrap_or(0);
        self.named_drafts
            .borrow()
            .get(index)
            .cloned()
            .map(|draft| (draft.path, draft.fields))
            .ok_or_else(|| anyhow::anyhow!("no selected draft"))
    }
}

fn attach_vim_context(body: &SourceView) -> VimIMContext {
    let vim_context = VimIMContext::new();
    let key_controller = gtk::EventControllerKey::new();
    key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    key_controller.set_im_context(Some(&vim_context));
    body.add_controller(key_controller);
    vim_context.set_client_widget(Some(body));
    vim_context
}

fn entry_with_placeholder(placeholder: &str) -> gtk::Entry {
    let entry = gtk::Entry::new();
    entry.set_placeholder_text(Some(placeholder));
    entry.set_widget_name(&format!("notm-compose-{}", placeholder.to_lowercase()));
    entry
}

fn update_vim_status(
    body: Option<&SourceView>,
    status_handler: &ComposerStatusHandler,
    vim_context: &VimIMContext,
) {
    if !body.is_some_and(SourceView::has_focus) {
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
    status_handler(text);
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

pub(crate) fn matching_address_suggestions(
    input: &str,
    suggestions: &[String],
    limit: usize,
) -> Vec<String> {
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

struct AddressCompletionView<'a> {
    list: &'a gtk::ListBox,
    active_entry: &'a RefCell<Option<gtk::Entry>>,
    completion: &'a RefCell<Option<AddressCompletionSession>>,
}

fn update_address_suggestions(
    view: AddressCompletionView<'_>,
    field: RecipientField,
    entry: &gtk::Entry,
    input: &str,
    suggestions: &[String],
    limit: usize,
) {
    let matches = matching_address_suggestions(input, suggestions, limit);
    if matches.is_empty() {
        hide_address_suggestions_list(view.list);
        return;
    }
    *view.active_entry.borrow_mut() = Some(entry.clone());
    *view.completion.borrow_mut() = Some(AddressCompletionSession {
        field,
        base: input.to_string(),
        suggestions: matches.clone(),
        next_index: 0,
        generated_text: None,
        suppress_next_change: false,
    });
    place_address_suggestions_after_entry(view.list, entry);
    populate_address_suggestions_list(view.list, &matches);
    view.list.set_visible(true);
}

fn hide_address_suggestions_list(list: &gtk::ListBox) {
    populate_address_suggestions_list(list, &[]);
    list.set_visible(false);
}

fn place_address_suggestions_after_entry(list: &gtk::ListBox, entry: &gtk::Entry) {
    let Some(parent) = entry.parent() else {
        return;
    };
    let Ok(parent_box) = parent.downcast::<gtk::Box>() else {
        return;
    };
    if let Some(current_parent) = list.parent()
        && let Ok(current_box) = current_parent.downcast::<gtk::Box>()
    {
        current_box.remove(list);
    }
    parent_box.insert_child_after(list, Some(entry));
}

fn complete_recipient(
    list: &gtk::ListBox,
    active_entry: &RefCell<Option<gtk::Entry>>,
    completion: &RefCell<Option<AddressCompletionSession>>,
    field: RecipientField,
    entry: &gtk::Entry,
    suggestions: &[String],
) -> bool {
    *active_entry.borrow_mut() = Some(entry.clone());
    place_address_suggestions_after_entry(list, entry);
    let current = entry.text().to_string();
    let reuse_session = completion.borrow().as_ref().is_some_and(|session| {
        session.field == field && address_session_matches_current(session, &current)
    });
    if !reuse_session {
        let matches = matching_address_suggestions(&current, suggestions, 20);
        if matches.is_empty() {
            hide_address_suggestions_list(list);
            return false;
        }
        *completion.borrow_mut() = Some(AddressCompletionSession {
            field,
            base: current.clone(),
            suggestions: matches,
            next_index: 0,
            generated_text: None,
            suppress_next_change: false,
        });
    }

    let (next, index, suggestions) = {
        let mut completion = completion.borrow_mut();
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

    // Drop the RefCell borrow before set_text synchronously emits `changed`.
    entry.set_text(&next);
    entry.set_position(-1);
    populate_address_suggestions_list(list, &suggestions);
    if let Some(row) = list.row_at_index(index as i32) {
        list.select_row(Some(&row));
    }
    list.set_visible(true);
    true
}

fn apply_recipient_suggestion_to_entry(entry: &gtk::Entry, suggestion: &str) {
    let current = entry.text().to_string();
    let next = recipient_suggestion_text(&current, suggestion);
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

fn populate_address_suggestions_list(list: &gtk::ListBox, suggestions: &[String]) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
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
        list.append(&row);
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

fn move_focus_in_targets(targets: &[gtk::Widget], delta: isize) {
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
    for target in targets {
        target.remove_css_class(KEYBOARD_CURSOR_CLASS);
    }
    targets[index].add_css_class(KEYBOARD_CURSOR_CLASS);
    targets[index].grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft_io::{MAX_NAMED_DRAFT_BYTES, MAX_NAMED_DRAFT_TOTAL_BYTES, MAX_NAMED_DRAFTS};

    fn fields_with_serialized_len(target_len: usize) -> ComposeFields {
        let empty_len = serde_json::to_vec_pretty(&ComposeFields::default())
            .expect("serialize empty draft")
            .len();
        assert!(target_len > empty_len);
        let fields = ComposeFields {
            body: "x".repeat(target_len - empty_len),
            ..ComposeFields::default()
        };
        assert_eq!(
            serde_json::to_vec_pretty(&fields)
                .expect("serialize sized draft")
                .len(),
            target_len
        );
        fields
    }

    fn assert_named_draft_entries(dir: &Path, expected: usize) {
        let entries = std::fs::read_dir(dir)
            .expect("list named-draft directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read named-draft entries");
        assert_eq!(entries.len(), expected);
        assert!(entries.iter().all(|entry| {
            entry.path().extension().and_then(OsStr::to_str) == Some("json")
                && !entry.file_name().to_string_lossy().ends_with(".tmp")
        }));
    }

    fn hooks() -> TransitionHooks {
        TransitionHooks::new(|| true, || {})
    }

    fn replacement(kind: ComposerReplacementKind) -> PendingAction {
        PendingAction::ReplaceComposer {
            kind,
            hooks: hooks(),
        }
    }

    #[test]
    fn confirmation_policy_matches_transient_and_saved_composer_state() {
        let blank = ComposeFields {
            from: "Me <me@example.test>".to_string(),
            ..ComposeFields::default()
        };
        assert!(!fields_has_content(&blank));
        assert!(!pending_action_requires_confirmation(
            &replacement(ComposerReplacementKind::New),
            &blank,
            None
        ));
        for fields in [
            ComposeFields {
                to: "you@example.test".to_string(),
                ..blank.clone()
            },
            ComposeFields {
                cc: "you@example.test".to_string(),
                ..blank.clone()
            },
            ComposeFields {
                bcc: "you@example.test".to_string(),
                ..blank.clone()
            },
            ComposeFields {
                subject: "subject".to_string(),
                ..blank.clone()
            },
            ComposeFields {
                body: "body".to_string(),
                ..blank.clone()
            },
            ComposeFields {
                attachments: vec!["/tmp/attachment".to_string()],
                ..blank.clone()
            },
        ] {
            assert!(fields_has_content(&fields));
            assert!(pending_action_requires_confirmation(
                &replacement(ComposerReplacementKind::Reply),
                &fields,
                None
            ));
        }

        let active = ActiveDraft {
            path: PathBuf::from("/tmp/saved-draft.json"),
            message_id: None,
            indexed: false,
            saved_fields: blank.clone(),
        };
        for action in [
            PendingAction::ClearComposer(hooks()),
            replacement(ComposerReplacementKind::NamedDraft),
            PendingAction::ShowSelectedMessage(hooks()),
            PendingAction::CloseMainWindow(hooks()),
        ] {
            assert!(!pending_action_requires_confirmation(
                &action,
                &blank,
                Some(&active)
            ));
        }
        let changed = ComposeFields {
            body: "changed".to_string(),
            ..blank
        };
        for action in [
            PendingAction::ClearComposer(hooks()),
            replacement(ComposerReplacementKind::ReplyAll),
            PendingAction::ShowSelectedMessage(hooks()),
            PendingAction::CloseMainWindow(hooks()),
        ] {
            assert!(pending_action_requires_confirmation(
                &action,
                &changed,
                Some(&active)
            ));
        }
    }

    #[test]
    fn permanent_draft_actions_always_confirm_and_keep_distinct_types() {
        let fields = ComposeFields::default();
        let active = ActiveDraft {
            path: PathBuf::from("/tmp/saved-draft.json"),
            message_id: None,
            indexed: false,
            saved_fields: fields.clone(),
        };
        let actions = [
            PendingAction::DeleteActiveDraft(hooks()),
            PendingAction::DeleteNamedDraft(hooks()),
            PendingAction::SaveDraftReplacement(hooks()),
            PendingAction::SendComposer(hooks()),
        ];
        assert_eq!(
            actions
                .iter()
                .map(PendingAction::kind_name)
                .collect::<Vec<_>>(),
            [
                "delete_active_draft",
                "delete_named_draft",
                "save_draft_replacement",
                "send_composer",
            ]
        );
        for action in &actions {
            assert!(action.always_requires_confirmation());
            assert!(pending_action_requires_confirmation(
                action,
                &fields,
                Some(&active)
            ));
        }
        for deletion in [
            PersistedDraftDeletion::ExplicitActive,
            PersistedDraftDeletion::ExplicitNamed,
            PersistedDraftDeletion::SaveReplacement,
            PersistedDraftDeletion::AcceptedSendCleanup,
        ] {
            assert!(persisted_draft_deletion_requires_confirmation(deletion));
        }
    }

    #[test]
    fn every_compose_replacement_kind_has_a_stable_distinct_harness_name() {
        let kinds = [
            ComposerReplacementKind::New,
            ComposerReplacementKind::Mailto,
            ComposerReplacementKind::Reply,
            ComposerReplacementKind::ReplyAll,
            ComposerReplacementKind::Forward,
            ComposerReplacementKind::ForwardAttachment,
            ComposerReplacementKind::StandaloneReply,
            ComposerReplacementKind::StandaloneReplyAll,
            ComposerReplacementKind::StandaloneForward,
            ComposerReplacementKind::StandaloneForwardAttachment,
            ComposerReplacementKind::NamedDraft,
            ComposerReplacementKind::RecoveryDraft,
            ComposerReplacementKind::IndexedDraft,
        ];
        let names = kinds.map(ComposerReplacementKind::name);
        assert_eq!(
            names
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            kinds.len()
        );
    }

    #[test]
    fn named_draft_delete_only_matches_the_same_active_path() {
        let active = ActiveDraft {
            path: PathBuf::from("/tmp/notm-active-draft.json"),
            message_id: None,
            indexed: false,
            saved_fields: ComposeFields::default(),
        };
        assert!(active_draft_matches_path(
            Some(&active),
            Path::new("/tmp/notm-active-draft.json")
        ));
        assert!(!active_draft_matches_path(
            Some(&active),
            Path::new("/tmp/notm-other-draft.json")
        ));
        assert!(!active_draft_matches_path(
            None,
            Path::new("/tmp/notm-active-draft.json")
        ));
    }

    #[test]
    fn address_completion_matches_and_replaces_only_the_active_recipient() {
        let suggestions = vec![
            "Alice <alice@example.test>".to_string(),
            "Bob <bob@example.test>".to_string(),
        ];
        assert_eq!(
            matching_address_suggestions("first@example.test, ali", &suggestions, 20),
            vec!["Alice <alice@example.test>"]
        );
        assert_eq!(
            recipient_suggestion_text("first@example.test, ali", &suggestions[0]),
            "first@example.test, Alice <alice@example.test>"
        );
        assert!(matching_address_suggestions("", &suggestions, 20).is_empty());
    }

    #[test]
    fn composed_message_preserves_reply_thread_headers() {
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
        assert_eq!(message.references, fields.references);
        assert!(rendered.contains("In-Reply-To: <original@example.test>\r\n"));
        assert!(rendered.contains("References: <older@example.test> <original@example.test>\r\n"));
    }

    #[test]
    fn composed_message_validates_and_canonically_formats_mailboxes() {
        let fields = ComposeFields {
            from: r#""Doe, Alice" <alice@example.test>"#.to_string(),
            to: r#""Smith, Bob" <bob@example.test>, carol@example.test"#.to_string(),
            subject: "Address formatting".to_string(),
            ..ComposeFields::default()
        };

        let message = composed_message_from_fields(&fields).expect("valid mailbox fields");

        assert_eq!(message.from, r#""Doe, Alice" <alice@example.test>"#);
        assert_eq!(
            message.to,
            [
                r#""Smith, Bob" <bob@example.test>"#.to_string(),
                "carol@example.test".to_string(),
            ]
        );
    }

    #[test]
    fn composed_message_rejects_invalid_or_missing_mailboxes() {
        let base = ComposeFields {
            from: "sender@example.test".to_string(),
            to: "recipient@example.test".to_string(),
            ..ComposeFields::default()
        };
        for (fields, expected) in [
            (
                ComposeFields {
                    from: "not-an-address".to_string(),
                    ..base.clone()
                },
                "invalid From address",
            ),
            (
                ComposeFields {
                    to: "Valid <valid@example.test>, Bad <bad@>".to_string(),
                    ..base.clone()
                },
                "invalid To recipients",
            ),
            (
                ComposeFields {
                    to: String::new(),
                    ..base
                },
                "at least one To, Cc, or Bcc recipient",
            ),
        ] {
            let error = composed_message_from_fields(&fields)
                .expect_err("malformed composer addresses must not be sent");
            assert!(
                error.to_string().contains(expected),
                "unexpected error for {fields:?}: {error:#}"
            );
        }
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
    fn compose_persistence_paths_use_the_xdg_state_layout() {
        let state_home = xdg_home_path(
            Some(OsStr::new("/tmp/notm-xdg-state")),
            Some(OsStr::new("/tmp/notm-home")),
            ".local/state",
            ".local/state",
        );
        assert_eq!(state_home, PathBuf::from("/tmp/notm-xdg-state"));
        for (name, expected) in [
            ("draft.json", "/tmp/notm-xdg-state/notm/draft.json"),
            ("drafts", "/tmp/notm-xdg-state/notm/drafts"),
            (
                "compose-attachments",
                "/tmp/notm-xdg-state/notm/compose-attachments",
            ),
        ] {
            assert_eq!(
                compose_state_path(&state_home, name),
                PathBuf::from(expected)
            );
        }
        assert_eq!(
            xdg_home_path(
                Some(OsStr::new("")),
                Some(OsStr::new("/tmp/notm-home")),
                ".local/state",
                ".local/state",
            ),
            PathBuf::from("/tmp/notm-home/.local/state")
        );
        assert_eq!(
            xdg_home_path(
                Some(OsStr::new("relative-state")),
                Some(OsStr::new("/tmp/notm-home")),
                ".local/state",
                ".local/state",
            ),
            PathBuf::from("/tmp/notm-home/.local/state")
        );
        assert_eq!(
            xdg_home_path(None, None, ".local/state", ".local/state"),
            PathBuf::from(".local/state")
        );
    }

    #[test]
    fn atomic_state_write_replaces_complete_file_without_temporary_artifacts() {
        let directory = tempfile::tempdir().expect("temporary state directory");
        let state_directory = directory.path().join("notm");
        std::fs::create_dir(&state_directory).expect("create state directory");
        let path = state_directory.join("draft.json");
        std::fs::write(&path, b"old draft").expect("seed initial draft");
        #[cfg(unix)]
        {
            std::fs::set_permissions(&state_directory, std::fs::Permissions::from_mode(0o755))
                .expect("make state directory non-private");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                .expect("make initial draft non-private");
        }
        atomic_write_durable(&path, b"complete replacement").expect("replace durable draft");
        assert_eq!(
            std::fs::read(&path).expect("read replaced draft"),
            b"complete replacement"
        );
        let entries = std::fs::read_dir(&state_directory)
            .expect("list state directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read state entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path(), path);
        #[cfg(unix)]
        {
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
                    .expect("draft metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn oversized_recovery_persist_preserves_last_good_file_without_temporary_artifacts() {
        let directory = tempfile::tempdir().expect("temporary recovery directory");
        let state_directory = directory.path().join("notm");
        let path = state_directory.join("draft.json");
        let valid = ComposeFields {
            subject: "Last good recovery".to_string(),
            body: "recoverable body".to_string(),
            ..ComposeFields::default()
        };
        persist_recovery_draft(&path, None, &valid).expect("seed valid recovery draft");
        let valid_bytes = std::fs::read(&path).expect("read valid recovery draft");

        let oversized = ComposeFields {
            body: "x".repeat(MAX_RECOVERY_BYTES),
            ..ComposeFields::default()
        };
        let error = persist_recovery_draft(&path, None, &oversized)
            .expect_err("oversized recovery draft must be rejected");

        let message = error.to_string();
        assert!(
            message.contains("recovery draft serializes to")
                && message.contains(&format!("limit is {MAX_RECOVERY_BYTES}")),
            "{message}"
        );
        assert_eq!(
            std::fs::read(&path).expect("read recovery draft after rejected persist"),
            valid_bytes
        );
        let entries = std::fs::read_dir(&state_directory)
            .expect("list recovery directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read recovery entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path(), path);
    }

    #[test]
    fn oversized_named_draft_save_preserves_existing_file_without_temporary_artifacts() {
        let directory = tempfile::tempdir().expect("temporary named-draft directory");
        let drafts_directory = directory.path().join("notm/drafts");
        let valid = ComposeFields {
            subject: "Last good named draft".to_string(),
            body: "saved body".to_string(),
            ..ComposeFields::default()
        };
        let valid_path = save_named_draft_fields(&drafts_directory, &valid, None)
            .expect("seed valid named draft");
        let valid_bytes = std::fs::read(&valid_path).expect("read valid named draft");

        let oversized = ComposeFields {
            body: "x".repeat(MAX_NAMED_DRAFT_BYTES),
            ..ComposeFields::default()
        };
        let error = save_named_draft_fields(&drafts_directory, &oversized, None)
            .expect_err("oversized named draft must be rejected");

        let message = error.to_string();
        assert!(
            message.contains("named draft serializes to")
                && message.contains(&format!("limit is {MAX_NAMED_DRAFT_BYTES}")),
            "{message}"
        );
        assert_eq!(
            std::fs::read(&valid_path).expect("read named draft after rejected save"),
            valid_bytes
        );
        let entries = std::fs::read_dir(&drafts_directory)
            .expect("list named-draft directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read named-draft entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path(), valid_path);
    }

    #[test]
    fn named_draft_save_at_count_limit_requires_in_place_replacement() {
        let directory = tempfile::tempdir().expect("temporary named-draft directory");
        let drafts_directory = directory.path().join("notm/drafts");
        std::fs::create_dir_all(&drafts_directory).expect("create named-draft directory");
        let seed = serde_json::to_vec_pretty(&ComposeFields {
            body: "saved body".to_string(),
            ..ComposeFields::default()
        })
        .expect("serialize seed draft");
        for index in 0..MAX_NAMED_DRAFTS {
            std::fs::write(
                drafts_directory.join(format!("draft-{index:03}.json")),
                &seed,
            )
            .expect("write seed draft");
        }
        let replacement = drafts_directory.join("draft-000.json");
        let prior_bytes = std::fs::read(&replacement).expect("read replacement draft");

        let error = save_named_draft_fields(
            &drafts_directory,
            &ComposeFields {
                body: "new draft".to_string(),
                ..ComposeFields::default()
            },
            None,
        )
        .expect_err("new draft at the file-count limit must be rejected");
        assert!(
            error.to_string().contains(&format!(
                "would contain {} JSON files; limit is {MAX_NAMED_DRAFTS}",
                MAX_NAMED_DRAFTS + 1
            )),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(&replacement).expect("read draft after rejected save"),
            prior_bytes
        );
        assert_named_draft_entries(&drafts_directory, MAX_NAMED_DRAFTS);

        let updated = ComposeFields {
            subject: "Updated in place".to_string(),
            body: "replacement body".to_string(),
            ..ComposeFields::default()
        };
        let returned =
            save_named_draft_fields(&drafts_directory, &updated, Some(replacement.as_path()))
                .expect("replace named draft at the file-count limit");
        assert_eq!(returned, replacement);
        assert_eq!(
            serde_json::from_slice::<ComposeFields>(
                &std::fs::read(&returned).expect("read replaced draft")
            )
            .expect("parse replaced draft"),
            updated
        );
        assert_named_draft_entries(&drafts_directory, MAX_NAMED_DRAFTS);
    }

    #[test]
    fn named_draft_save_rejects_replacement_outside_the_draft_directory() {
        let directory = tempfile::tempdir().expect("temporary named-draft directory");
        let drafts_directory = directory.path().join("notm/drafts");
        std::fs::create_dir_all(&drafts_directory).expect("create named-draft directory");
        let outside = directory.path().join("outside.json");
        std::fs::write(&outside, b"last good outside data").expect("write outside file");

        let error = save_named_draft_fields(
            &drafts_directory,
            &ComposeFields {
                body: "replacement body".to_string(),
                ..ComposeFields::default()
            },
            Some(&outside),
        )
        .expect_err("out-of-directory replacement must be rejected");

        assert!(
            error
                .to_string()
                .contains("is not an existing JSON file directly in"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(&outside).expect("read outside file after rejected replacement"),
            b"last good outside data"
        );
        assert_named_draft_entries(&drafts_directory, 0);
    }

    #[test]
    fn named_draft_save_near_total_limit_preserves_the_existing_store() {
        let directory = tempfile::tempdir().expect("temporary named-draft directory");
        let drafts_directory = directory.path().join("notm/drafts");
        std::fs::create_dir_all(&drafts_directory).expect("create named-draft directory");

        let full = serde_json::to_vec_pretty(&fields_with_serialized_len(MAX_NAMED_DRAFT_BYTES))
            .expect("serialize full-sized draft");
        let first_full = drafts_directory.join("full-00.json");
        std::fs::write(&first_full, &full).expect("write full-sized draft");
        for index in 1..15 {
            std::fs::hard_link(
                &first_full,
                drafts_directory.join(format!("full-{index:02}.json")),
            )
            .expect("hard-link full-sized draft");
        }
        let empty_len = serde_json::to_vec_pretty(&ComposeFields::default())
            .expect("serialize empty draft")
            .len();
        let replacement_len = empty_len + 64;
        let replacement = drafts_directory.join("replacement.json");
        let replacement_bytes =
            serde_json::to_vec_pretty(&fields_with_serialized_len(replacement_len))
                .expect("serialize replacement draft");
        std::fs::write(&replacement, &replacement_bytes).expect("write replacement draft");
        let filler_len = MAX_NAMED_DRAFT_TOTAL_BYTES
            .checked_sub(15 * MAX_NAMED_DRAFT_BYTES + replacement_len)
            .expect("calculate filler size");
        let filler = serde_json::to_vec_pretty(&fields_with_serialized_len(filler_len))
            .expect("serialize filler draft");
        std::fs::write(drafts_directory.join("filler.json"), filler).expect("write filler draft");
        assert_named_draft_entries(&drafts_directory, 17);

        let error = save_named_draft_fields(
            &drafts_directory,
            &ComposeFields {
                body: "another draft".to_string(),
                ..ComposeFields::default()
            },
            None,
        )
        .expect_err("new draft at the aggregate-byte limit must be rejected");
        assert!(
            error
                .to_string()
                .contains(&format!("limit is {MAX_NAMED_DRAFT_TOTAL_BYTES}")),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(&replacement).expect("read draft after rejected new save"),
            replacement_bytes
        );
        assert_named_draft_entries(&drafts_directory, 17);

        let larger_replacement = fields_with_serialized_len(replacement_len + 1);
        let error = save_named_draft_fields(
            &drafts_directory,
            &larger_replacement,
            Some(replacement.as_path()),
        )
        .expect_err("growing replacement beyond aggregate-byte limit must be rejected");
        assert!(
            error.to_string().contains(&format!(
                "would use {} bytes; limit is {MAX_NAMED_DRAFT_TOTAL_BYTES}",
                MAX_NAMED_DRAFT_TOTAL_BYTES + 1
            )),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(&replacement).expect("read draft after rejected replacement"),
            replacement_bytes
        );
        assert_named_draft_entries(&drafts_directory, 17);
    }

    #[cfg(unix)]
    #[test]
    fn named_drafts_are_created_in_a_private_directory() {
        let directory = tempfile::tempdir().expect("temporary state directory");
        let drafts_directory = directory.path().join("notm/drafts");
        let fields = ComposeFields {
            subject: "Private draft".to_string(),
            ..ComposeFields::default()
        };

        let path =
            save_named_draft_fields(&drafts_directory, &fields, None).expect("save named draft");

        assert_eq!(
            std::fs::metadata(&drafts_directory)
                .expect("draft directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(path)
                .expect("named draft metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn legacy_cache_recovery_is_migrated_without_changing_its_contents() {
        let directory = tempfile::tempdir().expect("temporary draft directory");
        let current = directory.path().join("state/notm/draft.json");
        let legacy = directory.path().join("cache/notm/draft.json");
        std::fs::create_dir_all(legacy.parent().expect("legacy parent"))
            .expect("create legacy directory");
        let bytes = br#"{"from":"Me","to":"you@example.test","cc":"","bcc":"","subject":"Legacy","body":"Body"}"#;
        std::fs::write(&legacy, bytes).expect("write legacy draft");
        assert!(migrate_legacy_recovery_draft(&current, &legacy).expect("migrate legacy draft"));
        assert_eq!(std::fs::read(&current).expect("read migrated draft"), bytes);
        assert!(!legacy.exists());
        assert!(
            !migrate_legacy_recovery_draft(&current, &legacy).expect("repeat migration is a no-op")
        );
    }

    #[test]
    fn empty_composer_removes_current_and_legacy_recovery_files() {
        let directory = tempfile::tempdir().expect("temporary draft directory");
        let current = directory.path().join("state/notm/draft.json");
        let legacy = directory.path().join("cache/notm/draft.json");
        let fields = ComposeFields {
            subject: "Still editing".to_string(),
            ..ComposeFields::default()
        };
        persist_recovery_draft(&current, Some(&legacy), &fields)
            .expect("write current recovery draft");
        std::fs::create_dir_all(legacy.parent().expect("legacy parent"))
            .expect("create legacy directory");
        std::fs::write(&legacy, b"legacy").expect("write stale legacy draft");
        persist_recovery_draft(&current, Some(&legacy), &ComposeFields::default())
            .expect("clear recovery drafts");
        assert!(!current.exists());
        assert!(!legacy.exists());
    }

    #[test]
    fn named_drafts_migrate_and_duplicate_fallback_rows_are_hidden() {
        let directory = tempfile::tempdir().expect("temporary named draft directory");
        let current = directory.path().join("state/notm/drafts");
        let legacy = directory.path().join("cache/notm/drafts");
        let existing = ComposeFields {
            subject: "Existing".to_string(),
            ..ComposeFields::default()
        };
        let legacy_only = ComposeFields {
            subject: "Legacy only".to_string(),
            ..ComposeFields::default()
        };
        std::fs::create_dir_all(&current).expect("create current drafts directory");
        std::fs::create_dir_all(&legacy).expect("create legacy drafts directory");
        let existing_bytes = serde_json::to_vec_pretty(&existing).expect("serialize draft");
        std::fs::write(current.join("existing.json"), &existing_bytes)
            .expect("write current draft");
        std::fs::write(legacy.join("existing.json"), &existing_bytes)
            .expect("write duplicate legacy draft");
        std::fs::write(
            legacy.join("legacy.json"),
            serde_json::to_vec_pretty(&legacy_only).expect("serialize legacy draft"),
        )
        .expect("write legacy draft");
        assert_eq!(
            migrate_legacy_named_drafts(&current, &legacy).expect("migrate named drafts"),
            1
        );
        assert!(!legacy.join("existing.json").exists());
        assert!(!legacy.join("legacy.json").exists());
        let migrated = list_named_drafts(&current, Some(&legacy));
        assert_eq!(migrated.len(), 2);
        assert!(
            migrated
                .iter()
                .any(|(_, fields)| fields.subject == "Legacy only")
        );
        std::fs::write(
            legacy.join("legacy.json"),
            serde_json::to_vec_pretty(&legacy_only).expect("serialize fallback draft"),
        )
        .expect("write duplicate fallback draft");
        assert_eq!(list_named_drafts(&current, Some(&legacy)).len(), 2);
    }

    #[test]
    fn successful_autosave_clears_only_transient_autosave_errors() {
        let mut last_error = Some("Draft autosave failed: temporary error".to_string());
        assert!(clear_transient_autosave_error(&mut last_error));
        assert_eq!(last_error, None);
        let mut last_error = Some("Send failed: permanent error".to_string());
        assert!(!clear_transient_autosave_error(&mut last_error));
        assert_eq!(last_error.as_deref(), Some("Send failed: permanent error"));
    }

    #[test]
    fn accepted_send_cleanup_plan_preserves_generation_and_draft_identity() {
        let fields = ComposeFields::default();
        let sent = ActiveDraft {
            path: PathBuf::from("/tmp/sent-draft"),
            message_id: None,
            indexed: false,
            saved_fields: fields.clone(),
        };
        let changed = ActiveDraft {
            path: PathBuf::from("/tmp/new-draft"),
            message_id: None,
            indexed: false,
            saved_fields: fields,
        };
        let reset = plan_accepted_send_cleanup(4, 4, Some(&sent), Some(&sent), true);
        assert!(reset.clear_active_draft);
        assert!(reset.clear_recovery);
        assert!(reset.reset_composer(true));
        let newer = plan_accepted_send_cleanup(4, 5, Some(&sent), Some(&sent), true);
        assert!(newer.clear_active_draft);
        assert!(!newer.clear_recovery);
        assert!(newer.newer_composer_changes);
        let identity = plan_accepted_send_cleanup(4, 4, Some(&sent), Some(&changed), true);
        assert!(identity.draft_identity_changed);
        assert!(!identity.clear_active_draft);
    }

    #[test]
    fn send_cleanup_issues_preserve_every_stage_in_order() {
        let issues = vec![
            SendCleanupIssue::new(SendCleanupStage::SentPersistence, "sent store"),
            SendCleanupIssue::new(SendCleanupStage::DraftDelete, "draft source"),
            SendCleanupIssue::new(SendCleanupStage::RecoveryClear, "recovery file"),
        ];
        assert_eq!(
            format_send_cleanup_issues(&issues).as_deref(),
            Some(
                "sent save/index failed: sent store; draft delete failed: draft source; draft recovery clear failed: recovery file"
            )
        );
    }
}
