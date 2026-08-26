use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::Path,
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use chrono::Utc;
use gtk::prelude::*;
use gtk4 as gtk;
use notm_notmuch::{
    Database, DatabaseMode, MessageSummary, OpenConfig, QueryOptions, Revision, SortOrder,
    ThreadSummary, database::BoundedThreadMessages,
};
use serde_json::json;

use crate::{
    cache::{BoundedLruCache, SEARCH_PAGE_CACHE_CAPACITY, THREAD_DETAIL_CACHE_CAPACITY},
    model::{MAX_SEARCH_PAGE_SIZE, MAX_THREAD_DETAIL_MESSAGES, ThreadUiDetails},
};

const THREAD_PREVIEW_CACHE_MAX_CHARS: usize = 1024;
const THREAD_DETAIL_SOURCE_MAX_BYTES: usize = 4 * 1024 * 1024;
const THREAD_DETAIL_BATCH_MESSAGE_LIMIT: usize = 4_096;
const MAX_VISIBLE_SEARCH_TAGS: usize = 256;
const SEARCH_WORKER_POLL_INTERVAL: Duration = Duration::from_millis(20);
const SEARCH_CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);
const THREAD_MODEL_ROWS_PER_UPDATE: usize = 48;
const THREAD_ROW_PREFIX: &str = "thread";
const THREAD_STATUS_PREFIX: &str = "status";

#[derive(Debug, Clone)]
pub(crate) struct SearchData {
    pub(crate) query: String,
    pub(crate) threads: Vec<ThreadSummary>,
    pub(crate) details: BTreeMap<String, ThreadUiDetails>,
    pub(crate) count: u32,
    pub(crate) offset: usize,
    pub(crate) limit: usize,
    pub(crate) tags: Vec<String>,
    pub(crate) database_path: String,
    pub(crate) revision: Revision,
    pub(crate) cached: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SearchRuntimeSnapshot {
    pub(crate) page_size: usize,
    pub(crate) excluded_tags: Vec<String>,
}

pub(crate) type SearchRuntimeProvider = Arc<dyn Fn() -> SearchRuntimeSnapshot + Send + Sync>;

#[derive(Clone)]
pub(crate) struct SearchPageCoordinator {
    runtime: SearchRuntimeProvider,
    worker: SearchWorkerService,
}

#[derive(Debug, Clone)]
pub(crate) struct SearchPageRequest {
    pub(crate) query: String,
    pub(crate) generation: u64,
    pub(crate) offset: usize,
    pub(crate) select_first: bool,
    pub(crate) delay: Duration,
}

pub(crate) struct SearchPageResponse {
    pub(crate) generation: u64,
    pub(crate) select_first: bool,
    pub(crate) result: anyhow::Result<SearchData>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SearchWorkerSnapshot {
    pub(crate) active_preparations: usize,
    pub(crate) peak_active_preparations: usize,
    pub(crate) submitted: usize,
    pub(crate) cancelled: usize,
    pub(crate) coalesced: usize,
}

#[derive(Debug, Default)]
struct SearchWorkerMetrics {
    active_preparations: AtomicUsize,
    peak_active_preparations: AtomicUsize,
    submitted: AtomicUsize,
    cancelled: AtomicUsize,
    coalesced: AtomicUsize,
}

impl SearchWorkerMetrics {
    fn snapshot(&self) -> SearchWorkerSnapshot {
        SearchWorkerSnapshot {
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

type SearchWorkerFn = dyn Fn(&SearchPageRequest, SearchRuntimeSnapshot, &AtomicBool) -> anyhow::Result<SearchData>
    + Send
    + Sync
    + 'static;

struct SearchPageJob {
    request: SearchPageRequest,
    runtime: SearchRuntimeSnapshot,
    cancelled: Arc<AtomicBool>,
    response: mpsc::Sender<SearchPageResponse>,
}

enum SearchWorkerCommand {
    Search(SearchPageJob),
    Cancel,
}

#[derive(Clone)]
struct SearchWorkerService {
    sender: Option<mpsc::Sender<SearchWorkerCommand>>,
    start_error: Option<Arc<str>>,
    active_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    metrics: Arc<SearchWorkerMetrics>,
}

impl SearchWorkerService {
    fn new(worker: Arc<SearchWorkerFn>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let active_cancel = Arc::new(Mutex::new(None));
        let metrics = Arc::new(SearchWorkerMetrics::default());
        let worker_cancel = active_cancel.clone();
        let worker_metrics = metrics.clone();
        match thread::Builder::new()
            .name("notm-search-worker".to_string())
            .spawn(move || {
                search_worker_loop(receiver, worker_cancel, worker_metrics, worker);
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

    fn submit(
        &self,
        request: SearchPageRequest,
        runtime: SearchRuntimeSnapshot,
    ) -> mpsc::Receiver<SearchPageResponse> {
        let (response, receiver) = mpsc::channel();
        self.metrics.submitted.fetch_add(1, Ordering::AcqRel);
        self.cancel_active();
        let generation = request.generation;
        let select_first = request.select_first;
        let cancelled = Arc::new(AtomicBool::new(false));
        *self
            .active_cancel
            .lock()
            .expect("search worker cancellation mutex poisoned") = Some(cancelled.clone());
        let job = SearchPageJob {
            request,
            runtime,
            cancelled,
            response: response.clone(),
        };
        if let Some(sender) = &self.sender
            && sender.send(SearchWorkerCommand::Search(job)).is_ok()
        {
            return receiver;
        }
        self.cancel_active();
        let error = self
            .start_error
            .as_deref()
            .unwrap_or("search worker disconnected");
        let _ = response.send(SearchPageResponse {
            generation,
            select_first,
            result: Err(anyhow::anyhow!(error.to_string())),
        });
        receiver
    }

    fn cancel(&self) {
        self.cancel_active();
        if let Some(sender) = &self.sender {
            let _ = sender.send(SearchWorkerCommand::Cancel);
        }
    }

    fn cancel_active(&self) {
        if let Some(cancelled) = self
            .active_cancel
            .lock()
            .expect("search worker cancellation mutex poisoned")
            .take()
            && !cancelled.swap(true, Ordering::AcqRel)
        {
            self.metrics.cancelled.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn snapshot(&self) -> SearchWorkerSnapshot {
        self.metrics.snapshot()
    }
}

fn search_worker_loop(
    receiver: mpsc::Receiver<SearchWorkerCommand>,
    active_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    metrics: Arc<SearchWorkerMetrics>,
    worker: Arc<SearchWorkerFn>,
) {
    while let Ok(first) = receiver.recv() {
        let Some(job) = latest_search_job(first, &receiver, &metrics) else {
            continue;
        };
        metrics.begin_preparation();
        let generation = job.request.generation;
        let select_first = job.request.select_first;
        let result = worker(&job.request, job.runtime, &job.cancelled);
        metrics.finish_preparation();
        {
            let mut active = active_cancel
                .lock()
                .expect("search worker cancellation mutex poisoned");
            if active
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &job.cancelled))
            {
                *active = None;
            }
        }
        let _ = job.response.send(SearchPageResponse {
            generation,
            select_first,
            result,
        });
    }
}

fn latest_search_job(
    first: SearchWorkerCommand,
    receiver: &mpsc::Receiver<SearchWorkerCommand>,
    metrics: &SearchWorkerMetrics,
) -> Option<SearchPageJob> {
    let mut latest = match first {
        SearchWorkerCommand::Search(job) => Some(job),
        SearchWorkerCommand::Cancel => None,
    };
    for command in receiver.try_iter() {
        match command {
            SearchWorkerCommand::Search(job) => {
                if latest.replace(job).is_some() {
                    metrics.coalesced.fetch_add(1, Ordering::AcqRel);
                }
            }
            SearchWorkerCommand::Cancel => {
                if latest.take().is_some() {
                    metrics.coalesced.fetch_add(1, Ordering::AcqRel);
                }
            }
        }
    }
    latest
}

impl SearchPageCoordinator {
    pub(crate) fn new(open_config: OpenConfig, runtime: SearchRuntimeProvider) -> Self {
        let worker = SearchWorkerService::new(Arc::new(move |request, runtime, cancelled| {
            sleep_search_delay(request.delay, cancelled)?;
            execute_search_page_with_cancel(
                &open_config,
                &request.query,
                request.offset,
                runtime.page_size,
                runtime.excluded_tags,
                cancelled,
            )
        }));
        Self { runtime, worker }
    }

    pub(crate) fn launch<C>(
        &self,
        request: SearchPageRequest,
        cancellation_message: &'static str,
        complete: C,
    ) where
        C: FnOnce(SearchPageResponse) + 'static,
    {
        let generation = request.generation;
        let select_first = request.select_first;
        let response = self.worker.submit(request, (self.runtime)());
        let mut complete = Some(complete);
        gtk::glib::timeout_add_local(SEARCH_WORKER_POLL_INTERVAL, move || {
            let response = match response.try_recv() {
                Ok(response) => response,
                Err(mpsc::TryRecvError::Empty) => return gtk::glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => SearchPageResponse {
                    generation,
                    select_first,
                    result: Err(anyhow::anyhow!(cancellation_message)),
                },
            };
            if let Some(complete) = complete.take() {
                complete(response);
            }
            gtk::glib::ControlFlow::Break
        });
    }

    pub(crate) fn cancel(&self) {
        self.worker.cancel();
    }

    pub(crate) fn worker_snapshot(&self) -> SearchWorkerSnapshot {
        self.worker.snapshot()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ThreadPagingSnapshot {
    pub(crate) search_loading: bool,
    pub(crate) current_query: String,
    pub(crate) window_offset: usize,
    pub(crate) loaded_count: usize,
    pub(crate) can_load_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoadMoreDecision {
    Busy,
    Exhausted,
    Ready { query: String, offset: usize },
}

pub(crate) fn plan_load_more(snapshot: &ThreadPagingSnapshot) -> LoadMoreDecision {
    if snapshot.search_loading {
        return LoadMoreDecision::Busy;
    }
    if !snapshot.can_load_more {
        return LoadMoreDecision::Exhausted;
    }
    LoadMoreDecision::Ready {
        query: snapshot.current_query.clone(),
        offset: snapshot.window_offset + snapshot.loaded_count,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocatePagePlan {
    pub(crate) query: String,
    pub(crate) target_index: usize,
    pub(crate) offset: usize,
    pub(crate) page_size: usize,
    pub(crate) visual_anchor_index: Option<usize>,
}

impl LocatePagePlan {
    pub(crate) fn new(
        query: &str,
        target_index: usize,
        page_size: usize,
        visual_anchor_index: Option<usize>,
    ) -> Self {
        let page_size = page_size.max(1);
        Self {
            query: query.to_string(),
            target_index,
            offset: page_offset_for_index(target_index, page_size),
            page_size,
            visual_anchor_index,
        }
    }

    pub(crate) fn loading_status(&self) -> String {
        format!(
            "Loading message {} (page {}-{})…",
            format_count(self.target_index + 1),
            format_count(self.offset + 1),
            format_count(self.offset + self.page_size)
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ThreadSearchStateSnapshot {
    pub(crate) window_offset: usize,
    pub(crate) threads: Vec<ThreadSummary>,
    pub(crate) details: BTreeMap<String, ThreadUiDetails>,
    pub(crate) selected_thread_id: Option<String>,
    pub(crate) selected_index: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThreadModelUpdate {
    Replace,
    Append { start: usize, count: usize },
}

#[derive(Debug, Clone)]
pub(crate) struct ThreadSearchStateUpdate {
    pub(crate) current_query: String,
    pub(crate) window_offset: usize,
    pub(crate) threads: Vec<ThreadSummary>,
    pub(crate) total_count: u32,
    pub(crate) loaded_count: usize,
    pub(crate) page_size: usize,
    pub(crate) can_load_more: bool,
    pub(crate) details: BTreeMap<String, ThreadUiDetails>,
    pub(crate) visible_tags: Vec<String>,
    pub(crate) database_path: String,
    pub(crate) revision: Revision,
    pub(crate) operation: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ReplaceSearchOutcome {
    pub(crate) update: ThreadSearchStateUpdate,
    pub(crate) cached: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct AppendSearchOutcome {
    pub(crate) update: ThreadSearchStateUpdate,
    pub(crate) model_update: ThreadModelUpdate,
    pub(crate) selected_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchErrorOutcome {
    pub(crate) error: String,
    pub(crate) message: String,
    pub(crate) clear_empty_counts: bool,
}

pub(crate) fn reduce_replace_search(data: SearchData) -> ReplaceSearchOutcome {
    let loaded_count = data.threads.len();
    let operation = format!(
        "search `{}` loaded {} of {} thread(s) from offset {}{}",
        data.query,
        loaded_count,
        data.count,
        data.offset,
        if data.cached { " from cache" } else { "" }
    );
    ReplaceSearchOutcome {
        cached: data.cached,
        update: ThreadSearchStateUpdate {
            current_query: data.query,
            window_offset: data.offset,
            threads: data.threads,
            total_count: data.count,
            loaded_count,
            page_size: data.limit,
            can_load_more: data.offset + loaded_count < data.count as usize,
            details: data.details,
            visible_tags: data.tags,
            database_path: data.database_path,
            revision: data.revision,
            operation,
        },
    }
}

pub(crate) fn reduce_append_search(
    snapshot: ThreadSearchStateSnapshot,
    data: SearchData,
    select_last_loaded: bool,
) -> AppendSearchOutcome {
    let expected_offset = snapshot.window_offset + snapshot.threads.len();
    let reset_model = data.offset != expected_offset;
    let mut threads = snapshot.threads;
    let mut details = snapshot.details;
    let append_start = if reset_model {
        threads.clear();
        details.clear();
        0
    } else {
        threads.len()
    };
    let append_count = data.threads.len();
    threads.extend(data.threads);
    details.extend(data.details);
    let window_offset = if reset_model {
        data.offset
    } else {
        snapshot.window_offset
    };
    let restored_index = if reset_model {
        snapshot
            .selected_thread_id
            .and_then(|thread_id| {
                threads
                    .iter()
                    .position(|thread| thread.thread_id == thread_id)
            })
            .or(snapshot.selected_index)
            .filter(|index| *index < threads.len())
    } else {
        snapshot.selected_index
    };
    let selected_index = if select_last_loaded && append_count > 0 {
        Some(append_start + append_count - 1)
    } else {
        restored_index
    };
    let loaded_count = threads.len();
    let operation = format!(
        "loaded page at offset {}: {}{}",
        data.offset,
        thread_window_status(window_offset, loaded_count, data.count as usize),
        if data.cached { " from cache" } else { "" }
    );
    AppendSearchOutcome {
        model_update: if reset_model {
            ThreadModelUpdate::Replace
        } else {
            ThreadModelUpdate::Append {
                start: append_start,
                count: append_count,
            }
        },
        selected_index,
        update: ThreadSearchStateUpdate {
            current_query: data.query,
            window_offset,
            threads,
            total_count: data.count,
            loaded_count,
            page_size: data.limit,
            can_load_more: window_offset + loaded_count < data.count as usize,
            details,
            visible_tags: data.tags,
            database_path: data.database_path,
            revision: data.revision,
            operation,
        },
    }
}

pub(crate) fn reduce_search_error(error: anyhow::Error, has_threads: bool) -> SearchErrorOutcome {
    let error = error.to_string();
    SearchErrorOutcome {
        message: format!("Search failed: {error}"),
        error,
        clear_empty_counts: !has_threads,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SearchCacheKey {
    cache_epoch: u64,
    database_path: String,
    database_uuid: String,
    database_revision: u64,
    query: String,
    offset: usize,
    limit: usize,
    excluded_tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ThreadDetailCacheKey {
    cache_epoch: u64,
    database_path: String,
    database_uuid: String,
    database_revision: u64,
    thread_id: String,
}

static SEARCH_CACHE: OnceLock<Mutex<BoundedLruCache<SearchCacheKey, SearchData>>> = OnceLock::new();
static THREAD_DETAIL_CACHE: OnceLock<
    Mutex<BoundedLruCache<ThreadDetailCacheKey, ThreadUiDetails>>,
> = OnceLock::new();
static CACHE_EPOCH: AtomicU64 = AtomicU64::new(0);

fn search_cache() -> &'static Mutex<BoundedLruCache<SearchCacheKey, SearchData>> {
    SEARCH_CACHE.get_or_init(|| Mutex::new(BoundedLruCache::new(SEARCH_PAGE_CACHE_CAPACITY)))
}

fn thread_detail_cache() -> &'static Mutex<BoundedLruCache<ThreadDetailCacheKey, ThreadUiDetails>> {
    THREAD_DETAIL_CACHE
        .get_or_init(|| Mutex::new(BoundedLruCache::new(THREAD_DETAIL_CACHE_CAPACITY)))
}

pub(crate) fn invalidate_search_caches() {
    CACHE_EPOCH.fetch_add(1, Ordering::AcqRel);
    search_cache().lock().expect("search cache lock").clear();
    thread_detail_cache()
        .lock()
        .expect("thread detail cache lock")
        .clear();
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ThreadListDisplay {
    pub(crate) numbers: bool,
    pub(crate) dates: bool,
    pub(crate) tags: bool,
    pub(crate) preview: bool,
    pub(crate) preview_lines: usize,
}

impl ThreadListDisplay {
    fn token_bits(self) -> String {
        format!(
            "{}{}{}{}-{}",
            if self.numbers { 1 } else { 0 },
            if self.dates { 1 } else { 0 },
            if self.tags { 1 } else { 0 },
            if self.preview { 1 } else { 0 },
            self.preview_lines,
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ThreadDisplayToggle {
    Numbers,
    Dates,
    Tags,
    Preview,
}

impl ThreadDisplayToggle {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Numbers => "Thread numbers",
            Self::Dates => "Thread dates",
            Self::Tags => "Thread tags",
            Self::Preview => "Thread preview",
        }
    }
}

pub(crate) struct ThreadRowSnapshot {
    pub(crate) thread: ThreadSummary,
    pub(crate) detail: ThreadUiDetails,
    pub(crate) absolute_index: usize,
    pub(crate) display: ThreadListDisplay,
    pub(crate) visual_selected: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ThreadModelSnapshot {
    pub(crate) len: usize,
    pub(crate) display: ThreadListDisplay,
    pub(crate) marked_indices: BTreeSet<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThreadModelRuntimeSnapshot {
    pub(crate) generation: u64,
    pub(crate) busy: bool,
    pub(crate) model_len: usize,
    pub(crate) max_rows_per_update: usize,
    pub(crate) peak_rows_per_iteration: usize,
    pub(crate) last_rows_applied: usize,
    pub(crate) completed: usize,
    pub(crate) cancelled: usize,
}

#[derive(Debug, Default)]
struct ThreadModelUpdateMetrics {
    busy: Cell<bool>,
    peak_rows_per_iteration: Cell<usize>,
    last_rows_applied: Cell<usize>,
    completed: Cell<usize>,
    cancelled: Cell<usize>,
}

impl ThreadModelUpdateMetrics {
    fn record_iteration(&self, rows: usize) {
        self.last_rows_applied.set(rows);
        self.peak_rows_per_iteration
            .set(self.peak_rows_per_iteration.get().max(rows));
    }
}

pub(crate) type ThreadRowProvider = Rc<dyn Fn(usize) -> Option<ThreadRowSnapshot>>;
pub(crate) type MultiSelectHandler = Rc<dyn Fn(&str) -> bool>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum ThreadModelRow {
    Thread { index: usize },
    Status { message: String, spinning: bool },
}

#[derive(Clone)]
pub(crate) struct ThreadListController {
    root: gtk::Box,
    list: gtk::ListView,
    model: gtk::StringList,
    selection: gtk::SingleSelection,
    selection_refreshing: Rc<Cell<bool>>,
    scroll_generation: Rc<Cell<u64>>,
    model_update_generation: Rc<Cell<u64>>,
    refresh_request_generation: Rc<Cell<u64>>,
    model_update_metrics: Rc<ThreadModelUpdateMetrics>,
    result_label: gtk::Label,
    load_more_button: gtk::Button,
    scrolled: gtk::ScrolledWindow,
}

impl ThreadListController {
    pub(crate) fn new(row_provider: ThreadRowProvider, multi_select: MultiSelectHandler) -> Self {
        let model = gtk::StringList::new(&[]);
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let factory = thread_list_factory(row_provider, multi_select);
        let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
        list.set_widget_name("notm-thread-list");
        list.set_single_click_activate(false);
        list.set_hexpand(true);
        list.set_vexpand(true);
        let scrolled = gtk::ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&list)
            .build();

        let result_label = gtk::Label::new(Some("No results loaded"));
        result_label.set_widget_name("notm-thread-result-label");
        result_label.set_xalign(0.0);
        result_label.set_hexpand(true);
        let load_more_button = gtk::Button::with_label("Load more");
        load_more_button.set_widget_name("notm-load-more-threads-button");
        load_more_button.set_sensitive(false);
        let result_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        result_row.append(&result_label);
        result_row.append(&load_more_button);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.append(&scrolled);
        root.append(&result_row);

        Self {
            root,
            list,
            model,
            selection,
            selection_refreshing: Rc::new(Cell::new(false)),
            scroll_generation: Rc::new(Cell::new(0)),
            model_update_generation: Rc::new(Cell::new(0)),
            refresh_request_generation: Rc::new(Cell::new(0)),
            model_update_metrics: Rc::new(ThreadModelUpdateMetrics::default()),
            result_label,
            load_more_button,
            scrolled,
        }
    }

    pub(crate) fn root(&self) -> gtk::Box {
        self.root.clone()
    }

    pub(crate) fn list(&self) -> gtk::ListView {
        self.list.clone()
    }

    pub(crate) fn scrolled(&self) -> gtk::ScrolledWindow {
        self.scrolled.clone()
    }

    pub(crate) fn load_more_button(&self) -> gtk::Button {
        self.load_more_button.clone()
    }

    pub(crate) fn set_result_label(&self, text: &str) {
        self.result_label.set_text(text);
    }

    pub(crate) fn set_load_more_state(&self, label: &str, sensitive: bool) {
        self.load_more_button.set_label(label);
        self.load_more_button.set_sensitive(sensitive);
    }

    pub(crate) fn set_load_more_sensitive(&self, sensitive: bool) {
        self.load_more_button.set_sensitive(sensitive);
    }

    pub(crate) fn connect_activate<F>(&self, callback: F)
    where
        F: Fn(usize) + 'static,
    {
        let model = self.model.downgrade();
        self.list.connect_activate(move |_, position| {
            let Some(model) = model.upgrade() else {
                return;
            };
            if let Some(index) = model
                .string(position)
                .and_then(|token| thread_index_from_model_token(&token))
            {
                callback(index);
            }
        });
    }

    pub(crate) fn connect_selection_changed<F>(&self, callback: F)
    where
        F: Fn(Option<usize>) + 'static,
    {
        let selection_refreshing = self.selection_refreshing.clone();
        self.selection.connect_selected_notify(move |selection| {
            if !selection_refreshing.get() {
                let selected = selection
                    .selected_item()
                    .and_downcast::<gtk::StringObject>()
                    .and_then(|item| thread_index_from_model_token(&item.string()));
                callback(selected);
            }
        });
    }

    pub(crate) fn connect_load_more<F>(&self, callback: F)
    where
        F: Fn() + 'static,
    {
        self.load_more_button.connect_clicked(move |_| callback());
    }

    pub(crate) fn connect_auto_load_more<F, G, H>(
        &self,
        load_state: F,
        on_scheduled: G,
        callback: H,
    ) where
        F: Fn() -> (bool, u64, usize) + 'static,
        G: Fn() + 'static,
        H: Fn() + 'static,
    {
        let last_auto_load = Rc::new(Cell::new((u64::MAX, usize::MAX)));
        let scheduled = Rc::new(Cell::new(false));
        let on_scheduled = Rc::new(on_scheduled);
        let callback = Rc::new(callback);
        self.scrolled
            .vadjustment()
            .connect_value_changed(move |adjustment| {
                let upper = adjustment.upper();
                let page = adjustment.page_size();
                let value = adjustment.value();
                if !(upper <= page + 24.0 || value + page + 24.0 >= upper) {
                    return;
                }
                let (can_load, generation, offset) = load_state();
                let key = (generation, offset);
                if !can_load || last_auto_load.get() == key || scheduled.get() {
                    return;
                }
                last_auto_load.set(key);
                scheduled.set(true);
                on_scheduled();
                let scheduled = scheduled.clone();
                let callback = callback.clone();
                gtk::glib::timeout_add_local_once(Duration::from_millis(120), move || {
                    scheduled.set(false);
                    callback();
                });
            });
    }

    pub(crate) fn show_loading(&self, message: &str) {
        self.set_status_row(message, true);
    }

    pub(crate) fn show_message(&self, message: &str) {
        self.set_status_row(message, false);
    }

    fn set_status_row(&self, message: &str, spinning: bool) {
        self.cancel_model_update();
        let generation = self.model_update_generation.get().saturating_add(1);
        self.model_update_generation.set(generation);
        self.model_update_metrics.busy.set(true);
        let token = thread_status_token(message, spinning);
        let model = self.model.clone();
        let selection = self.selection.clone();
        let selection_refreshing = self.selection_refreshing.clone();
        let update_generation = self.model_update_generation.clone();
        let metrics = self.model_update_metrics.clone();
        gtk::glib::idle_add_local(move || {
            if update_generation.get() != generation {
                return gtk::glib::ControlFlow::Break;
            }
            let count = (model.n_items() as usize).min(THREAD_MODEL_ROWS_PER_UPDATE);
            selection_refreshing.set(true);
            if count > 0 {
                model.splice(0, count as u32, &[]);
            }
            let finished = count == 0;
            if finished {
                model.append(&token);
                selection.set_selected(gtk::INVALID_LIST_POSITION);
            }
            selection_refreshing.set(false);
            metrics.record_iteration(count.max(usize::from(finished)));
            if finished {
                metrics.busy.set(false);
                metrics
                    .completed
                    .set(metrics.completed.get().saturating_add(1));
                gtk::glib::ControlFlow::Break
            } else {
                gtk::glib::ControlFlow::Continue
            }
        });
    }

    pub(crate) fn refresh_rows(
        &self,
        snapshot: &ThreadModelSnapshot,
        indices: &[usize],
        force: bool,
    ) {
        let refresh_generation = self.refresh_request_generation.get().saturating_add(1);
        self.refresh_request_generation.set(refresh_generation);
        if self.model_update_metrics.busy.get() {
            let model_generation = self.model_update_generation.get();
            let controller = self.clone();
            let snapshot = snapshot.clone();
            let indices = indices.to_vec();
            gtk::glib::idle_add_local(move || {
                if controller.refresh_request_generation.get() != refresh_generation
                    || controller.model_update_generation.get() != model_generation
                {
                    return gtk::glib::ControlFlow::Break;
                }
                if controller.model_update_metrics.busy.get() {
                    return gtk::glib::ControlFlow::Continue;
                }
                controller.refresh_rows_ready(&snapshot, &indices, force);
                gtk::glib::ControlFlow::Break
            });
            return;
        }
        self.refresh_rows_ready(snapshot, indices, force);
    }

    fn refresh_rows_ready(&self, snapshot: &ThreadModelSnapshot, indices: &[usize], force: bool) {
        if indices.len() > THREAD_MODEL_ROWS_PER_UPDATE {
            self.refresh_rows_bounded(snapshot, indices, force);
            return;
        }
        self.refresh_rows_now(snapshot, indices, force);
    }

    fn refresh_rows_now(&self, snapshot: &ThreadModelSnapshot, indices: &[usize], force: bool) {
        let selected = self.selected_position();
        self.selection_refreshing.set(true);
        for index in indices
            .iter()
            .copied()
            .filter(|index| *index < snapshot.len)
        {
            let position = index as u32;
            if position < self.model.n_items() {
                let token = thread_row_token(
                    index,
                    snapshot.marked_indices.contains(&index),
                    snapshot.display,
                );
                // Visual-selection refresh can run from `notify::selected`. Preserve
                // unchanged item identities there: splicing every row makes
                // GtkListView and GtkSingleSelection rebuild the model while they
                // are handling the selection change.
                if force
                    || self
                        .model
                        .string(position)
                        .is_none_or(|current| current.as_str() != token)
                {
                    self.model.splice(position, 1, &[token.as_str()]);
                }
            }
        }
        if let Some(position) = selected
            && position < self.model.n_items()
            && self.selection.selected() != position
        {
            self.selection.set_selected(position);
        }
        self.selection_refreshing.set(false);
    }

    fn refresh_rows_bounded(&self, snapshot: &ThreadModelSnapshot, indices: &[usize], force: bool) {
        self.cancel_model_update();
        let generation = self.model_update_generation.get().saturating_add(1);
        self.model_update_generation.set(generation);
        self.model_update_metrics.busy.set(true);
        self.model_update_metrics.last_rows_applied.set(0);
        let snapshot = snapshot.clone();
        let indices = indices
            .iter()
            .copied()
            .filter(|index| *index < snapshot.len)
            .collect::<Vec<_>>();
        let model = self.model.clone();
        let selection = self.selection.clone();
        let selection_refreshing = self.selection_refreshing.clone();
        let update_generation = self.model_update_generation.clone();
        let metrics = self.model_update_metrics.clone();
        let selected = self.selected_position();
        let mut next = 0_usize;
        gtk::glib::idle_add_local(move || {
            if update_generation.get() != generation {
                return gtk::glib::ControlFlow::Break;
            }
            let end = next
                .saturating_add(THREAD_MODEL_ROWS_PER_UPDATE)
                .min(indices.len());
            selection_refreshing.set(true);
            for index in indices[next..end].iter().copied() {
                let position = index as u32;
                if position >= model.n_items() {
                    continue;
                }
                let token = thread_row_token(
                    index,
                    snapshot.marked_indices.contains(&index),
                    snapshot.display,
                );
                if force
                    || model
                        .string(position)
                        .is_none_or(|current| current.as_str() != token)
                {
                    model.splice(position, 1, &[token.as_str()]);
                }
            }
            if let Some(position) = selected
                && position < model.n_items()
                && selection.selected() != position
            {
                selection.set_selected(position);
            }
            selection_refreshing.set(false);
            metrics.record_iteration(end.saturating_sub(next));
            next = end;
            if next < indices.len() {
                gtk::glib::ControlFlow::Continue
            } else {
                metrics.busy.set(false);
                metrics
                    .completed
                    .set(metrics.completed.get().saturating_add(1));
                gtk::glib::ControlFlow::Break
            }
        });
    }

    pub(crate) fn apply_model_update(
        &self,
        snapshot: &ThreadModelSnapshot,
        update: ThreadModelUpdate,
    ) {
        self.apply_model_update_then(snapshot, update, |_| {});
    }

    pub(crate) fn apply_model_update_then<F>(
        &self,
        snapshot: &ThreadModelSnapshot,
        update: ThreadModelUpdate,
        complete: F,
    ) where
        F: FnOnce(bool) + 'static,
    {
        self.cancel_model_update();
        let generation = self.model_update_generation.get().saturating_add(1);
        self.model_update_generation.set(generation);
        self.model_update_metrics.busy.set(true);
        self.model_update_metrics.last_rows_applied.set(0);

        let replacing = matches!(update, ThreadModelUpdate::Replace);
        let (mut next_add, end_add, refresh_end) = match update {
            ThreadModelUpdate::Replace => (0, snapshot.len, 0),
            ThreadModelUpdate::Append { start, count } => (
                start,
                start.saturating_add(count).min(snapshot.len),
                start.min(snapshot.len),
            ),
        };
        let snapshot = snapshot.clone();
        let model = self.model.clone();
        let selection = self.selection.clone();
        let selection_refreshing = self.selection_refreshing.clone();
        let selected = (!replacing).then(|| self.selected_position()).flatten();
        let update_generation = self.model_update_generation.clone();
        let metrics = self.model_update_metrics.clone();
        let mut removing = replacing;
        let mut next_refresh = 0_usize;
        let mut complete = Some(complete);
        gtk::glib::idle_add_local(move || {
            if update_generation.get() != generation {
                if let Some(complete) = complete.take() {
                    complete(false);
                }
                return gtk::glib::ControlFlow::Break;
            }

            let mut budget = THREAD_MODEL_ROWS_PER_UPDATE;
            let mut applied = 0_usize;
            selection_refreshing.set(true);
            if removing && budget > 0 {
                let count = (model.n_items() as usize).min(budget);
                if count > 0 {
                    model.splice(0, count as u32, &[]);
                    applied = applied.saturating_add(count);
                    budget -= count;
                }
                removing = model.n_items() > 0;
            }

            if !removing && next_add < end_add && budget > 0 {
                let end = next_add.saturating_add(budget).min(end_add);
                let tokens = (next_add..end)
                    .map(|index| {
                        thread_row_token(
                            index,
                            snapshot.marked_indices.contains(&index),
                            snapshot.display,
                        )
                    })
                    .collect::<Vec<_>>();
                let additions = tokens.iter().map(String::as_str).collect::<Vec<_>>();
                model.splice(model.n_items(), 0, &additions);
                let count = end.saturating_sub(next_add);
                next_add = end;
                applied = applied.saturating_add(count);
                budget -= count;
            }

            while !removing && next_add >= end_add && next_refresh < refresh_end && budget > 0 {
                let index = next_refresh;
                next_refresh += 1;
                budget -= 1;
                applied = applied.saturating_add(1);
                let position = index as u32;
                if position >= model.n_items() {
                    continue;
                }
                let token = thread_row_token(
                    index,
                    snapshot.marked_indices.contains(&index),
                    snapshot.display,
                );
                if model
                    .string(position)
                    .is_none_or(|current| current.as_str() != token)
                {
                    model.splice(position, 1, &[token.as_str()]);
                }
            }
            if let Some(position) = selected
                && position < model.n_items()
                && selection.selected() != position
            {
                selection.set_selected(position);
            }
            selection_refreshing.set(false);
            metrics.record_iteration(applied);

            if removing || next_add < end_add || next_refresh < refresh_end {
                gtk::glib::ControlFlow::Continue
            } else {
                metrics.busy.set(false);
                metrics
                    .completed
                    .set(metrics.completed.get().saturating_add(1));
                if let Some(complete) = complete.take() {
                    complete(true);
                }
                gtk::glib::ControlFlow::Break
            }
        });
    }

    pub(crate) fn cancel_model_update(&self) {
        self.model_update_generation
            .set(self.model_update_generation.get().saturating_add(1));
        if self.model_update_metrics.busy.replace(false) {
            self.model_update_metrics
                .cancelled
                .set(self.model_update_metrics.cancelled.get().saturating_add(1));
        }
    }

    pub(crate) fn model_update_snapshot(&self) -> ThreadModelRuntimeSnapshot {
        ThreadModelRuntimeSnapshot {
            generation: self.model_update_generation.get(),
            busy: self.model_update_metrics.busy.get(),
            model_len: self.model_len(),
            max_rows_per_update: THREAD_MODEL_ROWS_PER_UPDATE,
            peak_rows_per_iteration: self.model_update_metrics.peak_rows_per_iteration.get(),
            last_rows_applied: self.model_update_metrics.last_rows_applied.get(),
            completed: self.model_update_metrics.completed.get(),
            cancelled: self.model_update_metrics.cancelled.get(),
        }
    }

    pub(crate) fn model_len(&self) -> usize {
        self.model.n_items() as usize
    }

    pub(crate) fn selected_index(&self) -> Option<usize> {
        if let Some(position) = self.selected_position() {
            return self.index_at_position(position);
        }
        self.selection
            .selected_item()
            .and_downcast::<gtk::StringObject>()
            .and_then(|item| thread_index_from_model_token(&item.string()))
    }

    fn selected_position(&self) -> Option<u32> {
        let position = self.selection.selected();
        (position != gtk::INVALID_LIST_POSITION && position < self.model.n_items())
            .then_some(position)
    }

    pub(crate) fn index_at_position(&self, position: u32) -> Option<usize> {
        self.model
            .string(position)
            .and_then(|token| thread_index_from_model_token(&token))
    }

    pub(crate) fn select(&self, index: usize) {
        if index >= self.model_len() {
            return;
        }
        self.selection.set_selected(index as u32);
        self.scroll_into_view(index);
        self.focus();
    }

    pub(crate) fn select_silently(&self, index: usize) {
        if index >= self.model_len() {
            return;
        }
        self.selection_refreshing.set(true);
        self.selection.set_selected(index as u32);
        self.selection_refreshing.set(false);
        self.scroll_into_view(index);
    }

    pub(crate) fn clear_selection_silently(&self) {
        self.selection_refreshing.set(true);
        self.selection.set_selected(gtk::INVALID_LIST_POSITION);
        self.selection_refreshing.set(false);
    }

    pub(crate) fn visible_row_count(&self) -> isize {
        let row_height = 64.0;
        (self.scrolled.vadjustment().page_size() / row_height)
            .floor()
            .max(1.0) as isize
    }

    pub(crate) fn focus(&self) {
        self.list.grab_focus();
    }

    pub(crate) fn scroll_into_view(&self, index: usize) {
        if index >= self.model_len() {
            return;
        }
        let generation = self.scroll_generation.get().saturating_add(1);
        self.scroll_generation.set(generation);
        scroll_thread_index_into_view_once(&self.list, index);
        let scrolled = self.scrolled.clone();
        let list = self.list.clone();
        let scroll_generation = self.scroll_generation.clone();
        gtk::glib::idle_add_local_once(move || {
            if scroll_generation.get() != generation {
                return;
            }
            scroll_thread_index_into_view_once(&list, index);
            for delay_ms in [25_u64, 75, 160] {
                let scrolled = scrolled.clone();
                let list = list.clone();
                let scroll_generation = scroll_generation.clone();
                gtk::glib::timeout_add_local_once(Duration::from_millis(delay_ms), move || {
                    if scroll_generation.get() != generation {
                        return;
                    }
                    scroll_thread_index_into_view_once(&list, index);
                    nudge_realized_thread_row_into_view(&scrolled, &list, index);
                });
            }
        });
    }

    pub(crate) fn set_result(&self, text: &str, button_label: &str, can_load_more: bool) {
        self.result_label.set_text(text);
        self.load_more_button.set_label(button_label);
        self.load_more_button.set_sensitive(can_load_more);
    }

    pub(crate) fn selection_view_state(&self, window_offset: usize) -> serde_json::Value {
        let adjustment = self.scrolled.vadjustment();
        let value = adjustment.value();
        let page = visible_adjustment_page_size(&adjustment, &self.scrolled);
        let selected_local = self.selected_index();
        let selected_absolute = selected_local.map(|index| window_offset + index);
        let relative_to = self.scrolled.clone().upcast::<gtk::Widget>();
        let bounds = selected_local
            .and_then(|index| realized_thread_row_bounds_relative(&self.list, &relative_to, index));
        let (row_top, row_bottom, row_visible) = if let Some((top, bottom)) = bounds {
            (
                Some(top),
                Some(bottom),
                Some(top >= -1.0 && bottom <= page + 1.0),
            )
        } else {
            (None, None, None)
        };
        json!({
            "ok": true,
            "selected_local": selected_local,
            "selected_abs": selected_absolute,
            "scroll_value": value,
            "scroll_upper": adjustment.upper(),
            "scroll_page_size": page,
            "row_top": row_top,
            "row_bottom": row_bottom,
            "row_visible": row_visible,
        })
    }

    pub(crate) fn row_layout_state(&self, index: usize) -> serde_json::Value {
        let adjustment = self.scrolled.vadjustment();
        let viewport_height = visible_adjustment_page_size(&adjustment, &self.scrolled);
        let viewport_width = self.scrolled.width().max(0) as f64;
        let relative_to = self.scrolled.clone().upcast::<gtk::Widget>();
        let root = self.list.clone().upcast::<gtk::Widget>();
        json!({
            "ok": true,
            "index": index,
            "viewport_width": viewport_width,
            "viewport_height": viewport_height,
            "row": named_widget_bounds_json(&root, &relative_to, &format!("notm-thread-row-{index}"), viewport_width, viewport_height),
            "number": named_widget_bounds_json(&root, &relative_to, &format!("notm-thread-number-{index}"), viewport_width, viewport_height),
            "title": named_widget_bounds_json(&root, &relative_to, &format!("notm-thread-title-{index}"), viewport_width, viewport_height),
            "date": named_widget_bounds_json(&root, &relative_to, &format!("notm-thread-date-{index}"), viewport_width, viewport_height),
            "meta": named_widget_bounds_json(&root, &relative_to, &format!("notm-thread-meta-{index}"), viewport_width, viewport_height),
            "preview": named_widget_bounds_json(&root, &relative_to, &format!("notm-thread-preview-{index}"), viewport_width, viewport_height),
        })
    }
}

#[cfg(test)]
pub(crate) fn execute_search_page(
    open_config: &OpenConfig,
    query: &str,
    offset: usize,
    limit: usize,
    excluded_tags: Vec<String>,
) -> anyhow::Result<SearchData> {
    let cancelled = AtomicBool::new(false);
    execute_search_page_with_cancel(open_config, query, offset, limit, excluded_tags, &cancelled)
}

fn execute_search_page_with_cancel(
    open_config: &OpenConfig,
    query: &str,
    offset: usize,
    limit: usize,
    excluded_tags: Vec<String>,
    cancelled: &AtomicBool,
) -> anyhow::Result<SearchData> {
    anyhow::ensure!(
        (1..=MAX_SEARCH_PAGE_SIZE).contains(&limit),
        "search page size must be between 1 and {MAX_SEARCH_PAGE_SIZE}; got {limit}"
    );
    ensure_search_not_cancelled(cancelled)?;
    // Capture the epoch before opening the database. A mutation that commits
    // afterward bumps the epoch, so this worker cannot publish its older
    // snapshot under the post-mutation cache generation.
    let cache_epoch = CACHE_EPOCH.load(Ordering::Acquire);
    let db = Database::open(open_config, DatabaseMode::ReadOnly)?;
    ensure_search_not_cancelled(cancelled)?;
    let revision = db.revision();
    let db_path = db.path();
    let key = search_cache_key(
        query,
        &db_path,
        &revision,
        offset,
        limit,
        excluded_tags.clone(),
        cache_epoch,
    );
    let cached = {
        let mut cache = search_cache().lock().expect("search cache lock");
        cache.get(&key).cloned()
    };
    if let Some(mut cached) = cached {
        ensure_search_not_cancelled(cancelled)?;
        cached.cached = true;
        return Ok(cached);
    }

    let mut tags = db.all_tags();
    ensure_search_not_cancelled(cancelled)?;
    tags.truncate(MAX_VISIBLE_SEARCH_TAGS);
    let options = QueryOptions {
        limit,
        offset,
        sort: SortOrder::NewestFirst,
        excluded_tags,
    };
    let threads = db.search_threads(query, &options)?;
    ensure_search_not_cancelled(cancelled)?;
    let count = db
        .count_threads(query, &options)
        .unwrap_or(threads.len() as u32);
    ensure_search_not_cancelled(cancelled)?;
    let details =
        thread_details_for_threads(&db, &db_path, &revision, &threads, cache_epoch, cancelled)?;
    ensure_search_not_cancelled(cancelled)?;
    let data = SearchData {
        query: query.to_string(),
        threads,
        details,
        count,
        offset,
        limit,
        tags,
        database_path: db_path,
        revision,
        cached: false,
    };
    ensure_search_not_cancelled(cancelled)?;
    {
        let mut cache = search_cache().lock().expect("search cache lock");
        ensure_search_not_cancelled(cancelled)?;
        cache.insert(key, data.clone());
    }
    Ok(data)
}

fn ensure_search_not_cancelled(cancelled: &AtomicBool) -> anyhow::Result<()> {
    anyhow::ensure!(
        !cancelled.load(Ordering::Acquire),
        "search preparation was cancelled"
    );
    Ok(())
}

fn sleep_search_delay(delay: Duration, cancelled: &AtomicBool) -> anyhow::Result<()> {
    let mut remaining = delay;
    while !remaining.is_zero() {
        ensure_search_not_cancelled(cancelled)?;
        let chunk = remaining.min(SEARCH_CANCELLATION_POLL_INTERVAL);
        thread::sleep(chunk);
        remaining = remaining.saturating_sub(chunk);
    }
    ensure_search_not_cancelled(cancelled)
}

pub(crate) fn format_count(value: usize) -> String {
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

pub(crate) fn page_offset_for_index(target_index: usize, page_size: usize) -> usize {
    let page_size = page_size.max(1);
    (target_index / page_size) * page_size
}

pub(crate) fn thread_window_status(offset: usize, loaded: usize, total: usize) -> String {
    if loaded == 0 {
        return format!("Loaded 0 of {} thread(s)", format_count(total));
    }
    let start = offset + 1;
    let end = offset + loaded;
    if offset == 0 {
        format!(
            "Loaded {} of {} thread(s)",
            format_count(loaded),
            format_count(total.max(loaded))
        )
    } else {
        format!(
            "Showing {}-{} of {} thread(s) ({} loaded)",
            format_count(start),
            format_count(end),
            format_count(total.max(end)),
            format_count(loaded)
        )
    }
}

fn thread_list_factory(
    row_provider: ThreadRowProvider,
    multi_select: MultiSelectHandler,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let Some(list_item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        list_item.set_selectable(true);
        list_item.set_activatable(true);
    });

    factory.connect_bind(move |_, item| {
        let Some(list_item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let token = list_item
            .item()
            .and_downcast::<gtk::StringObject>()
            .map(|item| item.string().to_string())
            .unwrap_or_default();
        match parse_thread_model_row(&token) {
            Some(ThreadModelRow::Thread { index }) => {
                if let Some(snapshot) = row_provider(index) {
                    let row = thread_row_widget(index, &snapshot);
                    connect_thread_row_multi_select(
                        &row,
                        &snapshot.thread.thread_id,
                        multi_select.clone(),
                    );
                    list_item.set_selectable(true);
                    list_item.set_activatable(true);
                    list_item.set_child(Some(&row));
                } else {
                    list_item.set_selectable(false);
                    list_item.set_activatable(false);
                    list_item.set_child(Some(&thread_status_widget(
                        "Message row is no longer available.",
                        false,
                    )));
                }
            }
            Some(ThreadModelRow::Status { message, spinning }) => {
                list_item.set_selectable(false);
                list_item.set_activatable(false);
                list_item.set_child(Some(&thread_status_widget(&message, spinning)));
            }
            None => {
                list_item.set_selectable(false);
                list_item.set_activatable(false);
                list_item.set_child(Some(&thread_status_widget("Invalid message row.", false)));
            }
        }
    });
    factory
}

fn thread_row_widget(index: usize, snapshot: &ThreadRowSnapshot) -> gtk::Box {
    let thread = &snapshot.thread;
    let detail = &snapshot.detail;
    let display = snapshot.display;
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 2);
    box_.set_widget_name(&format!("notm-thread-row-{index}"));
    box_.add_css_class("notm-thread-row");
    box_.set_hexpand(true);
    box_.set_halign(gtk::Align::Fill);
    if thread.has_unread {
        box_.add_css_class("unread");
    }
    if snapshot.visual_selected {
        box_.add_css_class("notm-visual-selected");
        box_.add_css_class("notm-multi-selected");
    }
    let row_content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row_content.set_hexpand(true);
    row_content.set_halign(gtk::Align::Fill);
    row_content.set_margin_start(6);
    row_content.set_margin_end(6);
    row_content.set_margin_top(6);
    row_content.set_margin_bottom(6);
    if display.numbers {
        let number = gtk::Label::new(Some(&format!(
            "{}.",
            format_count(snapshot.absolute_index + 1)
        )));
        number.set_widget_name(&format!("notm-thread-number-{index}"));
        number.set_xalign(0.0);
        number.set_yalign(0.0);
        number.set_valign(gtk::Align::Start);
        number.add_css_class("dim-label");
        number.add_css_class("monospace");
        number.add_css_class("notm-thread-number");
        row_content.append(&number);
    }
    let content = gtk::Box::new(gtk::Orientation::Vertical, 3);
    content.set_hexpand(true);
    content.set_halign(gtk::Align::Fill);
    let title = gtk::Label::new(Some(&thread_title_text(thread, detail)));
    title.set_widget_name(&format!("notm-thread-title-{index}"));
    title.set_xalign(0.0);
    title.set_hexpand(true);
    title.set_halign(gtk::Align::Fill);
    title.set_wrap(true);
    title.set_tooltip_text(detail.load_warning.as_deref());
    let meta_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    meta_row.set_hexpand(true);
    meta_row.set_halign(gtk::Align::Fill);
    if display.dates {
        let date = gtk::Label::new(Some(&format_thread_list_date(thread.newest_date)));
        date.set_widget_name(&format!("notm-thread-date-{index}"));
        date.set_width_chars(16);
        date.set_xalign(1.0);
        date.set_yalign(0.0);
        date.set_valign(gtk::Align::Start);
        date.add_css_class("dim-label");
        date.add_css_class("monospace");
        date.add_css_class("notm-thread-date");
        meta_row.append(&date);
    }
    let meta_text = if display.tags {
        format!(
            "{}  ·  {}/{}  ·  {}",
            thread.authors,
            thread.matched_messages,
            thread.total_messages,
            thread.tags.join(" ")
        )
    } else {
        format!(
            "{}  ·  {}/{}",
            thread.authors, thread.matched_messages, thread.total_messages
        )
    };
    let meta = gtk::Label::new(Some(&meta_text));
    meta.set_widget_name(&format!("notm-thread-meta-{index}"));
    meta.set_xalign(0.0);
    meta.set_hexpand(true);
    meta.set_halign(gtk::Align::Fill);
    meta.add_css_class("dim-label");
    meta.set_wrap(true);
    content.append(&title);
    meta_row.append(&meta);
    content.append(&meta_row);
    if display.preview && !detail.preview.is_empty() {
        let preview = gtk::Label::new(Some(&detail.preview));
        preview.set_widget_name(&format!("notm-thread-preview-{index}"));
        preview.set_xalign(0.0);
        preview.set_hexpand(true);
        preview.set_halign(gtk::Align::Fill);
        preview.add_css_class("dim-label");
        preview.set_wrap(true);
        preview.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
        preview.set_lines(
            i32::try_from(display.preview_lines)
                .expect("validated thread preview line count fits in i32"),
        );
        content.append(&preview);
    }
    row_content.append(&content);
    box_.append(&row_content);
    box_
}

fn thread_title_text(thread: &ThreadSummary, detail: &ThreadUiDetails) -> String {
    format!(
        "{}{}{}{}{}{}{}",
        if thread.has_unread { "● " } else { "" },
        if thread.is_flagged { "★ " } else { "" },
        if detail.has_attachment { "📎 " } else { "" },
        if detail.has_encrypted { "🔒 " } else { "" },
        if detail.has_signed { "✍ " } else { "" },
        if detail.load_warning.is_some() {
            "⚠ "
        } else {
            ""
        },
        thread.subject
    )
}

fn connect_thread_row_multi_select(row: &gtk::Box, thread_id: &str, handler: MultiSelectHandler) {
    let click = gtk::GestureClick::new();
    click.set_button(0);
    let row_for_click = row.clone();
    let id = thread_id.to_string();
    click.connect_pressed(move |gesture, _, _, _| {
        if !gesture
            .current_event_state()
            .contains(gtk::gdk::ModifierType::CONTROL_MASK)
        {
            return;
        }
        if handler(&id) {
            row_for_click.add_css_class("notm-multi-selected");
            row_for_click.add_css_class("notm-visual-selected");
        } else {
            row_for_click.remove_css_class("notm-multi-selected");
            row_for_click.remove_css_class("notm-visual-selected");
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    row.add_controller(click);
}

fn thread_status_widget(message: &str, spinning: bool) -> gtk::Box {
    let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    box_.set_widget_name(if spinning {
        "notm-thread-loading-row"
    } else {
        "notm-thread-message-row"
    });
    box_.set_hexpand(true);
    box_.set_halign(gtk::Align::Fill);
    box_.set_margin_start(12);
    box_.set_margin_end(12);
    box_.set_margin_top(12);
    box_.set_margin_bottom(12);
    box_.set_valign(gtk::Align::Center);
    if spinning {
        let spinner = gtk::Spinner::new();
        spinner.set_widget_name("notm-thread-loading-spinner");
        spinner.start();
        box_.append(&spinner);
    }
    let label = gtk::Label::new(Some(message));
    label.set_widget_name("notm-thread-loading-label");
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class("dim-label");
    box_.append(&label);
    box_
}

fn thread_row_token(index: usize, visual_selected: bool, display: ThreadListDisplay) -> String {
    format!(
        "{THREAD_ROW_PREFIX}|{index}|{}|{}",
        if visual_selected { 1 } else { 0 },
        display.token_bits()
    )
}

fn thread_status_token(message: &str, spinning: bool) -> String {
    format!(
        "{THREAD_STATUS_PREFIX}|{}|{message}",
        if spinning { 1 } else { 0 }
    )
}

fn parse_thread_model_row(token: &str) -> Option<ThreadModelRow> {
    let mut parts = token.splitn(4, '|');
    match parts.next()? {
        THREAD_ROW_PREFIX => Some(ThreadModelRow::Thread {
            index: parts.next()?.parse().ok()?,
        }),
        THREAD_STATUS_PREFIX => Some(ThreadModelRow::Status {
            spinning: parts.next().is_some_and(|value| value == "1"),
            message: parts.next().unwrap_or_default().to_string(),
        }),
        _ => None,
    }
}

fn thread_index_from_model_token(token: &str) -> Option<usize> {
    match parse_thread_model_row(token)? {
        ThreadModelRow::Thread { index } => Some(index),
        ThreadModelRow::Status { .. } => None,
    }
}

fn scroll_thread_index_into_view_once(list: &gtk::ListView, index: usize) {
    list.scroll_to(index as u32, gtk::ListScrollFlags::NONE, None);
}

fn nudge_realized_thread_row_into_view(
    scrolled: &gtk::ScrolledWindow,
    list: &gtk::ListView,
    index: usize,
) {
    let relative_to = scrolled.clone().upcast::<gtk::Widget>();
    let Some((top, bottom)) = realized_thread_row_bounds_relative(list, &relative_to, index) else {
        return;
    };
    let adjustment = scrolled.vadjustment();
    let lower = adjustment.lower();
    let page = visible_adjustment_page_size(&adjustment, scrolled);
    let max_value = (adjustment.upper() - page).max(lower);
    if max_value <= lower {
        return;
    }
    let value = adjustment.value();
    let padding = 12.0;
    let delta = if top < padding {
        top - padding
    } else if bottom > page - padding {
        bottom - page + padding
    } else {
        return;
    };
    adjustment.set_value((value + delta).clamp(lower, max_value));
}

fn visible_adjustment_page_size(
    adjustment: &gtk::Adjustment,
    scrolled: &gtk::ScrolledWindow,
) -> f64 {
    let page = adjustment.page_size();
    if page > 0.0 {
        page
    } else {
        scrolled.height().max(0) as f64
    }
}

fn realized_thread_row_bounds_relative(
    list: &gtk::ListView,
    relative_to: &gtk::Widget,
    index: usize,
) -> Option<(f64, f64)> {
    let root = list.clone().upcast::<gtk::Widget>();
    let row = find_widget_by_name(&root, &format!("notm-thread-row-{index}"))?;
    let bounds = row.compute_bounds(relative_to)?;
    let top = bounds.y() as f64;
    let bottom = top + bounds.height() as f64;
    (bottom > top).then_some((top, bottom))
}

pub(crate) fn find_widget_by_name(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if root.widget_name().as_str() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_widget_by_name(&widget, name) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn named_widget_bounds_json(
    root: &gtk::Widget,
    relative_to: &gtk::Widget,
    name: &str,
    viewport_width: f64,
    viewport_height: f64,
) -> Option<serde_json::Value> {
    let widget = find_widget_by_name(root, name)?;
    let bounds = widget.compute_bounds(relative_to)?;
    let x = bounds.x() as f64;
    let y = bounds.y() as f64;
    let width = bounds.width() as f64;
    let height = bounds.height() as f64;
    let right = x + width;
    let bottom = y + height;
    Some(json!({
        "x": x,
        "y": y,
        "width": width,
        "height": height,
        "right": right,
        "bottom": bottom,
        "fully_visible": x >= -1.0 && y >= -1.0 && right <= viewport_width + 1.0 && bottom <= viewport_height + 1.0,
    }))
}

pub(crate) fn format_thread_list_date(timestamp: i64) -> String {
    chrono::DateTime::<Utc>::from_timestamp(timestamp, 0)
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| timestamp.to_string())
}

fn thread_details_for_threads(
    db: &Database,
    database_path: &str,
    revision: &Revision,
    threads: &[ThreadSummary],
    cache_epoch: u64,
    cancelled: &AtomicBool,
) -> anyhow::Result<BTreeMap<String, ThreadUiDetails>> {
    let mut out = BTreeMap::new();
    let mut missing = Vec::new();
    for thread in threads {
        ensure_search_not_cancelled(cancelled)?;
        let key = thread_detail_cache_key(database_path, revision, &thread.thread_id, cache_epoch);
        let cached = {
            let mut cache = thread_detail_cache()
                .lock()
                .expect("thread detail cache lock");
            cache.get(&key).cloned()
        };
        if let Some(detail) = cached {
            out.insert(thread.thread_id.clone(), detail);
            continue;
        }
        missing.push((thread, key));
    }

    let thread_ids = missing
        .iter()
        .map(|(thread, _)| thread.thread_id.clone())
        .collect::<Vec<_>>();
    ensure_search_not_cancelled(cancelled)?;
    let messages_by_thread = match db.thread_messages_for_threads_bounded(
        &thread_ids,
        MAX_THREAD_DETAIL_MESSAGES,
        THREAD_DETAIL_BATCH_MESSAGE_LIMIT,
    ) {
        Ok(messages) => messages,
        Err(error) => {
            for (thread, key) in missing {
                ensure_search_not_cancelled(cancelled)?;
                let detail = unavailable_thread_detail(&error);
                thread_detail_cache()
                    .lock()
                    .expect("thread detail cache lock")
                    .insert(key, detail.clone());
                out.insert(thread.thread_id.clone(), detail);
            }
            return Ok(out);
        }
    };
    ensure_search_not_cancelled(cancelled)?;
    for (thread, key) in missing {
        ensure_search_not_cancelled(cancelled)?;
        let detail = messages_by_thread
            .get(&thread.thread_id)
            .map(|outcome| match outcome {
                BoundedThreadMessages::Loaded(messages) => {
                    compute_thread_detail(messages, cancelled)
                }
                BoundedThreadMessages::ThreadLimitExceeded { .. }
                | BoundedThreadMessages::BatchLimitExceeded { .. } => {
                    Ok(unavailable_thread_detail(outcome))
                }
            })
            .transpose()?
            .unwrap_or_default();
        ensure_search_not_cancelled(cancelled)?;
        {
            let mut cache = thread_detail_cache()
                .lock()
                .expect("thread detail cache lock");
            ensure_search_not_cancelled(cancelled)?;
            cache.insert(key, detail.clone());
        }
        out.insert(thread.thread_id.clone(), detail);
    }
    Ok(out)
}

fn unavailable_thread_detail(error: &impl std::fmt::Display) -> ThreadUiDetails {
    ThreadUiDetails {
        load_warning: Some(format!("Thread details unavailable: {error}")),
        ..ThreadUiDetails::default()
    }
}

fn compute_thread_detail(
    messages: &[MessageSummary],
    cancelled: &AtomicBool,
) -> anyhow::Result<ThreadUiDetails> {
    compute_thread_detail_with_reader(messages, cancelled, read_thread_detail_source)
}

fn compute_thread_detail_with_reader<F>(
    messages: &[MessageSummary],
    cancelled: &AtomicBool,
    mut read: F,
) -> anyhow::Result<ThreadUiDetails>
where
    F: FnMut(&Path) -> anyhow::Result<Vec<u8>>,
{
    let mut detail = ThreadUiDetails::default();
    for message in messages {
        ensure_search_not_cancelled(cancelled)?;
        for filename in &message.filenames {
            ensure_search_not_cancelled(cancelled)?;
            let bytes = match read(Path::new(filename)) {
                Ok(bytes) => bytes,
                Err(_) => {
                    ensure_search_not_cancelled(cancelled)?;
                    continue;
                }
            };
            ensure_search_not_cancelled(cancelled)?;
            if let Ok(parsed) = notm_mail::mime::parse_rfc5322(&bytes) {
                ensure_search_not_cancelled(cancelled)?;
                detail.has_encrypted |= parsed.classification.has_encrypted();
                detail.has_signed |= parsed.classification.has_signed();
                detail.has_attachment |= !parsed.attachments.is_empty();
                if detail.preview.is_empty() {
                    detail.preview = body_preview(&parsed.safe_body);
                }
            }
            ensure_search_not_cancelled(cancelled)?;
            break;
        }
    }
    Ok(detail)
}

fn read_thread_detail_source(path: &Path) -> anyhow::Result<Vec<u8>> {
    let file = File::open(path)?;
    let limit = u64::try_from(THREAD_DETAIL_SOURCE_MAX_BYTES)
        .unwrap_or(u64::MAX - 1)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(THREAD_DETAIL_SOURCE_MAX_BYTES.min(256 * 1024));
    file.take(limit).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= THREAD_DETAIL_SOURCE_MAX_BYTES,
        "thread detail source exceeds the {THREAD_DETAIL_SOURCE_MAX_BYTES}-byte responsive preview limit"
    );
    Ok(bytes)
}

fn body_preview(body: &str) -> String {
    let mut preview = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('>'))
        .collect::<Vec<_>>()
        .join(" ");
    if preview.chars().count() > THREAD_PREVIEW_CACHE_MAX_CHARS {
        preview = preview
            .chars()
            .take(THREAD_PREVIEW_CACHE_MAX_CHARS.saturating_sub(1))
            .collect::<String>();
        preview.push('…');
    }
    preview
}

fn search_cache_key(
    query: &str,
    db_path: &str,
    revision: &Revision,
    offset: usize,
    limit: usize,
    excluded_tags: Vec<String>,
    cache_epoch: u64,
) -> SearchCacheKey {
    SearchCacheKey {
        cache_epoch,
        database_path: db_path.to_string(),
        database_uuid: revision.uuid.clone(),
        database_revision: revision.revision,
        query: query.to_string(),
        offset,
        limit,
        excluded_tags,
    }
}

fn thread_detail_cache_key(
    db_path: &str,
    revision: &Revision,
    thread_id: &str,
    cache_epoch: u64,
) -> ThreadDetailCacheKey {
    ThreadDetailCacheKey {
        cache_epoch,
        database_path: db_path.to_string(),
        database_uuid: revision.uuid.clone(),
        database_revision: revision.revision,
        thread_id: thread_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::Instant};

    fn test_revision(uuid: &str, revision: u64) -> Revision {
        Revision {
            uuid: uuid.to_string(),
            revision,
        }
    }

    fn test_thread(id: &str) -> ThreadSummary {
        ThreadSummary {
            thread_id: id.to_string(),
            subject: format!("subject {id}"),
            authors: "Author".to_string(),
            oldest_date: 1,
            newest_date: 2,
            matched_messages: 1,
            total_messages: 1,
            tags: vec!["inbox".to_string()],
            has_unread: false,
            is_flagged: false,
        }
    }

    fn test_message(id: &str, filenames: Vec<&str>) -> MessageSummary {
        MessageSummary {
            message_id: id.to_string(),
            thread_id: "thread-1".to_string(),
            date: 0,
            from: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            tags: Vec::new(),
            filenames: filenames.into_iter().map(str::to_string).collect(),
        }
    }

    fn test_search_data(query: &str, offset: usize, ids: &[&str], count: u32) -> SearchData {
        SearchData {
            query: query.to_string(),
            threads: ids.iter().map(|id| test_thread(id)).collect(),
            details: ids
                .iter()
                .map(|id| ((*id).to_string(), ThreadUiDetails::default()))
                .collect(),
            count,
            offset,
            limit: 2,
            tags: vec!["inbox".to_string(), "unread".to_string()],
            database_path: "/mail".to_string(),
            revision: test_revision("database", 7),
            cached: false,
        }
    }

    #[test]
    fn search_service_coalesces_stale_work_and_never_runs_pages_concurrently() {
        let started_slow = Arc::new(AtomicBool::new(false));
        let observed_started = started_slow.clone();
        let executed = Arc::new(Mutex::new(Vec::new()));
        let observed_executed = executed.clone();
        let service = SearchWorkerService::new(Arc::new(move |request, _, cancelled| {
            observed_executed
                .lock()
                .expect("executed mutex")
                .push(request.query.clone());
            if request.query == "slow" {
                observed_started.store(true, Ordering::Release);
                while !cancelled.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(2));
                }
                anyhow::bail!("cancelled slow search");
            }
            Ok(test_search_data(
                &request.query,
                request.offset,
                &["latest"],
                1,
            ))
        }));
        let request = |generation, query: &str| SearchPageRequest {
            query: query.to_string(),
            generation,
            offset: 0,
            select_first: true,
            delay: Duration::ZERO,
        };
        let runtime = SearchRuntimeSnapshot {
            page_size: 100,
            excluded_tags: Vec::new(),
        };
        let slow = service.submit(request(1, "slow"), runtime.clone());
        let wait_started = Instant::now();
        while !started_slow.load(Ordering::Acquire) {
            assert!(wait_started.elapsed() < Duration::from_secs(1));
            thread::sleep(Duration::from_millis(1));
        }
        let middle = service.submit(request(2, "middle"), runtime.clone());
        let latest = service.submit(request(3, "latest"), runtime);

        let latest = latest
            .recv_timeout(Duration::from_secs(1))
            .expect("latest response");
        assert_eq!(latest.generation, 3);
        assert_eq!(latest.result.expect("latest result").query, "latest");
        assert!(slow.recv_timeout(Duration::from_secs(1)).is_ok());
        assert!(middle.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(
            *executed.lock().expect("executed mutex"),
            vec!["slow".to_string(), "latest".to_string()]
        );
        let snapshot = service.snapshot();
        assert_eq!(snapshot.active_preparations, 0);
        assert_eq!(snapshot.peak_active_preparations, 1);
        assert!(snapshot.cancelled >= 2);
        assert!(snapshot.coalesced >= 1);
    }

    #[test]
    fn search_delay_is_interruptible_without_waiting_for_the_full_fixture_delay() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_from_thread = cancelled.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            cancel_from_thread.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let error = sleep_search_delay(Duration::from_secs(2), &cancelled)
            .expect_err("cancelled fixture delay should stop early");
        canceller.join().expect("canceller");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(error.to_string().contains("cancelled"));
    }

    #[test]
    fn search_page_size_is_rejected_before_database_work_when_above_the_hard_limit() {
        let error = execute_search_page(
            &OpenConfig::default(),
            "*",
            0,
            MAX_SEARCH_PAGE_SIZE + 1,
            Vec::new(),
        )
        .expect_err("oversized page must fail before opening a database");
        assert!(error.to_string().contains("between 1 and 1000"));
    }

    #[test]
    fn cancellation_is_checked_between_thread_detail_file_reads() {
        let cancelled = AtomicBool::new(false);
        let reads = Cell::new(0_usize);
        let error = compute_thread_detail_with_reader(
            &[test_message("message-1", vec!["one", "two"])],
            &cancelled,
            |_| {
                reads.set(reads.get().saturating_add(1));
                cancelled.store(true, Ordering::Release);
                Ok(b"From: sender@example.test\n\nbody".to_vec())
            },
        )
        .expect_err("cancellation must stop before the second detail file");
        assert_eq!(reads.get(), 1);
        assert!(error.to_string().contains("cancelled"));
    }

    #[test]
    fn load_more_planning_distinguishes_busy_exhausted_and_ready() {
        let mut snapshot = ThreadPagingSnapshot {
            search_loading: true,
            current_query: "tag:inbox".to_string(),
            window_offset: 10,
            loaded_count: 25,
            can_load_more: true,
        };
        assert_eq!(plan_load_more(&snapshot), LoadMoreDecision::Busy);
        snapshot.search_loading = false;
        snapshot.can_load_more = false;
        assert_eq!(plan_load_more(&snapshot), LoadMoreDecision::Exhausted);
        snapshot.can_load_more = true;
        assert_eq!(
            plan_load_more(&snapshot),
            LoadMoreDecision::Ready {
                query: "tag:inbox".to_string(),
                offset: 35,
            }
        );
    }

    #[test]
    fn locate_page_plan_preserves_target_anchor_and_page_boundaries() {
        let plan = LocatePagePlan::new("tag:inbox", 74, 25, Some(12));
        assert_eq!(plan.query, "tag:inbox");
        assert_eq!(plan.target_index, 74);
        assert_eq!(plan.offset, 50);
        assert_eq!(plan.page_size, 25);
        assert_eq!(plan.visual_anchor_index, Some(12));
        assert_eq!(plan.loading_status(), "Loading message 75 (page 51-75)…");
    }

    #[test]
    fn replace_reducer_builds_complete_state_and_exact_cache_status() {
        let mut data = test_search_data("tag:inbox", 25, &["a", "b"], 40);
        data.cached = true;
        let outcome = reduce_replace_search(data);
        assert!(outcome.cached);
        assert_eq!(outcome.update.window_offset, 25);
        assert_eq!(outcome.update.loaded_count, 2);
        assert_eq!(outcome.update.page_size, 2);
        assert!(outcome.update.can_load_more);
        assert_eq!(outcome.update.visible_tags, ["inbox", "unread"]);
        assert_eq!(
            outcome.update.operation,
            "search `tag:inbox` loaded 2 of 40 thread(s) from offset 25 from cache"
        );
    }

    #[test]
    fn append_reducer_appends_expected_page_and_can_select_its_last_row() {
        let snapshot = ThreadSearchStateSnapshot {
            window_offset: 0,
            threads: vec![test_thread("a"), test_thread("b")],
            details: BTreeMap::new(),
            selected_thread_id: Some("b".to_string()),
            selected_index: Some(1),
        };
        let outcome =
            reduce_append_search(snapshot, test_search_data("tag:inbox", 2, &["c"], 4), true);
        assert_eq!(
            outcome.model_update,
            ThreadModelUpdate::Append { start: 2, count: 1 }
        );
        assert_eq!(outcome.selected_index, Some(2));
        assert_eq!(
            outcome
                .update
                .threads
                .iter()
                .map(|thread| thread.thread_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert!(outcome.update.can_load_more);
    }

    #[test]
    fn append_reducer_resets_unexpected_page_and_restores_selection_by_id() {
        let snapshot = ThreadSearchStateSnapshot {
            window_offset: 0,
            threads: vec![test_thread("a"), test_thread("b")],
            details: BTreeMap::new(),
            selected_thread_id: Some("b".to_string()),
            selected_index: Some(0),
        };
        let outcome = reduce_append_search(
            snapshot,
            test_search_data("tag:inbox", 10, &["x", "b"], 12),
            false,
        );
        assert_eq!(outcome.model_update, ThreadModelUpdate::Replace);
        assert_eq!(outcome.update.window_offset, 10);
        assert_eq!(outcome.selected_index, Some(1));
        assert_eq!(
            outcome
                .update
                .threads
                .iter()
                .map(|thread| thread.thread_id.as_str())
                .collect::<Vec<_>>(),
            ["x", "b"]
        );
    }

    #[test]
    fn error_reducer_only_clears_counts_for_an_empty_result_window() {
        let empty = reduce_search_error(anyhow::anyhow!("broken"), false);
        assert_eq!(empty.error, "broken");
        assert_eq!(empty.message, "Search failed: broken");
        assert!(empty.clear_empty_counts);

        let retained = reduce_search_error(anyhow::anyhow!("broken"), true);
        assert!(!retained.clear_empty_counts);
    }

    #[test]
    fn cached_body_preview_is_bounded_but_not_capped_at_two_source_lines() {
        let preview = body_preview("first line\nsecond line\nthird line\n> ignored quote");
        assert_eq!(preview, "first line second line third line");
        let long = body_preview(&"x".repeat(THREAD_PREVIEW_CACHE_MAX_CHARS + 50));
        assert_eq!(long.chars().count(), THREAD_PREVIEW_CACHE_MAX_CHARS);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn oversized_thread_detail_failure_is_visible_and_explicitly_non_partial() {
        let error = notm_notmuch::Error::ThreadMessageLimitExceeded {
            thread_id: "large-thread".to_string(),
            total: MAX_THREAD_DETAIL_MESSAGES + 1,
            limit: MAX_THREAD_DETAIL_MESSAGES,
        };
        let detail = unavailable_thread_detail(&error);
        let warning = detail.load_warning.as_deref().expect("visible warning");

        assert!(warning.contains(&(MAX_THREAD_DETAIL_MESSAGES + 1).to_string()));
        assert!(warning.contains(&format!("safety limit of {MAX_THREAD_DETAIL_MESSAGES}")));
        assert!(warning.contains("no partial thread was loaded"));
        assert!(thread_title_text(&test_thread("large-thread"), &detail).contains("⚠ "));
    }

    #[test]
    fn absolute_thread_targets_floor_to_their_page_boundary() {
        assert_eq!(page_offset_for_index(0, 25), 0);
        assert_eq!(page_offset_for_index(24, 25), 0);
        assert_eq!(page_offset_for_index(25, 25), 25);
        assert_eq!(page_offset_for_index(74, 25), 50);
        assert_eq!(page_offset_for_index(7, 0), 7);
    }

    #[test]
    fn search_cache_key_preserves_every_dimension_and_tag_boundaries() {
        let base_revision = test_revision("database-a", 7);
        let base = search_cache_key(
            "tag:inbox",
            "/mail/a",
            &base_revision,
            25,
            25,
            vec!["a,b".to_string(), "c".to_string()],
            0,
        );
        let variants = [
            (
                "cache epoch",
                search_cache_key(
                    "tag:inbox",
                    "/mail/a",
                    &base_revision,
                    25,
                    25,
                    vec!["a,b".to_string(), "c".to_string()],
                    1,
                ),
            ),
            (
                "path",
                search_cache_key(
                    "tag:inbox",
                    "/mail/b",
                    &base_revision,
                    25,
                    25,
                    vec!["a,b".to_string(), "c".to_string()],
                    0,
                ),
            ),
            (
                "UUID",
                search_cache_key(
                    "tag:inbox",
                    "/mail/a",
                    &test_revision("database-b", 7),
                    25,
                    25,
                    vec!["a,b".to_string(), "c".to_string()],
                    0,
                ),
            ),
            (
                "revision",
                search_cache_key(
                    "tag:inbox",
                    "/mail/a",
                    &test_revision("database-a", 8),
                    25,
                    25,
                    vec!["a,b".to_string(), "c".to_string()],
                    0,
                ),
            ),
            (
                "query",
                search_cache_key(
                    "tag:sent",
                    "/mail/a",
                    &base_revision,
                    25,
                    25,
                    vec!["a,b".to_string(), "c".to_string()],
                    0,
                ),
            ),
            (
                "offset",
                search_cache_key(
                    "tag:inbox",
                    "/mail/a",
                    &base_revision,
                    50,
                    25,
                    vec!["a,b".to_string(), "c".to_string()],
                    0,
                ),
            ),
            (
                "limit",
                search_cache_key(
                    "tag:inbox",
                    "/mail/a",
                    &base_revision,
                    25,
                    50,
                    vec!["a,b".to_string(), "c".to_string()],
                    0,
                ),
            ),
            (
                "excluded tag boundaries",
                search_cache_key(
                    "tag:inbox",
                    "/mail/a",
                    &base_revision,
                    25,
                    25,
                    vec!["a".to_string(), "b,c".to_string()],
                    0,
                ),
            ),
            (
                "excluded tag order",
                search_cache_key(
                    "tag:inbox",
                    "/mail/a",
                    &base_revision,
                    25,
                    25,
                    vec!["c".to_string(), "a,b".to_string()],
                    0,
                ),
            ),
        ];
        let mut cache = BoundedLruCache::new(variants.len() + 1);
        cache.insert(base.clone(), "base");
        for (dimension, key) in &variants {
            assert_ne!(&base, key, "{dimension} did not distinguish the key");
            cache.insert(key.clone(), *dimension);
        }

        assert_eq!(cache.len(), variants.len() + 1);
        assert_eq!(cache.get(&base), Some(&"base"));
        for (dimension, key) in &variants {
            assert_eq!(cache.get(key), Some(dimension));
        }
    }

    #[test]
    fn thread_detail_cache_key_isolates_path_uuid_revision_and_thread() {
        let base_revision = test_revision("database-a", 7);
        let base = thread_detail_cache_key("/mail/a", &base_revision, "thread-a", 0);
        let variants = [
            (
                "cache epoch",
                thread_detail_cache_key("/mail/a", &base_revision, "thread-a", 1),
            ),
            (
                "path",
                thread_detail_cache_key("/mail/b", &base_revision, "thread-a", 0),
            ),
            (
                "UUID",
                thread_detail_cache_key("/mail/a", &test_revision("database-b", 7), "thread-a", 0),
            ),
            (
                "revision",
                thread_detail_cache_key("/mail/a", &test_revision("database-a", 8), "thread-a", 0),
            ),
            (
                "thread ID",
                thread_detail_cache_key("/mail/a", &base_revision, "thread-b", 0),
            ),
        ];
        let mut cache = BoundedLruCache::new(variants.len() + 1);
        cache.insert(base.clone(), "base");
        for (dimension, key) in &variants {
            assert_ne!(&base, key, "{dimension} did not distinguish the key");
            cache.insert(key.clone(), *dimension);
        }

        assert_eq!(cache.len(), variants.len() + 1);
        assert_eq!(cache.get(&base), Some(&"base"));
        for (dimension, key) in &variants {
            assert_eq!(cache.get(key), Some(dimension));
        }
    }

    #[test]
    fn newer_database_generations_evict_stale_search_entries_by_lru() {
        let make_key = |revision| {
            search_cache_key(
                "tag:inbox",
                "/mail/a",
                &test_revision("database-a", revision),
                0,
                25,
                vec!["deleted".to_string()],
                0,
            )
        };
        let old = make_key(1);
        let current = make_key(2);
        let newest = make_key(3);
        let mut cache = BoundedLruCache::new(2);
        cache.insert(old.clone(), "old");
        cache.insert(current.clone(), "current");
        assert_eq!(cache.get(&current), Some(&"current"));

        cache.insert(newest.clone(), "newest");

        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(&old), None);
        assert_eq!(cache.get(&current), Some(&"current"));
        assert_eq!(cache.get(&newest), Some(&"newest"));
    }

    #[test]
    fn invalidated_search_does_not_return_a_removed_newly_indexed_message() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("mail");
        let maildir = root.join("Drafts/cur");
        fs::create_dir_all(&maildir)?;
        let config_path = temp.path().join("notmuch-config");
        fs::write(
            &config_path,
            format!(
                "[database]\npath={}\n\n[user]\nname=Fixture User\nprimary_email=fixture@example.test\n",
                root.display()
            ),
        )?;
        let open_config = OpenConfig {
            database_path: Some(root),
            config_path: Some(config_path),
            profile: None,
        };
        let db = Database::create(&open_config)?;
        let message_id = format!("{}@fixture.test", uuid::Uuid::new_v4());
        let path = maildir.join("newly-indexed:2,D");
        fs::write(
            &path,
            format!(
                "From: Fixture User <fixture@example.test>\nTo: recipient@example.test\nSubject: Cached removal regression\nMessage-ID: <{message_id}>\nDate: Tue, 19 Aug 2026 12:00:00 -0600\n\nDraft body.\n"
            ),
        )?;
        db.index_file_with_tags(&path, &["draft"])?;
        let retained_path = maildir.join("retained:2,S");
        fs::write(
            &retained_path,
            format!(
                "From: Fixture User <fixture@example.test>\nTo: recipient@example.test\nSubject: Retained message\nMessage-ID: <{}@fixture.test>\nDate: Tue, 19 Aug 2026 12:01:00 -0600\n\nRetained body.\n",
                uuid::Uuid::new_v4()
            ),
        )?;
        db.index_file_with_tags(&retained_path, &["inbox"])?;
        drop(db);

        let first = execute_search_page(&open_config, "tag:draft", 0, 100, Vec::new())?;
        assert_eq!(first.threads.len(), 1);
        assert!(!first.cached);

        let db = Database::open(&open_config, DatabaseMode::ReadWrite)?;
        db.remove_message_file(&path)?;
        drop(db);
        fs::remove_file(&path)?;
        invalidate_search_caches();

        let second = execute_search_page(&open_config, "tag:draft", 0, 100, Vec::new())?;
        assert!(
            second.threads.is_empty(),
            "removed draft was returned after cache invalidation; result revision={:?}, cached={}",
            second.revision,
            second.cached
        );
        Ok(())
    }

    #[test]
    fn typed_search_cache_enforces_named_capacity_and_lru() {
        let revision = test_revision("database-a", 7);
        let keys = (0..SEARCH_PAGE_CACHE_CAPACITY)
            .map(|offset| {
                search_cache_key(
                    "tag:inbox",
                    "/mail/a",
                    &revision,
                    offset,
                    1,
                    vec!["deleted".to_string()],
                    0,
                )
            })
            .collect::<Vec<_>>();
        let mut cache = BoundedLruCache::new(SEARCH_PAGE_CACHE_CAPACITY);
        for (value, key) in keys.iter().cloned().enumerate() {
            cache.insert(key, value);
            assert!(cache.len() <= SEARCH_PAGE_CACHE_CAPACITY);
        }
        assert_eq!(cache.len(), SEARCH_PAGE_CACHE_CAPACITY);

        assert_eq!(cache.get(&keys[0]), Some(&0));
        assert_eq!(cache.insert(keys[1].clone(), 101), Some(1));
        assert_eq!(cache.len(), SEARCH_PAGE_CACHE_CAPACITY);
        let new_generation = search_cache_key(
            "tag:inbox",
            "/mail/a",
            &test_revision("database-a", 8),
            0,
            1,
            vec!["deleted".to_string()],
            0,
        );
        cache.insert(new_generation.clone(), 1_000);

        assert_eq!(cache.len(), SEARCH_PAGE_CACHE_CAPACITY);
        assert_eq!(cache.get(&keys[2]), None);
        assert_eq!(cache.get(&keys[0]), Some(&0));
        assert_eq!(cache.get(&keys[1]), Some(&101));
        assert_eq!(cache.get(&new_generation), Some(&1_000));
    }

    #[test]
    fn typed_thread_detail_cache_enforces_named_capacity_and_lru() {
        let revision = test_revision("database-a", 7);
        let keys = (0..THREAD_DETAIL_CACHE_CAPACITY)
            .map(|index| {
                thread_detail_cache_key("/mail/a", &revision, &format!("thread-{index}"), 0)
            })
            .collect::<Vec<_>>();
        let mut cache = BoundedLruCache::new(THREAD_DETAIL_CACHE_CAPACITY);
        for (value, key) in keys.iter().cloned().enumerate() {
            cache.insert(key, value);
            assert!(cache.len() <= THREAD_DETAIL_CACHE_CAPACITY);
        }
        assert_eq!(cache.len(), THREAD_DETAIL_CACHE_CAPACITY);

        assert_eq!(cache.get(&keys[0]), Some(&0));
        let new_generation =
            thread_detail_cache_key("/mail/a", &test_revision("database-b", 8), "thread-0", 0);
        cache.insert(new_generation.clone(), 10_000);

        assert_eq!(cache.len(), THREAD_DETAIL_CACHE_CAPACITY);
        assert_eq!(cache.get(&keys[1]), None);
        assert_eq!(cache.get(&keys[0]), Some(&0));
        assert_eq!(cache.get(&new_generation), Some(&10_000));
    }
}
