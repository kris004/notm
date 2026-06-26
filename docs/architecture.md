# Architecture

`notm` is a native GTK4 Rust application. Search, thread, message, tag, fixture indexing, and optional sent/draft indexing use `libnotmuch` through generated FFI. Message bodies are read from filenames returned by libnotmuch and parsed in Rust.

## Crates

- `notm-notmuch`: generated FFI plus safe RAII wrappers over databases, queries, thread/message/tag/filename iterators, config, revision, indexing, and tag mutation.
- `notm-mail`: RFC5322/MIME parsing, sanitized HTML-to-text fallback, attachment extraction, composition, reply/reply-all, inline forward, forward-as-attachment generation, and send transports.
- `notm-ui`: direct gtk4-rs desktop UI, WebKitGTK visual HTML view, shortcuts, command palette, debug panel, developer test harness, screenshot support, optional sent/draft persistence wiring.
- `notm-app`: CLI/config/logging/paths and app wiring.
- `notm-test-support`: fixture Maildir/database creation, fake send helpers, UI driver helpers, screenshot helpers.

## Runtime model

The UI opens Notmuch read-only for search/view and read-write only for explicit tag mutation or explicitly configured indexing of sent/draft files. It never runs startup sync and never shells out to the `notmuch` CLI. Sending is delegated to a configured external transport or the fake capture transport used in tests.

Search input is debounced and run through a background worker with stale-generation discard. Search results are paged (`ui.page_size`), expose a Load more action, auto-load the next page when the thread list is scrolled to the bottom, and are cached by database path, Notmuch UUID/revision, page size, excluded tags, query, and page offset. Thread UI details are cached by database path, revision, and thread id.

## Implementation notes

The current UI is direct gtk4-rs rather than Relm4/libadwaita because that was the fastest reliable path in this workstation environment. The message pane has a safe text renderer plus a visual WebKitGTK HTML view. HTML is sanitized before loading into WebKit; JavaScript, file/universal file access, in-app navigation, and remote image loading are disabled by default. Users can allow images for the current HTML view only or persist a sender-specific allow-list entry in `[ui].trusted_image_senders`.
