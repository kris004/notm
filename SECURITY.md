# Security policy

## Supported versions

`notm` has not published a tagged release yet. Once releases begin, only the
latest release series will receive security updates. Older release series will
not receive backported fixes unless a release note explicitly says otherwise.

## Reporting a vulnerability

Please do not disclose a suspected vulnerability in a public issue, discussion,
or pull request.

Use GitHub's [private vulnerability reporting
form](https://github.com/kris004/notm/security/advisories/new). Include, when
possible:

- the affected version or commit;
- the operating system and relevant GTK, WebKitGTK, and Notmuch versions;
- a minimal reproduction using synthetic mail;
- the expected impact and the data or boundary at risk;
- any mitigation or fix you have already tested.

If the private form is unavailable, open an issue containing only a request for
a private contact channel. Do not include vulnerability details in that issue.

Do not attach a real mailbox, credentials, access tokens, private configuration,
or an unredacted `notm print-config --show-secrets` result. If a real message is
essential to reproduce the problem, remove addresses, message IDs, headers,
body text, and attachments that are not required for the report.

Reports will be acknowledged and assessed as soon as practical. Fix and
disclosure timing depends on severity and maintainer availability; this project
does not currently offer a bug bounty or a guaranteed response time.

## Security model

`notm` is a local desktop client. It reads a local Notmuch database and Maildir
as the current user, and it treats message content as untrusted input.

- There is no hosted service, account system, or telemetry.
- The default message view is plain text. The visual HTML view sanitizes markup,
  disables JavaScript and in-app navigation, blocks file access, and does not
  load remote images without user approval. These controls are defense in depth,
  not a guarantee that malformed mail cannot expose a parser or rendering bug.
- Archive, trash, and spam change Notmuch tags; they do not delete ordinary
  message files. Deleting a locally saved draft is a separate, explicit action.
- Sending and synchronization are delegated to user-configured programs. Those
  programs run with the user's permissions and must be treated as trusted local
  configuration. Helper output is bounded, and timed-out helpers are terminated
  and reaped.
- Attachment saving and opening are user-initiated. Opened attachments are
  passed to the desktop's configured application.
- Draft recovery and saved drafts are stored locally. On Unix, notm-created
  state directories and private message/configuration files are restricted to
  the user. Optional sent-mail saving and indexing are disabled unless
  configured.
- The developer test harness is disabled by default. When enabled, it uses a
  owner-only local Unix-domain socket and a token, and its live send/tag
  operations have additional opt-in gates. It is intended for a controlled
  user session, not as a service exposed to other users or hosts.
- Logs omit message bodies by default. `notm print-config` redacts command
  arguments, environment values, sync commands, and harness tokens unless the
  user explicitly asks to show them.

An attacker who already controls the local user account, a configured send/sync
program, or the desktop opener can act with the same permissions as `notm` and
is outside its trust boundary. Malformed or attacker-supplied email remains in
scope. Reports about unsafe handling at any boundary are welcome when `notm`
can reduce the risk.
