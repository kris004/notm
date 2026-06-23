PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
DATADIR ?= $(PREFIX)/share
APPDIR ?= $(DATADIR)/applications
CARGO ?= cargo
INSTALL ?= install
RM ?= rm -f

.PHONY: all build install install-user uninstall uninstall-user check test smoke clean

all: build

build:
	$(CARGO) build --release -p notm-app

install: build
	$(INSTALL) -Dm755 target/release/notm "$(DESTDIR)$(BINDIR)/notm"
	$(INSTALL) -d target/install
	sed -e 's|^Exec=.*|Exec=$(BINDIR)/notm launch|' \
	    -e 's|^TryExec=.*|TryExec=$(BINDIR)/notm|' \
	    packaging/notm.desktop > target/install/notm.desktop
	$(INSTALL) -Dm644 target/install/notm.desktop "$(DESTDIR)$(APPDIR)/notm.desktop"

install-user:
	$(MAKE) PREFIX="$(HOME)/.local" install

uninstall:
	$(RM) "$(DESTDIR)$(BINDIR)/notm"
	$(RM) "$(DESTDIR)$(APPDIR)/notm.desktop"

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
