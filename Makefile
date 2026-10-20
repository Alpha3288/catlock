# Installs into the user's home by default; no root needed.
PREFIX ?= $(HOME)/.local
BIN := target/release/catlock
APP_ID := io.github.catlock.Catlock
ICONS := $(PREFIX)/share/icons/hicolor

.PHONY: build test install uninstall

build:
	cargo build --release

test:
	cargo test

install: build
	install -Dm755 $(BIN) $(PREFIX)/bin/catlock
	install -Dm644 data/$(APP_ID).desktop $(PREFIX)/share/applications/$(APP_ID).desktop
	install -Dm644 data/icons/hicolor/scalable/apps/$(APP_ID).svg $(ICONS)/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg $(ICONS)/symbolic/apps/$(APP_ID)-symbolic.svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APP_ID)-locked-symbolic.svg $(ICONS)/symbolic/apps/$(APP_ID)-locked-symbolic.svg
	-gtk-update-icon-cache -qtf $(ICONS)
	-update-desktop-database -q $(PREFIX)/share/applications

uninstall:
	rm -f $(PREFIX)/bin/catlock $(PREFIX)/share/applications/$(APP_ID).desktop
	rm -f $(ICONS)/scalable/apps/$(APP_ID).svg $(ICONS)/symbolic/apps/$(APP_ID)-symbolic.svg
	rm -f $(ICONS)/symbolic/apps/$(APP_ID)-locked-symbolic.svg
