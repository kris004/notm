use std::{ffi::CString, os::raw::c_char, path::Path, ptr::NonNull};

use serde::{Deserialize, Serialize};

use crate::{
    Error, Result, ThreadSummary,
    error::{check, check_index},
    ffi,
    message::{MessageSummary, TagMutation, TagOperationReport, ThreadRangeTagReport},
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
        let iter_status = unsafe { ffi::notmuch_threads_status(threads) };
        if iter_status != ffi::notmuch_status_t::NOTMUCH_STATUS_SUCCESS
            && iter_status != ffi::notmuch_status_t::NOTMUCH_STATUS_ITERATOR_EXHAUSTED
        {
            check(iter_status, self.status_string())?;
        }
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

    pub fn thread_messages(&self, thread_id: &str) -> Result<Vec<MessageSummary>> {
        let query = format!("thread:{thread_id}");
        let options = QueryOptions {
            sort: SortOrder::OldestFirst,
            limit: 1000,
            offset: 0,
            excluded_tags: Vec::new(),
        };
        self.search_messages(&query, &options)
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
        let mut changed = 0usize;
        let result: Result<()> = (|| {
            while unsafe { ffi::notmuch_messages_valid(messages) } != 0 {
                let message = unsafe { ffi::notmuch_messages_get(messages) };
                if !message.is_null() {
                    mutate_message(message, mutation, &self.status_string())?;
                    changed += 1;
                    unsafe { ffi::notmuch_message_destroy(message) };
                }
                unsafe { ffi::notmuch_messages_move_to_next(messages) };
            }
            let iter_status = unsafe { ffi::notmuch_messages_status(messages) };
            if iter_status != ffi::notmuch_status_t::NOTMUCH_STATUS_SUCCESS
                && iter_status != ffi::notmuch_status_t::NOTMUCH_STATUS_ITERATOR_EXHAUSTED
            {
                check(iter_status, self.status_string())?;
            }
            Ok(())
        })();
        let end = unsafe { ffi::notmuch_database_end_atomic(self.ptr.as_ptr()) };
        check(end, self.status_string())?;
        result?;
        Ok(TagOperationReport {
            query: query.to_string(),
            changed_messages: changed,
            added: mutation.add.clone(),
            removed: mutation.remove.clone(),
        })
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
        let mut changed_messages = 0usize;
        let result: Result<()> = (|| {
            while unsafe { ffi::notmuch_threads_valid(threads) } != 0 {
                let thread = unsafe { ffi::notmuch_threads_get(threads) };
                if !thread.is_null() {
                    if (start..=end).contains(&index) {
                        let thread_changed = mutate_thread_messages(
                            thread,
                            mutation,
                            &self.status_string(),
                            &mut changed_messages,
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
                let iter_status = unsafe { ffi::notmuch_threads_status(threads) };
                if iter_status != ffi::notmuch_status_t::NOTMUCH_STATUS_SUCCESS
                    && iter_status != ffi::notmuch_status_t::NOTMUCH_STATUS_ITERATOR_EXHAUSTED
                {
                    check(iter_status, self.status_string())?;
                }
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
            changed_messages,
            revision_before: revision_before.revision,
            revision_after: revision_after.revision,
            revision_uuid: revision_after.uuid,
            added: mutation.add.clone(),
            removed: mutation.remove.clone(),
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
        let iter_status = unsafe { ffi::notmuch_messages_status(messages) };
        if iter_status != ffi::notmuch_status_t::NOTMUCH_STATUS_SUCCESS
            && iter_status != ffi::notmuch_status_t::NOTMUCH_STATUS_ITERATOR_EXHAUSTED
        {
            check(iter_status, self.status_string())?;
        }
        Ok(out)
    }
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
    changed_messages: &mut usize,
) -> Result<bool> {
    let messages = unsafe { ffi::notmuch_thread_get_messages(thread) };
    let mut changed = false;
    while unsafe { ffi::notmuch_messages_valid(messages) } != 0 {
        let message = unsafe { ffi::notmuch_messages_get(messages) };
        if !message.is_null() {
            mutate_message(message, mutation, detail)?;
            *changed_messages += 1;
            changed = true;
            unsafe { ffi::notmuch_message_destroy(message) };
        }
        unsafe { ffi::notmuch_messages_move_to_next(messages) };
    }
    let iter_status = unsafe { ffi::notmuch_messages_status(messages) };
    if iter_status != ffi::notmuch_status_t::NOTMUCH_STATUS_SUCCESS
        && iter_status != ffi::notmuch_status_t::NOTMUCH_STATUS_ITERATOR_EXHAUSTED
    {
        check(iter_status, detail.to_string())?;
    }
    Ok(changed)
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

fn mutate_message(
    message: *mut ffi::notmuch_message_t,
    mutation: &TagMutation,
    detail: &str,
) -> Result<()> {
    check(
        unsafe { ffi::notmuch_message_freeze(message) },
        detail.to_string(),
    )?;
    for tag in &mutation.remove {
        let tag = CString::new(tag.as_str())?;
        if let Err(err) = check(
            unsafe { ffi::notmuch_message_remove_tag(message, tag.as_ptr()) },
            detail.to_string(),
        ) {
            let _ = unsafe { ffi::notmuch_message_thaw(message) };
            return Err(err);
        }
    }
    for tag in &mutation.add {
        let tag = CString::new(tag.as_str())?;
        if let Err(err) = check(
            unsafe { ffi::notmuch_message_add_tag(message, tag.as_ptr()) },
            detail.to_string(),
        ) {
            let _ = unsafe { ffi::notmuch_message_thaw(message) };
            return Err(err);
        }
    }
    if mutation.sync_maildir_flags
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
    )
}
