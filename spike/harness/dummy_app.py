#!/usr/bin/env python3
"""A plain GTK4 window that plays 'the app the user is working in'. Title from argv[1]."""
import sys

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gtk  # noqa: E402

title = sys.argv[1] if len(sys.argv) > 1 else "Dummy Editor"
size = (int(sys.argv[2]), int(sys.argv[3])) if len(sys.argv) > 3 else (1400, 900)


def on_activate(app):
    w = Gtk.ApplicationWindow(application=app, title=title)
    w.set_default_size(*size)
    tv = Gtk.TextView()
    buf = tv.get_buffer()
    buf.set_text(f"{title}: pretend the user is typing here.\n")
    # report every edit so the harness can detect keystrokes that leaked into this window
    buf.connect("changed", lambda b: print("TEXT", repr(b.get_text(b.get_start_iter(), b.get_end_iter(), False)), flush=True))
    print("TEXT", repr(buf.get_text(buf.get_start_iter(), buf.get_end_iter(), False)), flush=True)
    w.set_child(tv)
    w.present()


app = Gtk.Application(application_id="dev.spot.Dummy" + title.replace(" ", ""))
app.connect("activate", on_activate)
app.run([sys.argv[0]])
