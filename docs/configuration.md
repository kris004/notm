# Configuration

`notm` uses TOML at
`${XDG_CONFIG_HOME:-$HOME/.config}/notm/config.toml`. The file is optional; use
`notm --config PATH launch` to select another file. An explicitly selected file
must exist.

Unknown sections and keys are errors. Before launching the UI, inspect the
effective configuration with:

```sh
notm print-config
```

Command arguments, command environment values, sync commands, and the developer
test-harness token are redacted. `notm print-config --show-secrets` prints them
only when private local inspection is intentional.

## Minimal setup

If Notmuch already provides `database.path`, `database.mail_root`, and the user
identity, no `notm` configuration is required. A standalone configuration can
instead supply them explicitly:

```toml
[notmuch]
database_path = "/home/alice/.mail"

[identity]
name = "Alice Example"
primary_email = "alice@example.com"

[send]
command = "/usr/sbin/sendmail"
args = ["-t", "-oi"]
mode = "stdin_rfc5322"
```

The `--config` option selects the **notm** TOML file. The
`notmuch.config_path`, `notmuch.database_path`, and `notmuch.profile` keys select
the Notmuch configuration and database. When those keys are absent, libnotmuch
uses the standard `NOTMUCH_CONFIG`, `NOTMUCH_DATABASE`, and `NOTMUCH_PROFILE`
environment variables and Notmuch's XDG/legacy discovery rules.

## Notmuch and identity

```toml
[notmuch]
default_query = "tag:inbox and not tag:trash and not tag:spam"
excluded_tags = ["trash", "spam"]
open_readwrite_only_for_mutations = true
sync_maildir_flags_after_tag_change = true

[identity]
name = "Alice Example"
primary_email = "alice@example.com"
other_email = ["alice@work.example"]
```

| Key | Default and purpose |
| --- | --- |
| `notmuch.database_path` | Effective Notmuch database path. Standard Notmuch discovery is used when omitted. |
| `notmuch.config_path` | Explicit Notmuch configuration file. |
| `notmuch.profile` | Explicit Notmuch profile. |
| `notmuch.default_query` | Initial query; defaults to `tag:inbox and not tag:trash and not tag:spam`. |
| `notmuch.excluded_tags` | Tags omitted from normal searches; defaults to `trash` and `spam`. |
| `notmuch.open_readwrite_only_for_mutations` | Must remain `true`; searches and views are opened read-only. |
| `notmuch.sync_maildir_flags_after_tag_change` | Synchronize Maildir flags after tag changes; defaults to `true`. |
| `identity.name` | Display name used for composed mail. |
| `identity.primary_email` | Primary sender address. |
| `identity.other_email` | Additional addresses used when matching the user's identity. |

Explicit identity values override values discovered from Notmuch.

## Interface

```toml
[ui]
theme = "system"
layout = "auto"
page_size = 100
thread_preview_lines = 2
show_thread_numbers = true
show_thread_dates = true
show_thread_tags = true
show_thread_preview = true
show_keybind_hints = true
start_maximized = false

remote_images = false
html_mode = "sanitize_then_render_text_fallback"

custom_saved_searches = [
  { name = "Unread", query = "tag:unread and not tag:trash" },
]
hidden_tag_searches = []
```

`theme` accepts `system`, `light`, or `dark`. `layout` accepts `auto`,
`three_pane` (or `columns`), or `stacked`. `page_size` accepts 1 through 1,000,
and `thread_preview_lines` accepts 1 through 20. `html_mode` accepts
`sanitize_then_render_text_fallback` or `visual_html_preferred`.

`remote_images` defaults to `false`, which blocks remote content in Visual HTML.
**Load remote images once** permits sanitized remote images only for the current
message view; navigating away or restarting restores blocking. Setting
`remote_images = true` is an explicit global privacy override that permits
remote images in every message.

Raw `From:` headers are not authenticated by Notmuch and cannot grant durable
remote-image permission. The retired `trusted_image_senders` key is accepted
when reading an older configuration but ignored. A later successful Settings
write removes it rather than converting it into a broader permission.

The additional visibility keys `show_sidebar`, `show_message_list`,
`show_message_view`, and `show_debug_panel` control startup state. They default
to `true`, `true`, `true`, and `false`, respectively.

`message_view_preferences` and `sender_view_preferences` are application-managed
tables. Their values are `text`, `visual_html`, `full_headers`, or `raw_source`.
A Message-ID-specific preference takes precedence over a sender preference and
the global HTML mode.

## Sending

```toml
[send]
enabled = true
transport = "external"
command = "msmtp"
args = ["-t"]
mode = "stdin_rfc5322"
timeout_seconds = 120

# Optional command settings:
# working_dir = "/path/to/helper-state"
# env = { ACCOUNT = "personal" }

# Optional local Sent copy:
save_sent = false
sent_tags = ["sent"]
index_sent_after_send = false
# sent_maildir = "/home/alice/.mail/account/Sent"
```

`transport = "external"` is currently required. `mode` accepts `auto`,
`stdin_rfc5322`, `file_arg`, or `command_template`; `auto` currently behaves as
`stdin_rfc5322`. For `command_template`, at least one argument must contain
`{file}`. `timeout_seconds` must be a whole number from 1 through
946080000 (30 fixed 365-day years). This cap keeps the timeout representable as
a monotonic timer deadline. Config loading and the Settings dialog reject zero,
negative, nonnumeric, or larger values instead of substituting a default or
wrapping the stored value; a rejected Settings save leaves the previous config
unchanged. See [Sending mail](send-transport.md) for mode behavior, Bcc handling,
timeouts, and failure reporting.

When `save_sent` is enabled and `sent_maildir` is omitted, the default is
`Sent` under Notmuch's effective `database.mail_root` (falling back to the
database path for older Notmuch configurations).

## Drafts

```toml
[drafts]
save_maildir = true
tags = ["draft"]
index_after_save = true
# maildir = "/home/alice/.mail/account/Drafts"
```

Explicit saves use a Maildir by default. When `maildir` is omitted, the default
is `Drafts` under the effective Notmuch mail root, with the same legacy fallback
as Sent mail. Set `save_maildir = false` to keep named drafts only in notm's
private local state directory.

Composer recovery, local named drafts, cached compose attachments, and tag-undo
history live under `${XDG_STATE_HOME:-$HOME/.local/state}/notm/`. Settings and
state files that can contain command values or message content are restricted
to the user when `notm` creates or replaces them on Unix. Settings writes parse
the existing TOML before changing it and replace the file atomically; parse or
write failures are reported without partially updating the configuration.

## Synchronization

Synchronization is disabled until every relevant gate is enabled. See
[Synchronization](sync.md) for a complete example, execution order, shell and
environment behavior, timeouts, and testing guidance.

## Developer test harness

The `[automation]` section configures a local UI-driving test harness. It is not
mail rules or filtering support and is disabled by default.

| Key | Default and purpose |
| --- | --- |
| `enabled` | Enable the harness; defaults to `false`. |
| `socket_path` | Unix socket path; omission uses a per-process path under absolute `XDG_RUNTIME_DIR`, falling back to `/tmp`. |
| `token` | Request token. Use an explicit private value for non-fixture runs. |
| `screenshot_dir` | Screenshot output directory; defaults to `artifacts/screenshots`. |
| `allow_live_send_test` | Permit gated harness sends and `live-self-send`; defaults to `false`. |
| `allow_live_tag_test` | Permit harness tag mutations on a live database; defaults to `false`. |

Use fixture data first and read the [developer test-harness guide](automation/README.md)
before enabling live gates.

## Compatibility keys

`ui.confirm_destructive_tag_actions`, `send.one_live_self_test_per_run`, and
`sync.show_manual_sync_button` are accepted but ignored for compatibility.
They are omitted by `print-config` and can be removed.

The installed `notm-config(5)` manual is the exhaustive field reference for the
installed version.
