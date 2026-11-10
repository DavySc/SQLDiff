//! The edit checker answers "does another change interact with this one" from
//! state shared between the edits of a routine. With `SQLDIFF_EDIT_ORACLE`
//! set, every shared answer is compared with the exact per-edit scan and a
//! difference panics. This test runs that on generated routines: straight
//! code, IF and WHILE nesting (edits on compound statements and inside
//! them), shared variables and temp tables, dynamic SQL (the any-name on the
//! read and write side), jumps, and between 5 and 45 percent of the statements
//! edited, in both directions and for both dialects.

use sqldiff_core::equiv::edit::{loop_answer_counts, shared_answer_counts};
use sqldiff_core::{diff, Artifact, Dialect, Options};
use std::sync::Mutex;

/// The tests share the process-wide answer counters, so they run one at a
/// time: the difference of the counters around a test is then its own.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Fails unless the test got at least `shared` answers from the shared
/// sweeps and `reach` of them from the reach sweep: an oracle that compares
/// nothing proves nothing.
fn expect_answers(before: (usize, usize), shared: usize, reach: usize) {
    let after = shared_answer_counts();
    let (got_shared, got_reach) = (after.0 - before.0, after.1 - before.1);
    assert!(
        got_shared >= shared && got_reach >= reach,
        "the oracle compared {got_shared} shared answers ({got_reach} from the reach sweep), expected at least {shared} ({reach})"
    );
}

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
    let _guard = serial();
    let before = shared_answer_counts();
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
    expect_answers(before, 300, 80);
}

/// Fixed shapes for the sweep's wake-ups. The generator reaches them only by
/// chance, so each is pinned here: a unit that reads only through the
/// any-name (a quoted identifier `*` is the only way a read gets it) is offered while the taint is still empty, and a
/// later edit makes the taint non-empty; the unit must then fire, and its
/// any-name write must reach the last edit.
#[test]
fn sweep_wake_ups_agree_with_the_exact_scan() {
    std::env::set_var("SQLDIFF_EDIT_ORACLE", "1");
    let _guard = serial();
    let before = shared_answer_counts();
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
    expect_answers(before, 10, 6);
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
    let _guard = serial();
    let before = shared_answer_counts();
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
    expect_answers(before, 80, 50);
}

// ---- edits inside loops -------------------------------------------------------------------

const LOOPY_T: &[&str] = &[
    "SET @v1 = @p + {};",
    "SET @v2 = @v1 * {};",
    "SET @w1 = @w2 + {};",
    "SET @w2 = @w3 - {};",
    "SET @w3 = @v1 + {};",
    "INSERT INTO #t (a) VALUES (@w1 + {});",
    "UPDATE #t SET a = a + {} WHERE a > @v2;",
    "SELECT a FROM #t WHERE a = @w3 + {};",
    "EXEC(@sql);",
    "SELECT @w1 + {};",
];

const LOOPY_P: &[&str] = &[
    "v1 := p + {};",
    "v2 := v1 * {};",
    "w1 := w2 + {};",
    "w2 := w3 - {};",
    "w3 := v1 + {};",
    "INSERT INTO t1 (a) VALUES (w1 + {});",
    "UPDATE t1 SET a = a + {} WHERE a > v2;",
    "PERFORM a FROM t1 WHERE a = w3 + {};",
    "EXECUTE sql;",
    "PERFORM w1 + {};",
];

/// Routines that live inside loops: nested WHILE bodies, conditions, jumps
/// (BREAK / EXIT, RETURN) and reverse dependency chains.
fn gen_loopy(rng: &mut Rng, depth: u32, next_id: &mut usize, tsql: bool, n: u64) -> Vec<Node> {
    let leaves = if tsql { LOOPY_T } else { LOOPY_P };
    let mut out = Vec::new();
    for _ in 0..n {
        let id = *next_id;
        *next_id += 1;
        let pick = rng.below(10);
        let k = rng.below(5);
        let node = if depth < 3 && pick < 2 {
            let body_n = 1 + rng.below(4);
            let body = gen_loopy(rng, depth + 1, next_id, tsql, body_n);
            if tsql {
                Node {
                    head: "WHILE @v2 < {} BEGIN",
                    k,
                    body,
                    tail: "SET @v2 = @v2 + 1; END",
                    id,
                }
            } else {
                Node {
                    head: "WHILE v2 < {} LOOP",
                    k,
                    body,
                    tail: "v2 := v2 + 1; END LOOP;",
                    id,
                }
            }
        } else if depth < 3 && pick == 2 {
            let body_n = 1 + rng.below(3);
            let body = gen_loopy(rng, depth + 1, next_id, tsql, body_n);
            if tsql {
                Node {
                    head: "IF @w1 > {} BEGIN",
                    k,
                    body,
                    tail: "END",
                    id,
                }
            } else {
                Node {
                    head: "IF w1 > {} THEN",
                    k,
                    body,
                    tail: "END IF;",
                    id,
                }
            }
        } else if pick == 3 {
            Node {
                head: if tsql {
                    "IF @w2 = {} BREAK;"
                } else {
                    "IF w2 = {} THEN EXIT; END IF;"
                },
                k,
                body: vec![],
                tail: "",
                id,
            }
        } else if pick == 4 && depth > 0 {
            Node {
                head: if tsql {
                    "IF @v1 = {} RETURN;"
                } else {
                    "IF v1 = {} THEN RETURN; END IF;"
                },
                k,
                body: vec![],
                tail: "",
                id,
            }
        } else {
            Node {
                head: leaves[rng.below(leaves.len() as u64) as usize],
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

fn routine_loopy(nodes: &[Node], edited: &[bool], b: bool, tsql: bool, outer: bool) -> String {
    let mut body = String::new();
    render(nodes, edited, b, &mut body);
    // The whole body in one loop, or the loops only where generated.
    let (open, close) = match (outer, tsql) {
        (true, true) => ("WHILE @v2 < 100 BEGIN\n", "SET @v2 = @v2 + 1;\nEND\n"),
        (true, false) => ("WHILE v2 < 100 LOOP\n", "v2 := v2 + 1;\nEND LOOP;\n"),
        _ => ("", ""),
    };
    if tsql {
        format!(
            "CREATE PROCEDURE dbo.r @p int AS\nBEGIN\nDECLARE @v1 int = 0, @v2 int = 0, @w1 int = 0, @w2 int = 0, @w3 int = 0, @sql nvarchar(100) = N'';\nCREATE TABLE #t (a int);\n{open}{body}{close}SELECT a FROM #t;\nEND\n"
        )
    } else {
        format!(
            "CREATE FUNCTION r(p int) RETURNS void LANGUAGE plpgsql AS $$\nDECLARE v1 int := 0; v2 int := 0; w1 int := 0; w2 int := 0; w3 int := 0; sql text := '';\nBEGIN\n{open}{body}{close}END $$;\n"
        )
    }
}

/// Edits inside loops take the exact path (the shared sweeps skip them), now
/// a worklist; under the oracle every such answer is compared with the
/// original pass-loop scan.
#[test]
fn edits_in_loops_agree_with_the_pass_loop_reference() {
    std::env::set_var("SQLDIFF_EDIT_ORACLE", "1");
    let _guard = serial();
    let before = loop_answer_counts();
    let mut rng = Rng(0x100b5);
    let mut compared = 0usize;
    for case in 0..60 {
        let tsql = case % 2 == 0;
        let d = if tsql {
            Dialect::Tsql
        } else {
            Dialect::Postgres
        };
        let n = 4 + rng.below(16);
        let mut ids = 0;
        let nodes = gen_loopy(&mut rng, 0, &mut ids, tsql, n);
        let pct = 5 + rng.below(46);
        let edited: Vec<bool> = (0..ids).map(|_| rng.below(100) < pct).collect();
        let outer = case % 4 < 2;
        let (a, b) = (
            routine_loopy(&nodes, &edited, false, tsql, outer),
            routine_loopy(&nodes, &edited, true, tsql, outer),
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
    let (answers, visits) = {
        let after = loop_answer_counts();
        (after.0 - before.0, after.1 - before.1)
    };
    assert!(
        answers >= 300 && visits >= 1000,
        "the oracle compared {answers} loop answers ({visits} closure wake-ups), expected at least 300 (1000)"
    );
}

/// Fixed loop shapes for the worklist's wake-ups, which the generator reaches
/// only by chance. A unit that reads only through the any-name (a quoted
/// identifier `*`) must fire as soon as the set is non-empty, and a unit
/// that writes the any-name (a call) must wake every unit that reads
/// something. Each is the one thing a missed wake changes in the answer: a
/// later edit then reads `@q`, or the edit's result reaches the changed code
/// through `@q`.
#[test]
fn loop_wake_ups_agree_with_the_pass_loop_reference() {
    std::env::set_var("SQLDIFF_EDIT_ORACLE", "1");
    let _guard = serial();
    let before = loop_answer_counts();
    let d = Dialect::Tsql;
    let opts = Options::default();
    let any_reader = |k1: u32, k2: u32| {
        format!(
            "CREATE PROCEDURE dbo.r @p int, @q int, @lim int AS\nBEGIN\nDECLARE @v2 int = 0;\nWHILE @lim < 10 BEGIN\nSET @q = (SELECT @p AS [*]);\nSET @v2 = @p + {k1};\nSELECT @q + {k2};\nSET @lim = @lim + 1;\nEND\nEND\n"
        )
    };
    let any_writer = |k1: u32, k2: u32| {
        format!(
            "CREATE PROCEDURE dbo.r @p int, @q int AS\nBEGIN\nDECLARE @v2 int = 0;\nWHILE @v2 < 10 BEGIN\nEXEC dbo.f @p, {k1};\nSET @q = @p + 1;\nSELECT @q + {k2};\nSET @v2 = @v2 + 1;\nEND\nEND\n"
        )
    };
    for make in [&any_reader as &dyn Fn(u32, u32) -> String, &any_writer] {
        // Both edits, then each alone.
        for (a, b) in [((1, 1), (2, 2)), ((1, 1), (2, 1)), ((1, 1), (1, 2))] {
            for (x, y) in [(a, b), (b, a)] {
                let _ = diff(
                    &Artifact::new("a.sql", d, make(x.0, x.1).as_str()),
                    &Artifact::new("b.sql", d, make(y.0, y.1).as_str()),
                    &opts,
                );
            }
        }
    }
    let after = loop_answer_counts();
    assert!(
        after.0 - before.0 >= 6,
        "the oracle compared {} loop answers, expected at least 6",
        after.0 - before.0
    );
}

/// A header change whose parameter no statement mentions: the name still
/// makes the reached set non-empty, so the changed jump in the body (the
/// `RETURN`) is reached and the pair stays unknown. A missed wake of the
/// unknown name would call the header change different.
#[test]
fn an_unmentioned_changed_parameter_still_reaches_the_changed_jump() {
    std::env::set_var("SQLDIFF_EDIT_ORACLE", "1");
    let _guard = serial();
    let d = Dialect::Tsql;
    let a =
        "CREATE PROCEDURE dbo.r @p int, @q decimal(5,2) AS BEGIN IF @p > 1 RETURN; SELECT 1; END";
    let b =
        "CREATE PROCEDURE dbo.r @p int, @q decimal(7,2) AS BEGIN IF @p > 2 RETURN; SELECT 1; END";
    let r = diff(
        &Artifact::new("a.sql", d, a),
        &Artifact::new("b.sql", d, b),
        &Options::default(),
    );
    assert_eq!(
        r.verdict(),
        sqldiff_core::Verdict::Unknown,
        "{:#?}",
        r.findings
    );
}

/// Jump marks at the start of a sibling, not only inside one: a bare
/// RAISERROR / RAISE NOTICE statement between the generated ones (the same
/// lines on both sides, so they are not edits), at every nesting level, and
/// a bare RETURN at the end of some IF bodies. Under the oracle,
/// `earlier_siblings_with_marks` and `deciding_conditions` are compared with
/// the original walks (`*_reference` in `equiv::edit`) on every call.
#[test]
fn marks_at_sibling_starts_agree_with_the_original_walk() {
    std::env::set_var("SQLDIFF_EDIT_ORACLE", "1");
    let _guard = serial();
    let before = shared_answer_counts();
    let mut rng = Rng(0x3a7c);
    let mut compared = 0usize;
    for case in 0..40 {
        let tsql = case % 2 == 0;
        let d = if tsql {
            Dialect::Tsql
        } else {
            Dialect::Postgres
        };
        let n = 6 + rng.below(18);
        let mut ids = 0;
        let nodes = gen(&mut rng, 0, &mut ids, tsql, n);
        let pct = 5 + rng.below(41);
        let edited: Vec<bool> = (0..ids).map(|_| rng.below(100) < pct).collect();
        // Where the marks go: after the routine's line i when `at[i]`.
        let lines = routine(&nodes, &edited, false, tsql).lines().count();
        let at: Vec<u64> = (0..lines).map(|_| rng.below(4)).collect();
        let (raise, ret) = if tsql {
            ("RAISERROR('m', 0, 1);", "RETURN;")
        } else {
            ("RAISE NOTICE 'm';", "RETURN;")
        };
        let marked = |text: String| -> String {
            let mut out = String::new();
            let mut in_body = false;
            for (i, l) in text.lines().enumerate() {
                out.push_str(l);
                out.push('\n');
                if l.starts_with("CREATE TABLE #t") || l == "BEGIN" && !tsql {
                    in_body = true;
                    continue;
                }
                let opens = l.ends_with(" BEGIN") || l.ends_with(" THEN") || l.ends_with(" LOOP");
                if !in_body
                    || l.starts_with("END $$")
                    || l.ends_with("END") && tsql && i + 1 == text.lines().count()
                {
                    continue;
                }
                match at[i] {
                    0 => {
                        out.push_str(raise);
                        out.push('\n');
                    }
                    1 if opens && l.starts_with("IF") => {
                        out.push_str(raise);
                        out.push('\n');
                    }
                    _ => {}
                }
                // A bare RETURN as the last statement of an IF body.
                if at[i] == 3 && (l == "END" || l == "END IF;") && out.ends_with(&format!("{l}\n"))
                {
                    let cut = out.len() - l.len() - 1;
                    out.truncate(cut);
                    out.push_str(ret);
                    out.push('\n');
                    out.push_str(l);
                    out.push('\n');
                }
            }
            out
        };
        let (a, b) = (
            marked(routine(&nodes, &edited, false, tsql)),
            marked(routine(&nodes, &edited, true, tsql)),
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
    assert!(
        compared > 100,
        "the generated cases produce findings ({compared})"
    );
    expect_answers(before, 100, 10);
}
