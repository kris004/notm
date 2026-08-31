use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error as StdError,
    fmt, fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use mailparse::{MailHeaderMap as _, parse_content_disposition, parse_content_type, parse_headers};
use notm_mail::{
    ParsedMessage,
    html_sanitize::sanitize_html_with_cid_images,
    mime::{MimeLimits, extract_attachment_parts_detailed_with_limits, parse_rfc5322},
};
use notm_notmuch::{Database, DatabaseMode, MessagePathState, MessageSummary, OpenConfig};
use regex::Regex;

use crate::model::MAX_LOADED_THREAD_MESSAGES;

const DEFAULT_PREPARATION_LIMITS: PreparationLimits = PreparationLimits {
    message_count: MAX_LOADED_THREAD_MESSAGES,
    attachment_count: 2_048,
    mime_part_count: 4_096,
    source_bytes: 32 * 1024 * 1024,
    retained_bytes: 96 * 1024 * 1024,
    html_bytes: 4 * 1024 * 1024,
    raw_bytes: MAX_RAW_VIEW_BYTES,
    header_bytes: MAX_HEADER_VIEW_BYTES,
};
const MAX_CANDIDATE_THREAD_IDS: usize = 2_048;
const MAX_MESSAGE_FILE_CANDIDATES: usize = 256;
const MAX_TEXT_VIEW_BYTES: usize = 4 * 1024 * 1024;
const MAX_RAW_VIEW_BYTES: usize = 4 * 1024 * 1024;
const MAX_HEADER_VIEW_BYTES: usize = 1024 * 1024;
const MAX_MIME_NESTING_DEPTH: usize = 64;
const MAX_INLINE_IMAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOTAL_INLINE_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_INLINE_IMAGE_REFERENCES: usize = 2_048;
const MAX_INLINE_IMAGE_HTML_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
struct PreparationLimits {
    message_count: usize,
    attachment_count: usize,
    mime_part_count: usize,
    source_bytes: usize,
    retained_bytes: usize,
    html_bytes: usize,
    raw_bytes: usize,
    header_bytes: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct MessageSource {
    path: Arc<Mutex<CurrentMessagePath>>,
    source_bytes: usize,
    resolver: Option<Arc<MessageSourceResolver>>,
}

#[derive(Debug)]
struct CurrentMessagePath {
    path: PathBuf,
    generation: u64,
}

#[derive(Debug)]
pub(crate) struct AuthoritativePathMap<'a> {
    states_by_message_id: BTreeMap<&'a str, Vec<&'a MessagePathState>>,
}

impl<'a> AuthoritativePathMap<'a> {
    pub(crate) fn new(path_states: &'a [MessagePathState]) -> Self {
        let mut states_by_message_id = BTreeMap::<_, Vec<_>>::new();
        for state in path_states {
            states_by_message_id
                .entry(state.message_id.as_str())
                .or_default()
                .push(state);
        }
        Self {
            states_by_message_id,
        }
    }

    pub(crate) fn apply_to_source(&self, message_id: &str, source: &MessageSource) -> bool {
        self.states_by_message_id
            .get(message_id)
            .is_none_or(|states| source.apply_matching_path_states(states))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageSourceResolver {
    config: OpenConfig,
    message_id: String,
}

impl MessageSource {
    pub(crate) fn new(path: PathBuf, source_bytes: usize) -> Self {
        Self {
            path: Arc::new(Mutex::new(CurrentMessagePath {
                path,
                generation: 0,
            })),
            source_bytes,
            resolver: None,
        }
    }

    fn with_resolver(mut self, config: &OpenConfig, message_id: &str) -> Self {
        self.resolver = Some(Arc::new(MessageSourceResolver {
            config: config.clone(),
            message_id: message_id.to_string(),
        }));
        self
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.path_snapshot().0
    }

    pub(crate) const fn source_bytes(&self) -> usize {
        self.source_bytes
    }

    pub(crate) fn read_bounded(&self, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
        self.read_bounded_with_path(max_bytes)
            .map(|(_, bytes)| bytes)
    }

    pub(crate) fn read_bounded_with_path(
        &self,
        max_bytes: usize,
    ) -> anyhow::Result<(PathBuf, Vec<u8>)> {
        // Do not hold the path mutex across filesystem or Notmuch I/O. If an
        // authoritative tag result remaps the shared source while a read is in
        // progress, discard that stale completion and retry the newer path.
        for _ in 0..8 {
            let (cached_path, generation) = self.path_snapshot();
            match read_bounded(&cached_path, max_bytes) {
                Ok(bytes) if self.path_generation() == generation => {
                    return Ok((cached_path, bytes));
                }
                Ok(_) => continue,
                Err(_) if self.path_generation() != generation => continue,
                Err(cached_error) => {
                    let Some(resolver) = self.resolver.as_deref() else {
                        return Err(cached_error);
                    };
                    let resolved = (|| -> anyhow::Result<(PathBuf, Vec<u8>)> {
                        let database = Database::open(&resolver.config, DatabaseMode::ReadOnly)?;
                        let source = database
                            .open_message_id_file(&resolver.message_id)
                            .map_err(anyhow::Error::from)?;
                        let (resolved_path, file) = source.into_parts();
                        let bytes = read_reader_bounded(file, max_bytes).with_context(|| {
                            format!(
                                "cached message path {} was unavailable ({cached_error}); resolving current file for {}",
                                cached_path.display(),
                                resolver.message_id
                            )
                        })?;
                        Ok((resolved_path, bytes))
                    })();
                    if self.path_generation() != generation {
                        continue;
                    }
                    let (resolved_path, bytes) = resolved?;
                    if !self.replace_path_if_generation(generation, resolved_path.clone()) {
                        continue;
                    }
                    return Ok((resolved_path, bytes));
                }
            }
        }
        anyhow::bail!("message source path changed repeatedly while it was being read")
    }

    /// Applies byte-preserving Maildir path changes for one message.
    ///
    /// Clones share the same path cell, so updating a retained prepared
    /// message also updates lazy attachment and composer-preparation sources.
    /// A matching final state that does not contain the retained path is
    /// unresolved; callers must keep path-based actions blocked until a fresh
    /// model replaces that retained state.
    #[cfg(test)]
    pub(crate) fn apply_authoritative_path_states(
        &self,
        message_id: &str,
        path_states: &[MessagePathState],
    ) -> bool {
        AuthoritativePathMap::new(path_states).apply_to_source(message_id, self)
    }

    fn apply_matching_path_states(&self, path_states: &[&MessagePathState]) -> bool {
        let mut current = self
            .path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original_path = current.path.clone();
        for state in path_states {
            if let Some(change) = state
                .path_changes
                .iter()
                .find(|change| change.previous_path.as_path() == current.path.as_path())
            {
                current.path.clone_from(&change.current_path);
            }
        }
        if current.path != original_path {
            current.generation = current.generation.saturating_add(1);
        }
        path_states
            .last()
            .is_none_or(|state| state.paths.contains(&current.path))
    }

    fn path_snapshot(&self) -> (PathBuf, u64) {
        let current = self
            .path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (current.path.clone(), current.generation)
    }

    fn path_generation(&self) -> u64 {
        self.path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation
    }

    fn replace_path_if_generation(&self, expected_generation: u64, path: PathBuf) -> bool {
        let mut current = self
            .path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.generation != expected_generation {
            return false;
        }
        if current.path != path {
            current.path = path;
            current.generation = current.generation.saturating_add(1);
        }
        true
    }

    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(self.path().as_os_str().len())
            .saturating_add(
                self.resolver
                    .as_deref()
                    .map(message_source_resolver_bytes)
                    .unwrap_or(0),
            )
    }
}

impl PartialEq for MessageSource {
    fn eq(&self, other: &Self) -> bool {
        self.source_bytes == other.source_bytes
            && self.resolver == other.resolver
            && (Arc::ptr_eq(&self.path, &other.path) || self.path() == other.path())
    }
}

impl Eq for MessageSource {}

fn message_source_resolver_bytes(resolver: &MessageSourceResolver) -> usize {
    resolver
        .message_id
        .len()
        .saturating_add(
            resolver
                .config
                .database_path
                .as_deref()
                .map(|path| path.as_os_str().len())
                .unwrap_or(0),
        )
        .saturating_add(
            resolver
                .config
                .config_path
                .as_deref()
                .map(|path| path.as_os_str().len())
                .unwrap_or(0),
        )
        .saturating_add(
            resolver
                .config
                .profile
                .as_deref()
                .map(str::len)
                .unwrap_or(0),
        )
}

#[derive(Debug, Clone)]
struct PreparedText {
    expanded: Arc<str>,
    collapsed: Arc<str>,
}

#[derive(Debug, Clone)]
enum PreparedHtml {
    Missing,
    Ready {
        original_len: usize,
        images_allowed: Arc<str>,
        images_blocked: Arc<str>,
    },
    Unavailable {
        original_len: usize,
        error: Arc<str>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedMessage {
    source: Option<MessageSource>,
    raw: Result<Arc<str>, Arc<str>>,
    parsed: Result<Arc<ParsedMessage>, Arc<str>>,
    headers: Result<Arc<str>, Arc<str>>,
    text: Result<PreparedText, Arc<str>>,
    html: PreparedHtml,
}

impl PreparedMessage {
    fn failed(error: Arc<str>, source: Option<MessageSource>) -> Self {
        Self {
            source,
            raw: Err(error.clone()),
            parsed: Err(error.clone()),
            headers: Err(error.clone()),
            text: Err(error.clone()),
            html: PreparedHtml::Unavailable {
                original_len: 0,
                error,
            },
        }
    }

    pub(crate) fn raw_shared(&self) -> anyhow::Result<Arc<str>> {
        let raw = self
            .raw
            .clone()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(
            raw.len() <= MAX_RAW_VIEW_BYTES,
            "raw source is {} bytes; the responsive text-view limit is {MAX_RAW_VIEW_BYTES} bytes",
            raw.len()
        );
        Ok(raw)
    }

    pub(crate) fn parsed(&self) -> anyhow::Result<&ParsedMessage> {
        self.parsed
            .as_deref()
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    pub(crate) fn source(&self) -> anyhow::Result<&MessageSource> {
        self.source
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("message has no source file"))
    }

    pub(crate) fn headers(&self) -> anyhow::Result<Arc<str>> {
        let headers = self
            .headers
            .clone()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(
            headers.len() <= MAX_HEADER_VIEW_BYTES,
            "message headers are {} bytes; the responsive text-view limit is {MAX_HEADER_VIEW_BYTES} bytes",
            headers.len()
        );
        Ok(headers)
    }

    pub(crate) fn rendered_text(&self, collapse_quotes: bool) -> anyhow::Result<Arc<str>> {
        let rendered = self
            .text
            .as_ref()
            .map(|text| {
                if collapse_quotes {
                    text.collapsed.clone()
                } else {
                    text.expanded.clone()
                }
            })
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        anyhow::ensure!(
            rendered.len() <= MAX_TEXT_VIEW_BYTES,
            "rendered text is {} bytes; the responsive text-view limit is {MAX_TEXT_VIEW_BYTES} bytes",
            rendered.len()
        );
        Ok(rendered)
    }

    pub(crate) fn has_html(&self) -> bool {
        matches!(
            self.html,
            PreparedHtml::Ready { .. }
                | PreparedHtml::Unavailable {
                    original_len: 1..,
                    ..
                }
        )
    }

    pub(crate) fn html_document(&self, allow_remote_images: bool) -> anyhow::Result<Arc<str>> {
        match &self.html {
            PreparedHtml::Missing => anyhow::bail!("selected message has no HTML body"),
            PreparedHtml::Ready {
                images_allowed,
                images_blocked,
                ..
            } => Ok(if allow_remote_images {
                images_allowed.clone()
            } else {
                images_blocked.clone()
            }),
            PreparedHtml::Unavailable { error, .. } => {
                anyhow::bail!(error.to_string())
            }
        }
    }

    pub(crate) fn html_original_len(&self) -> usize {
        match &self.html {
            PreparedHtml::Missing => 0,
            PreparedHtml::Ready { original_len, .. }
            | PreparedHtml::Unavailable { original_len, .. } => *original_len,
        }
    }

    pub(crate) fn html_render_error(&self) -> Option<&str> {
        match &self.html {
            PreparedHtml::Unavailable { error, .. } => Some(error),
            PreparedHtml::Missing | PreparedHtml::Ready { .. } => None,
        }
    }

    fn retained_bytes(&self) -> usize {
        self.source
            .as_ref()
            .map(MessageSource::retained_bytes)
            .unwrap_or(0)
            .saturating_add(result_arc_str_bytes(&self.raw))
            .saturating_add(result_parsed_message_bytes(&self.parsed))
            .saturating_add(result_arc_str_bytes(&self.headers))
            .saturating_add(match &self.text {
                Ok(text) => text.expanded.len().saturating_add(text.collapsed.len()),
                Err(error) => error.len(),
            })
            .saturating_add(match &self.html {
                PreparedHtml::Missing => 0,
                PreparedHtml::Ready {
                    images_allowed,
                    images_blocked,
                    ..
                } => {
                    let blocked = if Arc::ptr_eq(images_allowed, images_blocked) {
                        0
                    } else {
                        images_blocked.len()
                    };
                    images_allowed.len().saturating_add(blocked)
                }
                PreparedHtml::Unavailable { error, .. } => error.len(),
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedAttachment {
    pub(crate) message_index: usize,
    pub(crate) attachment_index: usize,
    pub(crate) message_id: String,
    pub(crate) filename: String,
    pub(crate) content_type: String,
    /// Decoded size reported by MIME preparation. The payload itself is loaded
    /// only when the attachment is requested.
    pub(crate) size: usize,
    pub(crate) source: MessageSource,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedThread {
    pub(crate) thread_id: String,
    pub(crate) messages: Vec<MessageSummary>,
    pub(crate) message_contents: BTreeMap<String, Arc<PreparedMessage>>,
    pub(crate) attachments: Vec<PreparedAttachment>,
    pub(crate) target_message_index: Option<usize>,
    retained_bytes: usize,
}

impl PreparedThread {
    pub(crate) const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub(crate) fn apply_authoritative_path_states(
        &self,
        path_map: &AuthoritativePathMap<'_>,
    ) -> bool {
        let mut resolved = true;
        for (message_id, message) in &self.message_contents {
            if let Some(source) = &message.source {
                resolved &= path_map.apply_to_source(message_id, source);
            }
        }
        for attachment in &self.attachments {
            resolved &= path_map.apply_to_source(&attachment.message_id, &attachment.source);
        }
        resolved
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ThreadLoadRequest {
    pub(crate) generation: u64,
    pub(crate) config: OpenConfig,
    pub(crate) thread_id: String,
    pub(crate) candidate_thread_ids: Vec<String>,
    pub(crate) target_message_id: Option<String>,
    pub(crate) delay: Duration,
}

#[derive(Debug)]
pub(crate) struct ThreadLoadResponse {
    pub(crate) generation: u64,
    pub(crate) result: anyhow::Result<PreparedThread>,
}

#[derive(Debug)]
pub(crate) struct TargetMessageNotFound {
    message_id: String,
}

impl TargetMessageNotFound {
    fn new(message_id: &str) -> Self {
        Self {
            message_id: message_id.to_string(),
        }
    }

    pub(crate) fn message_id(&self) -> &str {
        &self.message_id
    }
}

impl fmt::Display for TargetMessageNotFound {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "message id not found: {}", self.message_id)
    }
}

impl StdError for TargetMessageNotFound {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThreadLoaderSnapshot {
    pub(crate) active_preparations: usize,
    pub(crate) peak_active_preparations: usize,
    pub(crate) submitted: usize,
    pub(crate) cancelled: usize,
    pub(crate) coalesced: usize,
}

#[derive(Debug, Default)]
struct ThreadLoaderMetrics {
    active_preparations: AtomicUsize,
    peak_active_preparations: AtomicUsize,
    submitted: AtomicUsize,
    cancelled: AtomicUsize,
    coalesced: AtomicUsize,
}

impl ThreadLoaderMetrics {
    fn snapshot(&self) -> ThreadLoaderSnapshot {
        ThreadLoaderSnapshot {
            active_preparations: self.active_preparations.load(Ordering::Acquire),
            peak_active_preparations: self.peak_active_preparations.load(Ordering::Acquire),
            submitted: self.submitted.load(Ordering::Acquire),
            cancelled: self.cancelled.load(Ordering::Acquire),
            coalesced: self.coalesced.load(Ordering::Acquire),
        }
    }

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

type ThreadLoaderFn = dyn Fn(&ThreadLoadRequest, &AtomicBool) -> anyhow::Result<PreparedThread>
    + Send
    + Sync
    + 'static;

struct ThreadLoadJob {
    request: ThreadLoadRequest,
    cancelled: Arc<AtomicBool>,
    response: mpsc::Sender<ThreadLoadResponse>,
}

enum ThreadLoaderCommand {
    Load(ThreadLoadJob),
    Cancel,
}

#[derive(Clone)]
struct ThreadLoaderService {
    sender: Option<mpsc::Sender<ThreadLoaderCommand>>,
    start_error: Option<Arc<str>>,
    active_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    metrics: Arc<ThreadLoaderMetrics>,
}

impl std::fmt::Debug for ThreadLoaderService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ThreadLoaderService")
            .field("start_error", &self.start_error)
            .field("metrics", &self.metrics.snapshot())
            .finish_non_exhaustive()
    }
}

impl Default for ThreadLoaderService {
    fn default() -> Self {
        Self::new(Arc::new(load_thread))
    }
}

impl ThreadLoaderService {
    fn new(loader: Arc<ThreadLoaderFn>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let active_cancel = Arc::new(Mutex::new(None));
        let metrics = Arc::new(ThreadLoaderMetrics::default());
        let worker_cancel = active_cancel.clone();
        let worker_metrics = metrics.clone();
        match thread::Builder::new()
            .name("notm-thread-loader".to_string())
            .spawn(move || {
                thread_loader_worker(receiver, worker_cancel, worker_metrics, loader);
            }) {
            Ok(_) => Self {
                sender: Some(sender),
                start_error: None,
                active_cancel,
                metrics,
            },
            Err(error) => Self {
                sender: None,
                start_error: Some(Arc::from(error.to_string())),
                active_cancel,
                metrics,
            },
        }
    }

    fn submit(&self, request: ThreadLoadRequest) -> mpsc::Receiver<ThreadLoadResponse> {
        let (response, receiver) = mpsc::channel();
        self.metrics.submitted.fetch_add(1, Ordering::AcqRel);
        self.cancel_active();
        let generation = request.generation;
        let cancelled = Arc::new(AtomicBool::new(false));
        *self
            .active_cancel
            .lock()
            .expect("thread loader cancellation mutex poisoned") = Some(cancelled.clone());
        let job = ThreadLoadJob {
            request,
            cancelled,
            response: response.clone(),
        };
        if let Some(sender) = &self.sender
            && sender.send(ThreadLoaderCommand::Load(job)).is_ok()
        {
            return receiver;
        }
        self.cancel_active();
        let error = self
            .start_error
            .as_deref()
            .unwrap_or("thread loader worker disconnected");
        let _ = response.send(ThreadLoadResponse {
            generation,
            result: Err(anyhow::anyhow!(error.to_string())),
        });
        receiver
    }

    fn cancel(&self) {
        self.cancel_active();
        if let Some(sender) = &self.sender {
            let _ = sender.send(ThreadLoaderCommand::Cancel);
        }
    }

    fn cancel_active(&self) {
        if let Some(cancelled) = self
            .active_cancel
            .lock()
            .expect("thread loader cancellation mutex poisoned")
            .take()
            && !cancelled.swap(true, Ordering::AcqRel)
        {
            self.metrics.cancelled.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn snapshot(&self) -> ThreadLoaderSnapshot {
        self.metrics.snapshot()
    }
}

fn thread_loader_worker(
    receiver: mpsc::Receiver<ThreadLoaderCommand>,
    active_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    metrics: Arc<ThreadLoaderMetrics>,
    loader: Arc<ThreadLoaderFn>,
) {
    while let Ok(first) = receiver.recv() {
        let Some(job) = latest_thread_load_job(first, &receiver, &metrics) else {
            continue;
        };
        metrics.begin_preparation();
        let generation = job.request.generation;
        let result = loader(&job.request, &job.cancelled);
        metrics.finish_preparation();
        {
            let mut active = active_cancel
                .lock()
                .expect("thread loader cancellation mutex poisoned");
            if active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &job.cancelled))
            {
                *active = None;
            }
        }
        let _ = job.response.send(ThreadLoadResponse { generation, result });
    }
}

fn latest_thread_load_job(
    first: ThreadLoaderCommand,
    receiver: &mpsc::Receiver<ThreadLoaderCommand>,
    metrics: &ThreadLoaderMetrics,
) -> Option<ThreadLoadJob> {
    let mut latest = match first {
        ThreadLoaderCommand::Load(job) => Some(job),
        ThreadLoaderCommand::Cancel => None,
    };
    for command in receiver.try_iter() {
        match command {
            ThreadLoaderCommand::Load(job) => {
                if latest.replace(job).is_some() {
                    metrics.coalesced.fetch_add(1, Ordering::AcqRel);
                }
            }
            ThreadLoaderCommand::Cancel => {
                if latest.take().is_some() {
                    metrics.coalesced.fetch_add(1, Ordering::AcqRel);
                }
            }
        }
    }
    latest
}

#[derive(Debug, Default)]
pub(crate) struct ThreadLoadCoordinator {
    generation: u64,
    active: Option<u64>,
    loader: ThreadLoaderService,
}

impl ThreadLoadCoordinator {
    pub(crate) fn begin(&mut self) -> u64 {
        self.loader.cancel();
        self.generation = self.generation.saturating_add(1);
        self.active = Some(self.generation);
        self.generation
    }

    pub(crate) fn cancel(&mut self) {
        self.loader.cancel();
        self.generation = self.generation.saturating_add(1);
        self.active = None;
    }

    pub(crate) fn accepts(&self, generation: u64) -> bool {
        self.active == Some(generation)
    }

    pub(crate) fn active_generation(&self) -> Option<u64> {
        self.active
    }

    pub(crate) fn finish(&mut self, generation: u64) -> bool {
        if !self.accepts(generation) {
            return false;
        }
        self.active = None;
        true
    }

    pub(crate) fn spawn(&self, request: ThreadLoadRequest) -> mpsc::Receiver<ThreadLoadResponse> {
        self.loader.submit(request)
    }

    pub(crate) fn loader_snapshot(&self) -> ThreadLoaderSnapshot {
        self.loader.snapshot()
    }
}

fn load_thread(
    request: &ThreadLoadRequest,
    cancelled: &AtomicBool,
) -> anyhow::Result<PreparedThread> {
    load_thread_with_reader(request, cancelled, read_bounded)
}

fn load_thread_with_reader<F>(
    request: &ThreadLoadRequest,
    cancelled: &AtomicBool,
    read: F,
) -> anyhow::Result<PreparedThread>
where
    F: FnMut(&Path, usize) -> anyhow::Result<Vec<u8>>,
{
    sleep_cancellable(request.delay, cancelled)?;
    ensure_not_cancelled(cancelled)?;
    anyhow::ensure!(
        request.candidate_thread_ids.len() <= MAX_CANDIDATE_THREAD_IDS,
        "message lookup has {} candidate threads; the responsive lookup limit is {MAX_CANDIDATE_THREAD_IDS}",
        request.candidate_thread_ids.len()
    );
    let database = Database::open(&request.config, DatabaseMode::ReadOnly)?;
    ensure_not_cancelled(cancelled)?;
    let loaded_thread_id = if let Some(target) = request.target_message_id.as_deref() {
        let Some(message) = database.find_message(target)? else {
            return Err(TargetMessageNotFound::new(target).into());
        };
        if request.candidate_thread_ids.is_empty()
            || request
                .candidate_thread_ids
                .iter()
                .any(|candidate| candidate == &message.thread_id)
        {
            message.thread_id
        } else {
            // The target exists but is outside the currently loaded result
            // window. Prepare the existing selection so the UI can retain its
            // direct id-query fallback without conflating this with absence.
            request.thread_id.clone()
        }
    } else {
        request.thread_id.clone()
    };
    ensure_not_cancelled(cancelled)?;
    let messages = database
        .thread_messages_bounded(&loaded_thread_id, DEFAULT_PREPARATION_LIMITS.message_count)?;
    ensure_not_cancelled(cancelled)?;
    prepare_thread_with_resolution(
        loaded_thread_id,
        messages,
        request.target_message_id.as_deref(),
        DEFAULT_PREPARATION_LIMITS,
        Some(&request.config),
        read,
        || cancelled.load(Ordering::Acquire),
    )
}

fn ensure_not_cancelled(cancelled: &AtomicBool) -> anyhow::Result<()> {
    anyhow::ensure!(
        !cancelled.load(Ordering::Acquire),
        "thread preparation was cancelled"
    );
    Ok(())
}

fn sleep_cancellable(delay: Duration, cancelled: &AtomicBool) -> anyhow::Result<()> {
    let mut remaining = delay;
    while !remaining.is_zero() {
        ensure_not_cancelled(cancelled)?;
        let chunk = remaining.min(Duration::from_millis(10));
        thread::sleep(chunk);
        remaining = remaining.saturating_sub(chunk);
    }
    ensure_not_cancelled(cancelled)
}

fn read_bounded(path: &Path, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    read_reader_bounded(file, max_bytes)
}

fn read_reader_bounded(reader: impl Read, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX - 1)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(max_bytes.min(1024 * 1024));
    reader.take(limit).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= max_bytes,
        "message source exceeds the {max_bytes}-byte per-source preparation limit"
    );
    Ok(bytes)
}

#[cfg(test)]
fn prepare_thread<F>(
    thread_id: String,
    messages: Vec<MessageSummary>,
    target_message_id: Option<&str>,
    read: F,
) -> anyhow::Result<PreparedThread>
where
    F: FnMut(&Path, usize) -> anyhow::Result<Vec<u8>>,
{
    prepare_thread_with_limits(
        thread_id,
        messages,
        target_message_id,
        DEFAULT_PREPARATION_LIMITS,
        read,
    )
}

#[cfg(test)]
pub(crate) fn prepare_thread_from_summaries_for_test(
    thread_id: String,
    messages: Vec<MessageSummary>,
    resolver_config: Option<&OpenConfig>,
) -> anyhow::Result<PreparedThread> {
    prepare_thread_with_resolution(
        thread_id,
        messages,
        None,
        DEFAULT_PREPARATION_LIMITS,
        resolver_config,
        read_bounded,
        || false,
    )
}

#[cfg(test)]
fn prepare_thread_with_limits<F>(
    thread_id: String,
    messages: Vec<MessageSummary>,
    target_message_id: Option<&str>,
    limits: PreparationLimits,
    read: F,
) -> anyhow::Result<PreparedThread>
where
    F: FnMut(&Path, usize) -> anyhow::Result<Vec<u8>>,
{
    prepare_thread_with_cancel(thread_id, messages, target_message_id, limits, read, || {
        false
    })
}

#[cfg(test)]
fn prepare_thread_with_cancel<F, C>(
    thread_id: String,
    messages: Vec<MessageSummary>,
    target_message_id: Option<&str>,
    limits: PreparationLimits,
    read: F,
    cancelled: C,
) -> anyhow::Result<PreparedThread>
where
    F: FnMut(&Path, usize) -> anyhow::Result<Vec<u8>>,
    C: FnMut() -> bool,
{
    prepare_thread_with_resolution(
        thread_id,
        messages,
        target_message_id,
        limits,
        None,
        read,
        cancelled,
    )
}

fn prepare_thread_with_resolution<F, C>(
    thread_id: String,
    messages: Vec<MessageSummary>,
    target_message_id: Option<&str>,
    limits: PreparationLimits,
    resolver_config: Option<&OpenConfig>,
    mut read: F,
    mut cancelled: C,
) -> anyhow::Result<PreparedThread>
where
    F: FnMut(&Path, usize) -> anyhow::Result<Vec<u8>>,
    C: FnMut() -> bool,
{
    anyhow::ensure!(!cancelled(), "thread preparation was cancelled");
    anyhow::ensure!(
        messages.len() <= limits.message_count,
        "thread has {} messages; prepared-content limit is {}",
        messages.len(),
        limits.message_count
    );
    let target_message_index = target_message_id.and_then(|target| {
        messages
            .iter()
            .position(|message| message.message_id == target)
    });
    let mut message_contents = BTreeMap::new();
    let mut attachments = Vec::new();
    let mut attachment_like_count = 0_usize;
    let mut mime_part_count = 0_usize;
    let mut retained_bytes = thread_id
        .len()
        .saturating_add(messages.iter().map(message_summary_bytes).sum::<usize>());
    anyhow::ensure!(
        retained_bytes <= limits.retained_bytes,
        "thread metadata needs {retained_bytes} bytes; prepared-content limit is {} bytes",
        limits.retained_bytes
    );

    for (message_index, message) in messages.iter().enumerate() {
        anyhow::ensure!(!cancelled(), "thread preparation was cancelled");
        let prepared = prepare_message_candidates(
            message,
            resolver_config,
            &mut read,
            limits,
            limits
                .attachment_count
                .saturating_sub(attachment_like_count),
            limits.mime_part_count.saturating_sub(mime_part_count),
            &mut cancelled,
        )?;
        let prepared_bytes = prepared
            .retained_bytes()
            .saturating_add(message.message_id.len())
            .saturating_add(
                prepared
                    .attachments
                    .len()
                    .saturating_mul(message.message_id.len()),
            );
        let next_retained_bytes = retained_bytes.saturating_add(prepared_bytes);
        anyhow::ensure!(
            next_retained_bytes <= limits.retained_bytes,
            "thread detail needs at least {next_retained_bytes} bytes; prepared-content limit is {} bytes",
            limits.retained_bytes
        );
        retained_bytes = next_retained_bytes;
        let next_attachment_count = attachments.len().saturating_add(prepared.attachments.len());
        anyhow::ensure!(
            next_attachment_count <= limits.attachment_count,
            "thread has at least {next_attachment_count} attachments; prepared attachment-count limit is {}",
            limits.attachment_count
        );
        attachment_like_count =
            attachment_like_count.saturating_add(prepared.attachment_like_count);
        debug_assert!(attachment_like_count <= limits.attachment_count);
        mime_part_count = mime_part_count.saturating_add(prepared.mime_part_count);
        debug_assert!(mime_part_count <= limits.mime_part_count);
        for attachment in prepared.attachments {
            attachments.push(PreparedAttachment {
                message_index,
                attachment_index: attachment.part_index,
                message_id: message.message_id.clone(),
                filename: attachment.filename,
                content_type: attachment.content_type,
                size: attachment.size,
                source: prepared.message.source()?.clone(),
            });
        }
        message_contents.insert(message.message_id.clone(), Arc::new(prepared.message));
        anyhow::ensure!(!cancelled(), "thread preparation was cancelled");
    }

    Ok(PreparedThread {
        thread_id,
        messages,
        message_contents,
        attachments,
        target_message_index,
        retained_bytes,
    })
}

fn prepare_message_candidates<F, C>(
    summary: &MessageSummary,
    resolver_config: Option<&OpenConfig>,
    read: &mut F,
    limits: PreparationLimits,
    remaining_attachment_count: usize,
    remaining_mime_part_count: usize,
    cancelled: &mut C,
) -> anyhow::Result<PreparedFile>
where
    F: FnMut(&Path, usize) -> anyhow::Result<Vec<u8>>,
    C: FnMut() -> bool,
{
    anyhow::ensure!(
        summary.filenames.len() <= MAX_MESSAGE_FILE_CANDIDATES,
        "message {} has {} indexed file candidates; responsive candidate limit is {MAX_MESSAGE_FILE_CANDIDATES}",
        summary.message_id,
        summary.filenames.len()
    );
    let mut candidates = summary
        .filenames
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    let mut failures = Vec::new();
    for path in &candidates {
        anyhow::ensure!(!cancelled(), "thread preparation was cancelled");
        match read(path, limits.source_bytes) {
            Ok(bytes) => {
                let mut prepared = prepare_message_bytes(
                    path,
                    bytes,
                    limits,
                    remaining_attachment_count,
                    remaining_mime_part_count,
                    cancelled,
                )?;
                attach_message_source_resolver(&mut prepared, resolver_config, &summary.message_id);
                return Ok(prepared);
            }
            Err(error) => {
                if failures.len() < 8 {
                    failures.push(format!("{}: {error}", path.display()));
                }
            }
        }
    }

    if let Some(config) = resolver_config {
        anyhow::ensure!(!cancelled(), "thread preparation was cancelled");
        let refreshed = (|| -> anyhow::Result<(PathBuf, Vec<u8>)> {
            let database = Database::open(config, DatabaseMode::ReadOnly)?;
            let source = database.open_message_id_file(&summary.message_id)?;
            let (path, file) = source.into_parts();
            let bytes = read_reader_bounded(file, limits.source_bytes)?;
            Ok((path, bytes))
        })();
        match refreshed {
            Ok((path, bytes)) => {
                let mut prepared = prepare_message_bytes(
                    &path,
                    bytes,
                    limits,
                    remaining_attachment_count,
                    remaining_mime_part_count,
                    cancelled,
                )?;
                attach_message_source_resolver(&mut prepared, resolver_config, &summary.message_id);
                return Ok(prepared);
            }
            Err(error) => {
                if failures.len() < 8 {
                    failures.push(format!("current Notmuch lookup: {error:#}"));
                }
            }
        }
    }

    let omitted = candidates.len().saturating_sub(failures.len());
    let omitted = (omitted > 0).then(|| format!("; {omitted} more candidate(s) failed"));
    let error = Arc::<str>::from(format!(
        "message {} has no readable indexed file: {}{}",
        summary.message_id,
        failures.join("; "),
        omitted.as_deref().unwrap_or_default()
    ));
    let source = candidates.first().cloned().map(|path| {
        let source = MessageSource::new(path, 0);
        if let Some(config) = resolver_config {
            source.with_resolver(config, &summary.message_id)
        } else {
            source
        }
    });
    Ok(PreparedFile {
        message: PreparedMessage::failed(error, source),
        attachments: Vec::new(),
        attachment_like_count: 0,
        mime_part_count: 0,
    })
}

fn prepare_message_bytes<C>(
    path: &Path,
    bytes: Vec<u8>,
    limits: PreparationLimits,
    remaining_attachment_count: usize,
    remaining_mime_part_count: usize,
    cancelled: &mut C,
) -> anyhow::Result<PreparedFile>
where
    C: FnMut() -> bool,
{
    let mut bytes = Some(bytes);
    prepare_message(
        path,
        &mut |_, _| {
            bytes
                .take()
                .ok_or_else(|| anyhow::anyhow!("message candidate was read twice"))
        },
        limits,
        remaining_attachment_count,
        remaining_mime_part_count,
        cancelled,
    )
}

fn attach_message_source_resolver(
    prepared: &mut PreparedFile,
    resolver_config: Option<&OpenConfig>,
    message_id: &str,
) {
    if let Some(config) = resolver_config
        && let Some(source) = prepared.message.source.take()
    {
        prepared.message.source = Some(source.with_resolver(config, message_id));
    }
}

#[derive(Debug)]
struct AttachmentManifest {
    /// Stable depth-first ordinal among attachment-like parts. This is the
    /// index consumed by `notm_mail::extract_attachments_detailed`.
    part_index: usize,
    filename: String,
    content_type: String,
    size: usize,
}

#[derive(Debug)]
struct PreparedFile {
    message: PreparedMessage,
    attachments: Vec<AttachmentManifest>,
    attachment_like_count: usize,
    mime_part_count: usize,
}

impl PreparedFile {
    fn retained_bytes(&self) -> usize {
        self.message.retained_bytes().saturating_add(
            self.attachments
                .iter()
                .map(attachment_manifest_bytes)
                .sum::<usize>(),
        )
    }
}

fn prepare_message<F, C>(
    path: &Path,
    read: &mut F,
    limits: PreparationLimits,
    remaining_attachment_count: usize,
    remaining_mime_part_count: usize,
    cancelled: &mut C,
) -> anyhow::Result<PreparedFile>
where
    F: FnMut(&Path, usize) -> anyhow::Result<Vec<u8>>,
    C: FnMut() -> bool,
{
    let mut parse = parse_rfc5322;
    prepare_message_with_parser(
        path,
        read,
        limits,
        remaining_attachment_count,
        remaining_mime_part_count,
        cancelled,
        &mut parse,
    )
}

fn prepare_message_with_parser<F, C, P>(
    path: &Path,
    read: &mut F,
    limits: PreparationLimits,
    remaining_attachment_count: usize,
    remaining_mime_part_count: usize,
    cancelled: &mut C,
    parse: &mut P,
) -> anyhow::Result<PreparedFile>
where
    F: FnMut(&Path, usize) -> anyhow::Result<Vec<u8>>,
    C: FnMut() -> bool,
    P: FnMut(&[u8]) -> anyhow::Result<ParsedMessage>,
{
    anyhow::ensure!(!cancelled(), "thread preparation was cancelled");
    Ok(match read(path, limits.source_bytes) {
        Ok(bytes) => {
            anyhow::ensure!(
                bytes.len() <= limits.source_bytes,
                "message source is {} bytes; the per-source preparation limit is {} bytes",
                bytes.len(),
                limits.source_bytes
            );
            anyhow::ensure!(!cancelled(), "thread preparation was cancelled");
            let preflight = preflight_mime(
                &bytes,
                MimePreflightLimits {
                    attachment_count: remaining_attachment_count,
                    part_count: remaining_mime_part_count,
                },
                cancelled,
            )?;
            anyhow::ensure!(!cancelled(), "thread preparation was cancelled");
            let source = MessageSource::new(path.to_path_buf(), bytes.len());
            let raw = if bytes.len() <= limits.raw_bytes {
                Ok(Arc::<str>::from(
                    String::from_utf8_lossy(&bytes).into_owned(),
                ))
            } else {
                Err(Arc::from(format!(
                    "raw source is {} bytes; the responsive text-view limit is {} bytes",
                    bytes.len(),
                    limits.raw_bytes
                )))
            };
            let headers = prepare_header_block(&bytes, limits.header_bytes);
            let parsed = if let Some(error) = &preflight.parse_error {
                Err(error.clone())
            } else {
                parse(&bytes)
                    .map(Arc::new)
                    .map_err(|error| Arc::<str>::from(error.to_string()))
            };
            anyhow::ensure!(!cancelled(), "thread preparation was cancelled");
            let attachments = match &parsed {
                Ok(parsed) => decodable_attachment_manifest(parsed),
                Err(_) => Vec::new(),
            };
            let (text, html) = match &parsed {
                Ok(parsed) => (
                    prepare_text(parsed, MAX_TEXT_VIEW_BYTES),
                    prepare_html(parsed, &bytes, limits.html_bytes),
                ),
                Err(error) => (
                    prepare_parse_failure_text(error, MAX_TEXT_VIEW_BYTES),
                    PreparedHtml::Unavailable {
                        original_len: 0,
                        error: error.clone(),
                    },
                ),
            };
            anyhow::ensure!(!cancelled(), "thread preparation was cancelled");
            PreparedFile {
                message: PreparedMessage {
                    source: Some(source),
                    raw,
                    parsed,
                    headers,
                    text,
                    html,
                },
                attachments,
                attachment_like_count: preflight.attachment_like_count,
                mime_part_count: preflight.part_count,
            }
        }
        Err(error) => {
            let error = Arc::<str>::from(error.to_string());
            let source = MessageSource::new(path.to_path_buf(), 0);
            PreparedFile {
                message: PreparedMessage::failed(error, Some(source)),
                attachments: Vec::new(),
                attachment_like_count: 0,
                mime_part_count: 0,
            }
        }
    })
}

fn decodable_attachment_manifest(parsed: &ParsedMessage) -> Vec<AttachmentManifest> {
    parsed
        .attachments
        .iter()
        .filter(|attachment| attachment.decode_error.is_none())
        .map(|attachment| AttachmentManifest {
            part_index: attachment.part_index,
            filename: attachment
                .filename
                .clone()
                .unwrap_or_else(|| "attachment.bin".to_string()),
            content_type: attachment.content_type.clone(),
            size: attachment.size,
        })
        .collect()
}

fn prepare_text(parsed: &ParsedMessage, max_bytes: usize) -> Result<PreparedText, Arc<str>> {
    let expanded = render_parsed_message_text(parsed, false);
    if expanded.len() > max_bytes {
        return Err(Arc::from(format!(
            "rendered text is {} bytes; the responsive text-view limit is {max_bytes} bytes",
            expanded.len()
        )));
    }
    let collapsed = render_parsed_message_text(parsed, true);
    if collapsed.len() > max_bytes {
        return Err(Arc::from(format!(
            "rendered text is {} bytes; the responsive text-view limit is {max_bytes} bytes",
            collapsed.len()
        )));
    }
    Ok(PreparedText {
        expanded: Arc::from(expanded),
        collapsed: Arc::from(collapsed),
    })
}

fn prepare_parse_failure_text(
    error: &Arc<str>,
    max_bytes: usize,
) -> Result<PreparedText, Arc<str>> {
    let rendered = format!("Could not parse body: {error}\n");
    if rendered.len() > max_bytes {
        return Err(Arc::from(format!(
            "parse failure text is {} bytes; the responsive text-view limit is {max_bytes} bytes",
            rendered.len()
        )));
    }
    let rendered = Arc::<str>::from(rendered);
    Ok(PreparedText {
        expanded: rendered.clone(),
        collapsed: rendered,
    })
}

fn prepare_header_block(bytes: &[u8], max_bytes: usize) -> Result<Arc<str>, Arc<str>> {
    let end = find_header_end(bytes).unwrap_or(bytes.len());
    if end > max_bytes {
        return Err(Arc::from(format!(
            "message headers are {end} bytes; the responsive text-view limit is {max_bytes} bytes"
        )));
    }
    Ok(Arc::from(
        String::from_utf8_lossy(&bytes[..end]).into_owned(),
    ))
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .or_else(|| bytes.windows(2).position(|window| window == b"\n\n"))
}

#[derive(Debug, Clone, Copy)]
struct MimePreflightLimits {
    attachment_count: usize,
    part_count: usize,
}

struct MimePreflight {
    attachment_like_count: usize,
    part_count: usize,
    parse_error: Option<Arc<str>>,
}

#[derive(Default)]
struct MimePreflightState {
    attachment_like_count: usize,
    part_count: usize,
}

#[derive(Debug)]
enum MimePreflightFailure {
    Cancelled,
    DepthLimit,
    PartLimit {
        visited: usize,
        limit: usize,
    },
    AttachmentLimit {
        attachments: usize,
        visited: usize,
        limit: usize,
    },
    MalformedHeaders(String),
}

impl MimePreflightFailure {
    const fn is_recoverable_parse_failure(&self) -> bool {
        matches!(self, Self::DepthLimit | Self::MalformedHeaders(_))
    }
}

impl fmt::Display for MimePreflightFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("thread preparation was cancelled"),
            Self::DepthLimit => write!(
                formatter,
                "message MIME nesting depth exceeds the responsive limit of {MAX_MIME_NESTING_DEPTH}"
            ),
            Self::PartLimit { visited, limit } => write!(
                formatter,
                "message has at least {visited} MIME parts; remaining prepared MIME-part limit is {limit}"
            ),
            Self::AttachmentLimit {
                attachments,
                visited,
                limit,
            } => write!(
                formatter,
                "message has at least {attachments} attachment-like MIME parts after visiting {visited} MIME parts; remaining prepared attachment-count limit is {limit}"
            ),
            Self::MalformedHeaders(error) => {
                write!(
                    formatter,
                    "message MIME headers could not be parsed: {error}"
                )
            }
        }
    }
}

impl StdError for MimePreflightFailure {}

/// Inspect the MIME structure without decoding any body payloads.
///
/// `mailparse::parse_mail` recursively materializes the complete tree before a
/// caller can inspect it, and `notm_mail::parse_rfc5322` then decodes every
/// attachment-like leaf. This bounded scanner follows mailparse's boundary
/// rules but visits children one at a time, so an over-budget message is
/// rejected after the first excess part and before either operation can run.
fn preflight_mime<C>(
    bytes: &[u8],
    limits: MimePreflightLimits,
    cancelled: &mut C,
) -> anyhow::Result<MimePreflight>
where
    C: FnMut() -> bool,
{
    let mut state = MimePreflightState::default();
    let parse_error = match preflight_mime_part(bytes, false, 0, limits, &mut state, cancelled) {
        Ok(()) => None,
        Err(error) if error.is_recoverable_parse_failure() => {
            Some(Arc::<str>::from(error.to_string()))
        }
        Err(error) => return Err(anyhow::Error::new(error)),
    };
    Ok(MimePreflight {
        attachment_like_count: state.attachment_like_count,
        part_count: state.part_count,
        parse_error,
    })
}

fn preflight_mime_part<C>(
    bytes: &[u8],
    in_multipart_digest: bool,
    depth: usize,
    limits: MimePreflightLimits,
    state: &mut MimePreflightState,
    cancelled: &mut C,
) -> Result<(), MimePreflightFailure>
where
    C: FnMut() -> bool,
{
    if cancelled() {
        return Err(MimePreflightFailure::Cancelled);
    }
    if depth > MAX_MIME_NESTING_DEPTH {
        return Err(MimePreflightFailure::DepthLimit);
    }
    state.part_count = state.part_count.saturating_add(1);
    if state.part_count > limits.part_count {
        return Err(MimePreflightFailure::PartLimit {
            visited: state.part_count,
            limit: limits.part_count,
        });
    }

    let (headers, body_start) = parse_headers(bytes)
        .map_err(|error| MimePreflightFailure::MalformedHeaders(error.to_string()))?;
    let content_type = headers
        .get_first_value("Content-Type")
        .map(|value| parse_content_type(&value))
        .unwrap_or_else(|| {
            parse_content_type(if in_multipart_digest {
                "message/rfc822"
            } else {
                "text/plain"
            })
        });

    if content_type.mimetype.starts_with("multipart/")
        && let Some(boundary) = content_type.params.get("boundary")
        && body_start < bytes.len()
    {
        let mut marker = Vec::with_capacity(boundary.len().saturating_add(2));
        marker.extend_from_slice(b"--");
        marker.extend_from_slice(boundary.as_bytes());
        if let Some(first_boundary) = find_mime_boundary_line(bytes, body_start, &marker) {
            let child_is_digest = content_type.mimetype == "multipart/digest";
            let mut boundary_line = first_boundary;
            let mut found_child = false;
            while !boundary_line.closing
                && let Some(part_start) = boundary_line.next_line_start
            {
                if cancelled() {
                    return Err(MimePreflightFailure::Cancelled);
                }
                let next_boundary = find_mime_boundary_line(bytes, part_start, &marker);
                let part_end = next_boundary
                    .map(|line| strip_trailing_crlf(bytes, part_start, line.start))
                    .unwrap_or(bytes.len());
                found_child = true;
                preflight_mime_part(
                    &bytes[part_start..part_end],
                    child_is_digest,
                    depth.saturating_add(1),
                    limits,
                    state,
                    cancelled,
                )?;
                let Some(next_boundary) = next_boundary else {
                    break;
                };
                boundary_line = next_boundary;
            }
            if found_child {
                return Ok(());
            }
        }
    }

    let disposition_value = headers.get_first_value("Content-Disposition");
    let disposition = disposition_value
        .as_deref()
        .map(parse_content_disposition)
        .unwrap_or_default();
    let filename = content_type
        .params
        .get("name")
        .cloned()
        .or_else(|| disposition.params.get("filename").cloned());
    let is_attachment = filename.is_some()
        || disposition_value
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains("attachment"));
    // Mirror notm_mail's leaf classification without decoding a body. In
    // particular, filename-less calendar parts are decoded as attachments,
    // while recognized crypto protocol parts are classified but not decoded
    // unless their disposition explicitly makes them attachments.
    let crypto_related = matches!(
        content_type.mimetype.as_str(),
        "application/pgp-encrypted"
            | "application/pgp-signature"
            | "application/pgp-keys"
            | "application/pkcs7-signature"
            | "application/x-pkcs7-signature"
            | "application/pkcs7-mime"
            | "application/x-pkcs7-mime"
    );
    let attachment_like = is_attachment
        || content_type.mimetype.eq_ignore_ascii_case("text/calendar")
        || (!crypto_related && !content_type.mimetype.starts_with("text/"));
    if attachment_like {
        state.attachment_like_count = state.attachment_like_count.saturating_add(1);
        if state.attachment_like_count > limits.attachment_count {
            return Err(MimePreflightFailure::AttachmentLimit {
                attachments: state.attachment_like_count,
                visited: state.part_count,
                limit: limits.attachment_count,
            });
        }
    }
    Ok(())
}

fn find_bytes(bytes: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    debug_assert!(!needle.is_empty());
    if start > bytes.len() || needle.len() > bytes.len() {
        return None;
    }
    let end = bytes.len().saturating_sub(needle.len());
    (start..=end).find(|&index| bytes[index..].starts_with(needle))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MimeBoundaryLine {
    start: usize,
    next_line_start: Option<usize>,
    closing: bool,
}

fn find_mime_boundary_line(bytes: &[u8], start: usize, marker: &[u8]) -> Option<MimeBoundaryLine> {
    let mut search_start = start;
    while let Some(index) = find_bytes(bytes, search_start, marker) {
        if (index == start || bytes[index.saturating_sub(1)] == b'\n')
            && let Some(line) = parse_mime_boundary_line(bytes, index, marker.len())
        {
            return Some(line);
        }
        search_start = index.saturating_add(1);
    }
    None
}

fn parse_mime_boundary_line(
    bytes: &[u8],
    start: usize,
    marker_len: usize,
) -> Option<MimeBoundaryLine> {
    let mut cursor = start.checked_add(marker_len)?;
    let closing = bytes
        .get(cursor..cursor.saturating_add(2))
        .is_some_and(|suffix| suffix == b"--");
    if closing {
        cursor = cursor.saturating_add(2);
    }
    while bytes
        .get(cursor)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        cursor = cursor.saturating_add(1);
    }
    let next_line_start = match bytes.get(cursor) {
        None => None,
        Some(b'\n') => Some(cursor.saturating_add(1)),
        Some(b'\r') if bytes.get(cursor.saturating_add(1)) == Some(&b'\n') => {
            Some(cursor.saturating_add(2))
        }
        Some(_) => return None,
    };
    Some(MimeBoundaryLine {
        start,
        next_line_start,
        closing,
    })
}

fn strip_trailing_crlf(bytes: &[u8], start: usize, mut end: usize) -> usize {
    if end > start && bytes[end - 1] == b'\n' {
        end -= 1;
        if end > start && bytes[end - 1] == b'\r' {
            end -= 1;
        }
    }
    end
}

fn prepare_html(parsed: &ParsedMessage, source: &[u8], max_bytes: usize) -> PreparedHtml {
    let Some(html) = parsed
        .html_body
        .as_deref()
        .filter(|html| !html.trim().is_empty())
    else {
        return PreparedHtml::Missing;
    };
    if html.len() > max_bytes {
        return PreparedHtml::Unavailable {
            original_len: html.len(),
            error: Arc::from(format!(
                "HTML body is {} bytes; the responsive rendering limit is {max_bytes} bytes; use Text or Raw Source view",
                html.len()
            )),
        };
    }

    let sanitized = sanitize_html_with_cid_images(html);
    let sanitized = resolve_inline_cid_images(&sanitized, parsed, source);
    // These documents intentionally remain distinct even when the sanitized
    // body contains no remote image markup. Their CSPs encode different
    // authority: both may display bounded message-local MIME images, while
    // only the one-shot document permits HTTP(S) image fetches.
    let images_allowed = Arc::<str>::from(visual_html_document(&sanitized, true));
    let blocked_body = strip_remote_img_tags(&sanitized);
    let images_blocked = Arc::<str>::from(visual_html_document(&blocked_body, false));
    PreparedHtml::Ready {
        original_len: html.len(),
        images_allowed,
        images_blocked,
    }
}

fn resolve_inline_cid_images(html: &str, parsed: &ParsedMessage, source: &[u8]) -> String {
    resolve_inline_cid_images_with_limits(
        html,
        parsed,
        source,
        MAX_INLINE_IMAGE_BYTES,
        MAX_TOTAL_INLINE_IMAGE_BYTES,
    )
}

fn resolve_inline_cid_images_with_limits(
    html: &str,
    parsed: &ParsedMessage,
    source: &[u8],
    max_inline_image_bytes: usize,
    max_total_inline_image_bytes: usize,
) -> String {
    let cid_source = Regex::new(r#"(?i)\bsrc="cid:([^"]+)""#).expect("valid cid source regex");
    let referenced = cid_source
        .captures_iter(html)
        .map(|captures| normalize_content_id(&captures[1]))
        .filter(|content_id| !content_id.is_empty())
        .collect::<BTreeSet<_>>();
    if referenced.is_empty() {
        return html.to_string();
    }

    // Parsed attachment metadata comes from decoding this exact bounded source
    // and therefore carries authoritative decoded sizes and stable part
    // indexes. Select only parts that fit both image budgets before the
    // batched extraction. An oversized candidate is skipped without making a
    // later valid sibling disappear, while the extractor retains the same
    // limits as defense in depth.
    let mut selected_content_ids = BTreeSet::new();
    let mut selected_total_bytes = 0_usize;
    let selected_part_indexes = parsed
        .attachments
        .iter()
        .filter_map(|attachment| {
            let content_id = attachment.content_id.as_deref().map(normalize_content_id)?;
            if content_id.is_empty()
                || attachment.decode_error.is_some()
                || !inline_image_content_type_is_safe(&attachment.content_type)
                || !referenced.contains(&content_id)
                || selected_content_ids.contains(&content_id)
                || attachment.size == 0
                || attachment.size > max_inline_image_bytes
            {
                return None;
            }
            let next_total = selected_total_bytes.checked_add(attachment.size)?;
            if next_total > max_total_inline_image_bytes {
                return None;
            }
            selected_total_bytes = next_total;
            selected_content_ids.insert(content_id);
            Some(attachment.part_index)
        })
        .collect::<Vec<_>>();

    let extraction_limits = MimeLimits {
        max_decoded_part_bytes: max_inline_image_bytes,
        max_total_decoded_bytes: max_total_inline_image_bytes,
        ..MimeLimits::default()
    };
    let resources = if selected_part_indexes.is_empty() {
        BTreeMap::new()
    } else {
        extract_attachment_parts_detailed_with_limits(
            source,
            &selected_part_indexes,
            extraction_limits,
        )
        .map(|report| {
            let mut total_bytes = 0_usize;
            let mut resources = BTreeMap::<String, String>::new();
            for attachment in report.attachments {
                let Some(content_id) = attachment.content_id.as_deref() else {
                    continue;
                };
                if !inline_image_content_type_is_safe(&attachment.content_type)
                    || attachment.bytes.is_empty()
                    || attachment.bytes.len() > max_inline_image_bytes
                {
                    continue;
                }
                let content_id = normalize_content_id(content_id);
                if !referenced.contains(&content_id) || resources.contains_key(&content_id) {
                    continue;
                }
                let Some(next_total) = total_bytes.checked_add(attachment.bytes.len()) else {
                    continue;
                };
                if next_total > max_total_inline_image_bytes {
                    continue;
                }
                total_bytes = next_total;
                resources.insert(
                    content_id,
                    format!(
                        "data:{};base64,{}",
                        attachment.content_type,
                        BASE64_STANDARD.encode(&attachment.bytes)
                    ),
                );
            }
            resources
        })
        .unwrap_or_default()
    };

    replace_cid_sources_bounded(
        html,
        &cid_source,
        &resources,
        MAX_INLINE_IMAGE_HTML_BYTES,
        MAX_INLINE_IMAGE_REFERENCES,
    )
}

fn replace_cid_sources_bounded(
    html: &str,
    cid_source: &Regex,
    resources: &BTreeMap<String, String>,
    max_output_bytes: usize,
    max_resolved_references: usize,
) -> String {
    // Resolve only when the sanitized base document fits the output budget,
    // then track projected final length before copying each data URI so
    // repeated references cannot amplify one bounded MIME part without limit.
    let resolution_fits_base_document = html.len() <= max_output_bytes;
    let effective_output_limit = max_output_bytes.max(html.len());
    let mut projected_len = html.len();
    let mut resolved_references = 0_usize;
    let mut last_end = 0_usize;
    let mut output = String::with_capacity(html.len());

    for captures in cid_source.captures_iter(html) {
        let Some(source) = captures.get(0) else {
            continue;
        };
        output.push_str(&html[last_end..source.start()]);
        let content_id = normalize_content_id(&captures[1]);
        let resource = resources.get(&content_id);
        let replacement_len = resource.map_or(r#"src="""#.len(), |resource| {
            r#"src="""#.len().saturating_add(resource.len())
        });
        let candidate_len = projected_len
            .saturating_sub(source.as_str().len())
            .saturating_add(replacement_len);
        let resolve = resource.is_some()
            && resolution_fits_base_document
            && resolved_references < max_resolved_references
            && candidate_len <= effective_output_limit;

        if resolve {
            let resource = resource.expect("checked resource");
            output.push_str(r#"src=""#);
            output.push_str(resource);
            output.push('"');
            projected_len = candidate_len;
            resolved_references += 1;
        } else {
            output.push_str(r#"src="""#);
            projected_len = projected_len
                .saturating_sub(source.as_str().len())
                .saturating_add(r#"src="""#.len());
        }
        last_end = source.end();
    }
    output.push_str(&html[last_end..]);
    debug_assert_eq!(output.len(), projected_len);
    debug_assert!(!resolution_fits_base_document || output.len() <= max_output_bytes);
    output
}

fn inline_image_content_type_is_safe(content_type: &str) -> bool {
    matches!(
        content_type.trim().to_ascii_lowercase().as_str(),
        "image/avif" | "image/gif" | "image/jpeg" | "image/png" | "image/webp"
    )
}

fn normalize_content_id(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim()
        .to_ascii_lowercase()
}

fn render_parsed_message_text(parsed: &ParsedMessage, collapse_quotes: bool) -> String {
    let mut rendered = render_body_with_quote_collapse(&parsed.safe_body, collapse_quotes);
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
        for attachment in &parsed.attachments {
            let filename = attachment.filename.as_deref().unwrap_or("unnamed");
            match &attachment.decode_error {
                Some(error) => rendered.push_str(&format!(
                    "- {filename} ({}, decode failed: {error})\n",
                    attachment.content_type
                )),
                None if attachment.decode_warnings.is_empty() => rendered.push_str(&format!(
                    "- {filename} ({}, {} bytes)\n",
                    attachment.content_type, attachment.size
                )),
                None => rendered.push_str(&format!(
                    "- {filename} ({}, {} bytes; decoded with warning)\n",
                    attachment.content_type, attachment.size
                )),
            }
        }
    }
    rendered.push_str("\n\nMIME tree:\n");
    for node in &parsed.mime_tree {
        rendered.push_str(&format!("  {node}\n"));
    }
    rendered
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

fn strip_remote_img_tags(html: &str) -> String {
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
            let end = start + relative_end + 1;
            let tag = &html[start..end];
            if tag.to_ascii_lowercase().contains(r#"src="data:image/"#) {
                out.push_str(tag);
            } else {
                out.push_str("<span class=\"notm-blocked-image\">[image blocked]</span>");
            }
            pos = end;
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
        "data: http: https:"
    } else {
        "data:"
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

fn result_arc_str_bytes(value: &Result<Arc<str>, Arc<str>>) -> usize {
    match value {
        Ok(value) | Err(value) => value.len(),
    }
}

fn result_parsed_message_bytes(value: &Result<Arc<ParsedMessage>, Arc<str>>) -> usize {
    match value {
        Ok(parsed) => parsed_message_bytes(parsed),
        Err(error) => error.len(),
    }
}

fn parsed_message_bytes(parsed: &ParsedMessage) -> usize {
    let scalar_strings = [
        &parsed.subject,
        &parsed.from,
        &parsed.to,
        &parsed.cc,
        &parsed.reply_to,
        &parsed.message_id,
        &parsed.references,
        &parsed.in_reply_to,
        &parsed.text_body,
        &parsed.safe_body,
    ];
    std::mem::size_of::<ParsedMessage>()
        .saturating_add(
            scalar_strings
                .iter()
                .fold(0_usize, |total, value| total.saturating_add(value.len())),
        )
        .saturating_add(parsed.html_body.as_ref().map(String::len).unwrap_or(0))
        .saturating_add(parsed.headers.iter().fold(0_usize, |total, (key, value)| {
            total
                .saturating_add(std::mem::size_of::<(String, String)>())
                .saturating_add(key.len())
                .saturating_add(value.len())
        }))
        .saturating_add(
            parsed
                .decode_warnings
                .iter()
                .fold(0_usize, |total, value| total.saturating_add(value.len())),
        )
        .saturating_add(
            parsed
                .mime_tree
                .iter()
                .fold(0_usize, |total, value| total.saturating_add(value.len())),
        )
        .saturating_add(
            parsed
                .attachments
                .iter()
                .fold(0_usize, |total, attachment| {
                    total
                        .saturating_add(std::mem::size_of_val(attachment))
                        .saturating_add(attachment.filename.as_ref().map(String::len).unwrap_or(0))
                        .saturating_add(attachment.content_type.len())
                        .saturating_add(
                            attachment.content_id.as_ref().map(String::len).unwrap_or(0),
                        )
                        .saturating_add(
                            attachment
                                .decode_error
                                .as_ref()
                                .map(String::len)
                                .unwrap_or(0),
                        )
                        .saturating_add(
                            attachment
                                .decode_warnings
                                .iter()
                                .fold(0_usize, |warnings, warning| {
                                    warnings.saturating_add(warning.len())
                                }),
                        )
                }),
        )
}

fn message_summary_bytes(message: &MessageSummary) -> usize {
    std::mem::size_of::<MessageSummary>()
        .saturating_add(message.message_id.len())
        .saturating_add(message.thread_id.len())
        .saturating_add(message.from.len())
        .saturating_add(message.to.len())
        .saturating_add(message.cc.len())
        .saturating_add(message.subject.len())
        .saturating_add(
            message
                .tags
                .iter()
                .fold(0_usize, |total, value| total.saturating_add(value.len())),
        )
        .saturating_add(
            message
                .filenames
                .iter()
                .fold(0_usize, |total, value| total.saturating_add(value.len())),
        )
}

fn attachment_manifest_bytes(attachment: &AttachmentManifest) -> usize {
    std::mem::size_of::<PreparedAttachment>()
        .saturating_add(attachment.filename.len())
        .saturating_add(attachment.content_type.len())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fmt::Write as _,
        path::{Path, PathBuf},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    use notm_notmuch::{Database, MessageSummary, OpenConfig};
    use regex::Regex;

    use super::{
        DEFAULT_PREPARATION_LIMITS, MAX_CANDIDATE_THREAD_IDS, MessageSource, MimePreflightLimits,
        PreparationLimits, TargetMessageNotFound, ThreadLoadCoordinator, ThreadLoadRequest,
        ThreadLoaderService, load_thread, load_thread_with_reader, parse_rfc5322, preflight_mime,
        prepare_message_with_parser, prepare_thread, prepare_thread_with_cancel,
        prepare_thread_with_limits, prepare_thread_with_resolution, read_bounded,
        replace_cid_sources_bounded, resolve_inline_cid_images_with_limits,
    };

    fn notmuch_fixture_config(temp: &tempfile::TempDir) -> (OpenConfig, PathBuf) {
        let root = temp.path().join("mail");
        let maildir = root.join("account/cur");
        std::fs::create_dir_all(&maildir).expect("create fixture Maildir");
        let config_path = temp.path().join("notmuch-config");
        std::fs::write(
            &config_path,
            format!(
                "[database]\npath={}\n\n[user]\nname=Fixture User\nprimary_email=fixture@example.test\n\n[new]\ntags=\nignore=\n\n[search]\nexclude_tags=\n\n[maildir]\nsynchronize_flags=true\n",
                root.display()
            ),
        )
        .expect("write Notmuch config");
        (
            OpenConfig {
                database_path: Some(root),
                config_path: Some(config_path),
                profile: None,
            },
            maildir,
        )
    }

    fn message(id: &str, filename: &str) -> MessageSummary {
        MessageSummary {
            message_id: id.to_string(),
            thread_id: "thread-1".to_string(),
            date: 0,
            from: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            tags: Vec::new(),
            filenames: vec![filename.to_string()],
        }
    }

    fn message_with_tiny_attachments(count: usize) -> Vec<u8> {
        let mut source =
            String::from("MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n");
        for index in 0..count {
            write!(
                source,
                "--x\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename={index}.bin\r\nContent-Transfer-Encoding: base64\r\n\r\neA==\r\n"
            )
            .expect("write attachment fixture");
        }
        source.push_str("--x--\r\n");
        source.into_bytes()
    }

    fn message_with_tiny_padded_attachments(count: usize) -> Vec<u8> {
        let mut source =
            String::from("MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n");
        for index in 0..count {
            write!(
                source,
                "--x \t\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename={index}.bin\r\nContent-Transfer-Encoding: base64\r\n\r\neA==\r\n"
            )
            .expect("write padded attachment fixture");
        }
        source.push_str("--x-- \t");
        source.into_bytes()
    }

    fn related_html_with_inline_jpegs(count: usize) -> Vec<u8> {
        const TINY_JPEG: &str = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/2wBDAQkJCQwLDBgNDRgyIRwhMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjIyMjL/wAARCAACAAIDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwD3+iiigD//2Q==";
        let mut source = String::from(
            "MIME-Version: 1.0\r\n\
             From: not a valid mailbox ???\r\n\
             Subject: Inline image fixture\r\n\
             Content-Type: multipart/related; boundary=related\r\n\r\n\
             --related\r\n\
             Content-Type: multipart/alternative; boundary=alternative\r\n\r\n\
             --alternative\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\r\n\
             Inline image fixture.\r\n\
             --alternative\r\n\
             Content-Type: text/html; charset=utf-8\r\n\r\n\
             <p>Inline images:</p>",
        );
        for index in 0..count {
            source.push_str(&format!(
                "<img src=\"cid:scan-{index}@example.test\" alt=\"scan {index}\">"
            ));
        }
        source.push_str(
            "<img src=\"https://remote.example.test/tracker.jpg\" alt=\"remote\">\r\n\
             --alternative--\r\n",
        );
        for index in 0..count {
            source.push_str(&format!(
                "--related\r\n\
                 Content-Type: image/jpeg; name=scan-{index}.jpg\r\n\
                 Content-Disposition: inline; filename=scan-{index}.jpg\r\n\
                 Content-ID: <scan-{index}@example.test>\r\n\
                 Content-Transfer-Encoding: base64\r\n\r\n\
                 {TINY_JPEG}\r\n"
            ));
        }
        source.push_str("--related--\r\n");
        source.into_bytes()
    }

    fn related_html_with_inline_base64_payloads(payloads: &[&str]) -> Vec<u8> {
        let mut source = String::from(
            "MIME-Version: 1.0\r\n\
             Content-Type: multipart/related; boundary=related\r\n\r\n\
             --related\r\n\
             Content-Type: text/html; charset=utf-8\r\n\r\n",
        );
        for index in 0..payloads.len() {
            write!(
                source,
                "<img src=\"cid:image-{index}@example.test\" alt=\"image {index}\">"
            )
            .expect("write HTML CID reference");
        }
        source.push_str("\r\n");
        for (index, payload) in payloads.iter().enumerate() {
            write!(
                source,
                "--related\r\n\
                 Content-Type: image/png; name=image-{index}.png\r\n\
                 Content-Disposition: inline; filename=image-{index}.png\r\n\
                 Content-ID: <image-{index}@example.test>\r\n\
                 Content-Transfer-Encoding: base64\r\n\r\n\
                 {payload}\r\n"
            )
            .expect("write inline image fixture");
        }
        source.push_str("--related--\r\n");
        source.into_bytes()
    }

    fn message_with_tiny_calendars(count: usize) -> Vec<u8> {
        let mut source =
            String::from("MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n");
        for index in 0..count {
            write!(
                source,
                "--x\r\nContent-Type: text/calendar; method=REQUEST; charset=utf-8\r\n\r\nBEGIN:VCALENDAR\r\nUID:{index}\r\nEND:VCALENDAR\r\n"
            )
            .expect("write tiny calendar part");
        }
        source.push_str("--x--\r\n");
        source.into_bytes()
    }

    fn message_with_nested_multiparts(depth: usize) -> Vec<u8> {
        fn append_part(source: &mut String, depth: usize) {
            if depth == 0 {
                source.push_str("Content-Type: text/plain; charset=utf-8\r\n\r\ndeep leaf");
                return;
            }
            let boundary = format!("depth-{depth}");
            write!(
                source,
                "Content-Type: multipart/mixed; boundary={boundary}\r\n\r\n--{boundary}\r\n"
            )
            .expect("write nested MIME headers");
            append_part(source, depth - 1);
            write!(source, "\r\n--{boundary}--\r\n").expect("write nested MIME boundary");
        }

        let mut source = String::from("MIME-Version: 1.0\r\n");
        append_part(&mut source, depth);
        source.into_bytes()
    }

    #[test]
    fn preparation_reads_each_message_once_and_builds_attachment_metadata() {
        let reads = Arc::new(AtomicUsize::new(0));
        let observed = reads.clone();
        let source = b"From: sender@example.test\n\
Subject: fixture\n\
MIME-Version: 1.0\n\
Content-Type: multipart/mixed; boundary=x\n\
\n\
--x\nContent-Type: text/plain\n\nhello\n\
--x\nContent-Type: text/plain; name=file.txt\n\
Content-Disposition: attachment; filename=file.txt\n\nattachment\n\
--x--\n"
            .to_vec();
        let expected_source_len = source.len();
        let prepared = prepare_thread(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/message")],
            Some("message-1"),
            move |_, _| {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok(source.clone())
            },
        )
        .expect("prepare thread");

        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert_eq!(prepared.target_message_index, Some(0));
        assert_eq!(prepared.attachments.len(), 1);
        assert_eq!(prepared.attachments[0].filename, "file.txt");
        assert_eq!(
            prepared.attachments[0].source.path(),
            Path::new("/fixture/message")
        );
        assert!(prepared.message_contents["message-1"].raw_shared().is_ok());
        assert_eq!(
            prepared.message_contents["message-1"]
                .source()
                .expect("message source")
                .source_bytes(),
            expected_source_len
        );
        assert!(prepared.message_contents["message-1"].parsed().is_ok());
    }

    #[test]
    fn preparation_tries_a_later_indexed_file_when_the_first_is_missing() {
        let mut summary = message("message-1", "/fixture/a-missing");
        summary.filenames.push("/fixture/b-readable".to_string());
        let source = b"From: sender@example.test\nSubject: fallback\n\nreadable copy\n".to_vec();
        let mut reads = Vec::new();
        let prepared = prepare_thread("thread-1".to_string(), vec![summary], None, |path, _| {
            reads.push(path.to_path_buf());
            if path == Path::new("/fixture/a-missing") {
                anyhow::bail!("missing indexed copy");
            }
            Ok(source.clone())
        })
        .expect("prepare from later indexed copy");

        assert_eq!(
            reads,
            vec![
                PathBuf::from("/fixture/a-missing"),
                PathBuf::from("/fixture/b-readable")
            ]
        );
        let content = &prepared.message_contents["message-1"];
        assert_eq!(
            content.source().expect("resolved source").path(),
            Path::new("/fixture/b-readable")
        );
        assert!(
            content
                .rendered_text(false)
                .unwrap()
                .contains("readable copy")
        );
    }

    #[test]
    fn preparation_refreshes_current_notmuch_file_after_all_summary_paths_go_stale() {
        let temp = tempfile::tempdir().expect("temporary Notmuch root");
        let (config, maildir) = notmuch_fixture_config(&temp);
        let database = Database::create(&config).expect("create Notmuch database");
        let current_path = maildir.join("current:2,");
        let source = b"From: Sender <sender@example.test>\r\n\
To: fixture@example.test\r\n\
Subject: Current source\r\n\
Message-ID: <current-source@fixture.test>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=x\r\n\r\n\
--x\r\nContent-Type: multipart/alternative; boundary=y\r\n\r\n\
--y\r\nContent-Type: text/plain; charset=utf-8\r\n\r\ncurrent plain body\r\n\
--y\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>current html body</p>\r\n\
--y--\r\n\
--x\r\nContent-Type: text/calendar; method=REQUEST; charset=utf-8\r\n\r\n\
BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n\
--x--\r\n";
        std::fs::write(&current_path, source).expect("write current message");
        database
            .index_file_with_tags(&current_path, &["inbox"])
            .expect("index current message");
        let mut summary = database
            .find_message("current-source@fixture.test")
            .expect("look up indexed message")
            .expect("indexed message summary");
        summary.filenames = vec![maildir.join("stale:2,").display().to_string()];
        drop(database);

        let mut attempted_summary_paths = Vec::new();
        let prepared = prepare_thread_with_resolution(
            summary.thread_id.clone(),
            vec![summary],
            Some("current-source@fixture.test"),
            DEFAULT_PREPARATION_LIMITS,
            Some(&config),
            |path, max_bytes| {
                attempted_summary_paths.push(path.to_path_buf());
                read_bounded(path, max_bytes)
            },
            || false,
        )
        .expect("refresh current Notmuch path after stale summary paths");

        assert_eq!(attempted_summary_paths.len(), 1);
        assert!(attempted_summary_paths[0].ends_with("stale:2,"));
        let content = &prepared.message_contents["current-source@fixture.test"];
        assert_eq!(
            content.source().expect("resolved source").path(),
            current_path
        );
        assert!(
            content
                .raw_shared()
                .expect("prepared raw source")
                .contains("current plain body")
        );
        assert!(content.parsed().is_ok(), "refreshed MIME was not parsed");
        assert!(
            content
                .rendered_text(false)
                .expect("prepared text")
                .contains("current plain body")
        );
        assert!(content.has_html(), "refreshed HTML body was unavailable");
        assert_eq!(prepared.attachments.len(), 1);
        assert_eq!(prepared.attachments[0].filename, "invitation.ics");
        assert_eq!(prepared.attachments[0].attachment_index, 0);
    }

    #[test]
    fn message_source_uses_readable_cached_path_when_resolver_is_unavailable() {
        let temp = tempfile::tempdir().expect("temporary source root");
        let cached_path = temp.path().join("cached-message");
        let cached = b"cached source remains readable";
        std::fs::write(&cached_path, cached).expect("write cached source");
        let unavailable = OpenConfig {
            database_path: Some(temp.path().join("missing-database")),
            config_path: Some(temp.path().join("missing-config")),
            profile: None,
        };
        let source = MessageSource::new(cached_path.clone(), cached.len())
            .with_resolver(&unavailable, "removed-message@fixture.test");

        let (resolved_path, bytes) = source
            .read_bounded_with_path(DEFAULT_PREPARATION_LIMITS.source_bytes)
            .expect("readable cached source must not require Notmuch");

        assert_eq!(resolved_path, cached_path);
        assert_eq!(bytes, cached);
    }

    #[test]
    fn authoritative_path_state_remaps_all_source_clones_before_cached_read() {
        let temp = tempfile::tempdir().expect("temporary source root");
        let readable_old_path = temp.path().join("message:2,S");
        let current_path = temp.path().join("message:2,");
        std::fs::write(&readable_old_path, b"poison from readable old path")
            .expect("write old source poison");
        std::fs::write(&current_path, b"authoritative current source")
            .expect("write current source");
        let source = MessageSource::new(readable_old_path.clone(), 28);
        let lazy_attachment_source = source.clone();
        let path_states = [notm_notmuch::MessagePathState {
            message_id: "mapped-source@fixture.test".to_string(),
            paths: vec![current_path.clone()],
            path_changes: vec![notm_notmuch::MaildirPathChange {
                previous_path: readable_old_path,
                current_path: current_path.clone(),
            }],
        }];

        assert!(
            source.apply_authoritative_path_states("mapped-source@fixture.test", &path_states,)
        );
        assert_eq!(source.path(), current_path);
        assert_eq!(lazy_attachment_source.path(), current_path);
        assert_eq!(
            lazy_attachment_source
                .read_bounded(DEFAULT_PREPARATION_LIMITS.source_bytes)
                .expect("read remapped source"),
            b"authoritative current source"
        );
    }

    #[test]
    fn authoritative_path_state_rejects_an_unmapped_retained_source() {
        let temp = tempfile::tempdir().expect("temporary source root");
        let retained_path = temp.path().join("retained:2,S");
        let unrelated_current_path = temp.path().join("different-copy:2,");
        std::fs::write(&retained_path, b"still readable but not authoritative")
            .expect("write retained source");
        std::fs::write(&unrelated_current_path, b"different current copy")
            .expect("write unrelated current source");
        let source = MessageSource::new(retained_path.clone(), 36);
        let path_states = [notm_notmuch::MessagePathState {
            message_id: "unresolved-source@fixture.test".to_string(),
            paths: vec![unrelated_current_path],
            path_changes: Vec::new(),
        }];

        assert!(
            !source
                .apply_authoritative_path_states("unresolved-source@fixture.test", &path_states,)
        );
        assert_eq!(source.path(), retained_path);
    }

    #[cfg(unix)]
    #[test]
    fn authoritative_source_remap_preserves_non_utf8_maildir_paths() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let old_path = PathBuf::from(OsString::from_vec(b"/mail/cur/message-\xff:2,S".to_vec()));
        let current_path = PathBuf::from(OsString::from_vec(b"/mail/cur/message-\xff:2,".to_vec()));
        let source = MessageSource::new(old_path.clone(), 1);
        let path_states = [notm_notmuch::MessagePathState {
            message_id: "non-utf8-source@fixture.test".to_string(),
            paths: vec![current_path.clone()],
            path_changes: vec![notm_notmuch::MaildirPathChange {
                previous_path: old_path,
                current_path: current_path.clone(),
            }],
        }];

        assert!(
            source.apply_authoritative_path_states("non-utf8-source@fixture.test", &path_states,)
        );
        assert_eq!(source.path(), current_path);
    }

    #[test]
    fn message_source_resolves_current_notmuch_file_when_cached_path_is_stale() {
        let temp = tempfile::tempdir().expect("temporary Notmuch root");
        let (config, maildir) = notmuch_fixture_config(&temp);
        let database = Database::create(&config).expect("create Notmuch database");
        let current_path = maildir.join("current-resolved:2,");
        let current = b"From: Sender <sender@example.test>\r\n\
Message-ID: <resolved-source@fixture.test>\r\n\
Subject: Resolved source\r\n\r\ncurrent database source\r\n";
        std::fs::write(&current_path, current).expect("write current message");
        database
            .index_file_with_tags(&current_path, &["inbox"])
            .expect("index current message");
        drop(database);
        let stale_path = maildir.join("stale-resolved:2,");
        let source = MessageSource::new(stale_path, 0)
            .with_resolver(&config, "resolved-source@fixture.test");
        let source_clone = source.clone();

        let (resolved_path, bytes) = source
            .read_bounded_with_path(DEFAULT_PREPARATION_LIMITS.source_bytes)
            .expect("stale source must resolve through current Notmuch state");

        assert_eq!(resolved_path, current_path);
        assert_eq!(bytes, current);
        assert_eq!(source.path(), current_path);
        assert_eq!(source_clone.path(), current_path);
    }

    #[test]
    fn malformed_attachment_is_hidden_without_renumbering_a_valid_sibling() {
        let source = b"MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=x\r\n\r\n\
--x\r\nContent-Type: application/octet-stream; name=broken.bin\r\n\
Content-Disposition: attachment; filename=broken.bin\r\n\
Content-Transfer-Encoding: base64\r\n\r\n!!!!\r\n\
--x\r\nContent-Type: text/plain; name=good.txt\r\n\
Content-Disposition: attachment; filename=good.txt\r\n\
Content-Transfer-Encoding: base64\r\n\r\nZ29vZCBzaWJsaW5n\r\n\
--x--\r\n"
            .to_vec();
        let prepared = prepare_thread(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/malformed-sibling")],
            None,
            move |_, _| Ok(source.clone()),
        )
        .expect("prepare malformed attachment siblings");

        assert_eq!(prepared.attachments.len(), 1);
        assert_eq!(prepared.attachments[0].filename, "good.txt");
        assert_eq!(prepared.attachments[0].attachment_index, 1);
    }

    #[test]
    fn implicit_nontext_part_preserves_attachment_like_index_for_a_valid_sibling() {
        let source = b"MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=x\r\n\r\n\
--x\r\nContent-Type: application/octet-stream\r\n\
Content-Transfer-Encoding: base64\r\n\r\n!!!!\r\n\
--x\r\nContent-Type: text/plain; name=good.txt\r\n\
Content-Disposition: attachment; filename=good.txt\r\n\
Content-Transfer-Encoding: base64\r\n\r\nZ29vZA==\r\n\
--x--\r\n"
            .to_vec();
        let prepared = prepare_thread(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/implicit-before-explicit")],
            None,
            move |_, _| Ok(source.clone()),
        )
        .expect("prepare implicit and explicit attachment-like parts");

        assert_eq!(prepared.attachments.len(), 1);
        assert_eq!(prepared.attachments[0].filename, "good.txt");
        assert_eq!(prepared.attachments[0].attachment_index, 1);
    }

    #[test]
    fn preparation_precomputes_both_text_views() {
        let source = b"From: sender@example.test\nSubject: fixture\n\nintro\n> first quote\n> second quote\noutro\n".to_vec();
        let prepared = prepare_thread(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/message")],
            None,
            move |_, _| Ok(source.clone()),
        )
        .expect("prepare thread");
        let content = &prepared.message_contents["message-1"];

        let expanded = content.rendered_text(false).expect("expanded text");
        let collapsed = content.rendered_text(true).expect("collapsed text");
        assert!(expanded.contains("> first quote\n> second quote"));
        assert!(collapsed.contains("[quoted text collapsed]"));
        assert!(!collapsed.contains("> first quote"));
    }

    #[test]
    fn preparation_builds_distinct_blocked_and_one_shot_image_csp_documents() {
        let source = b"From: sender@example.test\n\
Subject: fixture\n\
MIME-Version: 1.0\n\
Content-Type: text/html; charset=utf-8\n\n\
<html><body><p>No image markup is needed to distinguish policy.</p></body></html>\n"
            .to_vec();
        let prepared = prepare_thread(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/message")],
            None,
            move |_, _| Ok(source.clone()),
        )
        .expect("prepare HTML thread");
        let content = &prepared.message_contents["message-1"];

        let blocked = content.html_document(false).expect("blocked document");
        let allowed = content.html_document(true).expect("one-shot document");
        assert!(!Arc::ptr_eq(&blocked, &allowed));
        assert!(blocked.contains("default-src 'none'; img-src data:"));
        assert!(!blocked.contains("img-src data: http: https:"));
        assert!(allowed.contains("default-src 'none'; img-src data: http: https:"));
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
    fn preparation_rejects_message_count_before_reading() {
        let mut reads = 0;
        let error = prepare_thread_with_limits(
            "thread-1".to_string(),
            vec![
                message("message-1", "/fixture/one"),
                message("message-2", "/fixture/two"),
            ],
            None,
            PreparationLimits {
                message_count: 1,
                ..DEFAULT_PREPARATION_LIMITS
            },
            |_, _| {
                reads += 1;
                Ok(Vec::new())
            },
        )
        .expect_err("message count must be bounded");

        assert_eq!(reads, 0);
        assert!(error.to_string().contains("prepared-content limit is 1"));
    }

    #[test]
    fn preparation_enforces_total_retained_byte_budget() {
        let source = format!(
            "From: sender@example.test\nSubject: fixture\n\n{}",
            "body".repeat(400)
        )
        .into_bytes();
        let error = prepare_thread_with_limits(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/message")],
            None,
            PreparationLimits {
                message_count: 4,
                retained_bytes: 8 * 1024,
                html_bytes: 1024,
                ..DEFAULT_PREPARATION_LIMITS
            },
            move |_, max_bytes| {
                assert!(source.len() <= max_bytes);
                Ok(source.clone())
            },
        )
        .expect_err("retained representations must share one budget");

        assert!(error.to_string().contains("prepared-content limit"));
    }

    #[test]
    fn production_reader_stops_after_the_byte_budget() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("large-message.eml");
        std::fs::write(&path, vec![b'x'; 4096]).expect("write fixture message");

        let error = read_bounded(&path, 128).expect_err("reader must stop at its byte budget");

        assert!(
            error
                .to_string()
                .contains("128-byte per-source preparation limit")
        );
    }

    #[test]
    fn candidate_lookup_is_rejected_before_opening_or_materializing_messages() {
        let request = ThreadLoadRequest {
            generation: 1,
            config: Default::default(),
            thread_id: "fallback".to_string(),
            candidate_thread_ids: (0..=MAX_CANDIDATE_THREAD_IDS)
                .map(|index| format!("thread-{index}"))
                .collect(),
            target_message_id: Some("target@example.test".to_string()),
            delay: Duration::ZERO,
        };
        let error = load_thread(&request, &AtomicBool::new(false))
            .expect_err("candidate lookup must be bounded before database access");

        assert!(
            error
                .to_string()
                .contains("responsive lookup limit is 2048")
        );
    }

    #[test]
    fn globally_absent_target_returns_typed_error_before_reading_fallback_thread() {
        let temp = tempfile::tempdir().expect("temporary Notmuch root");
        let (config, maildir) = notmuch_fixture_config(&temp);
        let database = Database::create(&config).expect("create Notmuch database");
        let fallback_path = maildir.join("fallback:2,");
        std::fs::write(
            &fallback_path,
            b"From: Sender <sender@example.test>\r\n\
Message-ID: <fallback@fixture.test>\r\n\
Subject: Existing fallback\r\n\r\nbody that must not be read\r\n",
        )
        .expect("write fallback message");
        database
            .index_file_with_tags(&fallback_path, &["inbox"])
            .expect("index fallback message");
        let fallback_thread = database
            .find_message("fallback@fixture.test")
            .expect("look up fallback message")
            .expect("indexed fallback message")
            .thread_id;
        drop(database);
        let request = ThreadLoadRequest {
            generation: 1,
            config,
            thread_id: fallback_thread.clone(),
            candidate_thread_ids: vec![fallback_thread],
            target_message_id: Some("absent@fixture.test".to_string()),
            delay: Duration::ZERO,
        };
        let mut reads = 0_usize;
        let error =
            load_thread_with_reader(&request, &AtomicBool::new(false), |path, _max_bytes| {
                reads = reads.saturating_add(1);
                anyhow::bail!("unexpected fallback source read: {}", path.display())
            })
            .expect_err("globally absent target must fail before fallback preparation");

        let not_found = error
            .downcast_ref::<TargetMessageNotFound>()
            .expect("missing target must retain its typed error");
        assert_eq!(not_found.message_id(), "absent@fixture.test");
        assert_eq!(reads, 0, "absence lookup reached raw/MIME preparation");
    }

    #[test]
    fn decoded_attachment_payload_is_not_retained_in_prepared_thread() {
        let payload = vec![b'x'; 5 * 1024 * 1024];
        let mut source = b"From: sender@example.test\r\n\
Subject: large attachment\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=x\r\n\r\n\
--x\r\nContent-Type: text/plain\r\n\r\nhello\r\n\
--x\r\nContent-Type: application/octet-stream; name=large.bin\r\n\
Content-Disposition: attachment; filename=large.bin\r\n\
Content-Transfer-Encoding: binary\r\n\r\n"
            .to_vec();
        source.extend(payload);
        source.extend_from_slice(b"\r\n--x--\r\n");
        let prepared = prepare_thread(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/large")],
            None,
            move |_, _| Ok(source.clone()),
        )
        .expect("prepare large attachment thread");

        assert_eq!(prepared.attachments.len(), 1);
        assert_eq!(prepared.attachments[0].filename, "large.bin");
        assert!(prepared.retained_bytes() < 1024 * 1024);
        assert!(
            prepared.message_contents["message-1"]
                .raw_shared()
                .unwrap_err()
                .to_string()
                .contains("responsive text-view limit")
        );
    }

    #[test]
    fn preparation_enforces_attachment_count_without_retaining_payloads() {
        let source = b"MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=x\r\n\r\n\
--x\r\nContent-Disposition: attachment; filename=one.bin\r\n\r\none\r\n\
--x\r\nContent-Disposition: attachment; filename=two.bin\r\n\r\ntwo\r\n\
--x--\r\n"
            .to_vec();
        let error = prepare_thread_with_limits(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/two-attachments")],
            None,
            PreparationLimits {
                attachment_count: 1,
                ..DEFAULT_PREPARATION_LIMITS
            },
            move |_, _| Ok(source.clone()),
        )
        .expect_err("attachment count must be bounded");

        assert!(error.to_string().contains("attachment-count limit is 1"));
    }

    #[test]
    fn mime_boundary_marker_prefix_remains_body_content() {
        let source = b"MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=x\r\n\r\n\
--x\r\nContent-Type: text/plain\r\n\r\n\
body before marker prefix\r\n\
--x-with-extra-text\r\n\
Content-Type: application/octet-stream\r\n\
Content-Disposition: attachment; filename=not-a-part.bin\r\n\r\n\
still part of the text body\r\n\
--x--\r\n";
        let preflight = preflight_mime(
            source,
            MimePreflightLimits {
                attachment_count: 0,
                part_count: 2,
            },
            &mut || false,
        )
        .expect("boundary marker prefix must not split the text part");

        assert!(preflight.parse_error.is_none());
        assert_eq!(preflight.part_count, 2);
        assert_eq!(preflight.attachment_like_count, 0);
    }

    #[test]
    fn mime_closing_boundary_prefix_does_not_end_the_multipart() {
        let source = b"MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=x\r\n\r\n\
--x\r\nContent-Type: text/plain\r\n\r\n\
body before closing prefix\r\n\
--x--with-extra-text\r\n\
still part of the text body\r\n\
--x\r\nContent-Type: application/octet-stream\r\n\
Content-Disposition: attachment; filename=real.bin\r\n\r\n\
eA==\r\n\
--x--\r\n";
        let preflight = preflight_mime(
            source,
            MimePreflightLimits {
                attachment_count: 1,
                part_count: 3,
            },
            &mut || false,
        )
        .expect("closing marker prefix must remain in the first part body");

        assert!(preflight.parse_error.is_none());
        assert_eq!(preflight.part_count, 3);
        assert_eq!(preflight.attachment_like_count, 1);
    }

    #[test]
    fn mime_boundary_lines_allow_linear_padding_and_eof() {
        let source = b"MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=x\r\n\r\n\
--x \t\r\nContent-Type: text/plain\r\n\r\ntext\r\n\
--x\t \nContent-Type: application/octet-stream\n\
Content-Disposition: attachment; filename=padded.bin\n\nattachment\n\
--x-- \t";
        let preflight = preflight_mime(
            source,
            MimePreflightLimits {
                attachment_count: 1,
                part_count: 3,
            },
            &mut || false,
        )
        .expect("valid padded boundary lines must delimit both parts");

        assert!(preflight.parse_error.is_none());
        assert_eq!(preflight.part_count, 3);
        assert_eq!(preflight.attachment_like_count, 1);
    }

    #[test]
    fn many_tiny_attachments_are_rejected_before_full_parse_or_decode() {
        const ATTACHMENT_COUNT: usize = 4_096;
        let source = message_with_tiny_attachments(ATTACHMENT_COUNT);
        assert!(source.len() <= DEFAULT_PREPARATION_LIMITS.source_bytes);

        let mut parser_calls = 0_usize;
        let mut parse = |_: &[u8]| {
            parser_calls = parser_calls.saturating_add(1);
            anyhow::bail!("full MIME parser must not run after preflight rejection")
        };
        let mut read = move |_: &Path, max_bytes: usize| {
            assert!(source.len() <= max_bytes);
            Ok(source.clone())
        };
        let mut cancelled = || false;
        let error = prepare_message_with_parser(
            Path::new("/fixture/many-tiny-attachments"),
            &mut read,
            DEFAULT_PREPARATION_LIMITS,
            DEFAULT_PREPARATION_LIMITS.attachment_count,
            DEFAULT_PREPARATION_LIMITS.mime_part_count,
            &mut cancelled,
            &mut parse,
        )
        .expect_err("attachment preflight must reject before decoding");

        assert_eq!(parser_calls, 0, "full MIME parse would decode every part");
        let error = error.to_string();
        assert!(
            error.contains("at least 2049 attachment-like MIME parts"),
            "{error}"
        );
        assert!(
            error.contains("after visiting 2050 MIME parts"),
            "preflight must stop at the first excess attachment: {error}"
        );
        assert!(error.contains("attachment-count limit is 2048"), "{error}");
    }

    #[test]
    fn many_tiny_attachments_with_padded_boundaries_are_rejected_early() {
        const ATTACHMENT_COUNT: usize = 2_049;
        let source = message_with_tiny_padded_attachments(ATTACHMENT_COUNT);
        assert!(source.len() <= DEFAULT_PREPARATION_LIMITS.source_bytes);

        let mut parser_calls = 0_usize;
        let mut parse = |_: &[u8]| {
            parser_calls = parser_calls.saturating_add(1);
            anyhow::bail!("full MIME parser must not run after preflight rejection")
        };
        let mut read = move |_: &Path, max_bytes: usize| {
            assert!(source.len() <= max_bytes);
            Ok(source.clone())
        };
        let error = prepare_message_with_parser(
            Path::new("/fixture/many-padded-attachments"),
            &mut read,
            DEFAULT_PREPARATION_LIMITS,
            DEFAULT_PREPARATION_LIMITS.attachment_count,
            DEFAULT_PREPARATION_LIMITS.mime_part_count,
            &mut || false,
            &mut parse,
        )
        .expect_err("padded attachment delimiters must preserve pre-decode limits");

        assert_eq!(parser_calls, 0, "full MIME parse would decode every part");
        let error = error.to_string();
        assert!(
            error.contains("at least 2049 attachment-like MIME parts"),
            "{error}"
        );
        assert!(
            error.contains("after visiting 2050 MIME parts"),
            "preflight must stop at the first excess attachment: {error}"
        );
        assert!(error.contains("attachment-count limit is 2048"), "{error}");
    }

    #[test]
    fn many_tiny_calendars_are_rejected_before_full_parse_or_decode() {
        const CALENDAR_COUNT: usize = 4_096;
        let source = message_with_tiny_calendars(CALENDAR_COUNT);
        assert!(source.len() <= DEFAULT_PREPARATION_LIMITS.source_bytes);

        let mut parser_calls = 0_usize;
        let mut parse = |_: &[u8]| {
            parser_calls = parser_calls.saturating_add(1);
            anyhow::bail!("full MIME parser must not run after calendar preflight rejection")
        };
        let mut read = move |_: &Path, max_bytes: usize| {
            assert!(source.len() <= max_bytes);
            Ok(source.clone())
        };
        let mut cancelled = || false;
        let error = prepare_message_with_parser(
            Path::new("/fixture/many-tiny-calendars"),
            &mut read,
            DEFAULT_PREPARATION_LIMITS,
            DEFAULT_PREPARATION_LIMITS.attachment_count,
            DEFAULT_PREPARATION_LIMITS.mime_part_count,
            &mut cancelled,
            &mut parse,
        )
        .expect_err("calendar preflight must reject before decoding");

        assert_eq!(
            parser_calls, 0,
            "full MIME parse would decode every calendar"
        );
        let error = error.to_string();
        assert!(
            error.contains("at least 2049 attachment-like MIME parts"),
            "{error}"
        );
        assert!(
            error.contains("after visiting 2050 MIME parts"),
            "preflight must stop at the first excess calendar: {error}"
        );
        assert!(error.contains("attachment-count limit is 2048"), "{error}");
    }

    #[test]
    fn mime_part_budget_rejects_before_parsing_all_children() {
        const PART_LIMIT: usize = 64;
        let source = message_with_tiny_attachments(4_096);
        let mut parser_calls = 0_usize;
        let mut parse = |_: &[u8]| {
            parser_calls = parser_calls.saturating_add(1);
            anyhow::bail!("full MIME parser must not run after preflight rejection")
        };
        let mut read = move |_: &Path, max_bytes: usize| {
            assert!(source.len() <= max_bytes);
            Ok(source.clone())
        };
        let mut cancelled = || false;
        let error = prepare_message_with_parser(
            Path::new("/fixture/many-mime-parts"),
            &mut read,
            PreparationLimits {
                mime_part_count: PART_LIMIT,
                ..DEFAULT_PREPARATION_LIMITS
            },
            DEFAULT_PREPARATION_LIMITS.attachment_count,
            PART_LIMIT,
            &mut cancelled,
            &mut parse,
        )
        .expect_err("part preflight must reject before materializing the MIME tree");

        assert_eq!(parser_calls, 0, "full MIME tree must not be materialized");
        let error = error.to_string();
        assert!(error.contains("at least 65 MIME parts"), "{error}");
        assert!(error.contains("MIME-part limit is 64"), "{error}");
    }

    #[test]
    fn excessive_mime_depth_is_a_recoverable_parse_failure_without_full_parse() {
        let source = message_with_nested_multiparts(80);
        let mut parser_calls = 0_usize;
        let mut parse = |_: &[u8]| {
            parser_calls = parser_calls.saturating_add(1);
            anyhow::bail!("full MIME parser must not run after depth preflight failure")
        };
        let mut read = move |_: &Path, max_bytes: usize| {
            assert!(source.len() <= max_bytes);
            Ok(source.clone())
        };
        let prepared = prepare_message_with_parser(
            Path::new("/fixture/over-deep-mime"),
            &mut read,
            DEFAULT_PREPARATION_LIMITS,
            DEFAULT_PREPARATION_LIMITS.attachment_count,
            DEFAULT_PREPARATION_LIMITS.mime_part_count,
            &mut || false,
            &mut parse,
        )
        .expect("depth limit must preserve the message as a recoverable parse failure");

        assert_eq!(parser_calls, 0);
        assert!(
            prepared
                .message
                .raw_shared()
                .expect("raw source remains available")
                .contains("deep leaf")
        );
        let error = prepared
            .message
            .parsed()
            .expect_err("over-deep MIME must remain a parse failure")
            .to_string();
        assert!(error.contains("nesting"), "{error}");
        assert!(error.contains("responsive limit"), "{error}");
        assert!(
            prepared
                .message
                .rendered_text(false)
                .expect("parse failure remains renderable")
                .contains("Could not parse body:")
        );
        assert_eq!(prepared.mime_part_count, 65);
    }

    #[test]
    fn attachment_like_budget_is_cumulative_when_parts_have_no_manifest_entry() {
        let source = b"MIME-Version: 1.0\r\n\
Content-Type: application/octet-stream\r\n\
Content-Transfer-Encoding: base64\r\n\r\n\
eA==\r\n"
            .to_vec();
        let error = prepare_thread_with_limits(
            "thread-1".to_string(),
            vec![
                message("message-1", "/fixture/implicit-one"),
                message("message-2", "/fixture/implicit-two"),
            ],
            None,
            PreparationLimits {
                attachment_count: 1,
                ..DEFAULT_PREPARATION_LIMITS
            },
            move |_, _| Ok(source.clone()),
        )
        .expect_err("implicit non-text parts must share the thread attachment budget");

        let error = error.to_string();
        assert!(
            error.contains("attachment-like MIME parts"),
            "unexpected error: {error}"
        );
        assert!(error.contains("attachment-count limit is 0"), "{error}");
    }

    #[test]
    fn crypto_protocol_parts_only_consume_attachment_budget_when_explicitly_attached() {
        let bare = b"MIME-Version: 1.0\r\n\
Content-Type: application/pgp-signature\r\n\r\n\
signature\r\n"
            .to_vec();
        let prepared = prepare_thread_with_limits(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/bare-signature")],
            None,
            PreparationLimits {
                attachment_count: 0,
                ..DEFAULT_PREPARATION_LIMITS
            },
            move |_, _| Ok(bare.clone()),
        )
        .expect("bare crypto protocol part should not consume attachment budget");
        assert!(prepared.attachments.is_empty());
        assert!(
            prepared.message_contents["message-1"]
                .parsed()
                .expect("parse bare crypto part")
                .classification
                .has_signed()
        );

        let explicit = b"MIME-Version: 1.0\r\n\
Content-Type: application/pgp-signature\r\n\
Content-Disposition: attachment; filename=signature.asc\r\n\r\n\
signature\r\n"
            .to_vec();
        let error = prepare_thread_with_limits(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/attached-signature")],
            None,
            PreparationLimits {
                attachment_count: 0,
                ..DEFAULT_PREPARATION_LIMITS
            },
            move |_, _| Ok(explicit.clone()),
        )
        .expect_err("explicit crypto attachment must consume attachment budget");
        assert!(error.to_string().contains("attachment-count limit is 0"));
    }

    #[test]
    fn oversized_raw_source_is_not_stored_as_an_arc_string() {
        let source = b"From: sender@example.test\nSubject: fixture\n\nbody body body".to_vec();
        let prepared = prepare_thread_with_limits(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/raw-limit")],
            None,
            PreparationLimits {
                raw_bytes: 16,
                ..DEFAULT_PREPARATION_LIMITS
            },
            move |_, _| Ok(source.clone()),
        )
        .expect("prepare source above raw-view limit");

        let error = prepared.message_contents["message-1"]
            .raw_shared()
            .expect_err("raw source must not be retained");
        assert!(
            error
                .to_string()
                .contains("responsive text-view limit is 16")
        );
    }

    #[test]
    fn oversized_html_is_not_sanitized_or_rendered_on_the_ui_thread() {
        let source = b"From: sender@example.test\nSubject: fixture\nContent-Type: text/html\n\n<p>This HTML body is deliberately over the tiny fixture threshold.</p>"
            .to_vec();
        let prepared = prepare_thread_with_limits(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/message")],
            None,
            PreparationLimits {
                message_count: 4,
                retained_bytes: 1024 * 1024,
                html_bytes: 16,
                ..DEFAULT_PREPARATION_LIMITS
            },
            move |_, _| Ok(source.clone()),
        )
        .expect("prepare thread");
        let content = &prepared.message_contents["message-1"];

        assert!(content.has_html());
        assert!(content.html_original_len() > 16);
        assert!(
            content
                .html_document(false)
                .unwrap_err()
                .to_string()
                .contains("responsive rendering limit is 16 bytes")
        );
    }

    #[test]
    fn related_cid_images_render_locally_without_remote_image_permission() {
        let source = related_html_with_inline_jpegs(7);
        let prepared = prepare_thread(
            "thread-1".to_string(),
            vec![message("message-1", "/fixture/related")],
            None,
            move |_, _| Ok(source.clone()),
        )
        .expect("prepare related message");
        let content = &prepared.message_contents["message-1"];

        let blocked = content.html_document(false).expect("blocked HTML");
        assert!(blocked.contains("img-src data:"), "{blocked}");
        assert_eq!(blocked.matches("data:image/jpeg;base64,").count(), 7);
        assert!(blocked.contains("alt=\"scan 6\""), "{blocked}");
        assert!(!blocked.contains("remote.example.test"), "{blocked}");

        let allowed = content.html_document(true).expect("allowed HTML");
        assert!(allowed.contains("img-src data: http: https:"), "{allowed}");
        assert_eq!(allowed.matches("data:image/jpeg;base64,").count(), 7);
        assert!(allowed.contains("https://remote.example.test/tracker.jpg"));
        assert!(!allowed.contains("cid:"), "{allowed}");
    }

    #[test]
    fn oversized_cid_part_does_not_remove_valid_siblings() {
        // Decoded sizes are 2, 5, and 1 bytes. With a four-byte per-part
        // limit, the middle image must be omitted without losing either
        // fitting sibling.
        let source = related_html_with_inline_base64_payloads(&["QUI=", "QUJDREU=", "Wg=="]);
        let parsed = parse_rfc5322(&source).expect("parse related CID fixture");
        let html = parsed.html_body.as_deref().expect("fixture HTML body");

        let resolved = resolve_inline_cid_images_with_limits(html, &parsed, &source, 4, 4);

        assert_eq!(resolved.matches("data:image/png;base64,").count(), 2);
        assert!(
            resolved.contains("data:image/png;base64,QUI="),
            "{resolved}"
        );
        assert!(
            resolved.contains("data:image/png;base64,Wg=="),
            "{resolved}"
        );
        assert!(!resolved.contains("QUJDREU="), "{resolved}");
        assert!(!resolved.contains("cid:"), "{resolved}");
    }

    #[test]
    fn cid_part_crossing_total_budget_does_not_remove_later_fitting_sibling() {
        // Decoded sizes are 3, 3, and 1 bytes. The second candidate would
        // cross the four-byte aggregate limit; skipping it leaves room for the
        // final one-byte sibling.
        let source = related_html_with_inline_base64_payloads(&["QUJD", "REVG", "Rw=="]);
        let parsed = parse_rfc5322(&source).expect("parse related CID fixture");
        let html = parsed.html_body.as_deref().expect("fixture HTML body");

        let resolved = resolve_inline_cid_images_with_limits(html, &parsed, &source, 4, 4);

        assert_eq!(resolved.matches("data:image/png;base64,").count(), 2);
        assert!(
            resolved.contains("data:image/png;base64,QUJD"),
            "{resolved}"
        );
        assert!(
            resolved.contains("data:image/png;base64,Rw=="),
            "{resolved}"
        );
        assert!(!resolved.contains("REVG"), "{resolved}");
        assert!(!resolved.contains("cid:"), "{resolved}");
    }

    #[test]
    fn repeated_cid_references_are_bounded_by_count_and_generated_bytes() {
        let html = (0..32)
            .map(|_| r#"<img src="cid:shared@example.test">"#)
            .collect::<String>();
        let cid_source = Regex::new(r#"(?i)\bsrc="cid:([^"]+)""#).expect("valid cid source regex");
        let resource = format!("data:image/png;base64,{}", "A".repeat(400));
        let resources = BTreeMap::from([("shared@example.test".to_string(), resource)]);

        let count_limited =
            replace_cid_sources_bounded(&html, &cid_source, &resources, usize::MAX, 3);
        assert_eq!(count_limited.matches("data:image/png;base64,").count(), 3);
        assert!(!count_limited.contains("cid:"), "{count_limited}");
        assert!(count_limited.contains(r#"src="""#), "{count_limited}");

        let byte_limit = html.len() + 500;
        let byte_limited =
            replace_cid_sources_bounded(&html, &cid_source, &resources, byte_limit, usize::MAX);
        assert!(byte_limited.len() <= byte_limit);
        assert!(
            byte_limited.matches("data:image/png;base64,").count() < 32,
            "{byte_limited}"
        );
        assert!(!byte_limited.contains("cid:"), "{byte_limited}");

        let oversized_base =
            replace_cid_sources_bounded(&html, &cid_source, &resources, html.len() - 1, usize::MAX);
        assert_eq!(oversized_base.matches("data:image/png;base64,").count(), 0);
        assert!(!oversized_base.contains("cid:"), "{oversized_base}");
    }

    #[test]
    fn service_coalesces_stale_work_and_never_prepares_payloads_concurrently() {
        let started_slow = Arc::new(AtomicBool::new(false));
        let observed_started = started_slow.clone();
        let executed = Arc::new(Mutex::new(Vec::new()));
        let observed_executed = executed.clone();
        let service = ThreadLoaderService::new(Arc::new(move |request, cancelled| {
            observed_executed
                .lock()
                .expect("executed mutex")
                .push(request.thread_id.clone());
            if request.thread_id == "slow" {
                observed_started.store(true, Ordering::Release);
                while !cancelled.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(2));
                }
                anyhow::bail!("cancelled slow preparation");
            }
            prepare_thread(request.thread_id.clone(), Vec::new(), None, |_, _| {
                unreachable!("no messages")
            })
        }));
        let request = |generation, thread_id: &str| ThreadLoadRequest {
            generation,
            config: Default::default(),
            thread_id: thread_id.to_string(),
            candidate_thread_ids: vec![thread_id.to_string()],
            target_message_id: None,
            delay: Duration::ZERO,
        };
        let slow = service.submit(request(1, "slow"));
        let wait_started = Instant::now();
        while !started_slow.load(Ordering::Acquire) {
            assert!(wait_started.elapsed() < Duration::from_secs(1));
            std::thread::sleep(Duration::from_millis(1));
        }
        let middle = service.submit(request(2, "middle"));
        let latest = service.submit(request(3, "latest"));

        let latest = latest
            .recv_timeout(Duration::from_secs(1))
            .expect("latest response");
        assert_eq!(latest.generation, 3);
        assert_eq!(latest.result.expect("latest result").thread_id, "latest");
        assert!(slow.recv_timeout(Duration::from_secs(1)).is_ok());
        assert!(middle.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(
            *executed.lock().expect("executed mutex"),
            vec!["slow".to_string(), "latest".to_string()]
        );
        let snapshot = service.snapshot();
        assert_eq!(snapshot.active_preparations, 0);
        assert_eq!(snapshot.peak_active_preparations, 1);
        assert!(snapshot.coalesced >= 1);
    }

    #[test]
    fn cancellation_invalidates_an_in_flight_generation() {
        let mut coordinator = ThreadLoadCoordinator::default();
        let generation = coordinator.begin();
        assert!(coordinator.accepts(generation));
        coordinator.cancel();
        assert!(!coordinator.accepts(generation));
        assert!(!coordinator.finish(generation));
    }

    #[test]
    fn cancellation_is_checked_between_message_payloads() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let read_cancelled = cancelled.clone();
        let check_cancelled = cancelled.clone();
        let reads = Arc::new(AtomicUsize::new(0));
        let observed_reads = reads.clone();
        let error = prepare_thread_with_cancel(
            "thread-1".to_string(),
            vec![
                message("message-1", "/fixture/one"),
                message("message-2", "/fixture/two"),
            ],
            None,
            DEFAULT_PREPARATION_LIMITS,
            move |_, _| {
                observed_reads.fetch_add(1, Ordering::AcqRel);
                read_cancelled.store(true, Ordering::Release);
                Ok(b"From: sender@example.test\n\nbody".to_vec())
            },
            move || check_cancelled.load(Ordering::Acquire),
        )
        .expect_err("cancellation must stop before another message payload");

        assert!(
            error
                .to_string()
                .contains("thread preparation was cancelled")
        );
        assert_eq!(reads.load(Ordering::Acquire), 1);
    }

    #[test]
    fn read_failure_is_cached_for_both_views_without_retrying() {
        let mut reads = 0;
        let prepared = prepare_thread(
            "thread-1".to_string(),
            vec![message("message-1", "/missing")],
            None,
            |path: &Path, _| {
                reads += 1;
                anyhow::bail!("cannot read {}", path.display())
            },
        )
        .expect("prepare thread");
        let content = &prepared.message_contents["message-1"];
        assert_eq!(reads, 1);
        assert!(
            content
                .raw_shared()
                .unwrap_err()
                .to_string()
                .contains("cannot read")
        );
        assert!(
            content
                .parsed()
                .unwrap_err()
                .to_string()
                .contains("cannot read")
        );
        assert_eq!(content.source().expect("source locator").source_bytes(), 0);
    }
}
