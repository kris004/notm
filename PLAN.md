# notm implementation plan

## Architecture

- `notm-notmuch`: bindgen-generated FFI for the installed `notmuch.h`, plus safe RAII wrappers for databases, queries, threads, messages, tags, filenames, config, revision, indexing, and tag mutation.
- `notm-mail`: RFC5322/MIME parsing, safe HTML-to-text rendering, composition, reply/reply-all/forward generation, and send transport abstractions.
- `notm-ui`: native GTK4 desktop UI, three-pane mail workflow, local automation socket, screenshot fallback, shortcuts, command palette, debug panel.
- `notm-app`: CLI/config/logging/paths and app wiring.
- `notm-test-support`: fixture Maildir/database creation, fake transports, UI driver helpers, screenshot helpers.

## Safety

No sync commands run by default. Production code does not call the `notmuch` CLI. Explicit send is delegated to a configurable external transport only.

## Quality gates

After each feature slice: update progress/report, format, clippy, tests, fixture or live smoke where possible, screenshot where possible, and fix gate failures before continuing.
