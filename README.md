# notm

`notm` is a native Rust GTK4 desktop client for Notmuch mail stores. It links to `libnotmuch` through generated Rust FFI and does **not** shell out to the `notmuch` CLI for search, view, tag, fixture, indexing, or normal test behavior.

License: GPL-3.0-or-later. This is GPL-compatible and intentionally selected because `notm` links to GPL-family `libnotmuch`.

## Run

```sh
cargo run -p notm-app -- launch
```

With automation enabled for local testing:

```sh
cargo run -p notm-app -- launch --automation --automation-socket /tmp/notm.sock --automation-token dev-token
```

Against disposable fixture data:

```sh
cargo run -p notm-app -- launch --fixture --automation --automation-socket /tmp/notm.sock --automation-token dev-token
```


## Install desktop launcher

```sh
CARGO_HOME=$PWD/.cargo-home cargo build -p notm-app --release
install -Dm755 target/release/notm ~/.local/bin/notm
install -Dm644 packaging/notm.desktop ~/.local/share/applications/notm.desktop
update-desktop-database ~/.local/share/applications
```

The installed desktop entry launches `Exec=notm launch`; install the binary somewhere on your desktop session's `PATH` or adjust the desktop entry locally.

## Runtime/build dependencies

In addition to Rust and libnotmuch, visual HTML rendering requires WebKitGTK 6 development files (`pkg-config webkitgtk-6.0`). The app still has the safe text fallback for messages without HTML or if the user stays on the text view.

## Test

```sh
CARGO_HOME=$PWD/.cargo-home cargo fmt --all -- --check
CARGO_HOME=$PWD/.cargo-home cargo clippy --workspace --all-targets -- -D warnings
CARGO_HOME=$PWD/.cargo-home cargo test --workspace
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- fixture-smoke
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- probe-send
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- live-readonly-smoke
```

`live-self-send` is intentionally separate. It sends one unique self-test message per invocation and does not force sync:

```sh
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- live-self-send
```

## Safety defaults

- No startup sync.
- No automatic `notmuch new`.
- No receive/sync commands unless explicitly configured and manually invoked with the manual sync action.
- Automation disabled by default and token-gated when enabled.
- Remote images disabled by default; Visual HTML uses one image-policy button that starts as `Load images once` and changes to `Trust sender images` after one-shot loading. Trusted senders are stored in `[ui].trusted_image_senders`.
- Sending uses an explicit external transport abstraction and fake transport contract tests.
- Trash/spam/archive are tag operations only; ordinary message files are not deleted.
- Saved local drafts can be explicitly deleted with `Delete local draft`; this removes the local draft file and its notmuch index entry, not a Gmail server message.
- Sent Maildir saving/indexing is disabled unless explicitly configured.
- Explicit draft saves write/index a normal local Maildir message tagged `draft` by default; compose autosave remains a local crash-recovery file.

## Current status

This repository builds and runs a native GTK4 Notmuch client. Implemented daily-driver flows include:

- global Notmuch search and saved searches,
- tag-derived searches, including grouped `Parent/Child` tag menus and hidden-tag persistence,
- thread rows with unread/flagged/attachment/encrypted/signed indicators and body previews,
- thread/message view with safe text and visual sanitized WebKitGTK HTML toggle, one-shot/trusted-sender remote image controls, HTML-to-text fallback, full headers, raw source, filenames, tags, MIME tree, selectable attachment list, and quote collapse,
- tag operations and undo through libnotmuch,
- compose, reply, reply-all, inline forward, and forward-as-`message/rfc822` attachment,
- external send transport with stdin/file/template/auto modes and optional configured-helper auto behavior,
- fake send contract tests,
- optional sent Maildir save/indexing only when configured,
- local JSON compose crash recovery, local Maildir draft save/indexing under `tag:draft`, draft reopening in the composer, and explicit local draft deletion,
- address suggestions from Notmuch headers,
- keyboard shortcuts, shortcuts overlay, command palette, settings, debug panel,
- local Unix-socket automation, paged result loading with Load more and scroll-bottom auto-load, manual sync action disabled by default, and screenshot capture,
- fixture Notmuch database creation/indexing through libnotmuch,
- explicitly gated live read-only smoke for a configured Notmuch database,
- explicitly gated live send validation for configured send transports.

Known limitations: visual styling is direct gtk4-rs rather than libadwaita-polished; visual HTML rendering uses sanitized WebKitGTK with JavaScript blocked, links opened externally, and remote images disabled by default unless explicitly loaded/trusted; recipient chips are list/Tab suggestions rather than fully polished chips; large-mailbox performance has paging/debounce/cache and scroll-bottom loading but not virtualized rows.
