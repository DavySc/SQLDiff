//! A unit the engine's diff could not decide is "not proven": never
//! "converted" or "under assumptions", and never "differs" either.

use sqldiff_convert::{check, rewrite, undecided, Conversion, Status, Unit};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{ChangeClass, Dialect, Finding, Options, Severity, Verdict};
use std::sync::Arc;

const ORDERS: &str =
    "CREATE TABLE dbo.Orders (id int NOT NULL PRIMARY KEY, amt decimal(10,2) NOT NULL);\nGO\n";

fn proc(predicate: &str) -> String {
    format!(
        "{ORDERS}CREATE PROCEDURE dbo.DelOrders AS\nBEGIN\n  SET NOCOUNT ON;\n  DELETE FROM dbo.Orders WHERE {predicate};\nEND;\nGO\n"
    )
}

fn catalog(text: &str, d: Dialect) -> Arc<Catalog> {
    let mut c = Catalog::default();
    ddl::load_tree(&mut c, &sqldiff_syntax::parse(text, d).syntax(), d);
    Arc::new(c)
}

/// Converted and checked with the catalogs of both sides, as `sqldiff
/// convert` checks a tree.
fn convert(src: &str, to: Dialect) -> Conversion {
    let draft = rewrite(src, Dialect::Tsql, to);
    let opts = Options {
        facts_a: vec![catalog(src, Dialect::Tsql)],
        facts_b: vec![catalog(draft.output(), to)],
        ..Options::default()
    };
    check(src, draft, &opts)
}

fn delete<'a>(c: &'a Conversion, src: &str) -> &'a Unit {
    let at = src.find("DELETE").unwrap();
    c.units
        .iter()
        .filter(|u| u.src.start <= at && at < u.src.end)
        .min_by_key(|u| u.src.end - u.src.start)
        .unwrap()
}

fn has_rule(u: &Unit, rule: &str) -> bool {
    u.notes.iter().any(|n| n.rule == rule)
}

#[test]
fn a_literal_the_engines_convert_differently_is_not_proven() {
    // SQL Server rounds '1.005' to the column's scale (1.01); PostgreSQL
    // compares it exactly, and Db2 for i is not modelled. The rules only
    // assumed the schema and the collation, so this used to read "under
    // assumptions".
    let src = proc("amt = '1.005'");
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let c = convert(&src, to);
        let u = delete(&c, &src);
        assert_eq!(u.status, Status::Unproven, "{to:?}: {:?}", u.notes);
        assert_eq!(u.checked, Status::Unproven, "{to:?}");
        assert!(has_rule(u, "ASM-DBO-SCHEMA"), "{to:?}: {:?}", u.notes);
        let n = u.notes.iter().find(|n| n.rule == "equiv.unproven");
        let n = n.unwrap_or_else(|| panic!("{to:?}: {:?}", u.notes));
        assert_eq!(n.status, Status::Unproven);
        if to == Dialect::Postgres {
            assert!(n.text.contains("'1.005'"), "{}", n.text);
        }
        // Notes stay worst first.
        assert_eq!(u.notes[0].status, Status::Unproven, "{:?}", u.notes);
        // Unknown is not a difference.
        assert_eq!(c.counts.differs, 0, "{to:?}: {}", c.output);
        // Db2 for i tables are not compared yet: the CREATE TABLE is not
        // proven there either.
        let tables = usize::from(to == Dialect::Db2i);
        assert_eq!(c.counts.unproven, 1 + tables, "{to:?}");
    }
}

#[test]
fn a_literal_both_engines_read_alike_stays_under_assumptions() {
    // No rounding: '1.25' is 1.25 on SQL Server and PostgreSQL alike.
    let src = proc("amt = '1.25'");
    let c = convert(&src, Dialect::Postgres);
    let u = delete(&c, &src);
    assert_eq!(u.status, Status::Assumptions, "{:?}", u.notes);
    assert_eq!(u.checked, Status::Converted);
    assert_eq!(c.counts.unproven, 0, "{}", c.output);
}

#[test]
fn not_proven_ranks_between_decision_and_differs() {
    // A unit takes the worst of its notes and findings: an unknown never
    // hides a difference, and outranks what the rules merely assumed.
    assert!(Status::Assumptions < Status::Unproven);
    assert!(Status::Decision < Status::Unproven);
    assert!(Status::Unproven < Status::Differs);
    assert_eq!(Status::Unproven.label(), "not proven");
}

fn finding(rule: &str, class: ChangeClass, verdict: Verdict) -> Finding {
    Finding {
        fingerprint: String::new(),
        rule: rule.into(),
        class,
        severity: Severity::Medium,
        verdict,
        witness: String::new(),
        a: None,
        b: None,
        assumptions: Vec::new(),
    }
}

#[test]
fn only_findings_about_the_pair_leave_a_unit_undecided() {
    use ChangeClass as C;
    for (rule, class) in [
        ("equiv.unproven", C::Semantic),
        ("equiv.unsupported", C::Semantic),
        ("txn.durability", C::Transactional),
        ("error.control", C::Semantic),
        ("write.unpaired", C::Semantic),
    ] {
        assert!(undecided(&finding(rule, class, Verdict::Unknown)), "{rule}");
        for v in [
            Verdict::Different,
            Verdict::Equal,
            Verdict::EqualUnderAssumptions,
        ] {
            assert!(!undecided(&finding(rule, class, v)), "{rule} {v:?}");
        }
    }
    // One side's own code (analysis hits, lints, hazards), and what the
    // output's own check decides.
    for (rule, class) in [
        ("rules.semantic/top-without-order", C::Semantic),
        ("hazard.terminator-before-throw", C::Semantic),
        ("bind.unknown-table", C::Binding),
        ("residue.tsql.type", C::Residue),
        ("limit.identifier-length", C::Limit),
    ] {
        assert!(
            !undecided(&finding(rule, class, Verdict::Unknown)),
            "{rule}"
        );
    }
}

#[test]
fn a_statement_the_engine_never_compared_is_not_converted() {
    // The engine does not model CREATE INDEX: the rules' output for it may
    // be right, but nothing compared it with the original.
    let src = format!("{ORDERS}CREATE INDEX IX_Orders_Amt ON dbo.Orders (amt);\nGO\n");
    let c = convert(&src, Dialect::Postgres);
    let at = src.find("CREATE INDEX").unwrap();
    let u = c
        .units
        .iter()
        .find(|u| u.src.start <= at && at < u.src.end)
        .unwrap();
    assert_eq!(u.status, Status::Unproven, "{:?}", u.notes);
    assert_eq!(u.checked, Status::Unproven);
    let n = u
        .notes
        .iter()
        .find(|n| n.rule == sqldiff_convert::NOT_COMPARED)
        .unwrap_or_else(|| panic!("{:?}", u.notes));
    assert!(n.text.contains("CREATE"), "{}", n.text);
    // The table itself is compared, and converts.
    let t = c.units.iter().find(|u| u.src.start == 0).unwrap();
    assert!(t.status <= Status::Assumptions, "{:?}", t.notes);
    assert!(!has_rule(t, sqldiff_convert::NOT_COMPARED));
}
