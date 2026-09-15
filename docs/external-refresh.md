# External search refresh

An external indexer or user service can ask running notm instances to reread
their active search after the Notmuch database changes:

```sh
notm refresh --all --timeout-seconds 30
```

This is a production interface, independent of the optional developer test
harness. It neither initializes GTK nor activates an application. It never
opens or presents a window, changes the active query, runs external sync
commands, sends mail, or schedules draft/tag writes.

## Scope and exits

- `--all` is required. Discovery is a snapshot of already-owned application
  names on the **caller's existing local user session bus**. Normal GTK launches
  share `io.github.kris004.notm`, so there is normally one applicable process
  per bus, regardless of how many times `notm launch` was invoked.
- Every discovered same-user owner in the chosen namespace is addressed
  independently. A slow or incompatible peer does not stop dispatch to others.
  Separate session buses and sandboxes that hide the application name are not
  traversed. This is not a system-wide process or cross-session control tool.
- By default, all fixture and developer-harness instances are excluded.
  `--test-instances` instead targets **only** the isolated
  `io.github.kris004.notm.test.*` namespace on that bus. It never includes the
  normal instance. Fixtures without a test harness also use this namespace.
- Exit **0** means every discovered target completed a qualifying full search,
  including application of its result model. **No running instance is a
  successful no-op**, with `{"refreshed":0,"failed":[]}` on stdout.
- Exit **1** means discovery, connection, compatibility, search, or deadline
  failure. A normal completion report is a single JSON object on stdout with
  `refreshed` and a `failed` array containing `instance` and `error`. Early
  errors and the overall watchdog timeout are reported on stderr instead.
  Partial success is not rolled back.
- Invalid command-line syntax exits **2**. `--config` is rejected for refresh:
  configuration files and Notmuch profiles do not select bus targets, and no
  application or Notmuch configuration is read by this command.

The overall deadline defaults to 30 seconds and accepts 1 through 300 seconds.
It covers connection establishment, discovery, dispatch, waiting for busy
instances, and search completion. Each instance also bounds its pending
requests (64) and their lifetimes. Discovery accepts at most 64 targets.
On timeout, an already-dispatched read-only search may still finish; timeout
does not mean delivery was cancelled or that nothing happened. A later refresh
is safe, but tight retry loops are unnecessary.

## User-service integration

Run the command as the desktop user, after the indexing operation has finished.
Use the same `DBUS_SESSION_BUS_ADDRESS` as the desktop, or the standard existing
bus at `$XDG_RUNTIME_DIR/bus` when that variable is absent. No display variables
are needed. Only existing Unix session buses are accepted; D-Bus autolaunch is
not used. An unavailable bus is an error, not evidence of zero running apps.

Keep this command in a short, separate service work item rather than making it
part of a mail-fetch transaction. The command does not run `notmuch new`,
acquire the external sync tool's locks, or wait for that tool on your behalf.
There is no need to enable `[sync]`, the developer test harness, or any live
automation permissions in notm's configuration.

## Search ordering and user state

Requests wait for in-progress searches (including debounced input and
incremental model application), tag/send/sync workers, draft persistence,
composer/thread preparation, recovery, and pending composer confirmations.
They require a full search begun after receipt, not the completion of a search
that might have read the database before the external update. Concurrent
requests for the same next search coalesce; a request received during that
search requires a later one. User searches and mutations retain priority and
can supersede a refresh without being cancelled by it. A busy instance may
therefore exhaust the caller's deadline.

The existing full-search reconciliation is used without first-row activation
or focus routing. Composer contents, unsaved edits, and active drafts remain
intact. The selected exact thread ID is retained if it is still in the first
refreshed page and retained path state is not uncertain. As with other full
searches, pagination restarts at the first page, visual/multi-selection is
cleared, and prepared-thread caches are discarded. Existing uncertain-tag
warnings and their safety gates are not bypassed.

## Protocol and older processes

Updated processes export `io.github.kris004.notm.SearchRefresh1.Refresh(u)`
at `/io/github/kris004/notm/SearchRefresh`. The argument is the remaining
deadline in milliseconds (1–300000); the response is the completed full-search
generation (`t`). A D-Bus error represents a closed/unready window, a full
pending queue, a deadline, or a search error. This is deliberately separate
from application activation and from the test-harness socket protocol.

The client snapshots well-known names, resolves and verifies their Unix user,
then calls the **unique owner**, with `NO_AUTO_START`. If an owner disappears,
the request fails instead of activating a replacement. Application versions
without the interface report a clear relaunch-required error while compatible
peers still refresh. No Unix signals or legacy test-harness commands are sent.
Very old processes that do not own a discoverable application name cannot be
controlled by this protocol.

Replacing the executable on disk does not update already-running processes.
Relaunch older windows at a convenient time to gain this capability; installation
and `refresh` never close or restart them automatically. Capability is determined
by the versioned interface, not by the CLI package version string.
