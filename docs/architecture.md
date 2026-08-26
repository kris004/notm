# Architecture

`notm` is a native GTK4 Rust application. Search, thread, message, tag, fixture indexing, and optional sent/draft indexing use `libnotmuch` through generated FFI. Message bodies are read from filenames returned by libnotmuch and parsed in Rust.

## Crates

- `notm-notmuch`: generated FFI plus safe RAII wrappers over databases, queries, thread/message/tag/filename iterators, config, revision, indexing, and tag mutation.
- `notm-mail`: RFC5322/MIME parsing, sanitized HTML-to-text fallback, attachment extraction, composition, reply/reply-all, inline forward, forward-as-attachment generation, and send transports.
- `notm-ui`: direct gtk4-rs desktop UI, WebKitGTK visual HTML view, shortcuts, command palette, debug panel, developer test harness, screenshot support, optional sent/draft persistence wiring.
- `notm-app`: CLI/config/logging/paths and app wiring.
- `notm-test-support`: fixture Maildir/database creation, fake send helpers, UI driver helpers, screenshot helpers.

## Runtime model

The UI opens Notmuch read-only for search/view and read-write only for explicit
tag mutation or explicitly configured indexing of sent/draft files. Search,
view, tag, and sent/draft indexing operations use `libnotmuch` instead of the
`notmuch` CLI. Sync is disabled by default. On a non-fixture launch, a
configured sync command runs at startup only when `[sync].enabled`, that
command's `*_enabled` flag, a nonblank `*_command`, and its `*_on_startup` flag
are all set. Eligible receive and database-update commands run in that order
through the bounded, timeout-aware external-command runner, with the selected
Notmuch context in their environment. After the selected commands succeed, the
current search is refreshed. Fixture mode never executes configured external
sync commands. Sending uses the same hardened runner for a configured external
transport or the fake capture transport used in tests.

Search input is debounced and submitted to one persistent background worker.
A newer request cancels active work, coalesces queued requests to the latest
generation, and checks cancellation between the delay, Notmuch query, and
bounded thread-detail file-read phases. Search results are paged
(`ui.page_size`, from 1 through 1,000), expose a Load more action, and auto-load
the next page when the thread list is scrolled to the bottom. GTK removes,
appends, or refreshes at most 48 thread-model rows per main-loop iteration and
cancels stale model updates by generation.

Search pages use a 64-entry least-recently-used cache keyed by database path,
Notmuch UUID/revision, query, page offset/limit, and the complete excluded-tag
vector. Thread UI details use a separate 4,096-entry least-recently-used cache
keyed by database path, UUID/revision, and thread ID. Hits refresh recency, so
new database generations naturally evict stale entries instead of accumulating
without a bound. Cache locks cover only lookup or insertion; Notmuch queries,
bounded filesystem reads, and MIME parsing run outside them. Thread previews
are cached before the configured display-line limit is applied, so that
presentation setting is not part of either key.

## Implementation notes

The UI uses gtk4-rs directly, keeping widget behavior and the dependency
surface explicit. The message pane has a safe text renderer plus a visual
WebKitGTK HTML view. Remote content is blocked by default. Blocked rendering
sanitizes resource-bearing markup, disables automatic image loading, and uses a
restrictive document and WebView content security policy; the WebView also uses
an ephemeral network session. These layers cover direct images, redirects,
CSS URLs, `srcset`, and nested resource markup while JavaScript, file and
universal-file access, and in-app navigation remain disabled.

**Load remote images once** reloads only the selected message view with sanitized
HTTP(S) image sources enabled. The permission is not persisted or shared and
is reset by message/view navigation or application restart. Raw `From:` values
are unauthenticated message data, so they cannot establish durable permission.
`[ui].remote_images = true` remains an explicit global privacy override for all
messages. View selections are persisted by Message-ID, with optional
normalized-sender defaults and message-over-sender precedence; standalone
message windows use the same resolver and stores.
