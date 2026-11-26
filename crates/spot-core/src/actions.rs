//! System actions as searchable entries: Lock Screen, Log Out, Suspend, Restart, Power Off.
//!
//! They go through the same paths as GNOME's own system menu, so the usual confirmations
//! apply: Log Out / Restart / Power Off open GNOME's end-session dialog (with its countdown)
//! instead of acting immediately. Suspend and Lock act immediately, as in the system menu.

use gio::glib;
use gio::prelude::*;

use crate::entry::{Entry, EntryKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemAction {
    Lock,
    LogOut,
    Suspend,
    Restart,
    PowerOff,
}

pub const ALL: [SystemAction; 5] =
    [SystemAction::Lock, SystemAction::LogOut, SystemAction::Suspend, SystemAction::Restart, SystemAction::PowerOff];

impl SystemAction {
    fn key(self) -> &'static str {
        match self {
            SystemAction::Lock => "lock",
            SystemAction::LogOut => "log-out",
            SystemAction::Suspend => "suspend",
            SystemAction::Restart => "restart",
            SystemAction::PowerOff => "power-off",
        }
    }

    pub fn entry(self) -> Entry {
        let (name, comment, keywords, icon): (&str, &str, &[&str], &str) = match self {
            SystemAction::Lock => ("Lock Screen", "Lock the screen", &["lock", "screensaver", "away"], "system-lock-screen-symbolic"),
            SystemAction::LogOut => ("Log Out", "End the session (asks first)", &["logout", "sign out", "signout", "exit", "session"], "system-log-out-symbolic"),
            SystemAction::Suspend => ("Suspend", "Put the computer to sleep", &["sleep", "standby"], "media-playback-pause-symbolic"),
            SystemAction::Restart => ("Restart", "Restart the computer (asks first)", &["reboot"], "system-reboot-symbolic"),
            SystemAction::PowerOff => ("Power Off", "Shut down the computer (asks first)", &["shutdown", "shut down", "turn off", "halt"], "system-shutdown-symbolic"),
        };
        Entry {
            id: format!("spot-action:{}", self.key()),
            name: name.into(),
            generic_name: None,
            comment: Some(comment.into()),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            exec_name: None,
            icon: Some(icon.into()),
            kind: EntryKind::Action(self),
        }
    }

    /// (bus, service, object path, interface, method, arguments)
    fn call(self) -> (gio::BusType, &'static str, &'static str, &'static str, &'static str, Option<glib::Variant>) {
        const SM: (&str, &str, &str) = ("org.gnome.SessionManager", "/org/gnome/SessionManager", "org.gnome.SessionManager");
        match self {
            SystemAction::Lock => {
                (gio::BusType::Session, "org.gnome.ScreenSaver", "/org/gnome/ScreenSaver", "org.gnome.ScreenSaver", "Lock", None)
            }
            // Mode 0 = normal: GNOME shows its confirmation dialog.
            SystemAction::LogOut => (gio::BusType::Session, SM.0, SM.1, SM.2, "Logout", Some((0u32,).to_variant())),
            SystemAction::Restart => (gio::BusType::Session, SM.0, SM.1, SM.2, "Reboot", None),
            SystemAction::PowerOff => (gio::BusType::Session, SM.0, SM.1, SM.2, "Shutdown", None),
            // interactive=true: polkit may ask for a password if policy requires it.
            SystemAction::Suspend => (
                gio::BusType::System,
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
                "Suspend",
                Some((true,).to_variant()),
            ),
        }
    }

    /// Start the action. Returns once the request is sent; a failure of the call itself is
    /// reported on stderr when it comes back (needs a running main loop, as in the app).
    pub fn run(self) -> Result<(), glib::Error> {
        let (bus, service, path, iface, method, args) = self.call();
        let conn = gio::bus_get_sync(bus, gio::Cancellable::NONE)?;
        conn.call(
            Some(service),
            path,
            iface,
            method,
            args.as_ref(),
            None,
            gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION,
            -1,
            gio::Cancellable::NONE,
            move |r| {
                if let Err(e) = r {
                    eprintln!("spot: {method} failed: {e}");
                }
            },
        );
        Ok(())
    }
}

pub fn entries() -> Vec<Entry> {
    ALL.iter().map(|a| a.entry()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::Index;

    #[test]
    fn actions_are_found_by_name_and_synonyms() {
        let mut idx = Index::new(entries());
        let top = |idx: &mut Index, q: &str| idx.search(q, 1).first().map(|h| idx.entries()[h.index].name.clone());
        assert_eq!(top(&mut idx, "lock").as_deref(), Some("Lock Screen"));
        assert_eq!(top(&mut idx, "shutdown").as_deref(), Some("Power Off"));
        assert_eq!(top(&mut idx, "reboot").as_deref(), Some("Restart"));
        assert_eq!(top(&mut idx, "sleep").as_deref(), Some("Suspend"));
        assert_eq!(top(&mut idx, "sign out").as_deref(), Some("Log Out"));
        assert!(entries().iter().all(|e| e.id.starts_with("spot-action:")));
    }
}
