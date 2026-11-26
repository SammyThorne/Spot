//! `spot`: resident GTK4/libadwaita launcher.
//!
//! One primary instance stays running (hidden) and exports D-Bus actions on
//! `dev.spot.Spot` at `/dev/spot/Spot` (interface `org.gtk.Actions`):
//!
//!   toggle, show, hide   — used by the shortcut (helper extension, portal or gsd)
//!   prefill(s)           — type-ahead captured by the helper extension before the window had focus
//!   accept               — Enter typed during that window: launch the top result
//!   settings             — open the settings / setup window
//!   free-shortcut, enable-extension, forget-history — the settings window's buttons
//!
//! `activate` (clicking Spot in the app grid, or picking it in Spot) opens the settings window:
//! the launcher itself is opened with the shortcut.
//!
//! Command line (forwarded to the primary instance if one is running):
//!   spot                 show the launcher (starts the primary if needed)
//!   spot --toggle        show/hide
//!   spot --background    start resident without showing (autostart)
//!   spot --settings      open the settings window
//!   spot --quit          stop the primary instance
//!   spot --version
//!   spot --search QUERY  print ranked results and exit (no display needed; for debugging)

mod launcher;
mod settings;

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use launcher::Launcher;
use settings::SettingsWindow;

pub const APP_ID: &str = "dev.spot.Spot";

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--search") => return cli_search(&args[2..].join(" ")),
        Some("--version" | "-V") => {
            println!("spot {}", env!("CARGO_PKG_VERSION"));
            return glib::ExitCode::SUCCESS;
        }
        _ => {}
    }

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    for (name, help) in [
        ("toggle", "Show or hide the launcher"),
        ("background", "Start resident without showing the launcher"),
        ("settings", "Open the settings window"),
        ("quit", "Quit the running instance"),
    ] {
        app.add_main_option(name, glib::Char::from(0), glib::OptionFlags::NONE, glib::OptionArg::None, help, None);
    }

    app.connect_startup(|app| {
        // Stay alive with no visible window; the launcher is shown on demand.
        std::mem::forget(app.hold());
        load_css();
        let launcher = Launcher::new(app);
        let settings = SettingsWindow::new(app);
        install_actions(app, &launcher, &settings);
    });

    // Activating the .desktop file (DBusActivatable=true: app grid, or Spot launching "Spot")
    // arrives as `activate`. The shortcut uses the actions instead.
    app.connect_activate(|app| app.activate_action("settings", None));

    app.connect_command_line(|app, cmd| {
        let opts = cmd.options_dict();
        if opts.contains("quit") {
            app.quit();
        } else if opts.contains("toggle") {
            app.activate_action("toggle", None);
        } else if opts.contains("settings") {
            app.activate_action("settings", None);
        } else if !opts.contains("background") {
            app.activate_action("show", None);
        }
        glib::ExitCode::SUCCESS
    });

    app.run()
}

fn install_actions(app: &adw::Application, launcher: &Rc<Launcher>, settings: &Rc<SettingsWindow>) {
    let l = launcher.clone();
    let toggle = gio::ActionEntry::builder("toggle").activate(move |_: &adw::Application, _, _| l.toggle()).build();
    let l = launcher.clone();
    let show = gio::ActionEntry::builder("show").activate(move |_: &adw::Application, _, _| l.show()).build();
    let l = launcher.clone();
    let hide = gio::ActionEntry::builder("hide").activate(move |_: &adw::Application, _, _| l.hide()).build();
    let l = launcher.clone();
    let prefill = gio::ActionEntry::builder("prefill")
        .parameter_type(Some(glib::VariantTy::STRING))
        .activate(move |_: &adw::Application, _, p| {
            if let Some(text) = p.and_then(|v| v.get::<String>()) {
                l.prefill(&text);
            }
        })
        .build();
    let l = launcher.clone();
    let accept = gio::ActionEntry::builder("accept").activate(move |_: &adw::Application, _, _| l.accept()).build();
    let s = settings.clone();
    let open_settings = gio::ActionEntry::builder("settings").activate(move |_: &adw::Application, _, _| s.present()).build();
    let s = settings.clone();
    let free_shortcut =
        gio::ActionEntry::builder("free-shortcut").activate(move |_: &adw::Application, _, _| s.free_shortcut()).build();
    let s = settings.clone();
    let enable_extension = gio::ActionEntry::builder("enable-extension")
        .activate(move |_: &adw::Application, _, _| s.enable_extension())
        .build();
    let (s, l) = (settings.clone(), launcher.clone());
    let forget_history = gio::ActionEntry::builder("forget-history")
        .activate(move |_: &adw::Application, _, _| {
            let l = l.clone();
            s.confirm_forget_history(move || l.forget_history());
        })
        .build();
    app.add_action_entries([
        toggle,
        show,
        hide,
        prefill,
        accept,
        open_settings,
        free_shortcut,
        enable_extension,
        forget_history,
    ]);
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("no display"),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn cli_search(query: &str) -> glib::ExitCode {
    use spot_core::{appimage, config, desktop, history, results, Config, EntryKind, History, Index, Item};
    let config = Config::load(&config::default_path());
    let t = std::time::Instant::now();
    let mut entries = desktop::load_entries();
    let load = t.elapsed();
    let t = std::time::Instant::now();
    let appimages = appimage::scan(&appimage::default_dirs(), &appimage::default_cache_dir(), &desktop::exec_targets());
    let scan = t.elapsed();
    let n_appimages = appimages.len();
    entries.extend(appimages);
    if config.system_actions {
        entries.extend(spot_core::actions::entries());
    }
    let mut index = Index::new(entries);
    let history = History::load(&history::default_path());
    let t = std::time::Instant::now();
    let items = results::build(&mut index, &history, &config, query, 10, history::now());
    let search = t.elapsed();
    for item in &items {
        match item {
            Item::Entry(h) => {
                let e = &index.entries()[h.index];
                let note = match &e.kind {
                    EntryKind::AppImage(a) => format!("  [{:?}]", appimage::launch_mode(a)),
                    _ => String::new(),
                };
                println!("{:>6}  {:<32} {}{note}", h.score, e.name, e.id);
            }
            other => println!("{:>6}  {}", "", other.describe(&index)),
        }
    }
    eprintln!(
        "{} entries ({} AppImages) loaded in {load:?} + {scan:?}; query {query:?} ranked in {search:?}",
        index.len(),
        n_appimages
    );
    glib::ExitCode::SUCCESS
}
