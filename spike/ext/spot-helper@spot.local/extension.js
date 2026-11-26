// Phase 0 spike, hybrid design: the UI stays a normal GTK app (dev.spot.Spike); this tiny
// extension only does what an app cannot do on GNOME Wayland:
//   1. own the hotkey inside the compositor (instant, works in the overview),
//   2. grab the keyboard the moment the hotkey fires and buffer type-ahead,
//   3. centre + raise the app window on the pointer's monitor when it maps,
//   4. hand the buffered text to the app over D-Bus, then release the grab.
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const BUS_NAME = 'dev.spot.Spike';
const OBJ_PATH = '/dev/spot/Spike';
const WM_CLASS = 'dev.spot.Spike';
const GRAB_TIMEOUT_MS = 1500;
const mark = (ev, extra = {}) => globalThis.spike?.log.push({t: GLib.get_monotonic_time(), ev, ...extra});

class Helper {
    constructor() {
        this._buffer = '';
        this._grab = null;
        this._timeoutId = 0;
        this._flushing = false;
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
        if (w) { // visible but unfocused (e.g. user clicked away and focus-out hide is off)
            this._place(w);
            w.activate(global.get_current_time());
            return;
        }
        if (Main.overview.visible)
            Main.overview.hide();
        this._buffer = '';
        this._grab = Main.pushModal(this._sink, {actionMode: Shell.ActionMode.SYSTEM_MODAL});
        this._sink.grab_key_focus();
        mark('helper-grab', {ok: (this._grab.get_seat_state() & Clutter.GrabState.KEYBOARD) !== 0});
        this._timeoutId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, GRAB_TIMEOUT_MS, () => {
            this._timeoutId = 0;
            mark('helper-timeout');
            this._release();
            return GLib.SOURCE_REMOVE;
        });
        this._action('show');
    }

    _onKey(ev) {
        const sym = ev.get_key_symbol();
        if (sym === Clutter.KEY_Escape) {
            this._buffer = '';
            this._action('hide');
            this._release();
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

    // Wayland windows get their app-id after creation, so wait for wm-class / shown.
    _watch(w) {
        if (!this._grab)
            return;
        const tryPlace = () => {
            if (w.get_wm_class() !== WM_CLASS)
                return;
            this._place(w);
        };
        w.connectObject('notify::wm-class', tryPlace, 'shown', () => {
            if (w.get_wm_class() !== WM_CLASS)
                return;
            this._place(w);
            w.activate(global.get_current_time());
            mark('helper-shown', {rect: w.get_frame_rect()});
            this._flush();
        }, this);
        tryPlace();
    }

    _place(w) {
        const mon = Main.layoutManager.currentMonitor; // pointer's monitor
        const r = w.get_frame_rect();
        w.move_frame(true, mon.x + Math.round((mon.width - r.width) / 2), mon.y + Math.round(mon.height * 0.22));
        w.make_above();
    }

    // Send buffered text; keep the grab until the app has applied it so ordering is preserved.
    _flush() {
        if (!this._grab || this._flushing)
            return;
        if (!this._buffer) {
            this._release();
            return;
        }
        const text = this._buffer;
        this._buffer = '';
        this._flushing = true;
        this._action('prefill', text, () => {
            this._flushing = false;
            mark('helper-prefilled', {text});
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
            mark('helper-released');
        }
    }

    _action(name, param, done) {
        const params = param === undefined ? [] : [new GLib.Variant('s', param)];
        Gio.DBus.session.call(BUS_NAME, OBJ_PATH, 'org.gtk.Actions', 'Activate',
            new GLib.Variant('(sava{sv})', [name, params, {}]), null, Gio.DBusCallFlags.NONE, 2000, null,
            (conn, res) => {
                try {
                    conn.call_finish(res);
                } catch (e) {
                    mark('helper-dbus-error', {name, error: e.message});
                    this._release();
                }
                done?.();
            });
    }

    destroy() {
        this._release();
        global.display.disconnectObject(this);
        this._sink.destroy();
    }
}

export default class SpotHelper extends Extension {
    enable() {
        this._helper = new Helper();
        Main.wm.addKeybinding('toggle', this.getSettings(), Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW | Shell.ActionMode.POPUP,
            () => { mark('helper-keybinding'); this._helper.toggle(); });
    }

    disable() {
        Main.wm.removeKeybinding('toggle');
        this._helper.destroy();
        this._helper = null;
    }
}
