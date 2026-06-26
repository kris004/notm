# notm

`notm` is a native GTK4 desktop mail client for existing Notmuch mail
stores. It gives a three-pane, keyboard-friendly desktop interface for people
who already rely on Notmuch search and tags, while keeping mail storage and
sync under your control.

The app links to `libnotmuch` directly. Normal search, reading, tagging,
fixture indexing, and configured sent/draft indexing do not shell out to the
`notmuch` command-line client.

License: GPL-3.0-or-later. This is intentional because `notm` links to
GPL-family `libnotmuch`.

## What it feels like

- Sidebar saved searches for Inbox, Unread, Flagged, Sent, Drafts, Trash, All,
  plus custom saved searches and tag-derived searches.
- Paged thread list with unread/flagged/attachment/encrypted/signed indicators,
  body previews, keyboard navigation, multi-selection, and load-more behavior
  for large mailboxes.
- Message pane with safe text rendering by default, optional sanitized visual
  HTML, full headers, raw source, MIME tree details, quote collapse, attachment
  list, and copy/open/save attachment actions.
- Tag-first message actions: archive, trash, spam, read/unread, flagged/unflagged,
  custom tag edits, and undo for recent tag changes.
- Compose, reply, reply-all, inline forward, forward-as-attachment, address
  suggestions from Notmuch headers, local draft recovery, explicit draft save,
  and configurable external sending.
- Command palette, searchable shortcuts help, settings dialog, and optional
  manual sync button when a receive/index command is explicitly configured.

## Safety defaults

`notm` is conservative by default:

- no startup sync;
- no automatic `notmuch new`;
- no receive/sync commands unless configured and manually invoked;
- remote images disabled by default;
- JavaScript and in-app navigation disabled in the visual HTML view;
- trash, spam, and archive are tag operations, not file deletion;
- real sending uses a configured external command, such as `sendmail`, `msmtp`,
  or `gmi`;
- sent-mail indexing is disabled unless explicitly configured;
- saved local drafts can be deleted with the explicit `Delete local draft` action.

See [SECURITY.md](SECURITY.md) for the security and privacy model.

## Install

Build dependencies include a Rust toolchain, `libnotmuch` headers/library, GTK4,
GtkSourceView 5, WebKitGTK 6, `pkg-config`, and a C toolchain usable by
`bindgen`. Package names vary by distribution.

Install for the current user:

```sh
make install-user
```

That installs:

- `~/.local/bin/notm`
- `~/.local/share/applications/notm.desktop`
- man pages under `~/.local/share/man/`

For a system install, set `PREFIX`/`DESTDIR` as usual:

```sh
make PREFIX=/usr DESTDIR=/tmp/pkgroot install
```

## Run

```sh
notm launch
```

For development without installing first:

```sh
cargo run -p notm-app -- launch
```

## Configure

`notm` reads `~/.config/notm/config.toml` by default. If that file is absent, it
tries to discover the normal Notmuch config from
`$XDG_CONFIG_HOME/notmuch/default/config` or `~/.notmuch-config`.

Minimal example:

```toml
[notmuch]
database_path = "/home/alice/Mail"
default_query = "tag:inbox and not tag:trash and not tag:spam"
excluded_tags = ["trash", "spam"]

[identity]
name = "Alice Example"
primary_email = "alice@example.com"

[send]
command = "/usr/sbin/sendmail"
args = ["-t", "-oi"]
mode = "stdin_rfc5322"
```

Useful optional sections:

- `[ui]`: page size, pane visibility, thread-list fields, HTML/image preferences,
  custom saved searches, hidden tag searches.
- `[drafts]`: local Maildir draft location and tags.
- `[send]`: external send command, timeout, sent-mail persistence, and sent
  indexing.
- `[sync]`: disabled by default; opt into a manual receive/index command only if
  you want `notm` to expose a Sync button.

Run `notm print-config` to see the effective configuration after default
discovery. See `notm-config(5)` and [docs/send-transport.md](docs/send-transport.md)
for details.

## Keyboard basics

Press `?` in the app for searchable help. Common keys:

- `/` search, `:` command palette, `Ctrl+K` command palette.
- `h`/`l` move between panes; `Ctrl+1`, `Ctrl+2`, `Ctrl+3` hide/show panes.
- `j`/`k`, `gg`, `G`, `Ctrl+d`, `Ctrl+u`, `Ctrl+f` navigate threads and pages.
- `g i/u/f/s/d/t/a` open built-in saved searches.
- `a` archive, `u` unread/read, `f` flagged, `t` trash, `s` spam, `z z` undo.
- `r r`, `r a`, `r f`, `r A` reply, reply-all, forward, forward attached.
- `V t/v/h/r` switch message text, visual HTML, headers, and raw source.
- `c` compose, `Ctrl+Enter` send, `S` save draft, `D` delete the opened local
  draft.

## Documentation

- `man notm` - command overview and subcommands.
- `man notm-config` - configuration file reference.
- `man notm-test-harness` - developer test harness reference.
- [docs/architecture.md](docs/architecture.md) - crate layout and runtime model.
- [docs/testing.md](docs/testing.md) - test and smoke commands.
- [docs/automation/README.md](docs/automation/README.md) - developer test
  harness for agent-assisted validation.

## Current limitations

`notm` is still a young direct gtk4-rs application, not a polished libadwaita
mail suite. Row rendering is paged and cached but not fully virtualized. Recipient
completion is list/Tab based rather than fully polished chips. Visual HTML is
sanitized and useful, but intentionally restrictive: scripts, in-app navigation,
file access, and remote images are blocked unless the user explicitly allows
images for a message or sender.
