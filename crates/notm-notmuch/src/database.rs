use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, CString},
    fs::{self, File},
    io::{Read, Seek},
    os::raw::c_char,
    path::{Path, PathBuf},
    ptr::NonNull,
};

#[cfg(unix)]
use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
};

use serde::{Deserialize, Serialize};

use crate::{
    Error, Result, ThreadSummary,
    error::{check, check_index},
    ffi,
    message::{
        AppliedTagChange, MaildirFilenameChange, MaildirFlagSyncFailure, MaildirPathChange,
        MessagePathState, MessageSummary, MessageTagFailure, MessageTagMutation, TagBatchReport,
        TagFailureStage, TagMutation, TagOperationReport, ThreadResolutionFailure, ThreadTagReport,
    },
    query::{QueryOptions, SortOrder},
    safe::{cstr_to_string, path_to_cstring, take_malloc_string},
    tags::validate_tag,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DatabaseMode {
    ReadOnly,
    ReadWrite,
}

impl DatabaseMode {
    fn to_ffi(self) -> ffi::notmuch_database_mode_t {
        match self {
            DatabaseMode::ReadOnly => ffi::notmuch_database_mode_t::NOTMUCH_DATABASE_MODE_READ_ONLY,
            DatabaseMode::ReadWrite => {
                ffi::notmuch_database_mode_t::NOTMUCH_DATABASE_MODE_READ_WRITE
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenConfig {
    pub database_path: Option<std::path::PathBuf>,
    pub config_path: Option<std::path::PathBuf>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Revision {
    pub revision: u64,
    pub uuid: String,
}

/// A currently indexed message file opened for reading.
///
/// Keeping the handle and its path together lets callers parse the already-open
/// file without checking a path and reopening it after a Maildir move.
#[derive(Debug)]
pub struct ResolvedMessageFile {
    path: PathBuf,
    file: File,
}

impl ResolvedMessageFile {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    pub fn into_parts(self) -> (PathBuf, File) {
        (self.path, self.file)
    }
}

impl Read for ResolvedMessageFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buffer)
    }
}

impl Seek for ResolvedMessageFile {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.file.seek(position)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadMessagePage {
    pub thread_id: String,
    pub messages: Vec<MessageSummary>,
    pub total: u32,
    pub offset: usize,
    pub limit: usize,
    pub revision: Revision,
}

impl ThreadMessagePage {
    pub fn has_more(&self) -> bool {
        self.offset.saturating_add(self.messages.len()) < self.total as usize
    }
}

/// Per-thread outcome from a bounded batched message lookup.
///
/// Limit outcomes never contain a partial message prefix. Callers can still
/// use successfully loaded sibling threads from the same batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundedThreadMessages {
    Loaded(Vec<MessageSummary>),
    ThreadLimitExceeded {
        thread_id: String,
        total: usize,
        limit: usize,
    },
    BatchLimitExceeded {
        total: usize,
        limit: usize,
    },
}

impl std::fmt::Display for BoundedThreadMessages {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Loaded(messages) => {
                write!(formatter, "loaded {} message(s)", messages.len())
            }
            Self::ThreadLimitExceeded {
                thread_id,
                total,
                limit,
            } => write!(
                formatter,
                "thread `{thread_id}` contains {total} message(s), exceeding the requested safety limit of {limit}; no partial thread was loaded"
            ),
            Self::BatchLimitExceeded { total, limit } => write!(
                formatter,
                "the requested thread-detail batch contains {total} loadable message(s), exceeding the aggregate safety limit of {limit}; no partial batch was loaded"
            ),
        }
    }
}

const COMPLETE_THREAD_PAGE_SIZE: usize = 256;
const THREAD_MESSAGE_ID_PAGE_SIZE: usize = 256;

/// Maximum number of message IDs captured from any one exact thread before a
/// tag mutation. This mirrors the interactive thread safety ceiling without
/// making the Notmuch layer depend on the UI crate.
pub const MAX_THREAD_TAG_MESSAGES: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ThreadMessageIdPage {
    thread_id: String,
    message_ids: Vec<String>,
    total: u32,
    offset: usize,
    limit: usize,
    revision: Revision,
}

pub struct Database {
    ptr: NonNull<ffi::notmuch_database_t>,
    mode: DatabaseMode,
    closed: bool,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.path())
            .field("mode", &self.mode)
            .finish()
    }
}

impl Database {
    pub fn open(config: &OpenConfig, mode: DatabaseMode) -> Result<Self> {
        let database_path = optional_path_cstring(config.database_path.as_deref())?;
        let config_path = optional_path_cstring(config.config_path.as_deref())?;
        let profile = optional_string_cstring(config.profile.as_deref())?;
        let mut db = std::ptr::null_mut();
        let mut error_message: *mut c_char = std::ptr::null_mut();
        let status = unsafe {
            ffi::notmuch_database_open_with_config(
                ptr_or_null(&database_path),
                mode.to_ffi(),
                ptr_or_null(&config_path),
                ptr_or_null(&profile),
                &mut db,
                &mut error_message,
            )
        };
        let detail = unsafe { take_malloc_string(error_message) };
        check(status, detail)?;
        let ptr = NonNull::new(db).ok_or(Error::Null("notmuch_database_open_with_config"))?;
        Ok(Self {
            ptr,
            mode,
            closed: false,
        })
    }

    pub fn create(config: &OpenConfig) -> Result<Self> {
        let database_path = optional_path_cstring(config.database_path.as_deref())?;
        let config_path = optional_path_cstring(config.config_path.as_deref())?;
        let profile = optional_string_cstring(config.profile.as_deref())?;
        let mut db = std::ptr::null_mut();
        let mut error_message: *mut c_char = std::ptr::null_mut();
        let status = unsafe {
            ffi::notmuch_database_create_with_config(
                ptr_or_null(&database_path),
                ptr_or_null(&config_path),
                ptr_or_null(&profile),
                &mut db,
                &mut error_message,
            )
        };
        let detail = unsafe { take_malloc_string(error_message) };
        check(status, detail)?;
        let ptr = NonNull::new(db).ok_or(Error::Null("notmuch_database_create_with_config"))?;
        Ok(Self {
            ptr,
            mode: DatabaseMode::ReadWrite,
            closed: false,
        })
    }

    pub fn load_config(config: &OpenConfig) -> Result<Self> {
        let database_path = optional_path_cstring(config.database_path.as_deref())?;
        let config_path = optional_path_cstring(config.config_path.as_deref())?;
        let profile = optional_string_cstring(config.profile.as_deref())?;
        let mut db = std::ptr::null_mut();
        let mut error_message: *mut c_char = std::ptr::null_mut();
        let status = unsafe {
            ffi::notmuch_database_load_config(
                ptr_or_null(&database_path),
                ptr_or_null(&config_path),
                ptr_or_null(&profile),
                &mut db,
                &mut error_message,
            )
        };
        let detail = unsafe { take_malloc_string(error_message) };
        if status != ffi::notmuch_status_t::NOTMUCH_STATUS_SUCCESS
            && status != ffi::notmuch_status_t::NOTMUCH_STATUS_NO_DATABASE
            && status != ffi::notmuch_status_t::NOTMUCH_STATUS_NO_CONFIG
        {
            check(status, detail)?;
        }
        let ptr = NonNull::new(db).ok_or(Error::Null("notmuch_database_load_config"))?;
        Ok(Self {
            ptr,
            mode: DatabaseMode::ReadOnly,
            closed: false,
        })
    }

    pub fn mode(&self) -> DatabaseMode {
        self.mode
    }

    /// Commit pending writes, close the database, and surface flush failures.
    ///
    /// Dropping a database remains a best-effort fallback, but callers that
    /// mutate data should use this consuming API so a failed durable commit is
    /// not silently discarded.
    pub fn close(mut self) -> Result<()> {
        let detail = self.status_string();
        self.closed = true;
        close_with(detail, || unsafe {
            ffi::notmuch_database_close(self.ptr.as_ptr())
        })
    }

    pub fn path(&self) -> String {
        unsafe { cstr_to_string(ffi::notmuch_database_get_path(self.ptr.as_ptr())) }
    }

    pub fn revision(&self) -> Revision {
        let mut uuid = std::ptr::null();
        let revision = unsafe { ffi::notmuch_database_get_revision(self.ptr.as_ptr(), &mut uuid) };
        Revision {
            revision: revision as u64,
            uuid: unsafe { cstr_to_string(uuid) },
        }
    }

    pub fn status_string(&self) -> String {
        unsafe { cstr_to_string(ffi::notmuch_database_status_string(self.ptr.as_ptr())) }
    }

    pub fn get_config_raw(&self, key: &str) -> Result<String> {
        let key = CString::new(key)?;
        let mut value: *mut c_char = std::ptr::null_mut();
        let status = unsafe {
            ffi::notmuch_database_get_config(self.ptr.as_ptr(), key.as_ptr(), &mut value)
        };
        check(status, self.status_string())?;
        Ok(unsafe { take_malloc_string(value) })
    }

    pub fn all_tags(&self) -> Vec<String> {
        let tags = unsafe { ffi::notmuch_database_get_all_tags(self.ptr.as_ptr()) };
        let out = unsafe { collect_tags(tags) };
        if !tags.is_null() {
            unsafe { ffi::notmuch_tags_destroy(tags) };
        }
        out
    }

    pub fn count_threads(&self, query: &str, options: &QueryOptions) -> Result<u32> {
        let q = self.create_query(query, options)?;
        let mut count = 0;
        let status = unsafe { ffi::notmuch_query_count_threads(q.as_ptr(), &mut count) };
        check(status, self.status_string())?;
        Ok(count)
    }

    pub fn count_messages(&self, query: &str, options: &QueryOptions) -> Result<u32> {
        let q = self.create_query(query, options)?;
        let mut count = 0;
        let status = unsafe { ffi::notmuch_query_count_messages(q.as_ptr(), &mut count) };
        check(status, self.status_string())?;
        Ok(count)
    }

    pub fn search_threads(
        &self,
        query: &str,
        options: &QueryOptions,
    ) -> Result<Vec<ThreadSummary>> {
        let q = self.create_query(query, options)?;
        let mut threads = std::ptr::null_mut();
        let status = unsafe { ffi::notmuch_query_search_threads(q.as_ptr(), &mut threads) };
        check(status, self.status_string())?;
        let mut out = Vec::new();
        let mut skipped = 0usize;
        while unsafe { ffi::notmuch_threads_valid(threads) } != 0 {
            let thread = unsafe { ffi::notmuch_threads_get(threads) };
            if !thread.is_null() {
                if skipped >= options.offset && out.len() < options.limit {
                    out.push(thread_summary(thread));
                }
                skipped += 1;
                unsafe { ffi::notmuch_thread_destroy(thread) };
            }
            if out.len() >= options.limit {
                break;
            }
            unsafe { ffi::notmuch_threads_move_to_next(threads) };
        }
        check_threads_iterator(threads, self.status_string())?;
        Ok(out)
    }

    pub fn search_messages(
        &self,
        query: &str,
        options: &QueryOptions,
    ) -> Result<Vec<MessageSummary>> {
        let q = self.create_query(query, options)?;
        self.search_messages_with_query(q.as_ptr(), options)
    }

    /// Collect a complete thread without ever exceeding the caller's bound.
    ///
    /// A count-only snapshot checks the total before any message summaries are
    /// materialized. Threads above `maximum` return an explicit error rather
    /// than an oldest-only prefix that could be mistaken for the full thread.
    /// Accepted threads are then paged against the same database revision.
    /// Callers that do not need to retain the complete thread should consume
    /// [`Self::thread_messages_page`] directly instead.
    pub fn thread_messages_bounded(
        &self,
        thread_id: &str,
        maximum: usize,
    ) -> Result<Vec<MessageSummary>> {
        let snapshot = self.thread_messages_page(thread_id, 0, 0)?;
        let expected_total = snapshot.total as usize;
        if expected_total > maximum {
            return Err(Error::ThreadMessageLimitExceeded {
                thread_id: thread_id.to_string(),
                total: expected_total,
                limit: maximum,
            });
        }

        let expected_revision = snapshot.revision;
        let mut messages = Vec::with_capacity(expected_total);
        let mut message_ids = BTreeSet::new();
        let mut offset = 0usize;

        while offset < expected_total {
            let page_limit = COMPLETE_THREAD_PAGE_SIZE.min(expected_total - offset);
            let page = self.thread_messages_page(thread_id, offset, page_limit)?;
            check_thread_page_snapshot(
                thread_id,
                &expected_revision,
                expected_total,
                messages.len(),
                &page,
            )?;

            let page_len = page.messages.len();
            for message in page.messages {
                if !message_ids.insert(message.message_id.clone()) {
                    return Err(Error::InconsistentThreadMessages {
                        thread_id: thread_id.to_string(),
                        expected: expected_total,
                        loaded: messages.len(),
                    });
                }
                messages.push(message);
            }
            offset = offset.saturating_add(page_len);

            if page_len == 0 {
                return Err(Error::InconsistentThreadMessages {
                    thread_id: thread_id.to_string(),
                    expected: expected_total,
                    loaded: messages.len(),
                });
            }
        }

        if messages.len() != expected_total {
            return Err(Error::InconsistentThreadMessages {
                thread_id: thread_id.to_string(),
                expected: expected_total,
                loaded: messages.len(),
            });
        }
        Ok(messages)
    }

    /// Return one oldest-first page from a thread, together with its full count.
    ///
    /// Consumers paging over multiple calls must require the returned
    /// [`ThreadMessagePage::revision`] to remain identical. The bounded
    /// collector above performs that validation automatically.
    pub fn thread_messages_page(
        &self,
        thread_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<ThreadMessagePage> {
        let query = exact_term_query("thread", thread_id);
        let options = QueryOptions {
            sort: SortOrder::OldestFirst,
            limit,
            offset,
            excluded_tags: Vec::new(),
        };
        let revision_before = self.revision();
        let total = self.count_messages(&query, &options)?;
        let messages = if limit == 0 || offset >= total as usize {
            Vec::new()
        } else {
            self.search_messages(&query, &options)?
        };
        let revision_after = self.revision();
        let expected_page_len = (total as usize).saturating_sub(offset).min(limit);
        if revision_before != revision_after || messages.len() != expected_page_len {
            return Err(Error::InconsistentThreadMessages {
                thread_id: thread_id.to_string(),
                expected: total as usize,
                loaded: offset.saturating_add(messages.len()),
            });
        }
        Ok(ThreadMessagePage {
            thread_id: thread_id.to_string(),
            messages,
            total,
            offset,
            limit,
            revision: revision_before,
        })
    }

    /// Return one message-ID-only page from an exact thread snapshot.
    ///
    /// Tag target capture deliberately avoids [`MessageSummary`] construction:
    /// it reads only each message's stable ID and validates the database
    /// revision before and after the count/query work. The caller supplies the
    /// revision shared by the complete multi-thread snapshot.
    fn thread_message_ids_page(
        &self,
        thread_id: &str,
        offset: usize,
        limit: usize,
        expected_revision: &Revision,
    ) -> Result<ThreadMessageIdPage> {
        check_thread_tag_snapshot_revision(expected_revision, &self.revision())?;

        let query = exact_term_query("thread", thread_id);
        let options = QueryOptions {
            sort: SortOrder::MessageId,
            limit,
            offset,
            excluded_tags: Vec::new(),
        };
        let q = self.create_query(&query, &options)?;
        let mut total = 0;
        let count_status = unsafe { ffi::notmuch_query_count_messages(q.as_ptr(), &mut total) };
        check(count_status, self.status_string())?;
        let message_ids = if limit == 0 || offset >= total as usize {
            Vec::new()
        } else {
            self.search_message_ids_with_query(q.as_ptr(), &options)?
        };

        let observed_revision = self.revision();
        check_thread_tag_snapshot_revision(expected_revision, &observed_revision)?;
        let expected_page_len = (total as usize).saturating_sub(offset).min(limit);
        if message_ids.len() != expected_page_len {
            return Err(Error::InconsistentThreadMessages {
                thread_id: thread_id.to_string(),
                expected: total as usize,
                loaded: offset.saturating_add(message_ids.len()),
            });
        }

        Ok(ThreadMessageIdPage {
            thread_id: thread_id.to_string(),
            message_ids,
            total,
            offset,
            limit,
            revision: observed_revision,
        })
    }

    /// Capture every exact message ID in a thread without exceeding `maximum`.
    ///
    /// The count-only preflight runs before allocating or iterating message IDs.
    /// Accepted threads are paged in fixed-size ID-only chunks and every page
    /// must retain the multi-thread snapshot revision and exact count.
    fn thread_message_ids_bounded(
        &self,
        thread_id: &str,
        maximum: usize,
        expected_revision: &Revision,
        mut after_page: impl FnMut(usize),
    ) -> Result<Vec<String>> {
        let snapshot = self.thread_message_ids_page(thread_id, 0, 0, expected_revision)?;
        let expected_total = snapshot.total as usize;
        if expected_total > maximum {
            return Err(Error::ThreadMessageLimitExceeded {
                thread_id: thread_id.to_string(),
                total: expected_total,
                limit: maximum,
            });
        }

        let mut message_ids = Vec::with_capacity(expected_total);
        let mut unique_ids = BTreeSet::new();
        let mut offset = 0usize;
        while offset < expected_total {
            let page_limit = THREAD_MESSAGE_ID_PAGE_SIZE.min(expected_total - offset);
            let page =
                self.thread_message_ids_page(thread_id, offset, page_limit, expected_revision)?;
            check_thread_message_id_page_snapshot(
                thread_id,
                expected_revision,
                expected_total,
                message_ids.len(),
                &page,
            )?;

            let page_len = page.message_ids.len();
            for message_id in page.message_ids {
                if !unique_ids.insert(message_id.clone()) {
                    return Err(Error::InconsistentThreadMessages {
                        thread_id: thread_id.to_string(),
                        expected: expected_total,
                        loaded: message_ids.len(),
                    });
                }
                message_ids.push(message_id);
            }
            offset = offset.saturating_add(page_len);
            if page_len == 0 {
                return Err(Error::InconsistentThreadMessages {
                    thread_id: thread_id.to_string(),
                    expected: expected_total,
                    loaded: message_ids.len(),
                });
            }
            after_page(message_ids.len());
        }

        if message_ids.len() != expected_total {
            return Err(Error::InconsistentThreadMessages {
                thread_id: thread_id.to_string(),
                expected: expected_total,
                loaded: message_ids.len(),
            });
        }
        Ok(message_ids)
    }

    /// Resolve and open a message using the database's current filename list.
    ///
    /// Notmuch may associate several paths with one Message-ID, and a summary's
    /// filenames can become stale after Maildir flag synchronization. This looks
    /// the message up again, orders and de-duplicates its current filenames, and
    /// tries every candidate until one can be opened as a regular file.
    pub fn open_message_file(&self, message: &MessageSummary) -> Result<ResolvedMessageFile> {
        self.open_message_id_file(&message.message_id)
    }

    /// Resolve and open a message directly from its Message-ID.
    ///
    /// This performs one current database lookup, so lazy callers do not need
    /// to materialize a [`MessageSummary`] or trust a filename retained before
    /// a Maildir move.
    pub fn open_message_id_file(&self, message_id: &str) -> Result<ResolvedMessageFile> {
        let message_id_cstring = CString::new(message_id)?;
        let mut current = std::ptr::null_mut();
        let status = unsafe {
            ffi::notmuch_database_find_message(
                self.ptr.as_ptr(),
                message_id_cstring.as_ptr(),
                &mut current,
            )
        };
        check(status, self.status_string())?;
        if current.is_null() {
            return Err(Error::MessageNotFound(message_id.to_string()));
        }
        let filenames =
            unsafe { collect_filename_paths(ffi::notmuch_message_get_filenames(current)) };
        unsafe { ffi::notmuch_message_destroy(current) };
        open_message_candidates(message_id, filenames)
    }

    /// Looks up one message directly without materializing the messages from
    /// every thread that may have appeared in a surrounding search result.
    pub fn find_message(&self, message_id: &str) -> Result<Option<MessageSummary>> {
        let message_id = CString::new(message_id)?;
        let mut message = std::ptr::null_mut();
        let status = unsafe {
            ffi::notmuch_database_find_message(self.ptr.as_ptr(), message_id.as_ptr(), &mut message)
        };
        check(status, self.status_string())?;
        if message.is_null() {
            return Ok(None);
        }
        let summary = message_summary(message);
        unsafe { ffi::notmuch_message_destroy(message) };
        Ok(Some(summary))
    }

    /// Fetch complete message sets for several threads with one combined query.
    ///
    /// A lightweight thread iterator checks every per-thread count before any
    /// message summaries are materialized. If the sum of otherwise loadable
    /// threads exceeds `batch_maximum`, no message summaries are materialized
    /// for that batch. Otherwise a second iterator over the same query loads
    /// only threads within `thread_maximum`. The result contains an entry for
    /// every distinct requested ID, including IDs that no longer exist, and
    /// every loaded message list is ordered oldest first.
    pub fn thread_messages_for_threads_bounded(
        &self,
        thread_ids: &[String],
        thread_maximum: usize,
        batch_maximum: usize,
    ) -> Result<BTreeMap<String, BoundedThreadMessages>> {
        let requested = thread_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if requested.is_empty() {
            return Ok(BTreeMap::new());
        }

        let query = thread_query(&requested);
        let options = QueryOptions {
            sort: SortOrder::OldestFirst,
            limit: usize::MAX,
            offset: 0,
            excluded_tags: Vec::new(),
        };
        let query = self.create_query(&query, &options)?;
        let mut outcomes = requested
            .iter()
            .map(|thread_id| {
                (
                    (*thread_id).to_string(),
                    BoundedThreadMessages::Loaded(Vec::new()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut expected_counts = BTreeMap::new();
        let mut loadable_total = 0usize;

        let mut threads = std::ptr::null_mut();
        let status = unsafe { ffi::notmuch_query_search_threads(query.as_ptr(), &mut threads) };
        check(status, self.status_string())?;
        while unsafe { ffi::notmuch_threads_valid(threads) } != 0 {
            let thread = unsafe { ffi::notmuch_threads_get(threads) };
            if !thread.is_null() {
                let thread_id =
                    unsafe { cstr_to_string(ffi::notmuch_thread_get_thread_id(thread)) };
                let total =
                    usize::try_from(unsafe { ffi::notmuch_thread_get_total_messages(thread) })
                        .unwrap_or_default();
                if requested.contains(thread_id.as_str()) {
                    expected_counts.insert(thread_id.clone(), total);
                    if total > thread_maximum {
                        outcomes.insert(
                            thread_id.clone(),
                            BoundedThreadMessages::ThreadLimitExceeded {
                                thread_id,
                                total,
                                limit: thread_maximum,
                            },
                        );
                    } else {
                        loadable_total = loadable_total.saturating_add(total);
                    }
                }
                unsafe { ffi::notmuch_thread_destroy(thread) };
            }
            unsafe { ffi::notmuch_threads_move_to_next(threads) };
        }
        check_threads_iterator(threads, self.status_string())?;

        if loadable_total > batch_maximum {
            for (thread_id, outcome) in &mut outcomes {
                if expected_counts.get(thread_id).copied().unwrap_or_default() > 0
                    && matches!(outcome, BoundedThreadMessages::Loaded(_))
                {
                    *outcome = BoundedThreadMessages::BatchLimitExceeded {
                        total: loadable_total,
                        limit: batch_maximum,
                    };
                }
            }
            return Ok(outcomes);
        }
        if loadable_total == 0 {
            return Ok(outcomes);
        }

        for (thread_id, outcome) in &mut outcomes {
            if let BoundedThreadMessages::Loaded(messages) = outcome {
                messages.reserve(expected_counts.get(thread_id).copied().unwrap_or_default());
            }
        }

        let mut messages = std::ptr::null_mut();
        let status = unsafe { ffi::notmuch_query_search_messages(query.as_ptr(), &mut messages) };
        check(status, self.status_string())?;
        while unsafe { ffi::notmuch_messages_valid(messages) } != 0 {
            let message = unsafe { ffi::notmuch_messages_get(messages) };
            if !message.is_null() {
                let thread_id =
                    unsafe { cstr_to_string(ffi::notmuch_message_get_thread_id(message)) };
                if let Some(BoundedThreadMessages::Loaded(thread_messages)) =
                    outcomes.get_mut(&thread_id)
                {
                    thread_messages.push(message_summary(message));
                }
                unsafe { ffi::notmuch_message_destroy(message) };
            }
            unsafe { ffi::notmuch_messages_move_to_next(messages) };
        }
        check_messages_iterator(messages, self.status_string())?;

        for (thread_id, outcome) in &outcomes {
            let BoundedThreadMessages::Loaded(messages) = outcome else {
                continue;
            };
            let expected = expected_counts.get(thread_id).copied().unwrap_or_default();
            if messages.len() != expected {
                return Err(Error::InconsistentThreadMessages {
                    thread_id: thread_id.clone(),
                    expected,
                    loaded: messages.len(),
                });
            }
        }
        Ok(outcomes)
    }

    pub fn index_file_with_tags(&self, path: &Path, tags: &[&str]) -> Result<String> {
        for tag in tags {
            validate_tag(tag)?;
        }
        let path = path_to_cstring(path)?;
        let mut message = std::ptr::null_mut();
        let status = unsafe {
            ffi::notmuch_database_index_file(
                self.ptr.as_ptr(),
                path.as_ptr(),
                std::ptr::null_mut(),
                &mut message,
            )
        };
        check_index(status, self.status_string())?;
        if message.is_null() {
            return Err(Error::Null("notmuch_database_index_file message"));
        }
        let id = unsafe { cstr_to_string(ffi::notmuch_message_get_message_id(message)) };
        let freeze_status = unsafe { ffi::notmuch_message_freeze(message) };
        check(freeze_status, self.status_string())?;
        for tag in tags {
            let tag = CString::new(*tag)?;
            let status = unsafe { ffi::notmuch_message_add_tag(message, tag.as_ptr()) };
            if let Err(err) = check(status, self.status_string()) {
                let _ = unsafe { ffi::notmuch_message_thaw(message) };
                unsafe { ffi::notmuch_message_destroy(message) };
                return Err(err);
            }
        }
        let thaw_status = unsafe { ffi::notmuch_message_thaw(message) };
        check(thaw_status, self.status_string())?;
        unsafe { ffi::notmuch_message_destroy(message) };
        Ok(id)
    }

    pub fn remove_message_file(&self, path: &Path) -> Result<()> {
        let path = path_to_cstring(path)?;
        let status =
            unsafe { ffi::notmuch_database_remove_message(self.ptr.as_ptr(), path.as_ptr()) };
        check_index(status, self.status_string())
    }

    pub fn apply_tags_to_query(
        &self,
        query: &str,
        mutation: &TagMutation,
    ) -> Result<TagOperationReport> {
        validate_mutation(mutation)?;
        let options = QueryOptions {
            sort: SortOrder::Unsorted,
            limit: usize::MAX,
            offset: 0,
            excluded_tags: Vec::new(),
        };
        // Expand the query once before mutation. This preserves one immutable
        // target snapshot even if committing the batch changes query order or
        // membership.
        let messages = self.search_messages(query, &options)?;
        let prepared = prepare_uniform_mutations(
            messages.into_iter().map(|message| message.message_id),
            mutation,
        )?;
        let batch = self.apply_prepared_tag_mutations(&prepared)?;
        Ok(TagOperationReport {
            query: query.to_string(),
            added: mutation.add.clone(),
            removed: mutation.remove.clone(),
            batch,
        })
    }

    pub fn apply_tags_to_messages(
        &self,
        mutations: &[MessageTagMutation],
        sync_maildir_flags: bool,
    ) -> Result<TagBatchReport> {
        let mut prepared = Vec::with_capacity(mutations.len());
        for mutation in mutations {
            for tag in mutation.add.iter().chain(mutation.remove.iter()) {
                validate_tag(tag)?;
            }
            prepared.push(PreparedMessageMutation {
                message_id: mutation.message_id.clone(),
                message_id_c: CString::new(mutation.message_id.as_str())?,
                mutation: TagMutation {
                    add: mutation.add.clone(),
                    remove: mutation.remove.clone(),
                    sync_maildir_flags,
                },
            });
        }
        self.apply_prepared_tag_mutations(&prepared)
    }

    /// Apply a tag mutation to exactly the supplied thread IDs.
    ///
    /// The IDs should come from the UI's immutable result snapshot. This API
    /// deliberately has no positional range or search options, so a refresh or
    /// reorder cannot silently retarget the operation.
    pub fn apply_tags_to_threads(
        &self,
        thread_ids: &[String],
        mutation: &TagMutation,
    ) -> Result<ThreadTagReport> {
        self.apply_tags_to_threads_with_hooks(thread_ids, mutation, |_, _| {}, || {})
    }

    fn apply_tags_to_threads_with_hooks(
        &self,
        thread_ids: &[String],
        mutation: &TagMutation,
        mut after_snapshot_page: impl FnMut(&str, usize),
        before_mutation: impl FnOnce(),
    ) -> Result<ThreadTagReport> {
        validate_mutation(mutation)?;
        let mut seen_threads = BTreeSet::new();
        let unique_thread_ids = thread_ids
            .iter()
            .filter(|thread_id| seen_threads.insert((*thread_id).clone()))
            .cloned()
            .collect::<Vec<_>>();
        let snapshot_revision = self.revision();
        let mut missing_thread_ids = Vec::new();
        let mut thread_resolution_failures = Vec::new();
        let mut message_threads = BTreeMap::new();
        let mut matched_threads = 0usize;
        for thread_id in &unique_thread_ids {
            let resolved = self.thread_message_ids_bounded(
                thread_id,
                MAX_THREAD_TAG_MESSAGES,
                &snapshot_revision,
                |loaded| after_snapshot_page(thread_id, loaded),
            );
            match resolved {
                Ok(message_ids) if message_ids.is_empty() => {
                    missing_thread_ids.push(thread_id.clone());
                }
                Ok(message_ids) => {
                    matched_threads = matched_threads.saturating_add(1);
                    for message_id in message_ids {
                        message_threads
                            .entry(message_id)
                            .or_insert_with(|| thread_id.clone());
                    }
                }
                Err(error @ Error::ThreadTagSnapshotChanged { .. }) => return Err(error),
                Err(error) if isolated_thread_resolution_failure(&error) => {
                    thread_resolution_failures.push(ThreadResolutionFailure {
                        thread_id: thread_id.clone(),
                        detail: error.to_string(),
                    });
                }
                Err(error) => {
                    return Err(Error::ThreadTagSnapshotFailed {
                        detail: error.to_string(),
                    });
                }
            }
        }
        check_thread_tag_snapshot_revision(&snapshot_revision, &self.revision())?;
        let prepared = prepare_uniform_mutations(message_threads.keys().cloned(), mutation)
            .map_err(|error| Error::ThreadTagSnapshotFailed {
                detail: error.to_string(),
            })?;
        check_thread_tag_snapshot_revision(&snapshot_revision, &self.revision())?;
        before_mutation();
        let batch = self.apply_prepared_tag_mutations(&prepared)?;
        let changed_threads = batch
            .changes
            .iter()
            .filter_map(|change| message_threads.get(&change.message_id))
            .collect::<BTreeSet<_>>()
            .len();
        Ok(ThreadTagReport {
            thread_ids: unique_thread_ids,
            missing_thread_ids,
            thread_resolution_failures,
            matched_threads,
            changed_threads,
            added: mutation.add.clone(),
            removed: mutation.remove.clone(),
            batch,
        })
    }

    fn apply_prepared_tag_mutations(
        &self,
        prepared: &[PreparedMessageMutation],
    ) -> Result<TagBatchReport> {
        if prepared.is_empty() {
            return Ok(TagBatchReport::default());
        }
        let begin = unsafe { ffi::notmuch_database_begin_atomic(self.ptr.as_ptr()) };
        check(begin, self.status_string())?;
        let mut report = TagBatchReport {
            requested_messages: prepared.len(),
            ..TagBatchReport::default()
        };
        for prepared in prepared {
            let mut message = std::ptr::null_mut();
            let status = unsafe {
                ffi::notmuch_database_find_message(
                    self.ptr.as_ptr(),
                    prepared.message_id_c.as_ptr(),
                    &mut message,
                )
            };
            if let Err(err) = check(status, self.status_string()) {
                report.failures.push(MessageTagFailure {
                    message_id: prepared.message_id.clone(),
                    stage: TagFailureStage::Lookup,
                    detail: err.to_string(),
                    current_filenames: Vec::new(),
                    file_failures: Vec::new(),
                });
                continue;
            }
            if message.is_null() {
                report.failures.push(MessageTagFailure {
                    message_id: prepared.message_id.clone(),
                    stage: TagFailureStage::Lookup,
                    detail: "message was not found in the database".to_string(),
                    current_filenames: Vec::new(),
                    file_failures: Vec::new(),
                });
                continue;
            }
            let outcome = mutate_message(message, &prepared.mutation, &self.status_string());
            unsafe { ffi::notmuch_message_destroy(message) };
            if let Some(applied) = outcome.applied {
                report.changes.push(applied.change);
                report.path_states.push(applied.path_state);
            }
            report.failures.extend(outcome.failures);
        }
        report.changed_messages = report.changes.len();
        let end = unsafe { ffi::notmuch_database_end_atomic(self.ptr.as_ptr()) };
        if let Err(err) = check(end, self.status_string()) {
            report.record_finalization_error(err);
        }
        Ok(report)
    }

    fn create_query(&self, query: &str, options: &QueryOptions) -> Result<QueryGuard> {
        let query = CString::new(query)?;
        let q = unsafe { ffi::notmuch_query_create(self.ptr.as_ptr(), query.as_ptr()) };
        let q = NonNull::new(q).ok_or(Error::Null("notmuch_query_create"))?;
        unsafe {
            ffi::notmuch_query_set_sort(q.as_ptr(), sort_to_ffi(options.sort));
            ffi::notmuch_query_set_omit_excluded(
                q.as_ptr(),
                ffi::notmuch_exclude_t::NOTMUCH_EXCLUDE_TRUE,
            );
        }
        for tag in &options.excluded_tags {
            let tag = CString::new(tag.as_str())?;
            let status = unsafe { ffi::notmuch_query_add_tag_exclude(q.as_ptr(), tag.as_ptr()) };
            if status != ffi::notmuch_status_t::NOTMUCH_STATUS_SUCCESS
                && status != ffi::notmuch_status_t::NOTMUCH_STATUS_IGNORED
            {
                check(status, self.status_string())?;
            }
        }
        Ok(QueryGuard(q))
    }

    fn search_messages_with_query(
        &self,
        q: *mut ffi::notmuch_query_t,
        options: &QueryOptions,
    ) -> Result<Vec<MessageSummary>> {
        let mut messages = std::ptr::null_mut();
        let status = unsafe { ffi::notmuch_query_search_messages(q, &mut messages) };
        check(status, self.status_string())?;
        let mut out = Vec::new();
        let mut skipped = 0usize;
        while unsafe { ffi::notmuch_messages_valid(messages) } != 0 {
            let message = unsafe { ffi::notmuch_messages_get(messages) };
            if !message.is_null() {
                if skipped >= options.offset && out.len() < options.limit {
                    out.push(message_summary(message));
                }
                skipped += 1;
                unsafe { ffi::notmuch_message_destroy(message) };
            }
            if out.len() >= options.limit {
                break;
            }
            unsafe { ffi::notmuch_messages_move_to_next(messages) };
        }
        check_messages_iterator(messages, self.status_string())?;
        Ok(out)
    }

    fn search_message_ids_with_query(
        &self,
        q: *mut ffi::notmuch_query_t,
        options: &QueryOptions,
    ) -> Result<Vec<String>> {
        let mut messages = std::ptr::null_mut();
        let status = unsafe { ffi::notmuch_query_search_messages(q, &mut messages) };
        check(status, self.status_string())?;
        let mut out = Vec::new();
        let mut skipped = 0usize;
        while unsafe { ffi::notmuch_messages_valid(messages) } != 0 {
            let message = unsafe { ffi::notmuch_messages_get(messages) };
            if !message.is_null() {
                let message_id =
                    (skipped >= options.offset && out.len() < options.limit).then(|| unsafe {
                        required_message_id(ffi::notmuch_message_get_message_id(message))
                    });
                skipped = skipped.saturating_add(1);
                unsafe { ffi::notmuch_message_destroy(message) };
                if let Some(message_id) = message_id {
                    out.push(message_id?);
                }
            }
            if out.len() >= options.limit {
                break;
            }
            unsafe { ffi::notmuch_messages_move_to_next(messages) };
        }
        check_messages_iterator(messages, self.status_string())?;
        Ok(out)
    }
}

unsafe fn required_message_id(message_id: *const c_char) -> Result<String> {
    if message_id.is_null() {
        Err(Error::Null("notmuch_message_get_message_id"))
    } else {
        Ok(unsafe { cstr_to_string(message_id) })
    }
}

fn check_thread_page_snapshot(
    thread_id: &str,
    expected_revision: &Revision,
    expected_total: usize,
    loaded: usize,
    page: &ThreadMessagePage,
) -> Result<()> {
    if page.revision != *expected_revision || page.total as usize != expected_total {
        return Err(Error::InconsistentThreadMessages {
            thread_id: thread_id.to_string(),
            expected: expected_total,
            loaded,
        });
    }
    Ok(())
}

fn check_thread_tag_snapshot_revision(expected: &Revision, observed: &Revision) -> Result<()> {
    if observed != expected {
        return Err(Error::ThreadTagSnapshotChanged {
            expected_uuid: expected.uuid.clone(),
            expected_revision: expected.revision,
            observed_uuid: observed.uuid.clone(),
            observed_revision: observed.revision,
        });
    }
    Ok(())
}

fn isolated_thread_resolution_failure(error: &Error) -> bool {
    matches!(
        error,
        Error::Nul(_) | Error::ThreadMessageLimitExceeded { .. }
    )
}

fn check_thread_message_id_page_snapshot(
    thread_id: &str,
    expected_revision: &Revision,
    expected_total: usize,
    loaded: usize,
    page: &ThreadMessageIdPage,
) -> Result<()> {
    check_thread_tag_snapshot_revision(expected_revision, &page.revision)?;
    if page.thread_id != thread_id
        || page.total as usize != expected_total
        || page.offset != loaded
        || page.message_ids.len() > page.limit
    {
        return Err(Error::InconsistentThreadMessages {
            thread_id: thread_id.to_string(),
            expected: expected_total,
            loaded,
        });
    }
    Ok(())
}

impl Drop for Database {
    fn drop(&mut self) {
        if !self.closed {
            let _ = unsafe { ffi::notmuch_database_close(self.ptr.as_ptr()) };
            self.closed = true;
        }
        let _ = unsafe { ffi::notmuch_database_destroy(self.ptr.as_ptr()) };
    }
}

struct QueryGuard(NonNull<ffi::notmuch_query_t>);

impl QueryGuard {
    fn as_ptr(&self) -> *mut ffi::notmuch_query_t {
        self.0.as_ptr()
    }
}

impl Drop for QueryGuard {
    fn drop(&mut self) {
        unsafe { ffi::notmuch_query_destroy(self.0.as_ptr()) };
    }
}

fn optional_path_cstring(path: Option<&Path>) -> Result<Option<CString>> {
    path.map(path_to_cstring).transpose().map_err(Into::into)
}

fn optional_string_cstring(value: Option<&str>) -> Result<Option<CString>> {
    value.map(CString::new).transpose().map_err(Into::into)
}

fn ptr_or_null(value: &Option<CString>) -> *const c_char {
    value.as_ref().map_or(std::ptr::null(), |v| v.as_ptr())
}

fn close_with(
    detail: impl Into<String>,
    close: impl FnOnce() -> ffi::notmuch_status_t,
) -> Result<()> {
    check(close(), detail)
}

fn exact_term_query(prefix: &str, value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("{prefix}:\"{escaped}\"")
}

fn validate_mutation(mutation: &TagMutation) -> Result<()> {
    for tag in mutation.add.iter().chain(mutation.remove.iter()) {
        validate_tag(tag)?;
    }
    Ok(())
}

struct PreparedMessageMutation {
    message_id: String,
    message_id_c: CString,
    mutation: TagMutation,
}

fn prepare_uniform_mutations(
    message_ids: impl IntoIterator<Item = String>,
    mutation: &TagMutation,
) -> Result<Vec<PreparedMessageMutation>> {
    message_ids
        .into_iter()
        .map(|message_id| {
            Ok(PreparedMessageMutation {
                message_id_c: CString::new(message_id.as_str())?,
                message_id,
                mutation: mutation.clone(),
            })
        })
        .collect()
}

fn sort_to_ffi(sort: SortOrder) -> ffi::notmuch_sort_t {
    match sort {
        SortOrder::OldestFirst => ffi::notmuch_sort_t::NOTMUCH_SORT_OLDEST_FIRST,
        SortOrder::NewestFirst => ffi::notmuch_sort_t::NOTMUCH_SORT_NEWEST_FIRST,
        SortOrder::MessageId => ffi::notmuch_sort_t::NOTMUCH_SORT_MESSAGE_ID,
        SortOrder::Unsorted => ffi::notmuch_sort_t::NOTMUCH_SORT_UNSORTED,
    }
}

fn thread_query(thread_ids: &BTreeSet<&str>) -> String {
    thread_ids
        .iter()
        .map(|thread_id| format!("thread:{thread_id}"))
        .collect::<Vec<_>>()
        .join(" or ")
}

#[cfg(test)]
fn group_thread_messages(
    requested: &BTreeSet<&str>,
    messages: Vec<MessageSummary>,
) -> BTreeMap<String, Vec<MessageSummary>> {
    let mut grouped = requested
        .iter()
        .map(|thread_id| ((*thread_id).to_string(), Vec::new()))
        .collect::<BTreeMap<_, _>>();
    for message in messages {
        if let Some(thread_messages) = grouped.get_mut(message.thread_id.as_str()) {
            thread_messages.push(message);
        }
    }
    for thread_messages in grouped.values_mut() {
        thread_messages.sort_by_key(|message| message.date);
    }
    grouped
}

fn thread_summary(thread: *mut ffi::notmuch_thread_t) -> ThreadSummary {
    let tags = unsafe { collect_tags(ffi::notmuch_thread_get_tags(thread)) };
    ThreadSummary {
        thread_id: unsafe { cstr_to_string(ffi::notmuch_thread_get_thread_id(thread)) },
        subject: unsafe { cstr_to_string(ffi::notmuch_thread_get_subject(thread)) },
        authors: unsafe { cstr_to_string(ffi::notmuch_thread_get_authors(thread)) },
        oldest_date: unsafe { ffi::notmuch_thread_get_oldest_date(thread) as i64 },
        newest_date: unsafe { ffi::notmuch_thread_get_newest_date(thread) as i64 },
        matched_messages: unsafe { ffi::notmuch_thread_get_matched_messages(thread) },
        total_messages: unsafe { ffi::notmuch_thread_get_total_messages(thread) },
        has_unread: tags.iter().any(|t| t == "unread"),
        is_flagged: tags.iter().any(|t| t == "flagged"),
        tags,
    }
}

#[cfg(test)]
std::thread_local! {
    static MESSAGE_SUMMARY_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MESSAGE_FILENAME_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_message_materialization_counters() {
    MESSAGE_SUMMARY_READS.set(0);
    MESSAGE_FILENAME_READS.set(0);
}

#[cfg(test)]
fn message_materialization_counts() -> (usize, usize) {
    (MESSAGE_SUMMARY_READS.get(), MESSAGE_FILENAME_READS.get())
}

fn message_summary(message: *mut ffi::notmuch_message_t) -> MessageSummary {
    #[cfg(test)]
    MESSAGE_SUMMARY_READS.set(MESSAGE_SUMMARY_READS.get().saturating_add(1));
    MessageSummary {
        message_id: unsafe { cstr_to_string(ffi::notmuch_message_get_message_id(message)) },
        thread_id: unsafe { cstr_to_string(ffi::notmuch_message_get_thread_id(message)) },
        date: unsafe { ffi::notmuch_message_get_date(message) as i64 },
        from: header(message, "From"),
        to: header(message, "To"),
        cc: header(message, "Cc"),
        subject: header(message, "Subject"),
        tags: unsafe { collect_tags(ffi::notmuch_message_get_tags(message)) },
        filenames: unsafe { collect_filenames(ffi::notmuch_message_get_filenames(message)) },
    }
}

fn header(message: *mut ffi::notmuch_message_t, name: &str) -> String {
    let Ok(name) = CString::new(name) else {
        return String::new();
    };
    unsafe { cstr_to_string(ffi::notmuch_message_get_header(message, name.as_ptr())) }
}

#[cfg(notmuch_has_iterator_status)]
fn check_threads_iterator(
    threads: *mut ffi::notmuch_threads_t,
    detail: impl Into<String>,
) -> Result<()> {
    let status = unsafe { ffi::notmuch_threads_status(threads) };
    check_iterator_status(status, detail)
}

#[cfg(not(notmuch_has_iterator_status))]
fn check_threads_iterator(
    _threads: *mut ffi::notmuch_threads_t,
    _detail: impl Into<String>,
) -> Result<()> {
    // Before Notmuch 0.40, validity was the only available iterator signal.
    Ok(())
}

#[cfg(notmuch_has_iterator_status)]
fn check_messages_iterator(
    messages: *mut ffi::notmuch_messages_t,
    detail: impl Into<String>,
) -> Result<()> {
    let status = unsafe { ffi::notmuch_messages_status(messages) };
    check_iterator_status(status, detail)
}

#[cfg(not(notmuch_has_iterator_status))]
fn check_messages_iterator(
    _messages: *mut ffi::notmuch_messages_t,
    _detail: impl Into<String>,
) -> Result<()> {
    // Before Notmuch 0.40, validity was the only available iterator signal.
    Ok(())
}

#[cfg(notmuch_has_iterator_status)]
fn check_iterator_status(status: ffi::notmuch_status_t, detail: impl Into<String>) -> Result<()> {
    if status == ffi::notmuch_status_t::NOTMUCH_STATUS_SUCCESS
        || status == ffi::notmuch_status_t::NOTMUCH_STATUS_ITERATOR_EXHAUSTED
    {
        Ok(())
    } else {
        check(status, detail)
    }
}

unsafe fn collect_tags(tags: *mut ffi::notmuch_tags_t) -> Vec<String> {
    let mut out = Vec::new();
    while unsafe { ffi::notmuch_tags_valid(tags) } != 0 {
        out.push(unsafe { cstr_to_string(ffi::notmuch_tags_get(tags)) });
        unsafe { ffi::notmuch_tags_move_to_next(tags) };
    }
    out
}

unsafe fn collect_filenames(files: *mut ffi::notmuch_filenames_t) -> Vec<String> {
    unsafe { collect_filename_paths(files) }
        .iter()
        .map(|path| report_filename(path))
        .collect()
}

unsafe fn collect_filename_paths(files: *mut ffi::notmuch_filenames_t) -> Vec<PathBuf> {
    #[cfg(test)]
    MESSAGE_FILENAME_READS.set(MESSAGE_FILENAME_READS.get().saturating_add(1));
    let mut out = Vec::new();
    while unsafe { ffi::notmuch_filenames_valid(files) } != 0 {
        out.push(unsafe { cstr_to_path_buf(ffi::notmuch_filenames_get(files)) });
        unsafe { ffi::notmuch_filenames_move_to_next(files) };
    }
    if !files.is_null() {
        unsafe { ffi::notmuch_filenames_destroy(files) };
    }
    out
}

/// Convert a filename returned by libnotmuch without losing non-UTF-8 Unix bytes.
///
/// # Safety
///
/// `ptr` must be either null or point to a valid NUL-terminated string for the
/// duration of this call.
unsafe fn cstr_to_path_buf(ptr: *const c_char) -> PathBuf {
    if ptr.is_null() {
        return PathBuf::new();
    }
    let bytes = unsafe { CStr::from_ptr(ptr) }.to_bytes().to_vec();
    #[cfg(unix)]
    {
        PathBuf::from(OsString::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn open_message_candidates(
    message_id: &str,
    mut candidates: Vec<PathBuf>,
) -> Result<ResolvedMessageFile> {
    candidates.sort();
    candidates.dedup();

    let mut failures = Vec::new();
    for path in &candidates {
        match File::open(path) {
            Ok(file) => match file.metadata() {
                Ok(metadata) if metadata.is_file() => {
                    return Ok(ResolvedMessageFile {
                        path: path.clone(),
                        file,
                    });
                }
                Ok(_) => failures.push(format!("{}: not a regular file", path.display())),
                Err(error) => failures.push(format!("{}: {error}", path.display())),
            },
            Err(error) => failures.push(format!("{}: {error}", path.display())),
        }
    }

    const DISPLAYED_FAILURES: usize = 8;
    let omitted = failures.len().saturating_sub(DISPLAYED_FAILURES);
    failures.truncate(DISPLAYED_FAILURES);
    let mut detail = if failures.is_empty() {
        String::new()
    } else {
        format!(": {}", failures.join("; "))
    };
    if omitted > 0 {
        detail.push_str(&format!("; {omitted} more candidate(s) failed"));
    }
    Err(Error::MessageFileUnavailable {
        message_id: message_id.to_string(),
        detail,
    })
}

fn mutate_message(
    message: *mut ffi::notmuch_message_t,
    mutation: &TagMutation,
    detail: &str,
) -> MessageMutationOutcome {
    let message_id = unsafe { cstr_to_string(ffi::notmuch_message_get_message_id(message)) };
    let mut before_tags = unsafe { collect_tags(ffi::notmuch_message_get_tags(message)) };
    before_tags.sort();
    let mut before_filename_paths =
        unsafe { collect_filename_paths(ffi::notmuch_message_get_filenames(message)) };
    before_filename_paths.sort();
    let before_filenames = report_filenames(&before_filename_paths);
    let effective = match effective_tag_mutation(&before_tags, mutation) {
        Some(effective) => effective,
        None if mutation.sync_maildir_flags => TagMutation {
            add: Vec::new(),
            remove: Vec::new(),
            sync_maildir_flags: true,
        },
        None => return MessageMutationOutcome::default(),
    };
    if let Err(err) = check(
        unsafe { ffi::notmuch_message_freeze(message) },
        detail.to_string(),
    ) {
        return MessageMutationOutcome {
            applied: None,
            failures: vec![message_failure(
                &message_id,
                TagFailureStage::Freeze,
                err,
                before_filenames,
                Vec::new(),
            )],
        };
    }

    let mut failures = Vec::new();
    for tag in &effective.remove {
        let tag_c = match CString::new(tag.as_str()) {
            Ok(tag) => tag,
            Err(err) => {
                failures.push(message_failure(
                    &message_id,
                    TagFailureStage::RemoveTag,
                    err,
                    before_filenames.clone(),
                    Vec::new(),
                ));
                break;
            }
        };
        if let Err(err) = check(
            unsafe { ffi::notmuch_message_remove_tag(message, tag_c.as_ptr()) },
            detail.to_string(),
        ) {
            failures.push(message_failure(
                &message_id,
                TagFailureStage::RemoveTag,
                format!("removing tag `{tag}` failed: {err}"),
                before_filenames.clone(),
                Vec::new(),
            ));
            break;
        }
    }
    if failures.is_empty() {
        for tag in &effective.add {
            let tag_c = match CString::new(tag.as_str()) {
                Ok(tag) => tag,
                Err(err) => {
                    failures.push(message_failure(
                        &message_id,
                        TagFailureStage::AddTag,
                        err,
                        before_filenames.clone(),
                        Vec::new(),
                    ));
                    break;
                }
            };
            if let Err(err) = check(
                unsafe { ffi::notmuch_message_add_tag(message, tag_c.as_ptr()) },
                detail.to_string(),
            ) {
                failures.push(message_failure(
                    &message_id,
                    TagFailureStage::AddTag,
                    format!("adding tag `{tag}` failed: {err}"),
                    before_filenames.clone(),
                    Vec::new(),
                ));
                break;
            }
        }
    }

    let thaw_succeeded = match check(
        unsafe { ffi::notmuch_message_thaw(message) },
        detail.to_string(),
    ) {
        Ok(()) => true,
        Err(err) => {
            failures.push(message_failure(
                &message_id,
                TagFailureStage::Thaw,
                err,
                before_filenames.clone(),
                Vec::new(),
            ));
            false
        }
    };

    let mut after_tags = unsafe { collect_tags(ffi::notmuch_message_get_tags(message)) };
    after_tags.sort();
    let tag_operations_succeeded = failures
        .iter()
        .all(|failure| failure.stage == TagFailureStage::MaildirFlagSync);
    let mut current_filename_paths =
        unsafe { collect_filename_paths(ffi::notmuch_message_get_filenames(message)) };
    current_filename_paths.sort();
    let mut path_changes = before_filename_paths
        .iter()
        .cloned()
        .map(|path| MaildirPathChange {
            previous_path: path.clone(),
            current_path: path,
        })
        .collect::<Vec<_>>();

    if effective.sync_maildir_flags && thaw_succeeded && tag_operations_succeeded {
        let expectations = before_filename_paths
            .iter()
            .map(|filename| {
                (
                    filename.clone(),
                    expected_maildir_filename(filename, &after_tags),
                )
            })
            .collect::<Vec<_>>();
        let sync_error = check(
            unsafe { ffi::notmuch_message_tags_to_maildir_flags(message) },
            detail.to_string(),
        )
        .err();
        let mut database_filename_paths =
            unsafe { collect_filename_paths(ffi::notmuch_message_get_filenames(message)) };
        database_filename_paths.sort();
        let (authoritative, reconciled_path_changes, file_failures) =
            reconcile_maildir_filenames(&expectations, &database_filename_paths);
        current_filename_paths = authoritative;
        path_changes = reconciled_path_changes;
        let current_filenames = report_filenames(&current_filename_paths);
        if let Some(err) = sync_error {
            failures.push(message_failure(
                &message_id,
                TagFailureStage::MaildirFlagSync,
                err,
                current_filenames.clone(),
                file_failures,
            ));
        } else if !file_failures.is_empty() {
            failures.push(message_failure(
                &message_id,
                TagFailureStage::MaildirFlagSync,
                "one or more Maildir files were not renamed to the expected path",
                current_filenames.clone(),
                file_failures,
            ));
        }
    }

    finalize_message_mutation(
        message_id,
        &before_tags,
        after_tags,
        current_filename_paths,
        path_changes,
        failures,
    )
}

#[derive(Default)]
struct MessageMutationOutcome {
    applied: Option<AppliedMessageMutation>,
    failures: Vec<MessageTagFailure>,
}

struct AppliedMessageMutation {
    change: AppliedTagChange,
    path_state: MessagePathState,
}

fn finalize_message_mutation(
    message_id: String,
    before_tags: &[String],
    after_tags: Vec<String>,
    current_filename_paths: Vec<PathBuf>,
    path_changes: Vec<MaildirPathChange>,
    mut failures: Vec<MessageTagFailure>,
) -> MessageMutationOutcome {
    let current_filenames = report_filenames(&current_filename_paths);
    for failure in &mut failures {
        failure.current_filenames = current_filenames.clone();
    }

    if failures
        .iter()
        .any(|failure| failure.stage == TagFailureStage::Thaw)
    {
        return MessageMutationOutcome {
            applied: None,
            failures,
        };
    }

    let filename_changes = report_filename_changes(&path_changes);
    let applied = applied_tag_change(
        &message_id,
        before_tags,
        after_tags,
        current_filenames,
        filename_changes,
    )
    .map(|change| AppliedMessageMutation {
        change,
        path_state: MessagePathState {
            message_id,
            paths: current_filename_paths,
            path_changes,
        },
    });
    MessageMutationOutcome { applied, failures }
}

fn message_failure(
    message_id: &str,
    stage: TagFailureStage,
    detail: impl std::fmt::Display,
    current_filenames: Vec<String>,
    file_failures: Vec<MaildirFlagSyncFailure>,
) -> MessageTagFailure {
    MessageTagFailure {
        message_id: message_id.to_string(),
        stage,
        detail: detail.to_string(),
        current_filenames,
        file_failures,
    }
}

fn effective_tag_mutation(current_tags: &[String], mutation: &TagMutation) -> Option<TagMutation> {
    let before = current_tags.iter().cloned().collect::<BTreeSet<_>>();
    let mut after = before.clone();
    for tag in &mutation.remove {
        after.remove(tag);
    }
    for tag in &mutation.add {
        after.insert(tag.clone());
    }
    let added = after.difference(&before).cloned().collect::<Vec<_>>();
    let removed = before.difference(&after).cloned().collect::<Vec<_>>();
    if added.is_empty() && removed.is_empty() {
        return None;
    }
    Some(TagMutation {
        add: added,
        remove: removed,
        sync_maildir_flags: mutation.sync_maildir_flags,
    })
}

fn applied_tag_change(
    message_id: &str,
    before_tags: &[String],
    mut after_tags: Vec<String>,
    mut filenames: Vec<String>,
    filename_changes: Vec<MaildirFilenameChange>,
) -> Option<AppliedTagChange> {
    after_tags.sort();
    after_tags.dedup();
    filenames.sort();
    filenames.dedup();
    let before = before_tags.iter().cloned().collect::<BTreeSet<_>>();
    let after = after_tags.iter().cloned().collect::<BTreeSet<_>>();
    let added = after.difference(&before).cloned().collect::<Vec<_>>();
    let removed = before.difference(&after).cloned().collect::<Vec<_>>();
    let filenames_changed = filename_changes
        .iter()
        .any(|change| change.previous_filename != change.current_filename);
    if added.is_empty() && removed.is_empty() && !filenames_changed {
        return None;
    }
    Some(AppliedTagChange {
        message_id: message_id.to_string(),
        added,
        removed,
        tags: after_tags,
        filenames,
        filename_changes,
    })
}

// Public message/report models retain their String filename contract. Keep this
// lossy conversion at that boundary; filesystem reconciliation must use Path.
fn report_filename(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn report_filenames(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|path| report_filename(path)).collect()
}

fn report_filename_changes(changes: &[MaildirPathChange]) -> Vec<MaildirFilenameChange> {
    changes
        .iter()
        .map(|change| MaildirFilenameChange {
            previous_filename: report_filename(&change.previous_path),
            current_filename: report_filename(&change.current_path),
        })
        .collect()
}

fn expected_maildir_filename(filename: &Path, tags: &[String]) -> PathBuf {
    #[cfg(unix)]
    {
        let expected = expected_maildir_filename_bytes(filename.as_os_str().as_bytes(), tags);
        PathBuf::from(OsString::from_vec(expected))
    }
    #[cfg(not(unix))]
    {
        let filename = filename.to_string_lossy();
        let expected = expected_maildir_filename_bytes(filename.as_bytes(), tags);
        PathBuf::from(String::from_utf8_lossy(&expected).into_owned())
    }
}

fn expected_maildir_filename_bytes(filename: &[u8], tags: &[String]) -> Vec<u8> {
    let Some(last_slash) = filename.iter().rposition(|byte| *byte == b'/') else {
        return filename.to_vec();
    };
    let directory_start = filename[..last_slash]
        .iter()
        .rposition(|byte| *byte == b'/')
        .map_or(0, |slash| slash + 1);
    let directory = &filename[directory_start..last_slash];
    if directory != b"cur" && directory != b"new" {
        return filename.to_vec();
    }

    let maildir_info = filename
        .windows(3)
        .rposition(|window| window == b":2,")
        .filter(|info| *info > last_slash);
    let flag_bytes = maildir_info.map_or(&[][..], |info| &filename[info + 3..]);
    let mut flags = BTreeSet::new();
    let mut previous = None;
    for flag in flag_bytes {
        if !flag.is_ascii()
            || previous.is_some_and(|previous| *flag < previous)
            || !flags.insert(*flag)
        {
            return filename.to_vec();
        }
        previous = Some(*flag);
    }

    let tags = tags.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let desired_flags = [
        (b'D', tags.contains("draft")),
        (b'F', tags.contains("flagged")),
        (b'P', tags.contains("passed")),
        (b'R', tags.contains("replied")),
        (b'S', !tags.contains("unread")),
    ];
    let mut flags_changed = false;
    for (flag, desired) in desired_flags {
        if desired {
            flags_changed |= flags.insert(flag);
        } else {
            flags_changed |= flags.remove(&flag);
        }
    }

    if directory == b"new" && maildir_info.is_none() && !flags_changed {
        return filename.to_vec();
    }
    let prefix_end = maildir_info.unwrap_or(filename.len());
    let mut expected = Vec::with_capacity(prefix_end.saturating_add(3 + flags.len()));
    expected.extend_from_slice(&filename[..prefix_end]);
    expected.extend_from_slice(b":2,");
    expected.extend(flags);
    if directory == b"new" {
        expected.splice(directory_start..last_slash, b"cur".iter().copied());
    }
    expected
}

fn reconcile_maildir_filenames(
    expectations: &[(PathBuf, PathBuf)],
    database_filenames: &[PathBuf],
) -> (
    Vec<PathBuf>,
    Vec<MaildirPathChange>,
    Vec<MaildirFlagSyncFailure>,
) {
    let database_filenames = database_filenames.iter().cloned().collect::<BTreeSet<_>>();
    let mut authoritative = BTreeSet::new();
    let mut claimed = BTreeSet::new();
    let mut path_changes = Vec::new();
    let mut failures = Vec::new();
    for (previous, expected) in expectations {
        let current = [expected, previous]
            .into_iter()
            .find(|candidate| {
                !claimed.contains(candidate.as_path())
                    && database_filenames.contains(candidate.as_path())
                    && path_is_message_file(candidate)
            })
            .or_else(|| {
                [expected, previous].into_iter().find(|candidate| {
                    !claimed.contains(candidate.as_path()) && path_is_message_file(candidate)
                })
            })
            .cloned();
        if let Some(current) = &current {
            claimed.insert(current.clone());
            authoritative.insert(current.clone());
            path_changes.push(MaildirPathChange {
                previous_path: previous.clone(),
                current_path: current.clone(),
            });
        }
        let database_and_file_match =
            database_filenames.contains(expected.as_path()) && path_is_message_file(expected);
        if current.as_deref() != Some(expected.as_path()) || !database_and_file_match {
            failures.push(MaildirFlagSyncFailure {
                previous_filename: report_filename(previous),
                expected_filename: report_filename(expected),
                current_filename: current.as_deref().map(report_filename),
            });
        }
    }

    let known = expectations
        .iter()
        .flat_map(|(previous, expected)| [previous, expected])
        .collect::<BTreeSet<_>>();
    for filename in database_filenames {
        if !known.contains(&filename) && path_is_message_file(&filename) {
            authoritative.insert(filename);
        }
    }
    (authoritative.into_iter().collect(), path_changes, failures)
}

fn path_is_message_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, sync::atomic::AtomicU64};

    static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> std::io::Result<Self> {
            loop {
                let sequence = NEXT_TEMP_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "notm-notmuch-batch-test-{}-{sequence}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Ok(Self(path)),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn message(thread_id: &str, message_id: &str, date: i64) -> MessageSummary {
        MessageSummary {
            message_id: message_id.to_string(),
            thread_id: thread_id.to_string(),
            date,
            from: String::new(),
            to: String::new(),
            cc: String::new(),
            subject: String::new(),
            tags: Vec::new(),
            filenames: Vec::new(),
        }
    }

    #[test]
    fn batched_thread_query_deduplicates_requested_threads() {
        let requested = ["thread-b", "thread-a", "thread-b"]
            .into_iter()
            .collect::<BTreeSet<_>>();

        assert_eq!(
            thread_query(&requested),
            "thread:thread-a or thread:thread-b"
        );
    }

    #[test]
    fn grouped_thread_messages_include_missing_threads_and_sort_each_group_oldest_first() {
        let requested = ["thread-a", "thread-b", "thread-missing"]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let grouped = group_thread_messages(
            &requested,
            vec![
                message("thread-a", "newest-a", 30),
                message("thread-b", "only-b", 20),
                message("unrequested", "ignored", 1),
                message("thread-a", "oldest-a", 10),
                message("thread-a", "middle-a", 20),
            ],
        );

        assert_eq!(grouped.len(), 3);
        assert_eq!(
            grouped["thread-a"]
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            ["oldest-a", "middle-a", "newest-a"]
        );
        assert_eq!(grouped["thread-b"][0].message_id, "only-b");
        assert!(grouped["thread-missing"].is_empty());
        assert!(!grouped.contains_key("unrequested"));
    }

    #[test]
    fn batched_thread_message_lookup_groups_real_notmuch_results() -> Result<()> {
        let temp = TestDirectory::new()?;
        let root = temp.path().join("mail");
        let maildir = root.join("inbox/cur");
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
        let fixtures = [
            (
                "root:2,",
                "From: sender@example.test\nTo: fixture@example.test\nSubject: Batch thread\nMessage-ID: <batch-root@fixture.test>\nDate: Tue, 25 Aug 2026 12:00:00 -0600\n\nOldest.\n",
            ),
            (
                "newest:2,",
                "From: sender@example.test\nTo: fixture@example.test\nSubject: Re: Batch thread\nMessage-ID: <batch-newest@fixture.test>\nIn-Reply-To: <batch-middle@fixture.test>\nReferences: <batch-root@fixture.test> <batch-middle@fixture.test>\nDate: Tue, 25 Aug 2026 12:02:00 -0600\n\nNewest.\n",
            ),
            (
                "middle:2,",
                "From: sender@example.test\nTo: fixture@example.test\nSubject: Re: Batch thread\nMessage-ID: <batch-middle@fixture.test>\nIn-Reply-To: <batch-root@fixture.test>\nReferences: <batch-root@fixture.test>\nDate: Tue, 25 Aug 2026 12:01:00 -0600\n\nMiddle.\n",
            ),
            (
                "other:2,",
                "From: other@example.test\nTo: fixture@example.test\nSubject: Other batch thread\nMessage-ID: <batch-other@fixture.test>\nDate: Tue, 25 Aug 2026 12:03:00 -0600\n\nOther.\n",
            ),
        ];
        for (filename, raw) in fixtures {
            let path = maildir.join(filename);
            fs::write(&path, raw)?;
            db.index_file_with_tags(&path, &["batch-test"])?;
        }

        let threads = db.search_threads(
            "tag:batch-test",
            &QueryOptions {
                sort: SortOrder::NewestFirst,
                limit: 10,
                offset: 0,
                excluded_tags: Vec::new(),
            },
        )?;
        assert_eq!(threads.len(), 2);
        let multi_thread_id = threads
            .iter()
            .find(|thread| thread.total_messages == 3)
            .expect("three-message thread")
            .thread_id
            .clone();
        let other_thread_id = threads
            .iter()
            .find(|thread| thread.total_messages == 1)
            .expect("single-message thread")
            .thread_id
            .clone();
        let missing_thread_id = "missing-thread-id".to_string();

        let requested = [
            other_thread_id.clone(),
            missing_thread_id.clone(),
            multi_thread_id.clone(),
            multi_thread_id.clone(),
        ];
        let grouped = db.thread_messages_for_threads_bounded(&requested, 3, 4)?;

        assert_eq!(grouped.len(), 3);
        let BoundedThreadMessages::Loaded(multi_thread) = &grouped[&multi_thread_id] else {
            panic!("three-message thread should load at its exact bound");
        };
        assert_eq!(
            multi_thread
                .iter()
                .map(|message| message.message_id.as_str())
                .collect::<Vec<_>>(),
            [
                "batch-root@fixture.test",
                "batch-middle@fixture.test",
                "batch-newest@fixture.test"
            ]
        );
        let BoundedThreadMessages::Loaded(other_thread) = &grouped[&other_thread_id] else {
            panic!("single-message sibling should load");
        };
        assert_eq!(other_thread[0].message_id, "batch-other@fixture.test");
        assert_eq!(
            grouped[&missing_thread_id],
            BoundedThreadMessages::Loaded(Vec::new())
        );

        let per_thread_limited = db.thread_messages_for_threads_bounded(&requested, 2, 4)?;
        assert!(matches!(
            &per_thread_limited[&multi_thread_id],
            BoundedThreadMessages::ThreadLimitExceeded {
                thread_id,
                total: 3,
                limit: 2,
            } if thread_id == &multi_thread_id
        ));
        let BoundedThreadMessages::Loaded(other_thread) = &per_thread_limited[&other_thread_id]
        else {
            panic!("oversized thread must not hide an in-bound sibling");
        };
        assert_eq!(other_thread.len(), 1);

        let batch_limited = db.thread_messages_for_threads_bounded(&requested, 3, 3)?;
        for thread_id in [&multi_thread_id, &other_thread_id] {
            assert_eq!(
                batch_limited[thread_id],
                BoundedThreadMessages::BatchLimitExceeded { total: 4, limit: 3 }
            );
        }
        assert_eq!(
            batch_limited[&missing_thread_id],
            BoundedThreadMessages::Loaded(Vec::new()),
            "a missing requested ID consumes no materialization budget"
        );
        let direct = db
            .find_message("batch-middle@fixture.test")?
            .expect("direct message lookup");
        assert_eq!(direct.message_id, "batch-middle@fixture.test");
        assert_eq!(direct.thread_id, multi_thread_id);
        assert!(db.find_message("missing@fixture.test")?.is_none());
        Ok(())
    }

    fn create_test_database() -> (tempfile::TempDir, Database, PathBuf) {
        let temp = tempfile::tempdir().expect("temporary Notmuch test root");
        let root = temp.path().join("mail");
        let maildir = root.join("account/cur");
        std::fs::create_dir_all(&maildir).expect("create test Maildir cur");
        std::fs::create_dir_all(root.join("account/new")).expect("create test Maildir new");
        std::fs::create_dir_all(root.join("account/tmp")).expect("create test Maildir tmp");
        let config_path = temp.path().join("notmuch-config");
        std::fs::write(
            &config_path,
            format!(
                "[database]\npath={}\n\n[user]\nname=Fixture User\nprimary_email=fixture@example.test\n\n[new]\ntags=\nignore=\n\n[search]\nexclude_tags=\n\n[maildir]\nsynchronize_flags=true\n",
                root.display()
            ),
        )
        .expect("write Notmuch test config");
        let database = Database::create(&OpenConfig {
            database_path: Some(root),
            config_path: Some(config_path),
            profile: None,
        })
        .expect("create Notmuch test database");
        (temp, database, maildir)
    }

    fn summary_by_id(database: &Database, message_id: &str) -> MessageSummary {
        database
            .search_messages(
                &format!("id:{message_id}"),
                &QueryOptions {
                    limit: 1,
                    offset: 0,
                    sort: SortOrder::MessageId,
                    excluded_tags: Vec::new(),
                },
            )
            .expect("query test message")
            .pop()
            .expect("indexed test message")
    }

    fn index_test_thread(
        database: &Database,
        maildir: &Path,
        prefix: &str,
        message_count: usize,
    ) -> (String, String) {
        assert!(message_count > 0);
        let root_id = format!("{prefix}-0000@example.test");
        for index in 0..message_count {
            let message_id = format!("{prefix}-{index:04}@example.test");
            let reply_headers = if index == 0 {
                String::new()
            } else {
                format!("In-Reply-To: <{root_id}>\r\nReferences: <{root_id}>\r\n")
            };
            let raw = format!(
                "From: sender@example.test\r\nTo: fixture@example.test\r\nSubject: {prefix} thread\r\nDate: Thu, 18 Jun 2037 20:{:02}:00 -0600\r\nMessage-ID: <{message_id}>\r\n{reply_headers}\r\nbody {index}\r\n",
                index % 60
            );
            let path = maildir.join(format!("{prefix}-{index:04}:2,"));
            std::fs::write(&path, raw).expect("write threaded test message");
            database
                .index_file_with_tags(&path, &[])
                .expect("index threaded test message");
        }
        let thread_id = summary_by_id(database, &root_id).thread_id;
        (thread_id, root_id)
    }

    #[test]
    fn effective_tag_mutation_records_only_net_per_message_delta() {
        let mutation = TagMutation {
            add: vec!["inbox".to_string(), "project".to_string()],
            remove: vec!["unread".to_string(), "missing".to_string()],
            sync_maildir_flags: true,
        };

        let effective =
            effective_tag_mutation(&["inbox".to_string(), "unread".to_string()], &mutation)
                .expect("net change");

        assert_eq!(effective.add, ["project"]);
        assert_eq!(effective.remove, ["unread"]);
        assert!(effective.sync_maildir_flags);
    }

    #[test]
    fn effective_tag_mutation_respects_remove_then_add_for_overlapping_tags() {
        let mutation = TagMutation {
            add: vec!["inbox".to_string()],
            remove: vec!["inbox".to_string()],
            sync_maildir_flags: false,
        };

        assert!(effective_tag_mutation(&["inbox".to_string()], &mutation).is_none());
        let effective = effective_tag_mutation(&[], &mutation)
            .expect("remove then add leaves an absent tag added");
        assert_eq!(effective.add, ["inbox"]);
        assert!(effective.remove.is_empty());
    }

    #[test]
    fn thaw_failure_finalization_suppresses_uncommitted_applied_state() {
        let message_id = "thaw-failure@example.test";
        let path = PathBuf::from("/mail/cur/thaw-failure:2,S");
        let outcome = finalize_message_mutation(
            message_id.to_string(),
            &["inbox".to_string()],
            vec!["inbox".to_string(), "staged".to_string()],
            vec![path.clone()],
            vec![MaildirPathChange {
                previous_path: path.clone(),
                current_path: path.clone(),
            }],
            vec![MessageTagFailure {
                message_id: message_id.to_string(),
                stage: TagFailureStage::Thaw,
                detail: "forced thaw failure".to_string(),
                current_filenames: Vec::new(),
                file_failures: Vec::new(),
            }],
        );

        assert!(
            outcome.applied.is_none(),
            "staged tags and their path state are not authoritative after thaw fails"
        );
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].stage, TagFailureStage::Thaw);
        assert_eq!(outcome.failures[0].detail, "forced thaw failure");
        assert_eq!(
            outcome.failures[0].current_filenames,
            [path.to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn successful_finalization_retains_applied_change_and_path_state() {
        let message_id = "thaw-success@example.test";
        let path = PathBuf::from("/mail/cur/thaw-success:2,S");
        let outcome = finalize_message_mutation(
            message_id.to_string(),
            &["inbox".to_string()],
            vec!["inbox".to_string(), "stored".to_string()],
            vec![path.clone()],
            vec![MaildirPathChange {
                previous_path: path.clone(),
                current_path: path.clone(),
            }],
            Vec::new(),
        );

        assert!(outcome.failures.is_empty());
        let applied = outcome.applied.expect("successful applied mutation");
        assert_eq!(applied.change.message_id, message_id);
        assert_eq!(applied.change.added, ["stored"]);
        assert_eq!(applied.change.tags, ["inbox", "stored"]);
        assert_eq!(applied.path_state.message_id, message_id);
        assert_eq!(
            applied.path_state.paths.as_slice(),
            std::slice::from_ref(&path)
        );
        assert_eq!(
            applied.path_state.path_changes,
            [MaildirPathChange {
                previous_path: path.clone(),
                current_path: path,
            }]
        );
    }

    #[test]
    fn expected_maildir_filename_handles_new_cur_and_preserved_flags() {
        assert_eq!(
            expected_maildir_filename(Path::new("/mail/new/example"), &["unread".to_string()]),
            Path::new("/mail/new/example")
        );
        assert_eq!(
            expected_maildir_filename(Path::new("/mail/new/example"), &[]),
            Path::new("/mail/cur/example:2,S")
        );
        assert_eq!(
            expected_maildir_filename(
                Path::new("/mail/cur/example:2,RSZ"),
                &["draft".to_string(), "unread".to_string()]
            ),
            Path::new("/mail/cur/example:2,DZ")
        );
        assert_eq!(
            expected_maildir_filename(Path::new("/mail/cur/example:2,SS"), &[]),
            Path::new("/mail/cur/example:2,SS"),
            "invalid duplicate flags must not be rewritten"
        );
    }

    #[test]
    fn close_failure_seam_surfaces_the_injected_status() {
        let error = close_with("forced close failure", || {
            ffi::notmuch_status_t::NOTMUCH_STATUS_XAPIAN_EXCEPTION
        })
        .expect_err("injected close failure must be returned");

        assert!(
            error
                .to_string()
                .contains("NOTMUCH_STATUS_XAPIAN_EXCEPTION")
        );
        assert!(error.to_string().contains("forced close failure"));
    }

    #[test]
    fn message_file_resolution_refreshes_and_tries_every_current_filename() {
        let (_temp, database, maildir) = create_test_database();
        let first = maildir.join("a-missing:2,");
        let duplicate = maildir.join("z-duplicate:2,");
        let raw = b"From: sender@example.test\r\nTo: fixture@example.test\r\nSubject: resolver\r\nMessage-ID: <resolver@example.test>\r\n\r\nresolver body\r\n";
        std::fs::write(&first, raw).expect("write resolver message");
        database
            .index_file_with_tags(&first, &[])
            .expect("index first resolver filename");
        std::fs::hard_link(&first, &duplicate).expect("create hard-linked duplicate");
        database
            .index_file_with_tags(&duplicate, &[])
            .expect("index hard-linked duplicate");
        let stale_summary = summary_by_id(&database, "resolver@example.test");
        assert!(
            stale_summary
                .filenames
                .iter()
                .any(|path| Path::new(path) == first)
        );
        assert!(
            stale_summary
                .filenames
                .iter()
                .any(|path| Path::new(path) == duplicate)
        );

        std::fs::remove_file(&first).expect("remove lexically first copy");
        let mut resolved = database
            .open_message_file(&stale_summary)
            .expect("fall back to later current filename");
        assert_eq!(resolved.path(), duplicate);
        let mut resolved_raw = Vec::new();
        resolved
            .read_to_end(&mut resolved_raw)
            .expect("read already-open resolver file");
        assert_eq!(resolved_raw, raw);

        let moved = maildir.join("m-moved:2,S");
        std::fs::rename(&duplicate, &moved).expect("move duplicate within Maildir");
        database
            .remove_message_file(&duplicate)
            .expect("remove old moved filename from index");
        database
            .index_file_with_tags(&moved, &[])
            .expect("index moved filename");
        let preferred = maildir.join("b-preferred:2,S");
        std::fs::hard_link(&moved, &preferred).expect("create second current hard link");
        database
            .index_file_with_tags(&preferred, &[])
            .expect("index second current hard link");

        let resolved = database
            .open_message_file(&stale_summary)
            .expect("refresh stale summary filenames after move");
        assert_eq!(
            resolved.path(),
            preferred,
            "current readable candidates should be chosen in deterministic path order"
        );
        let resolved_by_id = database
            .open_message_id_file("resolver@example.test")
            .expect("resolve the moved message directly by Message-ID");
        assert_eq!(resolved_by_id.path(), preferred);
        assert!(matches!(
            database.open_message_id_file("missing@example.test"),
            Err(Error::MessageNotFound(message_id)) if message_id == "missing@example.test"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn current_filename_conversion_preserves_non_utf8_unix_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let raw = CString::new(vec![b'm', b'a', b'i', b'l', b'-', 0xff]).expect("test C string");
        let path = unsafe { cstr_to_path_buf(raw.as_ptr()) };

        assert_eq!(path.as_os_str().as_bytes(), b"mail-\xff");
    }

    #[test]
    fn complete_thread_messages_pages_past_one_thousand_without_truncation() {
        let (_temp, database, maildir) = create_test_database();
        const MESSAGE_COUNT: usize = 4_097;
        for index in 0..MESSAGE_COUNT {
            let date = chrono::DateTime::from_timestamp(1_700_000_000 + index as i64, 0)
                .expect("valid bulk message timestamp")
                .to_rfc2822();
            let reply_headers = if index == 0 {
                String::new()
            } else {
                "In-Reply-To: <bulk-0000@example.test>\r\nReferences: <bulk-0000@example.test>\r\n"
                    .to_string()
            };
            let raw = format!(
                "From: sender@example.test\r\nTo: fixture@example.test\r\nSubject: Bulk thread\r\nDate: {date}\r\nMessage-ID: <bulk-{index:04}@example.test>\r\n{reply_headers}\r\nbody {index}\r\n"
            );
            let path = maildir.join(format!("bulk-{index:04}:2,"));
            std::fs::write(&path, raw).expect("write bulk message");
            database
                .index_file_with_tags(&path, &[])
                .expect("index bulk message");
        }

        let root = summary_by_id(&database, "bulk-0000@example.test");
        let count_only = database
            .thread_messages_page(&root.thread_id, 0, 0)
            .expect("load count-only thread snapshot");
        assert_eq!(count_only.total as usize, MESSAGE_COUNT);
        assert!(count_only.messages.is_empty());
        assert_eq!(count_only.limit, 0);
        let first_page = database
            .thread_messages_page(&root.thread_id, 0, 257)
            .expect("load first explicit thread page");
        assert_eq!(first_page.total as usize, MESSAGE_COUNT);
        assert_eq!(first_page.messages.len(), 257);
        assert!(first_page.has_more());
        let final_page = database
            .thread_messages_page(&root.thread_id, 4_000, 257)
            .expect("load final explicit thread page");
        assert_eq!(final_page.messages.len(), 97);
        assert!(!final_page.has_more());

        let tag_snapshot = database.revision();
        reset_message_materialization_counters();
        let id_limit_error = database
            .thread_message_ids_bounded(
                &root.thread_id,
                MAX_THREAD_TAG_MESSAGES,
                &tag_snapshot,
                |_| panic!("an over-limit thread must fail before reading an ID page"),
            )
            .expect_err("reject the tag snapshot before allocating message IDs");
        assert!(matches!(
            &id_limit_error,
            Error::ThreadMessageLimitExceeded {
                total: MESSAGE_COUNT,
                limit: MAX_THREAD_TAG_MESSAGES,
                ..
            }
        ));
        assert_eq!(message_materialization_counts(), (0, 0));

        let (safe_thread_id, safe_message_id) =
            index_test_thread(&database, &maildir, "bounded-safe", 1);
        reset_message_materialization_counters();
        let rejected_tag = "notm/oversized-must-not-change";
        let rejected = database
            .apply_tags_to_threads(
                &[root.thread_id.clone(), safe_thread_id],
                &TagMutation {
                    add: vec![rejected_tag.to_string()],
                    remove: Vec::new(),
                    sync_maildir_flags: false,
                },
            )
            .expect("return an explicit per-thread resolution failure");
        assert_eq!(rejected.matched_threads, 1);
        assert_eq!(rejected.changed_threads, 1);
        assert_eq!(rejected.thread_resolution_failures.len(), 1);
        assert!(
            rejected.thread_resolution_failures[0]
                .detail
                .contains("4097 message(s)")
        );
        assert_eq!(rejected.batch.requested_messages, 1);
        assert_eq!(rejected.batch.changed_messages, 1);
        assert_eq!(message_materialization_counts().0, 0);
        assert_eq!(
            database
                .count_messages(
                    &format!("tag:{rejected_tag}"),
                    &QueryOptions {
                        sort: SortOrder::MessageId,
                        limit: 1,
                        offset: 0,
                        excluded_tags: Vec::new(),
                    },
                )
                .expect("count rejected tag"),
            1
        );
        assert_eq!(
            database
                .search_messages(
                    &format!("tag:{rejected_tag}"),
                    &QueryOptions {
                        sort: SortOrder::MessageId,
                        limit: 2,
                        offset: 0,
                        excluded_tags: Vec::new(),
                    },
                )
                .expect("load the isolated safe target")
                .into_iter()
                .map(|message| message.message_id)
                .collect::<Vec<_>>(),
            [safe_message_id]
        );
        assert_eq!(
            database
                .count_messages(
                    &format!(
                        "{} and tag:{rejected_tag}",
                        exact_term_query("thread", &root.thread_id)
                    ),
                    &QueryOptions {
                        sort: SortOrder::MessageId,
                        limit: 1,
                        offset: 0,
                        excluded_tags: Vec::new(),
                    },
                )
                .expect("count rejected oversized thread"),
            0,
            "the oversized thread itself must remain untouched"
        );

        let too_small = database
            .thread_messages_bounded(&root.thread_id, 4_096)
            .expect_err("reject a thread above the caller's explicit bound");
        assert!(matches!(
            &too_small,
            Error::ThreadMessageLimitExceeded {
                total: MESSAGE_COUNT,
                limit: 4_096,
                ..
            }
        ));
        assert!(
            too_small
                .to_string()
                .contains("no partial thread was loaded")
        );

        let messages = database
            .thread_messages_bounded(&root.thread_id, MESSAGE_COUNT)
            .expect("load complete internally paged thread at the exact bound");
        assert_eq!(messages.len(), MESSAGE_COUNT);
        let ids = messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), MESSAGE_COUNT);
        assert!(ids.contains("bulk-4096@example.test"));
        assert_eq!(
            messages.last().map(|message| message.message_id.as_str()),
            Some("bulk-4096@example.test"),
            "the complete oldest-first result must end with the actual newest message"
        );

        let final_path = maildir.join("bulk-4096:2,");
        database
            .remove_message_file(&final_path)
            .expect("remove the final message from the index");
        std::fs::remove_file(&final_path).expect("remove the final message file");
        let exact_limit_revision = database.revision();
        reset_message_materialization_counters();
        let mut id_page_ends = Vec::new();
        let exact_limit_ids = database
            .thread_message_ids_bounded(
                &root.thread_id,
                MAX_THREAD_TAG_MESSAGES,
                &exact_limit_revision,
                |loaded| id_page_ends.push(loaded),
            )
            .expect("accept an ID-only snapshot at the exact 4,096-message bound");
        assert_eq!(exact_limit_ids.len(), MAX_THREAD_TAG_MESSAGES);
        assert_eq!(id_page_ends.len(), 16);
        assert_eq!(id_page_ends.first(), Some(&THREAD_MESSAGE_ID_PAGE_SIZE));
        assert_eq!(id_page_ends.last(), Some(&MAX_THREAD_TAG_MESSAGES));
        assert!(
            id_page_ends
                .windows(2)
                .all(|page| page[1] - page[0] == THREAD_MESSAGE_ID_PAGE_SIZE)
        );
        assert_eq!(
            exact_limit_ids.iter().collect::<BTreeSet<_>>().len(),
            MAX_THREAD_TAG_MESSAGES
        );
        assert!(
            exact_limit_ids
                .iter()
                .any(|id| id == "bulk-0000@example.test")
        );
        assert!(
            !exact_limit_ids
                .iter()
                .any(|id| id == "bulk-4096@example.test")
        );
        assert_eq!(message_materialization_counts(), (0, 0));

        let accepted_tag = "notm/exact-limit-accepted";
        let accepted = database
            .apply_tags_to_threads(
                std::slice::from_ref(&root.thread_id),
                &TagMutation {
                    add: vec![accepted_tag.to_string()],
                    remove: Vec::new(),
                    sync_maildir_flags: false,
                },
            )
            .expect("tag the exact bounded ID snapshot");
        assert!(accepted.is_complete(), "unexpected report: {accepted:#?}");
        assert_eq!(accepted.batch.requested_messages, MAX_THREAD_TAG_MESSAGES);
        assert_eq!(accepted.batch.changed_messages, MAX_THREAD_TAG_MESSAGES);
    }

    #[test]
    fn bounded_thread_collection_rejects_revision_change_with_unchanged_total() {
        let expected_revision = Revision {
            revision: 7,
            uuid: "database".to_string(),
        };
        let page = ThreadMessagePage {
            thread_id: "thread".to_string(),
            messages: Vec::new(),
            total: 512,
            offset: 256,
            limit: 256,
            revision: Revision {
                revision: 8,
                uuid: "database".to_string(),
            },
        };

        assert!(matches!(
            check_thread_page_snapshot("thread", &expected_revision, 512, 256, &page),
            Err(Error::InconsistentThreadMessages {
                expected: 512,
                loaded: 256,
                ..
            })
        ));
    }

    #[test]
    fn thread_tag_snapshot_revision_checks_number_and_uuid() {
        let expected = Revision {
            revision: 7,
            uuid: "database-a".to_string(),
        };
        assert!(check_thread_tag_snapshot_revision(&expected, &expected).is_ok());

        let numeric_drift = Revision {
            revision: 8,
            uuid: expected.uuid.clone(),
        };
        assert!(matches!(
            check_thread_tag_snapshot_revision(&expected, &numeric_drift),
            Err(Error::ThreadTagSnapshotChanged {
                expected_revision: 7,
                observed_revision: 8,
                ..
            })
        ));

        let uuid_drift = Revision {
            revision: expected.revision,
            uuid: "database-b".to_string(),
        };
        assert!(matches!(
            check_thread_tag_snapshot_revision(&expected, &uuid_drift),
            Err(Error::ThreadTagSnapshotChanged {
                expected_uuid,
                observed_uuid,
                ..
            }) if expected_uuid == "database-a" && observed_uuid == "database-b"
        ));
    }

    #[test]
    fn id_only_snapshot_rejects_a_null_libnotmuch_message_id() {
        let valid = CString::new("message@example.test").expect("valid message ID");
        assert_eq!(
            unsafe { required_message_id(valid.as_ptr()) }.expect("copy valid message ID"),
            "message@example.test"
        );

        let error = unsafe { required_message_id(std::ptr::null()) }
            .expect_err("a null ID getter result must invalidate target capture");
        assert!(matches!(
            error,
            Error::Null("notmuch_message_get_message_id")
        ));
    }

    #[test]
    fn thread_tag_snapshot_revision_drift_aborts_before_mutation() {
        let (_temp, database, maildir) = create_test_database();
        let (thread_id, root_id) = index_test_thread(&database, &maildir, "drift", 300);
        let drift_path = maildir.join("drift-interloper:2,");
        let inserted = std::cell::Cell::new(false);
        let mutation = TagMutation {
            add: vec!["notm/must-not-apply-after-drift".to_string()],
            remove: Vec::new(),
            sync_maildir_flags: false,
        };

        let error = database
            .apply_tags_to_threads_with_hooks(
                std::slice::from_ref(&thread_id),
                &mutation,
                |_, loaded| {
                    if loaded >= THREAD_MESSAGE_ID_PAGE_SIZE && !inserted.replace(true) {
                        let raw = format!(
                            "From: sender@example.test\r\nTo: fixture@example.test\r\nSubject: drift thread\r\nDate: Thu, 18 Jun 2037 19:00:00 -0600\r\nMessage-ID: <drift-interloper@example.test>\r\nIn-Reply-To: <{root_id}>\r\nReferences: <{root_id}>\r\n\r\ninterloper\r\n"
                        );
                        std::fs::write(&drift_path, raw).expect("write drift interloper");
                        database
                            .index_file_with_tags(&drift_path, &[])
                            .expect("index drift interloper");
                    }
                },
                || {},
            )
            .expect_err("revision drift must abort the whole immutable snapshot");
        assert!(inserted.get());
        assert!(matches!(error, Error::ThreadTagSnapshotChanged { .. }));
        assert_eq!(
            database
                .count_messages(
                    "tag:\"notm/must-not-apply-after-drift\"",
                    &QueryOptions {
                        sort: SortOrder::MessageId,
                        limit: usize::MAX,
                        offset: 0,
                        excluded_tags: Vec::new(),
                    },
                )
                .expect("count tags after rejected snapshot"),
            0,
            "snapshot-wide revision drift must have no tag effects"
        );
    }

    #[test]
    fn thread_tag_snapshot_keeps_exact_ids_when_new_mail_reorders_after_capture() {
        let (_temp, database, maildir) = create_test_database();
        let (thread_id, root_id) = index_test_thread(&database, &maildir, "capture", 2);
        let interloper_id = "aaaa-capture-interloper@example.test";
        let interloper_path = maildir.join("aaaa-capture-interloper:2,");
        let mutation = TagMutation {
            add: vec!["notm/captured-before-new-mail".to_string()],
            remove: Vec::new(),
            sync_maildir_flags: false,
        };

        let report = database
            .apply_tags_to_threads_with_hooks(
                std::slice::from_ref(&thread_id),
                &mutation,
                |_, _| {},
                || {
                    let raw = format!(
                        "From: sender@example.test\r\nTo: fixture@example.test\r\nSubject: capture thread\r\nDate: Thu, 18 Jun 1998 19:00:00 -0600\r\nMessage-ID: <{interloper_id}>\r\nIn-Reply-To: <{root_id}>\r\nReferences: <{root_id}>\r\n\r\nnew older mail\r\n"
                    );
                    std::fs::write(&interloper_path, raw).expect("write post-snapshot mail");
                    database
                        .index_file_with_tags(&interloper_path, &[])
                        .expect("index post-snapshot mail");
                },
            )
            .expect("mutate the already captured exact message IDs");
        assert!(report.is_complete(), "unexpected report: {report:#?}");
        assert_eq!(report.batch.requested_messages, 2);
        assert_eq!(report.batch.changed_messages, 2);

        let options = QueryOptions {
            sort: SortOrder::MessageId,
            limit: usize::MAX,
            offset: 0,
            excluded_tags: Vec::new(),
        };
        assert_eq!(
            database
                .count_messages(&exact_term_query("thread", &thread_id), &options)
                .expect("count thread after new mail"),
            3
        );
        let tagged_ids = database
            .search_messages("tag:\"notm/captured-before-new-mail\"", &options)
            .expect("query captured tag")
            .into_iter()
            .map(|message| message.message_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(tagged_ids.len(), 2);
        assert!(!tagged_ids.contains(interloper_id));
    }

    #[test]
    fn thread_tag_report_keeps_missing_and_isolated_resolution_failures_explicit() {
        let (_temp, database, maildir) = create_test_database();
        let (thread_id, _) = index_test_thread(&database, &maildir, "partial-targets", 1);
        let missing = "missing-thread".to_string();
        let invalid = "invalid\0thread".to_string();
        let report = database
            .apply_tags_to_threads(
                &[thread_id.clone(), missing.clone(), invalid.clone()],
                &TagMutation {
                    add: vec!["notm/safe-target".to_string()],
                    remove: Vec::new(),
                    sync_maildir_flags: false,
                },
            )
            .expect("retain safe-target effects beside isolated resolution failures");

        assert_eq!(report.matched_threads, 1);
        assert_eq!(report.changed_threads, 1);
        assert_eq!(report.missing_thread_ids, [missing]);
        assert_eq!(report.thread_resolution_failures.len(), 1);
        assert_eq!(report.thread_resolution_failures[0].thread_id, invalid);
        assert!(
            report.thread_resolution_failures[0]
                .detail
                .contains("interior NUL")
        );
        assert_eq!(report.batch.requested_messages, 1);
        assert_eq!(report.batch.changed_messages, 1);
        assert!(!report.is_complete());
    }
}
