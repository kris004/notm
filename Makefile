PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
DATADIR ?= $(PREFIX)/share
APPDIR ?= $(DATADIR)/applications
MANDIR ?= $(DATADIR)/man
CARGO ?= cargo
INSTALL ?= install
RM ?= rm -f

.PHONY: all build install install-user install-man uninstall uninstall-user uninstall-man check test smoke clean

all: build

build:
	$(CARGO) build --release -p notm-app

install: build install-man
	$(INSTALL) -Dm755 target/release/notm "$(DESTDIR)$(BINDIR)/notm"
	$(INSTALL) -d target/install
	sed -e 's|^Exec=.*|Exec=$(BINDIR)/notm launch|' \
	    -e 's|^TryExec=.*|TryExec=$(BINDIR)/notm|' \
	    packaging/notm.desktop > target/install/notm.desktop
	$(INSTALL) -Dm644 target/install/notm.desktop "$(DESTDIR)$(APPDIR)/notm.desktop"

install-man:
	$(INSTALL) -Dm644 docs/man/notm.1 "$(DESTDIR)$(MANDIR)/man1/notm.1"
	$(INSTALL) -Dm644 docs/man/notm-config.5 "$(DESTDIR)$(MANDIR)/man5/notm-config.5"
	$(INSTALL) -Dm644 docs/man/notm-test-harness.7 "$(DESTDIR)$(MANDIR)/man7/notm-test-harness.7"
	$(INSTALL) -Dm644 docs/man/notm-automation.7 "$(DESTDIR)$(MANDIR)/man7/notm-automation.7"

install-user:
	$(MAKE) PREFIX="$(HOME)/.local" install

uninstall: uninstall-man
	$(RM) "$(DESTDIR)$(BINDIR)/notm"
	$(RM) "$(DESTDIR)$(APPDIR)/notm.desktop"

uninstall-man:
	$(RM) "$(DESTDIR)$(MANDIR)/man1/notm.1"
	$(RM) "$(DESTDIR)$(MANDIR)/man5/notm-config.5"
	$(RM) "$(DESTDIR)$(MANDIR)/man7/notm-test-harness.7"
	$(RM) "$(DESTDIR)$(MANDIR)/man7/notm-automation.7"

uninstall-user:
	$(MAKE) PREFIX="$(HOME)/.local" uninstall

check:
	$(CARGO) fmt --all --check
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

test:
	$(CARGO) test --workspace --all-targets --all-features

smoke:
	$(CARGO) run -p notm-app -- fixture-smoke
	$(CARGO) run -p notm-app -- live-readonly-smoke

clean:
	$(CARGO) clean
