//! The review page's list as the server groups it: lenses, the filter,
//! the groups and the order J and K walk (`review_list`).

use sqldiff_convert::queue::{Item, Kind};
use sqldiff_gui::convert_review::ListedItem;
use sqldiff_gui::review_list::{causes, list, Group, Place, Query};

fn unit(key: &str, tier: u8, path: &str, open: &[&str]) -> ListedItem {
    ListedItem {
        item: Item {
            key: key.into(),
            tier,
            kind: Kind::Unit,
            path: path.into(),
            unit: Some(key.into()),
            src_line: 1,
            out_line: 1,
            status: "under assumptions".into(),
            rules: open.iter().map(|s| s.to_string()).collect(),
            reason: format!("reason of {key}"),
            units: Vec::new(),
            instances: 0,
            failing_instances: Vec::new(),
            open: open.iter().map(|s| s.to_string()).collect(),
            written_by: None,
            outdated: false,
        },
        open_assumptions: open.iter().map(|s| s.to_string()).collect(),
        blocking: Vec::new(),
        hashes: None,
    }
}

fn query(tiers: &[u8], group: Group, filter: &str) -> Query {
    Query {
        tiers: tiers.to_vec(),
        group,
        filter: filter.into(),
    }
}

fn keys(items: &[ListedItem], v: &[u32]) -> Vec<String> {
    v.iter()
        .map(|&i| items[i as usize].item.key.clone())
        .collect()
}

#[test]
fn causes_are_what_is_open_else_what_blocks() {
    let mut a = unit("a", 5, "x.sql", &["ASM-B", "ASM-A", "ASM-B"]);
    assert_eq!(causes(&a), ["ASM-B", "ASM-A"]);
    a.item.open.clear();
    assert_eq!(causes(&a), ["under assumptions"]);
    a.item.tier = 3;
    assert_eq!(causes(&a), ["written by an agent"]);
    a.item.tier = 6;
    assert_eq!(causes(&a), ["nothing found, not proven"]);
    a.item.tier = 1;
    assert_eq!(causes(&a), ["rejected in review"]);
    a.item.tier = 2;
    a.item.status = "unproven".into();
    assert_eq!(causes(&a), ["not proven"]);
    a.blocking = vec!["x.b".into(), "x.a".into(), "x.b".into()];
    assert_eq!(causes(&a), ["x.b", "x.a"]);
    let mut m = unit("m", 1, "y.sql", &[]);
    m.item.kind = Kind::MissingObject;
    assert_eq!(causes(&m), ["missing object"]);
    m.item.rules = vec!["bind.unknown-table".into(); 2];
    assert_eq!(causes(&m), ["bind.unknown-table"]);
}

#[test]
fn a_query_takes_tier_6_only_with_all() {
    let q = Query::parse("tiers=3,4,5,6&group=file&q=a%20b", false);
    assert_eq!(q, query(&[3, 4, 5], Group::File, "a b"));
    assert_eq!(Query::parse("tiers=6", true).tiers, [6]);
    assert_eq!(Query::parse("", false), query(&[], Group::Cause, ""));
}

#[test]
fn groups_by_cause_biggest_first_with_a_unit_in_each_of_its_groups() {
    let items = vec![
        unit("u1", 5, "b.sql", &["ASM-X"]),
        unit("u2", 5, "a.sql", &["ASM-X", "asm-y"]),
        unit("u3", 4, "a.sql", &["ASM-Z"]),
        unit("u4", 2, "c.sql", &[]),
        unit("u5", 5, "c.sql", &["ASM-Y"]),
    ];
    let l = list(&items, &query(&[3, 4, 5], Group::Cause, ""));
    let g: Vec<(&str, Vec<String>)> = l
        .groups
        .iter()
        .map(|g| (g.key.as_str(), keys(&items, &g.items)))
        .collect();
    // The biggest first; then by key without case, and as written for keys
    // that differ only in case ("ASM-Y" before "asm-y"); "ASM-Z" after both.
    assert_eq!(
        g,
        [
            ("ASM-X", vec!["u1".to_string(), "u2".into()]),
            ("ASM-Y", vec!["u5".into()]),
            ("asm-y", vec!["u2".into()]),
            ("ASM-Z", vec!["u3".into()]),
        ]
    );
    assert_eq!(l.shown, 4);
    // Each item once, group by group.
    assert_eq!(keys(&items, &l.order), ["u1", "u2", "u5", "u3"]);
    // A lone unit's group says its path; "accept for all" counts the units
    // with the cause open.
    assert_eq!(l.groups[1].desc, "c.sql");
    assert_eq!(l.groups[0].desc, "reason of u1");
    assert_eq!(l.groups[0].accept, 2);
}

#[test]
fn a_unit_that_needs_a_fix_is_grouped_by_its_first_blocker_only() {
    let mut a = unit("a", 1, "a.sql", &[]);
    a.blocking = vec!["x.first".into(), "x.second".into()];
    let items = vec![a];
    let l = list(&items, &query(&[1, 2], Group::Cause, ""));
    assert_eq!(l.groups.len(), 1);
    assert_eq!(l.groups[0].key, "x.first");
}

#[test]
fn groups_by_file_and_by_tier() {
    let items = vec![
        unit("u1", 5, "b.sql", &["ASM-X"]),
        unit("u2", 4, "a.sql", &["ASM-Y"]),
        unit("u3", 5, "a.sql", &["ASM-X", "ASM-Z"]),
    ];
    let l = list(&items, &query(&[4, 5], Group::File, ""));
    let g: Vec<(&str, &str)> = l
        .groups
        .iter()
        .map(|g| (g.key.as_str(), g.desc.as_str()))
        .collect();
    assert_eq!(g, [("a.sql", "ASM-Y, ASM-X, ASM-Z"), ("b.sql", "ASM-X")]);
    assert!(l.groups.iter().all(|g| g.accept == 0));
    let l = list(&items, &query(&[4, 5], Group::Tier, ""));
    let g: Vec<&str> = l.groups.iter().map(|g| g.key.as_str()).collect();
    assert_eq!(g, ["4", "5"]);
}

#[test]
fn the_filter_matches_path_reason_or_cause_without_case() {
    let items = vec![
        unit("u1", 5, "Sales/Orders.sql", &["ASM-X"]),
        unit("u2", 5, "b.sql", &["ASM-COLLATION"]),
        unit("u3", 5, "c.sql", &["ASM-Y"]),
    ];
    let shown = |f: &str| {
        let l = list(&items, &query(&[5], Group::Cause, f));
        keys(&items, &l.order)
    };
    assert_eq!(shown("  orders "), ["u1"]);
    assert_eq!(shown("collation"), ["u2"]);
    assert_eq!(shown("REASON OF U3"), ["u3"]);
    assert!(shown("nothing like it").is_empty());
    // Outside the lens, never.
    assert!(list(&items, &query(&[4], Group::Cause, ""))
        .order
        .is_empty());
}

#[test]
fn stepping_walks_the_order_and_stops_at_the_ends() {
    let items = vec![
        unit("u1", 5, "a.sql", &["ASM-X"]),
        unit("u2", 5, "a.sql", &["ASM-X", "ASM-Y"]),
        unit("u3", 5, "b.sql", &["ASM-Y"]),
        unit("u4", 5, "b.sql", &["ASM-Z"]),
    ];
    let l = list(&items, &query(&[5], Group::Cause, ""));
    // Groups: ASM-X [u1, u2], ASM-Y [u2, u3], ASM-Z [u4]; order u1 u2 u3 u4.
    let key = |p: Option<Place>| p.map(|p| p.key);
    assert_eq!(key(l.step(&items, "u1", 1)), Some("u2".into()));
    assert_eq!(key(l.step(&items, "u2", 1)), Some("u3".into()));
    assert_eq!(key(l.step(&items, "u4", 5)), Some("u4".into()));
    assert_eq!(key(l.step(&items, "u1", -1)), Some("u1".into()));
    // Not shown: the first.
    assert_eq!(key(l.step(&items, "gone", 1)), Some("u1".into()));
    // u3 is listed first in ASM-Y, second.
    let p = l.find(&items, "u3").unwrap();
    assert_eq!((p.index, p.group, p.offset), (2, 1, 1));
    // The next group's first item, there: u2 is listed in ASM-X first.
    let p = l.step_group(&items, "u1", 1).unwrap();
    assert_eq!((p.key.as_str(), p.group, p.offset), ("u2", 1, 0));
    let p = l.step_group(&items, "u4", 1).unwrap();
    assert_eq!((p.key.as_str(), p.group), ("u4", 2));
    // By index, clamped: what takes a place left by a decided item.
    assert_eq!(key(l.at(&items, 9)), Some("u4".into()));
    assert!(list(&items, &query(&[1], Group::Cause, ""))
        .at(&items, 0)
        .is_none());
}
