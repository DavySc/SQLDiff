//! A long chain of one operator (`a OR b OR c ...`, `a AND b AND ...`) is
//! lowered in linear work and compares as the set of its operands: the order
//! of the terms never matters, a changed term always does.

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

fn view(op: &str, terms: &[String]) -> String {
    format!(
        "CREATE VIEW dbo.v AS SELECT id FROM dbo.t WHERE {};\n",
        terms.join(&format!(" {op} "))
    )
}

fn terms(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("c{} = {i}", i % 40)).collect()
}

/// Runs `f` on a thread with a large stack: the parser and the other passes
/// still recurse once per link of a chain (a parser depth limit is separate
/// work); this test is about the work done per link.
fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn verdicts(a: &str, b: &str) -> Vec<Verdict> {
    let (a, b) = (a.to_string(), b.to_string());
    big_stack(move || verdicts_here(&a, &b))
}

fn verdicts_here(a: &str, b: &str) -> Vec<Verdict> {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a.to_string()),
        &Artifact::new("b.sql", Dialect::Tsql, b.to_string()),
        &Options::default(),
    )
    .findings
    .iter()
    .map(|f| f.verdict)
    .collect()
}

#[test]
fn the_order_of_the_terms_of_a_long_chain_does_not_matter() {
    for op in ["OR", "AND"] {
        let t = terms(300);
        let mut r = t.clone();
        r.reverse();
        assert!(
            verdicts(&view(op, &t), &view(op, &r)).is_empty(),
            "{op}: a reordered chain is equal"
        );
    }
}

#[test]
fn a_changed_term_in_a_long_chain_is_found() {
    for op in ["OR", "AND"] {
        let t = terms(300);
        let mut c = t.clone();
        c[150] = "c7 = 99999".to_string();
        let v = verdicts(&view(op, &t), &view(op, &c));
        assert!(!v.is_empty(), "{op}: a changed term is reported");
    }
}

#[test]
fn nested_and_mixed_chains_flatten_as_before() {
    // (a OR b) OR (c OR d) is the chain a OR b OR c OR d, in any grouping.
    let a =
        "CREATE VIEW dbo.v AS SELECT id FROM dbo.t WHERE (x = 1 OR y = 2) OR (z = 3 OR w = 4);\n";
    let b = "CREATE VIEW dbo.v AS SELECT id FROM dbo.t WHERE w = 4 OR x = 1 OR (z = 3 OR y = 2);\n";
    assert!(verdicts(a, b).is_empty());
    // AND under OR does not flatten into it.
    let c = "CREATE VIEW dbo.v AS SELECT id FROM dbo.t WHERE x = 1 OR (y = 2 AND z = 3);\n";
    let d = "CREATE VIEW dbo.v AS SELECT id FROM dbo.t WHERE x = 1 OR y = 2 OR z = 3;\n";
    assert!(!verdicts(c, d).is_empty());
}
