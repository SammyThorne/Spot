//! The result list for a query: instant answers, ranked entries and the web fallback.
//!
//! Order: calculator answer → strong URL/path (explicit scheme, `www.`, existing path) →
//! ranked apps/AppImages/system actions → weak URL (bare `name.tld`) → "Search the web".

use crate::calc::{self, Answer};
use crate::config::Config;
use crate::history::History;
use crate::open::{self, Target};
use crate::search::{Hit, Index};

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Calc(Answer),
    Open(Target),
    Entry(Hit),
    Web { query: String, uri: String },
}

impl Item {
    /// Stable short description, used by the test trace and `spot --search`.
    pub fn describe(&self, index: &Index) -> String {
        match self {
            Item::Calc(a) => format!("calc:{}", a.text),
            Item::Open(t) => format!("open:{}", t.uri()),
            Item::Entry(h) => index.entries()[h.index].id.clone(),
            Item::Web { uri, .. } => format!("web:{uri}"),
        }
    }
}

/// `limit` caps the ranked entries; instant answers and the web row come on top of that.
pub fn build(index: &mut Index, history: &History, config: &Config, query: &str, limit: usize, now: u64) -> Vec<Item> {
    let mut items = Vec::new();
    if query.trim().is_empty() {
        return items;
    }
    if config.calculator {
        if let Some(a) = calc::evaluate(query) {
            items.push(Item::Calc(a));
        }
    }
    let target = if config.open_targets { open::detect(query) } else { None };
    let (strong, weak) = match target {
        Some(t) if t.is_strong() => (Some(t), None),
        t => (None, t),
    };
    items.extend(strong.map(Item::Open));
    items.extend(
        index.search_with(query, limit, |e, q| history.boost(&e.id, q, now)).into_iter().map(Item::Entry),
    );
    items.extend(weak.map(Item::Open));
    if config.web_search {
        let q = query.split_whitespace().collect::<Vec<_>>().join(" ");
        items.push(Item::Web { uri: config.web_search_uri(&q), query: q });
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Entry, EntryKind};

    fn idx() -> Index {
        let e = |id: &str, name: &str| Entry {
            id: id.into(),
            name: name.into(),
            generic_name: None,
            comment: None,
            keywords: vec![],
            exec_name: None,
            icon: None,
            kind: EntryKind::Desktop,
        };
        let mut v = vec![e("calc.desktop", "Calculator"), e("gnome.desktop", "GNOME Tweaks")];
        v.extend(crate::actions::entries());
        Index::new(v)
    }

    fn describe(q: &str, c: &Config) -> Vec<String> {
        let mut i = idx();
        let items = build(&mut i, &History::in_memory(), c, q, 8, 0);
        items.iter().map(|it| it.describe(&i)).collect()
    }

    #[test]
    fn order_of_answers_entries_and_fallbacks() {
        let c = Config::default();
        assert!(describe("  ", &c).is_empty());
        assert_eq!(describe("2*21", &c), ["calc:42", "web:https://duckduckgo.com/?q=2%2A21"]);
        assert_eq!(describe("calc", &c), ["calc.desktop", "web:https://duckduckgo.com/?q=calc"]);
        assert_eq!(describe("gnome.org", &c), ["open:https://gnome.org", "web:https://duckduckgo.com/?q=gnome.org"]);
        assert_eq!(describe("https://gnome.org", &c)[0], "open:https://gnome.org");
        assert_eq!(describe("lock", &c)[0], "spot-action:lock");
        let off = Config { web_search: false, calculator: false, ..Config::default() };
        assert!(describe("2*21", &off).is_empty());
    }
}
