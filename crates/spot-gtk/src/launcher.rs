//! The launcher window: a search field and a short ranked result list.
//!
//! Phase 0 lessons applied here:
//! - one window, built at startup and hidden/shown (recreating was slower and lost type-ahead);
//! - Escape is handled in the CAPTURE phase (GtkSearchEntry swallows it otherwise);
//! - results update on every keystroke (no GtkSearchEntry 150 ms search delay);
//! - the window is positioned by the helper extension, never by the app (Wayland).
//!
//! Sources: desktop apps load synchronously at startup (fast, needed for the first query);
//! AppImages are scanned on a worker thread and merged in when ready, then rescanned when
//! their folders or the installed-apps set change.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use spot_core::open::{self, Target};
use spot_core::{actions, appimage, config, desktop, history, results, Config, Entry, History, Index, Item};

const WIDTH: i32 = 680;
const MAX_RESULTS: usize = 8;
const ICON_SIZE: i32 = 32;

/// Opt-in event log for automated UI tests: `SPOT_TRACE=/path/file` appends `<event>\t<value>` lines.
pub fn trace(event: &str, value: &str) {
    use std::io::Write;
    thread_local!(static FILE: Option<std::cell::RefCell<std::fs::File>> = std::env::var_os("SPOT_TRACE")
        .and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok())
        .map(std::cell::RefCell::new));
    FILE.with(|f| {
        if let Some(f) = f {
            let _ = writeln!(f.borrow_mut(), "{event}\t{}", value.replace(['\n', '\t'], " "));
        }
    });
}

pub struct Launcher {
    window: adw::ApplicationWindow,
    entry: gtk::SearchEntry,
    list: gtk::ListBox,
    separator: gtk::Separator,
    /// "No results" (only reachable with web search turned off).
    empty: gtk::Label,
    config: Config,
    history: RefCell<History>,
    index: RefCell<Index>,
    items: RefCell<Vec<Item>>,
    desktop_entries: RefCell<Vec<Entry>>,
    appimage_entries: RefCell<Vec<Entry>>,
    /// Incremented per AppImage scan; results of an older scan are dropped.
    scan_generation: Cell<u64>,
    desktop_reload_pending: Cell<bool>,
    appimage_rescan_pending: Cell<bool>,
    // Kept alive so their `changed` signals keep firing.
    app_monitor: gio::AppInfoMonitor,
    dir_monitors: RefCell<Vec<gio::FileMonitor>>,
}

impl Launcher {
    pub fn new(app: &adw::Application) -> Rc<Self> {
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Search apps, calculate, open…")
            .hexpand(true)
            .width_request(WIDTH - 24)
            .build();
        entry.add_css_class("spot-entry");

        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).visible(false).build();
        list.add_css_class("spot-list");
        let separator = gtk::Separator::builder().visible(false).build();
        let empty = gtk::Label::builder().label("No results").visible(false).build();
        empty.add_css_class("spot-empty");

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.add_css_class("spot-content");
        content.append(&entry);
        content.append(&separator);
        content.append(&list);
        content.append(&empty);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Spot")
            .resizable(false)
            .hide_on_close(true)
            .content(&content)
            .build();
        window.add_css_class("spot");

        trace("startup", "");
        let launcher = Rc::new(Launcher {
            window,
            entry,
            list,
            separator,
            empty,
            config: Config::load(&config::default_path()),
            history: RefCell::new(History::load(&history::default_path())),
            index: RefCell::new(Index::new(Vec::new())),
            items: RefCell::new(Vec::new()),
            desktop_entries: RefCell::new(desktop::load_entries()),
            appimage_entries: RefCell::new(Vec::new()),
            scan_generation: Cell::new(0),
            desktop_reload_pending: Cell::new(false),
            appimage_rescan_pending: Cell::new(false),
            app_monitor: gio::AppInfoMonitor::get(),
            dir_monitors: RefCell::new(Vec::new()),
        });
        launcher.rebuild_index();
        launcher.connect_signals();
        launcher.watch_appimage_dirs();
        launcher.rescan_appimages();
        WidgetExt::realize(&launcher.window);
        launcher.warm_icon_cache();
        launcher
    }

    fn connect_signals(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        self.entry.connect_changed(move |_| {
            if let Some(l) = this.upgrade() {
                l.refresh();
            }
        });

        let this = Rc::downgrade(self);
        self.list.connect_row_activated(move |_, row| {
            if let Some(l) = this.upgrade() {
                l.launch(row.index() as usize);
            }
        });

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(l) = this.upgrade() else { return glib::Propagation::Proceed };
            let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
            match key {
                gdk::Key::Escape => l.hide(),
                gdk::Key::Down => l.move_selection(1),
                gdk::Key::Up => l.move_selection(-1),
                gdk::Key::n if ctrl => l.move_selection(1),
                gdk::Key::p if ctrl => l.move_selection(-1),
                gdk::Key::Return | gdk::Key::KP_Enter => l.launch_selected(),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.window.add_controller(keys);

        // Spotlight semantics: clicking anywhere else dismisses the launcher.
        let this = Rc::downgrade(self);
        self.window.connect_is_active_notify(move |w| {
            if !w.is_active() && w.is_visible() {
                let this = this.clone();
                glib::idle_add_local_once(move || {
                    if let Some(l) = this.upgrade() {
                        l.hide();
                    }
                });
            }
        });

        // Re-index when apps are installed/removed (debounced: package installs touch many files).
        let this = Rc::downgrade(self);
        self.app_monitor.connect_changed(move |_| {
            if let Some(l) = this.upgrade() {
                l.debounce(|l| &l.desktop_reload_pending, 500, |l| l.reload_desktop());
            }
        });
    }

    /// Run `f` once, `ms` after the first of a burst of calls.
    fn debounce(
        self: &Rc<Self>,
        flag: fn(&Self) -> &Cell<bool>,
        ms: u64,
        f: impl FnOnce(&Rc<Self>) + 'static,
    ) {
        if flag(self).replace(true) {
            return;
        }
        let this = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(ms), move || {
            if let Some(l) = this.upgrade() {
                flag(&l).set(false);
                f(&l);
            }
        });
    }

    /// Watch the AppImage folders (non-recursive). Missing folders are watched too, so creating
    /// ~/Applications later works. Browsers write downloads under a temporary name and rename at
    /// the end, which shows up here as one more change.
    fn watch_appimage_dirs(self: &Rc<Self>) {
        let mut monitors = Vec::new();
        for dir in appimage::default_dirs() {
            let Ok(m) = gio::File::for_path(&dir).monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            else {
                continue;
            };
            let this = Rc::downgrade(self);
            // Any event: created/deleted/renamed, but also content writes and chmod, which can
            // turn a partial file into a complete AppImage. Unchanged files hit the cache.
            m.connect_changed(move |_, _, _, _| {
                if let Some(l) = this.upgrade() {
                    l.debounce(|l| &l.appimage_rescan_pending, 1000, |l| l.rescan_appimages());
                }
            });
            monitors.push(m);
        }
        *self.dir_monitors.borrow_mut() = monitors;
    }

    fn rescan_appimages(self: &Rc<Self>) {
        let generation = self.scan_generation.get() + 1;
        self.scan_generation.set(generation);
        // Computed here (cheap, GIO's app cache) and handed to the worker.
        let skip = desktop::exec_targets();
        let this = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let found = gio::spawn_blocking(move || {
                appimage::scan(&appimage::default_dirs(), &appimage::default_cache_dir(), &skip)
            })
            .await;
            let (Some(l), Ok(found)) = (this.upgrade(), found) else { return };
            if l.scan_generation.get() != generation {
                return;
            }
            trace("appimages", &found.iter().map(|e| e.name.as_str()).collect::<Vec<_>>().join(";"));
            *l.appimage_entries.borrow_mut() = found;
            l.rebuild_index();
        });
    }

    pub fn toggle(&self) {
        if self.window.is_visible() {
            self.hide();
        } else {
            self.show();
        }
    }

    pub fn show(&self) {
        trace("show", "");
        if !self.window.is_visible() {
            self.entry.set_text("");
        }
        self.window.present();
        self.entry.grab_focus();
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    /// Text the helper extension captured between the hotkey and our window getting focus.
    pub fn prefill(&self, text: &str) {
        let combined = format!("{}{}", self.entry.text(), text);
        self.entry.set_text(&combined);
        self.entry.set_position(-1);
    }

    fn reload_desktop(self: &Rc<Self>) {
        *self.desktop_entries.borrow_mut() = desktop::load_entries();
        self.rebuild_index();
        // A new desktop entry may integrate an AppImage we list separately (or stop doing so).
        self.rescan_appimages();
    }

    fn rebuild_index(&self) {
        let mut all = self.desktop_entries.borrow().clone();
        all.extend(self.appimage_entries.borrow().iter().cloned());
        if self.config.system_actions {
            all.extend(actions::entries());
        }
        *self.index.borrow_mut() = Index::new(all);
        if self.window.is_visible() {
            self.refresh();
        }
    }

    fn refresh(&self) {
        let query = self.entry.text();
        trace("text", &query);
        let mut index = self.index.borrow_mut();
        let items =
            results::build(&mut index, &self.history.borrow(), &self.config, &query, MAX_RESULTS, history::now());

        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for item in &items {
            self.list.append(&self.item_row(&mut index, item, &query));
        }
        trace("results", &items.iter().map(|i| i.describe(&index)).collect::<Vec<_>>().join(";"));
        let any = !items.is_empty();
        let none = !any && !query.trim().is_empty();
        self.list.set_visible(any);
        self.empty.set_visible(none);
        self.separator.set_visible(any || none);
        if let Some(first) = self.list.row_at_index(0) {
            self.list.select_row(Some(&first));
        }
        *self.items.borrow_mut() = items;
    }

    fn item_row(&self, index: &mut Index, item: &Item, query: &str) -> gtk::ListBoxRow {
        let themed = |name: &str| -> gio::Icon { gio::ThemedIcon::new(name).upcast() };
        let esc = |s: &str| glib::markup_escape_text(s).to_string();
        match item {
            Item::Entry(hit) => {
                let entry = &index.entries()[hit.index];
                let icon = entry
                    .icon
                    .as_deref()
                    .and_then(|s| gio::Icon::for_string(s).ok())
                    .unwrap_or_else(|| themed("application-x-executable"));
                let (name, subtitle, tag) = (entry.name.clone(), entry.subtitle().map(str::to_owned), entry.kind_label());
                let marks = index.highlight(hit.index, query);
                result_row(&icon, &highlight_markup(&name, &marks), subtitle.as_deref(), tag, false)
            }
            Item::Calc(a) => result_row(
                &themed("accessories-calculator-symbolic"),
                &format!("= {}", esc(&a.text)),
                Some(&format!("{} · Enter copies the result", a.expr)),
                None,
                true,
            ),
            Item::Open(Target::Url { uri, .. }) => {
                let web = uri.starts_with("http://") || uri.starts_with("https://");
                result_row(
                    &themed(if web { "web-browser-symbolic" } else { "emblem-shared-symbolic" }),
                    &esc(uri),
                    Some(if web { "Open in browser" } else { "Open link" }),
                    None,
                    false,
                )
            }
            Item::Open(Target::Path { path, is_dir }) => {
                let icon = gio::File::for_path(path)
                    .query_info("standard::icon", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
                    .ok()
                    .and_then(|i| i.icon())
                    .unwrap_or_else(|| themed(if *is_dir { "folder" } else { "text-x-generic" }));
                result_row(
                    &icon,
                    &esc(&open::display_path(path)),
                    Some(if *is_dir { "Open folder" } else { "Open file" }),
                    None,
                    false,
                )
            }
            Item::Web { query, .. } => result_row(
                &themed("system-search-symbolic"),
                &format!("Search the web for “{}”", esc(query)),
                Some(&self.config.web_search_engine()),
                None,
                false,
            ),
        }
    }

    fn move_selection(&self, delta: i32) {
        let n = self.items.borrow().len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.list.selected_row().map(|r| r.index()).unwrap_or(0);
        let next = (cur + delta).rem_euclid(n);
        if let Some(row) = self.list.row_at_index(next) {
            self.list.select_row(Some(&row));
        }
    }

    pub fn forget_history(&self) {
        if let Err(e) = self.history.borrow_mut().clear() {
            eprintln!("spot: could not delete history: {e}");
        }
        trace("history", "cleared");
    }

    /// Enter pressed before the window had focus (forwarded by the helper extension).
    pub fn accept(&self) {
        self.launch_selected();
    }

    fn launch_selected(&self) {
        if let Some(row) = self.list.selected_row() {
            self.launch(row.index() as usize);
        }
    }

    fn launch(&self, position: usize) {
        let Some(item) = self.items.borrow().get(position).cloned() else { return };
        // The display's launch context hands the new app an activation token -> it gets focus.
        let ctx = WidgetExt::display(&self.window).app_launch_context();
        let ctx = Some(ctx.upcast_ref::<gio::AppLaunchContext>());
        match item {
            Item::Entry(hit) => {
                let entry = self.index.borrow().entries()[hit.index].clone();
                trace("launch", &entry.id);
                if let Err(e) = spot_core::launch(&entry, ctx) {
                    eprintln!("spot: {e}");
                }
                let query = self.entry.text();
                if let Err(e) = self.history.borrow_mut().record(&entry.id, &query, history::now()) {
                    eprintln!("spot: could not save history: {e}");
                }
            }
            Item::Calc(a) => {
                // Set while we still have focus (Wayland only lets the focused client set it);
                // the resident process keeps serving it after the window hides.
                WidgetExt::clipboard(&self.window).set_text(&a.text);
                trace("copy", &a.text);
            }
            Item::Open(t) => {
                trace("open", &t.uri());
                if let Err(e) = t.open(ctx) {
                    eprintln!("spot: could not open {}: {e}", t.uri());
                }
            }
            Item::Web { uri, .. } => {
                trace("open", &uri);
                if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, ctx) {
                    eprintln!("spot: could not open {uri}: {e}");
                }
            }
        }
        self.hide();
    }

    /// Resolve every entry's icon once, off the hot path, so the first query after login
    /// doesn't pay for icon-theme lookups (Phase 0 measured ~120 ms on first show).
    fn warm_icon_cache(&self) {
        let theme = gtk::IconTheme::for_display(&WidgetExt::display(&self.window));
        let icons: Vec<String> = self.index.borrow().entries().iter().filter_map(|e| e.icon.clone()).collect();
        let mut iter = icons.into_iter();
        glib::idle_add_local(move || {
            for name in iter.by_ref().take(16) {
                if let Ok(icon) = gio::Icon::for_string(&name) {
                    let _ = theme.lookup_by_gicon(&icon, ICON_SIZE, 1, gtk::TextDirection::None, gtk::IconLookupFlags::empty());
                }
            }
            if iter.len() == 0 { glib::ControlFlow::Break } else { glib::ControlFlow::Continue }
        });
    }
}

/// Pango markup for `name` with the chars at `marks` (sorted char indices) in bold.
fn highlight_markup(name: &str, marks: &[usize]) -> String {
    let mut out = String::new();
    let mut bold = false;
    let mut buf = [0u8; 4];
    for (i, c) in name.chars().enumerate() {
        let mark = marks.binary_search(&i).is_ok();
        if mark != bold {
            out.push_str(if mark { "<b>" } else { "</b>" });
            bold = mark;
        }
        out.push_str(&glib::markup_escape_text(c.encode_utf8(&mut buf)));
    }
    if bold {
        out.push_str("</b>");
    }
    out
}

/// `name_markup` must already be escaped Pango markup.
fn result_row(
    icon: &gio::Icon,
    name_markup: &str,
    subtitle: Option<&str>,
    tag: Option<&str>,
    answer: bool,
) -> gtk::ListBoxRow {
    let image = gtk::Image::from_gicon(icon);
    image.set_pixel_size(ICON_SIZE);

    // Ellipsizing labels with a tiny max-width-chars report a small natural width, so long
    // names/comments get cut off instead of widening the (non-resizable) window.
    let name = gtk::Label::builder()
        .label(name_markup)
        .use_markup(true)
        .xalign(0.0)
        .hexpand(true)
        .max_width_chars(1)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    name.add_css_class(if answer { "spot-answer" } else { "spot-name" });
    let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    text.set_valign(gtk::Align::Center);
    text.append(&name);
    if let Some(sub) = subtitle {
        let sub = gtk::Label::builder()
            .label(sub)
            .xalign(0.0)
            .hexpand(true)
            .max_width_chars(1)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        sub.add_css_class("spot-subtitle");
        text.append(&sub);
    }

    let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row_box.append(&image);
    row_box.append(&text);
    if let Some(tag) = tag {
        let tag = gtk::Label::builder().label(tag).valign(gtk::Align::Center).build();
        tag.add_css_class("spot-tag");
        row_box.append(&tag);
    }
    // Rows never take focus: typing must always go to the search field.
    gtk::ListBoxRow::builder().child(&row_box).focusable(false).build()
}
