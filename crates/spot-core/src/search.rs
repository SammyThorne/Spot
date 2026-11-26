//! Ranking.
//!
//! An entry scores by the *strongest way* it matches the query (tiers below); weak ways only
//! count when nothing better applies. Personal boosts (see [`crate::history`]) are added on top
//! and are bounded so they reorder similar matches but don't override an exact name match.
//!
//! Lessons from running v1 against a real app list: fuzzy matching on descriptions matches
//! almost anything ("calc" → Clocks via c…a…l…c), and scattered fuzzy matches on names are
//! noise ("ca" → Claude). Descriptions now need whole-word prefixes, and fuzzy name matches
//! must hit word starts (acronyms like "vsc" → Visual Studio Code) or be contiguous.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32String};

use crate::entry::Entry;

pub const T_EXACT: u32 = 1000;
pub const T_NAME_PREFIX: u32 = 800;
pub const T_NAME_WORDS: u32 = 700;
pub const T_KEYWORD_EXACT: u32 = 600;
pub const T_KEYWORD_PREFIX: u32 = 500;
pub const T_EXEC: u32 = 450;
pub const T_GENERIC: u32 = 400;
pub const T_ACRONYM: u32 = 300;
pub const T_COMMENT: u32 = 250;
/// Descriptions are long; 1–2 letters would match half of them.
const MIN_COMMENT_QUERY: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Index into [`Index::entries`].
    pub index: usize,
    pub score: u32,
}

/// Lower-cased words of a field. CamelCase parts are added too, so "office" finds LibreOffice
/// and "storm" finds WebStorm.
fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in s.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()) {
        out.push(token.to_lowercase());
        let chars: Vec<char> = token.chars().collect();
        let mut start = 0;
        for i in 1..chars.len() {
            if chars[i - 1].is_lowercase() && chars[i].is_uppercase() {
                if start > 0 {
                    out.push(chars[start..i].iter().collect::<String>().to_lowercase());
                }
                start = i;
            }
        }
        if start > 0 {
            out.push(chars[start..].iter().collect::<String>().to_lowercase());
        }
    }
    out
}

/// Every query word is a prefix of some field word.
fn words_match(field: &[String], query: &[String]) -> bool {
    !query.is_empty() && query.iter().all(|q| field.iter().any(|w| w.starts_with(q.as_str())))
}

/// Char positions that start a word: after a non-alphanumeric char or at a camelCase hump.
fn word_starts(chars: &[char]) -> Vec<bool> {
    (0..chars.len())
        .map(|i| {
            i == 0
                || !chars[i - 1].is_alphanumeric() && chars[i].is_alphanumeric()
                || chars[i - 1].is_lowercase() && chars[i].is_uppercase()
        })
        .collect()
}

/// Pre-processed haystacks for one entry (done once per index build, not per keystroke).
struct Prepared {
    name: Utf32String,
    name_chars: Vec<char>,
    name_starts: Vec<bool>,
    name_lower: String,
    name_words: Vec<String>,
    keywords: Vec<String>,
    exec_words: Vec<String>,
    generic_words: Vec<String>,
    comment_words: Vec<String>,
}

struct Query {
    lower: String,
    words: Vec<String>,
    pattern: Pattern,
}

impl Query {
    fn new(q: &str) -> Option<Self> {
        let lower = q.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        if lower.is_empty() {
            return None;
        }
        let words = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect();
        let pattern = Pattern::parse(&lower, CaseMatching::Ignore, Normalization::Smart);
        Some(Query { lower, words, pattern })
    }
}

pub struct Index {
    entries: Vec<Entry>,
    prepared: Vec<Prepared>,
    matcher: Matcher,
}

impl Index {
    pub fn new(entries: Vec<Entry>) -> Self {
        let prepared = entries
            .iter()
            .map(|e| {
                let name_chars: Vec<char> = e.name.chars().collect();
                Prepared {
                    name: e.name.as_str().into(),
                    name_starts: word_starts(&name_chars),
                    name_chars,
                    name_lower: e.name.to_lowercase(),
                    name_words: words(&e.name),
                    keywords: e.keywords.iter().map(|k| k.to_lowercase()).collect(),
                    exec_words: e.exec_name.as_deref().map(words).unwrap_or_default(),
                    generic_words: e.generic_name.as_deref().map(words).unwrap_or_default(),
                    comment_words: e.comment.as_deref().map(words).unwrap_or_default(),
                }
            })
            .collect();
        Index { entries, prepared, matcher: Matcher::new(Config::DEFAULT) }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Best `limit` matches, best first, without personal boosts. An empty query returns
    /// nothing (Spotlight shows only the search field until you type).
    pub fn search(&mut self, query: &str, limit: usize) -> Vec<Hit> {
        self.search_with(query, limit, |_, _| 0)
    }

    /// Like [`search`](Self::search), adding `boost(entry, normalized_query)` to every match.
    pub fn search_with(&mut self, query: &str, limit: usize, boost: impl Fn(&Entry, &str) -> u32) -> Vec<Hit> {
        let Some(q) = Query::new(query) else { return Vec::new() };
        if limit == 0 {
            return Vec::new();
        }
        // (hit, exact name match). Exact matches always come first, whatever the boosts:
        // typing an app's full name must open that app.
        let mut hits: Vec<(Hit, bool)> = Vec::new();
        for (index, p) in self.prepared.iter().enumerate() {
            if let Some(base) = score(&q, p, &mut self.matcher) {
                hits.push((Hit { index, score: base + boost(&self.entries[index], &q.lower) }, base == T_EXACT));
            }
        }
        hits.sort_by(|(a, a_exact), (b, b_exact)| {
            let (ea, eb) = (&self.entries[a.index], &self.entries[b.index]);
            b_exact
                .cmp(a_exact)
                .then(b.score.cmp(&a.score))
                .then(ea.name.len().cmp(&eb.name.len()))
                .then_with(|| ea.name.to_lowercase().cmp(&eb.name.to_lowercase()))
        });
        hits.truncate(limit);
        hits.into_iter().map(|(h, _)| h).collect()
    }

    /// Char indices of the entry's name to emphasise for this query (empty if the match came
    /// from another field).
    pub fn highlight(&mut self, index: usize, query: &str) -> Vec<usize> {
        let Some(q) = Query::new(query) else { return Vec::new() };
        let p = &self.prepared[index];
        let qlen = q.lower.chars().count();
        if p.name_lower.starts_with(&q.lower) && p.name_lower.chars().count() == p.name_chars.len() {
            return (0..qlen).collect();
        }
        if words_match(&p.name_words, &q.words) {
            let lower: Vec<char> = p.name_lower.chars().collect();
            if lower.len() == p.name_chars.len() {
                let mut out = Vec::new();
                for w in &q.words {
                    let wc: Vec<char> = w.chars().collect();
                    let at = (0..lower.len()).find(|&i| {
                        p.name_starts[i] && lower[i..].starts_with(&wc) && !out.contains(&i)
                    });
                    if let Some(i) = at {
                        out.extend(i..i + wc.len());
                    }
                }
                out.sort_unstable();
                out.dedup();
                return out;
            }
        }
        acronym_indices(&q, p, &mut self.matcher).unwrap_or_default()
    }
}

/// Fuzzy name match whose every matched char is at a word start or right after the previous
/// matched char; returns those char indices.
fn acronym_indices(q: &Query, p: &Prepared, m: &mut Matcher) -> Option<Vec<usize>> {
    let mut idx: Vec<u32> = Vec::new();
    q.pattern.indices(p.name.slice(..), m, &mut idx)?;
    idx.sort_unstable();
    idx.dedup();
    let ok = idx.iter().enumerate().all(|(n, &i)| {
        let i = i as usize;
        p.name_starts.get(i).copied().unwrap_or(false) || (n > 0 && idx[n - 1] as usize + 1 == i)
    });
    ok.then(|| idx.into_iter().map(|i| i as usize).collect())
}

fn score(q: &Query, p: &Prepared, m: &mut Matcher) -> Option<u32> {
    if p.name_lower == q.lower {
        return Some(T_EXACT);
    }
    if p.name_lower.starts_with(&q.lower) {
        return Some(T_NAME_PREFIX);
    }
    if words_match(&p.name_words, &q.words) {
        return Some(T_NAME_WORDS);
    }
    if p.keywords.contains(&q.lower) {
        return Some(T_KEYWORD_EXACT);
    }
    if p.keywords.iter().any(|k| k.starts_with(&q.lower)) {
        return Some(T_KEYWORD_PREFIX);
    }
    if words_match(&p.exec_words, &q.words) {
        return Some(T_EXEC);
    }
    if words_match(&p.generic_words, &q.words) {
        return Some(T_GENERIC);
    }
    if q.lower.chars().count() >= 2 {
        if let Some(idx) = acronym_indices(q, p, m) {
            // Within the tier, tighter matches (fewer gaps) first.
            let span = idx.last().unwrap() - idx.first().unwrap() + 1;
            return Some(T_ACRONYM + (50 * idx.len() / span.max(1)) as u32);
        }
    }
    if q.lower.chars().count() >= MIN_COMMENT_QUERY && words_match(&p.comment_words, &q.words) {
        return Some(T_COMMENT);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::EntryKind;

    fn e(name: &str, generic: Option<&str>, keywords: &[&str], exec: &str, comment: Option<&str>) -> Entry {
        Entry {
            id: format!("{}.desktop", name.replace(' ', "")),
            name: name.into(),
            generic_name: generic.map(Into::into),
            comment: comment.map(Into::into),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            exec_name: Some(exec.into()),
            icon: None,
            kind: EntryKind::Desktop,
        }
    }

    fn index() -> Index {
        Index::new(vec![
            e("Firefox", Some("Web Browser"), &["Internet", "WWW", "Browser"], "firefox-esr", Some("Browse the World Wide Web")),
            e("Files", Some("File Manager"), &["folder", "manager", "explore"], "nautilus", Some("Access and organize files")),
            e("Visual Studio Code", Some("Text Editor"), &["vscode"], "code", None),
            e("Calculator", None, &["calculation", "arithmetic"], "gnome-calculator", Some("Perform arithmetic, scientific or financial calculations")),
            e("Calendar", None, &["event", "reminder"], "gnome-calendar", None),
            e("Clocks", None, &["time", "timer", "alarm"], "gnome-clocks", Some("Clocks for world times, plus alarms, stopwatch and a timer")),
            e("Claude", None, &[], "claude-desktop", Some("AI assistant")),
            e("Settings", None, &["preferences", "control"], "gnome-control-center", None),
            e("Terminal", None, &["shell", "prompt", "command"], "gnome-terminal", None),
            e("Text Editor", None, &["notepad", "txt"], "gnome-text-editor", Some("Edit text files")),
            e("Sublime Text", None, &[], "sublime_text", Some("Sophisticated text editor for code")),
            e("LibreOffice Calc", Some("Spreadsheet"), &[], "libreoffice", None),
            e("WebStorm", None, &[], "webstorm", None),
            e("Zed", None, &[], "zed", Some("A high-performance code editor")),
            e("Discord", None, &["chat"], "discord", None),
        ])
    }

    fn top(idx: &mut Index, q: &str, n: usize) -> Vec<String> {
        idx.search(q, n).iter().map(|h| idx.entries()[h.index].name.clone()).collect()
    }

    fn all(idx: &mut Index, q: &str) -> Vec<String> {
        top(idx, q, 100)
    }

    #[test]
    fn tiers_order_results() {
        let mut i = index();
        assert_eq!(top(&mut i, "fire", 1), ["Firefox"]);
        // name prefix > word in name > generic name ("Text Editor")
        assert_eq!(all(&mut i, "text"), ["Text Editor", "Sublime Text", "Visual Studio Code"]);
        assert_eq!(all(&mut i, "calc"), ["Calculator", "LibreOffice Calc"]);
        assert_eq!(top(&mut i, "settings", 1), ["Settings"]);
    }

    #[test]
    fn keywords_generic_and_exec_match() {
        let mut i = index();
        assert_eq!(top(&mut i, "browser", 1), ["Firefox"]);
        assert_eq!(top(&mut i, "shell", 1), ["Terminal"]);
        assert_eq!(top(&mut i, "naut", 1), ["Files"]);
        assert_eq!(top(&mut i, "preferences", 1), ["Settings"]);
        assert_eq!(top(&mut i, "spreadsheet", 1), ["LibreOffice Calc"]);
    }

    #[test]
    fn camel_case_parts_are_words() {
        let mut i = index();
        assert_eq!(top(&mut i, "office", 1), ["LibreOffice Calc"]);
        assert_eq!(top(&mut i, "storm", 1), ["WebStorm"]);
    }

    #[test]
    fn acronyms_match_but_scattered_letters_do_not() {
        let mut i = index();
        assert_eq!(top(&mut i, "vsc", 1), ["Visual Studio Code"]);
        assert!(!all(&mut i, "ca").contains(&"Claude".to_string()), "{:?}", all(&mut i, "ca"));
    }

    #[test]
    fn descriptions_need_whole_word_prefixes() {
        let mut i = index();
        // v1 returned Clocks for "calc" (c…a…l…c inside its description).
        assert!(!all(&mut i, "calc").contains(&"Clocks".to_string()));
        assert_eq!(all(&mut i, "world"), ["Clocks", "Firefox"]);
        assert!(all(&mut i, "wo").is_empty(), "short queries skip descriptions");
        let code = all(&mut i, "code");
        assert_eq!(code[0], "Visual Studio Code");
        assert!(code.contains(&"Zed".to_string()) && code.contains(&"Sublime Text".to_string()), "{code:?}");
    }

    #[test]
    fn multi_word_and_case_and_whitespace() {
        let mut i = index();
        assert_eq!(top(&mut i, "  FIREFOX ", 1), ["Firefox"]);
        assert_eq!(top(&mut i, "lib calc", 1), ["LibreOffice Calc"]);
        assert_eq!(top(&mut i, "text ed", 1), ["Text Editor"]);
    }

    #[test]
    fn empty_and_nonsense_queries_return_nothing() {
        let mut i = index();
        assert!(i.search("", 10).is_empty());
        assert!(i.search("   ", 10).is_empty());
        assert!(i.search("zzqxj", 10).is_empty());
        assert!(i.search("e", 3).len() <= 3);
    }

    #[test]
    fn boosts_break_ties_but_not_exact_matches() {
        let mut i = index();
        assert_ne!(top(&mut i, "c", 1), ["Calendar"]);
        let boost_calendar = |e: &Entry, _: &str| if e.name == "Calendar" { 100 } else { 0 };
        let hits = i.search_with("c", 1, boost_calendar);
        assert_eq!(i.entries()[hits[0].index].name, "Calendar");
        let boost_files = |e: &Entry, _: &str| if e.name == "Files" { 300 } else { 0 };
        let hits = i.search_with("firefox", 1, boost_files);
        assert_eq!(i.entries()[hits[0].index].name, "Firefox");
        // Even the largest history boost (frecency + learned query) can't beat an exact name.
        let max = crate::history::MAX_FRECENCY + crate::history::MAX_LEARNED;
        let mut i = Index::new(vec![
            e("Spot", None, &[], "spot", None),
            e("Spot Learn Alpha", None, &[], "true", None),
        ]);
        let boost_other = |e: &Entry, _: &str| if e.name != "Spot" { max } else { 0 };
        let hits = i.search_with("spot", 2, boost_other);
        assert!(hits[1].score > hits[0].score, "the other one really has the higher score");
        assert_eq!(i.entries()[hits[0].index].name, "Spot");
    }

    #[test]
    fn highlights() {
        let mut i = index();
        let pos = |i: &mut Index, name: &str| i.entries().iter().position(|e| e.name == name).unwrap();
        let ff = pos(&mut i, "Firefox");
        assert_eq!(i.highlight(ff, "fir"), [0, 1, 2]);
        let lc = pos(&mut i, "LibreOffice Calc");
        assert_eq!(i.highlight(lc, "calc"), [12, 13, 14, 15]);
        let vsc = pos(&mut i, "Visual Studio Code");
        assert_eq!(i.highlight(vsc, "vsc"), [0, 7, 14]);
        assert!(i.highlight(ff, "browser").is_empty());
    }
}
