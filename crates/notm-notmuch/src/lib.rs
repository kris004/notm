#![deny(unsafe_op_in_unsafe_fn)]

pub mod config;
pub mod database;
pub mod error;
pub mod ffi;
pub mod index;
pub mod message;
pub mod query;
pub mod safe;
pub mod tags;
pub mod thread;

pub use config::{ConfigProfile, LoadedIdentity};
pub use database::{
    Database, DatabaseMode, OpenConfig, ResolvedMessageFile, Revision, ThreadMessagePage,
};
pub use error::{Error, Result};
pub use message::{
    AppliedTagChange, MaildirFilenameChange, MaildirFlagSyncFailure, MaildirPathChange,
    MessagePathState, MessageSummary, MessageTagFailure, MessageTagMutation, TagBatchReport,
    TagFailureStage, TagMutation, TagOperationReport, ThreadTagReport,
};
pub use query::{QueryOptions, SortOrder};
pub use thread::ThreadSummary;
