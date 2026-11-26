//! Desktop applications, discovered and launched through GIO.
//!
//! GIO already implements the parts of the XDG specs that are easy to get subtly wrong:
//! data-dir precedence (a user file shadows the system one with the same id), `NoDisplay`,
//! `Hidden`, `OnlyShowIn`/`NotShowIn`, `TryExec`, localized keys, `%U`/`%f` field codes,
//! `Terminal=true` and `DBusActivatable`. Using it also means Spot sees exactly the app set
//! GNOME's own overview sees.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use gio_unix::prelude::*;
use gio_unix::DesktopAppInfo;

use crate::entry::{Entry, EntryKind};
use crate::LaunchError;

/// Every application the desktop would show in its app grid.
pub fn load_entries() -> Vec<Entry> {
    let mut entries: Vec<Entry> = gio::AppInfo::all()
        .into_iter()
        .filter(|a| a.should_show())
        .filter_map(|a| a.downcast::<DesktopAppInfo>().ok())
        .filter_map(|d| entry_from_app_info(&d))
        .collect();
    entries.sort_by_key(|e| e.name.to_lowercase());
    entries
}

/// Parse a single desktop file (used by tests and, later, for files outside XDG dirs).
pub fn entry_from_file(path: &Path) -> Option<Entry> {
    DesktopAppInfo::from_filename(path).and_then(|d| entry_from_app_info(&d))
}

fn entry_from_app_info(info: &DesktopAppInfo) -> Option<Entry> {
    // Files parsed with from_filename have no id; fall back to the file name.
    let id = info.id().map(|s| s.to_string()).or_else(|| {
        info.filename()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
    })?;
    let non_empty = |s: Option<glib::GString>| s.map(|s| s.to_string()).filter(|s| !s.trim().is_empty());
    Some(Entry {
        id,
        name: info.display_name().to_string(),
        generic_name: non_empty(info.generic_name()),
        comment: non_empty(info.description()),
        keywords: info.keywords().iter().map(|k| k.to_string()).collect(),
        exec_name: info
            .executable()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
        icon: info.icon().and_then(|i| IconExt::to_string(&i)).map(|s| s.to_string()),
        kind: EntryKind::Desktop,
    })
}

/// Every absolute path mentioned in an installed app's `Exec=` line, canonicalized. AppImages
/// that already have a desktop entry (AppImageLauncher, Gear Lever, the app's own installer)
/// are in here and are not listed twice.
pub fn exec_targets() -> HashSet<PathBuf> {
    gio::AppInfo::all()
        .into_iter()
        .filter_map(|a| a.commandline())
        .filter_map(|c| glib::shell_parse_argv(c.as_os_str()).ok())
        .flatten()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .filter_map(|p| p.canonicalize().ok())
        .collect()
}

/// Launch a desktop application by id.
pub fn launch(id: &str, ctx: Option<&gio::AppLaunchContext>) -> Result<(), LaunchError> {
    let info = DesktopAppInfo::new(id).ok_or_else(|| LaunchError::NotFound(id.to_owned()))?;
    info.launch(&[], ctx).map_err(LaunchError::Gio)
}

// gio re-exports glib; keep the path short above.
use gio::glib;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn desktop_file(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::File::create(&p).unwrap().write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn parses_fields_from_desktop_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = desktop_file(
            dir.path(),
            "org.example.Files.desktop",
            "[Desktop Entry]\nType=Application\nName=Files\nGenericName=File Manager\n\
             Comment=Access and organize files\nKeywords=folder;manager;explore;\n\
             Exec=/usr/bin/nautilus --new-window %U\nIcon=org.gnome.Nautilus\n",
        );
        let e = entry_from_file(&p).expect("parsed");
        assert_eq!(e.id, "org.example.Files.desktop");
        assert_eq!(e.name, "Files");
        assert_eq!(e.generic_name.as_deref(), Some("File Manager"));
        assert_eq!(e.comment.as_deref(), Some("Access and organize files"));
        assert_eq!(e.keywords, vec!["folder", "manager", "explore"]);
        assert_eq!(e.exec_name.as_deref(), Some("nautilus"));
        assert_eq!(e.icon.as_deref(), Some("org.gnome.Nautilus"));
        assert_eq!(e.subtitle(), Some("Access and organize files"));
    }

    #[test]
    fn missing_optional_fields_are_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = desktop_file(dir.path(), "min.desktop", "[Desktop Entry]\nType=Application\nName=Min\nExec=true\n");
        let e = entry_from_file(&p).unwrap();
        assert_eq!(e.generic_name, None);
        assert_eq!(e.comment, None);
        assert!(e.keywords.is_empty());
        assert_eq!(e.subtitle(), None);
    }

    #[test]
    fn invalid_file_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = desktop_file(dir.path(), "bad.desktop", "[Desktop Entry]\nType=Link\nName=Not an app\nURL=https://x\n");
        assert!(entry_from_file(&p).is_none());
    }
}
