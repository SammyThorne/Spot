// Phase 0 spike: Spotlight-style overlay drawn by GNOME Shell itself.
// Compared against the GTK4 window approach (app/spot_spike.py). In a real build the
// result list would come from the Rust core over D-Bus; here Shell.AppSystem is used so
// the spike measures only the UI/focus mechanics.
import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const WIDTH = 680;
const MAX_RESULTS = 8;
// Harness hook (only present in the isolated test session).
const mark = (ev, extra = {}) => globalThis.spike?.log.push({t: GLib.get_monotonic_time(), ev, ...extra});

class Overlay {
    constructor() {
        this._appSys = Shell.AppSystem.get_default();
        this._results = [];
        this._sel = 0;
        this._grab = null;

        // Full-monitor transparent backdrop: catches clicks outside the panel.
        this._backdrop = new St.Widget({reactive: true, visible: false, layout_manager: new Clutter.FixedLayout()});
        this._backdrop.connect('button-press-event', () => { this.close('click-outside'); return Clutter.EVENT_STOP; });

        this._panel = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL, reactive: true, width: WIDTH,
            style: 'background-color: rgba(34,34,38,0.97); border-radius: 16px; padding: 10px; spacing: 6px;' +
                   'box-shadow: 0 10px 40px rgba(0,0,0,0.55); border: 1px solid rgba(255,255,255,0.08);',
        });
        this._panel.connect('button-press-event', () => Clutter.EVENT_STOP);
        this._entry = new St.Entry({
            hint_text: 'Search apps…', can_focus: true, x_expand: true,
            style: 'font-size: 20px; padding: 12px 14px; border-radius: 10px;',
        });
        this._list = new St.BoxLayout({orientation: Clutter.Orientation.VERTICAL, style: 'spacing: 2px;'});
        this._panel.add_child(this._entry);
        this._panel.add_child(this._list);
        this._backdrop.add_child(this._panel);
        Main.layoutManager.addTopChrome(this._backdrop);

        this._entry.clutter_text.connect('text-changed', () => this._refresh());
        this._entry.clutter_text.connect('key-press-event', (_a, ev) => this._onKey(ev));
    }

    get isOpen() { return this._backdrop.visible; }

    toggle() { this.isOpen ? this.close('toggle') : this.open(); }

    open() {
        mark('overlay-open-begin');
        // Monitor under the pointer, like Spotlight on the active display.
        const mon = Main.layoutManager.currentMonitor;
        this._backdrop.set_position(mon.x, mon.y);
        this._backdrop.set_size(mon.width, mon.height);
        this._panel.set_position(Math.round((mon.width - WIDTH) / 2), Math.round(mon.height * 0.22));
        this._entry.set_text('');
        this._refresh();
        this._backdrop.show();
        Main.layoutManager.uiGroup.set_child_above_sibling(this._backdrop, null);

        this._grab = Main.pushModal(this._backdrop, {actionMode: Shell.ActionMode.SYSTEM_MODAL});
        if ((this._grab.get_seat_state() & Clutter.GrabState.KEYBOARD) === 0) {
            Main.popModal(this._grab);
            this._grab = null;
            this._backdrop.hide();
            mark('overlay-grab-failed');
            return;
        }
        this._entry.grab_key_focus();
        mark('overlay-shown', {monitor: mon.index, x: mon.x + this._panel.x, y: mon.y + this._panel.y});
        const id = global.stage.connect_after('after-paint', () => {
            global.stage.disconnect(id);
            mark('overlay-painted');
        });
    }

    close(reason) {
        if (!this.isOpen)
            return;
        if (this._grab) {
            Main.popModal(this._grab);
            this._grab = null;
        }
        this._backdrop.hide();
        mark('overlay-closed', {reason});
    }

    _refresh() {
        const q = this._entry.get_text().trim().toLowerCase();
        const infos = this._appSys.get_installed().filter(a => a.should_show() &&
            (!q || a.get_display_name().toLowerCase().includes(q)));
        infos.sort((a, b) => {
            const pa = a.get_display_name().toLowerCase().startsWith(q) ? 0 : 1;
            const pb = b.get_display_name().toLowerCase().startsWith(q) ? 0 : 1;
            return pa - pb || a.get_display_name().localeCompare(b.get_display_name());
        });
        this._results = infos.slice(0, MAX_RESULTS).map(i => this._appSys.lookup_app(i.get_id())).filter(Boolean);
        this._sel = 0;
        this._list.destroy_all_children();
        this._results.forEach((app, i) => {
            const row = new St.BoxLayout({style: 'spacing: 12px; padding: 8px 10px; border-radius: 8px;', reactive: true});
            row.add_child(app.create_icon_texture(32));
            row.add_child(new St.Label({text: app.get_name(), y_align: Clutter.ActorAlign.CENTER}));
            row.connect('button-press-event', () => { this._launch(app); return Clutter.EVENT_STOP; });
            this._list.add_child(row);
        });
        this._paintSel();
    }

    _paintSel() {
        this._list.get_children().forEach((r, i) =>
            r.set_style(`spacing: 12px; padding: 8px 10px; border-radius: 8px;${
                i === this._sel ? ' background-color: rgba(255,255,255,0.12);' : ''}`));
    }

    _onKey(ev) {
        const sym = ev.get_key_symbol();
        if (sym === Clutter.KEY_Escape) { this.close('esc'); return Clutter.EVENT_STOP; }
        if (sym === Clutter.KEY_Down || sym === Clutter.KEY_Up) {
            const n = this._results.length;
            if (n) this._sel = (this._sel + (sym === Clutter.KEY_Down ? 1 : n - 1)) % n;
            this._paintSel();
            return Clutter.EVENT_STOP;
        }
        if (sym === Clutter.KEY_Return || sym === Clutter.KEY_KP_Enter) {
            const app = this._results[this._sel];
            if (app) this._launch(app);
            return Clutter.EVENT_STOP;
        }
        return Clutter.EVENT_PROPAGATE;
    }

    _launch(app) {
        mark('overlay-launch', {app: app.get_id()});
        this.close('launched');
        app.activate(); // Shell.App: proper launch context, startup-notification, focus
    }

    query() { return {open: this.isOpen, text: this._entry.get_text(), results: this._results.map(a => a.get_id())}; }

    destroy() { this.close('disable'); this._backdrop.destroy(); }
}

export default class SpotOverlay extends Extension {
    enable() {
        this._overlay = new Overlay();
        Main.wm.addKeybinding('toggle', this.getSettings(), Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW | Shell.ActionMode.POPUP,
            () => { mark('overlay-keybinding'); this._overlay.toggle(); });
        globalThis.spotOverlay = this._overlay;
    }

    disable() {
        Main.wm.removeKeybinding('toggle');
        this._overlay.destroy();
        this._overlay = null;
        delete globalThis.spotOverlay;
    }
}
