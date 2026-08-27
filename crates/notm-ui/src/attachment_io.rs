use std::{
    error::Error,
    ffi::{OsStr, OsString},
    fmt,
    fs::File,
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

use anyhow::Context as _;
use notm_mail::attachments::sanitize_attachment_filename;
use notm_mail::compose::AttachmentInput;
use notm_mail::mime::extract_attachments_detailed;
use uuid::Uuid;

use crate::{
    thread_loader::{AuthoritativePathMap, MessageSource},
    widgets::composer,
};

#[cfg(test)]
use std::sync::Condvar;

pub(crate) const MAX_FIXTURE_DELAY: Duration = Duration::from_secs(5);
pub(crate) const MAX_ATTACHMENT_SOURCE_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_ATTACHMENT_DECODED_BYTES: usize = 32 * 1024 * 1024;
const MAX_COMPOSER_CACHE_SOURCES: usize = 256;
const MAX_COMPOSER_CACHE_BYTES: usize = 32 * 1024 * 1024;
const COMPOSER_CACHE_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachmentIoAction {
    SaveToDirectory,
    SaveToTarget,
    PrepareOpen,
}

impl AttachmentIoAction {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::SaveToDirectory => "save_to_directory",
            Self::SaveToTarget => "save_to_target",
            Self::PrepareOpen => "prepare_open",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AttachmentIoToken {
    pub(crate) generation: u64,
    pub(crate) request_id: u64,
}

#[derive(Debug, Default)]
pub(crate) struct AttachmentIoCoordinator {
    generation: u64,
    next_request_id: u64,
    active: Option<AttachmentIoToken>,
}

impl AttachmentIoCoordinator {
    pub(crate) fn begin(&mut self) -> AttachmentIoToken {
        self.generation = self.generation.saturating_add(1);
        self.next_request_id = self.next_request_id.saturating_add(1);
        let token = AttachmentIoToken {
            generation: self.generation,
            request_id: self.next_request_id,
        };
        self.active = Some(token);
        token
    }

    pub(crate) fn cancel(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.active = None;
    }

    pub(crate) fn accepts(&self, token: AttachmentIoToken) -> bool {
        self.active == Some(token)
    }

    pub(crate) fn active_token(&self) -> Option<AttachmentIoToken> {
        self.active
    }

    pub(crate) fn finish(&mut self, token: AttachmentIoToken) -> bool {
        if !self.accepts(token) {
            return false;
        }
        self.active = None;
        true
    }
}

#[derive(Debug)]
enum AttachmentIoDestination {
    Directory {
        directory: PathBuf,
        filename: String,
    },
    Target {
        target: PathBuf,
    },
    OpenStore {
        directory: PathBuf,
        filename: String,
    },
}

impl AttachmentIoDestination {
    const fn action(&self) -> AttachmentIoAction {
        match self {
            Self::Directory { .. } => AttachmentIoAction::SaveToDirectory,
            Self::Target { .. } => AttachmentIoAction::SaveToTarget,
            Self::OpenStore { .. } => AttachmentIoAction::PrepareOpen,
        }
    }
}

#[derive(Debug)]
pub(crate) struct AttachmentIoRequest {
    token: AttachmentIoToken,
    destination: AttachmentIoDestination,
    source: AttachmentIoSource,
    fixture_delay: Duration,
    fixture_fail_before_publish: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum AttachmentIoSource {
    Shared(Arc<[u8]>),
    MimePart {
        source: MessageSource,
        part_index: usize,
    },
}

impl From<Arc<[u8]>> for AttachmentIoSource {
    fn from(bytes: Arc<[u8]>) -> Self {
        Self::Shared(bytes)
    }
}

impl AttachmentIoSource {
    pub(crate) fn mime_part(source: MessageSource, part_index: usize) -> Self {
        Self::MimePart { source, part_index }
    }

    pub(crate) fn apply_authoritative_path_states(
        &self,
        message_id: &str,
        path_map: &AuthoritativePathMap<'_>,
    ) -> bool {
        match self {
            Self::Shared(_) => true,
            Self::MimePart { source, .. } => path_map.apply_to_source(message_id, source),
        }
    }
}

impl AttachmentIoRequest {
    pub(crate) fn save_to_directory(
        token: AttachmentIoToken,
        directory: PathBuf,
        filename: String,
        source: impl Into<AttachmentIoSource>,
    ) -> Self {
        Self {
            token,
            destination: AttachmentIoDestination::Directory {
                directory,
                filename,
            },
            source: source.into(),
            fixture_delay: Duration::ZERO,
            fixture_fail_before_publish: false,
        }
    }

    pub(crate) fn save_to_target(
        token: AttachmentIoToken,
        target: PathBuf,
        source: impl Into<AttachmentIoSource>,
    ) -> Self {
        Self {
            token,
            destination: AttachmentIoDestination::Target { target },
            source: source.into(),
            fixture_delay: Duration::ZERO,
            fixture_fail_before_publish: false,
        }
    }

    pub(crate) fn prepare_open(
        token: AttachmentIoToken,
        private_directory: PathBuf,
        filename: String,
        source: impl Into<AttachmentIoSource>,
    ) -> Self {
        Self {
            token,
            destination: AttachmentIoDestination::OpenStore {
                directory: private_directory,
                filename,
            },
            source: source.into(),
            fixture_delay: Duration::ZERO,
            fixture_fail_before_publish: false,
        }
    }

    pub(crate) fn with_fixture_delay(mut self, delay: Duration) -> Self {
        self.fixture_delay = delay.min(MAX_FIXTURE_DELAY);
        self
    }

    pub(crate) fn with_fixture_fail_before_publish(mut self, fail: bool) -> Self {
        self.fixture_fail_before_publish = fail;
        self
    }

    pub(crate) const fn token(&self) -> AttachmentIoToken {
        self.token
    }

    pub(crate) const fn action(&self) -> AttachmentIoAction {
        self.destination.action()
    }

    #[cfg(test)]
    const fn fixture_delay(&self) -> Duration {
        self.fixture_delay
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttachmentIoCompleted {
    pub(crate) action: AttachmentIoAction,
    pub(crate) path: PathBuf,
}

#[derive(Debug)]
pub(crate) enum AttachmentIoError {
    LoadPayload {
        action: AttachmentIoAction,
        source: anyhow::Error,
    },
    SaveToDirectory {
        directory: PathBuf,
        filename: String,
        source: io::Error,
    },
    SaveToTarget {
        target: PathBuf,
        source: io::Error,
    },
    PrepareOpen {
        directory: PathBuf,
        filename: String,
        source: io::Error,
    },
    WorkerStart {
        action: AttachmentIoAction,
        source: io::Error,
    },
}

impl AttachmentIoError {
    #[cfg(test)]
    pub(crate) const fn action(&self) -> AttachmentIoAction {
        match self {
            Self::LoadPayload { action, .. } => *action,
            Self::SaveToDirectory { .. } => AttachmentIoAction::SaveToDirectory,
            Self::SaveToTarget { .. } => AttachmentIoAction::SaveToTarget,
            Self::PrepareOpen { .. } => AttachmentIoAction::PrepareOpen,
            Self::WorkerStart { action, .. } => *action,
        }
    }
}

impl fmt::Display for AttachmentIoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoadPayload { action, source } => {
                write!(formatter, "loading attachment for {action:?}: {source:#}")
            }
            Self::SaveToDirectory {
                directory,
                filename,
                source,
            } => write!(
                formatter,
                "saving attachment {filename:?} in {}: {source}",
                directory.display()
            ),
            Self::SaveToTarget { target, source } => write!(
                formatter,
                "saving attachment to {}: {source}",
                target.display()
            ),
            Self::PrepareOpen {
                directory,
                filename,
                source,
            } => write!(
                formatter,
                "preparing attachment {filename:?} in private directory {}: {source}",
                directory.display()
            ),
            Self::WorkerStart { action, source } => {
                write!(formatter, "starting {action:?} attachment worker: {source}")
            }
        }
    }
}

impl Error for AttachmentIoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::LoadPayload { source, .. } => Some(source.root_cause()),
            Self::SaveToDirectory { source, .. }
            | Self::SaveToTarget { source, .. }
            | Self::PrepareOpen { source, .. }
            | Self::WorkerStart { source, .. } => Some(source),
        }
    }
}

#[derive(Debug)]
pub(crate) struct AttachmentIoResponse {
    pub(crate) token: AttachmentIoToken,
    pub(crate) result: Result<AttachmentIoCompleted, AttachmentIoError>,
}

#[derive(Debug)]
pub(crate) enum ComposerAttachmentSource {
    Owned {
        filename: String,
        bytes: Vec<u8>,
        source_path: Option<PathBuf>,
    },
    Shared {
        filename: String,
        bytes: Arc<[u8]>,
    },
    MessageFile {
        filename: String,
        source: MessageSource,
    },
    MimePart {
        filename: String,
        source: MessageSource,
        part_index: usize,
        encoded_size: usize,
    },
}

impl ComposerAttachmentSource {
    pub(crate) fn from_input(input: AttachmentInput) -> Self {
        match input.source_path {
            Some(source_path) => Self::Owned {
                filename: input.filename,
                bytes: input.bytes,
                source_path: Some(source_path),
            },
            None => Self::shared(input.filename, Arc::from(input.bytes)),
        }
    }

    pub(crate) fn shared(filename: String, bytes: Arc<[u8]>) -> Self {
        Self::Shared { filename, bytes }
    }

    pub(crate) fn message_file(filename: String, source: MessageSource) -> Self {
        Self::MessageFile { filename, source }
    }

    pub(crate) fn mime_part(
        filename: String,
        source: MessageSource,
        part_index: usize,
        encoded_size: usize,
    ) -> Self {
        Self::MimePart {
            filename,
            source,
            part_index,
            encoded_size,
        }
    }

    fn filename(&self) -> &str {
        match self {
            Self::Owned { filename, .. }
            | Self::Shared { filename, .. }
            | Self::MessageFile { filename, .. }
            | Self::MimePart { filename, .. } => filename,
        }
    }

    fn resident_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Owned { bytes, .. } => Some(bytes),
            Self::Shared { bytes, .. } => Some(bytes),
            Self::MessageFile { .. } | Self::MimePart { .. } => None,
        }
    }

    pub(crate) fn byte_len(&self) -> usize {
        match self {
            Self::Owned { bytes, .. } => bytes.len(),
            Self::Shared { bytes, .. } => bytes.len(),
            Self::MessageFile { source, .. } => source.source_bytes(),
            Self::MimePart { encoded_size, .. } => *encoded_size,
        }
    }

    fn existing_source(&self) -> Option<&Path> {
        match self {
            Self::Owned {
                source_path: Some(path),
                ..
            } if path.exists() => Some(path),
            Self::Owned { .. }
            | Self::Shared { .. }
            | Self::MessageFile { .. }
            | Self::MimePart { .. } => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ComposerAttachmentCacheRequest {
    pub(crate) generation: u64,
    sources: Vec<ComposerAttachmentSource>,
    directory: PathBuf,
    fixture_delay: Duration,
    #[cfg(test)]
    fixture_step_delay: Duration,
    #[cfg(test)]
    fixture_gate: Option<Arc<ComposerAttachmentCacheFixtureGate>>,
}

impl ComposerAttachmentCacheRequest {
    pub(crate) fn new(
        generation: u64,
        sources: Vec<ComposerAttachmentSource>,
        directory: PathBuf,
    ) -> Self {
        Self {
            generation,
            sources,
            directory,
            fixture_delay: Duration::ZERO,
            #[cfg(test)]
            fixture_step_delay: Duration::ZERO,
            #[cfg(test)]
            fixture_gate: None,
        }
    }

    pub(crate) fn with_fixture_delay(mut self, delay: Duration) -> Self {
        self.fixture_delay = delay.min(MAX_FIXTURE_DELAY);
        self
    }

    #[cfg(test)]
    fn with_fixture_step_delay(mut self, delay: Duration) -> Self {
        self.fixture_step_delay = delay.min(MAX_FIXTURE_DELAY);
        self
    }

    #[cfg(test)]
    fn with_fixture_gate(mut self, gate: Arc<ComposerAttachmentCacheFixtureGate>) -> Self {
        self.fixture_gate = Some(gate);
        self
    }

    fn fixture_step_delay(&self) -> Duration {
        #[cfg(test)]
        {
            self.fixture_step_delay
        }
        #[cfg(not(test))]
        {
            Duration::ZERO
        }
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
struct ComposerAttachmentCacheFixtureGate {
    state: Mutex<ComposerAttachmentCacheFixtureGateState>,
    changed: Condvar,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct ComposerAttachmentCacheFixtureGateState {
    entered: bool,
    released: bool,
}

#[cfg(test)]
impl ComposerAttachmentCacheFixtureGate {
    fn enter_and_wait(&self) {
        let mut state = self.state.lock().expect("composer cache gate poisoned");
        state.entered = true;
        self.changed.notify_all();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(2), |state| !state.released)
            .expect("composer cache gate poisoned while waiting");
        drop(state);
    }

    fn wait_until_entered(&self, timeout: Duration) -> bool {
        let state = self.state.lock().expect("composer cache gate poisoned");
        let (state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| !state.entered)
            .expect("composer cache gate poisoned while observing");
        state.entered
    }

    fn release(&self) {
        let mut state = self.state.lock().expect("composer cache gate poisoned");
        state.released = true;
        self.changed.notify_all();
    }
}

#[derive(Debug)]
pub(crate) struct ComposerAttachmentCacheResponse {
    pub(crate) generation: u64,
    pub(crate) result: anyhow::Result<ComposerAttachmentCacheLease>,
}

/// Owns the private directory produced by one cache request until the UI
/// accepts its replacement. Dropping a stale completion removes only that
/// request-owned tree; reused user-selected paths are never removed.
#[derive(Debug)]
pub(crate) struct ComposerAttachmentCacheLease {
    paths: Vec<String>,
    owned_directory: Option<PathBuf>,
}

impl ComposerAttachmentCacheLease {
    pub(crate) fn paths(&self) -> &[String] {
        &self.paths
    }

    pub(crate) fn commit(mut self) -> Vec<String> {
        self.owned_directory = None;
        std::mem::take(&mut self.paths)
    }
}

impl Drop for ComposerAttachmentCacheLease {
    fn drop(&mut self) {
        cleanup_cache_directory_best_effort(self.owned_directory.as_deref());
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ComposerAttachmentCacheSnapshot {
    pub(crate) active_generation: Option<u64>,
    pub(crate) pending_generation: Option<u64>,
    pub(crate) latest_generation: u64,
    pub(crate) pending_requests: usize,
    pub(crate) peak_pending_requests: usize,
    pub(crate) active_preparations: usize,
    pub(crate) peak_active_preparations: usize,
    pub(crate) submitted: usize,
    pub(crate) cancelled: usize,
    pub(crate) coalesced: usize,
}

#[derive(Debug, Default)]
struct ComposerAttachmentCacheMetrics {
    latest_generation: AtomicU64,
    peak_pending_requests: AtomicUsize,
    active_preparations: AtomicUsize,
    peak_active_preparations: AtomicUsize,
    submitted: AtomicUsize,
    cancelled: AtomicUsize,
    coalesced: AtomicUsize,
}

impl ComposerAttachmentCacheMetrics {
    fn begin_preparation(&self) {
        let active = self
            .active_preparations
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        self.peak_active_preparations
            .fetch_max(active, Ordering::AcqRel);
    }

    fn finish_preparation(&self) {
        self.active_preparations.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct ActiveComposerAttachmentCache {
    generation: u64,
    cancelled: Arc<AtomicBool>,
}

struct ComposerAttachmentCacheJob {
    request: ComposerAttachmentCacheRequest,
    cancelled: Arc<AtomicBool>,
    response: mpsc::Sender<ComposerAttachmentCacheResponse>,
}

enum ComposerAttachmentCacheCommand {
    Wake,
    Shutdown,
}

struct ComposerAttachmentCacheServiceInner {
    sender: Option<mpsc::SyncSender<ComposerAttachmentCacheCommand>>,
    start_error: Option<Arc<str>>,
    active: Arc<Mutex<Option<ActiveComposerAttachmentCache>>>,
    pending: Arc<Mutex<Option<ComposerAttachmentCacheJob>>>,
    metrics: Arc<ComposerAttachmentCacheMetrics>,
}

impl Drop for ComposerAttachmentCacheServiceInner {
    fn drop(&mut self) {
        cancel_active_composer_cache(&self.active, &self.metrics);
        self.pending
            .lock()
            .expect("composer cache pending mutex poisoned")
            .take();
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(ComposerAttachmentCacheCommand::Shutdown);
        }
    }
}

/// Cloneable handle to one serialized composer-attachment cache worker. The
/// worker is shut down only after the final service handle is dropped.
#[derive(Clone)]
pub(crate) struct ComposerAttachmentCacheService {
    inner: Arc<ComposerAttachmentCacheServiceInner>,
}

impl fmt::Debug for ComposerAttachmentCacheService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ComposerAttachmentCacheService")
            .field("snapshot", &self.snapshot())
            .field("start_error", &self.inner.start_error)
            .finish_non_exhaustive()
    }
}

impl Default for ComposerAttachmentCacheService {
    fn default() -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let active = Arc::new(Mutex::new(None));
        let pending = Arc::new(Mutex::new(None));
        let metrics = Arc::new(ComposerAttachmentCacheMetrics::default());
        let worker_active = active.clone();
        let worker_pending = pending.clone();
        let worker_metrics = metrics.clone();
        let (sender, start_error) = match thread::Builder::new()
            .name("notm-composer-attachment-cache".to_string())
            .spawn(move || {
                composer_attachment_cache_worker(
                    receiver,
                    worker_active,
                    worker_pending,
                    worker_metrics,
                );
            }) {
            Ok(_) => (Some(sender), None),
            Err(error) => (None, Some(Arc::<str>::from(error.to_string()))),
        };
        Self {
            inner: Arc::new(ComposerAttachmentCacheServiceInner {
                sender,
                start_error,
                active,
                pending,
                metrics,
            }),
        }
    }
}

impl ComposerAttachmentCacheService {
    pub(crate) fn submit(
        &self,
        request: ComposerAttachmentCacheRequest,
    ) -> mpsc::Receiver<ComposerAttachmentCacheResponse> {
        let (response, receiver) = mpsc::channel();
        let generation = request.generation;
        self.inner
            .metrics
            .latest_generation
            .fetch_max(generation, Ordering::AcqRel);
        self.inner.metrics.submitted.fetch_add(1, Ordering::AcqRel);
        cancel_active_composer_cache(&self.inner.active, &self.inner.metrics);

        let cancelled = Arc::new(AtomicBool::new(false));
        *self
            .inner
            .active
            .lock()
            .expect("composer cache activity mutex poisoned") =
            Some(ActiveComposerAttachmentCache {
                generation,
                cancelled: cancelled.clone(),
            });
        let job = ComposerAttachmentCacheJob {
            request,
            cancelled: cancelled.clone(),
            response: response.clone(),
        };
        let replaced = self
            .inner
            .pending
            .lock()
            .expect("composer cache pending mutex poisoned")
            .replace(job);
        if replaced.is_some() {
            self.inner.metrics.coalesced.fetch_add(1, Ordering::AcqRel);
        }
        self.inner
            .metrics
            .peak_pending_requests
            .fetch_max(1, Ordering::AcqRel);
        let worker_available = self.inner.sender.as_ref().is_some_and(|sender| {
            matches!(
                sender.try_send(ComposerAttachmentCacheCommand::Wake),
                Ok(()) | Err(mpsc::TrySendError::Full(_))
            )
        });
        if worker_available {
            return receiver;
        }

        remove_pending_composer_cache(&self.inner.pending, generation, &cancelled);
        clear_active_composer_cache(&self.inner.active, generation, &cancelled);
        let error = self
            .inner
            .start_error
            .as_deref()
            .unwrap_or("composer attachment cache worker disconnected");
        let _ = response.send(ComposerAttachmentCacheResponse {
            generation,
            result: Err(anyhow::anyhow!(error.to_string())),
        });
        receiver
    }

    pub(crate) fn cancel(&self) {
        cancel_active_composer_cache(&self.inner.active, &self.inner.metrics);
        if self
            .inner
            .pending
            .lock()
            .expect("composer cache pending mutex poisoned")
            .take()
            .is_some()
        {
            self.inner.metrics.coalesced.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub(crate) fn snapshot(&self) -> ComposerAttachmentCacheSnapshot {
        let active_generation = self
            .inner
            .active
            .lock()
            .expect("composer cache activity mutex poisoned")
            .as_ref()
            .map(|active| active.generation);
        let pending_generation = self
            .inner
            .pending
            .lock()
            .expect("composer cache pending mutex poisoned")
            .as_ref()
            .map(|job| job.request.generation);
        ComposerAttachmentCacheSnapshot {
            active_generation,
            pending_generation,
            latest_generation: self.inner.metrics.latest_generation.load(Ordering::Acquire),
            pending_requests: usize::from(pending_generation.is_some()),
            peak_pending_requests: self
                .inner
                .metrics
                .peak_pending_requests
                .load(Ordering::Acquire),
            active_preparations: self
                .inner
                .metrics
                .active_preparations
                .load(Ordering::Acquire),
            peak_active_preparations: self
                .inner
                .metrics
                .peak_active_preparations
                .load(Ordering::Acquire),
            submitted: self.inner.metrics.submitted.load(Ordering::Acquire),
            cancelled: self.inner.metrics.cancelled.load(Ordering::Acquire),
            coalesced: self.inner.metrics.coalesced.load(Ordering::Acquire),
        }
    }
}

fn cache_composer_attachments_cancellable(
    request: ComposerAttachmentCacheRequest,
    cancelled: &AtomicBool,
) -> anyhow::Result<ComposerAttachmentCacheLease> {
    cache_composer_attachments_cancellable_with_limit(request, cancelled, MAX_COMPOSER_CACHE_BYTES)
}

fn cache_composer_attachments_cancellable_with_limit(
    request: ComposerAttachmentCacheRequest,
    cancelled: &AtomicBool,
    max_cache_bytes: usize,
) -> anyhow::Result<ComposerAttachmentCacheLease> {
    #[cfg(test)]
    if let Some(gate) = request.fixture_gate.as_ref() {
        gate.enter_and_wait();
    }
    anyhow::ensure!(
        request.sources.len() <= MAX_COMPOSER_CACHE_SOURCES,
        "composer attachment cache has {} sources; limit is {MAX_COMPOSER_CACHE_SOURCES}",
        request.sources.len()
    );
    let source_bytes = request.sources.iter().fold(0_usize, |total, source| {
        total.saturating_add(source.byte_len())
    });
    anyhow::ensure!(
        source_bytes <= max_cache_bytes,
        "composer attachment cache sources total {source_bytes} bytes; limit is {max_cache_bytes} bytes"
    );
    ensure_composer_cache_current(cancelled)?;
    wait_for_composer_cache_delay(request.fixture_delay, cancelled)?;

    let fixture_step_delay = request.fixture_step_delay();
    let mut paths = Vec::with_capacity(request.sources.len());
    let mut owned_directory = None;
    let mut materialized_sources = 0_usize;
    let mut cached_bytes = 0_usize;
    let cache_result = (|| -> anyhow::Result<()> {
        for source in request.sources {
            ensure_composer_cache_current(cancelled)?;
            if let Some(path) = source.existing_source() {
                paths.push(path.display().to_string());
                continue;
            }
            let bytes = load_composer_attachment_source(&source)?;
            cached_bytes = cached_bytes.saturating_add(bytes.len());
            anyhow::ensure!(
                cached_bytes <= max_cache_bytes,
                "composer attachment cache loaded {cached_bytes} bytes; limit is {max_cache_bytes} bytes"
            );
            ensure_composer_cache_current(cancelled)?;
            let cache_directory = match owned_directory.as_ref() {
                Some(directory) => directory,
                None => {
                    let directory = create_composer_cache_request_directory(&request.directory)?;
                    owned_directory.insert(directory)
                }
            };
            let source_directory =
                create_composer_cache_source_directory(cache_directory, materialized_sources)?;
            materialized_sources = materialized_sources.saturating_add(1);
            let path = source_directory.join(sanitize_attachment_filename(source.filename()));
            // The atomic writer can report a durability failure after rename.
            // The enclosing UUID directory is request-owned, so every error
            // path can remove the visible cache file, its source directory,
            // and any temporary file the writer already tries to clean up.
            composer::atomic_write_durable(&path, &bytes)
                .with_context(|| format!("caching composer attachment at {}", path.display()))?;
            wait_for_composer_cache_delay(fixture_step_delay, cancelled)?;
            ensure_composer_cache_current(cancelled)?;
            paths.push(path.display().to_string());
        }
        ensure_composer_cache_current(cancelled)
    })();
    if let Err(error) = cache_result {
        return Err(cleanup_cache_directory_after_error(
            owned_directory.as_deref(),
            error,
        ));
    }
    Ok(ComposerAttachmentCacheLease {
        paths,
        owned_directory,
    })
}

fn create_composer_cache_request_directory(root: &Path) -> anyhow::Result<PathBuf> {
    composer::ensure_private_directory(root)
        .with_context(|| format!("preparing composer attachment cache at {}", root.display()))?;
    loop {
        let directory = root.join(Uuid::new_v4().to_string());
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        match builder.create(&directory) {
            Ok(()) => {
                #[cfg(unix)]
                if let Err(error) =
                    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                {
                    let _ = std::fs::remove_dir(&directory);
                    return Err(error).with_context(|| {
                        format!(
                            "setting private composer attachment cache permissions on {}",
                            directory.display()
                        )
                    });
                }
                return Ok(directory);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "creating private composer attachment cache directory in {}",
                        root.display()
                    )
                });
            }
        }
    }
}

fn create_composer_cache_source_directory(
    request_directory: &Path,
    source_index: usize,
) -> anyhow::Result<PathBuf> {
    let directory = request_directory.join(source_index.to_string());
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(&directory).with_context(|| {
        format!(
            "creating private composer attachment source directory {}",
            directory.display()
        )
    })?;
    #[cfg(unix)]
    if let Err(error) = std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
    {
        let _ = std::fs::remove_dir(&directory);
        return Err(error).with_context(|| {
            format!(
                "setting private composer attachment cache permissions on {}",
                directory.display()
            )
        });
    }
    Ok(directory)
}

fn load_composer_attachment_source(source: &ComposerAttachmentSource) -> anyhow::Result<Vec<u8>> {
    match source {
        ComposerAttachmentSource::Owned { .. } | ComposerAttachmentSource::Shared { .. } => {
            let bytes = source
                .resident_bytes()
                .expect("resident composer source matched above");
            anyhow::ensure!(
                bytes.len() <= MAX_ATTACHMENT_DECODED_BYTES,
                "composer attachment is {} bytes; the decoded attachment limit is {MAX_ATTACHMENT_DECODED_BYTES} bytes",
                bytes.len()
            );
            Ok(bytes.to_vec())
        }
        ComposerAttachmentSource::MessageFile { source, .. } => read_message_source(source),
        ComposerAttachmentSource::MimePart {
            source, part_index, ..
        } => extract_requested_part(source, *part_index),
    }
}

fn ensure_composer_cache_current(cancelled: &AtomicBool) -> anyhow::Result<()> {
    anyhow::ensure!(
        !cancelled.load(Ordering::Acquire),
        "composer attachment cache was cancelled"
    );
    Ok(())
}

fn wait_for_composer_cache_delay(delay: Duration, cancelled: &AtomicBool) -> anyhow::Result<()> {
    if delay.is_zero() {
        return ensure_composer_cache_current(cancelled);
    }
    let deadline = Instant::now() + delay;
    loop {
        ensure_composer_cache_current(cancelled)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        thread::sleep(remaining.min(COMPOSER_CACHE_CANCEL_POLL_INTERVAL));
    }
}

fn cleanup_cache_directory_after_error(
    owned_directory: Option<&Path>,
    error: anyhow::Error,
) -> anyhow::Error {
    match cleanup_cache_directory(owned_directory) {
        Ok(()) => error,
        Err(cleanup_error) => error.context(format!(
            "cleaning partial composer attachment cache failed: {cleanup_error:#}"
        )),
    }
}

fn cleanup_cache_directory(owned_directory: Option<&Path>) -> anyhow::Result<()> {
    let Some(directory) = owned_directory else {
        return Ok(());
    };
    match std::fs::remove_dir_all(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "removing stale composer attachment cache directory {}",
                directory.display()
            )
        }),
    }
}

fn cleanup_cache_directory_best_effort(owned_directory: Option<&Path>) {
    if let Err(error) = cleanup_cache_directory(owned_directory) {
        tracing::warn!(%error, "could not remove stale composer attachment cache");
    }
}

fn cancel_active_composer_cache(
    active: &Mutex<Option<ActiveComposerAttachmentCache>>,
    metrics: &ComposerAttachmentCacheMetrics,
) {
    if let Some(active) = active
        .lock()
        .expect("composer cache activity mutex poisoned")
        .take()
        && !active.cancelled.swap(true, Ordering::AcqRel)
    {
        metrics.cancelled.fetch_add(1, Ordering::AcqRel);
    }
}

fn clear_active_composer_cache(
    active: &Mutex<Option<ActiveComposerAttachmentCache>>,
    generation: u64,
    cancelled: &Arc<AtomicBool>,
) {
    let mut active = active
        .lock()
        .expect("composer cache activity mutex poisoned");
    if active.as_ref().is_some_and(|current| {
        current.generation == generation && Arc::ptr_eq(&current.cancelled, cancelled)
    }) {
        *active = None;
    }
}

fn remove_pending_composer_cache(
    pending: &Mutex<Option<ComposerAttachmentCacheJob>>,
    generation: u64,
    cancelled: &Arc<AtomicBool>,
) {
    let mut pending = pending
        .lock()
        .expect("composer cache pending mutex poisoned");
    if pending.as_ref().is_some_and(|job| {
        job.request.generation == generation && Arc::ptr_eq(&job.cancelled, cancelled)
    }) {
        pending.take();
    }
}

fn composer_attachment_cache_worker(
    receiver: mpsc::Receiver<ComposerAttachmentCacheCommand>,
    active: Arc<Mutex<Option<ActiveComposerAttachmentCache>>>,
    pending: Arc<Mutex<Option<ComposerAttachmentCacheJob>>>,
    metrics: Arc<ComposerAttachmentCacheMetrics>,
) {
    while let Ok(command) = receiver.recv() {
        if matches!(command, ComposerAttachmentCacheCommand::Shutdown) {
            break;
        }
        loop {
            let Some(job) = pending
                .lock()
                .expect("composer cache pending mutex poisoned")
                .take()
            else {
                break;
            };
            metrics.begin_preparation();
            let generation = job.request.generation;
            let result = cache_composer_attachments_cancellable(job.request, &job.cancelled);
            metrics.finish_preparation();
            clear_active_composer_cache(&active, generation, &job.cancelled);
            let _ = job
                .response
                .send(ComposerAttachmentCacheResponse { generation, result });
        }
    }
}

pub(crate) fn spawn(request: AttachmentIoRequest) -> mpsc::Receiver<AttachmentIoResponse> {
    let (sender, receiver) = mpsc::channel();
    let token = request.token();
    let action = request.action();
    let worker_sender = sender.clone();
    if let Err(source) = thread::Builder::new()
        .name("notm-attachment-io".to_string())
        .spawn(move || {
            let result = execute(request);
            let _ = worker_sender.send(AttachmentIoResponse { token, result });
        })
    {
        let _ = sender.send(AttachmentIoResponse {
            token,
            result: Err(AttachmentIoError::WorkerStart { action, source }),
        });
    }
    receiver
}

fn execute(request: AttachmentIoRequest) -> Result<AttachmentIoCompleted, AttachmentIoError> {
    if !request.fixture_delay.is_zero() {
        thread::sleep(request.fixture_delay);
    }
    let action = request.action();
    let fail_before_publish = request.fixture_fail_before_publish;
    let bytes = load_attachment_source(request.source)
        .map_err(|source| AttachmentIoError::LoadPayload { action, source })?;
    let path = match request.destination {
        AttachmentIoDestination::Directory {
            directory,
            filename,
        } => save_attachment_atomically_without_overwrite(
            &directory,
            &filename,
            &bytes,
            fail_before_publish,
        )
        .map_err(|source| AttachmentIoError::SaveToDirectory {
            directory,
            filename,
            source,
        })?,
        AttachmentIoDestination::Target { target } => {
            save_attachment_to_target_atomically_without_overwrite(
                &target,
                &bytes,
                fail_before_publish,
            )
            .map_err(|source| AttachmentIoError::SaveToTarget { target, source })?
        }
        AttachmentIoDestination::OpenStore {
            directory,
            filename,
        } => save_attachment_atomically_without_overwrite(
            &directory,
            &filename,
            &bytes,
            fail_before_publish,
        )
        .map_err(|source| AttachmentIoError::PrepareOpen {
            directory,
            filename,
            source,
        })?,
    };
    Ok(AttachmentIoCompleted { action, path })
}

fn save_attachment_atomically_without_overwrite(
    target_dir: &Path,
    filename: &str,
    bytes: &[u8],
    fail_before_publish: bool,
) -> io::Result<PathBuf> {
    let filename = sanitize_attachment_filename(filename);
    save_attachment_to_target_atomically_without_overwrite(
        &target_dir.join(filename),
        bytes,
        fail_before_publish,
    )
}

fn save_attachment_to_target_atomically_without_overwrite(
    target: &Path,
    bytes: &[u8],
    fail_before_publish: bool,
) -> io::Result<PathBuf> {
    save_attachment_to_target_atomically_without_overwrite_with(
        target,
        |file| {
            file.write_all(bytes)?;
            file.sync_all()
        },
        fail_before_publish,
    )
}

fn save_attachment_to_target_atomically_without_overwrite_with(
    target: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
    fail_before_publish: bool,
) -> io::Result<PathBuf> {
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let filename = target.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "attachment target must include a filename",
        )
    })?;
    std::fs::create_dir_all(parent)?;

    let mut builder = tempfile::Builder::new();
    builder.prefix(".notm-attachment-");
    #[cfg(unix)]
    builder.permissions(std::fs::Permissions::from_mode(0o666));
    let mut temporary = builder.tempfile_in(parent)?;
    write(temporary.as_file_mut())?;
    if fail_before_publish {
        return Err(io::Error::other(
            "injected attachment write failure before atomic publish",
        ));
    }

    let mut collision_index = 0_u64;
    loop {
        let candidate = numbered_attachment_filename(filename, collision_index);
        let path = target.with_file_name(candidate);
        match temporary.persist_noclobber(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                temporary = error.file;
                collision_index = collision_index.checked_add(1).ok_or_else(|| {
                    io::Error::other("attachment filename collision counter overflowed")
                })?;
            }
            Err(error) => return Err(error.error),
        }
    }
}

fn numbered_attachment_filename(filename: &OsStr, collision_index: u64) -> OsString {
    if collision_index == 0 {
        return filename.to_os_string();
    }

    let filename_path = Path::new(filename);
    let extension = filename_path
        .extension()
        .filter(|extension| !extension.is_empty());
    let stem = extension
        .and_then(|_| filename_path.file_stem())
        .unwrap_or(filename);
    let mut numbered = OsString::from(stem);
    numbered.push(format!(" ({collision_index})"));
    if let Some(extension) = extension {
        numbered.push(".");
        numbered.push(extension);
    }
    numbered
}

fn load_attachment_source(source: AttachmentIoSource) -> anyhow::Result<Vec<u8>> {
    match source {
        AttachmentIoSource::Shared(bytes) => {
            anyhow::ensure!(
                bytes.len() <= MAX_ATTACHMENT_DECODED_BYTES,
                "attachment is {} bytes; the decoded attachment limit is {MAX_ATTACHMENT_DECODED_BYTES} bytes",
                bytes.len()
            );
            Ok(bytes.as_ref().to_vec())
        }
        AttachmentIoSource::MimePart { source, part_index } => {
            extract_requested_part(&source, part_index)
        }
    }
}

fn read_message_source(source: &MessageSource) -> anyhow::Result<Vec<u8>> {
    read_message_source_with_limit(source, MAX_ATTACHMENT_SOURCE_BYTES)
}

fn read_message_source_with_limit(
    source: &MessageSource,
    max_source_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        source.source_bytes() <= max_source_bytes,
        "message source is {} bytes; the attachment source limit is {max_source_bytes} bytes",
        source.source_bytes()
    );
    source
        .read_bounded(max_source_bytes)
        .with_context(|| format!("reading message source {}", source.path().display()))
}

fn extract_requested_part(source: &MessageSource, part_index: usize) -> anyhow::Result<Vec<u8>> {
    extract_requested_part_with_limits(
        source,
        part_index,
        MAX_ATTACHMENT_SOURCE_BYTES,
        MAX_ATTACHMENT_DECODED_BYTES,
    )
}

fn extract_requested_part_with_limits(
    source: &MessageSource,
    part_index: usize,
    max_source_bytes: usize,
    max_decoded_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    let bytes = read_message_source_with_limit(source, max_source_bytes)?;
    let report = extract_attachments_detailed(&bytes).with_context(|| {
        format!(
            "parsing attachment part {part_index} from {}",
            source.path().display()
        )
    })?;
    if let Some(failure) = report
        .failures
        .into_iter()
        .find(|failure| failure.part_index == part_index)
    {
        anyhow::bail!("{}", failure.error);
    }
    let attachment = report
        .attachments
        .into_iter()
        .find(|attachment| attachment.part_index == part_index)
        .ok_or_else(|| anyhow::anyhow!("attachment part {part_index} is no longer available"))?;
    anyhow::ensure!(
        attachment.bytes.len() <= max_decoded_bytes,
        "decoded attachment part {part_index} is {} bytes; the decoded attachment limit is {max_decoded_bytes} bytes",
        attachment.bytes.len()
    );
    Ok(attachment.bytes)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        ffi::OsStr,
        fs,
        sync::{Barrier, mpsc::TryRecvError},
        time::Instant,
    };

    use super::*;

    fn bytes(value: &'static [u8]) -> Arc<[u8]> {
        Arc::from(value)
    }

    fn tree_contains_file(directory: &Path) -> bool {
        fs::read_dir(directory).ok().is_some_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                let path = entry.path();
                path.is_file() || (path.is_dir() && tree_contains_file(&path))
            })
        })
    }

    fn attachment_temporary_paths(directory: &Path) -> Vec<PathBuf> {
        fs::read_dir(directory)
            .expect("read attachment destination directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.starts_with(".notm-attachment-"))
            })
            .collect()
    }

    #[test]
    fn lazy_mime_part_source_shares_authoritative_path_remaps() {
        let source = MessageSource::new("/mail/cur/message:2,S".into(), 128);
        let attachment = AttachmentIoSource::mime_part(source.clone(), 3);
        let path_states = [notm_notmuch::MessagePathState {
            message_id: "message@example.test".to_string(),
            paths: vec!["/mail/cur/message:2,".into()],
            path_changes: vec![notm_notmuch::MaildirPathChange {
                previous_path: "/mail/cur/message:2,S".into(),
                current_path: "/mail/cur/message:2,".into(),
            }],
        }];

        let path_map = AuthoritativePathMap::new(&path_states);
        assert!(attachment.apply_authoritative_path_states("message@example.test", &path_map,));
        assert_eq!(source.path(), Path::new("/mail/cur/message:2,"));
    }

    #[test]
    fn slow_write_runs_off_the_calling_thread_and_delay_is_bounded() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut coordinator = AttachmentIoCoordinator::default();
        let token = coordinator.begin();
        let request = AttachmentIoRequest::save_to_directory(
            token,
            directory.path().to_path_buf(),
            "slow.txt".to_string(),
            bytes(b"slow attachment"),
        )
        .with_fixture_delay(Duration::from_millis(200));
        let started = Instant::now();

        let receiver = spawn(request);

        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        let response = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("receive delayed write response");
        assert!(started.elapsed() >= Duration::from_millis(150));
        assert!(coordinator.finish(response.token));
        assert!(response.result.is_ok());

        let capped = AttachmentIoRequest::save_to_directory(
            AttachmentIoToken {
                generation: 9,
                request_id: 10,
            },
            PathBuf::new(),
            "ignored".to_string(),
            bytes(b"ignored"),
        )
        .with_fixture_delay(Duration::from_secs(60));
        assert_eq!(capped.fixture_delay(), MAX_FIXTURE_DELAY);
    }

    #[test]
    fn stale_and_cancelled_completions_are_rejected() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut coordinator = AttachmentIoCoordinator::default();
        let slow_token = coordinator.begin();
        let slow = spawn(
            AttachmentIoRequest::save_to_directory(
                slow_token,
                directory.path().to_path_buf(),
                "slow.txt".to_string(),
                bytes(b"slow"),
            )
            .with_fixture_delay(Duration::from_millis(80)),
        );
        let current_token = coordinator.begin();
        let current = spawn(AttachmentIoRequest::save_to_directory(
            current_token,
            directory.path().to_path_buf(),
            "current.txt".to_string(),
            bytes(b"current"),
        ));

        let current_response = current
            .recv_timeout(Duration::from_secs(1))
            .expect("receive current response");
        assert!(coordinator.accepts(current_response.token));
        assert!(coordinator.finish(current_response.token));
        let stale_response = slow
            .recv_timeout(Duration::from_secs(1))
            .expect("receive stale response");
        assert!(!coordinator.accepts(stale_response.token));
        assert!(!coordinator.finish(stale_response.token));

        let cancelled_token = coordinator.begin();
        let cancelled = spawn(
            AttachmentIoRequest::prepare_open(
                cancelled_token,
                directory.path().to_path_buf(),
                "cancelled.txt".to_string(),
                bytes(b"cancelled"),
            )
            .with_fixture_delay(Duration::from_millis(40)),
        );
        coordinator.cancel();
        assert!(!coordinator.accepts(cancelled_token));
        let cancelled_response = cancelled
            .recv_timeout(Duration::from_secs(1))
            .expect("receive cancelled response");
        assert!(!coordinator.accepts(cancelled_response.token));
        assert!(!coordinator.finish(cancelled_response.token));
    }

    #[test]
    fn directory_and_explicit_target_saves_do_not_overwrite_collisions() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let original = directory.path().join("report.txt");
        fs::write(&original, b"keep original").expect("write original");
        let mut coordinator = AttachmentIoCoordinator::default();

        let directory_token = coordinator.begin();
        let directory_response = spawn(AttachmentIoRequest::save_to_directory(
            directory_token,
            directory.path().to_path_buf(),
            "report.txt".to_string(),
            bytes(b"directory save"),
        ))
        .recv_timeout(Duration::from_secs(1))
        .expect("receive directory save");
        assert!(coordinator.finish(directory_response.token));
        let directory_result = directory_response.result.expect("directory save result");
        assert_eq!(directory_result.action, AttachmentIoAction::SaveToDirectory);
        assert_eq!(
            directory_result.path,
            directory.path().join("report (1).txt")
        );

        let target_token = coordinator.begin();
        let target_response = spawn(AttachmentIoRequest::save_to_target(
            target_token,
            original.clone(),
            bytes(b"target save"),
        ))
        .recv_timeout(Duration::from_secs(1))
        .expect("receive target save");
        assert!(coordinator.finish(target_response.token));
        let target_result = target_response.result.expect("target save result");
        assert_eq!(target_result.action, AttachmentIoAction::SaveToTarget);
        assert_eq!(target_result.path, directory.path().join("report (2).txt"));

        assert_eq!(fs::read(original).expect("read original"), b"keep original");
        assert_eq!(
            fs::read(directory_result.path).expect("read directory save"),
            b"directory save"
        );
        assert_eq!(
            fs::read(target_result.path).expect("read target save"),
            b"target save"
        );
        assert!(
            attachment_temporary_paths(directory.path()).is_empty(),
            "successful atomic saves left temporary files behind"
        );
    }

    #[test]
    fn atomic_target_save_publishes_exact_complete_bytes() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("large.bin");
        let expected = (0..(2 * 1024 * 1024))
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();

        let saved =
            save_attachment_to_target_atomically_without_overwrite(&target, &expected, false)
                .expect("atomically save attachment");

        assert_eq!(saved, target);
        assert_eq!(fs::read(&saved).expect("read saved attachment"), expected);
        assert!(
            attachment_temporary_paths(directory.path()).is_empty(),
            "successful atomic save left a temporary file behind"
        );
    }

    #[test]
    fn concurrent_atomic_saves_publish_distinct_complete_files() {
        const SAVE_COUNT: usize = 12;

        let directory = tempfile::tempdir().expect("temporary directory");
        let target = Arc::new(directory.path().join("report.bin"));
        let barrier = Arc::new(Barrier::new(SAVE_COUNT));
        let handles = (0..SAVE_COUNT)
            .map(|index| {
                let target = Arc::clone(&target);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let expected = format!("attachment {index}").into_bytes();
                    barrier.wait();
                    let saved = save_attachment_to_target_atomically_without_overwrite(
                        &target, &expected, false,
                    )
                    .expect("atomically save concurrent attachment");
                    (saved, expected)
                })
            })
            .collect::<Vec<_>>();
        let saved = handles
            .into_iter()
            .map(|handle| handle.join().expect("attachment save thread"))
            .collect::<Vec<_>>();
        let paths = saved
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<BTreeSet<_>>();

        assert_eq!(paths.len(), SAVE_COUNT);
        for (path, expected) in saved {
            assert_eq!(fs::read(path).expect("read attachment"), expected);
        }
        assert!(
            attachment_temporary_paths(directory.path()).is_empty(),
            "concurrent atomic saves left temporary files behind"
        );
    }

    #[test]
    fn injected_pre_publish_failure_preserves_destination_and_cleans_temporary_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("report.bin");
        fs::write(&target, b"keep prior destination").expect("write prior destination");
        let mut coordinator = AttachmentIoCoordinator::default();
        let token = coordinator.begin();

        let response = spawn(
            AttachmentIoRequest::save_to_target(
                token,
                target.clone(),
                bytes(b"fully written replacement bytes"),
            )
            .with_fixture_fail_before_publish(true),
        )
        .recv_timeout(Duration::from_secs(1))
        .expect("receive injected write failure");

        assert!(coordinator.finish(response.token));
        let error = response.result.expect_err("write must fail before publish");
        assert_eq!(error.action(), AttachmentIoAction::SaveToTarget);
        assert!(error.to_string().contains("failure before atomic publish"));
        assert_eq!(
            fs::read(&target).expect("read prior destination"),
            b"keep prior destination"
        );
        assert!(!directory.path().join("report (1).bin").exists());
        assert!(
            attachment_temporary_paths(directory.path()).is_empty(),
            "failed atomic save left a temporary file behind"
        );

        let absent_target = directory.path().join("absent.bin");
        let error = save_attachment_to_target_atomically_without_overwrite(
            &absent_target,
            b"fully written unpublished bytes",
            true,
        )
        .expect_err("injected failure must not publish a new destination");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(!absent_target.exists());
        assert!(
            attachment_temporary_paths(directory.path()).is_empty(),
            "failed new save left a temporary file behind"
        );
    }

    #[test]
    fn prepare_open_returns_a_private_store_path_without_launching() {
        let directory = tempfile::tempdir().expect("private open directory");
        let mut coordinator = AttachmentIoCoordinator::default();
        let token = coordinator.begin();

        let response = spawn(AttachmentIoRequest::prepare_open(
            token,
            directory.path().to_path_buf(),
            "preview.bin".to_string(),
            bytes(b"preview"),
        ))
        .recv_timeout(Duration::from_secs(1))
        .expect("receive prepare-open response");

        assert!(coordinator.finish(response.token));
        let completed = response.result.expect("prepare-open result");
        assert_eq!(completed.action, AttachmentIoAction::PrepareOpen);
        assert_eq!(completed.path, directory.path().join("preview.bin"));
        assert_eq!(fs::read(completed.path).expect("read preview"), b"preview");
    }

    #[test]
    fn requested_mime_part_is_read_and_extracted_only_in_the_io_worker() {
        let directory = tempfile::tempdir().expect("attachment fixture directory");
        let message = directory.path().join("message.eml");
        let raw = b"MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=x\r\n\r\n\
--x\r\nContent-Disposition: attachment; filename=first.txt\r\n\r\nfirst\r\n\
--x\r\nContent-Disposition: attachment; filename=second.txt\r\n\r\nsecond requested\r\n\
--x--\r\n";
        fs::write(&message, raw).expect("write MIME fixture");
        let source = MessageSource::new(message, raw.len());
        let target = directory.path().join("saved.txt");
        let mut coordinator = AttachmentIoCoordinator::default();
        let token = coordinator.begin();

        let response = spawn(AttachmentIoRequest::save_to_target(
            token,
            target.clone(),
            AttachmentIoSource::mime_part(source, 1),
        ))
        .recv_timeout(Duration::from_secs(1))
        .expect("receive lazy MIME extraction");

        assert!(coordinator.finish(response.token));
        response.result.expect("save requested MIME part");
        assert_eq!(
            fs::read(target).expect("read saved part"),
            b"second requested"
        );
    }

    #[test]
    fn lazy_mime_extraction_enforces_source_and_decoded_size_limits() {
        let directory = tempfile::tempdir().expect("attachment fixture directory");
        let message = directory.path().join("message.eml");
        let raw = b"Content-Disposition: attachment; filename=large.txt\r\n\r\n0123456789";
        fs::write(&message, raw).expect("write MIME fixture");
        let source = MessageSource::new(message.clone(), raw.len());

        let source_error = extract_requested_part_with_limits(&source, 0, 16, 1024)
            .expect_err("source limit must reject before extraction");
        assert!(
            source_error
                .to_string()
                .contains("attachment source limit is 16")
        );

        let decoded_error = extract_requested_part_with_limits(&source, 0, raw.len(), 4)
            .expect_err("decoded size limit must reject the requested part");
        assert!(
            decoded_error
                .to_string()
                .contains("decoded attachment limit is 4 bytes")
        );
    }

    #[test]
    fn write_failure_preserves_typed_operation_context() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let non_directory = directory.path().join("not-a-directory");
        fs::write(&non_directory, b"block parent creation").expect("write blocker");
        let target = non_directory.join("attachment.bin");
        let mut coordinator = AttachmentIoCoordinator::default();
        let token = coordinator.begin();

        let response = spawn(AttachmentIoRequest::save_to_target(
            token,
            target.clone(),
            bytes(b"attachment"),
        ))
        .recv_timeout(Duration::from_secs(1))
        .expect("receive failed write");

        assert!(coordinator.finish(response.token));
        let error = response.result.expect_err("write must fail");
        assert_eq!(error.action(), AttachmentIoAction::SaveToTarget);
        match &error {
            AttachmentIoError::SaveToTarget {
                target: failed_target,
                ..
            } => assert_eq!(failed_target, &target),
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(format!("{error}").contains(&target.display().to_string()));
    }

    #[test]
    fn composer_cache_moves_shared_bytes_to_a_private_request_directory() {
        let directory = tempfile::tempdir().expect("composer cache directory");
        let service = ComposerAttachmentCacheService::default();
        let source_bytes = bytes(b"shared composer attachment");
        let retained = source_bytes.clone();
        let cache_root = directory.path().join("cache");
        let request = ComposerAttachmentCacheRequest::new(
            41,
            vec![ComposerAttachmentSource::shared(
                "forwarded.eml".to_string(),
                source_bytes,
            )],
            cache_root.clone(),
        )
        .with_fixture_delay(Duration::from_millis(80));

        let receiver = service.submit(request);
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        assert_eq!(retained.as_ref(), b"shared composer attachment");
        let response = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("receive composer cache response");

        assert_eq!(response.generation, 41);
        let paths = response.result.expect("cache composer attachment").commit();
        assert_eq!(paths.len(), 1);
        let path = Path::new(&paths[0]);
        assert_eq!(
            path.file_name().and_then(OsStr::to_str),
            Some("forwarded.eml")
        );
        let source_directory = path.parent().expect("source cache directory");
        let request_directory = source_directory.parent().expect("request cache directory");
        assert_eq!(request_directory.parent(), Some(cache_root.as_path()));
        Uuid::parse_str(
            request_directory
                .file_name()
                .and_then(OsStr::to_str)
                .expect("UTF-8 request cache directory"),
        )
        .expect("UUID request cache directory");
        assert_eq!(
            fs::read(path).expect("read cached composer attachment"),
            retained.as_ref()
        );
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&cache_root)
                    .expect("cache root metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(request_directory)
                    .expect("request cache directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(source_directory)
                    .expect("source cache directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(path)
                    .expect("cached attachment metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn composer_cache_preserves_duplicate_sanitized_basenames_in_source_directories() {
        let directory = tempfile::tempdir().expect("composer cache directory");
        let cache_root = directory.path().join("cache");
        let service = ComposerAttachmentCacheService::default();
        let response = service.submit(ComposerAttachmentCacheRequest::new(
            42,
            vec![
                ComposerAttachmentSource::shared("report.txt".to_string(), bytes(b"first")),
                ComposerAttachmentSource::shared("report.txt".to_string(), bytes(b"second")),
                ComposerAttachmentSource::shared(
                    "folder/report.txt".to_string(),
                    bytes(b"sanitized"),
                ),
            ],
            cache_root.clone(),
        ));

        let lease = response
            .recv_timeout(Duration::from_secs(1))
            .expect("duplicate basename response")
            .result
            .expect("cache duplicate basenames");
        let paths = lease.paths().iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            paths
                .iter()
                .map(|path| path.file_name().and_then(OsStr::to_str))
                .collect::<Vec<_>>(),
            vec![
                Some("report.txt"),
                Some("report.txt"),
                Some("folder_report.txt")
            ]
        );
        assert_ne!(paths[0].parent(), paths[1].parent());
        assert_ne!(paths[0].parent(), paths[2].parent());
        let request_directory = paths[0]
            .parent()
            .and_then(Path::parent)
            .expect("request cache directory");
        assert_eq!(request_directory.parent(), Some(cache_root.as_path()));
        assert!(
            paths
                .iter()
                .all(|path| { path.parent().and_then(Path::parent) == Some(request_directory) })
        );
        assert_eq!(fs::read(&paths[0]).expect("read first duplicate"), b"first");
        assert_eq!(
            fs::read(&paths[1]).expect("read second duplicate"),
            b"second"
        );
        assert_eq!(
            fs::read(&paths[2]).expect("read sanitized basename"),
            b"sanitized"
        );
        let committed = lease.commit();
        assert_eq!(committed.len(), 3);
    }

    #[test]
    fn forwarded_message_source_is_bounded_and_read_lazily_in_cache_worker() {
        let directory = tempfile::tempdir().expect("composer cache directory");
        let service = ComposerAttachmentCacheService::default();
        let source_path = directory.path().join("source.eml");
        fs::write(&source_path, b"first").expect("write initial source");
        let source = MessageSource::new(source_path.clone(), 5);
        let request = ComposerAttachmentCacheRequest::new(
            51,
            vec![ComposerAttachmentSource::message_file(
                "forwarded.eml".to_string(),
                source,
            )],
            directory.path().join("cache"),
        )
        .with_fixture_delay(Duration::from_millis(80));

        let receiver = service.submit(request);
        fs::write(source_path, b"later").expect("replace source before worker read");
        let response = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("receive forwarded source response");

        let paths = response.result.expect("cache forwarded source").commit();
        assert_eq!(fs::read(&paths[0]).expect("read cached source"), b"later");
    }

    #[test]
    fn composer_cache_reuses_an_existing_owned_source_without_rewriting() {
        let directory = tempfile::tempdir().expect("composer cache directory");
        let service = ComposerAttachmentCacheService::default();
        let existing = directory.path().join("existing.txt");
        fs::write(&existing, b"existing attachment").expect("write existing attachment");
        let request = ComposerAttachmentCacheRequest::new(
            7,
            vec![ComposerAttachmentSource::from_input(AttachmentInput {
                filename: "ignored.txt".to_string(),
                content_type: "text/plain".to_string(),
                bytes: b"replacement bytes".to_vec(),
                source_path: Some(existing.clone()),
            })],
            directory.path().join("unused-cache"),
        );

        let response = service
            .submit(request)
            .recv_timeout(Duration::from_secs(1))
            .expect("receive composer source response");

        assert_eq!(
            response.result.expect("reuse source").commit(),
            vec![existing.display().to_string()]
        );
        assert_eq!(
            fs::read(existing).expect("read existing source"),
            b"existing attachment"
        );
        assert!(!directory.path().join("unused-cache").exists());
    }

    #[test]
    fn composer_cache_reports_atomic_write_failure_without_a_partial_attachment() {
        let directory = tempfile::tempdir().expect("composer cache directory");
        let service = ComposerAttachmentCacheService::default();
        let blocker = directory.path().join("not-a-directory");
        fs::write(&blocker, b"block cache directory creation").expect("write blocker");
        let cache_directory = blocker.join("cache");
        let request = ComposerAttachmentCacheRequest::new(
            12,
            vec![ComposerAttachmentSource::shared(
                "failed.bin".to_string(),
                bytes(b"must not be partially cached"),
            )],
            cache_directory.clone(),
        );

        let response = service
            .submit(request)
            .recv_timeout(Duration::from_secs(1))
            .expect("receive composer cache failure");

        assert_eq!(response.generation, 12);
        let error = response.result.expect_err("cache write must fail");
        assert_eq!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::NotADirectory),
            "unexpected cache failure: {error:#}"
        );
        assert!(!cache_directory.exists());
    }

    #[test]
    fn composer_cache_service_serializes_and_coalesces_to_the_newest_request() {
        let directory = tempfile::tempdir().expect("composer cache directory");
        let cache_directory = directory.path().join("cache");
        let service = ComposerAttachmentCacheService::default();
        let gate = Arc::new(ComposerAttachmentCacheFixtureGate::default());
        let slow = service.submit(
            ComposerAttachmentCacheRequest::new(
                1,
                vec![ComposerAttachmentSource::shared(
                    "slow.txt".to_string(),
                    bytes(b"slow"),
                )],
                cache_directory.clone(),
            )
            .with_fixture_gate(gate.clone()),
        );
        assert!(
            gate.wait_until_entered(Duration::from_secs(1)),
            "slow cache did not enter its fixture gate"
        );

        let middle = service.submit(ComposerAttachmentCacheRequest::new(
            2,
            vec![ComposerAttachmentSource::shared(
                "middle.txt".to_string(),
                bytes(b"middle"),
            )],
            cache_directory.clone(),
        ));
        let latest = service.submit(ComposerAttachmentCacheRequest::new(
            3,
            vec![ComposerAttachmentSource::shared(
                "latest.txt".to_string(),
                bytes(b"latest"),
            )],
            cache_directory.clone(),
        ));
        let pending_snapshot = service.snapshot();
        assert_eq!(pending_snapshot.active_generation, Some(3));
        assert_eq!(pending_snapshot.pending_generation, Some(3));
        assert_eq!(pending_snapshot.pending_requests, 1);
        assert_eq!(pending_snapshot.peak_pending_requests, 1);
        assert_eq!(pending_snapshot.active_preparations, 1);
        assert_eq!(pending_snapshot.peak_active_preparations, 1);
        assert_eq!(pending_snapshot.submitted, 3);
        assert_eq!(pending_snapshot.cancelled, 2);
        assert_eq!(pending_snapshot.coalesced, 1);
        assert!(
            matches!(
                middle.recv_timeout(Duration::from_secs(1)),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ),
            "coalesced middle request unexpectedly executed"
        );

        gate.release();

        let slow_response = slow
            .recv_timeout(Duration::from_secs(1))
            .expect("cancelled slow response");
        assert_eq!(slow_response.generation, 1);
        assert!(
            slow_response
                .result
                .expect_err("slow cache must be cancelled")
                .to_string()
                .contains("cancelled")
        );
        let latest_response = latest
            .recv_timeout(Duration::from_secs(1))
            .expect("latest cache response");
        assert_eq!(latest_response.generation, 3);
        let latest_paths = latest_response
            .result
            .expect("latest cache succeeds")
            .commit();
        assert_eq!(
            fs::read(&latest_paths[0]).expect("read latest cache"),
            b"latest"
        );

        let snapshot = service.snapshot();
        assert_eq!(snapshot.active_generation, None);
        assert_eq!(snapshot.pending_generation, None);
        assert_eq!(snapshot.latest_generation, 3);
        assert_eq!(snapshot.pending_requests, 0);
        assert_eq!(snapshot.peak_pending_requests, 1);
        assert_eq!(snapshot.active_preparations, 0);
        assert_eq!(snapshot.peak_active_preparations, 1);
        assert_eq!(snapshot.submitted, 3);
        assert_eq!(snapshot.cancelled, 2);
        assert_eq!(snapshot.coalesced, 1);
        let cached = fs::read_dir(&cache_directory)
            .expect("read cache directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect cache entries");
        assert_eq!(cached.len(), 1, "stale cache files survived: {cached:?}");
    }

    #[test]
    fn composer_cache_cancellation_removes_files_created_before_a_slow_step() {
        let directory = tempfile::tempdir().expect("composer cache directory");
        let cache_directory = directory.path().join("cache");
        let service = ComposerAttachmentCacheService::default();
        let response = service.submit(
            ComposerAttachmentCacheRequest::new(
                11,
                vec![
                    ComposerAttachmentSource::shared("first.txt".to_string(), bytes(b"first")),
                    ComposerAttachmentSource::shared("second.txt".to_string(), bytes(b"second")),
                ],
                cache_directory.clone(),
            )
            .with_fixture_step_delay(Duration::from_secs(2)),
        );
        let file_deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if tree_contains_file(&cache_directory) {
                break;
            }
            assert!(
                Instant::now() < file_deadline,
                "first cache file was not created"
            );
            thread::sleep(Duration::from_millis(2));
        }

        let cancel_started = Instant::now();
        service.cancel();
        let response = response
            .recv_timeout(Duration::from_secs(1))
            .expect("cancelled cache response");
        assert!(cancel_started.elapsed() < Duration::from_millis(500));
        assert!(
            response
                .result
                .expect_err("cache must be cancelled")
                .to_string()
                .contains("cancelled")
        );
        assert_eq!(
            fs::read_dir(&cache_directory)
                .expect("read cache directory")
                .count(),
            0,
            "cancelled request left a cache file"
        );
    }

    #[test]
    fn composer_cache_replacement_removes_the_superseded_request_directory() {
        let directory = tempfile::tempdir().expect("composer cache directory");
        let cache_directory = directory.path().join("cache");
        let service = ComposerAttachmentCacheService::default();
        let superseded = service.submit(
            ComposerAttachmentCacheRequest::new(
                18,
                vec![ComposerAttachmentSource::shared(
                    "superseded.txt".to_string(),
                    bytes(b"superseded"),
                )],
                cache_directory.clone(),
            )
            .with_fixture_step_delay(Duration::from_secs(2)),
        );
        let file_deadline = Instant::now() + Duration::from_secs(1);
        while !tree_contains_file(&cache_directory) {
            assert!(
                Instant::now() < file_deadline,
                "superseded cache file was not created"
            );
            thread::sleep(Duration::from_millis(2));
        }
        let superseded_directory = fs::read_dir(&cache_directory)
            .expect("read cache root")
            .next()
            .expect("superseded request directory")
            .expect("read superseded request directory")
            .path();

        let replacement = service.submit(ComposerAttachmentCacheRequest::new(
            19,
            vec![ComposerAttachmentSource::shared(
                "replacement.txt".to_string(),
                bytes(b"replacement"),
            )],
            cache_directory.clone(),
        ));
        let superseded = superseded
            .recv_timeout(Duration::from_secs(1))
            .expect("superseded cache response");
        assert!(
            superseded
                .result
                .expect_err("old request must be cancelled")
                .to_string()
                .contains("cancelled")
        );
        let replacement_path = replacement
            .recv_timeout(Duration::from_secs(1))
            .expect("replacement cache response")
            .result
            .expect("replacement cache succeeds")
            .commit()
            .remove(0);

        assert!(
            !superseded_directory.exists(),
            "superseded request directory survived replacement"
        );
        assert_eq!(
            fs::read(replacement_path).expect("read replacement cache"),
            b"replacement"
        );
        assert_eq!(
            fs::read_dir(cache_directory)
                .expect("read cache root after replacement")
                .count(),
            1,
            "replacement left more than its committed request directory"
        );
    }

    #[test]
    fn composer_cache_error_and_uncommitted_lease_remove_only_created_files() {
        let directory = tempfile::tempdir().expect("composer cache directory");
        let cache_directory = directory.path().join("cache");
        let missing = directory.path().join("missing-message.eml");
        let service = ComposerAttachmentCacheService::default();
        let partial = service.submit(ComposerAttachmentCacheRequest::new(
            21,
            vec![
                ComposerAttachmentSource::shared("created.txt".to_string(), bytes(b"created")),
                ComposerAttachmentSource::message_file(
                    "missing.eml".to_string(),
                    MessageSource::new(missing, 16),
                ),
            ],
            cache_directory.clone(),
        ));
        let partial = partial
            .recv_timeout(Duration::from_secs(1))
            .expect("partial cache response");
        assert!(partial.result.is_err());
        assert_eq!(
            fs::read_dir(&cache_directory)
                .expect("read cache directory")
                .count(),
            0,
            "failed request left its first cache file"
        );

        let existing = directory.path().join("existing.txt");
        fs::write(&existing, b"existing").expect("write existing source");
        let stale = service.submit(ComposerAttachmentCacheRequest::new(
            22,
            vec![
                ComposerAttachmentSource::shared("stale.txt".to_string(), bytes(b"stale")),
                ComposerAttachmentSource::from_input(AttachmentInput {
                    filename: "existing.txt".to_string(),
                    content_type: "text/plain".to_string(),
                    bytes: b"unused".to_vec(),
                    source_path: Some(existing.clone()),
                }),
            ],
            cache_directory.clone(),
        ));
        let lease = stale
            .recv_timeout(Duration::from_secs(1))
            .expect("stale cache response")
            .result
            .expect("cache stale replacement");
        let created = PathBuf::from(&lease.paths()[0]);
        let source_directory = created
            .parent()
            .expect("request-owned source cache directory")
            .to_path_buf();
        let request_directory = source_directory
            .parent()
            .expect("request-owned cache directory")
            .to_path_buf();
        assert!(created.is_file());
        assert_eq!(lease.paths()[1], existing.display().to_string());
        drop(lease);
        assert!(!created.exists(), "uncommitted cache file survived");
        assert!(
            !source_directory.exists(),
            "uncommitted source cache directory survived"
        );
        assert!(
            !request_directory.exists(),
            "uncommitted request cache directory survived"
        );
        assert_eq!(
            fs::read(existing).expect("read existing source"),
            b"existing"
        );
    }

    #[test]
    fn composer_cache_enforces_actual_loaded_byte_budget_and_removes_prior_files() {
        let directory = tempfile::tempdir().expect("composer cache directory");
        let cache_directory = directory.path().join("cache");
        let first = directory.path().join("first.eml");
        let second = directory.path().join("second.eml");
        fs::write(&first, b"first").expect("write first source");
        fs::write(&second, b"later").expect("write second source");
        let request = ComposerAttachmentCacheRequest::new(
            30,
            vec![
                ComposerAttachmentSource::message_file(
                    "first.eml".to_string(),
                    MessageSource::new(first, 1),
                ),
                ComposerAttachmentSource::message_file(
                    "second.eml".to_string(),
                    MessageSource::new(second, 1),
                ),
            ],
            cache_directory.clone(),
        );

        let error =
            cache_composer_attachments_cancellable_with_limit(request, &AtomicBool::new(false), 8)
                .expect_err("actual source bytes must remain bounded");

        assert!(error.to_string().contains("loaded 10 bytes"), "{error:#}");
        assert_eq!(
            fs::read_dir(&cache_directory)
                .expect("read cache directory")
                .count(),
            0,
            "loaded-byte rejection left a prior cache file"
        );
    }

    #[test]
    fn composer_cache_clone_keeps_worker_alive_and_request_count_is_bounded() {
        let directory = tempfile::tempdir().expect("composer cache directory");
        let service = ComposerAttachmentCacheService::default();
        let clone = service.clone();
        drop(service);
        let response = clone.submit(ComposerAttachmentCacheRequest::new(
            31,
            vec![ComposerAttachmentSource::shared(
                "clone.txt".to_string(),
                bytes(b"clone"),
            )],
            directory.path().join("cache"),
        ));
        let paths = response
            .recv_timeout(Duration::from_secs(1))
            .expect("clone-backed response")
            .result
            .expect("clone-backed cache")
            .commit();
        assert_eq!(fs::read(&paths[0]).expect("read clone cache"), b"clone");

        let too_many = (0..=MAX_COMPOSER_CACHE_SOURCES)
            .map(|index| ComposerAttachmentSource::shared(format!("{index}.txt"), bytes(b"x")))
            .collect();
        let response = clone.submit(ComposerAttachmentCacheRequest::new(
            32,
            too_many,
            directory.path().join("bounded-cache"),
        ));
        let error = response
            .recv_timeout(Duration::from_secs(1))
            .expect("bounded response")
            .result
            .expect_err("source count must be bounded");
        assert!(error.to_string().contains("limit is 256"), "{error:#}");
        assert!(!directory.path().join("bounded-cache").exists());
    }
}
