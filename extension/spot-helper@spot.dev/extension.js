// Spot helper: the parts of a Spotlight-style launcher that an ordinary app can't do on
// GNOME Wayland. The launcher UI itself is the `spot` GTK app (dev.spot.Spot); this
// extension only
//   1. owns the shortcut inside the compositor (instant, and works in the Overview),
//   2. grabs the keyboard the moment the shortcut fires and buffers what the user types,
//   3. centres and raises the Spot window on the pointer's monitor when it maps,
//   4. sends the buffered text to the app over D-Bus (`prefill`, plus `accept` if Enter was
//      typed too), then drops the grab.
// If the app isn't running, the D-Bus call starts it (dev.spot.Spot.service).
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const BUS_NAME = 'dev.spot.Spot';
const OBJ_PATH = '/dev/spot/Spot';
const WM_CLASS = 'dev.spot.Spot';
const VERTICAL_POSITION = 0.22; // top edge at 22% of the monitor height, like Spotlight
// Safety net: never hold the keyboard longer than this if the app fails to appear.
const GRAB_TIMEOUT_MS = 2500;
// Cold start through D-Bus activation can take a while on a busy login.
const DBUS_TIMEOUT_MS = 5000;

// Timeline events for the test harness (spike/harness probe); a no-op otherwise.
const trace = (ev, extra = {}) => globalThis.spike?.log.push({t: GLib.get_monotonic_time(), ev, ...extra});

class Helper {
    constructor() {
        this._buffer = '';
        this._grab = null;
        this._timeoutId = 0;
        this._flushing = false;
        this._accept = false;
        this._sink = new St.Widget({reactive: true, can_focus: true, width: 1, height: 1, opacity: 0});
        Main.layoutManager.uiGroup.add_child(this._sink);
        this._sink.connect('key-press-event', (_a, ev) => this._onKey(ev));
        global.display.connectObject('window-created', (_d, w) => this._watch(w), this);
    }

    _spotWindow() {
        return global.get_window_actors().map(a => a.meta_window)
            .find(w => w.get_wm_class() === WM_CLASS && !w.is_hidden());
    }

    toggle() {
        const w = this._spotWindow();
        if (w?.has_focus()) {
            this._action('hide');
            return;
        }
        if (w) { // visible but not focused
            this._place(w);
            w.activate(global.get_current_time());
            return;
        }
        if (Main.overview.visible)
            Main.overview.hide();
        if (!this._grab) {
            this._buffer = '';
            this._accept = false;
            this._grab = Main.pushModal(this._sink, {actionMode: Shell.ActionMode.SYSTEM_MODAL});
            this._sink.grab_key_focus();
            trace('helper-grab', {ok: (this._grab.get_seat_state() & Clutter.GrabState.KEYBOARD) !== 0});
            this._timeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, GRAB_TIMEOUT_MS, () => {
                this._timeoutId = 0;
                trace('helper-timeout');
                this._release();
                return GLib.SOURCE_REMOVE;
            });
        }
        this._action('show');
    }

    _onKey(ev) {
        const sym = ev.get_key_symbol();
        if (sym === Clutter.KEY_Escape) {
            this._buffer = '';
            this._action('hide');
            this._release();
        } else if (this._accept) {
            // Enter already pressed: the launch is decided, ignore anything after it.
        } else if (sym === Clutter.KEY_Return || sym === Clutter.KEY_KP_Enter || sym === Clutter.KEY_ISO_Enter) {
            this._accept = true;
        } else if (sym === Clutter.KEY_BackSpace) {
            this._buffer = this._buffer.slice(0, -1);
        } else {
            let u = ev.get_key_unicode();
            if (typeof u === 'number')
                u = u ? String.fromCodePoint(u) : '';
            if (u && u >= ' ')
                this._buffer += u;
        }
        return Clutter.EVENT_STOP;
    }

    // GTK unmaps the window on hide, so every show is a new MetaWindow. Wayland windows get
    // their app id after creation, so wait for wm-class / shown before deciding it's ours.
    _watch(w) {
        const isSpot = () => w.get_wm_class() === WM_CLASS;
        w.connectObject('notify::wm-class', () => {
            if (isSpot())
                this._place(w);
        }, 'shown', () => {
            if (!isSpot())
                return;
            this._place(w);
            w.activate(global.get_current_time());
            trace('helper-shown', {rect: w.get_frame_rect()});
            this._flush();
        }, 'size-changed', () => {
            if (isSpot() && w.get_frame_rect().width !== w._spotPlacedWidth)
                this._place(w);
        }, 'unmanaged', () => w.disconnectObject(this), this);
        if (isSpot())
            this._place(w);
    }

    _place(w) {
        const mon = Main.layoutManager.currentMonitor; // the pointer's monitor
        const r = w.get_frame_rect();
        w._spotPlacedWidth = r.width; // re-centre only on width changes; growing taller keeps the top edge
        w.move_frame(true, mon.x + Math.round((mon.width - r.width) / 2),
            mon.y + Math.round(mon.height * VERTICAL_POSITION));
        w.make_above();
    }

    // Send the buffered text; keep the grab until the app has applied it so key order is kept.
    _flush() {
        if (!this._grab || this._flushing)
            return;
        if (!this._buffer) {
            if (this._accept) {
                this._accept = false;
                this._action('accept');
            }
            this._release();
            return;
        }
        const text = this._buffer;
        this._buffer = '';
        this._flushing = true;
        this._action('prefill', text, () => {
            this._flushing = false;
            trace('helper-prefilled', {text});
            this._flush(); // anything typed while the call was in flight
        });
    }

    _release() {
        if (this._timeoutId) {
            GLib.source_remove(this._timeoutId);
            this._timeoutId = 0;
        }
        if (this._grab) {
            Main.popModal(this._grab);
            this._grab = null;
            trace('helper-released');
        }
    }

    _action(name, param, done) {
        const params = param === undefined ? [] : [new GLib.Variant('s', param)];
        Gio.DBus.session.call(BUS_NAME, OBJ_PATH, 'org.gtk.Actions', 'Activate',
            new GLib.Variant('(sava{sv})', [name, params, {}]), null, Gio.DBusCallFlags.NONE,
            DBUS_TIMEOUT_MS, null, (conn, res) => {
                try {
                    conn.call_finish(res);
                } catch (e) {
                    console.warn(`Spot helper: ${name} failed: ${e.message}`);
                    trace('helper-dbus-error', {name, error: e.message});
                    this._release();
                }
                done?.();
            });
    }

    destroy() {
        this._release();
        global.display.disconnectObject(this);
        for (const a of global.get_window_actors())
            a.meta_window.disconnectObject(this);
        this._sink.destroy();
    }
}

export default class SpotHelper extends Extension {
    enable() {
        this._helper = new Helper();
        Main.wm.addKeybinding('toggle', this.getSettings(), Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW | Shell.ActionMode.POPUP,
            () => {
                trace('helper-keybinding');
                this._helper.toggle();
            });
    }

    disable() {
        Main.wm.removeKeybinding('toggle');
        this._helper.destroy();
        this._helper = null;
    }
}
