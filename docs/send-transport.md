# Send transport

`notm` does not implement SMTP. It submits complete RFC5322 messages to an
external command in one of four modes:

- `stdin_rfc5322`
- `file_arg`
- `command_template`
- `auto`

`notm probe-send` checks whether the configured helper and working directory
are available; it never submits a message or loads a Notmuch database. The
`auto` transport mode is a compatibility alias for `stdin_rfc5322` and **does
send mail** when used by the composer. Fixture tests use a fake capture
transport instead of a real helper.

The live UI refuses to fall back to the fake capture transport when no
`send.command` is configured. Fake capture is only enabled for fixture/test
launches so a real compose window cannot report success for an unsent message.

Transport mode behavior:

- `auto`: currently the same as `stdin_rfc5322`. `notm` writes the complete
  RFC5322 message to the command's standard input and passes exactly the
  configured `send.args`.
- `stdin_rfc5322`: writes the complete RFC5322 message to standard input. Use this for sendmail-style commands that read the message from stdin.
- `file_arg`: writes the complete RFC5322 message to a temporary file, then appends that file path after all configured `send.args`.
- `command_template`: writes the complete RFC5322 message to a temporary file, then replaces `{file}` wherever it appears in each configured argument. Use this when the command needs the message path in a specific position, such as `args = ["--message", "{file}"]`. This mode fails if no argument contains `{file}`.

Any helper-specific flags must be set explicitly in config. notm does not inspect helper scripts or add implicit arguments.

Before invoking the helper, notm constructs and validates the complete wire
message. It uses CRLF line endings throughout, folds address, subject, and
threading fields, applies RFC 2047 encoded-words to Unicode or otherwise
overlong display names and subjects, and uses RFC 2231 continuations for
Unicode or long attachment filenames. Text and HTML parts use wrapped
quoted-printable encoding; ordinary attachments use wrapped Base64. Attached
`message/rfc822` messages are parsed, normalized, checked for wire line limits,
and sent with the standards-permitted `8bit` transfer encoding rather than an
illegal Base64 encoding. Multipart boundaries, the Date, and Message-ID are
stored with the composed message, so repeated rendering produces identical
bytes for transport and Sent persistence.

Header values containing CR, LF, or control characters other than the RFC 5322
HTAB whitespace character are rejected, not silently repaired. Invalid mailbox
or threading identifiers and unsafe embedded messages also fail before the
helper starts or a fake capture is written. The composer reports that
validation error so the user can correct the message rather than unknowingly
sending altered headers.

The helper runs with the configured `timeout_seconds` (120 by default). On
timeout, `notm` terminates its process group and reaps the direct child. Stdout
and stderr are drained with bounded capture; a nonzero exit is reported as a
rejected send together with the available status and stderr.

When the composer has Bcc recipients, notm includes a `Bcc` field in the
submitted RFC5322 message so a header-reading helper can add those recipients to
the delivery envelope. The helper must remove that field before final delivery;
the sendmail-style `-t` configuration in the README follows this contract. The
field is therefore expected in a pre-helper fixture capture, but must not appear
in a message captured after a correctly configured delivery helper has applied
its normal Bcc stripping.

Optional post-send persistence is explicit:

```toml
[send]
save_sent = true
sent_maildir = "/path/to/Mail/Sent" # optional; defaults to <mail-root>/Sent when enabled
sent_tags = ["sent"]
index_sent_after_send = true
```

If enabled, `notm` writes the exact RFC5322 bytes submitted to the helper to a
Maildir and, only when `index_sent_after_send=true`, indexes that one file
through libnotmuch and applies the configured tags. The default location is
`Sent` under Notmuch's effective `database.mail_root`, with the database path as
a legacy fallback. It does not run `notmuch new` or any sync command.

Explicit draft saves are Maildir-backed by default. They write a normal local
message under `<mail-root>/Drafts`, index that file, and apply `tag:draft`; set
`save_maildir = false` to fall back to local JSON draft files. Because this is a
local Maildir file, Gmail does not auto-delete it: use `Delete local draft` while
editing the draft to remove the file and its notmuch index entry. Opening a
message tagged with the configured draft tag opens it in the composer for later
editing/sending.

```toml
[drafts]
save_maildir = true
maildir = "/path/to/Mail/Drafts" # optional; defaults to <mail-root>/Drafts when enabled
tags = ["draft"]
index_after_save = true
```

Live send validation is explicitly gated and should only be run against a user-configured throwaway or personal test transport.
