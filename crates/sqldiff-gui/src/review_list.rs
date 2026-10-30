//! The review page's list, served a page at a time. The page shows one lens
//! (a set of tiers) of the queue, filtered by text and grouped by cause, by
//! file or by tier. Here the lens, the filter and the groups are taken
//! over the whole queue, so the page gets group headings and the rows it
//! shows, never the whole queue: on a large tree that was hundreds of
//! megabytes per action, in the server and in the browser.
//!
//! The rules are the ones the page applied itself before (`causes`,
//! `shown`, `groups`, `order` in `review.html`); the tests in
//! `tests/review_list.rs` hold them.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use sqldiff_convert::queue::Kind;

use crate::convert_review::ListedItem;

/// How the lens is grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// By what is open or wrong on an item: a unit with two open
    /// assumptions is in both groups; one that needs a fix is grouped by
    /// its first blocker only.
    Cause,
    File,
    Tier,
}

/// One view of the queue: the lens's tiers, the grouping and the filter
/// text (matched, without case, against the path, the reason and the
/// causes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    pub tiers: Vec<u8>,
    pub group: Group,
    pub filter: String,
}

impl Query {
    /// From the request's `tiers=3,4,5&group=cause&q=...`; tier 6 only
    /// with `all` (plainly converted units are listed for their own lens).
    pub fn parse(query: &str, all: bool) -> Query {
        let param = |k| crate::server::param(query, k);
        let tiers = param("tiers")
            .unwrap_or_default()
            .split(',')
            .filter_map(|t| t.trim().parse::<u8>().ok())
            .filter(|&t| (1..=5).contains(&t) || (t == 6 && all))
            .collect();
        let group = match param("group").as_deref() {
            Some("file") => Group::File,
            Some("tier") => Group::Tier,
            _ => Group::Cause,
        };
        Query {
            tiers,
            group,
            filter: param("q").unwrap_or_default(),
        }
    }
}

/// The status as the page names it (`statusLabel`).
pub fn status_label(s: &str) -> &'static str {
    match s {
        "converted" | "nothing found, not proven" => "nothing found, not proven",
        "assumptions" | "under assumptions" => "under assumptions",
        "decision" | "needs a decision" => "needs a decision",
        "differs" => "differs",
        "not_converted" | "not converted" => "not converted",
        _ => "not proven",
    }
}

fn kind_label(k: &Kind) -> &'static str {
    match k {
        Kind::Unit => "unit",
        Kind::MissingObject => "missing object",
        Kind::Unplaced => "outside any unit",
        Kind::Template => "template instances",
        Kind::NotExercised => "template no instance uses",
        Kind::NotInManifest => "not in the manifest",
        Kind::Unreadable => "unreadable file",
    }
}

fn distinct(v: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    v.iter()
        .filter(|x| seen.insert(x.as_str()))
        .cloned()
        .collect()
}

/// Why an item is listed, as group keys: what is open on it, else what no
/// decision can discharge.
pub fn causes(it: &ListedItem) -> Vec<String> {
    let i = &it.item;
    if i.kind != Kind::Unit {
        return match i.rules.is_empty() {
            false => distinct(&i.rules),
            true => vec![kind_label(&i.kind).to_string()],
        };
    }
    if i.tier <= 2 {
        return match it.blocking.is_empty() {
            false => distinct(&it.blocking),
            true if i.tier == 1 => vec!["rejected in review".into()],
            true => vec![status_label(&i.status).into()],
        };
    }
    match i.tier {
        3 => vec!["written by an agent".into()],
        6 => vec!["nothing found, not proven".into()],
        _ if !i.open.is_empty() => distinct(&i.open),
        _ => vec![status_label(&i.status).into()],
    }
}

/// A group of the lens: its key (a cause, a path or a tier), its items
/// (indices into the queue, in queue order), a line describing it, and
/// how many of its units have the cause as an open assumption (what
/// "accept for all" takes).
#[derive(Debug, Clone)]
pub struct Grp {
    pub key: String,
    pub items: Vec<u32>,
    pub desc: String,
    pub accept: usize,
}

/// A query's groups and the order the page walks the items in.
#[derive(Debug)]
pub struct Listing {
    pub query: Query,
    pub groups: Vec<Grp>,
    /// Items the lens and filter show, each once.
    pub shown: usize,
    /// Each item once, group by group: J and K walk this.
    pub order: Vec<u32>,
    /// An item's first place: group index and position in it.
    first: HashMap<u32, (u32, u32)>,
}

/// Keys in the order people read them: without case first (the page
/// sorted with `localeCompare`), then as written.
fn key_order(a: &str, b: &str) -> std::cmp::Ordering {
    let fold = |s: &str| s.chars().flat_map(char::to_lowercase).collect::<String>();
    fold(a).cmp(&fold(b)).then_with(|| a.cmp(b))
}

pub fn list(items: &[ListedItem], q: &Query) -> Listing {
    let f = q.filter.trim().to_lowercase();
    let mut groups: Vec<Grp> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut shown = 0;
    let mut add = |key: String, i: u32, groups: &mut Vec<Grp>| {
        let g = *by_key.entry(key.clone()).or_insert_with(|| {
            groups.push(Grp {
                key,
                items: Vec::new(),
                desc: String::new(),
                accept: 0,
            });
            groups.len() - 1
        });
        groups[g].items.push(i);
    };
    for (i, it) in items.iter().enumerate() {
        if !q.tiers.contains(&it.item.tier) {
            continue;
        }
        let cs = causes(it);
        let hit = f.is_empty()
            || [&it.item.path, &it.item.reason]
                .into_iter()
                .chain(&cs)
                .any(|x| x.to_lowercase().contains(&f));
        if !hit {
            continue;
        }
        shown += 1;
        let i = i as u32;
        match q.group {
            Group::Tier => add(it.item.tier.to_string(), i, &mut groups),
            Group::File => add(it.item.path.clone(), i, &mut groups),
            Group::Cause if it.item.tier <= 2 => add(cs[0].clone(), i, &mut groups),
            Group::Cause => {
                for c in cs {
                    add(c, i, &mut groups);
                }
            }
        }
    }
    match q.group {
        Group::Tier => groups.sort_by_key(|g| g.key.parse::<u8>().unwrap_or(u8::MAX)),
        _ => groups.sort_by(|a, b| {
            b.items
                .len()
                .cmp(&a.items.len())
                .then_with(|| key_order(&a.key, &b.key))
        }),
    }
    for g in &mut groups {
        let first = &items[g.items[0] as usize];
        g.desc = match q.group {
            Group::Cause if first.item.kind == Kind::Unit && g.items.len() == 1 => {
                first.item.path.clone()
            }
            Group::Cause => first.item.reason.clone(),
            Group::File => {
                let all: Vec<String> = g
                    .items
                    .iter()
                    .flat_map(|&i| causes(&items[i as usize]))
                    .collect();
                distinct(&all).join(", ")
            }
            Group::Tier => String::new(),
        };
        if q.group == Group::Cause {
            g.accept = g
                .items
                .iter()
                .filter(|&&i| items[i as usize].open_assumptions.contains(&g.key))
                .count();
        }
    }
    let mut order = Vec::with_capacity(shown);
    let mut first = HashMap::with_capacity(shown);
    for (gi, g) in groups.iter().enumerate() {
        for (k, &i) in g.items.iter().enumerate() {
            if let std::collections::hash_map::Entry::Vacant(e) = first.entry(i) {
                e.insert((gi as u32, k as u32));
                order.push(i);
            }
        }
    }
    Listing {
        query: q.clone(),
        groups,
        shown,
        order,
        first,
    }
}

/// Where an item is listed first: its group's index and its place there.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Place {
    pub key: String,
    /// Position in the walk order.
    pub index: usize,
    pub group: usize,
    pub offset: usize,
}

impl Listing {
    pub fn group(&self, key: &str) -> Option<&Grp> {
        self.groups.iter().find(|g| g.key == key)
    }

    /// The place of the item at `index` of the walk order (clamped), if
    /// anything is shown.
    pub fn at(&self, items: &[ListedItem], index: usize) -> Option<Place> {
        let i = *self
            .order
            .get(index.min(self.order.len().checked_sub(1)?))?;
        self.place(items, i)
    }

    fn place(&self, items: &[ListedItem], i: u32) -> Option<Place> {
        let &(g, k) = self.first.get(&i)?;
        Some(Place {
            key: items[i as usize].item.key.clone(),
            index: self.order.iter().position(|&x| x == i)?,
            group: g as usize,
            offset: k as usize,
        })
    }

    /// The place of the item with `key`, if shown.
    pub fn find(&self, items: &[ListedItem], key: &str) -> Option<Place> {
        let i = self
            .order
            .iter()
            .find(|&&i| items[i as usize].item.key == key)?;
        self.place(items, *i)
    }

    /// `d` items on from `key` in the walk order (from the first when it
    /// is not shown), stopping at either end.
    pub fn step(&self, items: &[ListedItem], key: &str, d: i64) -> Option<Place> {
        let at = self.find(items, key).map_or(0, |p| p.index as i64 + d);
        self.at(items, at.clamp(0, self.order.len() as i64 - 1) as usize)
    }

    /// The first item of the group `d` groups on from `key`'s.
    pub fn step_group(&self, items: &[ListedItem], key: &str, d: i64) -> Option<Place> {
        if self.groups.is_empty() {
            return None;
        }
        let at = self.find(items, key).map_or(0, |p| p.group as i64 + d);
        let g = at.clamp(0, self.groups.len() as i64 - 1) as usize;
        // That group's first item, there (it may be listed in an earlier
        // group too).
        let p = self.place(items, self.groups[g].items[0])?;
        Some(Place {
            group: g,
            offset: 0,
            ..p
        })
    }
}
