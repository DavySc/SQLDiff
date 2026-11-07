//! The edit checker answers "does another change interact with this one" from
//! state shared between the edits of a routine. With `SQLDIFF_EDIT_ORACLE`
//! set, every shared answer is compared with the exact per-edit scan and a
//! difference panics. This test runs that on generated routines: straight
//! code, IF and WHILE nesting (edits on compound statements and inside
//! them), shared variables and temp tables, dynamic SQL (the any-name on the
//! read and write side), jumps, and between 5 and 45 percent of the statements
//! edited, in both directions and for both dialects.

use sqldiff_core::{diff, Artifact, Dialect, Options};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A statement skeleton: a template where `{}` stands for the constant the
/// edit changes, and the statements nested in it.
struct Node {
    head: &'static str,
    k: u64,
    body: Vec<Node>,
    tail: &'static str,
    id: usize,
}

const LEAVES_T: &[&str] = &[
    "SET @v1 = @p + {};",
    "SET @v2 = @v1 * {};",
    "SET @v3 = @v2 - {};",
    "INSERT INTO #t (a) VALUES (@v1 + {});",
    "UPDATE #t SET a = a + {} WHERE a > @v2;",
    "SELECT a FROM #t WHERE a = @v3 + {};",
    "EXEC(@sql);",
    "SET @sql = 'select {}';",
    "SELECT @v1 + {};",
];

const LEAVES_P: &[&str] = &[
    "v1 := p + {};",
    "v2 := v1 * {};",
    "v3 := v2 - {};",
    "INSERT INTO t1 (a) VALUES (v1 + {});",
    "UPDATE t1 SET a = a + {} WHERE a > v2;",
    "PERFORM a FROM t1 WHERE a = v3 + {};",
    "EXECUTE sql;",
    "sql := 'select {}';",
    "PERFORM v1 + {};",
];

fn gen(rng: &mut Rng, depth: u32, next_id: &mut usize, tsql: bool, n: u64) -> Vec<Node> {
    let leaves = if tsql { LEAVES_T } else { LEAVES_P };
    let mut out = Vec::new();
    for _ in 0..n {
        let id = *next_id;
        *next_id += 1;
        let pick = rng.below(10);
        let node = if depth < 2 && pick < 2 {
            let body_n = 1 + rng.below(3);
            let body = gen(rng, depth + 1, next_id, tsql, body_n);
            if tsql {
                Node {
                    head: "IF @v1 > {} BEGIN",
                    k: rng.below(5),
                    body,
                    tail: "END",
                    id,
                }
            } else {
                Node {
                    head: "IF v1 > {} THEN",
                    k: rng.below(5),
                    body,
                    tail: "END IF;",
                    id,
                }
            }
        } else if depth < 2 && pick == 2 {
            let body_n = 1 + rng.below(2);
            let body = gen(rng, depth + 1, next_id, tsql, body_n);
            if tsql {
                Node {
                    head: "WHILE @v2 < {} BEGIN",
                    k: rng.below(5),
                    body,
                    tail: "SET @v2 = @v2 + 1; END",
                    id,
                }
            } else {
                Node {
                    head: "WHILE v2 < {} LOOP",
                    k: rng.below(5),
                    body,
                    tail: "v2 := v2 + 1; END LOOP;",
                    id,
                }
            }
        } else if pick == 3 {
            if tsql {
                Node {
                    head: "IF @v1 = {} RETURN;",
                    k: rng.below(5),
                    body: vec![],
                    tail: "",
                    id,
                }
            } else {
                Node {
                    head: "IF v1 = {} THEN RETURN; END IF;",
                    k: rng.below(5),
                    body: vec![],
                    tail: "",
                    id,
                }
            }
        } else if pick == 4 && tsql {
            Node {
                head: "IF @v3 = {} THROW 50000, 'x', 1;",
                k: rng.below(5),
                body: vec![],
                tail: "",
                id,
            }
        } else {
            let head = leaves[rng.below(leaves.len() as u64) as usize];
            Node {
                head,
                k: rng.below(9),
                body: vec![],
                tail: "",
                id,
            }
        };
        out.push(node);
    }
    out
}

fn render(nodes: &[Node], edited: &[bool], b: bool, out: &mut String) {
    for n in nodes {
        let k = if b && edited[n.id] { n.k + 1 } else { n.k };
        out.push_str(&n.head.replace("{}", &k.to_string()));
        out.push('\n');
        render(&n.body, edited, b, out);
        if !n.tail.is_empty() {
            out.push_str(n.tail);
            out.push('\n');
        }
    }
}

fn routine(nodes: &[Node], edited: &[bool], b: bool, tsql: bool) -> String {
    let mut body = String::new();
    render(nodes, edited, b, &mut body);
    if tsql {
        format!(
            "CREATE PROCEDURE dbo.r @p int AS\nBEGIN\nDECLARE @v1 int = 0, @v2 int = 0, @v3 int = 0, @sql nvarchar(100) = N'';\nCREATE TABLE #t (a int);\n{body}SELECT a FROM #t;\nEND\n"
        )
    } else {
        format!(
            "CREATE FUNCTION r(p int) RETURNS void LANGUAGE plpgsql AS $$\nDECLARE v1 int := 0; v2 int := 0; v3 int := 0; sql text := '';\nBEGIN\n{body}END $$;\n"
        )
    }
}

#[test]
fn shared_state_agrees_with_the_exact_scan() {
    // Every test in this binary needs it, and this is the only one.
    std::env::set_var("SQLDIFF_EDIT_ORACLE", "1");
    let mut rng = Rng(0x5eed);
    let mut compared = 0usize;
    for case in 0..60 {
        let tsql = case % 2 == 0;
        let d = if tsql {
            Dialect::Tsql
        } else {
            Dialect::Postgres
        };
        let n = 4 + rng.below(18);
        let mut ids = 0;
        let nodes = gen(&mut rng, 0, &mut ids, tsql, n);
        let pct = 5 + rng.below(41);
        let edited: Vec<bool> = (0..ids).map(|_| rng.below(100) < pct).collect();
        let (a, b) = (
            routine(&nodes, &edited, false, tsql),
            routine(&nodes, &edited, true, tsql),
        );
        let ddl = if tsql {
            "CREATE TABLE dbo.t1 (a int);"
        } else {
            "CREATE TABLE t1 (a int);"
        };
        let opts = Options {
            catalog_a: vec![Artifact::new("catalog.sql", d, ddl)],
            catalog_b: vec![Artifact::new("catalog.sql", d, ddl)],
            ..Options::default()
        };
        for (x, y) in [(&a, &b), (&b, &a)] {
            let r = diff(
                &Artifact::new("a.sql", d, x.as_str()),
                &Artifact::new("b.sql", d, y.as_str()),
                &opts,
            );
            compared += r.findings.len();
        }
    }
    assert!(compared > 200, "the generated cases produce findings");
}

/// Fixed shapes for the sweep's wake-ups. The generator reaches them only by
/// chance, so each is pinned here: a unit that reads only through the
/// any-name (a quoted identifier `*` is the only way a read gets it) is offered while the taint is still empty, and a
/// later edit makes the taint non-empty; the unit must then fire, and its
/// any-name write must reach the last edit.
#[test]
fn sweep_wake_ups_agree_with_the_exact_scan() {
    std::env::set_var("SQLDIFF_EDIT_ORACLE", "1");
    let tsql = |k1: u32, k2: u32| {
        format!(
            "CREATE PROCEDURE dbo.r @p int, @q int AS\nBEGIN\nDECLARE @v2 int = 0;\nSET @q = (SELECT @p AS [*]);\nSET @v2 = @p + {k1};\nSELECT @q + {k2};\nEND\n"
        )
    };
    let pg = |k1: u32, k2: u32| {
        format!(
            "CREATE FUNCTION r(p int, q int) RETURNS void LANGUAGE plpgsql AS $$\nDECLARE v2 int := 0;\nBEGIN\nq := (SELECT p AS \"*\");\nv2 := p + {k1};\nPERFORM q + {k2};\nEND $$;\n"
        )
    };
    // The same unit between the two edits: the reach sweep must wake it.
    let tsql_mid = |k1: u32, k2: u32| {
        format!(
            "CREATE PROCEDURE dbo.r @p int, @q int AS\nBEGIN\nDECLARE @v2 int = 0;\nSET @v2 = @p + {k1};\nSET @q = (SELECT @p AS [*]);\nSELECT @q + {k2};\nEND\n"
        )
    };
    let pg_mid = |k1: u32, k2: u32| {
        format!(
            "CREATE FUNCTION r(p int, q int) RETURNS void LANGUAGE plpgsql AS $$\nDECLARE v2 int := 0;\nBEGIN\nv2 := p + {k1};\nq := (SELECT p AS \"*\");\nPERFORM q + {k2};\nEND $$;\n"
        )
    };
    let mut answered = 0;
    for d in [Dialect::Tsql, Dialect::Postgres] {
        let mid = |a: u32, b: u32| {
            if d == Dialect::Tsql {
                tsql_mid(a, b)
            } else {
                pg_mid(a, b)
            }
        };
        for (x, y) in [(mid(1, 1), mid(2, 2)), (mid(2, 2), mid(1, 1))] {
            let r = diff(
                &Artifact::new("a.sql", d, x.as_str()),
                &Artifact::new("b.sql", d, y.as_str()),
                &Options::default(),
            );
            answered += r.findings.len();
        }
        let make = |a: u32, b: u32| {
            if d == Dialect::Tsql {
                tsql(a, b)
            } else {
                pg(a, b)
            }
        };
        // both statements after the dynamic call are edited
        for (x, y) in [(make(1, 1), make(2, 2)), (make(2, 2), make(1, 1))] {
            let r = diff(
                &Artifact::new("a.sql", d, x.as_str()),
                &Artifact::new("b.sql", d, y.as_str()),
                &Options::default(),
            );
            answered += r.findings.len();
        }
    }
    assert!(answered > 0);
}

/// The shapes the design review asked for that the generator reaches only by
/// chance: statements that write nothing but the status names, a finding
/// that exists on one side only (an added statement), and findings that are
/// no candidate edit (a changed declaration, a changed parameter default)
/// sitting in the span list beside edits. Every subset of the constants is
/// edited, in both directions, for both dialects.
#[test]
fn status_only_one_sided_and_non_candidate_findings_agree_with_the_exact_scan() {
    std::env::set_var("SQLDIFF_EDIT_ORACLE", "1");
    // (declared default, [k1, k2, k3], extra statement in the routine)
    let tsql = |d: u32, k: [u32; 3], extra: bool| {
        let add = if extra { "SET @v3 = @p;\n" } else { "" };
        format!(
            "CREATE PROCEDURE dbo.r @p int AS\nBEGIN\nDECLARE @v2 int = {d}, @v3 int = 0;\nSELECT @p + {};\nSET @v2 = @p + {};\n{add}SELECT @v2 + {};\nSELECT @v3 + 1;\nEND\n",
            k[0], k[1], k[2]
        )
    };
    let pg = |d: u32, k: [u32; 3], extra: bool| {
        let add = if extra { "v3 := p;\n" } else { "" };
        format!(
            "CREATE FUNCTION r(p int) RETURNS void LANGUAGE plpgsql AS $$\nDECLARE v2 int := {d}; v3 int := 0;\nBEGIN\nPERFORM p + {};\nv2 := p + {};\n{add}PERFORM v2 + {};\nPERFORM v3 + 1;\nEND $$;\n",
            k[0], k[1], k[2]
        )
    };
    let mut answered = 0;
    for d in [Dialect::Tsql, Dialect::Postgres] {
        let make = |dflt: u32, k: [u32; 3], extra: bool| {
            if d == Dialect::Tsql {
                tsql(dflt, k, extra)
            } else {
                pg(dflt, k, extra)
            }
        };
        for mask in 0..8u32 {
            for (dflt, extra) in [(0u32, false), (1, false), (0, true), (1, true)] {
                if mask == 0 && dflt == 0 && !extra {
                    continue;
                }
                let bit = |n: u32| 1 + ((mask >> n) & 1);
                let x = make(0, [1, 1, 1], false);
                let y = make(dflt, [bit(0), bit(1), bit(2)], extra);
                for (p, q) in [(&x, &y), (&y, &x)] {
                    let r = diff(
                        &Artifact::new("a.sql", d, p.as_str()),
                        &Artifact::new("b.sql", d, q.as_str()),
                        &Options::default(),
                    );
                    answered += r.findings.len();
                }
            }
        }
    }
    assert!(answered > 0);
}
