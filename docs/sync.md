# Synchronization

`notm` does not fetch mail or update the Notmuch index unless commands are
explicitly configured. Synchronization is a UI action—not a top-level CLI
subcommand—and is disabled by default.

## Example

This configuration runs `mbsync` first and `notmuch new` second when **Sync** is
selected in the sidebar or `:sync` is run from the command palette:

```toml
[sync]
enabled = true
manual_action_label = "Sync"
timeout_seconds = 300

external_receive_enabled = true
external_receive_command = "mbsync --all"
external_receive_on_startup = false

notmuch_database_update_enabled = true
notmuch_database_update_command = "notmuch new"
notmuch_database_update_on_startup = false
```

Use only the database-update block when another service already retrieves mail.
Use only the receive block when that command also updates Notmuch itself.

## Gates and order

A manual command runs only when all three conditions are true:

1. `sync.enabled` is `true`;
2. that command's `*_enabled` key is `true`;
3. its `*_command` value is not blank.

Startup adds a fourth condition: the matching `*_on_startup` key must be
`true`. Fixture launches never execute external sync commands, regardless of
configuration.

When both commands are eligible, receive runs before the database update. A
nonzero exit or timeout stops the sequence, reports the helper's bounded output,
and does not refresh the message list. After every selected command succeeds,
`notm` refreshes the active search. Commands run on a worker so the desktop
remains responsive, and closing the main window waits for an outstanding sync
to finish or time out.

## Command environment

Each value is run as `sh -c COMMAND`. Shell expansion, pipelines, redirects,
and quoting therefore follow the system's POSIX shell. Commands are
non-interactive and inherit `notm`'s environment, `PATH`, and current working
directory. Desktop launchers may not have the same `PATH` or working directory
as an interactive shell, so prefer absolute paths or a small executable wrapper
for complex setups.

`notm` exports its effective database as `NOTMUCH_DATABASE` to both sync
commands. It also exports `NOTMUCH_CONFIG` and `NOTMUCH_PROFILE` when those were
selected explicitly or through the environment; otherwise the child inherits
the same standard Notmuch discovery environment as the UI. This keeps
`notmuch new` and helpers that invoke Notmuch on the same database as the UI.

`timeout_seconds` is a per-command wall-clock limit and defaults to 300 seconds.
On timeout, `notm` terminates the helper process group and reaps the direct
child. Stdout and stderr are drained to prevent a verbose helper from blocking;
capture is bounded and truncated before it is placed in UI state.

## Safe validation

Before using a real fetch command:

1. Run `notm print-config --show-secrets` privately and confirm the enabled
   gates and exact commands.
2. Temporarily configure a harmless wrapper that records its arguments and
   selected `NOTMUCH_*` values into a disposable directory.
3. Launch `notm`, run **Sync**, and confirm receive precedes database update.
4. Test a nonzero exit and a short timeout before enabling either startup key.

Repository contributors can run the focused unit coverage and the real GTK
responsiveness/lifetime smokes described in [Testing](testing.md). Never point a
fixture test at a real fetch helper; fixture mode deliberately rejects sync.
