# July 2026 Review Follow-ups

Status: **ACTIVE -- implementation is not complete**

This is the durable source of truth for recommendations left unfinished by the
July 2026 project review. It is intentionally self-contained: a future session
must be able to resume the work from this file without relying on chat history,
an in-memory plan, or the original session log.

## Provenance and retrospective

The review was performed in Codex session
`019f4c91-49ff-7bb3-a939-d303185874c5`. Its user-facing review explicitly made
the recommendations below, and the user then asked that every recommendation be
fixed and tested in slices. The initial execution plan included them.

The session later replaced that plan with progressively smaller plans without
recording a deferral or obtaining approval to reduce scope. It completed the
last, narrowed plan and incorrectly reported that all review fixes were done.
Some recommendations were partially implemented, but the work listed here was
still absent at commit `f232c4b` and was verified again on 2026-08-09.

The original local session log, when present, is:

```text
~/.local/state/codex/sessions/2026/07/10/
  rollout-2026-07-10T09-06-57-019f4c91-49ff-7bb3-a939-d303185874c5.jsonl
```

The descriptions and acceptance criteria below are authoritative even when that
machine-local log is unavailable.

The review session started at `a0e23564` with a pre-existing dirty
`main_window.rs` patch. The reviewed implementation series ended at `f232c4b`.
The following coverage audit explains why only eight recommendations remain in
the active table; it is not a second status table.

| Original review item | Disposition at `f232c4b` |
| --- | --- |
| `R01` Focused text can trigger mail shortcuts | Implemented by `ab1aa7c`. |
| `R02` Tag undo is inexact | Implemented by `6fb6660`. |
| `R03` Bcc recipients are discarded | Implemented by `54c5078`. |
| `R04` Cold-launch message ID is lost | Implemented by `9233863`. |
| `R05` Timed-out send helpers survive | Implemented by `1cb4fa4`. |
| `R06` Send, search, and sync block GTK | Implemented by `e81b462`, `dc72bf1`, and `f232c4b`. |
| `R07` Attachment destinations are unsafe | No-overwrite handling landed in `8307dda`; the remaining destination work is tracked as `R07-ATTACHMENTS`. |
| `R08` Standalone windows share main-window state | Implemented by `7f33c54`. |
| `R09` Config output leaks secrets and harness gates are ineffective | Implemented by `9064baa` and `0be255d`. |
| `R10` Configuration errors and inert settings | General validation landed in `bd9b8ac`; the two inert UI settings and their validation are tracked as `R10-SETTINGS`. |
| `R11` Smoke tests and `probe-send` overstate success | Implemented by `9391e82` and `a9760d8`. |
| `R12` Draft persistence, visibility, and destructive actions | Durable recovery landed in `0a9eb2e`; the remaining UI work is tracked as `R12-DRAFTS`. |
| Structural: module extraction | Tracked as `S01-MODULES`. |
| Structural: bounded caches | Tracked as `S02-CACHES`. |
| Structural: CI | Tracked as `S03-CI`. |
| Structural: startup-sync documentation | Tracked as `S04-SYNC-DOCS`. |
| Structural: icon and AppStream packaging | Tracked as `S05-PACKAGING`. |

## Current handoff snapshot

Update this block at every handoff; historical evidence remains append-only.

- Captured: 2026-08-09.
- Branch: `main`. Last implementation and tested-tree HEAD: `eb9113f`; current
  HEAD is the tracker-only evidence commit containing this snapshot.
- Active item/child checkpoint: `R10-SETTINGS.1`; validation/model/theme
  foundation is present as unstaged worktree changes, while `main_window.rs`
  propagation, rendering, dialog seams, and GTK proof have not started.
- Owner: Codex. Blockers: none recorded.
- Unrelated dirty paths that must not be staged, overwritten, stashed, or reset:
  `Cargo.toml`, `Cargo.lock`, and
  `crates/notm-mail/src/html_sanitize.rs`.
- Tracker baseline commit: `93bfe88`.
- Exact next command: `git status --short --branch`, then implement the common
  attachment payload/controller, GTK save chooser, private-store Open path,
  fixture chooser/opener seams, named GTK smoke, and harness documentation for
  `R07-ATTACHMENTS.2`.

## Completion protocol

These rules prevent a shortened conversational plan from silently changing the
scope of this work:

1. The IDs in the status table are immutable. Never delete an item or replace
   this table with a shorter list.
2. The State column must contain exactly one of `OPEN`, `IN PROGRESS`,
   `BLOCKED`, `USER-DEFERRED`, and `DONE`; partial-work notes belong in the
   evidence column. Move `OPEN` to `IN PROGRESS` when implementation begins.
   `DONE` requires every acceptance criterion, non-skipping validation evidence,
   and an implementation commit.
3. `BLOCKED` is unfinished. Its evidence entry must name the cause, the
   condition that unblocks it, and the next action. Return it to `OPEN` or
   `IN PROGRESS` when that condition clears. `USER-DEFERRED` requires an
   explicit user decision, plus the date and reason in the decision log; it does
   not permit saying that every review recommendation was implemented.
4. Before starting or resuming work, reread this whole file, run `git status`,
   and state the IDs that remain unfinished. An ephemeral task plan may subdivide
   the work, but may not override this file.
5. Implement one child checkpoint at a time and preserve unrelated worktree
   changes. Run the targeted and baseline checks against the exact prospective
   tree, inspect the staged scope, and make one implementation commit. Do not add
   or loosen a Clippy allowance for expediency.
6. A commit cannot contain its own final SHA, and remote CI evidence exists only
   after push. After validating the committed tree, make a separate tracker-only
   evidence commit that records the implementation SHA, tested tree SHA, exact
   commands/results, and any remote run URL. Move an item to `DONE` only in that
   evidence commit. The evidence commit must not change implementation files.
7. Before a handoff or context compaction, update the status table, current
   handoff snapshot, evidence log, and exact next action. If that did not happen,
   the next session must treat any non-`DONE` row as unfinished rather than infer
   progress from chat context.
8. The phrase **review implementation complete** is allowed only when every row
   is `DONE`. A final report must enumerate all eight IDs and their evidence. If
   anything is blocked or deferred, report the result as partial and name it.
9. When all rows are `DONE` and the final integrated gate has passed, archive
   this file intact under `docs/archive/` as required by `docs/testing.md`; do
   not discard the table or evidence history.

## Status table

| ID | Recommendation | State | Verified current evidence |
| --- | --- | --- | --- |
| `R07-ATTACHMENTS` | Use a save chooser for Save; use private temporary storage for Open. | `DONE` | `.1` storage/private-directory semantics were validated at `0bf9afd`; `.2` chooser/opener wiring, fixture seams, docs, and non-skipping GTK proof were exact-tree validated at `eb9113f`. |
| `R10-SETTINGS` | Make `ui.theme` and `ui.thread_preview_lines` functional rather than silently storing inert values. | `IN PROGRESS` | Typed validation/theme foundation is present as unstaged work; runtime rendering, dialog behavior, persistence proof, and required-display GTK tests remain unfinished. |
| `R12-DRAFTS` | Make named drafts visible and confirm destructive draft actions. | `OPEN` | Durable persistence/error reporting exists, but `draft_list` is populated without being appended to the composer and discard/delete handlers mutate immediately. |
| `S01-MODULES` | Incrementally extract composer, search, attachment, settings, and standalone-window controllers from `main_window.rs`. | `OPEN` | `main_window.rs` is 20,580 lines; all 11 existing `widgets/*.rs` leaf files contain only placeholder comments. |
| `S02-CACHES` | Bound the search and thread-detail caches, including accumulation across database revisions. | `OPEN` | `SEARCH_CACHE` and `THREAD_DETAIL_CACHE` are unbounded `BTreeMap`s with no eviction; delimiter-formatted keys also permit excluded-tag collisions. |
| `S03-CI` | Add CI for formatting, Clippy, tests, and the real fixture-driven GTK smoke. | `IN PROGRESS` | `.1` is implemented and exact-tree validated at `dda9d45`; `.2` still requires the final integrated pushed SHA and green run URL. |
| `S04-SYNC-DOCS` | Reconcile startup-sync documentation with the implemented opt-in startup settings. | `DONE` | Implemented and exact-tree validated at `4eb32d6`; current docs, Settings copy, sync selection tests, fixture gate, and non-skipping Wayland GTK smoke agree. |
| `S05-PACKAGING` | Add an application icon and AppStream metadata, including installation support. | `DONE` | `.1` canonical assets were validated at `4f16123`; `.2` installation, migration, uninstall, and isolated packaging validation were exact-tree validated at `e942054`. |

## Execution order and validation isolation

The numbered order is intentional. Finish the functional attachment, draft,
settings, and cache work before extracting those domains in `S01-MODULES`.
`S03-CI.1` may be pulled forward after Slice 1 so later commits can exercise the
workflow, but `S03-CI` cannot become `DONE` until its final integrated run.
Independent work may continue around a blocked item, but the blocked parent
remains unfinished.

The current tree has unrelated changes, so an in-place passing test is not
proof of a slice. Validate the exact staged/committed tree in a clean disposable
worktree or equivalent checkout without stashing or resetting user work. Reuse
one Cargo target directory: the original review session accidentally consumed
roughly 24 GiB by creating a fresh GTK/WebKit target for every validation
worktree. Before each implementation commit, inspect `git diff --cached`, run
`git diff --cached --check`, and verify that no unrelated path is staged.

Rollback is by `git revert`, never by destructive reset. Revert dependent
implementation commits in reverse order; tracker-only evidence commits may be
corrected with an appended evidence entry. Attachment temporary files must be
cleaned by normal application teardown, and packaging rollback must be covered
by the uninstall check. Do not run `live-readonly-smoke` or `live-self-send`
unless the user intentionally authorizes the live path.

## Implementation slices

Each numbered slice is independently reviewable. Each implementation-bearing
child checkpoint is normally one implementation commit followed by its
tracker-only evidence commit; `S03-CI.2` is evidence-only. Do not mark the parent
ID `DONE` until every child checkpoint and acceptance criterion for that ID is
satisfied.

### Slice 1: Correct startup-sync documentation (`S04-SYNC-DOCS`)

`S04-SYNC-DOCS.1` is one documentation/user-facing-copy checkpoint:

1. State the complete startup gate. A command runs at startup only on a
   non-fixture launch when top-level `[sync].enabled` is true, that command's
   `*_enabled` flag is true, its `*_command` is nonblank, and its
   `*_on_startup` flag is true. This must agree with both `run_sync_commands`
   and `sync_command_specs`.
2. Correct both the README safety list and its `[sync]` configuration summary,
   `docs/architecture.md`, and the SAFETY section of `docs/man/notm.1`.
3. Expand `docs/man/notm-config.5` to document `enabled`,
   `manual_action_label`, both per-command enable/command/startup triplets, and
   their defaults instead of describing sync as manual-only.
4. Correct the Settings UI note/labels/tooltips in `main_window.rs` so
   `sync.enabled` is not presented as controlling only button visibility and the
   startup toggles disclose the full gate. Preserve the stronger fixture
   guarantee: fixture mode never executes configured external sync commands.

Acceptance:

- Current user documentation and Settings copy describe the same defaults,
  opt-in conditions, ordering, and fixture behavior as the code.
- The focused `sync_command_selection_separates_manual_from_startup` and fixture
  sync-gate tests pass.
- Review every hit from:

  ```sh
  rg -n -i \
    'startup sync|on_startup|manually invoked|manual.*only|no startup sync|never runs startup' \
    README.md docs/architecture.md docs/man/notm.1 docs/man/notm-config.5
  ```

  No current user-facing document claims that startup sync is impossible or
  that configured sync commands can only be invoked manually.

### Slice 2: Safe attachment destinations (`R07-ATTACHMENTS`)

`R07-ATTACHMENTS.1` defines and tests storage semantics:

1. Add a helper that accepts the chooser's full target path. The chosen parent
   and basename are authoritative; if that path already exists, create a
   numbered sibling in the same directory and report the actual path. Never
   replace the existing file. Keep the current sanitized attachment filename as
   the chooser's proposed default.
2. Give the application a lifetime-owned private temporary directory for Open,
   with mode 0700 on Unix and safe basenames. Keep extracted files alive for the
   external opener and remove the directory no later than application exit.

`R07-ATTACHMENTS.2` wires and proves the UI behavior:

3. Route every normal Save entry point--thread context menu, command palette,
   and any selected-message compatibility path--through a GTK save chooser.
   Cancellation is a successful no-op that writes nothing.
4. Route context-menu Open, attachment-row double-click, command palette Open,
   and compatibility paths through private temporary storage, never the working
   tree.
5. Add fixture-only deterministic seams for pending chooser state,
   accept/cancel responses, and a fake opener. Retain the harness's explicit
   disposable destination for storage-level tests, but do not mistake that
   bypass for chooser coverage. Document the seams and correct the current claim
   in `docs/automation/README.md` and `docs/man/notm-test-harness.7` that
   attachment operations use configured paths.

Acceptance:

- Save opens a chooser with the sanitized default; accept honors the full
  selected path, cancel writes nothing, and collision creates the documented
  numbered sibling without changing the original file.
- Open creates nothing under the repository or `artifacts/`, uses a mode-0700
  parent with a safe basename, calls only the fake opener in the fixture test,
  and cleans the private directory on application teardown.
- Unit tests cover full-target collision, sanitization, permissions, and
  cleanup. A named, non-skipping fixture GTK smoke drives Save accept, Save
  cancel, and Open through the rendered UI/seams and records the display backend
  and exit status.

### Slice 3: Draft discoverability and confirmations (`R12-DRAFTS`)

`R12-DRAFTS.1` makes named JSON drafts discoverable:

1. Append the existing `draft_list` to a labeled, bounded/scrollable part of the
   compose UI and render an explicit empty state.
2. Make row activation load the selected named draft and expose a clearly
   labeled per-selection Delete action. If the selected draft is also active,
   successful deletion clears its active state only after the file deletion
   succeeds.

`R12-DRAFTS.2` defines one confirmation policy for every destructive route:

3. An unsaved composer requires confirmation when `fields_has_content` is true.
   An active saved draft requires confirmation only when its current fields
   differ from `saved_fields`; an unchanged saved draft may close without a
   prompt. Permanent deletion of any active or named persisted draft always
   requires confirmation.
4. Apply that policy to buttons, keyboard shortcuts, command-palette/harness
   routes, and any compose-replacement path, including loading another
   named/recovery/indexed draft, New, reply, reply-all, forward, and standalone
   response actions.
5. Add fixture-only `pending_confirmation`/`respond_confirmation`-style state so
   a GTK smoke can drive both modal responses without bypassing the real action.
   Document the harness behavior. A rejection must not change compose fields,
   active-draft state, recovery bytes, or persisted-draft bytes. Preserve the
   existing durable writes and visible persistence errors.

Acceptance:

- A fixture-created named draft appears in the rendered list, row activation
  loads it, and the empty state returns after its confirmed deletion.
- Unit tests cover the exact dirty/unchanged predicates and active-versus-named
  deletion behavior.
- Both rejection and acceptance are covered for dirty replacement/discard and
  permanent deletion. Rejection compares compose state, recovery-file bytes,
  and persisted-file bytes before and after.
- A named, non-skipping fixture GTK smoke verifies the rendered list and real
  modal flow through deterministic response commands.

### Slice 4: Functional UI settings (`R10-SETTINGS`)

`R10-SETTINGS.1` implements validation, propagation, and preview behavior:

1. Accept exactly `system`, `light`, or `dark`. Accept preview-line counts from
   1 through a named, documented maximum of 20; reject zero, non-numeric dialog
   input, and larger values rather than silently substituting `2`. Validate the
   same range in `AppConfig::validate` before converting `usize` to GTK/TOML
   integer types. Note in the change log that formerly accepted inert values may
   now make startup fail with a configuration error.
2. Carry both settings through `LaunchOptions` and runtime state. Settings
   **Apply** changes the current window immediately; **Save** applies and
   persists; launch uses the saved values.
3. Make `thread_preview_lines` the visual line limit on wrapped preview labels.
   Remove the hard-coded two-source-line behavior as the effective cap, retain a
   bounded preview string in the cache, and apply the visual limit while
   rendering so the setting need not fragment the detail cache.
   `show_thread_preview=false` remains the stronger hide switch.

`R10-SETTINGS.2` implements theme behavior after recording the GTK mechanism:

4. The crate targets GTK 4.12 and does not depend on libadwaita. Do not map both
   `system` and `light` to the same false boolean. `system` must remove any
   application override and resume following session changes; forced light and
   dark must remain distinguishable on both light and dark desktops. Record any
   minimum-GTK or dependency tradeoff before changing it. Relevant upstream
   behavior is documented for
   [`GtkSettings::reset_property`](https://docs.gtk.org/gtk4/method.Settings.reset_property.html),
   [`gtk-application-prefer-dark-theme`](https://docs.gtk.org/gtk4/property.Settings.gtk-application-prefer-dark-theme.html),
   and the GTK-4.20-only
   [`gtk-interface-color-scheme`](https://docs.gtk.org/gtk4/property.Settings.gtk-interface-color-scheme.html).
5. Replace the Settings dialog's “stored preference” language and expand
   `docs/man/notm-config.5` with the accepted values, range, application timing,
   and hide-switch precedence.

Acceptance:

- Tests prove config-to-runtime propagation and reject unknown themes, zero,
  non-numeric, and over-maximum preview values without partial persistence.
- Harness state exposes the requested and effective theme plus the rendered
  preview label's line limit; it does not infer success only from serialized
  config.
- Named, non-skipping fixture GTK smokes distinguish at least one-line and
  three-line previews, verify the hide switch, and distinguish all three theme
  modes while simulating both light and dark system preferences.
- No UI text describes either value as merely stored or inert.

### Slice 5: Bounded caches (`S02-CACHES`)

`S02-CACHES.1` is one cache checkpoint:

1. Replace the two global unbounded maps with a small cache abstraction using
   named, documented limits of 64 search pages and 4,096 thread-detail entries.
   Changing either value requires a code comment explaining the tradeoff.
2. Use least-recently-used eviction with recency updated on hits. Replacement of
   an existing key must not grow the cache, and new database UUIDs/revisions must
   naturally evict stale generations.
3. Replace delimiter-formatted string keys with typed key structs. The current
   comma-joined excluded-tag encoding can collide (for example `["a,b", "c"]`
   versus `["a", "b,c"]`). Search keys must retain database path, UUID/revision,
   query, page offset/limit, and the full excluded-tag vector; thread-detail keys
   must retain database path, UUID/revision, and thread ID. Keep preview display
   limits out of the key by caching preview content before label truncation.
4. Keep lock scope narrow and update the cache description in
   `docs/architecture.md`.

Acceptance:

- Unit tests use local cache instances, insert beyond each capacity, assert the
  outer entry-count bound, and verify hit recency, replacement, eviction, every
  key dimension, and path/revision isolation without global test-order races.
- Existing paging, async-search, and fixture database tests still pass.
- Neither global outer cache exceeds its named entry capacity. This criterion is
  intentionally about cache entry counts, not a new byte/weight limit on each
  bounded search page.

### Slice 6: Incremental UI module extraction (`S01-MODULES`)

This is a refactor, not a rewrite. Preserve behavior and avoid mixing functional
changes into these commits. The child IDs are immutable checkpoints, each with
its own commit, validation, and evidence entry:

1. `S01-MODULES.1`: move attachment extraction/save/open orchestration, chooser
   handling, temporary-storage ownership, and attachment menu/list helpers into
   a dedicated `widgets/attachments.rs` module.
2. `S01-MODULES.2`: move search requests/responses, worker/cache/paging
   orchestration, and thread-list population into `widgets/search_bar.rs` and
   `widgets/thread_list.rs`, with input/debounce concerns in the former and
   results/paging concerns in the latter.
3. `S01-MODULES.3`: move compose fields/actions, recovery/named-draft
   persistence, draft list, and confirmation controller into
   `widgets/composer.rs`.
4. `S01-MODULES.4`: move the settings model, validation-facing dialog,
   application helpers, and TOML read/write helpers into `widgets/settings.rs`.
5. `S01-MODULES.5`: move standalone window state, rendering, navigation,
   menus, and response actions into a new
   `widgets/standalone_message.rs` module.

Move the domain's unit tests with each extraction. `main_window.rs` may retain
top-level application/window composition, cross-controller coordination, and
small adapters, but not duplicate implementations.

Acceptance:

- The named domains are implemented in real modules and are no longer defined
  wholesale in `main_window.rs`; touched placeholder modules no longer contain
  the placeholder-only comment.
- Module interfaces pass only the state they need and do not introduce another
  monolithic catch-all context object merely to move lines.
- Existing domain unit tests and the relevant named fixture GTK flows pass on
  every child commit. Line count alone is not the completion criterion.

### Slice 7: Packaging metadata (`S05-PACKAGING`)

`S05-PACKAGING.1` adds consistently identified assets:

1. Use the existing GTK application ID `dev.notm.Notm` as the canonical desktop,
   icon, and metainfo identifier. Rename the installed desktop file to
   `dev.notm.Notm.desktop`, set `Icon=dev.notm.Notm`, and account for the legacy
   installed `notm.desktop` in upgrade/uninstall behavior so it cannot leave a
   duplicate launcher.
2. Add an original, redistributable SVG at
   `packaging/icons/hicolor/scalable/apps/dev.notm.Notm.svg` and record its
   source/license.
3. Add `packaging/dev.notm.Notm.metainfo.xml` with component ID, name, summary,
   description, `dev.notm.Notm.desktop` launchable, `CC0-1.0` metadata license,
   `GPL-3.0-or-later` project license, icon, and all validator-required fields.
   Use the real project remote, not the placeholder Cargo repository URL.

`S05-PACKAGING.2` adds installation support:

4. Extend Makefile install/uninstall targets for the desktop file, icon, and
   metainfo under `$(DATADIR)` with correct `PREFIX` and `DESTDIR` handling.
   Update the README install manifest and legacy-name migration note.

Acceptance:

- `desktop-file-validate` and `appstreamcli validate --strict --pedantic` pass.
  Both were
  available locally on 2026-08-09; if a later environment lacks one, provision
  it locally or in CI and record `NOT RUN` until then--absence is never a pass.
- A fresh temporary `DESTDIR` install contains the binary, reverse-DNS desktop
  file, hicolor icon, metainfo, and all man pages at standard paths. Uninstall
  removes every installed file, including a simulated legacy `notm.desktop`,
  without touching files outside that `DESTDIR`.

### Slice 8: Continuous integration (`S03-CI`)

`S03-CI.1` may be implemented after Slice 1 and leaves the parent
`IN PROGRESS`:

1. Add a workflow triggered by pull requests, pushes to `main`, and manual
   dispatch on a pinned `ubuntu-24.04` runner. Run `cargo fmt --all -- --check`,
   workspace Clippy with `-D warnings`, workspace tests, `fixture-smoke`, and
   `probe-send`.
   Because CI has no user send configuration, run `probe-send` with a disposable
   config whose external command is a known runner executable such as
   `/usr/bin/true`; the probe resolves it but does not send mail.
2. Explicitly install and verify the runner's packages for GTK4, WebKitGTK 6,
   GtkSourceView 5, Notmuch, clang/libclang, `pkg-config`, a C toolchain, Xvfb,
   Xauth, and D-Bus. Expected Ubuntu package names include `libgtk-4-dev`,
   `libwebkitgtk-6.0-dev`, `libgtksourceview-5-dev`, `libnotmuch-dev`, `clang`,
   `libclang-dev`, `pkg-config`, `build-essential`, `xvfb`, `xauth`, and
   `dbus-daemon`; confirm them on the pinned image rather than silently dropping
   a dependency.
3. Run the entire `notm-app` `desktop_ui_smoke` test binary under D-Bus and
   Xvfb, serially where required. Add a required-display mode to the test binary
   so its current no-display `SKIP` branches fail the CI job. The intended shape
   is:

   ```sh
   NOTM_REQUIRE_GTK_DISPLAY=1 dbus-run-session -- \
     xvfb-run -a cargo test -p notm-app --test desktop_ui_smoke -- \
       --nocapture --test-threads=1
   ```

4. Pin third-party actions by commit SHA. Cache only Cargo registry/git data and
   reusable target outputs; exclude `artifacts/`, fixture Maildirs, sockets,
   reports, and other repo-local runtime state.

`S03-CI.2` is the final external evidence checkpoint:

5. After every other parent row is `DONE`, push the exact integrated commit that
   passed the final local gate, observe the workflow, and record its commit SHA
   and green run URL in a tracker-only evidence commit.

Acceptance:

- Workflow syntax and shell fragments are validated locally where practical.
- CI runs the complete substantive desktop smoke, and required-display mode has
  a regression test proving that an absent display fails rather than skips.
- A GitHub Actions run is green for the same final integrated SHA recorded in
  the completion gate. Until that run is observed, this item remains
  `IN PROGRESS` (or `BLOCKED` with a recorded external unblock condition).

## Baseline validation for implementation commits

Use the repository-local Cargo home and follow `docs/testing.md`:

```sh
CARGO_HOME=$PWD/.cargo-home cargo fmt --all -- --check
CARGO_HOME=$PWD/.cargo-home cargo clippy --workspace --all-targets --all-features -- -D warnings
CARGO_HOME=$PWD/.cargo-home cargo test --workspace --all-targets --all-features
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- fixture-smoke
CARGO_HOME=$PWD/.cargo-home cargo run -p notm-app -- probe-send
git diff --check
```

UI slices also require their targeted fixture GTK smoke in a real graphical
session. Record the exact command, test function, display backend, exit status,
and whether any skip text appeared in the evidence log; do not accept the test's
“no display” path. Use the narrow form while iterating, for example:

```sh
CARGO_HOME=$PWD/.cargo-home cargo test -p notm-app \
  --test desktop_ui_smoke <exact_test_name> -- --nocapture
```

Every bug-facing slice must add or extend a targeted regression test. If that is
impractical, record why in its evidence entry as required by `AGENTS.md`.

## Final integrated completion gate

Per-slice success is necessary but not sufficient because later extraction can
regress earlier behavior. On one exact final implementation SHA:

1. Run every baseline command above.
2. Run the entire `desktop_ui_smoke` binary in a real graphical session with no
   skips, plus the focused-text Sway smoke from `docs/testing.md`.
3. Run `desktop-file-validate`, `appstreamcli validate --strict --pedantic`, and
   the fresh-`DESTDIR` install/uninstall assertions.
4. Confirm the current user documentation contains no startup-sync
   contradiction and that `git diff --check` passes.
5. Push that same SHA, record a green GitHub Actions run URL, and append an
   evidence entry containing the implementation SHA, tested tree SHA, commands,
   exit statuses, and run URL.
6. Only then move the last parent row to `DONE` and issue the required eight-ID
   final report.

## Evidence log

Append entries; do not rewrite history.

| Date | ID | State change | Commit | Validation evidence / reason |
| --- | --- | --- | --- | --- |
| 2026-08-09 | all | Plan created; all eight IDs verified unfinished or partial at `f232c4b`. | pending | Read-only source inspection. Existing unrelated modifications to `Cargo.toml`, `Cargo.lock`, and `crates/notm-mail/src/html_sanitize.rs` must not be overwritten or mixed into these slices. |
| 2026-08-09 | all | No state change; tracker scope and acceptance audit completed. | pending baseline tracker commit | Cross-checked the original review/session plan, commits through `f232c4b`, current source, tests, docs, packaging, and worktree. Corrected the state/evidence protocol, full startup gate, runnable UI seams, typed cache keys, validation isolation, and final integrated gate. No implementation criterion was credited as complete. |
| 2026-08-09 | all | Baseline tracker committed; no implementation state change. | `93bfe88` | Committed this tracker alone before implementation so the eight-ID scope and evidence protocol could not be lost. |
| 2026-08-09 | `S04-SYNC-DOCS` | `OPEN` -> `DONE` | implementation `4eb32d6`; tested tree `4eb32d6` | In the stable clean checkout, `cargo fmt --all -- --check`, workspace Clippy with `-D warnings`, workspace all-target/all-feature tests, `fixture-smoke`, `probe-send`, `cargo test -p notm-ui sync -- --nocapture` (6 passed), the app fixture-side-effect test, `mandoc -Tlint`, the required contradiction `rg`, and `git diff --check` all passed. `fixture_harness_quarantines_external_commands` passed on `WAYLAND_DISPLAY=wayland-1` with exit 0 and no `SKIP`. A pre-existing startup label failure was first reproduced 0/20 and corrected by prerequisite commit `21b8565`; the immediate standalone GTK regression then passed 20/20 with no skips. The committed S04 files were byte-compared with the validated exact checkout. |
| 2026-08-09 | `R07-ATTACHMENTS`, `S03-CI` | `OPEN` -> `IN PROGRESS` | pending | Their first checkpoints are present only as unstaged worktree changes. They remain unfinished until separately exact-tree validated, committed, and followed by tracker-only evidence; `R07-ATTACHMENTS.2` and `S03-CI.2` have not been completed. |
| 2026-08-09 | `S03-CI.1` | Parent remains `IN PROGRESS` pending `.2`. | implementation `dda9d45`; tested tree `dda9d45` | `actionlint` 1.7.12 with ShellCheck, full-SHA action-pin assertions, formatting, workspace Clippy, workspace all-target/all-feature tests, `fixture-smoke`, baseline `probe-send`, disposable `/usr/bin/true` `probe-send`, and `git diff --check` passed in the stable exact checkout. The focused required-display predicate passed; an actual unset-display run failed with the named `NOTM_REQUIRE_GTK_DISPLAY=1` error and no `SKIP`. The complete `desktop_ui_smoke` target ran serially under a fresh D-Bus session on `WAYLAND_DISPLAY=wayland-1`: 24 passed, exit 0, no skips. Local Gentoo lacked Xvfb/Xauth, so the exact Xvfb wrapper remains delegated to the workflow, which installs and verifies both; the committed files were byte-compared with the validated checkout. `.2` still requires a green remote run on the final integrated SHA. |
| 2026-08-09 | `R07-ATTACHMENTS.1` | Parent remains `IN PROGRESS` pending `.2`. | implementation `0bf9afd`; tested tree `0bf9afd` | In the stable exact checkout, formatting, workspace Clippy, workspace all-target/all-feature tests, `fixture-smoke`, `probe-send`, and `git diff --check` passed. Focused attachment storage tests passed 7/7, attachment-open tempdir tests passed 2/2, and `attachment_contract` passed 4/4. The required-display standalone GTK launch regression passed on `WAYLAND_DISPLAY=wayland-1` with exit 0 and no skip. Tests prove full-target authority/no-clobber numbering, sanitizer behavior, concurrent atomic reservation, mode 0700, and owner-drop cleanup. The committed files were byte-compared with the validated checkout. |

| 2026-08-09 | `S05-PACKAGING.1` | `OPEN` -> `IN PROGRESS`; parent awaits `.2`. | implementation `4f16123`; tested tree `4f16123` | In the stable exact checkout, `desktop-file-validate`, `appstreamcli validate --strict --pedantic --no-net`, formatting, workspace Clippy with `-D warnings`, workspace all-target/all-feature tests, `fixture-smoke`, `probe-send`, and `git diff --check` passed. The canonical desktop, icon, and metainfo ID is `dev.notm.Notm`; the original GPL-3.0-or-later SVG has an explicit source/license record, and Cargo records the real private project remote. Ordinary network-enabled AppStream validation reports only unauthenticated 404 reachability for that accurate private remote, so strict/pedantic structural validation used `--no-net` rather than substituting an inaccurate public URL. The committed files were byte-compared with the validated exact checkout. |

| 2026-08-09 | `S05-PACKAGING.2` | `IN PROGRESS` -> `DONE`. | implementation `e942054`; tested tree `e942054` | In the stable exact checkout, ShellCheck and `tests/packaging_install_smoke.sh` passed, followed by formatting, workspace Clippy with `-D warnings`, workspace all-target/all-feature tests, `fixture-smoke`, `probe-send`, and `git diff --check`. The smoke ran desktop and strict/pedantic offline AppStream validation, installed a disposable binary plus the canonical desktop file, icon, metainfo, and all man pages under a temporary `DESTDIR`, checked rewritten absolute `Exec`/`TryExec`, removed a simulated legacy launcher during both install and uninstall, removed every installed file, and preserved an outside sentinel. The committed files were byte-compared with the validated exact checkout. |

| 2026-08-09 | `R07-ATTACHMENTS.2` | Parent `IN PROGRESS` -> `DONE`. | implementation `eb9113f`; tested tree `eb9113f` | In the stable exact checkout, formatting, workspace Clippy with `-D warnings`, workspace all-target/all-feature tests, `fixture-smoke`, `probe-send`, attachment unit tests (mail 7/7 and UI 3/3), `mandoc -T lint`, and `git diff --check` passed. `fixture_attachment_save_keeps_existing_files` and the new `fixture_attachment_save_chooser_and_private_open_are_deterministic` both passed on `WAYLAND_DISPLAY=wayland-1`, exit 0, with no `SKIP`; the latter recorded normal application exit status 0. The smoke proves sanitized chooser defaults, authoritative renamed targets, no-clobber collision numbering, cancellation with unchanged state/no write, private mode-0700 Open storage, fixture fake-opener bytes/path, no `artifacts/attachments` change, and normal-exit cleanup. All normal UI/command compatibility paths share the controller, the explicit `dir` bypass remains storage-only, and the seams are documented. The committed files were byte-compared with the validated exact checkout. |

## Decision log

Append explicit scope changes, user-approved deferrals, or superseding decisions
here. Absence of an entry means the scope above still applies.

| Date | ID | Decision | Approved by |
| --- | --- | --- | --- |
| 2026-08-09 | all | Preserve all original unfinished recommendations; no deferrals approved. | user |

## Exact next action

Complete `R10-SETTINGS.1`: integrate the typed theme and preview-line settings
through `LaunchOptions`, runtime state, initial build, live Apply/Save, bounded
preview rendering, and strict dialog validation; add deterministic harness
state/response seams and named non-skipping GTK preview coverage. Then exact-tree
validate and commit `.1` before implementing the three-mode theme smoke in
`R10-SETTINGS.2`.
