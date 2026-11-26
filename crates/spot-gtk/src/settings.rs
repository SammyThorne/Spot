//! Settings and first-run setup window.
//!
//! Opened by clicking Spot in the app grid (D-Bus `Activate`), by picking "Spot" in Spot
//! itself, or with `spot --settings`. It checks the things a package can't do for you:
//! - the helper extension is installed but GNOME only loads new extensions at login, and each
//!   user has to turn it on;
//! - GNOME binds Super+Space to "switch keyboard layout" by default;
//! - staying resident (autostart) is a per-user choice.
//!
//! Every change here is an explicit button press; nothing is changed just by opening it.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use spot_core::setup::{self, EXTENSION_SCHEMA, EXTENSION_UUID};

use crate::launcher::trace;

/// GSettings schemas holding GNOME's global shortcuts, checked for clashes with Spot's.
const KEYBINDING_SCHEMAS: [&str; 4] = [
    "org.gnome.desktop.wm.keybindings",
    "org.gnome.shell.keybindings",
    "org.gnome.mutter.keybindings",
    "org.gnome.settings-daemon.plugins.media-keys",
];

#[derive(Debug, Clone, PartialEq)]
enum ExtState {
    Active,
    /// Loaded by the shell but not turned on.
    Inactive,
    /// Turned on in settings but not loaded yet: installed after this login started.
    PendingLogin,
    /// On disk, unknown to the running shell.
    NotLoaded,
    Error(String),
    NotInstalled,
    /// No GNOME Shell on the session bus.
    NoShell,
}

impl ExtState {
    fn key(&self) -> &'static str {
        match self {
            ExtState::Active => "active",
            ExtState::Inactive => "inactive",
            ExtState::PendingLogin => "pending-login",
            ExtState::NotLoaded => "not-loaded",
            ExtState::Error(_) => "error",
            ExtState::NotInstalled => "not-installed",
            ExtState::NoShell => "no-shell",
        }
    }
}

fn settings_if_installed(schema: &str) -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()?.lookup(schema, true).map(|_| gio::Settings::new(schema))
}

fn shell_call(method: &str, args: glib::Variant, reply: &str) -> Result<glib::Variant, glib::Error> {
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
    // The interface is org.gnome.Shell.Extensions, but the bus name is the shell's own;
    // "org.gnome.Shell.Extensions" as a *name* is the Extensions prefs app.
    conn.call_sync(
        Some("org.gnome.Shell"),
        "/org/gnome/Shell",
        "org.gnome.Shell.Extensions",
        method,
        Some(&args),
        Some(glib::VariantTy::new(reply).expect("valid type")),
        gio::DBusCallFlags::NO_AUTO_START,
        2000,
        gio::Cancellable::NONE,
    )
}

fn extension_state() -> ExtState {
    let installed = setup::extension_dir().is_some();
    let info = match shell_call("GetExtensionInfo", (EXTENSION_UUID,).to_variant(), "(a{sv})") {
        Ok(v) => glib::VariantDict::new(Some(&v.child_value(0))),
        Err(_) => return if installed { ExtState::NoShell } else { ExtState::NotInstalled },
    };
    // GNOME 45+: ACTIVE=1 INACTIVE=2 ERROR=3 OUT_OF_DATE=4 … UNINSTALLED=99
    match info.lookup::<f64>("state").ok().flatten().map(|s| s as i32) {
        Some(1) => ExtState::Active,
        Some(3) => ExtState::Error(info.lookup::<String>("error").ok().flatten().unwrap_or_default()),
        Some(4) => ExtState::Error("not compatible with this version of GNOME".into()),
        Some(99) | None if !installed => ExtState::NotInstalled,
        Some(99) | None => {
            let enabled = settings_if_installed("org.gnome.shell")
                .is_some_and(|s| s.strv("enabled-extensions").iter().any(|u| u == EXTENSION_UUID));
            if enabled { ExtState::PendingLogin } else { ExtState::NotLoaded }
        }
        Some(_) => ExtState::Inactive,
    }
}

/// Spot's shortcuts, from the helper extension's own settings schema.
fn spot_shortcuts() -> Vec<String> {
    let from_ext = setup::extension_dir().and_then(|dir| {
        let src = gio::SettingsSchemaSource::from_directory(
            dir.join("schemas"),
            gio::SettingsSchemaSource::default().as_ref(),
            false,
        )
        .ok()?;
        let schema = src.lookup(EXTENSION_SCHEMA, false)?;
        let s = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
        Some(s.strv("toggle").iter().map(|a| a.to_string()).collect::<Vec<_>>())
    });
    from_ext.filter(|v| !v.is_empty()).unwrap_or_else(|| vec![setup::DEFAULT_SHORTCUT.into()])
}

/// GNOME shortcuts that use one of Spot's keys: (schema, key, human description).
fn conflicts(shortcuts: &[String]) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for schema_id in KEYBINDING_SCHEMAS {
        let Some(schema) = gio::SettingsSchemaSource::default().and_then(|s| s.lookup(schema_id, true)) else {
            continue;
        };
        let settings = gio::Settings::new(schema_id);
        for key in schema.list_keys() {
            let k = schema.key(&key);
            if k.value_type().as_str() != "as" {
                continue;
            }
            let bound = settings.strv(&key);
            if bound.iter().any(|b| shortcuts.iter().any(|s| setup::same_accel(b, s))) {
                let what = k.summary().map(|s| s.to_string()).unwrap_or_else(|| key.to_string());
                out.push((schema_id.to_owned(), key.to_string(), what));
            }
        }
    }
    out
}

pub struct SettingsWindow {
    window: adw::ApplicationWindow,
    ext_row: adw::ActionRow,
    ext_button: gtk::Button,
    shortcut_row: adw::ActionRow,
    conflict_row: adw::ActionRow,
    autostart_row: adw::SwitchRow,
    /// Set while refresh() updates widgets, so their change signals don't act.
    updating: Cell<bool>,
}

impl SettingsWindow {
    pub fn new(app: &adw::Application) -> Rc<Self> {
        let page = adw::PreferencesPage::new();

        let shortcut_group = adw::PreferencesGroup::builder().title("Keyboard shortcut").build();
        let ext_button = gtk::Button::builder()
            .label("Turn On")
            .valign(gtk::Align::Center)
            .action_name("app.enable-extension")
            .build();
        ext_button.add_css_class("suggested-action");
        let ext_row = adw::ActionRow::builder().title("Spot helper extension").build();
        ext_row.add_suffix(&ext_button);
        let shortcut_row = adw::ActionRow::builder().title("Open Spot").build();
        let conflict_button = gtk::Button::builder()
            .label("Use for Spot")
            .valign(gtk::Align::Center)
            .action_name("app.free-shortcut")
            .build();
        let conflict_row = adw::ActionRow::builder().title("Shortcut already in use").build();
        conflict_row.add_css_class("warning");
        conflict_row.add_suffix(&conflict_button);
        shortcut_group.add(&ext_row);
        shortcut_group.add(&shortcut_row);
        shortcut_group.add(&conflict_row);

        let general = adw::PreferencesGroup::builder().title("General").build();
        let autostart_row = adw::SwitchRow::builder()
            .title("Start at login")
            .subtitle("Keeps Spot ready in the background so it opens instantly")
            .build();
        let forget = gtk::Button::builder()
            .label("Forget…")
            .valign(gtk::Align::Center)
            .action_name("app.forget-history")
            .build();
        let history_row = adw::ActionRow::builder()
            .title("Launch history")
            .subtitle("Spot ranks what you open often higher. Stored only on this computer.")
            .build();
        history_row.add_suffix(&forget);
        general.add(&autostart_row);
        general.add(&history_row);

        let about = adw::PreferencesGroup::builder()
            .description(format!(
                "Spot {} · Settings for calculator, web search and more are in {}",
                env!("CARGO_PKG_VERSION"),
                spot_core::open::display_path(&spot_core::config::default_path())
            ))
            .build();

        page.add(&shortcut_group);
        page.add(&general);
        page.add(&about);

        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&page));
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Spot Settings")
            .icon_name(setup::APP_ID)
            .default_width(600)
            .default_height(560)
            .hide_on_close(true)
            .content(&view)
            .build();

        let this = Rc::new(SettingsWindow {
            window,
            ext_row,
            ext_button,
            shortcut_row,
            conflict_row,
            autostart_row,
            updating: Cell::new(false),
        });

        let weak = Rc::downgrade(&this);
        this.autostart_row.connect_active_notify(move |row| {
            let Some(s) = weak.upgrade() else { return };
            if s.updating.get() {
                return;
            }
            let exe = std::env::current_exe().unwrap_or_else(|_| "spot".into());
            if let Err(e) = setup::set_autostart(&setup::autostart_path(), &exe, row.is_active()) {
                eprintln!("spot: could not change autostart: {e}");
            }
            s.refresh();
        });
        // Re-check when coming back to the window (e.g. after using the Extensions app).
        let weak = Rc::downgrade(&this);
        this.window.connect_is_active_notify(move |w| {
            if w.is_active() {
                if let Some(s) = weak.upgrade() {
                    s.refresh();
                }
            }
        });
        this
    }

    pub fn present(&self) {
        self.refresh();
        self.window.present();
    }

    pub fn refresh(&self) {
        self.updating.set(true);
        let state = extension_state();
        let user_ext_off = settings_if_installed("org.gnome.shell").is_some_and(|s| s.boolean("disable-user-extensions"));
        let (subtitle, button) = match &state {
            ExtState::Active => ("On — owns the shortcut and keeps what you type before Spot appears".to_owned(), None),
            ExtState::Inactive => ("Off — Spot's shortcut won't work until it is on".to_owned(), Some("Turn On")),
            ExtState::NotLoaded => (
                "Installed after you logged in. Turn it on, then log out and back in.".to_owned(),
                Some("Turn On"),
            ),
            ExtState::PendingLogin => ("Turns on the next time you log in".to_owned(), None),
            ExtState::Error(e) => (format!("Failed to load: {e}"), None),
            ExtState::NotInstalled => ("Not installed. Reinstall Spot to get it.".to_owned(), None),
            ExtState::NoShell => (
                "GNOME Shell isn't running. Bind a custom shortcut to “spot --toggle” instead.".to_owned(),
                None,
            ),
        };
        let subtitle = if user_ext_off && state != ExtState::NoShell {
            format!("{subtitle}\nExtensions are switched off for this user (Extensions app).")
        } else {
            subtitle
        };
        self.ext_row.set_subtitle(&glib::markup_escape_text(&subtitle));
        self.ext_button.set_visible(button.is_some());

        let shortcuts = spot_shortcuts();
        let labels: Vec<String> = shortcuts.iter().map(|s| setup::accel_label(s)).collect();
        self.shortcut_row.set_subtitle(&glib::markup_escape_text(&labels.join(", ")));
        let clashes = conflicts(&shortcuts);
        self.conflict_row.set_visible(!clashes.is_empty());
        if !clashes.is_empty() {
            let what: Vec<&str> = clashes.iter().map(|c| c.2.as_str()).collect();
            self.conflict_row.set_subtitle(&glib::markup_escape_text(&format!(
                "GNOME also uses {} for: {}",
                labels.join(", "),
                what.join("; ")
            )));
        }

        let autostart = setup::autostart_enabled(&setup::autostart_path());
        self.autostart_row.set_active(autostart);
        self.updating.set(false);

        trace(
            "settings",
            &format!(
                "extension={};shortcut={};conflicts={};autostart={autostart}",
                state.key(),
                shortcuts.join(","),
                clashes.iter().map(|c| c.1.as_str()).collect::<Vec<_>>().join(",")
            ),
        );
    }

    /// Remove Spot's shortcut from every GNOME shortcut that also uses it.
    pub fn free_shortcut(&self) {
        let shortcuts = spot_shortcuts();
        for (schema, key, _) in conflicts(&shortcuts) {
            let s = gio::Settings::new(&schema);
            let mut list: Vec<String> = s.strv(&key).iter().map(|a| a.to_string()).collect();
            for sc in &shortcuts {
                list = setup::without_accel(&list, sc);
            }
            let refs: Vec<&str> = list.iter().map(String::as_str).collect();
            if let Err(e) = s.set_strv(&key, refs.as_slice()) {
                eprintln!("spot: could not change {schema} {key}: {e}");
            }
        }
        gio::Settings::sync();
        self.refresh();
    }

    pub fn enable_extension(self: &Rc<Self>) {
        match extension_state() {
            ExtState::Inactive => {
                if let Err(e) = shell_call("EnableExtension", (EXTENSION_UUID,).to_variant(), "(b)") {
                    eprintln!("spot: could not turn on the extension: {e}");
                }
            }
            // The shell hasn't seen it yet: put it on the list it loads at login.
            ExtState::NotLoaded => {
                if let Some(s) = settings_if_installed("org.gnome.shell") {
                    let mut on: Vec<String> = s.strv("enabled-extensions").iter().map(|u| u.to_string()).collect();
                    if !on.iter().any(|u| u == EXTENSION_UUID) {
                        on.push(EXTENSION_UUID.into());
                    }
                    let off: Vec<String> =
                        s.strv("disabled-extensions").iter().map(|u| u.to_string()).filter(|u| u != EXTENSION_UUID).collect();
                    let on: Vec<&str> = on.iter().map(String::as_str).collect();
                    let off: Vec<&str> = off.iter().map(String::as_str).collect();
                    let _ = s.set_strv("enabled-extensions", on.as_slice());
                    let _ = s.set_strv("disabled-extensions", off.as_slice());
                    gio::Settings::sync();
                }
            }
            _ => {}
        }
        // The shell enables asynchronously; look again shortly.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            if let Some(s) = weak.upgrade().filter(|s| s.window.is_visible()) {
                s.refresh();
            }
        });
        self.refresh();
    }

    /// Ask first; forgetting can't be undone.
    pub fn confirm_forget_history(&self, forget: impl Fn() + 'static) {
        let dialog = adw::MessageDialog::builder()
            .transient_for(&self.window)
            .modal(true)
            .heading("Forget launch history?")
            .body("Spot will stop ranking the apps you use most first, until it learns again.")
            .build();
        dialog.add_responses(&[("cancel", "Cancel"), ("forget", "Forget")]);
        dialog.set_response_appearance("forget", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.connect_response(None, move |_, r| {
            if r == "forget" {
                forget();
            }
        });
        dialog.present();
    }
}
