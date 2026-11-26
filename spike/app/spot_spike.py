#!/usr/bin/env python3
"""Phase 0 spike: a resident GTK4/libadwaita launcher window toggled by `--toggle`.

Same libgtk/GApplication code paths a gtk4-rs build would use; Python only to avoid
needing the -dev packages for the spike.

    spot_spike.py [--strategy hide|recreate] [--log FILE]   # start resident primary
    spot_spike.py --toggle                                   # show/hide (remote-activates primary)
    spot_spike.py --quit

Every interesting event is appended to the JSONL log with CLOCK_MONOTONIC µs, the same
clock gnome-shell's GLib.get_monotonic_time() uses, so latencies can be joined.
"""
import json
import os
import sys
import time

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
gi.require_version("Gdk", "4.0")
from gi.repository import Adw, Gdk, Gio, GLib, Gtk  # noqa: E402

APP_ID = "dev.spot.Spike"
CSS = b"""
window.spot { border-radius: 14px; }
.spot-entry { font-size: 20px; min-height: 48px; padding: 0 12px; }
.spot-row { padding: 6px 10px; }
.spot-row label.sub { opacity: 0.6; font-size: 11px; }
"""


def now_us():
    return time.clock_gettime_ns(time.CLOCK_MONOTONIC) // 1000


class Spike(Adw.Application):
    def __init__(self):
        super().__init__(application_id=APP_ID, flags=Gio.ApplicationFlags.HANDLES_COMMAND_LINE)
        self.add_main_option("toggle", 0, GLib.OptionFlags.NONE, GLib.OptionArg.NONE, "Show/hide", None)
        self.add_main_option("quit", 0, GLib.OptionFlags.NONE, GLib.OptionArg.NONE, "Quit primary", None)
        self.add_main_option("strategy", 0, GLib.OptionFlags.NONE, GLib.OptionArg.STRING, "hide|recreate", None)
        self.add_main_option("log", 0, GLib.OptionFlags.NONE, GLib.OptionArg.STRING, "JSONL log", None)
        self.strategy = "hide"
        self.log_path = os.environ.get("SPOT_SPIKE_LOG")
        self.win = None
        self.apps = []

    # ---- logging -------------------------------------------------------------------------
    def log(self, ev, **kw):
        rec = {"t": now_us(), "ev": ev, **kw}
        if self.log_path:
            with open(self.log_path, "a") as f:
                f.write(json.dumps(rec) + "\n")
        print(json.dumps(rec), flush=True)

    # ---- GApplication plumbing ----------------------------------------------------------
    def do_before_emit(self, platform_data):
        # Shows whether the remote invocation carried an activation token.
        pd = platform_data.unpack() if platform_data else {}
        self.log("before-emit", platform_data={k: (v if isinstance(v, (str, int, bool)) else str(v))
                                                for k, v in pd.items() if k != "environ"})
        Adw.Application.do_before_emit(self, platform_data)

    def do_startup(self):
        Adw.Application.do_startup(self)
        self.hold()  # stay resident with no windows
        prov = Gtk.CssProvider()
        prov.load_from_data(CSS)
        Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), prov,
                                                  Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
        self.apps = sorted((a for a in Gio.AppInfo.get_all() if a.should_show()),
                           key=lambda a: a.get_display_name().lower())
        # D-Bus-callable action: lets the shortcut be a tiny `gdbus call` (no client startup cost)
        act = Gio.SimpleAction.new("toggle", None)
        act.connect("activate", lambda *_: (self.log("action-toggle"), self.toggle()))
        self.add_action(act)
        # Actions used by the hybrid helper extension (ext/spot-helper@spot.local)
        for name, fn in (("show", lambda *_: self.show()), ("hide", lambda *_: self.hide(reason="helper"))):
            a = Gio.SimpleAction.new(name, None)
            a.connect("activate", lambda _a, _p, n=name, f=fn: (self.log("action-" + n), f()))
            self.add_action(a)
        pre = Gio.SimpleAction.new("prefill", GLib.VariantType.new("s"))
        pre.connect("activate", lambda _a, p: self.prefill(p.get_string()))
        self.add_action(pre)
        self.log("startup", pid=os.getpid(), apps=len(self.apps))

    def do_command_line(self, cl):
        opts = cl.get_options_dict().end().unpack()
        if cl.get_is_remote():
            env = {k: cl.getenv(k) for k in ("XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID")}
            self.log("remote-cmdline", opts=opts, env=env)
        else:
            self.strategy = opts.get("strategy", self.strategy)
            self.log_path = opts.get("log", self.log_path)
            self.log("primary-start", strategy=self.strategy)
            if self.strategy == "hide":
                self.win = self.build_window()  # pre-built, never shown until toggled
                if os.environ.get("SPOT_SPIKE_PREWARM", "1") == "1":
                    self.win.realize()  # create GdkSurface + GSK renderer now, not on first show
        if opts.get("quit"):
            self.quit()
        elif opts.get("toggle"):
            self.toggle()
        return 0

    # ---- window --------------------------------------------------------------------------
    def build_window(self):
        win = Gtk.Window(application=self, title="Spot Spike", decorated=False, resizable=False)
        win.set_default_size(680, 440)
        win.add_css_class("spot")
        win.set_hide_on_close(True)

        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6, margin_top=10, margin_bottom=10,
                      margin_start=10, margin_end=10)
        entry = Gtk.SearchEntry(placeholder_text="Search apps…", hexpand=True)
        entry.add_css_class("spot-entry")
        lst = Gtk.ListBox(selection_mode=Gtk.SelectionMode.BROWSE)
        lst.add_css_class("navigation-sidebar")
        for a in self.apps:
            row = Gtk.ListBoxRow()
            row.app = a
            h = Gtk.Box(spacing=10)
            h.add_css_class("spot-row")
            icon = Gtk.Image.new_from_gicon(a.get_icon() or Gio.ThemedIcon.new("application-x-executable"))
            icon.set_pixel_size(32)
            v = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
            v.append(Gtk.Label(label=a.get_display_name(), xalign=0))
            sub = Gtk.Label(label=a.get_description() or a.get_id() or "", xalign=0, ellipsize=3)
            sub.add_css_class("sub")
            v.append(sub)
            h.append(icon)
            h.append(v)
            row.set_child(h)
            lst.append(row)
        scr = Gtk.ScrolledWindow(vexpand=True, hscrollbar_policy=Gtk.PolicyType.NEVER)
        scr.set_child(lst)
        box.append(entry)
        box.append(scr)
        win.set_child(box)

        def refilter(*_):
            q = entry.get_text().lower()
            lst.set_filter_func(lambda r: q in r.app.get_display_name().lower())
            first = next((r for r in iter_rows(lst) if r.get_child_visible() and q in r.app.get_display_name().lower()), None)
            if first:
                lst.select_row(first)

        def activate(*_):
            row = lst.get_selected_row()
            if row:
                self.launch(row.app)

        entry.connect("search-changed", refilter)
        entry.connect("changed", lambda e: self.log("text", text=e.get_text()))
        entry.connect("activate", activate)
        lst.connect("row-activated", lambda _l, r: self.launch(r.app))

        keys = Gtk.EventControllerKey()

        def on_key(_c, keyval, _code, _state):
            if keyval == Gdk.KEY_Escape:
                self.hide()
                return True
            if keyval in (Gdk.KEY_Down, Gdk.KEY_Up):
                rows = [r for r in iter_rows(lst) if r.get_mapped()]
                cur = lst.get_selected_row()
                i = rows.index(cur) if cur in rows else -1
                i = min(len(rows) - 1, i + 1) if keyval == Gdk.KEY_Down else max(0, i - 1)
                if rows:
                    lst.select_row(rows[i])
                return True
            return False

        keys.connect("key-pressed", on_key)
        # CAPTURE: Gtk.SearchEntry consumes Escape (stop-search) before a bubble-phase handler sees it
        keys.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
        win.add_controller(keys)

        win.connect("map", lambda w: self.log("map"))
        win.connect("unmap", lambda w: self.log("unmap"))
        win.connect("notify::is-active", self.on_active_changed)
        win.entry = entry
        refilter()
        return win

    def on_active_changed(self, win, _p):
        active = win.is_active()
        self.log("active" if active else "inactive")
        # Spotlight semantics: dismiss when focus goes elsewhere.
        if not active and win.get_visible():
            GLib.idle_add(lambda: (self.hide(reason="focus-out"), False)[1])

    def toggle(self):
        if self.win is not None and self.win.get_visible():
            self.hide(reason="toggle")
        else:
            self.show()

    def show(self):
        if self.win is None:
            self.win = self.build_window()
        self.win.entry.set_text("")
        self.log("present-called")
        self.win.present()
        self.win.entry.grab_focus()

    def prefill(self, text):
        """Type-ahead captured by the helper extension before this window had focus."""
        if self.win is None:
            return
        e = self.win.entry
        e.set_text(e.get_text() + text)
        e.set_position(-1)
        self.log("prefill", text=text)

    def hide(self, reason="esc"):
        if self.win is None or not self.win.get_visible():
            return
        self.log("hide", reason=reason)
        if self.strategy == "recreate":
            self.win.destroy()
            self.win = None
        else:
            self.win.set_visible(False)

    def launch(self, app):
        ctx = self.win.get_display().get_app_launch_context()
        ctx.connect("launched", lambda _c, info, pd: self.log(
            "launched", app=info.get_id(),
            token=(pd.unpack() or {}).get("activation-token") if pd else None))
        self.log("launch", app=app.get_id())
        try:
            app.launch([], ctx)
        except GLib.Error as e:
            self.log("launch-error", error=str(e))
        self.hide(reason="launched")


def iter_rows(lst):
    r = lst.get_first_child()
    while r is not None:
        yield r
        r = r.get_next_sibling()


if __name__ == "__main__":
    sys.exit(Spike().run(sys.argv))
