#!/usr/bin/env python3
"""Phase 0 experiment runner. Each (variant, scenario) runs in a fresh isolated headless
GNOME Shell session; results go to <out>/results.json plus screenshots.

Variants
  gtk-py      GTK4 window, gsd custom shortcut runs `spot_spike.py --toggle` (python client)
  gtk-dbus    GTK4 window, gsd custom shortcut runs `gdbus call ... org.gtk.Actions.Activate toggle`
  gtk-recreate same as gtk-dbus but the window is destroyed/recreated on every show
  shell-ext   overlay drawn by a GNOME Shell extension, keybinding owned by the compositor
  hybrid      GTK4 window + tiny helper extension (hotkey, type-ahead buffer, placement)

usage: experiments.py <outdir> [variant ...]
"""
import json
import os
import re
import shlex
import statistics
import subprocess
import sys
import time

import shell
from shell import screenshot, sp, wait_for

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
APP = os.path.join(ROOT, "app", "spot_spike.py")
DUMMY = os.path.join(HERE, "dummy_app.py")
EXT = os.path.join(ROOT, "ext", "spot-overlay@spot.local")
HELPER = os.path.join(ROOT, "ext", "spot-helper@spot.local")
GDBUS_TOGGLE = ("gdbus call --session --dest dev.spot.Spike --object-path /dev/spot/Spike "
                "--method org.gtk.Actions.Activate toggle [] {}")
SHORTCUT = ["Super_L", "space"]
EDITOR = "Dummy Editor"


class Session:
    def __init__(self, out, name, variant, monitors=("1920x1080",), center=False):
        self.dir = os.path.join(out, "sessions", name)
        self.variant = variant
        self.is_ext = variant == "shell-ext"
        uses_ext = variant in ("shell-ext", "hybrid")
        env = dict(os.environ, CENTER_NEW_WINDOWS="true" if center else "false",
                   WITH_GSD="0" if uses_ext else "1",
                   EXTRA_EXTENSIONS={"shell-ext": EXT, "hybrid": HELPER}.get(variant, ""))
        subprocess.run([os.path.join(HERE, "stop.sh"), self.dir], capture_output=True)
        subprocess.run([os.path.join(HERE, "session.sh"), "start", self.dir, *monitors], env=env, check=True,
                       capture_output=True)
        line = open(os.path.join(self.dir, "env.sh")).read().split(";")[0]
        for kv in shlex.split(line)[1:]:
            k, v = kv.split("=", 1)
            os.environ[k] = v
        os.environ.pop("DISPLAY", None)
        shell._bus = None
        self.applog = os.path.join(self.dir, "spike.jsonl")
        self.editorlog = os.path.join(self.dir, "logs", "editor.log")
        self.procs = []
        if not uses_ext:
            self._shortcut(GDBUS_TOGGLE if variant in ("gtk-dbus", "gtk-recreate") else f"{APP} --toggle")
        self._desktop_file()

    def gs(self, *args):
        subprocess.run(["gsettings", *args], check=True)

    def _shortcut(self, command):
        k = "org.gnome.settings-daemon.plugins.media-keys"
        p = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/spot/"
        self.gs("set", "org.gnome.desktop.wm.keybindings", "switch-input-source", "[]")
        self.gs("set", "org.gnome.desktop.wm.keybindings", "switch-input-source-backward", "[]")
        self.gs("set", f"{k}.custom-keybinding:{p}", "name", "Spot")
        self.gs("set", f"{k}.custom-keybinding:{p}", "command", command)
        self.gs("set", f"{k}.custom-keybinding:{p}", "binding", "<Super>space")
        self.gs("set", k, "custom-keybindings", f"['{p}']")
        time.sleep(0.5)  # gsd picks up keyfile changes via file monitor

    def _desktop_file(self):
        with open(os.path.join(self.dir, "data", "applications", "spot-test-target.desktop"), "w") as f:
            f.write("[Desktop Entry]\nType=Application\nName=Spot Test Target\n"
                    f"Exec=python3 {DUMMY} \"Launched Target\" 900 600\nIcon=applications-science\n")

    def spawn(self, argv, log):
        p = shell.spawn(argv, log=log)
        self.procs.append(p)
        return p

    def start_editor(self, size=(1400, 900)):
        self.spawn(["python3", DUMMY, EDITOR, str(size[0]), str(size[1])], self.editorlog)
        assert wait_for(lambda: (sp("spike.focus()") or {}).get("title") == EDITOR, 10), "editor never focused"

    def start_app(self, strategy=None):
        if self.is_ext:
            return
        strategy = strategy or ("recreate" if self.variant == "gtk-recreate" else "hide")
        self.spawn([APP, "--strategy", strategy, "--log", self.applog], os.path.join(self.dir, "logs", "app.log"))
        assert wait_for(lambda: os.path.exists(self.applog) and "primary-start" in open(self.applog).read(), 10)
        time.sleep(0.3)

    # --- observation -------------------------------------------------------------------------
    def launcher_focused(self):
        if self.is_ext:
            return sp("globalThis.spotOverlay.isOpen && global.stage.key_focus === globalThis.spotOverlay._entry.clutter_text")
        f = sp("spike.focus()") or {}
        return f.get("wmclass") == "dev.spot.Spike"

    def launcher_open(self):
        if self.is_ext:
            return sp("globalThis.spotOverlay.isOpen")
        return any((w or {}).get("wmclass") == "dev.spot.Spike" and not w.get("hidden") for w in sp("spike.windows()"))

    def launcher_rect(self):
        if self.is_ext:
            return sp("(() => { const o = globalThis.spotOverlay; const [x, y] = o._panel.get_transformed_position();"
                      " return {x: Math.round(x), y: Math.round(y), w: o._panel.width, h: o._panel.height,"
                      " monitor: spike.Main.layoutManager.findIndexForActor(o._panel)}; })()")
        return next((w for w in sp("spike.windows()") if (w or {}).get("wmclass") == "dev.spot.Spike"), None)

    def launcher_text(self):
        if self.is_ext:
            return sp("globalThis.spotOverlay.query().text")
        texts = [e for e in self.app_events() if e["ev"] == "text"]
        return texts[-1]["text"] if texts else ""

    def editor_text(self):
        lines = [l for l in open(self.editorlog).read().splitlines() if l.startswith("TEXT ")]
        return eval(lines[-1][5:]) if lines else None  # repr() of a str, written by our own dummy app

    def app_events(self):
        if not os.path.exists(self.applog):
            return []
        return [json.loads(l) for l in open(self.applog) if l.strip()]

    def shell_events(self, since):
        return [e for e in sp("spike.log") if e["t"] >= since]

    def press(self, combo=SHORTCUT):
        return sp(f"spike.key({json.dumps(combo)})")

    def stop(self):
        for p in self.procs:
            try:
                os.killpg(p.pid, 15)
            except ProcessLookupError:
                pass
        r = subprocess.run([os.path.join(HERE, "stop.sh"), self.dir], capture_output=True, text=True)
        return r.stdout.strip()


def first(events, ev, since=0, **match):
    for e in events:
        if e["ev"] == ev and e["t"] >= since and all(e.get(k) == v or (isinstance(e.get("win"), dict)
                                                                       and e["win"].get(k) == v) for k, v in match.items()):
            return e
    return None


def ms(a, b):
    return None if a is None or b is None else round((b - a) / 1000, 1)


# ---------------------------------------------------------------------------------------------
def sc_latency(s: Session, n=12):
    s.start_editor()
    s.start_app()
    rows = []
    for i in range(n):
        t0 = s.press()
        ok = wait_for(s.launcher_focused, 5)
        time.sleep(0.25)
        ev = s.shell_events(t0)
        if s.is_ext:
            shown = first(ev, "overlay-shown", t0)
            painted = first(ev, "overlay-painted", t0)
            row = {"focused": bool(ok), "key_to_focus_ms": ms(t0, shown and shown["t"]),
                   "key_to_pixels_ms": ms(t0, painted and painted["t"])}
        else:
            ff = first(ev, "first-frame", t0, wmclass="dev.spot.Spike") or first(ev, "first-frame", t0)
            fo = first(ev, "focus", t0, wmclass="dev.spot.Spike")
            rcv = next((e for e in s.app_events() if e["t"] >= t0 and e["ev"] in ("remote-cmdline", "action-toggle", "action-show")), None)
            row = {"focused": bool(ok), "key_to_app_ms": ms(t0, rcv and rcv["t"]),
                   "key_to_pixels_ms": ms(t0, ff and ff["t"]), "key_to_focus_ms": ms(t0, fo and fo["t"])}
        row["ready_notifications"] = sum(1 for e in ev if e["ev"] in ("demands-attention", "urgent"))
        if i == 0:
            screenshot(os.path.join(s.dir, "shown.png"))
        sp('spike.key(["Escape"])')
        back = wait_for(lambda: (sp("spike.focus()") or {}).get("title") == EDITOR, 3)
        row["focus_returned_to_editor"] = bool(back)
        rows.append(row)
        time.sleep(0.3)

    def stats(key):
        v = [r[key] for r in rows if r.get(key) is not None]
        return {"median": statistics.median(v), "min": min(v), "max": max(v)} if v else None
    return {"cycles": n, "all_focused": all(r["focused"] for r in rows),
            "ready_notifications": sum(r["ready_notifications"] for r in rows),
            "focus_always_returned": all(r["focus_returned_to_editor"] for r in rows),
            "key_to_focus_ms": stats("key_to_focus_ms"), "key_to_pixels_ms": stats("key_to_pixels_ms"),
            "key_to_app_ms": stats("key_to_app_ms"), "rows": rows}


def sc_typeahead(s: Session, delays_ms=(0, 30, 60, 100, 150), gap_ms=40):
    """Spotlight users type right after the shortcut. First key at <delay> after the shortcut,
    then one key every <gap_ms> (a fast typist). Did every character land in the launcher?"""
    s.start_editor()
    s.start_app()
    trials = []
    for d in delays_ms:
        before = s.editor_text() or ""
        s.press()
        time.sleep(d / 1000)
        for ch in "calc":
            sp(f'spike.type("{ch}")')
            time.sleep(gap_ms / 1000)
        time.sleep(1.0)
        after = s.editor_text() or ""
        got = s.launcher_text()
        trials.append({"first_key_delay_ms": d, "launcher_text": got, "leaked_into_editor": after[len(before):],
                       "pass": got == "calc" and after == before})
        sp('spike.key(["Escape"])')
        wait_for(lambda: (sp("spike.focus()") or {}).get("title") == EDITOR, 3)
        time.sleep(0.3)
    return {"gap_ms": gap_ms, "trials": trials,
            "min_safe_delay_ms": next((t["first_key_delay_ms"] for t in trials
                                       if all(u["pass"] for u in trials if u["first_key_delay_ms"] >= t["first_key_delay_ms"])), None)}


def sc_placement(s: Session):
    s.start_editor()  # big editor covering the centre of the screen
    s.start_app()
    s.press()
    wait_for(s.launcher_focused, 5)
    time.sleep(0.3)
    r = s.launcher_rect()
    wa = sp("spike.workarea(0)")
    mon = sp("spike.monitors()")[0]
    cx = mon["x"] + (mon["w"] - r["w"]) / 2
    screenshot(os.path.join(s.dir, "placement.png"))
    return {"rect": r, "workarea": wa, "horizontally_centered": abs(r["x"] - cx) <= 2,
            "y": r["y"], "editor": sp(f'spike.windows().find(w => w && w.title === "{EDITOR}")')}


def sc_multimonitor(s: Session):
    """Editor focused on monitor 0, pointer parked on monitor 1. Where does the launcher open?"""
    mons = sp("spike.monitors()")
    s.start_editor(size=(1200, 800))
    s.start_app()
    m1 = mons[1]
    sp(f"spike.pointerTo({m1['x'] + m1['w'] // 2}, {m1['y'] + m1['h'] // 2})")
    time.sleep(0.2)
    focus_mon = (sp("spike.focus()") or {}).get("monitor")
    s.press()
    wait_for(s.launcher_focused, 5)
    time.sleep(0.3)
    r = s.launcher_rect()
    screenshot(os.path.join(s.dir, "multimonitor.png"))
    return {"monitors": mons, "editor_monitor": focus_mon, "pointer_monitor": 1, "launcher": r,
            "launcher_monitor": r.get("monitor")}


def sc_overview(s: Session):
    s.start_editor()
    s.start_app()
    sp("spike.overview(true)")
    wait_for(lambda: sp("spike.overviewVisible()"), 3)
    time.sleep(0.5)
    t0 = s.press()
    ok = wait_for(s.launcher_focused, 4)
    time.sleep(0.4)
    screenshot(os.path.join(s.dir, "overview.png"))
    return {"shortcut_works_in_overview": bool(ok), "overview_still_visible": sp("spike.overviewVisible()"),
            "launcher_open": s.launcher_open(),
            "ready_notifications": sum(1 for e in s.shell_events(t0) if e["ev"] in ("demands-attention", "urgent"))}


def sc_fullscreen(s: Session):
    s.start_editor()
    s.start_app()
    sp(f'spike.fullscreen("{EDITOR}")')
    time.sleep(0.6)
    s.press()
    ok = wait_for(s.launcher_focused, 4)
    time.sleep(0.3)
    screenshot(os.path.join(s.dir, "fullscreen.png"))
    return {"focused_over_fullscreen": bool(ok), "stacking_bottom_to_top": sp("spike.stack()")}


def sc_focus_out(s: Session):
    """Show launcher, then click on the editor: launcher should dismiss itself."""
    s.start_editor()
    s.start_app()
    s.press()
    wait_for(s.launcher_focused, 4)
    time.sleep(0.3)
    sp("spike.pointerTo(1500, 800)")  # inside the editor, outside the launcher panel
    sp("spike.click()")
    closed = wait_for(lambda: not s.launcher_open(), 3)
    return {"dismissed_on_click_outside": bool(closed),
            "focus_now": (sp("spike.focus()") or {}).get("title")}


def sc_launch(s: Session):
    # the test .desktop file is written after the shell started; wait for the shell's app index
    app_seen = wait_for(lambda: sp('spike.Shell.AppSystem.get_default().lookup_app("spot-test-target.desktop") !== null'), 10)
    s.start_editor()
    s.start_app()
    s.press()
    wait_for(s.launcher_focused, 4)
    time.sleep(0.4)
    t0 = sp("spike.now()")
    sp('spike.type("Spot Test Target")')
    time.sleep(0.6)
    sp('spike.key(["Return"])')
    ok = wait_for(lambda: (sp("spike.focus()") or {}).get("title") == "Launched Target", 8)
    time.sleep(0.5)
    ev = s.shell_events(t0)
    launched = [e for e in s.app_events() if e["ev"] in ("launch", "launched", "launch-error")]
    return {"app_indexed_by_shell": bool(app_seen), "launched_window_focused": bool(ok), "launcher_closed": not s.launcher_open(),
            "ready_notifications": sum(1 for e in ev if e["ev"] in ("demands-attention", "urgent")),
            "notifications": sp("spike.notifications()"), "app_launch_events": launched}


def sc_coldstart(s: Session):
    """No resident process: the shortcut has to start the app from scratch (python here)."""
    if s.variant != "gtk-py":
        return {"skipped": "only gtk-py can cold-start from the shortcut (gdbus needs a D-Bus .service file)"}
    s.start_editor()
    rows = []
    for _ in range(3):
        if os.path.exists(s.applog):
            os.remove(s.applog)
        # the shortcut command (`spot_spike.py --toggle`) becomes the primary when none exists
        t0 = s.press()
        ok = wait_for(s.launcher_focused, 8)
        time.sleep(0.3)
        ev = s.shell_events(t0)
        ff = first(ev, "first-frame", t0, wmclass="dev.spot.Spike")
        rows.append({"focused": bool(ok), "key_to_pixels_ms": ms(t0, ff and ff["t"])})
        subprocess.run(["pkill", "-f", f"{APP}"], capture_output=True)
        time.sleep(0.8)
    return {"runs": rows}


SCENARIOS = {
    "latency": (sc_latency, {}),
    "typeahead": (sc_typeahead, {}),
    "placement_center_false": (sc_placement, {"center": False}),
    "placement_center_true": (sc_placement, {"center": True}),
    "multimonitor": (sc_multimonitor, {"monitors": ("1920x1080", "1920x1080")}),
    "overview": (sc_overview, {}),
    "fullscreen": (sc_fullscreen, {}),
    "focus_out": (sc_focus_out, {}),
    "launch": (sc_launch, {}),
    "coldstart": (sc_coldstart, {}),
}
VARIANTS = ["gtk-py", "gtk-dbus", "gtk-recreate", "shell-ext", "hybrid"]


def main():
    out = os.path.abspath(sys.argv[1])
    variants = [a for a in sys.argv[2:] if a in VARIANTS] or VARIANTS
    only = [a for a in sys.argv[2:] if a in SCENARIOS] or list(SCENARIOS)
    os.makedirs(out, exist_ok=True)
    res_path = os.path.join(out, "results.json")
    results = json.load(open(res_path)) if os.path.exists(res_path) else {}
    for v in variants:
        for name in only:
            fn, kw = SCENARIOS[name]
            if v == "gtk-recreate" and name not in ("latency", "typeahead", "placement_center_true"):
                continue  # recreate only differs in show/hide mechanics
            s = Session(out, f"{v}--{name}", v, **kw)
            try:
                r = fn(s)
            except Exception as e:  # keep going; record the failure
                r = {"error": repr(e)}
            finally:
                guard = s.stop()
            r["dconf_guard"] = guard
            results.setdefault(v, {})[name] = r
            print(f"[{v}] {name}: " + json.dumps({k: val for k, val in r.items() if k != "rows"})[:400], flush=True)
            json.dump(results, open(res_path, "w"), indent=2)
    print("wrote", res_path)


if __name__ == "__main__":
    main()
