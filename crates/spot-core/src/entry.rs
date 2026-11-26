use std::path::PathBuf;

/// Something the launcher can show and start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Stable identifier. Desktop apps: the desktop-file id (`firefox-esr.desktop`);
    /// AppImages: `appimage:` + absolute path.
    pub id: String,
    /// Localized display name; the primary search field.
    pub name: String,
    /// e.g. "Web Browser".
    pub generic_name: Option<String>,
    /// One-line description shown under the name.
    pub comment: Option<String>,
    /// `Keywords=` from the desktop file (localized).
    pub keywords: Vec<String>,
    /// Basename of the executable (`nautilus` for Files), so users can search by command.
    pub exec_name: Option<String>,
    /// Serialized `GIcon` (theme name or path), resolved by the UI's icon theme.
    pub icon: Option<String>,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// Installed application described by a `.desktop` file found via XDG data dirs
    /// (covers distro packages, Flatpak and Snap exports, and `~/.local/share/applications`).
    Desktop,
    /// A portable AppImage file found in one of the scanned folders.
    AppImage(AppImage),
    /// Lock, log out, suspend, restart, power off.
    Action(crate::actions::SystemAction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppImage {
    pub path: PathBuf,
    /// The embedded runtime is the old AppImageKit one, which loads libfuse.so.2 at run time.
    pub needs_libfuse2: bool,
    /// `StartupWMClass` from the embedded desktop file, so the shell can match the window.
    pub wm_class: Option<String>,
}

impl Entry {
    /// The line shown under the name in the result list.
    pub fn subtitle(&self) -> Option<&str> {
        self.comment.as_deref().or(self.generic_name.as_deref())
    }

    /// Short tag shown next to results that aren't ordinary installed apps.
    pub fn kind_label(&self) -> Option<&'static str> {
        match self.kind {
            EntryKind::Desktop => None,
            EntryKind::AppImage(_) => Some("AppImage"),
            EntryKind::Action(_) => Some("System"),
        }
    }
}
