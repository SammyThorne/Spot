// Test-only extension for the isolated headless session. Enables unsafe mode (so the
// harness can call org.gnome.Shell.Eval / Screenshot) and exposes `globalThis.spike`,
// a small API for injecting input and observing window/focus behaviour.
import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const now = () => GLib.get_monotonic_time(); // µs, CLOCK_MONOTONIC (same clock as clients)

function winInfo(w) {
    if (!w) return null;
    const r = w.get_frame_rect();
    return {
        title: w.get_title(), wmclass: w.get_wm_class(), gtkAppId: w.get_gtk_application_id(),
        pid: w.get_pid(), x: r.x, y: r.y, w: r.width, h: r.height,
        monitor: w.get_monitor(), above: w.is_above(), hidden: w.is_hidden(),
        focus: w.has_focus(), demandsAttention: w.demands_attention,
    };
}

export default class Probe extends Extension {
    enable() {
        global.context.unsafe_mode = true;
        const log = [];
        const push = (ev, extra = {}) => log.push({t: now(), ev, ...extra});
        const seat = Clutter.get_default_backend().get_default_seat();
        const kbd = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
        const ptr = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);

        global.display.connectObject(
            'window-created', (_d, w) => {
                push('window-created', {title: w.get_title(), wmclass: w.get_wm_class()});
                // first-frame = the moment the compositor first has pixels for this window
                const hook = () => {
                    const actor = w.get_compositor_private();
                    if (!actor) return GLib.SOURCE_CONTINUE;
                    actor.connect('first-frame', () => { const r = w.get_frame_rect(); push('first-frame', {title: w.get_title(), wmclass: w.get_wm_class(), x: r.x, y: r.y}); });
                    return GLib.SOURCE_REMOVE;
                };
                if (hook() === GLib.SOURCE_CONTINUE) GLib.idle_add(GLib.PRIORITY_HIGH, hook);
            },
            'notify::focus-window', () => push('focus', {win: winInfo(global.display.focus_window)}),
            'window-demands-attention', (_d, w) => push('demands-attention', {title: w.get_title(), wmclass: w.get_wm_class()}),
            'window-marked-urgent', (_d, w) => push('urgent', {title: w.get_title()}),
            this);
        Main.messageTray.connectObject('source-added', (_t, src) => push('notification-source', {title: src.title}), this);

        globalThis.spike = {
            Main, GLib, Gio, Clutter, Meta, Shell, log,
            now,
            clearLog() { log.length = 0; return now(); },
            // combo like ['Super_L','space']: press in order, release in reverse. Returns press time.
            key(combo) {
                const t0 = now();
                const kv = combo.map(k => Clutter[`KEY_${k}`]);
                kv.forEach(k => kbd.notify_keyval(now(), k, Clutter.KeyState.PRESSED));
                [...kv].reverse().forEach(k => kbd.notify_keyval(now(), k, Clutter.KeyState.RELEASED));
                push('key', {combo});
                return t0;
            },
            type(text) { for (const ch of text) { const k = Clutter.unicode_to_keysym(ch.codePointAt(0));
                kbd.notify_keyval(now(), k, Clutter.KeyState.PRESSED); kbd.notify_keyval(now(), k, Clutter.KeyState.RELEASED); } },
            pointerTo(x, y) { ptr.notify_absolute_motion(now(), x, y); },
            click() { ptr.notify_button(now(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.PRESSED);
                      ptr.notify_button(now(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.RELEASED); },
            windows() { return global.get_window_actors().map(a => winInfo(a.meta_window)); },
            focus() { return winInfo(global.display.focus_window); },
            monitors() { return [...Array(global.display.get_n_monitors()).keys()].map(i => {
                const g = global.display.get_monitor_geometry(i); return {i, x: g.x, y: g.y, w: g.width, h: g.height}; }); },
            workarea(i = 0) { const a = Main.layoutManager.getWorkAreaForMonitor(i); return {x: a.x, y: a.y, w: a.width, h: a.height}; },
            activate(substr) { const w = global.get_window_actors().map(a => a.meta_window)
                .find(m => (m.get_title() ?? '').includes(substr)); w?.activate(global.get_current_time()); return winInfo(w); },
            move(substr, x, y, wd, ht) { const w = global.get_window_actors().map(a => a.meta_window)
                .find(m => (m.get_title() ?? '').includes(substr)); w?.move_resize_frame(false, x, y, wd, ht); return winInfo(w); },
            stack() { return global.display.sort_windows_by_stacking(global.get_window_actors().map(a => a.meta_window))
                .map(w => w.get_title()); },
            fullscreen(substr) { const w = global.get_window_actors().map(a => a.meta_window)
                .find(m => (m.get_title() ?? '').includes(substr)); w?.make_fullscreen(); return winInfo(w); },
            overview(show) { show ? Main.overview.show() : Main.overview.hide(); return Main.overview.visible; },
            overviewVisible() { return Main.overview.visible; },
            notifications() { return Main.messageTray.getSources().map(s => ({title: s.title,
                n: s.notifications.map(n => ({title: n.title, body: n.body}))})); },
        };
        const hideOverview = () => Main.overview.hide();
        if (Main.layoutManager._startingUp)
            Main.layoutManager.connectObject('startup-complete', hideOverview, this);
        else
            hideOverview();
    }
    disable() {
        global.display.disconnectObject(this);
        Main.messageTray.disconnectObject(this);
        Main.layoutManager.disconnectObject(this);
        delete globalThis.spike;
    }
}
