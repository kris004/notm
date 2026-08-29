# Changelog

Notable user-facing changes are recorded here.

## Unreleased

## [0.1.2] - 2026-08-28

- Move searches, message and MIME loading, attachment work, draft recovery,
  autosave, and tag updates off the GTK main thread. Large searches and messages
  now use bounded, generation-aware work so stale results cannot overwrite the
  current view.
- Add debounced, durable draft autosave with atomic replacement, last-good
  preservation, restart recovery, explicit send/close flushing, and safer
  handling of corrupt, oversized, or legacy named drafts.
- Make attachment saves atomic, avoid overwriting an existing destination, and
  keep the application alive until a save finishes after the last window closes.
- Target tag operations by exact message and thread IDs, reject stale database
  snapshots, preserve non-UTF-8 Maildir paths, reconcile renamed files across
  open views, and publish undo state only after durable success.
- Harden MIME parsing and outgoing mail interoperability with bounded legacy
  input handling, standards-compliant header and attachment encoding, validation
  of unsafe headers and embedded messages, and representable send timeouts.
- Tighten Visual HTML privacy: remote images remain blocked by default, one-time
  loading is scoped to the selected view, spoofable sender headers no longer
  grant durable trust, and WebKit uses ephemeral sessions plus restrictive CSPs.
- Add hardware-backed signed, immutable releases with exact-source archives,
  deterministic bundles, checksum and provenance verification, and CodeQL gates.

## [0.1.1] - 2026-08-24

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

[Unreleased]: https://github.com/kris004/notm/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/kris004/notm/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/kris004/notm/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/kris004/notm/releases/tag/v0.1.0
