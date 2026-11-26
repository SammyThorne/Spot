//! First-run setup helpers (no GTK): shortcut conflicts, autostart, locating the helper extension.

use std::path::{Path, PathBuf};

use gio::glib;

pub const APP_ID: &str = "dev.spot.Spot";
pub const EXTENSION_UUID: &str = "spot-helper@spot.dev";
pub const EXTENSION_SCHEMA: &str = "org.gnome.shell.extensions.spot-helper";
pub const DEFAULT_SHORTCUT: &str = "<Super>space";

/// Modifiers and key of a GTK accelerator string, normalized for comparison
/// (`<Primary>`/`<Ctrl>`/`<Control>` are the same, key names are case-insensitive).
fn parse_accel(accel: &str) -> Option<(Vec<&'static str>, String)> {
    let mut mods = Vec::new();
    let mut rest = accel.trim();
    while let Some(after) = rest.strip_prefix('<') {
        let (m, tail) = after.split_once('>')?;
        mods.push(match m.to_ascii_lowercase().as_str() {
            "super" | "mod4" => "super",
            "shift" => "shift",
            "control" | "ctrl" | "primary" | "ctl" => "control",
            "alt" | "mod1" => "alt",
            "meta" => "meta",
            "hyper" => "hyper",
            _ => return None,
        });
        rest = tail;
    }
    if rest.is_empty() {
        return None;
    }
    mods.sort_unstable();
    mods.dedup();
    Some((mods, rest.to_ascii_lowercase()))
}

pub fn same_accel(a: &str, b: &str) -> bool {
    matches!((parse_accel(a), parse_accel(b)), (Some(x), Some(y)) if x == y)
}

/// `list` without entries equal to `accel`.
pub fn without_accel(list: &[String], accel: &str) -> Vec<String> {
    list.iter().filter(|a| !same_accel(a, accel)).cloned().collect()
}

/// "<Super>space" -> "Super+Space".
pub fn accel_label(accel: &str) -> String {
    let Some((mods, key)) = parse_accel(accel) else { return accel.to_owned() };
    let order = ["control", "alt", "shift", "super", "meta", "hyper"];
    let mut parts: Vec<String> = order
        .iter()
        .filter(|m| mods.contains(m))
        .map(|m| match *m {
            "control" => "Ctrl".to_owned(),
            other => capitalize(other),
        })
        .collect();
    parts.push(if key.chars().count() == 1 { key.to_uppercase() } else { capitalize(&key) });
    parts.join("+")
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

pub fn autostart_path() -> PathBuf {
    glib::user_config_dir().join("autostart").join(format!("{APP_ID}.desktop"))
}

pub fn autostart_contents(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Spot\nComment=Keep the Spot launcher ready in the background\n\
         Exec={} --background\nIcon={APP_ID}\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\nX-GNOME-AutoRestart=true\n",
        exec_quote(exe)
    )
}

fn exec_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) {
        s.into_owned()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('`', "\\`").replace('$', "\\$"))
    }
}

/// An autostart file that exists and isn't switched off (by `Hidden=true` or GNOME's key).
pub fn autostart_enabled(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    !text.lines().map(|l| l.replace(' ', "").to_ascii_lowercase()).any(|l| {
        l == "hidden=true" || l == "x-gnome-autostart-enabled=false"
    })
}

pub fn set_autostart(path: &Path, exe: &Path, on: bool) -> std::io::Result<()> {
    if on {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, autostart_contents(exe))
    } else {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Where GNOME Shell would load the helper extension from: the user's data dir first, then
/// the system data dirs (where a distro package puts it).
pub fn extension_dir() -> Option<PathBuf> {
    std::iter::once(glib::user_data_dir())
        .chain(glib::system_data_dirs())
        .map(|d| d.join("gnome-shell").join("extensions").join(EXTENSION_UUID))
        .find(|d| d.join("metadata.json").is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerators() {
        assert!(same_accel("<Super>space", "<Mod4>Space"));
        assert!(same_accel("<Primary><Shift>a", "<Shift><Control>A"));
        assert!(!same_accel("<Super>space", "<Shift><Super>space"));
        assert!(!same_accel("<Super>space", "XF86Keyboard"));
        assert!(!same_accel("<Bogus>x", "<Bogus>x"));
        let gnome_default = vec!["<Super>space".to_owned(), "XF86Keyboard".to_owned()];
        assert_eq!(without_accel(&gnome_default, DEFAULT_SHORTCUT), ["XF86Keyboard"]);
        assert_eq!(accel_label("<Super>space"), "Super+Space");
        assert_eq!(accel_label("<Shift><Primary>k"), "Ctrl+Shift+K");
    }

    #[test]
    fn autostart_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("autostart").join("dev.spot.Spot.desktop");
        assert!(!autostart_enabled(&p));
        set_autostart(&p, Path::new("/opt/my apps/spot"), true).unwrap();
        assert!(autostart_enabled(&p));
        assert!(std::fs::read_to_string(&p).unwrap().contains("Exec=\"/opt/my apps/spot\" --background"));
        std::fs::write(&p, autostart_contents(Path::new("/usr/bin/spot")) + "Hidden=true\n").unwrap();
        assert!(!autostart_enabled(&p), "turned off by the user elsewhere");
        set_autostart(&p, Path::new("/usr/bin/spot"), false).unwrap();
        set_autostart(&p, Path::new("/usr/bin/spot"), false).unwrap();
        assert!(!p.exists());
    }
}
