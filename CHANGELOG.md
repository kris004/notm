# Changelog

Notable user-facing changes are recorded here.

## Unreleased

- The installed desktop entry can act as the default `mailto` handler. RFC 6068
  recipient, Cc, Bcc, subject, and plain-text body fields open in an editable
  composer and are routed to an existing notm instance when one is running.

## [0.1.0] - 2026-08-20

- Initial GTK desktop client for searching, reading, tagging, composing, and
  managing mail in a Notmuch database.
- Column and stacked layouts, saved searches, paged thread results, keyboard
  navigation, and configurable thread previews.
- Plain-text and sanitized visual HTML message views, keyboard link hints,
  attachment handling, and standalone message windows.
- Reply, reply-all, forwarding, local draft recovery, saved drafts, and external
  send transport support.
- Optional external receive/database-update commands with explicit manual and
  startup gates, bounded diagnostics, Notmuch-context propagation, and timeout
  cleanup.
- System, light, and dark theme preferences, plus configurable thread-preview
  length. Invalid theme and preview values now fail clearly at startup.
- Standard Notmuch environment/profile discovery and split database/mail-root
  support for default Sent and Drafts locations.
- Owner-private settings, tag-undo history, recovery drafts, attachment caches,
  and created Maildir messages on Unix.
- Byte-identical external submission and optional local Sent copies for each
  composed message.
- Composer `A`, `S`, `x`, and `D` shortcuts now take precedence over global
  mail actions after leaving insert mode, including from focused header fields.
- `Ctrl+e` and `Ctrl+y` scroll the message-list viewport without changing the
  selected message.
- Pending `g` shortcuts, including `g d` for Drafts, now complete when a
  composer header field still has focus in Normal mode.
- Saving an indexed draft refreshes results without dismissing the composer;
  clean saved drafts no longer leave recovery state that prompts during later
  navigation or after a restart.
- Deleting an opened indexed draft invalidates cached search results, removes
  its row immediately, and no longer renders the deleted file as a missing
  message body while results refresh.
- Sender-default view actions now use compact labels and the `V a` shortcut.

[0.1.0]: https://github.com/kris004/notm/releases/tag/v0.1.0
