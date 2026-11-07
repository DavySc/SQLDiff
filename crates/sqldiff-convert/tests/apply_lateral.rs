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

fn conv(view: &str, to: Dialect) -> (String, sqldiff_convert::Status) {
    let src = format!("{TABLES}{view}");
    let c = convert(&src, Dialect::Tsql, to).unwrap();
    let at = src.find("create view").unwrap();
    let st = c
        .units
        .iter()
        .filter(|u| u.src.start >= at)
        .map(|u| u.status)
        .max()
        .unwrap();
    (c.output, st)
}

#[test]
fn apply_of_a_function_becomes_a_lateral_join_on_postgres() {
    let o =
        out("create view \"v\" as\nselect p.id, f.at from \"p\" p cross apply \"f\"(p.id) f\ngo\n");
    assert!(o.contains("CROSS JOIN LATERAL f(p.id) f"), "{o}");
    let o = out("create view \"v\" as\nselect p.id, f.at from \"p\" p outer apply dbo.\"f\"(p.id) as f (at)\nwhere p.id > 0\ngo\n");
    assert!(o.contains("LEFT JOIN LATERAL"), "{o}");
    assert!(o.contains("f (at) ON TRUE\nwhere p.id > 0"), "{o}");
}

#[test]
fn apply_on_db2_for_i_is_not_converted() {
    for v in [
        "create view \"v\" as\nselect p.id, l.at from \"p\" p cross apply (select top 1 c.at as at from \"c\" c where c.p_id = p.id) l\ngo\n",
        "create view \"v\" as\nselect p.id, f.at from \"p\" p outer apply \"f\"(p.id) f\ngo\n",
    ] {
        let (o, st) = conv(v, Dialect::Db2i);
        assert_eq!(st, sqldiff_convert::Status::NotConverted, "{o}");
    }
}

#[test]
fn an_unrecognised_apply_shape_on_postgres_is_not_converted() {
    let (o, st) = conv(
        "create view \"v\" as\nselect p.id from \"p\" p cross apply @t x\ngo\n",
        Dialect::Postgres,
    );
    assert_eq!(st, sqldiff_convert::Status::NotConverted, "{o}");
}
