# Phase 0 spike: Spotlight-style launcher on GNOME 48 / Wayland

**Date:** 2026-11-25. **Target:** GNOME Shell 48.7, mutter 48.7, GTK 4.18.6, libadwaita 1.7, Debian 13.

## Decision

**Use a hybrid design.** The launcher is a normal GTK4/libadwaita app (Rust, `gtk4-rs`) that runs
in the background. A *tiny* GNOME Shell extension (~170 lines of GJS) does only the four things
an app can't do on GNOME Wayland:

1. Own the hotkey inside the compositor. It's instant and also works in the Activities overview.
2. Grab the keyboard the instant the hotkey fires and buffer type-ahead.
3. Center the window on the pointer's monitor (upper third, like Spotlight) and keep it above other windows.
4. Pass the buffered text to the app over D-Bus (`org.gtk.Actions`), then release the grab.

All search, indexing, AppImage handling and UI stay in the Rust app. If the extension is missing
or broken (another desktop, or a GNOME upgrade that breaks it), the app falls back to the
**GlobalShortcuts portal**, then to a **gsd custom shortcut**. It still works, just without the
type-ahead and placement guarantees.

## How it was tested

Everything ran in an **isolated headless GNOME Shell 48** (`harness/session.sh`) with its own
D-Bus, dconf (keyfile backend), runtime dir and Wayland socket. Input was injected with Clutter
virtual devices, so it goes through the same pipeline as a real keyboard. A probe extension
recorded window/focus/first-frame events on the compositor's monotonic clock, which is the same
clock the app logs with, so the latencies are end to end. Each variant × scenario ran in a fresh
session. Raw data: `results/results.json`. Screenshots: `results/screenshots/`.

Variants:

| Variant | UI | Trigger |
|---|---|---|
| `gtk-py` | GTK window (hidden/shown) | gsd custom shortcut → `spot_spike.py --toggle` (Python client process) |
| `gtk-dbus` | GTK window (hidden/shown) | gsd custom shortcut → `gdbus call … org.gtk.Actions.Activate toggle` |
| `gtk-recreate` | GTK window destroyed and recreated on every show | same as `gtk-dbus` |
| `shell-ext` | Overlay drawn by a GNOME Shell extension (St widgets) | compositor keybinding |
| **`hybrid`** | GTK window + helper extension | compositor keybinding in the helper |

## Results

Latency is the median over 12 show/hide cycles, from keypress to the launcher having keyboard
focus and pixels on screen.

| | gtk-py | gtk-dbus | gtk-recreate | **hybrid** | shell-ext |
|---|---|---|---|---|---|
| Key → focused & drawn (median) | 117 ms | 31 ms | 40 ms | **18 ms** | 8 ms |
| First show after app start | 215 ms | 124 ms | 182 ms | 121 ms | 24 ms |
| "App is ready" notifications | 0 | 0 | 0 | **0** | 0 |
| Type-ahead safe if first key arrives ≥ | 150 ms | 30 ms | 60 ms | **0 ms** | 0 ms |
| Placement, `center-new-windows=false` | top-left (0,32) | top-left | – | **centered, upper third** | centered |
| Placement, `center-new-windows=true` | dead center | dead center | dead center | **centered, upper third** | centered |
| Opens on the pointer's monitor | ✅ | ✅ | – | ✅ | ✅ |
| Works from the Activities overview | ❌ thumbnail, no focus | ❌ | – | ✅ closes overview | ✅ |
| Above a fullscreen window | ✅ | ✅ | – | ✅ | ✅ |
| Dismissed when you click away | ✅ | ✅ | – | ✅ | ✅ |
| Launched app gets focus | ✅ | ✅ | – | ✅ | ✅ |
| Cold start (no resident process) | 330–580 ms | n/a | – | n/a | n/a |

What I learned:

- **Focus isn't a problem on GNOME 48.** A hidden GTK window re-shown with *no* activation token
  always got focus, and mutter never showed an "is ready" notification. gsd custom shortcuts and
  the portal both deliver **no** activation token. It works anyway because GTK4 destroys the
  `xdg_toplevel` on hide, so every show is a *new* window to mutter, and new windows get focus
  under `focus-new-windows='smart'`.
- **Type-ahead is the real gap.** Spotlight users press the hotkey and type straight away. With
  plain GTK, any key that arrives before the window maps goes to the previous app: `calc` ended
  up in the editor. The faster the trigger, the shorter that window (Python client 150 ms, `gdbus`
  30 ms), but it never reaches zero. Only a compositor-side grab closes it.
- **Keep the window, don't recreate it.** Recreating was slower (40 vs 31 ms) and lost more
  type-ahead (60 vs 30 ms). `realize()` at startup didn't remove the ~120 ms first-show cost. That
  cost is the first draw (icons, fonts, list rows), so in Phase 1, warm it up or use a lazily
  populated `GtkListView`.
- **The client process matters.** Starting a process per toggle (`spot --toggle`) costs ~100 ms
  in Python. Trigger through D-Bus actions instead (`org.gtk.Actions.Activate`), which takes 1–9 ms.
- **The hybrid has no flicker.** The helper moves the window before mutter draws it: its first
  frame is already at the target position (620,238).
- **GtkSearchEntry swallows Escape** (`stop-search`). Esc-to-close needs a key controller in the
  **capture** phase.

### Other findings

- **No layer-shell on GNOME.** `tools/wl-globals` against your real session lists 37 globals and
  no `zwlr_layer_shell_v1` (`results/wayland-globals-real-session.txt`). It does offer
  `xdg_activation_v1`, `xdg_wm_dialog_v1` and `zwp_keyboard_shortcuts_inhibit_manager_v1`, and no
  blur protocol, so a translucent or blurred background can only be drawn in the extension, not
  by the app.
- **The GlobalShortcuts portal (v1) works for non-Flatpak apps:**
  - The app must ship an installed `dev.spot.Spike.desktop`. Without it,
    `org.freedesktop.host.portal.Registry.Register` fails with "App info not found", and
    `CreateSession` then fails too.
  - The first bind shows a one-time GNOME Settings dialog: "Add Keyboard Shortcuts →
    Toggle Spot launcher → Super+Space" (`screenshots/portal-consent-dialog.png`).
  - `Activated` arrives **~2 ms** after the keypress, with options `{}`, so **no activation
    token**.
  - Quirk: `BindShortcuts` returned response `2` even though the binding was applied and fired.
  - Not tested: whether the binding persists across logins without asking again.
- **Hotkey:** GNOME binds `<Super>space` to `switch-input-source`. You have one input source
  (`us`), so you can clear that binding (Settings → Keyboard → Shortcuts → Typing) without losing
  anything. Every route needs this.

## What changes in the plan (Phase 1 onward)

- **App shape:** one Rust binary with `adw::Application` (unique instance), exporting the
  `show` / `hide` / `toggle` / `prefill(s)` actions; plus a D-Bus `.service` and a `.desktop`
  file, so cold start via D-Bus activation and portal registration both work. Pre-build the
  window and hide/show it; never recreate it.
- **New component, `spot-helper` GNOME Shell extension.** Port `ext/spot-helper@spot.local`
  roughly as is. It uses only stable, long-lived APIs (`Main.wm.addKeybinding`, `Main.pushModal`,
  `MetaWindow.move_frame` / `make_above`, `Gio.DBus`), which keeps GNOME-upgrade breakage to a
  minimum. Bump `shell-version` each release.
- **Trigger fallback chain:** helper extension → GlobalShortcuts portal → print instructions for
  a gsd custom shortcut that runs a `gdbus` command.
- **Install dev packages before Phase 1:**
  `sudo apt install libgtk-4-dev libadwaita-1-dev pkgconf`. This machine has neither the
  headers nor pkg-config. The spike used PyGObject, which runs the same libgtk/GApplication code
  paths.
- **Install `libfuse2t64`** for older AppImages (from the original plan).

## Limits of this spike

- Headless virtual monitors: latencies don't include display scanout. A real 60 Hz panel adds
  up to one frame.
- Python stood in for the GTK client, so the Rust numbers should be equal or better. The UI
  mechanics are identical (same libgtk).
- Only GNOME 48.7 was tested. Re-run `harness/experiments.py` on each GNOME upgrade: it's cheap.

## Reproduce

```sh
cd spot/spike/harness
python3 experiments.py <outdir>                  # all variants × scenarios (~6 min)
python3 experiments.py <outdir> hybrid typeahead # a subset
```

`session.sh` checks that your real `~/.config/dconf/user` is byte-identical before and after
every run, and `stop.sh` reports it.
