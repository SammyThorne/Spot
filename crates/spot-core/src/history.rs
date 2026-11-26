//! Launch history: what you start, how often, and what you had typed when you picked it.
//!
//! Two boosts come out of it, added to [`crate::search`] tier scores:
//! - **frecency**: launches weighted by recency (half-life 14 days), up to [`MAX_FRECENCY`];
//! - **learned queries**: picking an entry after typing `q` makes it rank higher for `q` and
//!   for queries on the way to `q` ("c", "ca" after you typed "calc"). Up to [`MAX_LEARNED`],
//!   which is enough to lift a learned choice over other name-prefix matches.
//!
//! Stored locally only, as an append-only TSV log in `~/.local/share/spot/history.tsv`:
//!   `L <unix-time> <entry id>` and `Q <unix-time> <query> <entry id>`
//! The log is compacted when it grows past [`COMPACT_AT`] lines. Delete the file to forget.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const DAY: f64 = 86_400.0;
const FRECENCY_HALF_LIFE_DAYS: f64 = 14.0;
const LEARNED_HALF_LIFE_DAYS: f64 = 30.0;
pub const MAX_FRECENCY: u32 = 150;
pub const MAX_LEARNED: u32 = 400;
/// Frecency points per (fresh) launch.
const FRECENCY_PER_LAUNCH: f64 = 40.0;
/// Launches remembered per entry.
const KEEP_LAUNCHES: usize = 20;
/// Repeats of the same (query, entry) that count; more don't make it stronger.
const SATURATE_COUNT: u32 = 3;
const MAX_AGE_DAYS: f64 = 180.0;
const MAX_QUERY_CHARS: usize = 64;
const COMPACT_AT: usize = 4000;

#[derive(Debug, Clone, PartialEq)]
struct Learned {
    query: String,
    count: u32,
    last: u64,
}

#[derive(Debug, Default)]
pub struct History {
    path: Option<PathBuf>,
    launches: HashMap<String, Vec<u64>>,
    learned: HashMap<String, Vec<Learned>>,
    lines: usize,
}

pub fn default_path() -> PathBuf {
    gio::glib::user_data_dir().join("spot").join("history.tsv")
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Same normalization as the search query: trimmed, collapsed whitespace, lower case.
pub fn normalize_query(q: &str) -> String {
    let q = q.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    q.chars().take(MAX_QUERY_CHARS).collect()
}

fn decay(age_secs: u64, half_life_days: f64) -> f64 {
    0.5f64.powf(age_secs as f64 / DAY / half_life_days)
}

impl History {
    /// History not backed by a file (tests, `--search` without history).
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Load from `path`; a missing or unreadable file gives an empty history that will be
    /// created on the first launch.
    pub fn load(path: &Path) -> Self {
        let mut h = History { path: Some(path.to_owned()), ..Default::default() };
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                h.apply_line(line);
                h.lines += 1;
            }
        }
        h
    }

    fn apply_line(&mut self, line: &str) {
        let f: Vec<&str> = line.split('\t').collect();
        match f.as_slice() {
            ["L", t, id] => {
                if let Ok(t) = t.parse() {
                    self.add_launch(id, t);
                }
            }
            ["Q", t, q, id] => {
                if let Ok(t) = t.parse() {
                    self.add_learned(id, q, t);
                }
            }
            _ => {}
        }
    }

    fn add_launch(&mut self, id: &str, t: u64) {
        let v = self.launches.entry(id.to_owned()).or_default();
        v.push(t);
        if v.len() > KEEP_LAUNCHES {
            v.remove(0);
        }
    }

    fn add_learned(&mut self, id: &str, query: &str, t: u64) {
        let v = self.learned.entry(id.to_owned()).or_default();
        if let Some(l) = v.iter_mut().find(|l| l.query == query) {
            l.count += 1;
            l.last = l.last.max(t);
        } else {
            v.push(Learned { query: query.to_owned(), count: 1, last: t });
        }
    }

    /// Record that `id` was launched after typing `query`, and persist it.
    pub fn record(&mut self, id: &str, query: &str, now: u64) -> io::Result<()> {
        let id = id.replace(['\t', '\n'], " ");
        let q = normalize_query(&query.replace(['\t', '\n'], " "));
        self.add_launch(&id, now);
        let mut text = format!("L\t{now}\t{id}\n");
        if !q.is_empty() {
            self.add_learned(&id, &q, now);
            text.push_str(&format!("Q\t{now}\t{q}\t{id}\n"));
        }
        let Some(path) = self.path.clone() else { return Ok(()) };
        self.lines += text.lines().count();
        if self.lines > COMPACT_AT {
            return self.compact(now);
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        OpenOptions::new().create(true).append(true).open(&path)?.write_all(text.as_bytes())
    }

    /// Rewrite the log from the in-memory state, dropping old and redundant lines.
    fn compact(&mut self, now: u64) -> io::Result<()> {
        let Some(path) = self.path.clone() else { return Ok(()) };
        let max_age = (MAX_AGE_DAYS * DAY) as u64;
        let fresh = |t: u64| now.saturating_sub(t) <= max_age;
        let mut out = String::new();
        for (id, ts) in &self.launches {
            for t in ts.iter().filter(|t| fresh(**t)) {
                out.push_str(&format!("L\t{t}\t{id}\n"));
            }
        }
        for (id, ls) in &self.learned {
            for l in ls.iter().filter(|l| fresh(l.last)) {
                for _ in 0..l.count.min(SATURATE_COUNT) {
                    out.push_str(&format!("Q\t{}\t{}\t{id}\n", l.last, l.query));
                }
            }
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tsv.tmp");
        fs::write(&tmp, &out)?;
        fs::rename(&tmp, &path)?;
        // Reload so memory matches what is on disk.
        *self = History::load(&path);
        Ok(())
    }

    /// Personal boost for entry `id` when the (normalized) query is `query`.
    pub fn boost(&self, id: &str, query: &str, now: u64) -> u32 {
        let frecency: f64 = self
            .launches
            .get(id)
            .map(|ts| ts.iter().map(|t| FRECENCY_PER_LAUNCH * decay(now.saturating_sub(*t), FRECENCY_HALF_LIFE_DAYS)).sum())
            .unwrap_or(0.0);
        let learned = self
            .learned
            .get(id)
            .map(|ls| {
                ls.iter()
                    .map(|l| {
                        let strength = query_affinity(query, &l.query);
                        let repeats = l.count.min(SATURATE_COUNT) as f64 / SATURATE_COUNT as f64;
                        strength * repeats * decay(now.saturating_sub(l.last), LEARNED_HALF_LIFE_DAYS)
                    })
                    .fold(0.0, f64::max)
            })
            .unwrap_or(0.0);
        (frecency.min(MAX_FRECENCY as f64) + learned * MAX_LEARNED as f64).round() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.launches.is_empty()
    }

    /// Forget everything, in memory and on disk.
    pub fn clear(&mut self) -> io::Result<()> {
        let path = self.path.take();
        *self = History { path, ..Default::default() };
        match &self.path {
            Some(p) => match fs::remove_file(p) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
            None => Ok(()),
        }
    }
}

/// How much a past query says about the current one, 0..=1.
/// - same query: 1
/// - current is a prefix of the past one ("ca" vs "calc"): 0.4–1, more the closer it gets
/// - current extends the past one ("calcu" vs "calc"): 0.7
fn query_affinity(current: &str, past: &str) -> f64 {
    if current == past {
        1.0
    } else if past.starts_with(current) {
        0.4 + 0.6 * current.chars().count() as f64 / past.chars().count() as f64
    } else if current.starts_with(past) {
        0.7
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_800_000_000;
    const DAY_S: u64 = 86_400;

    #[test]
    fn frecency_grows_with_use_and_fades_with_time() {
        let mut h = History::in_memory();
        assert_eq!(h.boost("a", "x", T0), 0);
        h.record("a", "", T0).unwrap();
        let one = h.boost("a", "zzz", T0);
        assert_eq!(one, FRECENCY_PER_LAUNCH as u32);
        for _ in 0..10 {
            h.record("a", "", T0).unwrap();
        }
        assert_eq!(h.boost("a", "zzz", T0), MAX_FRECENCY, "capped");
        let mut once = History::in_memory();
        once.record("b", "", T0).unwrap();
        assert_eq!(once.boost("b", "zzz", T0 + 28 * DAY_S), (FRECENCY_PER_LAUNCH / 4.0) as u32, "two half-lives later");
    }

    #[test]
    fn learned_queries_apply_to_prefixes_and_extensions() {
        let mut h = History::in_memory();
        for _ in 0..3 {
            h.record("calc", "Calc", T0).unwrap();
        }
        let f = MAX_FRECENCY; // 3 fresh launches = 120 < cap; compare relative to that
        let base = h.boost("calc", "unrelated", T0);
        assert!(base <= f);
        let exact = h.boost("calc", "calc", T0) - base;
        let prefix = h.boost("calc", "c", T0) - base;
        let longer = h.boost("calc", "calcu", T0) - base;
        assert_eq!(exact, MAX_LEARNED);
        assert!(prefix > 0 && prefix < exact, "{prefix}");
        assert!(longer > 0 && longer < exact, "{longer}");
        assert_eq!(h.boost("other", "calc", T0), 0);
    }

    #[test]
    fn one_pick_is_weaker_than_a_habit() {
        let mut h = History::in_memory();
        h.record("a", "te", T0).unwrap();
        let once = h.boost("a", "te", T0);
        h.record("a", "te", T0).unwrap();
        h.record("a", "te", T0).unwrap();
        assert!(h.boost("a", "te", T0) > once);
    }

    #[test]
    fn persists_and_reloads() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("history.tsv");
        let mut h = History::load(&p);
        assert!(h.is_empty());
        h.record("org.gnome.Calculator.desktop", "  CALC ", T0).unwrap();
        h.record("appimage:/home/u/My App.AppImage", "my\tapp", T0).unwrap();
        let again = History::load(&p);
        assert_eq!(again.boost("org.gnome.Calculator.desktop", "calc", T0), h.boost("org.gnome.Calculator.desktop", "calc", T0));
        assert!(again.boost("appimage:/home/u/My App.AppImage", "my app", T0) > MAX_LEARNED / 3);
    }

    #[test]
    fn compaction_keeps_meaning_and_drops_old() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("history.tsv");
        let mut h = History::load(&p);
        h.record("old", "old", T0).unwrap();
        let later = T0 + 365 * DAY_S;
        for _ in 0..(COMPACT_AT / 2 + 1) {
            h.record("new", "ne", later).unwrap();
        }
        let lines = fs::read_to_string(&p).unwrap().lines().count();
        // 20 launches + 3 query lines, plus whatever was appended after the compaction ran.
        assert!(lines < 50, "compacted to {lines} lines");
        let reloaded = History::load(&p);
        assert_eq!(reloaded.boost("old", "old", later), 0, "older than MAX_AGE dropped");
        assert_eq!(reloaded.boost("new", "ne", later), h.boost("new", "ne", later));
    }

    #[test]
    fn clear_forgets_and_keeps_working() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("history.tsv");
        let mut h = History::load(&p);
        h.record("a", "a", T0).unwrap();
        h.clear().unwrap();
        assert!(h.is_empty() && !p.exists());
        h.clear().unwrap(); // already gone: fine
        h.record("b", "b", T0).unwrap();
        assert!(History::load(&p).boost("b", "b", T0) > 0, "still persists after clearing");
    }

    #[test]
    fn query_normalization() {
        assert_eq!(normalize_query("  Fire   FOX "), "fire fox");
    }
}
