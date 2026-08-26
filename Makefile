PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
DATADIR ?= $(PREFIX)/share
APPDIR ?= $(DATADIR)/applications
ICONDIR ?= $(DATADIR)/icons/hicolor/scalable/apps
METAINFODIR ?= $(DATADIR)/metainfo
MANDIR ?= $(DATADIR)/man
CARGO ?= cargo
INSTALL ?= install
RM ?= rm -f
BINARY ?= target/release/notm
DESKTOP_ID := io.github.kris004.notm
LEGACY_DESKTOP_ID := dev.notm.Notm
DESKTOP_FILE := $(DESKTOP_ID).desktop
ICON_FILE := $(DESKTOP_ID).svg
METAINFO_FILE := $(DESKTOP_ID).metainfo.xml

.PHONY: all build install install-user install-man uninstall uninstall-user \
	uninstall-man check check-policy check-packaging test smoke \
	check-source-archive smoke-live-readonly smoke-live-send clean

all: build

build:
	$(CARGO) build --release --locked -p notm-app

install: build install-man
	$(INSTALL) -Dm755 "$(BINARY)" "$(DESTDIR)$(BINDIR)/notm"
	$(INSTALL) -d target/install
	sed -e 's|^Exec=.*|Exec=$(BINDIR)/notm launch %u|' \
	    -e 's|^TryExec=.*|TryExec=$(BINDIR)/notm|' \
	    "packaging/$(DESKTOP_FILE)" > "target/install/$(DESKTOP_FILE)"
	$(RM) "$(DESTDIR)$(APPDIR)/notm.desktop"
	$(RM) "$(DESTDIR)$(APPDIR)/$(LEGACY_DESKTOP_ID).desktop"
	$(RM) "$(DESTDIR)$(ICONDIR)/$(LEGACY_DESKTOP_ID).svg"
	$(RM) "$(DESTDIR)$(METAINFODIR)/$(LEGACY_DESKTOP_ID).metainfo.xml"
	$(INSTALL) -Dm644 "target/install/$(DESKTOP_FILE)" "$(DESTDIR)$(APPDIR)/$(DESKTOP_FILE)"
	$(INSTALL) -Dm644 "packaging/icons/hicolor/scalable/apps/$(ICON_FILE)" \
	    "$(DESTDIR)$(ICONDIR)/$(ICON_FILE)"
	$(INSTALL) -Dm644 "packaging/$(METAINFO_FILE)" \
	    "$(DESTDIR)$(METAINFODIR)/$(METAINFO_FILE)"

install-man:
	$(INSTALL) -Dm644 docs/man/notm.1 "$(DESTDIR)$(MANDIR)/man1/notm.1"
	$(INSTALL) -Dm644 docs/man/notm-config.5 "$(DESTDIR)$(MANDIR)/man5/notm-config.5"
	$(INSTALL) -Dm644 docs/man/notm-test-harness.7 "$(DESTDIR)$(MANDIR)/man7/notm-test-harness.7"
	$(INSTALL) -Dm644 docs/man/notm-automation.7 "$(DESTDIR)$(MANDIR)/man7/notm-automation.7"

install-user:
	$(MAKE) PREFIX="$(HOME)/.local" install

uninstall: uninstall-man
	$(RM) "$(DESTDIR)$(BINDIR)/notm"
	$(RM) "$(DESTDIR)$(APPDIR)/$(DESKTOP_FILE)"
	$(RM) "$(DESTDIR)$(APPDIR)/$(LEGACY_DESKTOP_ID).desktop"
	$(RM) "$(DESTDIR)$(APPDIR)/notm.desktop"
	$(RM) "$(DESTDIR)$(ICONDIR)/$(ICON_FILE)"
	$(RM) "$(DESTDIR)$(ICONDIR)/$(LEGACY_DESKTOP_ID).svg"
	$(RM) "$(DESTDIR)$(METAINFODIR)/$(METAINFO_FILE)"
	$(RM) "$(DESTDIR)$(METAINFODIR)/$(LEGACY_DESKTOP_ID).metainfo.xml"

uninstall-man:
	$(RM) "$(DESTDIR)$(MANDIR)/man1/notm.1"
	$(RM) "$(DESTDIR)$(MANDIR)/man5/notm-config.5"
	$(RM) "$(DESTDIR)$(MANDIR)/man7/notm-test-harness.7"
	$(RM) "$(DESTDIR)$(MANDIR)/man7/notm-automation.7"

uninstall-user:
	$(MAKE) PREFIX="$(HOME)/.local" uninstall

check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --locked --workspace --all-targets --all-features -- -D warnings
	$(MAKE) check-policy

check-policy:
	./tests/cargo_lock_policy_smoke.py
	./tests/cargo_lock_guard_smoke.sh
	./tests/make_smoke_policy.sh
	./packaging/verify-release-metadata.py .
	./tests/release_metadata_smoke.py
	./tests/release_key_expiry_smoke.sh
	./packaging/check-release-key-expiry.sh \
		docs/release-signing-key.asc \
		BE592562E6131A53F4BADE4A046928E9A919BAF9

check-packaging: check-policy
	./tests/packaging_install_smoke.sh
	./tests/release_bundle_smoke.sh
	./tests/release_tag_smoke.sh

check-source-archive:
	./tests/source_archive_smoke.sh

test:
	$(CARGO) test --locked --workspace --all-targets --all-features

# Keep the default smoke hermetic. Live mailbox and transport checks have
# deliberately explicit targets below.
smoke:
	$(CARGO) run --locked -p notm-app -- fixture-smoke

smoke-live-readonly:
	$(CARGO) run --locked -p notm-app -- live-readonly-smoke

smoke-live-send:
	$(CARGO) run --locked -p notm-app -- live-self-send

clean:
	$(CARGO) clean
