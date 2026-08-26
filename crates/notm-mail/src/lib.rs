pub mod address;
pub mod attachments;
pub mod compose;
pub mod external_command;
pub mod forward;
pub mod html_sanitize;
pub mod mailto;
pub mod message_io;
pub mod mime;
pub mod reply;
pub mod rfc5322;
pub mod send;
pub mod send_timeout;
pub mod transport;

pub use compose::{AttachmentInput, ComposedMessage, Identity};
pub use external_command::{EXTERNAL_COMMAND_OUTPUT_LIMIT, run_external_command};
pub use mailto::{MailtoRequest, parse_mailto_uri};
pub use mime::{Attachment, ParsedMessage};
pub use reply::{ReplyKind, build_reply};
pub use send::{ProbeReport, SendReport, TransportDescription};
pub use send_timeout::{
    MAX_SEND_TIMEOUT_SECONDS, parse_send_timeout_seconds, send_timeout_duration,
    validate_send_timeout_seconds,
};
pub use transport::{ExternalCommandTransport, FakeSendTransport, SendTransport, TransportMode};
