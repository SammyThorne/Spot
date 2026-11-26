# Spot

A Spotlight-style launcher for GNOME on Wayland. Press **Super+Space**, type, press **Enter**.

Status: rough draft. Finds installed desktop apps and AppImages, learns what you pick, and
answers calculations, URLs, paths, system actions and web searches.

## Layout

| Path | What |
|---|---|
| `crates/spot-core` | Sources (`desktop.rs` via GIO, `appimage.rs`, `actions.rs`), ranking (`search.rs`, `history.rs`), extras (`calc.rs`, `open.rs`, `config.rs`), the result list (`results.rs`) and launching. No GTK. |
| `crates/spot-gtk` | The `spot` binary: resident libadwaita app with one launcher window that is shown and hidden, plus the settings/setup window. |
| `extension/spot-helper@spot.dev` | GNOME Shell 48 helper: owns the shortcut, buffers keys typed before the window appears, centres the window. |
| `data/` | `.desktop` (D-Bus activatable) and D-Bus `.service` templates, icons, AppStream metainfo, man page. |
| `Makefile` | `make`, `make install` (`PREFIX`, `DESTDIR`), `make check`, `make extension-zip`. |
| `scripts/` | `build-deb.sh` (Debian package), `install-dev.sh` / `uninstall-dev.sh` (per-user, `~/.local`). |
| `tests/e2e.py` | End-to-end test in an isolated headless GNOME Shell (never touches your session). |
| `spike/` | Phase 0 research spike and the test harness. See `spike/FINDINGS.md`. |

## AppImages

Spot looks for AppImages in `~/Applications`, `~/AppImages`, `~/.local/bin`, Downloads and
Desktop. It doesn't look in subfolders. `SPOT_APPIMAGE_DIRS=dir1:dir2` replaces that list.

- **Detection:** by content (ELF + `AI\x02` magic), not by file name.
- **Name, description and icon:** read from the `.desktop` file and icon inside the image. The
  file is never run to get them. Results are cached in `~/.cache/spot/appimages`.
- **Updates:** new, removed or replaced files show up about a second later.
- **Duplicates:** an AppImage that already has a `.desktop` entry (AppImageLauncher, Gear Lever,
  the app's own "integrate" option) is listed once, as that entry.
- **Launching:** done through GIO, so the app gets focus. A file without the executable bit gets
  `u+x` when you launch it from Spot.
- **Old runtimes:** AppImages built with the old AppImageKit runtime need libfuse2. Debian 13 and
  Ubuntu 24.04+ don't install it, so Spot runs those with `APPIMAGE_EXTRACT_AND_RUN=1`. They start
  slower because they unpack to /tmp first. `sudo apt install libfuse2t64` makes them mount
  normally.

`spot --search <query>` shows the launch mode each AppImage will use.

## Ranking and history

Results rank by how they match, strongest first:

1. exact name
2. name prefix
3. words of the name ("code" → Visual Studio Code)
4. keywords
5. command name
6. generic name
7. initials ("vsc")
8. words in the description

Matched letters are shown in bold.

On top of that, Spot learns from what you launch:

- Apps you start often and recently get a small boost.
- An app you pick after typing something gets a large boost for that text and for shorter
  versions of it. After choosing "Calculator" for "calc" a few times, "ca" finds it first.

Both boosts fade over weeks. They can reorder similar matches but never push an app above an
exact name match.

The history is stored only on this machine, in `~/.local/share/spot/history.tsv`. Delete the
file to forget everything.

## Extras

| Type | Example | Enter does |
|---|---|---|
| Calculation | `2+3*4`, `sqrt(2)`, `2pi`, `10!`, `log(8, 2)`, `= 42` | copies the result |
| System action | `lock`, `log out`, `suspend`, `restart`, `shutdown` | same as the system menu: log out, restart and power off ask first |
| URL | `https://…`, `www.debian.org`, `gnome.org` | opens it in your browser |
| Path | `~/Downloads`, `/etc/fstab` (must exist) | opens it with the default app |
| Web search | anything (always the last row) | searches DuckDuckGo |

Settings go in `~/.config/spot/config.ini`. All are optional. Run `spot --quit` after editing,
and the next Super+Space starts Spot with the new settings.

```ini
[search]
calculator=true
system-actions=true
open-urls-and-paths=true
web-search=true
web-search-url=https://duckduckgo.com/?q=%s
```

## Install

### Debian 13, Ubuntu 25.04 or newer (GNOME 48)

```sh
make                       # needs: cargo, libgtk-4-dev, libadwaita-1-dev
scripts/build-deb.sh --no-build
sudo apt install ./target/deb/spot-launcher_0.1.0_amd64.deb
```

The package is `spot-launcher`. It contains:

- `/usr/bin/spot`
- the helper extension, in `/usr/share/gnome-shell/extensions/`
- a D-Bus service, so Spot starts on demand
- an app-grid entry, icons, AppStream metadata and a man page

It doesn't change anyone's settings on install. Set `DEBFULLNAME` and `DEBEMAIL` before building
to fill in the package's Maintainer field.

### Anything else, or your home folder only

```sh
make && sudo make install          # /usr/local
make && make install PREFIX=~/.local   # just you (scripts/install-dev.sh does this)
```

### First run (each user, once)

1. Log out and back in. GNOME on Wayland only discovers new extensions at login.
2. Open **Spot** from the app grid. This opens its settings window, which:
   - turns on the helper extension;
   - frees Super+Space if GNOME also uses it, usually for "switch input source". Only that one
     shortcut is changed. You can still switch layouts with Shift+Super+Space;
   - turns **Start at login** on or off. It's off by default. Without it, the first Super+Space
     after login takes about half a second, and keys you type during that time aren't lost.

   Nothing changes until you press the matching button.

To use a different shortcut:

```sh
gsettings --schemadir /usr/share/gnome-shell/extensions/spot-helper@spot.dev/schemas \
  set org.gnome.shell.extensions.spot-helper toggle "['<Super>s']"
```

Without GNOME Shell, bind `spot --toggle` to a key in your desktop's shortcut settings. You lose
type-ahead and window centring.

### Uninstall

```sh
sudo apt remove spot-launcher     # or: sudo make uninstall / scripts/uninstall-dev.sh
```

Your history, config and cache stay in `~/.local/share/spot`, `~/.config/spot` and
`~/.cache/spot`. If Spot freed Super+Space for you, restore GNOME's binding with
`gsettings reset org.gnome.desktop.wm.keybindings switch-input-source`.

## Develop

```sh
make check                               # unit tests + desktop file and AppStream validation
target/release/spot --search firefox     # rank against your installed apps, no UI
make && tests/e2e.py                     # full UI test in a headless, isolated GNOME Shell
tests/e2e.py --system-layout             # same, installed like the .deb (/usr layout via XDG_DATA_DIRS)
tests/e2e.py --first-run                 # package installed mid-session: the setup flow
tests/e2e.py --launch-appimages          # also launch the test AppImages (executes the official
                                         # AppImage runtimes downloaded into target/appimage-runtimes)
make extension-zip                       # extension zip in the extensions.gnome.org format
```

The e2e tests never touch your session. They run on a private D-Bus with private XDG folders and
check afterwards that your dconf database is unchanged. Web searches go to a test URL handler, and
system actions are never triggered.

D-Bus interface: `dev.spot.Spot` at `/dev/spot/Spot`, `org.gtk.Actions`. Actions:

- `toggle`, `show`, `hide`, `prefill(s)`, `accept`: the launcher
- `settings`, `free-shortcut`, `enable-extension`, `forget-history`: the settings window

Example:
`gdbus call --session --dest dev.spot.Spot --object-path /dev/spot/Spot --method org.gtk.Actions.Activate toggle [] {}`
