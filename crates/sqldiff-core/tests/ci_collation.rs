//! Davy, 2026-10-29: a conversion keeps the target's text types, and each
//! statement whose result T-SQL's case-insensitive, trailing-space-blind
//! string equality can change is flagged with `ASM-CI-COLLATION` for the
//! reviewer. Plain diffs still report the difference. Ordering depends on
//! more than case and trailing spaces and stays not proven.

use sqldiff_core::equiv::compare::ci_collation_scope;
use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const DDL: &str = "
CREATE TABLE customer (customer_id integer PRIMARY KEY, email varchar(254) NOT NULL,
  nick varchar(10), code char(5), region_id integer NOT NULL);
";

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;
const D: Dialect = Dialect::Db2i;

fn run(a: &str, da: Dialect, b: &str, db: Dialect, catalog: bool, conversion: bool) -> Report {
    let opts = if catalog {
        Options {
            catalog_a: vec![Artifact::new("catalog.sql", da, DDL)],
            catalog_b: vec![Artifact::new("catalog.sql", db, DDL)],
            ..Options::default()
        }
    } else {
        Options::default()
    };
    let _ci = ci_collation_scope(conversion);
    diff(
        &Artifact::new("a.sql", da, a),
        &Artifact::new("b.sql", db, b),
        &opts,
    )
}

fn view(d: Dialect, body: &str) -> String {
    match d {
        T => format!(
            "CREATE VIEW dbo.v AS {}",
            body.replace("FROM customer", "FROM dbo.customer")
        ),
        _ => format!("CREATE VIEW v AS {body}"),
    }
}

/// The same view body on T-SQL and on `d`, in a conversion check.
fn convert(body: &str, d: Dialect) -> Report {
    run(&view(T, body), T, &view(d, body), d, true, true)
}

fn ids(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|a| a.id.as_str()).collect()
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

fn flagged(r: &Report) {
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?}",
        r.findings
    );
    assert!(ids(r).contains(&"ASM-CI-COLLATION"), "{:?}", ids(r));
}

fn not_equal(r: &Report) {
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}: {:?}",
        r.verdict(),
        r.findings
    );
}

#[test]
fn string_equality_is_flagged_in_a_conversion_and_different_in_a_diff() {
    let body = "SELECT customer_id FROM customer WHERE email = 'A@x.com';";
    flagged(&convert(body, P));
    let r = run(&view(T, body), T, &view(P, body), P, true, false);
    assert_eq!(r.verdict(), Verdict::Different);
    assert!(rules(&r).contains(&"equiv.case-sensitivity"));
    assert!(rules(&r).contains(&"equiv.trailing-spaces"));
    // <> and a comparison of two columns (as in a join condition).
    flagged(&convert(
        "SELECT customer_id FROM customer WHERE email <> nick;",
        P,
    ));
    flagged(&convert(
        "SELECT a.customer_id FROM customer a JOIN customer b ON a.email = b.nick;",
        P,
    ));
    // Db2 for i blank-pads like T-SQL; only the case differs.
    let r = convert(body, D);
    flagged(&r);
    assert!(ids(&r).contains(&"ASM-DB2-SRTSEQ-HEX"), "{:?}", ids(&r));
}

#[test]
fn string_in_lists_are_flagged_only_with_string_types() {
    let body =
        "SELECT customer_id FROM customer WHERE email IN ('Novelty Items', 'USB Novelties');";
    flagged(&convert(body, P));
    let not_in = "SELECT customer_id FROM customer WHERE email NOT IN ('a', 'b');";
    flagged(&convert(not_in, P));
    // Without a catalog the operand's type is unknown: it may be a number
    // the literals convert to, so nothing is flagged.
    not_equal(&run(&view(T, body), T, &view(P, body), P, false, true));
    // Not a collation difference: plain diffs keep it unproven.
    not_equal(&run(&view(T, body), T, &view(P, body), P, true, false));
}

#[test]
fn grouping_and_distinct_are_flagged() {
    flagged(&convert(
        "SELECT email, COUNT(*) AS n FROM customer GROUP BY email;",
        P,
    ));
    flagged(&convert("SELECT DISTINCT email FROM customer;", P));
}

#[test]
fn ordering_is_not_covered() {
    for body in [
        "SELECT customer_id FROM customer WHERE email < 'm';",
        "SELECT email FROM customer ORDER BY email;",
    ] {
        let r = convert(body, P);
        not_equal(&r);
    }
}

/// LIKE on varchar with a literal pattern differs only by the collation
/// (case, the value's trailing spaces, expanded characters; checked on SQL
/// Server 2022 and PostgreSQL 18). A `[` (T-SQL character class) or `\`
/// (PostgreSQL escape) in the pattern, a pattern that is not a literal and a
/// blank-padded char value are not covered.
#[test]
fn like_on_varchar_with_a_plain_pattern_is_flagged() {
    flagged(&convert(
        "SELECT customer_id FROM customer WHERE email LIKE 'a%';",
        P,
    ));
    flagged(&convert(
        "SELECT customer_id FROM customer WHERE email NOT LIKE '%@x_com';",
        P,
    ));
    for body in [
        "SELECT customer_id FROM customer WHERE email LIKE '[ab]%';",
        "SELECT customer_id FROM customer WHERE email LIKE 'a\\_%';",
        "SELECT customer_id FROM customer WHERE email LIKE nick;",
        "SELECT customer_id FROM customer WHERE code LIKE 'a%';",
    ] {
        not_equal(&convert(body, P));
    }
    // The pattern is still compared.
    not_equal(&run(
        &view(T, "SELECT customer_id FROM customer WHERE email LIKE 'a%';"),
        T,
        &view(P, "SELECT customer_id FROM customer WHERE email LIKE 'b%';"),
        P,
        true,
        true,
    ));
    // A plain diff keeps it unproven.
    let body = "SELECT customer_id FROM customer WHERE email LIKE 'a%';";
    not_equal(&run(&view(T, body), T, &view(P, body), P, true, false));
}

/// The assumption covers the collation only: any other change between the
/// sides is still found.
#[test]
fn other_changes_next_to_a_flagged_comparison_are_found() {
    let ts = view(
        T,
        "SELECT customer_id FROM customer WHERE email IN ('A@x.com', 'b');",
    );
    for pg in [
        // An item dropped.
        "SELECT customer_id FROM customer WHERE email IN ('A@x.com');",
        // A literal's case changed: the stored value is compared, not folded.
        "SELECT customer_id FROM customer WHERE email IN ('a@x.com', 'b');",
        // NOT IN for IN.
        "SELECT customer_id FROM customer WHERE email NOT IN ('A@x.com', 'b');",
        // Another column.
        "SELECT customer_id FROM customer WHERE nick IN ('A@x.com', 'b');",
    ] {
        not_equal(&run(&ts, T, &view(P, pg), P, true, true));
    }
    let ts = view(
        T,
        "SELECT customer_id FROM customer WHERE email = 'A@x.com';",
    );
    for pg in [
        "SELECT customer_id FROM customer WHERE email = 'a@x.com';",
        "SELECT customer_id FROM customer WHERE email <> 'A@x.com';",
        "SELECT customer_id FROM customer WHERE nick = 'A@x.com';",
    ] {
        not_equal(&run(&ts, T, &view(P, pg), P, true, true));
    }
}

#[test]
fn same_dialect_changes_are_not_flagged() {
    let a = view(
        T,
        "SELECT customer_id FROM customer WHERE email = 'A@x.com';",
    );
    let b = view(
        T,
        "SELECT customer_id FROM customer WHERE email = 'A@x.com' ;",
    );
    let r = run(&a, T, &b, T, true, true);
    assert!(!ids(&r).contains(&"ASM-CI-COLLATION"), "{:?}", ids(&r));
    // PostgreSQL varchar against char compares with trailing spaces removed
    // from the char side only: a real difference, not a T-SQL collation.
    let a = view(P, "SELECT customer_id FROM customer WHERE nick = email;");
    let b = view(P, "SELECT customer_id FROM customer WHERE nick = code;");
    not_equal(&run(&a, P, &b, P, true, true));
}

#[test]
fn string_keys_are_flagged_in_a_conversion() {
    let ts = "CREATE TABLE dbo.k (id int NOT NULL PRIMARY KEY, code varchar(10) NOT NULL UNIQUE);";
    let pg = "CREATE TABLE k (id integer NOT NULL PRIMARY KEY, code varchar(10) NOT NULL UNIQUE);";
    let r = run(ts, T, pg, P, false, true);
    assert!(
        !rules(&r).contains(&"equiv.case-sensitivity")
            && !rules(&r).contains(&"equiv.trailing-spaces"),
        "{:?}",
        r.findings
    );
    assert!(ids(&r).contains(&"ASM-CI-COLLATION"), "{:?}", ids(&r));
    let r = run(ts, T, pg, P, false, false);
    assert!(
        rules(&r).contains(&"equiv.case-sensitivity"),
        "{:?}",
        r.findings
    );
}
