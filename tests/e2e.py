#!/usr/bin/env python3
"""End-to-end check of the real Spot build in an isolated headless GNOME Shell session.

Never touches your desktop: it reuses the Phase 0 harness (spike/harness), which runs
gnome-shell --headless on a private D-Bus with private XDG dirs and the keyfile GSettings backend.
Spot is installed into that session with scripts/install-dev.sh (overridden target dirs), the
helper extension is enabled there, and everything is driven by synthetic key/pointer events.

AppImage checks build real AppImages from the two official runtimes (the current static
type2-runtime and the old libfuse2 AppImageKit one), downloaded once into target/appimage-runtimes.
By default the runtimes are only read (indexing, icons, hot-add, de-duplication). Launching them,
which executes the downloaded runtimes, needs --launch-appimages.

--system-layout installs with `make install PREFIX=<outdir>/sysroot/usr` (the layout of the .deb)
and adds that to XDG_DATA_DIRS, instead of the per-user install: the extension, D-Bus service and
desktop file are then found the way a distro package's are.

--first-run installs the system layout *after* the shell started (what happens when you install
the .deb while logged in) and only checks the settings window's setup flow.

usage: tests/e2e.py [--launch-appimages] [--system-layout | --first-run] [outdir]   (build first: make)
"""
import json
import os
import shlex
import statistics
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HARNESS = os.path.join(ROOT, "spike", "harness")
sys.path.insert(0, HARNESS)
import shell  # noqa: E402
from shell import screenshot, sp, wait_for  # noqa: E402

EXT = os.path.join(ROOT, "extension", "spot-helper@spot.dev")
DUMMY = os.path.join(HARNESS, "dummy_app.py")
WM_CLASS = "dev.spot.Spot"
EDITOR = "Dummy Editor"
TARGET = "Launched Target"
WEB_HANDLER = "Web Handler"
RUNTIMES = os.path.join(ROOT, "target", "appimage-runtimes")
RUNTIME_URLS = {
    "new": "https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64",
    "old": "https://github.com/AppImage/AppImageKit/releases/download/continuous/runtime-x86_64",
}
# (file name, runtime, squashfs compression, display name, window title, icon colour)
APPIMAGES = [
    ("Fresh-Notes-2.1.0-x86_64.AppImage", "new", "zstd", "Fresh Notes", "Fresh Notes Window", 0x3584E4FF),
    ("Legacy Paint.AppImage", "old", "gzip", "Legacy Paint", "Legacy Paint Window", 0xE66100FF),
]
HOT = ("Hot-Added-1.0.AppImage", "new", "zstd", "Hot Added", "Hot Added Window", 0x26A269FF)


def runtime(kind):
    path = os.path.join(RUNTIMES, kind, "runtime-x86_64")
    if not os.path.exists(path):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        subprocess.run(["curl", "-fsSL", "-o", path, RUNTIME_URLS[kind]], check=True)
    return path


def build_appimage(dest_dir, spec, work):
    """AppDir (AppRun -> dummy GTK app, .desktop, icon, .DirIcon) -> squashfs -> runtime + squashfs.
    Nothing is executed here. Left without the executable bit, like a fresh browser download."""
    fname, kind, comp, name, title, colour = spec
    app_id = name.lower().replace(" ", "-")
    appdir = os.path.join(work, app_id + ".AppDir")
    subprocess.run(["rm", "-rf", appdir], check=True)
    os.makedirs(appdir)
    subprocess.run(["cp", DUMMY, appdir], check=True)
    with open(os.path.join(appdir, "AppRun"), "w") as f:
        f.write(f'#!/bin/sh\nHERE="$(dirname "$(readlink -f "$0")")"\nexec python3 "$HERE/dummy_app.py" "{title}" 900 600\n')
    os.chmod(os.path.join(appdir, "AppRun"), 0o755)
    with open(os.path.join(appdir, app_id + ".desktop"), "w") as f:
        f.write(f"[Desktop Entry]\nType=Application\nName={name}\nComment=Test AppImage ({kind} runtime)\n"
                f"Exec=AppRun\nIcon={app_id}\nCategories=Utility;\nKeywords=spot-e2e;\n")
    import gi
    gi.require_version("GdkPixbuf", "2.0")
    from gi.repository import GdkPixbuf
    pb = GdkPixbuf.Pixbuf.new(GdkPixbuf.Colorspace.RGB, True, 8, 128, 128)
    pb.fill(colour)
    pb.savev(os.path.join(appdir, app_id + ".png"), "png", [], [])
    os.symlink(app_id + ".png", os.path.join(appdir, ".DirIcon"))
    sq = os.path.join(work, app_id + ".squashfs")
    subprocess.run(["mksquashfs", appdir, sq, "-root-owned", "-noappend", "-comp", comp, "-quiet"], check=True,
                   stdout=subprocess.DEVNULL)
    out = os.path.join(dest_dir, fname)
    tmp = os.path.join(work, fname + ".part")
    with open(tmp, "wb") as o:
        for part in (runtime(kind), sq):
            with open(part, "rb") as i:
                o.write(i.read())
    os.chmod(tmp, 0o644)
    os.replace(tmp, out)  # appear atomically, as a browser's final rename does
    return out


class Session:
    def __init__(self, out, system_layout=False, first_run=False):
        self.out = out
        self.dir = os.path.join(out, "session")
        subprocess.run([os.path.join(HARNESS, "stop.sh"), self.dir], capture_output=True)
        # The shell loads the extension from the source tree before anything is installed, and
        # its settings schema must be compiled for that (the compiled file isn't in git).
        subprocess.run(["glib-compile-schemas", os.path.join(EXT, "schemas")], check=True)
        sysroot = os.path.join(out, "sysroot")
        install_sys = ["make", "-s", "-C", ROOT, "install", f"PREFIX={sysroot}/usr"]
        if system_layout or first_run:
            subprocess.run(["rm", "-rf", sysroot], check=True)
            os.makedirs(os.path.join(sysroot, "usr", "share"))
            os.environ["XDG_DATA_DIRS"] = f"{sysroot}/usr/share:" + os.environ.get("XDG_DATA_DIRS",
                                                                                   "/usr/local/share:/usr/share")
        if system_layout:
            # Installed before the shell starts: like a package, it is there at login.
            subprocess.run(install_sys, check=True, capture_output=True)
            env = dict(os.environ, ENABLE_EXTENSIONS=os.path.basename(EXT))
        elif first_run:
            env = dict(os.environ)
        else:
            env = dict(os.environ, EXTRA_EXTENSIONS=EXT)
        subprocess.run([os.path.join(HARNESS, "session.sh"), "start", self.dir], env=env, check=True,
                       capture_output=True)
        line = open(os.path.join(self.dir, "env.sh")).read().split(";")[0]
        for kv in shlex.split(line)[1:]:
            k, v = kv.split("=", 1)
            os.environ[k] = v
        os.environ.pop("DISPLAY", None)
        shell._bus = None
        self.trace = os.path.join(self.dir, "spot-trace.tsv")
        self.editorlog = os.path.join(self.dir, "logs", "editor.log")
        self.procs = []
        self.appimage_dir = os.path.join(self.dir, "AppImages")
        self.fixture_work = os.path.join(self.dir, "appimage-build")
        os.makedirs(self.appimage_dir)
        os.makedirs(self.fixture_work)
        self.appimages = {spec[3]: build_appimage(self.appimage_dir, spec, self.fixture_work) for spec in APPIMAGES}

        # What a user does after install: free Super+Space from the input-source switcher.
        for k in ("switch-input-source", "switch-input-source-backward"):
            subprocess.run(["gsettings", "set", "org.gnome.desktop.wm.keybindings", k, "[]"], check=True)
        if first_run:
            subprocess.run(install_sys, check=True, capture_output=True)  # "apt install" mid-session
        elif not system_layout:
            # Install exactly as a user would, but into the isolated session's dirs.
            inst = subprocess.run([os.path.join(ROOT, "scripts", "install-dev.sh"), "--no-build"],
                                  env=dict(os.environ, BINDIR=os.path.join(self.dir, "bin")),
                                  capture_output=True, text=True)
            assert inst.returncode == 0, inst.stderr
        # D-Bus-activated services inherit the bus's activation environment, not ours. A real GNOME
        # login pushes WAYLAND_DISPLAY & co. there; the headless test session doesn't, so do it here.
        subprocess.run(["dbus-update-activation-environment", f"SPOT_TRACE={self.trace}",
                        f"SPOT_APPIMAGE_DIRS={self.appimage_dir}", "WAYLAND_DISPLAY",
                        "XDG_SESSION_TYPE", "XDG_CURRENT_DESKTOP", "GDK_BACKEND", "NO_AT_BRIDGE"], check=True)
        with open(os.path.join(self.dir, "data", "applications", "spot-test-target.desktop"), "w") as f:
            f.write("[Desktop Entry]\nType=Application\nName=Spot Test Target\n"
                    f"Exec=python3 {DUMMY} \"{TARGET}\" 900 600\nIcon=applications-science\n")
        # Two look-alike apps for the history check; they start nothing visible.
        for n in ("Alpha", "Beta"):
            with open(os.path.join(self.dir, "data", "applications", f"spot-learn-{n.lower()}.desktop"), "w") as f:
                f.write(f"[Desktop Entry]\nType=Application\nName=Spot Learn {n}\nExec=true\n")
        # Web search goes to a private URL scheme handled by a dummy window, so the test never
        # starts your real browser (which would run with your real profile).
        with open(os.path.join(self.dir, "data", "applications", "spot-web-handler.desktop"), "w") as f:
            f.write(f"[Desktop Entry]\nType=Application\nName=Spot Web Handler\nNoDisplay=true\n"
                    f"Exec=python3 {DUMMY} \"{WEB_HANDLER}\" 900 600 %u\nMimeType=x-scheme-handler/spot-test;\n")
        with open(os.path.join(self.dir, "config", "mimeapps.list"), "a") as f:
            f.write("[Default Applications]\nx-scheme-handler/spot-test=spot-web-handler.desktop\n")
        os.makedirs(os.path.join(self.dir, "config", "spot"), exist_ok=True)
        with open(os.path.join(self.dir, "config", "spot", "config.ini"), "w") as f:
            f.write("[search]\nweb-search-url=spot-test://search?q=%s\n")
        self.history = os.path.join(self.dir, "data", "spot", "history.tsv")
        time.sleep(0.6)  # let file monitors (gsettings keyfile, app dirs) settle

    def press(self, combo=("Super_L", "space")):
        return sp(f"spike.key({json.dumps(list(combo))})")

    def focus(self):
        return sp("spike.focus()") or {}

    def launcher_focused(self):
        return self.focus().get("wmclass") == WM_CLASS

    def launcher_window(self):
        return next((w for w in sp("spike.windows()") if (w or {}).get("wmclass") == WM_CLASS and not w["hidden"]),
                    None)

    def events(self):
        if not os.path.exists(self.trace):
            return []
        return [tuple((l.rstrip("\n").split("\t", 1) + [""])[:2]) for l in open(self.trace) if l.strip()]

    def last_text(self):
        texts = [v for e, v in self.events() if e == "text"]
        return texts[-1] if texts else None

    def editor_text(self):
        lines = [l for l in open(self.editorlog).read().splitlines() if l.startswith("TEXT ")]
        return eval(lines[-1][5:]) if lines else None  # repr() written by our own dummy app

    def results(self):
        """Result list of the last refresh, as ids (`calc:…`, `open:…`, `web:…` for extras)."""
        r = [v for e, v in self.events() if e == "results"]
        return r[-1].split(";") if r and r[-1] else []

    def show_and_type(self, query):
        self.to_editor()
        self.press()
        wait_for(self.launcher_focused, 3)
        time.sleep(0.15)
        sp(f"spike.type({json.dumps(query)})")
        return wait_for(lambda: self.last_text() == query, 3)

    def settings_state(self):
        r = [v for e, v in self.events() if e == "settings"]
        return dict(kv.split("=", 1) for kv in r[-1].split(";")) if r else None

    def action(self, name):
        subprocess.run(["gdbus", "call", "--session", "--dest", WM_CLASS, "--object-path", "/dev/spot/Spot",
                        "--method", "org.gtk.Actions.Activate", name, "[]", "{}"], check=True, capture_output=True)

    def appimage_scans(self):
        return [v.split(";") if v else [] for e, v in self.events() if e == "appimages"]

    def to_editor(self):
        sp(f'spike.activate("{EDITOR}")')
        return wait_for(lambda: self.focus().get("title") == EDITOR, 3)

    def start_editor(self):
        self.procs.append(shell.spawn(["python3", DUMMY, EDITOR, "1400", "900"], log=self.editorlog))
        assert wait_for(lambda: self.focus().get("title") == EDITOR, 10), "editor never focused"

    def stop(self):
        for p in self.procs:
            try:
                os.killpg(p.pid, 15)
            except ProcessLookupError:
                pass
        r = subprocess.run([os.path.join(HARNESS, "stop.sh"), self.dir], capture_output=True, text=True)
        return r.stdout.strip()


def ms(a, b):
    return None if a is None or b is None else round((b - a) / 1000, 1)


def first(events, ev, since, wmclass=None):
    for e in events:
        w = e.get("win") if isinstance(e.get("win"), dict) else e
        if e["ev"] == ev and e["t"] >= since and (wmclass is None or (w or {}).get("wmclass") == wmclass):
            return e
    return None


def shell_events(since):
    return [e for e in sp("spike.log") if e["t"] >= since]


def check_coldstart_typeahead_enter(s):
    """Nothing running. Super+Space, type 'spot test' and Enter immediately -> D-Bus activation
    starts Spot, the helper buffers the keys, and the top hit (the test target app) is launched."""
    assert not os.path.exists(s.trace)
    t0 = s.press()
    sp('spike.type("spot test")')
    sp('spike.key(["Return"])')
    launched = wait_for(lambda: s.focus().get("title") == TARGET, 10)
    ev = shell_events(t0)
    ff = first(ev, "first-frame", t0, WM_CLASS)
    fo = first(ev, "focus", t0, WM_CLASS)
    return {"target_launched_and_focused": bool(launched), "key_to_pixels_ms": ms(t0, ff and ff["t"]),
            "key_to_focus_ms": ms(t0, fo and fo["t"]), "text_seen_by_app": s.last_text(),
            "app_events": [e for e, _ in s.events()][:8],
            "editor_unchanged": s.editor_text() == f"{EDITOR}: pretend the user is typing here.\n",
            "helper_errors": [e for e in ev if e["ev"] == "helper-dbus-error"]}


def check_warm_latency(s, n=10):
    s.to_editor()
    rows = []
    for i in range(n):
        t0 = s.press()
        ok = wait_for(s.launcher_focused, 4)
        time.sleep(0.2)
        ev = shell_events(t0)
        ff = first(ev, "first-frame", t0, WM_CLASS)
        fo = first(ev, "focus", t0, WM_CLASS)
        sp('spike.key(["Escape"])')
        back = wait_for(lambda: s.focus().get("title") == EDITOR, 3)
        rows.append({"focused": bool(ok), "pixels": ms(t0, ff and ff["t"]), "focus": ms(t0, fo and fo["t"]),
                     "esc_returned_focus": bool(back),
                     "ready_notifications": sum(1 for e in ev if e["ev"] in ("demands-attention", "urgent"))})
        time.sleep(0.2)

    def med(k):
        v = [r[k] for r in rows if r[k] is not None]
        return statistics.median(v) if v else None
    return {"cycles": n, "all_focused": all(r["focused"] for r in rows),
            "esc_always_returned_focus": all(r["esc_returned_focus"] for r in rows),
            "ready_notifications": sum(r["ready_notifications"] for r in rows),
            "median_key_to_pixels_ms": med("pixels"), "median_key_to_focus_ms": med("focus")}


def check_typeahead_and_placement(s):
    s.to_editor()
    s.press()
    sp('spike.type("calc")')  # 0 ms after the shortcut
    ok = wait_for(lambda: s.last_text() == "calc" and s.launcher_focused(), 3)
    time.sleep(0.3)
    w, mon = s.launcher_window(), sp("spike.monitors()")[0]
    shot = screenshot(os.path.join(s.out, "spot-calc.png"))
    result = {"typed_at_0ms_arrived": bool(ok), "text": s.last_text(),
              "editor_unchanged": s.editor_text() == f"{EDITOR}: pretend the user is typing here.\n",
              "window": w, "centered_x": abs(w["x"] + w["w"] / 2 - (mon["x"] + mon["w"] / 2)) <= 2,
              "top_at_22pct": abs(w["y"] - (mon["y"] + round(mon["h"] * 0.22))) <= 2, "screenshot": shot}
    sp('spike.key(["Escape"])')
    wait_for(lambda: s.focus().get("title") == EDITOR, 3)
    return result


def check_keyboard_launch(s):
    s.to_editor()
    s.press()
    wait_for(s.launcher_focused, 3)
    time.sleep(0.2)
    sp('spike.type("Spot Test Target")')
    time.sleep(0.3)
    sp('spike.key(["Return"])')
    ok = wait_for(lambda: s.focus().get("title") == TARGET, 8)
    return {"launched_and_focused": bool(ok), "launcher_hidden": s.launcher_window() is None}


def check_overview(s):
    s.to_editor()
    sp("spike.overview(true)")
    wait_for(lambda: sp("spike.overviewVisible()"), 3)
    time.sleep(0.4)
    s.press()
    ok = wait_for(s.launcher_focused, 3)
    r = {"works_from_overview": bool(ok), "overview_closed": not sp("spike.overviewVisible()")}
    sp('spike.key(["Escape"])')
    time.sleep(0.3)
    return r


def check_toggle_and_click_away(s):
    s.to_editor()
    s.press()
    shown = wait_for(s.launcher_focused, 3)
    time.sleep(0.2)
    s.press()  # same shortcut again hides it
    toggled_off = wait_for(lambda: s.launcher_window() is None, 3)
    s.press()
    wait_for(s.launcher_focused, 3)
    time.sleep(0.2)
    ed = next(w for w in sp("spike.windows()") if (w or {}).get("title") == EDITOR)
    sp(f"spike.pointerTo({ed['x'] + 40}, {ed['y'] + ed['h'] - 40})")
    sp("spike.click()")
    dismissed = wait_for(lambda: s.launcher_window() is None, 3)
    return {"shown": bool(shown), "second_press_hides": bool(toggled_off),
            "click_outside_dismisses": bool(dismissed), "focus_now": s.focus().get("title")}


def search_and_launch(s, query, title):
    s.to_editor()
    s.press()
    wait_for(s.launcher_focused, 3)
    sp(f"spike.type({json.dumps(query)})")
    time.sleep(0.3)
    sp('spike.key(["Return"])')
    return wait_for(lambda: s.focus().get("title") == title, 20)


def check_appimages_indexed(s):
    scans = wait_for(lambda: s.appimage_scans(), 5)
    s.to_editor()
    s.press()
    wait_for(s.launcher_focused, 3)
    sp('spike.type("e2e")')  # keyword shared by the test AppImages
    time.sleep(0.5)
    shot = screenshot(os.path.join(s.out, "spot-appimage.png"))
    sp('spike.key(["Escape"])')
    wait_for(lambda: s.focus().get("title") == EDITOR, 3)
    return {"found": sorted(scans[-1]) if scans else None, "screenshot": shot}


def check_appimage_launch_new_runtime(s):
    ok = search_and_launch(s, "fresh notes", "Fresh Notes Window")
    return {"launched_and_focused": bool(ok), "made_executable": os.access(s.appimages["Fresh Notes"], os.X_OK)}


def check_appimage_launch_old_runtime(s):
    """Old AppImageKit runtime + no libfuse2 on this machine: Spot must use extract-and-run."""
    ok = search_and_launch(s, "legacy paint", "Legacy Paint Window")
    return {"launched_and_focused": bool(ok), "made_executable": os.access(s.appimages["Legacy Paint"], os.X_OK)}


def check_appimage_hot_add_and_integration(s):
    before = len(s.appimage_scans())
    build_appimage(s.appimage_dir, HOT, s.fixture_work)
    added = wait_for(lambda: len(s.appimage_scans()) > before and "Hot Added" in s.appimage_scans()[-1], 8)
    # A desktop entry pointing at an AppImage (what AppImageLauncher/Gear Lever create) wins:
    # the AppImage must no longer be listed separately.
    # Integrators make the file executable; GIO ignores entries whose Exec isn't executable.
    os.chmod(s.appimages["Legacy Paint"], 0o755)
    time.sleep(1.5)  # let the chmod-triggered rescan finish first
    n = len(s.appimage_scans())
    with open(os.path.join(s.dir, "data", "applications", "legacy-paint-integrated.desktop"), "w") as f:
        f.write("[Desktop Entry]\nType=Application\nName=Legacy Paint\n"
                f"Exec=\"{s.appimages['Legacy Paint']}\"\n")
    deduped = wait_for(lambda: len(s.appimage_scans()) > n and "Legacy Paint" not in s.appimage_scans()[-1], 8)
    return {"hot_added_indexed": bool(added), "integrated_appimage_deduplicated": bool(deduped),
            "last_scan": s.appimage_scans()[-1] if s.appimage_scans() else None}


def check_history_learns_choice(s):
    """Pick the 2nd result for the same query three times -> it becomes the 1st."""
    s.show_and_type("spot learn")
    time.sleep(0.2)
    before = s.results()[:2]
    target = before[1]
    sp('spike.key(["Escape"])')
    for _ in range(3):
        s.show_and_type("spot learn")
        time.sleep(0.2)
        for _ in range(s.results().index(target)):
            sp('spike.key(["Down"])')
        sp('spike.key(["Return"])')
        wait_for(lambda: s.launcher_window() is None, 3)
        time.sleep(0.2)
    s.show_and_type("spot learn")
    time.sleep(0.2)
    after = s.results()[:2]
    s.show_and_type("spot l")  # learned for prefixes too
    time.sleep(0.2)
    prefix = s.results()[:1]
    sp('spike.key(["Escape"])')
    wait_for(lambda: s.focus().get("title") == EDITOR, 3)
    launches = [v for e, v in s.events() if e == "launch"][-3:]
    return {"before": before, "after": after, "prefix_top": prefix, "launched": launches,
            "learned": after[:1] == [target] and prefix == [target] and launches == [target] * 3,
            "history_file_in_test_session": os.path.exists(s.history)}


def check_extras_in_results(s):
    """Calculator, system actions and URL/path answers show up (nothing is activated here)."""
    out = {}
    for q in ("6*7", "lock screen", "gnome.org", "~"):
        s.show_and_type(q)
        time.sleep(0.2)
        out[q] = s.results()[:2]
        if q == "6*7":
            out["screenshot"] = screenshot(os.path.join(s.out, "spot-calc-answer.png"))
        sp('spike.key(["Escape"])')
    wait_for(lambda: s.focus().get("title") == EDITOR, 3)
    return {"results": out, "ok": out["6*7"][:1] == ["calc:42"] and out["lock screen"][:1] == ["spot-action:lock"]
            and out["gnome.org"][:1] == ["open:https://gnome.org"] and out["~"][:1][0].startswith("open:file://")}


def check_calculator_copies(s):
    """Enter on the answer copies it; pasting into the editor proves the clipboard survives hiding."""
    s.show_and_type("6*7")
    time.sleep(0.2)
    sp('spike.key(["Return"])')
    hidden = wait_for(lambda: s.launcher_window() is None, 3)
    back = wait_for(lambda: s.focus().get("title") == EDITOR, 3)
    time.sleep(0.2)
    sp('spike.key(["Control_L", "End"])')
    sp('spike.key(["Control_L", "v"])')
    pasted = wait_for(lambda: (s.editor_text() or "").endswith("42"), 3)
    return {"hidden": bool(hidden), "editor_refocused": bool(back), "pasted_42": bool(pasted),
            "copy_events": [v for e, v in s.events() if e == "copy"]}


def check_web_search(s):
    s.show_and_type("rust gtk4")
    time.sleep(0.2)
    last = s.results()[-1:]
    sp('spike.key(["Up"])')  # wraps to the last row: the web search
    sp('spike.key(["Return"])')
    ok = wait_for(lambda: s.focus().get("title") == WEB_HANDLER, 10)
    opened = [v for e, v in s.events() if e == "open"][-1:]
    return {"last_row": last, "opened": opened, "handler_focused": bool(ok)}


def check_settings_window(s):
    """'Spot' in Spot opens its settings (D-Bus Activate path, like the app grid), which reports
    the extension and spots a GNOME shortcut that steals Super+Space; its fix removes only that."""
    n = len([e for e, _ in s.events() if e == "settings"])
    s.show_and_type("spot")
    time.sleep(0.2)
    top = s.results()[:1]
    sp('spike.key(["Return"])')
    opened = wait_for(lambda: s.focus().get("title") == "Spot Settings", 5)
    time.sleep(0.4)
    first_state = s.settings_state() if len([e for e, _ in s.events() if e == "settings"]) > n else None
    shot = screenshot(os.path.join(s.out, "spot-settings.png"))
    # GNOME's default: Super+Space switches keyboard layouts.
    gs = ["gsettings", "set", "org.gnome.desktop.wm.keybindings", "switch-input-source", "['<Super>space', 'XF86Keyboard']"]
    subprocess.run(gs, check=True)
    time.sleep(0.3)
    s.action("settings")
    time.sleep(0.4)
    with_conflict = s.settings_state()
    shot2 = screenshot(os.path.join(s.out, "spot-settings-conflict.png"))
    s.action("free-shortcut")
    time.sleep(0.4)
    fixed = s.settings_state()
    now = subprocess.run(["gsettings", "get", "org.gnome.desktop.wm.keybindings", "switch-input-source"],
                         capture_output=True, text=True).stdout.strip()
    return {"top_result": top, "opened_from_launcher": bool(opened), "state": first_state,
            "conflict_found": with_conflict and with_conflict.get("conflicts"),
            "after_fix": fixed and fixed.get("conflicts"), "switch_input_source_now": now,
            "screenshots": [shot, shot2],
            "ok": bool(opened) and top == ["dev.spot.Spot.desktop"] and (first_state or {}).get("extension") == "active"
            and with_conflict.get("conflicts") == "switch-input-source" and fixed.get("conflicts") == ""
            and now == "['XF86Keyboard']"}


def check_first_run_setup(s):
    """Package installed mid-session: the shell hasn't loaded the extension. Opening Spot (app grid
    = D-Bus Activate) shows settings; 'Turn On' must queue the extension for the next login."""
    subprocess.run(["gdbus", "call", "--session", "--dest", WM_CLASS, "--object-path", "/dev/spot/Spot",
                    "--method", "org.freedesktop.Application.Activate", "{}"], check=True, capture_output=True)
    opened = wait_for(lambda: s.focus().get("title") == "Spot Settings", 10)
    time.sleep(0.4)
    before = s.settings_state()
    shot = screenshot(os.path.join(s.out, "spot-first-run.png"))
    s.action("enable-extension")
    time.sleep(0.8)
    after = s.settings_state()
    enabled = subprocess.run(["gsettings", "get", "org.gnome.shell", "enabled-extensions"],
                             capture_output=True, text=True).stdout.strip()
    shot2 = screenshot(os.path.join(s.out, "spot-first-run-after.png"))
    return {"settings_opened": bool(opened), "before": before, "after": after, "enabled_extensions": enabled,
            "notifications": sp("spike.notifications()"),
            "screenshots": [shot, shot2],
            "ok": bool(opened) and (before or {}).get("extension") == "not-loaded"
            and (after or {}).get("extension") == "pending-login" and "spot-helper@spot.dev" in enabled}


def main():
    args = sys.argv[1:]
    launch_appimages = "--launch-appimages" in args
    system_layout = "--system-layout" in args
    first_run = "--first-run" in args
    args = [a for a in args if a not in ("--launch-appimages", "--system-layout", "--first-run")]
    out = os.path.abspath(args[0] if args else os.path.join(ROOT, "target", "e2e"))
    os.makedirs(out, exist_ok=True)
    if not os.path.exists(os.path.join(ROOT, "target", "release", "spot")):
        sys.exit("build first: make")
    s = Session(out, system_layout, first_run)
    results = {}
    try:
        s.start_editor()
        for name, fn in [("first_run_setup", check_first_run_setup)] if first_run else [("coldstart_typeahead_enter", check_coldstart_typeahead_enter),
                         ("warm_latency", check_warm_latency),
                         ("typeahead_and_placement", check_typeahead_and_placement),
                         ("keyboard_launch", check_keyboard_launch),
                         ("overview", check_overview),
                         ("toggle_and_click_away", check_toggle_and_click_away),
                         ("appimages_indexed", check_appimages_indexed),
                         *([("appimage_new_runtime", check_appimage_launch_new_runtime),
                            ("appimage_old_runtime_no_libfuse2", check_appimage_launch_old_runtime)]
                           if launch_appimages else []),
                         ("appimage_hot_add_and_integration", check_appimage_hot_add_and_integration),
                         ("history_learns_choice", check_history_learns_choice),
                         ("extras_in_results", check_extras_in_results),
                         ("calculator_copies", check_calculator_copies),
                         ("web_search", check_web_search),
                         ("settings_window", check_settings_window)]:
            try:
                results[name] = fn(s)
            except Exception as e:  # keep going; report what failed
                results[name] = {"error": repr(e)}
            print(name, json.dumps(results[name]), flush=True)
    finally:
        results["stop"] = s.stop()
        print(results["stop"])
    json.dump(results, open(os.path.join(out, "results.json"), "w"), indent=2)


if __name__ == "__main__":
    main()
