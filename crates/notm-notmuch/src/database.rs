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
        TagFailureStage, TagMutation, TagOperationReport, ThreadTagReport,
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

const COMPLETE_THREAD_PAGE_SIZE: usize = 256;

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

    /// Resolve and open a message using the database's current filename list.
    ///
    /// Notmuch may associate several paths with one Message-ID, and a summary's
    /// filenames can become stale after Maildir flag synchronization. This looks
    /// the message up again, orders and de-duplicates its current filenames, and
    /// tries every candidate until one can be opened as a regular file.
    pub fn open_message_file(&self, message: &MessageSummary) -> Result<ResolvedMessageFile> {
        let message_id = CString::new(message.message_id.as_str())?;
        let mut current = std::ptr::null_mut();
        let status = unsafe {
            ffi::notmuch_database_find_message(self.ptr.as_ptr(), message_id.as_ptr(), &mut current)
        };
        check(status, self.status_string())?;
        if current.is_null() {
            return Err(Error::MessageNotFound(message.message_id.clone()));
        }
        let filenames =
            unsafe { collect_filename_paths(ffi::notmuch_message_get_filenames(current)) };
        unsafe { ffi::notmuch_message_destroy(current) };
        open_message_candidates(&message.message_id, filenames)
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
        validate_mutation(mutation)?;
        let mut seen_threads = BTreeSet::new();
        let unique_thread_ids = thread_ids
            .iter()
            .filter(|thread_id| seen_threads.insert((*thread_id).clone()))
            .cloned()
            .collect::<Vec<_>>();
        let mut missing_thread_ids = Vec::new();
        let mut message_threads = BTreeMap::new();
        let mut message_ids = Vec::new();
        for thread_id in &unique_thread_ids {
            let messages = self.thread_messages_exact(thread_id)?;
            if messages.is_empty() {
                missing_thread_ids.push(thread_id.clone());
            }
            for message in messages {
                if message_threads
                    .insert(message.message_id.clone(), thread_id.clone())
                    .is_none()
                {
                    message_ids.push(message.message_id);
                }
            }
        }
        let matched_threads = unique_thread_ids.len() - missing_thread_ids.len();
        let prepared = prepare_uniform_mutations(message_ids, mutation)?;
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

    fn thread_messages_exact(&self, thread_id: &str) -> Result<Vec<MessageSummary>> {
        self.thread_messages_bounded(thread_id, usize::MAX)
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

fn message_summary(message: *mut ffi::notmuch_message_t) -> MessageSummary {
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
}
