//! Typing a URL or a path opens it: `https://…`, `gnome.org`, `~/Downloads`, `/etc/fstab`.

use std::path::{Path, PathBuf};

use gio::glib;
use gio::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `strong`: explicit scheme or `www.`, shown above app results. A bare `name.tld` could
    /// also be a typo'd app name, so it goes below them.
    Url { uri: String, strong: bool },
    /// An existing file or folder.
    Path { path: PathBuf, is_dir: bool },
}

impl Target {
    pub fn uri(&self) -> String {
        match self {
            Target::Url { uri, .. } => uri.clone(),
            Target::Path { path, .. } => gio::File::for_path(path).uri().into(),
        }
    }

    pub fn is_strong(&self) -> bool {
        match self {
            Target::Url { strong, .. } => *strong,
            Target::Path { .. } => true,
        }
    }

    /// Open with the default handler (browser, file manager, viewer…).
    pub fn open(&self, ctx: Option<&gio::AppLaunchContext>) -> Result<(), glib::Error> {
        gio::AppInfo::launch_default_for_uri(&self.uri(), ctx)
    }
}

pub fn detect(query: &str) -> Option<Target> {
    let q = query.trim();
    if q.is_empty() || q.chars().any(char::is_whitespace) {
        return None;
    }
    if q.starts_with('/') || q == "~" || q.starts_with("~/") {
        return detect_path(q);
    }
    if let Some((scheme, rest)) = q.split_once("://") {
        let valid_scheme = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
        return (valid_scheme && !rest.is_empty()).then(|| Target::Url { uri: q.to_owned(), strong: true });
    }
    if q.starts_with("mailto:") && q.contains('@') {
        return Some(Target::Url { uri: q.to_owned(), strong: true });
    }
    looks_like_domain(q).then(|| Target::Url {
        uri: format!("https://{q}"),
        strong: q.to_ascii_lowercase().starts_with("www."),
    })
}

fn detect_path(q: &str) -> Option<Target> {
    let path = match q.strip_prefix('~') {
        Some(rest) => {
            let home = glib::home_dir();
            let rest = rest.trim_start_matches('/');
            if rest.is_empty() { home } else { home.join(rest) }
        }
        None => PathBuf::from(q),
    };
    let meta = std::fs::metadata(&path).ok()?;
    Some(Target::Path { path, is_dir: meta.is_dir() })
}

/// `host.tld[:port][/path…]` where tld is 2+ letters and every label is a valid hostname label.
fn looks_like_domain(q: &str) -> bool {
    let host_port = q.split(['/', '?', '#']).next().unwrap_or("");
    let host = match host_port.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h,
        Some(_) => return false,
        None => host_port,
    };
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let tld = labels[labels.len() - 1];
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && l.chars().all(|c| c.is_alphanumeric() || c == '-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    labels.iter().all(|l| label_ok(l)) && tld.len() >= 2 && tld.chars().all(char::is_alphabetic)
}

/// Shorten a path for display: `/home/me/Downloads` -> `~/Downloads`.
pub fn display_path(p: &Path) -> String {
    let home = glib::home_dir();
    match p.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(q: &str) -> Option<(String, bool)> {
        match detect(q) {
            Some(Target::Url { uri, strong }) => Some((uri, strong)),
            _ => None,
        }
    }

    #[test]
    fn urls() {
        assert_eq!(url("https://gnome.org/a?b#c"), Some(("https://gnome.org/a?b#c".into(), true)));
        assert_eq!(url("www.debian.org"), Some(("https://www.debian.org".into(), true)));
        assert_eq!(url("gnome.org"), Some(("https://gnome.org".into(), false)));
        assert_eq!(url("localhost.dev:8080/x"), Some(("https://localhost.dev:8080/x".into(), false)));
        assert_eq!(url("mailto:a@b.org").map(|u| u.1), Some(true));
        for q in ["firefox", "1.5", "v1.2.3", "a.b c", "://x", "org.gnome.", "foo.c", "3+4.5", "e.g"] {
            assert_eq!(detect(q), None, "{q:?}");
        }
    }

    #[test]
    fn paths_must_exist() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("notes.txt");
        std::fs::write(&f, "x").unwrap();
        assert_eq!(detect(f.to_str().unwrap()), Some(Target::Path { path: f.clone(), is_dir: false }));
        assert_eq!(detect(d.path().to_str().unwrap()), Some(Target::Path { path: d.path().into(), is_dir: true }));
        assert_eq!(detect(d.path().join("missing").to_str().unwrap()), None);
        assert!(matches!(detect("~"), Some(Target::Path { is_dir: true, .. })));
        assert_eq!(display_path(&glib::home_dir().join("x")), "~/x");
    }
}
