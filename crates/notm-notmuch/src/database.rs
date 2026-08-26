use std::{
    collections::BTreeSet,
    ffi::{CStr, CString},
    fs::File,
    io::{Read, Seek},
    os::raw::c_char,
    path::{Path, PathBuf},
    ptr::NonNull,
};

#[cfg(unix)]
use std::{ffi::OsString, os::unix::ffi::OsStringExt};

use serde::{Deserialize, Serialize};

use crate::{
    Error, Result, ThreadSummary,
    error::{check, check_index},
    ffi,
    message::{
        AppliedTagChange, MessageSummary, MessageTagMutation, TagMutation, TagOperationReport,
        ThreadRangeTagReport,
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
        Ok(Self { ptr, mode })
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
        })
    }

    pub fn mode(&self) -> DatabaseMode {
        self.mode
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
        let query = format!("thread:{thread_id}");
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
        for tag in mutation.add.iter().chain(mutation.remove.iter()) {
            validate_tag(tag)?;
        }
        let options = QueryOptions {
            sort: SortOrder::Unsorted,
            limit: usize::MAX,
            offset: 0,
            excluded_tags: Vec::new(),
        };
        let q = self.create_query(query, &options)?;
        let mut messages = std::ptr::null_mut();
        let status = unsafe { ffi::notmuch_query_search_messages(q.as_ptr(), &mut messages) };
        check(status, self.status_string())?;
        let begin = unsafe { ffi::notmuch_database_begin_atomic(self.ptr.as_ptr()) };
        check(begin, self.status_string())?;
        let mut changes = Vec::new();
        let result: Result<()> = (|| {
            while unsafe { ffi::notmuch_messages_valid(messages) } != 0 {
                let message = unsafe { ffi::notmuch_messages_get(messages) };
                if !message.is_null() {
                    let change = mutate_message(message, mutation, &self.status_string());
                    unsafe { ffi::notmuch_message_destroy(message) };
                    if let Some(change) = change? {
                        changes.push(change);
                    }
                }
                unsafe { ffi::notmuch_messages_move_to_next(messages) };
            }
            check_messages_iterator(messages, self.status_string())?;
            Ok(())
        })();
        let end = unsafe { ffi::notmuch_database_end_atomic(self.ptr.as_ptr()) };
        check(end, self.status_string())?;
        result?;
        Ok(TagOperationReport {
            query: query.to_string(),
            changed_messages: changes.len(),
            added: mutation.add.clone(),
            removed: mutation.remove.clone(),
            changes,
        })
    }

    pub fn apply_tags_to_messages(
        &self,
        mutations: &[MessageTagMutation],
        sync_maildir_flags: bool,
    ) -> Result<Vec<AppliedTagChange>> {
        let mut prepared = Vec::with_capacity(mutations.len());
        for mutation in mutations {
            for tag in mutation.add.iter().chain(mutation.remove.iter()) {
                validate_tag(tag)?;
            }
            prepared.push((
                CString::new(mutation.message_id.as_str())?,
                TagMutation {
                    add: mutation.add.clone(),
                    remove: mutation.remove.clone(),
                    sync_maildir_flags,
                },
            ));
        }

        let begin = unsafe { ffi::notmuch_database_begin_atomic(self.ptr.as_ptr()) };
        check(begin, self.status_string())?;
        let mut changes = Vec::new();
        let result: Result<()> = (|| {
            for (message_id, mutation) in &prepared {
                let mut message = std::ptr::null_mut();
                let status = unsafe {
                    ffi::notmuch_database_find_message(
                        self.ptr.as_ptr(),
                        message_id.as_ptr(),
                        &mut message,
                    )
                };
                check(status, self.status_string())?;
                if message.is_null() {
                    continue;
                }
                let change = mutate_message(message, mutation, &self.status_string());
                unsafe { ffi::notmuch_message_destroy(message) };
                if let Some(change) = change? {
                    changes.push(change);
                }
            }
            Ok(())
        })();
        let end = unsafe { ffi::notmuch_database_end_atomic(self.ptr.as_ptr()) };
        check(end, self.status_string())?;
        result?;
        Ok(changes)
    }

    pub fn apply_tags_to_thread_range(
        &self,
        query: &str,
        options: &QueryOptions,
        start: usize,
        end: usize,
        mutation: &TagMutation,
    ) -> Result<ThreadRangeTagReport> {
        for tag in mutation.add.iter().chain(mutation.remove.iter()) {
            validate_tag(tag)?;
        }
        let revision_before = self.revision();
        let q = self.create_query(query, options)?;
        let mut threads = std::ptr::null_mut();
        let status = unsafe { ffi::notmuch_query_search_threads(q.as_ptr(), &mut threads) };
        check(status, self.status_string())?;
        let begin = unsafe { ffi::notmuch_database_begin_atomic(self.ptr.as_ptr()) };
        check(begin, self.status_string())?;
        let mut index = 0usize;
        let mut changed_threads = 0usize;
        let mut changes = Vec::new();
        let result: Result<()> = (|| {
            while unsafe { ffi::notmuch_threads_valid(threads) } != 0 {
                let thread = unsafe { ffi::notmuch_threads_get(threads) };
                if !thread.is_null() {
                    if (start..=end).contains(&index) {
                        let thread_changed = mutate_thread_messages(
                            thread,
                            mutation,
                            &self.status_string(),
                            &mut changes,
                        )?;
                        if thread_changed {
                            changed_threads += 1;
                        }
                    }
                    unsafe { ffi::notmuch_thread_destroy(thread) };
                }
                if index >= end {
                    break;
                }
                index = index.saturating_add(1);
                unsafe { ffi::notmuch_threads_move_to_next(threads) };
            }
            if index < end {
                check_threads_iterator(threads, self.status_string())?;
            }
            Ok(())
        })();
        let end_atomic = unsafe { ffi::notmuch_database_end_atomic(self.ptr.as_ptr()) };
        check(end_atomic, self.status_string())?;
        result?;
        let revision_after = self.revision();
        Ok(ThreadRangeTagReport {
            query: query.to_string(),
            start,
            end,
            changed_threads,
            changed_messages: changes.len(),
            revision_before: revision_before.revision,
            revision_after: revision_after.revision,
            revision_uuid: revision_after.uuid,
            added: mutation.add.clone(),
            removed: mutation.remove.clone(),
            changes,
        })
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

fn mutate_thread_messages(
    thread: *mut ffi::notmuch_thread_t,
    mutation: &TagMutation,
    detail: &str,
    changes: &mut Vec<AppliedTagChange>,
) -> Result<bool> {
    let messages = unsafe { ffi::notmuch_thread_get_messages(thread) };
    let mut changed = false;
    while unsafe { ffi::notmuch_messages_valid(messages) } != 0 {
        let message = unsafe { ffi::notmuch_messages_get(messages) };
        if !message.is_null() {
            let change = mutate_message(message, mutation, detail);
            unsafe { ffi::notmuch_message_destroy(message) };
            if let Some(change) = change? {
                changes.push(change);
                changed = true;
            }
        }
        unsafe { ffi::notmuch_messages_move_to_next(messages) };
    }
    check_messages_iterator(messages, detail.to_string())?;
    Ok(changed)
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
    let mut out = Vec::new();
    while unsafe { ffi::notmuch_filenames_valid(files) } != 0 {
        out.push(unsafe { cstr_to_string(ffi::notmuch_filenames_get(files)) });
        unsafe { ffi::notmuch_filenames_move_to_next(files) };
    }
    if !files.is_null() {
        unsafe { ffi::notmuch_filenames_destroy(files) };
    }
    out
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
) -> Result<Option<AppliedTagChange>> {
    let message_id = unsafe { cstr_to_string(ffi::notmuch_message_get_message_id(message)) };
    let current_tags = unsafe { collect_tags(ffi::notmuch_message_get_tags(message)) };
    let Some((effective, change)) = effective_tag_change(&message_id, &current_tags, mutation)
    else {
        return Ok(None);
    };
    check(
        unsafe { ffi::notmuch_message_freeze(message) },
        detail.to_string(),
    )?;
    for tag in &effective.remove {
        let tag = CString::new(tag.as_str())?;
        if let Err(err) = check(
            unsafe { ffi::notmuch_message_remove_tag(message, tag.as_ptr()) },
            detail.to_string(),
        ) {
            let _ = unsafe { ffi::notmuch_message_thaw(message) };
            return Err(err);
        }
    }
    for tag in &effective.add {
        let tag = CString::new(tag.as_str())?;
        if let Err(err) = check(
            unsafe { ffi::notmuch_message_add_tag(message, tag.as_ptr()) },
            detail.to_string(),
        ) {
            let _ = unsafe { ffi::notmuch_message_thaw(message) };
            return Err(err);
        }
    }
    if effective.sync_maildir_flags
        && let Err(err) = check(
            unsafe { ffi::notmuch_message_tags_to_maildir_flags(message) },
            detail.to_string(),
        )
    {
        let _ = unsafe { ffi::notmuch_message_thaw(message) };
        return Err(err);
    }
    check(
        unsafe { ffi::notmuch_message_thaw(message) },
        detail.to_string(),
    )?;
    Ok(Some(change))
}

fn effective_tag_change(
    message_id: &str,
    current_tags: &[String],
    mutation: &TagMutation,
) -> Option<(TagMutation, AppliedTagChange)> {
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
    Some((
        TagMutation {
            add: added.clone(),
            remove: removed.clone(),
            sync_maildir_flags: mutation.sync_maildir_flags,
        },
        AppliedTagChange {
            message_id: message_id.to_string(),
            added,
            removed,
        },
    ))
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
    fn effective_tag_change_records_only_net_per_message_delta() {
        let mutation = TagMutation {
            add: vec!["inbox".to_string(), "project".to_string()],
            remove: vec!["unread".to_string(), "missing".to_string()],
            sync_maildir_flags: true,
        };

        let (effective, change) = effective_tag_change(
            "message@example.test",
            &["inbox".to_string(), "unread".to_string()],
            &mutation,
        )
        .expect("net change");

        assert_eq!(effective.add, ["project"]);
        assert_eq!(effective.remove, ["unread"]);
        assert!(effective.sync_maildir_flags);
        assert_eq!(change.added, ["project"]);
        assert_eq!(change.removed, ["unread"]);
        assert_eq!(change.inverse().add, ["unread"]);
        assert_eq!(change.inverse().remove, ["project"]);
    }

    #[test]
    fn effective_tag_change_respects_remove_then_add_for_overlapping_tags() {
        let mutation = TagMutation {
            add: vec!["inbox".to_string()],
            remove: vec!["inbox".to_string()],
            sync_maildir_flags: false,
        };

        assert!(
            effective_tag_change("present@example.test", &["inbox".to_string()], &mutation,)
                .is_none()
        );
        let (_, change) = effective_tag_change("absent@example.test", &[], &mutation)
            .expect("remove then add leaves an absent tag added");
        assert_eq!(change.added, ["inbox"]);
        assert!(change.removed.is_empty());
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
