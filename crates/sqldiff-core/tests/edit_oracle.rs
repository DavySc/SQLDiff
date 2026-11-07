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
