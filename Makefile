# Build and install Spot.
#
#   make                                  build (cargo, release)
#   make install PREFIX=/usr DESTDIR=...  install (distro packages: see scripts/build-deb.sh)
#   make install PREFIX=~/.local          per-user install (scripts/install-dev.sh does this)
#   make uninstall PREFIX=...             remove what install put there
#   make extension-zip                    helper extension as a zip for extensions.gnome.org
#
# `install` never builds, so it is safe to run as root after building as a normal user.

PREFIX  ?= /usr/local
BINDIR  ?= $(PREFIX)/bin
DATADIR ?= $(PREFIX)/share
MANDIR  ?= $(DATADIR)/man
DESTDIR ?=
CARGO   ?= cargo

APP_ID   := dev.spot.Spot
EXT_UUID := spot-helper@spot.dev
BIN      := target/release/spot
EXT_SRC  := extension/$(EXT_UUID)
EXT_DST  := $(DESTDIR)$(DATADIR)/gnome-shell/extensions/$(EXT_UUID)
VERSION  := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

INSTALLED := \
	$(BINDIR)/spot \
	$(DATADIR)/applications/$(APP_ID).desktop \
	$(DATADIR)/dbus-1/services/$(APP_ID).service \
	$(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg \
	$(DATADIR)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg \
	$(DATADIR)/metainfo/$(APP_ID).metainfo.xml \
	$(MANDIR)/man1/spot.1.gz

.PHONY: all build install uninstall check extension-zip clean

all: build

build:
	$(CARGO) build --release --locked -p spot-gtk

check:
	$(CARGO) test --workspace --locked
	install -d target/check
	sed 's|@BINDIR@|/usr/bin|g' data/$(APP_ID).desktop.in > target/check/$(APP_ID).desktop
	desktop-file-validate target/check/$(APP_ID).desktop
	@# No project homepage exists yet; add <url type="homepage"> once there is one.
	appstreamcli validate --no-net --explain --override=url-homepage-missing=info data/$(APP_ID).metainfo.xml

install:
	@test -x $(BIN) || { echo "$(BIN) is missing: run 'make' first"; exit 1; }
	install -Dm755 $(BIN) $(DESTDIR)$(BINDIR)/spot
	install -d $(DESTDIR)$(DATADIR)/applications $(DESTDIR)$(DATADIR)/dbus-1/services
	sed 's|@BINDIR@|$(BINDIR)|g' data/$(APP_ID).desktop.in > $(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop
	sed 's|@BINDIR@|$(BINDIR)|g' data/$(APP_ID).service.in > $(DESTDIR)$(DATADIR)/dbus-1/services/$(APP_ID).service
	chmod 644 $(DESTDIR)$(DATADIR)/applications/$(APP_ID).desktop $(DESTDIR)$(DATADIR)/dbus-1/services/$(APP_ID).service
	install -Dm644 data/icons/$(APP_ID).svg $(DESTDIR)$(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/$(APP_ID)-symbolic.svg $(DESTDIR)$(DATADIR)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg
	install -Dm644 data/$(APP_ID).metainfo.xml $(DESTDIR)$(DATADIR)/metainfo/$(APP_ID).metainfo.xml
	install -d $(DESTDIR)$(MANDIR)/man1
	gzip -9nc data/spot.1 > $(DESTDIR)$(MANDIR)/man1/spot.1.gz
	chmod 644 $(DESTDIR)$(MANDIR)/man1/spot.1.gz
	rm -rf $(EXT_DST)
	install -Dm644 -t $(EXT_DST) $(EXT_SRC)/extension.js $(EXT_SRC)/metadata.json
	install -Dm644 -t $(EXT_DST)/schemas $(EXT_SRC)/schemas/*.gschema.xml
	glib-compile-schemas $(EXT_DST)/schemas
	chmod 644 $(EXT_DST)/schemas/gschemas.compiled
ifeq ($(DESTDIR),)
	-update-desktop-database -q $(DATADIR)/applications
	-gtk-update-icon-cache -qtf $(DATADIR)/icons/hicolor
endif

uninstall:
	rm -f $(addprefix $(DESTDIR),$(INSTALLED))
	rm -rf $(EXT_DST)
ifeq ($(DESTDIR),)
	-update-desktop-database -q $(DATADIR)/applications
	-gtk-update-icon-cache -qtf $(DATADIR)/icons/hicolor
endif

extension-zip:
	install -d target/dist
	gnome-extensions pack --force --out-dir=target/dist \
		--schema=schemas/org.gnome.shell.extensions.spot-helper.gschema.xml $(EXT_SRC)
	@echo "target/dist/$(EXT_UUID).shell-extension.zip"

clean:
	$(CARGO) clean
