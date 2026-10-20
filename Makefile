# Installs into the user's home by default; no root needed. Packagers set
# PREFIX=/usr and DESTDIR to stage into a package root.
PREFIX ?= $(HOME)/.local
DESTDIR ?=
BIN := target/release/catlock
APP_ID := io.github.catlock.Catlock
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ROOT := $(DESTDIR)$(PREFIX)
ICONS := $(ROOT)/share/icons/hicolor

.PHONY: build test install uninstall dist

build:
	cargo build --release

test:
	cargo test

# Rebuilds only when the binary is missing, so a packager's build() output is reused.
$(BIN):
	cargo build --release

install: $(BIN)
	install -Dm755 $(BIN) $(ROOT)/bin/catlock
	install -Dm644 data/$(APP_ID).desktop $(ROOT)/share/applications/$(APP_ID).desktop
	install -Dm644 data/icons/hicolor/scalable/apps/$(APP_ID).svg $(ICONS)/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg $(ICONS)/symbolic/apps/$(APP_ID)-symbolic.svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APP_ID)-locked-symbolic.svg $(ICONS)/symbolic/apps/$(APP_ID)-locked-symbolic.svg
	install -Dm644 README.md $(ROOT)/share/doc/catlock/README.md
# Package managers refresh these caches through their own hooks.
ifeq ($(DESTDIR),)
	-gtk-update-icon-cache -qtf $(ICONS)
	-update-desktop-database -q $(ROOT)/share/applications
endif

uninstall:
	rm -f $(ROOT)/bin/catlock $(ROOT)/share/applications/$(APP_ID).desktop
	rm -f $(ICONS)/scalable/apps/$(APP_ID).svg $(ICONS)/symbolic/apps/$(APP_ID)-symbolic.svg
	rm -f $(ICONS)/symbolic/apps/$(APP_ID)-locked-symbolic.svg
	rm -rf $(ROOT)/share/doc/catlock

# Source tarball of the committed tree, consumed by packaging/arch/PKGBUILD.
dist:
	git archive --format=tar.gz --prefix=catlock-$(VERSION)/ -o packaging/arch/catlock-$(VERSION).tar.gz HEAD
	@echo packaging/arch/catlock-$(VERSION).tar.gz
