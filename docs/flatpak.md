# Flatpak

The production manifest is
[`io.github.kris004.notm.yml`](../io.github.kris004.notm.yml). It builds the
released `v0.1.2` source and all non-runtime dependencies from source for both
`x86_64` and `aarch64`. It is preparation for a possible future Flathub
submission; this repository does not submit or publish the application.

## Build and install locally

Install Flatpak, `flatpak-builder`, and configure the Flathub remote. The
manifest uses `org.gnome.Platform//50`, `org.gnome.Sdk//50`, and the
`org.freedesktop.Sdk.Extension.rust-stable` and
`org.freedesktop.Sdk.Extension.llvm20` SDK extensions. LLVM is build-only and
provides libclang for the generated Notmuch bindings. Then run:

```sh
flatpak remote-add --user --if-not-exists --from \
  flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --force-clean --install-deps-from=flathub \
  --compose-url-policy=full \
  --mirror-screenshots-url=https://dl.flathub.org/media \
  --install build-flatpak io.github.kris004.notm.yml
flatpak run io.github.kris004.notm
```

The installed desktop entry registers `mailto:` and launches
`notm launch %u`. To inspect the command-line tools included in the sandbox:

```sh
flatpak run --command=notm io.github.kris004.notm --version
flatpak run --command=notmuch io.github.kris004.notm --version
flatpak run --command=msmtp io.github.kris004.notm --version
```

## Mail and configuration

The Flatpak has read/write access only to the two common mail roots `~/Mail`
and `~/.mail`. It can read the legacy `~/.notmuch-config` file, but it does not
have default access to the host's modern `$XDG_CONFIG_HOME/notmuch` directory.
It does not have general home or host filesystem access.

notm's own configuration is sandbox-private. On the host, create or edit:

```text
~/.var/app/io.github.kris004.notm/config/notm/config.toml
```

Use the same schema documented in [configuration.md](configuration.md). The
Settings dialog uses the file chooser portal, so it is the preferred way to
select an existing Notmuch configuration and database. When writing paths by
hand, use paths as seen inside the sandbox. This command prints the app-private
XDG configuration root:

```sh
flatpak run --command=sh io.github.kris004.notm -c \
  'printf "%s\n" "$XDG_CONFIG_HOME"'
```

For example, copy or create a minimal Notmuch configuration beneath that
app-private directory, point `notmuch.config_path` at it, and point
`notmuch.database_path` at `~/Mail`, `~/.mail`, or a narrow filesystem override
described below. The legacy `~/.notmuch-config` path works without an explicit
`config_path`. If you instead want to reuse a modern host configuration in
place, grant that exact directory read-only:

```sh
flatpak override --user \
  --filesystem="$XDG_CONFIG_HOME/notmuch:ro" \
  io.github.kris004.notm
```

Remove this opt-in without disturbing unrelated overrides by repeating the
same permission with `--nofilesystem`:

```sh
flatpak override --user \
  --nofilesystem="$XDG_CONFIG_HOME/notmuch" \
  io.github.kris004.notm
```

Sending uses the bundled `/app/bin/msmtp` when it is selected as
`send.command`; network access is available for SMTP. Keep msmtp credentials
in the app-private configuration area, not in command-line arguments or the
project checkout. msmtp is intentionally built without libsecret access. You
can create a mode-0600 msmtp configuration at:

```text
~/.var/app/io.github.kris004.notm/config/msmtp/config
```

Use `flatpak run --command=sh ...` to print `$XDG_CONFIG_HOME` and pass the
resulting config path to msmtp with its `--file` option. See
[send-transport.md](send-transport.md) for notm's external transport contract.

The sandbox includes the Notmuch CLI, so a configured database-update command
can use `/app/bin/notmuch new`. Host executables and host shell scripts are not
visible: receive/synchronization programs such as `mbsync` are not bundled and
cannot be launched through the host. Run those separately outside notm or
package them explicitly rather than granting host command execution.

## Permissions and portals

The manifest requests only:

| Permission | Reason |
| --- | --- |
| `--share=network` | Configured SMTP delivery and optional remote HTML images |
| `--socket=wayland` | Native GTK display |
| `--socket=fallback-x11`, `--share=ipc` | X11 fallback and its shared-memory transport |
| `--device=dri` | GTK/WebKitGTK rendering acceleration |
| `~/Mail`, `~/.mail` read/write | Search, tags, Maildir flags, drafts, and sent copies in common mail roots |
| `~/.notmuch-config` read-only | Existing legacy Notmuch configuration |

GTK's native file chooser routes attachment selection and saving through the
FileChooser portal. GIO routes attachment opening, web links, and `mailto:`
links through the OpenURI portal. Portals grant only the user-selected resource
and do not require a broad filesystem or session-bus permission.

There is deliberately no `home`, `host`, secret-service, SSH, GPG, or
`org.freedesktop.Flatpak` host-command permission. This means arbitrary paths,
host password stores, and host-only send/sync helpers are unavailable.

### Mail outside the common roots

Grant only the exact mail root and, if necessary, the exact configuration file:

```sh
flatpak override --user \
  --filesystem=/absolute/path/to/mail \
  --filesystem=/absolute/path/to/notmuch-config:ro \
  io.github.kris004.notm
```

Do not use `--filesystem=home` or `--filesystem=host`. Review overrides with:

```sh
flatpak override --user --show io.github.kris004.notm
```

To roll back all user overrides for notm after recording any you want to keep:

```sh
flatpak override --user --reset io.github.kris004.notm
```

## Offline and source verification

Every archive source has a SHA-256 in the manifest. The application source is
the `v0.1.2` tag resolved to commit
`a4f90fcc642ef566af256d20390917a56a455373`; the tag alone is not trusted.
Cargo dependencies are generated from the committed `Cargo.lock`, downloaded
by flatpak-builder as checked sources, and compiled with
`cargo build --locked --frozen` and `CARGO_NET_OFFLINE=true`.

Important input hashes are:

| Input | SHA-256 |
| --- | --- |
| `Cargo.lock` | `7b354555ea427eee1adbc1c347a0952547f5e5c351c7883bd663ad28a3b68089` |
| `packaging/flatpak/cargo-sources.json` | `dcc37705a051837be630135a55a81158e3b1bbe4c3d27e3dd8044d6be823984b` |
| Xapian 1.4.32 | `c4fd64e81127311756adf5579268d14a79f285ce8ac4ead0930c96195897aece` |
| talloc 2.4.3 | `dc46c40b9f46bb34dd97fe41f548b0e8b247b77a918576733c528e83abd854dd` |
| GMime 3.2.15 | `84cd2a481a27970ec39b5c95f72db026722904a2ccf3fdbd57b280cf2d02b5c4` |
| Notmuch 0.40 | `4b4314bbf1c2029fdf793637e6c7bb15c1b1730d22be9aa04803c98c5bbc446f` |
| msmtp 1.8.33 | `41c163ce2c4c8c3c326cda8d0abd9391a7323788f0a893f49bfbe7aff3d4f276` |

`cargo-sources.json` was generated with the official
`flatpak-cargo-generator.py` at flatpak-builder-tools commit
`737c0085912f9f7dabf9341d4608e2a77a51a73a` (script SHA-256
`b373c8ab1a05378ec5d8ed0645c7b127bcec7d2f7a1798694fbc627d570d856c`).

To prove the source cache is complete before a build, download first and then
rebuild from a clean directory with downloading disabled:

```sh
flatpak-builder --user --force-clean --disable-rofiles-fuse \
  --disable-updates --download-only \
  --install-deps-from=flathub --state-dir=.flatpak-state \
  build-flatpak io.github.kris004.notm.yml
rm -rf build-flatpak
flatpak-builder --user --force-clean --disable-rofiles-fuse \
  --disable-cache --disable-download --disable-updates \
  --compose-url-policy=full \
  --mirror-screenshots-url=https://dl.flathub.org/media \
  --state-dir=.flatpak-state --repo=flatpak-repo \
  build-flatpak io.github.kris004.notm.yml
```

`--disable-download` proves that every manifest/module source is already in the
verified flatpak-builder source cache. Screenshot mirroring is a separate
AppStream media fetch: the Flathub-grade compose step retrieves the image from
the commit-pinned raw URL and rewrites the composed metadata to the Flathub
media base. The E2E verifies the mirrored PNG against SHA-256
`5e7eb334b633ade7582f45b43fcc67a8791178e4235494833ec41b328882a4ec`
and the pinned source PNG against
`8099c265a0aa7dd3d779c97786200056847bc0bb79f3a5d1150e8903e87c443d`.
The hashes differ because appstreamcli losslessly rewrites the original-size
PNG while composing its thumbnails.

The runtime, SDK, Rust extension, and LLVM extension are selected by supported
Flatpak branch, not an immutable commit in the manifest. Record their installed
Flatpak commit IDs when capturing reproducibility evidence. An offline rebuild
reuses the already installed runtime commits and the downloaded source cache;
a later online build after a runtime update is not necessarily byte-identical.

## Distribution E2E

The hard-gate E2E refuses to run on the interactive display or when existing
notm Flatpak data is present. It downloads sources, performs a clean
`--disable-download` rebuild, exports and lints a repository, installs into a
disposable user installation, inspects permissions and ELF dependencies, and
runs the standard distribution flow inside the sandbox. That flow covers
search/open, text and WebKitGTK HTML, attachment portals, compose, draft
restart, loopback-only SMTP capture, sent mail, secret denial, and uninstall:

```sh
test_root=$(mktemp -d "${TMPDIR:-/tmp}/notm-flatpak-e2e.XXXXXX")
trap 'rm -rf "$test_root"' EXIT
NOTM_TEST_DISPOSABLE_ROOT="$test_root" \
  ./tests/run_with_headless_weston.sh \
  ./tests/run_with_private_dbus.sh \
  ./tests/flatpak_distribution_e2e.sh
```

Keep Weston outside the private D-Bus wrapper so portal shutdown releases its
private runtime mount before Weston removes that runtime.

The HTML-to-`mailto` check uses the installed Flatpak desktop export but
shadows it only inside the disposable XDG data directory with the fixture and
test-harness flags. This gives the activated process the same isolated
GApplication ID as the observable fixture process, while retaining the
production Flatpak command, app ID, URI forwarding, and sandbox boundary. The
unmodified production desktop entry and its `Exec=notm launch %u` command are
validated separately before that test-only shadow is created.

If `flatpak-builder` is provided by `org.flatpak.Builder`, set
`NOTM_FLATPAK_BUILDER_COMMAND` to the committed Builder wrapper and set
`NOTM_FLATPAK_LINTER_COMMAND` to that wrapper plus `--linter`, as below. The
gate accepts only this committed wrapper as an explicit linter override so a
placeholder command cannot bypass the required official Flathub lint. Set
`NOTM_FLATPAK_USER_DIR` to the
disposable Flatpak user installation containing that Builder app and the GNOME
runtimes. That installation must be a child of `NOTM_TEST_DISPOSABLE_ROOT`;
the gate rejects any external or normal user installation. Set
`NOTM_TEST_DISPOSABLE_ROOT` to a new caller-owned mode-`0700` directory visible
on the host and inside the Builder sandbox. The Weston runtime, Builder home,
`TMPDIR`, XDG roots, Flatpak app data, and test fixture all remain below that
one root. The wrapper requires the runtime to be caller-owned mode `0700` and
rejects `/run/user` or any runtime outside the root. Do not embed `HOME` or XDG
overrides in either command. For example:

```sh
test_root=$(mktemp -d "${TMPDIR:-/tmp}/notm-flatpak-e2e.XXXXXX")
trap 'rm -rf "$test_root"' EXIT
flatpak_home="$test_root/flatpak-user"
mkdir -m 700 "$flatpak_home"
FLATPAK_USER_DIR="$flatpak_home" flatpak remote-add --user --if-not-exists \
  --from flathub https://dl.flathub.org/repo/flathub.flatpakrepo
FLATPAK_USER_DIR="$flatpak_home" flatpak install --user --noninteractive \
  --assumeyes flathub \
  org.flatpak.Builder \
  org.gnome.Platform//50 \
  org.gnome.Sdk//50 \
  org.freedesktop.Sdk.Extension.rust-stable//25.08 \
  org.freedesktop.Sdk.Extension.llvm20//25.08
NOTM_FLATPAK_USER_DIR="$flatpak_home" \
NOTM_TEST_DISPOSABLE_ROOT="$test_root" \
NOTM_FLATPAK_USE_INSTALLED_RUNTIMES=1 \
NOTM_FLATPAK_BUILDER_COMMAND='packaging/flatpak/run-builder-flatpak-e2e.sh' \
NOTM_FLATPAK_LINTER_COMMAND='packaging/flatpak/run-builder-flatpak-e2e.sh --linter' \
./tests/run_with_headless_weston.sh \
  ./tests/run_with_private_dbus.sh \
  ./tests/flatpak_distribution_e2e.sh
```

Run `./tests/flatpak_builder_wrapper_smoke.sh` first to verify that the wrapper
accepts the private test runtime but rejects live or out-of-root runtimes and
Flatpak installations.

`NOTM_FLATPAK_USE_INSTALLED_RUNTIMES=1` is only for a deliberately disposable
user installation where the four runtime/SDK refs are already installed. The
gate proves each ref is present before omitting Builder's dependency-install
step; without this opt-in it installs dependencies from the isolated Flathub
remote. The committed Builder wrapper grants the official Builder Flatpak only
the disposable root read/write and the source checkout read-only. It explicitly
revokes Builder's packaged `host`, `home`, `/var/lib/flatpak`, and normal user
Flatpak grants, clears inherited environment variables, exposes only the
test's private session bus, and propagates the temporary runtime and XDG roots.
Before every Builder or linter call, an inside-sandbox assertion proves that a
pre-existing entry from the caller's live HOME is hidden. The wrapper fails if
HOME, TMPDIR, the work directory, Flatpak installation, session bus, or runtime
escapes the root. This avoids Builder's private `/tmp` view and any fallback to
the caller's live state. Never point `NOTM_FLATPAK_USER_DIR` or
`NOTM_FLATPAK_WORK_ROOT` outside the test root; the gate rejects either before
creating it.

When an official native `flatpak-builder-lint` is already in `PATH`, omit
`NOTM_FLATPAK_LINTER_COMMAND`; the gate selects it directly.

On task supervisors that immediately reap `p11-kit server` after it daemonizes,
set `NOTM_FLATPAK_DISABLE_SESSION_P11_KIT=1`. This makes only the disposable
test bus's Flatpak session helper omit its optional PKCS#11 trust proxy; it does
not change the application manifest or installed permissions. Use it only when
the log otherwise reports a missing `pkcs11-flatpak-*` socket, and record the
exception in the test evidence.

The script requires the caller's original notm app-data path to be absent and
proves that it remains absent. It also compares entry-metadata fingerprints
for the resolved caller `HOME` directory and its `.var/app` entry before and
after the gate. Those fingerprints contain only device, inode, type/mode,
ownership, size, and timestamp fields from `stat`; the gate does not enumerate
or hash contents in either live directory. It probes only a fixed list of
conventional entry paths to select the Builder denial assertion. It never uses
live mail or an external SMTP server.

## Uninstall and rollback

Remove the application and its sandbox-private configuration, cache, and state:

```sh
flatpak uninstall --user --delete-data io.github.kris004.notm
```

This does not delete host mail or the host Notmuch database. If you added a
local build remote, remove it separately with `flatpak remote-delete --user`
after confirming no other locally installed refs use it.
