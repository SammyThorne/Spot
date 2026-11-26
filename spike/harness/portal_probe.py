#!/usr/bin/env python3
"""Probe the org.freedesktop.portal.GlobalShortcuts portal as a *host* (non-Flatpak) app.

  1. Register a host app id (org.freedesktop.host.portal.Registry, xdg-desktop-portal >= 1.19)
  2. CreateSession, BindShortcuts(<Super>space) -> GNOME shows an approval dialog
  3. Log every Activated/Deactivated signal (with CLOCK_MONOTONIC µs + options) to <log>

usage: portal_probe.py <jsonl-log> <seconds-to-listen>
"""
import json
import os
import sys
import time

from gi.repository import Gio, GLib

LOG, SECS = sys.argv[1], float(sys.argv[2])
APP_ID = "dev.spot.Spike"
DEST, PATH = "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop"
IFACE = "org.freedesktop.portal.GlobalShortcuts"

bus = Gio.DBusConnection.new_for_address_sync(
    os.environ["DBUS_SESSION_BUS_ADDRESS"],
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
loop = GLib.MainLoop()
sender = bus.get_unique_name()[1:].replace(".", "_")


def log(ev, **kw):
    rec = {"t": time.clock_gettime_ns(time.CLOCK_MONOTONIC) // 1000, "ev": ev, **kw}
    print(json.dumps(rec, default=str), flush=True)
    with open(LOG, "a") as f:
        f.write(json.dumps(rec, default=str) + "\n")


def call(iface, method, args, sig):
    return bus.call_sync(DEST, PATH, iface, method, GLib.Variant(sig, args), None, Gio.DBusCallFlags.NONE, 120000, None)


def request(method, args, sig, token, timeout=60):
    """Call a portal method that answers through a Request object; return (response, results)."""
    path = f"/org/freedesktop/portal/desktop/request/{sender}/{token}"
    out = {}

    def on_resp(_c, _s, _p, _i, _sig, params):
        out["r"] = params.unpack()
        loop.quit()
    sid = bus.signal_subscribe(DEST, "org.freedesktop.portal.Request", "Response", path, None,
                               Gio.DBusSignalFlags.NONE, on_resp)
    call(IFACE, method, args, sig)
    GLib.timeout_add_seconds(timeout, loop.quit)
    loop.run()
    bus.signal_unsubscribe(sid)
    return out.get("r")


try:
    v = bus.call_sync(DEST, PATH, "org.freedesktop.DBus.Properties", "Get", GLib.Variant("(ss)", (IFACE, "version")),
                      None, 0, 5000, None).unpack()[0]
    log("portal-version", version=v)
except GLib.Error as e:
    log("portal-missing", error=str(e))
    sys.exit(1)

try:
    call("org.freedesktop.host.portal.Registry", "Register", (APP_ID, {}), "(sa{sv})")
    log("host-registry", ok=True)
except GLib.Error as e:
    log("host-registry", ok=False, error=str(e))

r = request("CreateSession", ({"handle_token": GLib.Variant("s", "t1"),
                               "session_handle_token": GLib.Variant("s", "spot")},), "(a{sv})", "t1")
log("create-session", response=r)
session = r[1]["session_handle"]

for sig_name in ("Activated", "Deactivated", "ShortcutsChanged"):
    bus.signal_subscribe(DEST, IFACE, sig_name, PATH, None, Gio.DBusSignalFlags.NONE,
                         lambda _c, _s, _p, _i, name, params: log(name, params=params.unpack()))

shortcuts = [("toggle", {"description": GLib.Variant("s", "Toggle Spot launcher"),
                         "preferred_trigger": GLib.Variant("s", "<Super>space")})]
log("bind-requested")
r = request("BindShortcuts", (session, shortcuts, "", {"handle_token": GLib.Variant("s", "t2")}),
            "(oa(sa{sv})sa{sv})", "t2", timeout=int(SECS))
log("bind-response", response=r)

GLib.timeout_add(int(SECS * 1000), loop.quit)
loop.run()
log("done")
