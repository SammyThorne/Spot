"""Tiny client for the isolated headless shell: Eval JS against `globalThis.spike`, screenshot.

Run with the session env loaded (`source <state>/env.sh`).
"""
import json
import subprocess
import time

from gi.repository import Gio, GLib

_bus = None


def bus():
    global _bus
    if _bus is None or _bus.is_closed():
        # connect by address (not the process-wide singleton) so each isolated session gets its own bus
        import os
        _bus = Gio.DBusConnection.new_for_address_sync(
            os.environ["DBUS_SESSION_BUS_ADDRESS"],
            Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
    return _bus


def ev(js: str):
    """Evaluate JS in gnome-shell; returns parsed JSON if the result is JSON, else the raw string."""
    res = bus().call_sync("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell", "Eval",
                          GLib.Variant("(s)", (js,)), GLib.VariantType("(bs)"),
                          Gio.DBusCallFlags.NONE, 10000, None).unpack()
    ok, out = res
    if not ok:
        raise RuntimeError(f"Eval failed: {out}\n--- js: {js}")
    # Eval JSON-encodes its result, and sp() already stringifies -> decode up to twice.
    for _ in range(2):
        if not isinstance(out, str):
            break
        try:
            out = json.loads(out)
        except json.JSONDecodeError:
            break
    return out


def sp(expr: str):
    """Evaluate `JSON.stringify(<expr>)` where expr can use `spike`."""
    return ev(f"JSON.stringify((() => {{ const spike = globalThis.spike; return ({expr}); }})())")


def screenshot(path: str):
    bus().call_sync("org.gnome.Shell.Screenshot", "/org/gnome/Shell/Screenshot", "org.gnome.Shell.Screenshot",
                    "Screenshot", GLib.Variant("(bbs)", (False, False, path)), GLib.VariantType("(bs)"),
                    Gio.DBusCallFlags.NONE, 10000, None)
    return path


def wait_for(pred, timeout=5.0, interval=0.02):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        v = pred()
        if v:
            return v
        time.sleep(interval)
    return None


def now_us() -> int:
    return time.clock_gettime_ns(time.CLOCK_MONOTONIC) // 1000


def spawn(argv, env_extra=None, log=None):
    import os
    env = dict(os.environ, **(env_extra or {}))
    out = open(log, "ab") if log else subprocess.DEVNULL
    return subprocess.Popen(argv, env=env, stdout=out, stderr=subprocess.STDOUT, start_new_session=True)
