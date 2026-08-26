mod attachment_io;
pub mod automation;
mod cache;
mod composer_preparation;
mod draft_autosave;
mod draft_io;
mod draft_recovery;
mod html_view_lifecycle;
pub mod main_window;
pub mod messages;
pub mod model;
pub mod screenshot;
pub mod theme;
mod thread_loader;
pub mod widgets;

pub use main_window::{LaunchOptions, SavedSearch, launch};
