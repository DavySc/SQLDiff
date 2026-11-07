//! T-SQL `CROSS APPLY` / `OUTER APPLY` of a subquery become PostgreSQL
//! `CROSS JOIN LATERAL` / `LEFT JOIN LATERAL … ON TRUE`.

use sqldiff_convert::convert;
use sqldiff_core::Dialect;

const TABLES: &str = "create table \"p\" (\"id\" int not null)\ngo\ncreate table \"c\" (\"id\" int not null, \"p_id\" int not null, \"at\" datetime2 not null)\ngo\n";

fn out(view: &str) -> String {
    convert(&format!("{TABLES}{view}"), Dialect::Tsql, Dialect::Postgres)
        .unwrap()
        .output
}

#[test]
fn cross_apply_becomes_cross_join_lateral() {
    let o = out("create view \"v\" as\nselect p.id, l.at from \"p\" p\ncross apply (select top 1 c.at as at from \"c\" c where c.p_id = p.id order by c.at desc) l\ngo\n");
    assert!(o.contains("CROSS JOIN LATERAL (select c.at as at"), "{o}");
    assert!(!o.to_lowercase().contains("apply"), "{o}");
}

#[test]
fn outer_apply_becomes_left_join_lateral_on_true() {
    let o = out("create view \"v\" as\nselect p.id, l.at from \"p\" p\nouter apply (select top 1 c.at as at from \"c\" c where c.p_id = p.id order by c.at desc) as l\nwhere p.id > 0\ngo\n");
    assert!(o.contains("LEFT JOIN LATERAL (select c.at as at"), "{o}");
    assert!(o.contains("LIMIT 1) as l ON TRUE\nwhere p.id > 0"), "{o}");
    assert!(!o.to_lowercase().contains("apply"), "{o}");
}
