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

The installed desktop entry launches `Exec=/home/user/.local/bin/notm launch`.

## Runtime/build dependencies

In addition to Rust and libnotmuch, visual HTML rendering requires WebKitGTK 6 development files (`pkg-config webkitgtk-6.0`). The app still has the safe text fallback for messages without HTML or if the user stays on the rendered text view.

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
- Remote images disabled by default; Visual HTML has explicit `Load images once` and `Trust sender images` controls. Trusted senders are stored in `[ui].trusted_image_senders`.
- Sending uses an explicit external transport abstraction and fake transport contract tests.
- Trash/spam/archive are tag operations only; message files are not deleted.
- Sent Maildir saving/indexing and draft Maildir saving/indexing are disabled unless explicitly configured.

## Current status

This repository builds and runs a native GTK4 Notmuch client. Implemented daily-driver flows include:

- global Notmuch search and saved searches,
- thread rows with unread/flagged/attachment/encrypted/signed indicators and body previews,
- thread/message view with rendered safe text, visual sanitized WebKitGTK HTML rendering, one-shot/trusted-sender remote image controls, HTML-to-text fallback, full headers, raw source, filenames, tags, MIME tree, selectable attachment list, and quote collapse,
- tag operations and undo through libnotmuch,
- compose, reply, reply-all, inline forward, and forward-as-`message/rfc822` attachment,
- external send transport with stdin/file/template/auto modes and lieer helper auto `-t`,
- fake send contract tests,
- optional sent Maildir save/indexing only when configured,
- local JSON drafts, multi-draft manager, and optional Maildir draft save/indexing only when configured,
- address suggestions from Notmuch headers,
- keyboard shortcuts, shortcuts overlay, command palette, settings, debug panel,
- local Unix-socket automation, paged result loading with Load more and scroll-bottom auto-load, manual sync action disabled by default, and screenshot capture,
- fixture Notmuch database creation/indexing through libnotmuch,
- live read-only smoke against the real Notmuch database,
- bounded live send validation emails with consistent subjects.

Known limitations: visual styling is direct gtk4-rs rather than libadwaita-polished; visual HTML rendering uses sanitized WebKitGTK with JavaScript/navigation blocked and remote images disabled by default unless explicitly loaded/trusted; recipient chips are list/Tab suggestions rather than fully polished chips; large-mailbox performance has paging/debounce/cache and scroll-bottom loading but not virtualized rows.
