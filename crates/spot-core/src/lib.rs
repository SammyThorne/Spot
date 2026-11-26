//! GUI-free core of Spot: what can be launched (entries), how it is found (sources),
//! how it is ranked (search) and how it is started (launch).

pub mod actions;
pub mod appimage;
pub mod calc;
pub mod config;
pub mod desktop;
pub mod entry;
pub mod history;
pub mod open;
pub mod results;
pub mod search;
pub mod setup;

pub use entry::{AppImage, Entry, EntryKind};
pub use config::Config;
pub use history::History;
pub use results::Item;
pub use search::{Hit, Index};

use std::fmt;

#[derive(Debug)]
pub enum LaunchError {
    NotFound(String),
    Io(std::io::Error),
    Gio(gio::glib::Error),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::NotFound(what) => write!(f, "{what} not found"),
            LaunchError::Io(e) => write!(f, "launch failed: {e}"),
            LaunchError::Gio(e) => write!(f, "launch failed: {e}"),
        }
    }
}

impl std::error::Error for LaunchError {}

/// Start an entry. Pass the display's launch context from the UI so the new app receives an
/// XDG activation token (it then gets focus instead of an "is ready" notification).
pub fn launch(entry: &Entry, ctx: Option<&gio::AppLaunchContext>) -> Result<(), LaunchError> {
    match &entry.kind {
        EntryKind::Desktop => desktop::launch(&entry.id, ctx),
        EntryKind::AppImage(app) => appimage::launch(app, &entry.name, ctx),
        EntryKind::Action(action) => action.run().map_err(LaunchError::Gio),
    }
}
