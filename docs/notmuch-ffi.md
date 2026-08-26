# Notmuch FFI

`notm-notmuch` links to `libnotmuch` and generates Rust bindings for the
installed `notmuch.h` at build time. The build script tries `pkg-config notmuch`
first. If a platform lacks a Notmuch `.pc` file but has the standard header and
library in the compiler and linker's default search paths, it falls back to
`-lnotmuch`. Nonstandard installations should expose their `.pc` file through
`PKG_CONFIG_PATH`. The generated bindings also select optional iterator-status
handling by API presence. With Notmuch 0.40 or newer, iteration distinguishes
normal exhaustion from a runtime invalidation or allocation failure. Older
supported headers, including 0.38, retain their validity-only iterator behavior
instead of failing to compile.

Safe wrappers copy C strings before libnotmuch owners are destroyed and wrap
database, query, message, tag, filename, config, revision, indexing, and tag
mutation lifetimes with Rust RAII types.

Tag batches preserve partial-result information. In particular,
`notmuch_message_tags_to_maildir_flags` can return success after an individual
filesystem rename fails, so the wrapper verifies every expected Maildir path
and reports the filenames that remain usable, every previous-to-current path
mapping, and any per-file mismatch. A sync-only retry still repairs filenames
when the requested tags already match the database.
Writable callers must use the consuming `Database::close` API to surface the
final durable-commit status; `Drop` is only a best-effort fallback.
Callers may use known previous/current mappings from an explicit partial
Maildir result, but must treat an unresolved filename or close/commit failure
as unsafe for any retained path-bearing cache.
