//! `CREATE ROLE name` becomes `CREATE ROLE name NOLOGIN` (rule
//! `tsql.stmt.create-role`, `ASM-ROLE-NOLOGIN`): a PostgreSQL role has a login
//! only when given one. Options and Db2 for i stay refused with the reason.

use sqldiff_convert::{check, rewrite_in, Conversion};
use sqldiff_core::{Dialect, Options};

fn pg(src: &str) -> Conversion {
    let opts = Options {
        catalog_a: vec![],
        catalog_b: vec![],
        ..Options::default()
    };
    check(
        src,
        rewrite_in(src, Dialect::Tsql, Dialect::Postgres, None),
        &opts,
    )
}

#[test]
fn create_role_is_a_nologin_role() {
    let c = pg("CREATE ROLE report_readers;\nGO\nCREATE ROLE [External Sales];\n");
    assert!(
        c.output.contains("CREATE ROLE report_readers NOLOGIN;"),
        "{}",
        c.output
    );
    assert!(
        c.output.contains("CREATE ROLE \"External Sales\" NOLOGIN;"),
        "{}",
        c.output
    );
    assert!(format!("{:?}", c.units).contains("ASM-ROLE-NOLOGIN"));
}

#[test]
fn role_options_stay_refused() {
    let c = pg("CREATE ROLE report_readers AUTHORIZATION dbo;\n");
    assert!(!c.output.contains("NOLOGIN"), "{}", c.output);
    assert!(c.counts.not_converted > 0, "{:?}", c.counts);
}
