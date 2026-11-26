//! Optional settings in `~/.config/spot/config.ini` (read at startup; `spot --quit` to reload):
//!
//! ```ini
//! [search]
//! calculator=true
//! system-actions=true
//! open-urls-and-paths=true
//! web-search=true
//! # %s is replaced by the URL-encoded query
//! web-search-url=https://duckduckgo.com/?q=%s
//! ```

use std::path::{Path, PathBuf};

use gio::glib;

pub const DEFAULT_WEB_SEARCH_URL: &str = "https://duckduckgo.com/?q=%s";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub calculator: bool,
    pub system_actions: bool,
    pub open_targets: bool,
    pub web_search: bool,
    pub web_search_url: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            calculator: true,
            system_actions: true,
            open_targets: true,
            web_search: true,
            web_search_url: DEFAULT_WEB_SEARCH_URL.into(),
        }
    }
}

pub fn default_path() -> PathBuf {
    glib::user_config_dir().join("spot").join("config.ini")
}

impl Config {
    /// Missing file or keys fall back to the defaults; a malformed file is reported and ignored.
    pub fn load(path: &Path) -> Self {
        let mut c = Config::default();
        if !path.exists() {
            return c;
        }
        let kf = glib::KeyFile::new();
        if let Err(e) = kf.load_from_file(path, glib::KeyFileFlags::NONE) {
            eprintln!("spot: ignoring {}: {e}", path.display());
            return c;
        }
        let flag = |key: &str, v: &mut bool| {
            if let Ok(b) = kf.boolean("search", key) {
                *v = b;
            }
        };
        flag("calculator", &mut c.calculator);
        flag("system-actions", &mut c.system_actions);
        flag("open-urls-and-paths", &mut c.open_targets);
        flag("web-search", &mut c.web_search);
        if let Ok(url) = kf.string("search", "web-search-url") {
            if url.contains("%s") {
                c.web_search_url = url.into();
            } else {
                eprintln!("spot: web-search-url needs a %s placeholder; using the default");
            }
        }
        c
    }

    pub fn web_search_uri(&self, query: &str) -> String {
        let q = query.split_whitespace().collect::<Vec<_>>().join(" ");
        self.web_search_url.replace("%s", &glib::uri_escape_string(q.as_str(), None::<&str>, false))
    }

    /// Host of the search URL, for the result subtitle ("duckduckgo.com").
    pub fn web_search_engine(&self) -> String {
        let after_scheme = self.web_search_url.split_once("://").map_or("", |(_, rest)| rest);
        Some(after_scheme.split(['/', '?', '#', ':']).next().unwrap_or(""))
            .filter(|h| !h.is_empty() && !h.contains('%'))
            .map(|h| h.trim_start_matches("www.").to_string())
            .unwrap_or_else(|| "the web".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_overrides_and_bad_values() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.ini");
        assert_eq!(Config::load(&p), Config::default());
        std::fs::write(&p, "[search]\nweb-search=false\ncalculator=nope\nweb-search-url=https://www.google.com/search?q=%s\n").unwrap();
        let c = Config::load(&p);
        assert!(!c.web_search);
        assert!(c.calculator, "unparseable value keeps the default");
        assert_eq!(c.web_search_engine(), "google.com");
        assert_eq!(c.web_search_uri("  rust  gtk&co "), "https://www.google.com/search?q=rust%20gtk%26co");
        std::fs::write(&p, "[search]\nweb-search-url=https://example.org/\n").unwrap();
        assert_eq!(Config::load(&p).web_search_url, DEFAULT_WEB_SEARCH_URL);
    }
}
