use std::{ffi::NulError, io};

use thiserror::Error;

use crate::ffi;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("libnotmuch returned {name} ({code}){detail}")]
    Status {
        name: &'static str,
        code: i32,
        detail: String,
    },
    #[error("libnotmuch returned a null pointer for {0}")]
    Null(&'static str),
    #[error("string contains interior NUL: {0}")]
    Nul(#[from] NulError),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid notmuch tag `{0}`")]
    InvalidTag(String),
}

pub fn status_name(status: ffi::notmuch_status_t) -> &'static str {
    use ffi::notmuch_status_t::*;
    match status {
        NOTMUCH_STATUS_SUCCESS => "NOTMUCH_STATUS_SUCCESS",
        NOTMUCH_STATUS_OUT_OF_MEMORY => "NOTMUCH_STATUS_OUT_OF_MEMORY",
        NOTMUCH_STATUS_READ_ONLY_DATABASE => "NOTMUCH_STATUS_READ_ONLY_DATABASE",
        NOTMUCH_STATUS_XAPIAN_EXCEPTION => "NOTMUCH_STATUS_XAPIAN_EXCEPTION",
        NOTMUCH_STATUS_FILE_ERROR => "NOTMUCH_STATUS_FILE_ERROR",
        NOTMUCH_STATUS_FILE_NOT_EMAIL => "NOTMUCH_STATUS_FILE_NOT_EMAIL",
        NOTMUCH_STATUS_DUPLICATE_MESSAGE_ID => "NOTMUCH_STATUS_DUPLICATE_MESSAGE_ID",
        NOTMUCH_STATUS_NULL_POINTER => "NOTMUCH_STATUS_NULL_POINTER",
        NOTMUCH_STATUS_TAG_TOO_LONG => "NOTMUCH_STATUS_TAG_TOO_LONG",
        NOTMUCH_STATUS_UNBALANCED_FREEZE_THAW => "NOTMUCH_STATUS_UNBALANCED_FREEZE_THAW",
        NOTMUCH_STATUS_UNBALANCED_ATOMIC => "NOTMUCH_STATUS_UNBALANCED_ATOMIC",
        NOTMUCH_STATUS_UNSUPPORTED_OPERATION => "NOTMUCH_STATUS_UNSUPPORTED_OPERATION",
        NOTMUCH_STATUS_UPGRADE_REQUIRED => "NOTMUCH_STATUS_UPGRADE_REQUIRED",
        NOTMUCH_STATUS_PATH_ERROR => "NOTMUCH_STATUS_PATH_ERROR",
        NOTMUCH_STATUS_IGNORED => "NOTMUCH_STATUS_IGNORED",
        NOTMUCH_STATUS_ILLEGAL_ARGUMENT => "NOTMUCH_STATUS_ILLEGAL_ARGUMENT",
        NOTMUCH_STATUS_MALFORMED_CRYPTO_PROTOCOL => "NOTMUCH_STATUS_MALFORMED_CRYPTO_PROTOCOL",
        NOTMUCH_STATUS_FAILED_CRYPTO_CONTEXT_CREATION => {
            "NOTMUCH_STATUS_FAILED_CRYPTO_CONTEXT_CREATION"
        }
        NOTMUCH_STATUS_UNKNOWN_CRYPTO_PROTOCOL => "NOTMUCH_STATUS_UNKNOWN_CRYPTO_PROTOCOL",
        NOTMUCH_STATUS_NO_CONFIG => "NOTMUCH_STATUS_NO_CONFIG",
        NOTMUCH_STATUS_NO_DATABASE => "NOTMUCH_STATUS_NO_DATABASE",
        NOTMUCH_STATUS_DATABASE_EXISTS => "NOTMUCH_STATUS_DATABASE_EXISTS",
        NOTMUCH_STATUS_BAD_QUERY_SYNTAX => "NOTMUCH_STATUS_BAD_QUERY_SYNTAX",
        NOTMUCH_STATUS_NO_MAIL_ROOT => "NOTMUCH_STATUS_NO_MAIL_ROOT",
        NOTMUCH_STATUS_CLOSED_DATABASE => "NOTMUCH_STATUS_CLOSED_DATABASE",
        #[cfg(notmuch_has_iterator_status)]
        NOTMUCH_STATUS_ITERATOR_EXHAUSTED => "NOTMUCH_STATUS_ITERATOR_EXHAUSTED",
        #[cfg(notmuch_has_iterator_status)]
        NOTMUCH_STATUS_OPERATION_INVALIDATED => "NOTMUCH_STATUS_OPERATION_INVALIDATED",
        NOTMUCH_STATUS_LAST_STATUS => "NOTMUCH_STATUS_LAST_STATUS",
    }
}

pub fn check(status: ffi::notmuch_status_t, detail: impl Into<String>) -> Result<()> {
    if status == ffi::notmuch_status_t::NOTMUCH_STATUS_SUCCESS {
        Ok(())
    } else {
        Err(Error::Status {
            name: status_name(status),
            code: status as i32,
            detail: format_detail(detail.into()),
        })
    }
}

pub fn check_index(status: ffi::notmuch_status_t, detail: impl Into<String>) -> Result<()> {
    if status == ffi::notmuch_status_t::NOTMUCH_STATUS_SUCCESS
        || status == ffi::notmuch_status_t::NOTMUCH_STATUS_DUPLICATE_MESSAGE_ID
    {
        Ok(())
    } else {
        check(status, detail)
    }
}

fn format_detail(detail: String) -> String {
    if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    }
}
