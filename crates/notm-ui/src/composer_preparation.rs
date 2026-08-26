use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use notm_mail::{
    ComposedMessage, ReplyKind,
    compose::Identity,
    forward::{build_attachment_forward, build_inline_forward},
};

use crate::{
    attachment_io::ComposerAttachmentSource, model::ComposeFields, thread_loader::PreparedThread,
    widgets::composer,
};

/// Composer text is applied to GTK entries and text buffers in one bounded UI
/// update. Keep the aggregate below the existing four-MiB message text limit,
/// rather than allowing each independently cloned field to reach that size.
const MAX_COMPOSER_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// A draft with more parts than this is rejected before building the GTK-facing
/// replacement or starting the attachment-cache worker.
const MAX_COMPOSER_ATTACHMENT_COUNT: usize = 256;
/// Forwarded source and indexed-draft attachments stay as lazy locators, but
/// their eventual decoded/copied payload still needs an aggregate budget.
const MAX_COMPOSER_ATTACHMENT_BYTES: usize = 32 * 1024 * 1024;
const MAX_FIXTURE_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy)]
struct PreparationLimits {
    text_bytes: usize,
    attachment_count: usize,
    attachment_bytes: usize,
}

const DEFAULT_LIMITS: PreparationLimits = PreparationLimits {
    text_bytes: MAX_COMPOSER_TEXT_BYTES,
    attachment_count: MAX_COMPOSER_ATTACHMENT_COUNT,
    attachment_bytes: MAX_COMPOSER_ATTACHMENT_BYTES,
};

#[derive(Debug, Clone)]
pub(crate) enum ComposerPreparationAction {
    Reply {
        kind: ReplyKind,
        identity: Identity,
        own_addresses: Vec<String>,
    },
    InlineForward {
        identity: Identity,
    },
    AttachmentForward {
        identity: Identity,
    },
    IndexedDraft,
}

#[derive(Debug)]
pub(crate) enum ComposerPreparationOutput {
    Message(ComposedMessage),
    MessageWithAttachments {
        message: ComposedMessage,
        sources: Vec<ComposerAttachmentSource>,
    },
    IndexedDraft {
        fields: ComposeFields,
        sources: Vec<ComposerAttachmentSource>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ComposerPreparationToken {
    generation: u64,
    current_generation: Arc<AtomicU64>,
}

impl ComposerPreparationToken {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    fn cancelled(&self) -> bool {
        self.current_generation.load(Ordering::Acquire) != self.generation
    }

    fn ensure_current(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.cancelled(), "composer preparation was cancelled");
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct ComposerPreparationRequest {
    token: ComposerPreparationToken,
    prepared_thread: Arc<PreparedThread>,
    message_id: String,
    action: ComposerPreparationAction,
    fixture_delay: Duration,
    limits: PreparationLimits,
}

impl ComposerPreparationRequest {
    pub(crate) fn new(
        token: ComposerPreparationToken,
        prepared_thread: Arc<PreparedThread>,
        message_id: String,
        action: ComposerPreparationAction,
    ) -> Self {
        Self {
            token,
            prepared_thread,
            message_id,
            action,
            fixture_delay: Duration::ZERO,
            limits: DEFAULT_LIMITS,
        }
    }

    pub(crate) fn with_fixture_delay(mut self, delay: Duration) -> Self {
        self.fixture_delay = delay.min(MAX_FIXTURE_DELAY);
        self
    }
}

#[derive(Debug)]
pub(crate) struct ComposerPreparationResponse {
    pub(crate) generation: u64,
    pub(crate) result: anyhow::Result<ComposerPreparationOutput>,
}

#[derive(Debug)]
pub(crate) struct ComposerPreparationCoordinator {
    next_generation: u64,
    active: Option<u64>,
    current_generation: Arc<AtomicU64>,
}

impl Default for ComposerPreparationCoordinator {
    fn default() -> Self {
        Self {
            next_generation: 0,
            active: None,
            current_generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl ComposerPreparationCoordinator {
    pub(crate) fn begin(&mut self) -> ComposerPreparationToken {
        self.next_generation = self.next_generation.saturating_add(1);
        self.active = Some(self.next_generation);
        self.current_generation
            .store(self.next_generation, Ordering::Release);
        ComposerPreparationToken {
            generation: self.next_generation,
            current_generation: self.current_generation.clone(),
        }
    }

    pub(crate) fn cancel(&mut self) {
        self.next_generation = self.next_generation.saturating_add(1);
        self.active = None;
        self.current_generation
            .store(self.next_generation, Ordering::Release);
    }

    pub(crate) fn accepts(&self, generation: u64) -> bool {
        self.active == Some(generation)
            && self.current_generation.load(Ordering::Acquire) == generation
    }

    pub(crate) fn finish(&mut self, generation: u64) -> bool {
        if !self.accepts(generation) {
            return false;
        }
        self.active = None;
        true
    }

    pub(crate) fn active_generation(&self) -> Option<u64> {
        self.active
    }
}

pub(crate) fn spawn(
    request: ComposerPreparationRequest,
) -> mpsc::Receiver<ComposerPreparationResponse> {
    let generation = request.token.generation();
    spawn_with(generation, request.token.clone(), move || prepare(request))
}

fn spawn_with<F>(
    generation: u64,
    token: ComposerPreparationToken,
    prepare: F,
) -> mpsc::Receiver<ComposerPreparationResponse>
where
    F: FnOnce() -> anyhow::Result<ComposerPreparationOutput> + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    if let Err(error) = thread::Builder::new()
        .name("notm-composer-preparation".to_string())
        .spawn(move || {
            let result = token
                .ensure_current()
                .and_then(|()| prepare())
                .and_then(|output| token.ensure_current().map(|()| output));
            let _ = worker_sender.send(ComposerPreparationResponse { generation, result });
        })
    {
        let _ = sender.send(ComposerPreparationResponse {
            generation,
            result: Err(anyhow::anyhow!(
                "could not spawn composer preparation worker: {error}"
            )),
        });
    }
    receiver
}

fn prepare(request: ComposerPreparationRequest) -> anyhow::Result<ComposerPreparationOutput> {
    let ComposerPreparationRequest {
        token,
        prepared_thread,
        message_id,
        action,
        fixture_delay,
        limits,
    } = request;
    if !fixture_delay.is_zero() {
        thread::sleep(fixture_delay);
    }
    token.ensure_current()?;
    let prepared = prepared_thread
        .message_contents
        .get(&message_id)
        .ok_or_else(|| anyhow::anyhow!("message content is still loading"))?;
    let parsed = prepared.parsed()?;

    let output = match action {
        ComposerPreparationAction::Reply {
            kind,
            identity,
            own_addresses,
        } => ComposerPreparationOutput::Message(notm_mail::build_reply(
            parsed,
            &identity,
            &own_addresses,
            kind,
        )),
        ComposerPreparationAction::InlineForward { identity } => {
            ComposerPreparationOutput::Message(build_inline_forward(parsed, &identity))
        }
        ComposerPreparationAction::AttachmentForward { identity } => {
            let mut message = build_attachment_forward(parsed, &identity, Vec::new());
            let template = message
                .attachments
                .pop()
                .ok_or_else(|| anyhow::anyhow!("forward attachment metadata was not created"))?;
            ComposerPreparationOutput::MessageWithAttachments {
                message,
                sources: vec![ComposerAttachmentSource::message_file(
                    template.filename,
                    prepared.source()?.clone(),
                )],
            }
        }
        ComposerPreparationAction::IndexedDraft => {
            let (fields, _) = composer::prepare_draft_fields_from_message(parsed, Vec::new());
            let sources = prepared_thread
                .attachments
                .iter()
                .filter(|attachment| attachment.message_id == message_id)
                .map(|attachment| {
                    ComposerAttachmentSource::mime_part(
                        attachment.filename.clone(),
                        attachment.source.clone(),
                        attachment.attachment_index,
                        attachment.size,
                    )
                })
                .collect();
            ComposerPreparationOutput::IndexedDraft { fields, sources }
        }
    };
    token.ensure_current()?;
    validate_output(&output, limits)?;
    Ok(output)
}

fn validate_output(
    output: &ComposerPreparationOutput,
    limits: PreparationLimits,
) -> anyhow::Result<()> {
    let (text_bytes, sources) = match output {
        ComposerPreparationOutput::Message(message) => (message_text_bytes(message), &[][..]),
        ComposerPreparationOutput::MessageWithAttachments { message, sources } => {
            (message_text_bytes(message), sources.as_slice())
        }
        ComposerPreparationOutput::IndexedDraft { fields, sources } => {
            (compose_fields_text_bytes(fields), sources.as_slice())
        }
    };
    anyhow::ensure!(
        text_bytes <= limits.text_bytes,
        "prepared composer text is {text_bytes} bytes; the responsive composer limit is {} bytes",
        limits.text_bytes
    );
    anyhow::ensure!(
        sources.len() <= limits.attachment_count,
        "prepared composer has {} attachments; the responsive composer limit is {} attachments",
        sources.len(),
        limits.attachment_count
    );
    let attachment_bytes = sources.iter().fold(0_usize, |total, source| {
        total.saturating_add(source.byte_len())
    });
    anyhow::ensure!(
        attachment_bytes <= limits.attachment_bytes,
        "prepared composer attachments total {attachment_bytes} bytes; the responsive composer limit is {} bytes",
        limits.attachment_bytes
    );
    Ok(())
}

fn message_text_bytes(message: &ComposedMessage) -> usize {
    let scalars = [
        &message.from,
        &message.subject,
        &message.body,
        &message.message_id,
    ];
    scalars
        .into_iter()
        .fold(0_usize, |total, value| total.saturating_add(value.len()))
        .saturating_add(string_list_bytes(&message.to))
        .saturating_add(string_list_bytes(&message.cc))
        .saturating_add(string_list_bytes(&message.bcc))
        .saturating_add(string_list_bytes(&message.references))
        .saturating_add(message.html_body.as_ref().map(String::len).unwrap_or(0))
        .saturating_add(
            message
                .text_reply_quote
                .as_ref()
                .map(String::len)
                .unwrap_or(0),
        )
        .saturating_add(
            message
                .html_reply_quote
                .as_ref()
                .map(String::len)
                .unwrap_or(0),
        )
        .saturating_add(message.in_reply_to.as_ref().map(String::len).unwrap_or(0))
        .saturating_add(
            message
                .attachments
                .iter()
                .fold(0_usize, |total, attachment| {
                    total
                        .saturating_add(attachment.filename.len())
                        .saturating_add(attachment.content_type.len())
                        .saturating_add(attachment.bytes.len())
                        .saturating_add(
                            attachment
                                .source_path
                                .as_ref()
                                .map(|path| path.as_os_str().len())
                                .unwrap_or(0),
                        )
                }),
        )
}

fn compose_fields_text_bytes(fields: &ComposeFields) -> usize {
    let scalars = [
        &fields.from,
        &fields.to,
        &fields.cc,
        &fields.bcc,
        &fields.subject,
        &fields.body,
    ];
    scalars
        .into_iter()
        .fold(0_usize, |total, value| total.saturating_add(value.len()))
        .saturating_add(string_list_bytes(&fields.attachments))
        .saturating_add(string_list_bytes(&fields.references))
        .saturating_add(fields.in_reply_to.as_ref().map(String::len).unwrap_or(0))
        .saturating_add(
            fields
                .text_reply_quote
                .as_ref()
                .map(String::len)
                .unwrap_or(0),
        )
        .saturating_add(
            fields
                .html_reply_quote
                .as_ref()
                .map(String::len)
                .unwrap_or(0),
        )
}

fn string_list_bytes(values: &[String]) -> usize {
    values
        .iter()
        .fold(0_usize, |total, value| total.saturating_add(value.len()))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn message(body: String) -> ComposedMessage {
        ComposedMessage::new(
            "sender@example.test".to_string(),
            vec!["recipient@example.test".to_string()],
            "subject".to_string(),
            body,
        )
    }

    #[test]
    fn coordinator_generations_are_ordered_and_only_latest_finishes() {
        let mut coordinator = ComposerPreparationCoordinator::default();
        let first = coordinator.begin();
        let second = coordinator.begin();

        assert!(first.generation() < second.generation());
        assert!(!coordinator.accepts(first.generation()));
        assert!(!coordinator.finish(first.generation()));
        assert!(coordinator.finish(second.generation()));
        assert_eq!(coordinator.active_generation(), None);
    }

    #[test]
    fn cancellation_reaches_slow_worker_and_invalidates_completion() {
        let mut coordinator = ComposerPreparationCoordinator::default();
        let token = coordinator.begin();
        let generation = token.generation();
        let (started_sender, started_receiver) = mpsc::channel();
        let response = spawn_with(generation, token, move || {
            started_sender.send(()).expect("announce worker start");
            thread::sleep(Duration::from_millis(50));
            Ok(ComposerPreparationOutput::Message(message(
                "slow".to_string(),
            )))
        });
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("slow worker started");
        coordinator.cancel();

        let response = response.recv().expect("worker response");
        assert!(response.result.is_err());
        assert!(!coordinator.accepts(response.generation));
        assert!(!coordinator.finish(response.generation));
    }

    #[test]
    fn stale_slow_completion_cannot_overtake_newer_completion() {
        let mut coordinator = ComposerPreparationCoordinator::default();
        let slow = coordinator.begin();
        let slow_generation = slow.generation();
        let (started_sender, started_receiver) = mpsc::channel();
        let slow_response = spawn_with(slow_generation, slow, move || {
            started_sender.send(()).expect("announce worker start");
            thread::sleep(Duration::from_millis(60));
            Ok(ComposerPreparationOutput::Message(message(
                "slow".to_string(),
            )))
        });
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("slow worker started");
        let fast = coordinator.begin();
        let fast_generation = fast.generation();
        let started = Instant::now();
        let fast_response = spawn_with(fast_generation, fast, move || {
            Ok(ComposerPreparationOutput::Message(message(
                "fast".to_string(),
            )))
        });

        let fast = fast_response.recv().expect("fast response");
        assert!(started.elapsed() < Duration::from_millis(50));
        assert!(coordinator.finish(fast.generation));
        let slow = slow_response.recv().expect("slow response");
        assert!(!coordinator.finish(slow.generation));
    }

    #[test]
    fn oversized_composer_text_is_rejected() {
        let output = ComposerPreparationOutput::Message(message("0123456789".to_string()));
        let error = validate_output(
            &output,
            PreparationLimits {
                text_bytes: 8,
                ..DEFAULT_LIMITS
            },
        )
        .expect_err("composer text must be bounded");

        assert!(error.to_string().contains("responsive composer limit is 8"));
    }

    #[test]
    fn attachment_count_and_bytes_are_bounded_without_copying_shared_payloads() {
        let payload: Arc<[u8]> = Arc::from(vec![b'x'; 9]);
        let output = ComposerPreparationOutput::MessageWithAttachments {
            message: message(String::new()),
            sources: vec![
                ComposerAttachmentSource::shared("one.bin".to_string(), payload.clone()),
                ComposerAttachmentSource::shared("two.bin".to_string(), payload),
            ],
        };
        let count_error = validate_output(
            &output,
            PreparationLimits {
                attachment_count: 1,
                ..DEFAULT_LIMITS
            },
        )
        .expect_err("attachment count must be bounded");
        assert!(count_error.to_string().contains("limit is 1 attachments"));

        let bytes_error = validate_output(
            &output,
            PreparationLimits {
                attachment_bytes: 17,
                ..DEFAULT_LIMITS
            },
        )
        .expect_err("attachment bytes must be bounded");
        assert!(bytes_error.to_string().contains("limit is 17 bytes"));
    }
}
