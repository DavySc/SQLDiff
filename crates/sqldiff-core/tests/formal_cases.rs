//! Replays the scenarios of the TLA+ model (`formal/profiles/error-txn.toml`,
//! design §12 "Error/transaction model") against the IR: each case's program
//! is written out as SQL in its dialect, parsed, lowered and interpreted, and
//! the outcome is compared with the case's expectation. Both models are
//! thereby checked against the same table.
//!
//! The file belongs to the formal model; when it is not in the tree the test
//! passes without running anything. `SQLDIFF_FORMAL_CASES` points at another copy.

use std::collections::BTreeSet;

use sqldiff_core::artifact::Analyzed;
use sqldiff_core::ir::{self, ErrorKind, Exit, Fate, Fault, Scenario};
use sqldiff_core::model::Side;
use sqldiff_core::{Artifact, Dialect};

#[derive(Debug, Default, PartialEq, Eq)]
struct Got {
    durable: BTreeSet<String>,
    open: BTreeSet<String>,
    raised: bool,
    aborted: bool,
    code: i64,
    tc: Option<u32>,
    doomed: bool,
}

struct Writer {
    d: Dialect,
    out: Vec<String>,
    /// Program tag of each write, in order, with its rows count.
    tags: Vec<(String, bool)>,
}

impl Writer {
    fn table(tag: &str) -> String {
        format!("t_{tag}")
    }

    fn line(&mut self, depth: usize, s: &str) {
        self.out.push(format!("{}{s}", "  ".repeat(depth)));
    }

    fn block(&mut self, lines: &[String], i: &mut usize, depth: usize) {
        while *i < lines.len() {
            let l = lines[*i].trim().to_string();
            *i += 1;
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            if l.starts_with('}') {
                *i -= 1;
                return;
            }
            self.stmt(&l, lines, i, depth);
        }
    }

    /// Reads `{ ... }` bodies: returns the body text lines and the word after `}`.
    fn body(&mut self, lines: &[String], i: &mut usize, depth: usize) -> String {
        self.block(lines, i, depth);
        let close = lines
            .get(*i)
            .map(|x| x.trim().to_string())
            .unwrap_or_default();
        *i += 1;
        close
    }

    fn stmt(&mut self, l: &str, lines: &[String], i: &mut usize, depth: usize) {
        let w: Vec<&str> = l.split_whitespace().collect();
        let d = self.d;
        match w[0] {
            "write" => {
                let tag = w[1].to_string();
                let rows2 = w.contains(&"rows=2");
                let notatomic = w.contains(&"notatomic");
                let t = Self::table(&tag);
                self.tags.push((tag, rows2));
                let s = if notatomic {
                    format!("MERGE INTO {t} USING src s ON (1 = 1) WHEN MATCHED THEN UPDATE SET c = 1 NOT ATOMIC CONTINUE ON SQLEXCEPTION;")
                } else if rows2 {
                    format!("UPDATE {t} SET c = 1;")
                } else {
                    format!("INSERT INTO {t} (c) VALUES (1);")
                };
                self.line(depth, &s);
            }
            "begin" => self.line(
                depth,
                if d == Dialect::Tsql {
                    "BEGIN TRAN;"
                } else {
                    "BEGIN;"
                },
            ),
            "commit" => self.line(depth, "COMMIT;"),
            "rollback" => self.line(depth, "ROLLBACK;"),
            "savepoint" => self.line(
                depth,
                &match d {
                    Dialect::Tsql => format!("SAVE TRAN {};", w[1]),
                    Dialect::Db2i => format!("SAVEPOINT {} ON ROLLBACK RETAIN CURSORS;", w[1]),
                    Dialect::Postgres => format!("SAVEPOINT {};", w[1]),
                },
            ),
            "rollback-to" => self.line(
                depth,
                &match d {
                    Dialect::Tsql => format!("ROLLBACK TRAN {};", w[1]),
                    _ => format!("ROLLBACK TO SAVEPOINT {};", w[1]),
                },
            ),
            "throw" => self.line(depth, "THROW 50001, 'x', 1;"),
            "raiserror" => self.line(depth, "RAISERROR('x', 16, 1);"),
            "raise" => self.line(depth, "RAISE EXCEPTION 'x';"),
            "notice" => self.line(depth, "RAISE NOTICE 'x';"),
            "signal" => self.line(depth, "SIGNAL SQLSTATE '75001';"),
            "return" => self.line(depth, &format!("RETURN {};", w[1])),
            "on-error" | "on-notfound" => {
                let rollback = w.contains(&"rollback");
                let n = w.last().unwrap();
                let cond = match (w[0], d) {
                    ("on-error", Dialect::Tsql) => "@@ERROR <> 0",
                    ("on-error", Dialect::Db2i) => "SQLCODE < 0",
                    ("on-error", Dialect::Postgres) => "NOT FOUND",
                    (_, Dialect::Tsql) => "@@ROWCOUNT = 0",
                    (_, Dialect::Postgres) => "NOT FOUND",
                    (_, Dialect::Db2i) => "SQLCODE = 100",
                };
                if d == Dialect::Tsql {
                    if rollback {
                        self.line(depth, &format!("IF {cond} BEGIN ROLLBACK; RETURN {n}; END"));
                    } else {
                        self.line(depth, &format!("IF {cond} RETURN {n};"));
                    }
                } else {
                    let rb = if rollback { "ROLLBACK; " } else { "" };
                    self.line(depth, &format!("IF {cond} THEN {rb}RETURN {n}; END IF;"));
                }
            }
            "try" => {
                self.line(depth, "BEGIN TRY");
                let close = self.body(lines, i, depth + 1);
                self.line(depth, "END TRY");
                assert!(close.contains("catch"), "{close}");
                self.line(depth, "BEGIN CATCH");
                self.body(lines, i, depth + 1);
                self.line(depth, "END CATCH");
            }
            "begin-exception" => {
                self.line(depth, "BEGIN");
                let close = self.body(lines, i, depth + 1);
                assert!(close.contains("when-others"), "{close}");
                self.line(depth, "EXCEPTION WHEN OTHERS THEN");
                let n = self.out.len();
                self.body(lines, i, depth + 1);
                if self.out.len() == n {
                    self.line(depth + 1, "NULL;");
                }
                self.line(depth, "END;");
            }
            "compound" => {
                let atomic = w.contains(&"atomic");
                let ty = ["continue", "exit", "undo"]
                    .into_iter()
                    .find(|t| w.contains(t));
                self.line(depth, if atomic { "BEGIN ATOMIC" } else { "BEGIN" });
                let mark = self.out.len();
                let close = self.body(lines, i, depth + 1);
                let mut handler = Vec::new();
                if close.contains("handler") {
                    let before = self.out.len();
                    self.body(lines, i, depth + 2);
                    handler = self.out.split_off(before);
                }
                if let Some(ty) = ty {
                    let mut decl = vec![format!(
                        "{}DECLARE {} HANDLER FOR SQLEXCEPTION",
                        "  ".repeat(depth + 1),
                        ty.to_uppercase()
                    )];
                    decl.push(format!("{}BEGIN", "  ".repeat(depth + 1)));
                    decl.extend(handler);
                    decl.push(format!("{}END;", "  ".repeat(depth + 1)));
                    let rest = self.out.split_off(mark);
                    self.out.extend(decl);
                    self.out.extend(rest);
                }
                self.line(depth, "END;");
            }
            other => panic!("unknown program statement `{other}`"),
        }
    }
}

fn sql(d: Dialect, program: &str, commit: Option<&str>) -> (String, Vec<(String, bool)>) {
    let lines: Vec<String> = program
        .lines()
        .map(|l| l.split('#').next().unwrap().to_string())
        .collect();
    let mut w = Writer {
        d,
        out: Vec::new(),
        tags: Vec::new(),
    };
    let mut i = 0;
    w.block(&lines, &mut i, 1);
    let body = w.out.join("\n");
    let text = match d {
        Dialect::Tsql => format!("CREATE PROCEDURE p AS\nBEGIN\n{body}\nEND\n"),
        Dialect::Postgres => {
            format!("CREATE PROCEDURE p() LANGUAGE plpgsql AS $$\nBEGIN\n{body}\nEND;\n$$;\n")
        }
        Dialect::Db2i => format!(
            "CREATE PROCEDURE p ()\nLANGUAGE SQL\nSET OPTION COMMIT = *{}\nBEGIN\n{body}\nEND;\n",
            commit.unwrap_or("CHG")
        ),
    };
    (text, w.tags)
}

fn tag_of(key: &str, tags: &[(String, bool)]) -> String {
    if key == ir::interp::CALLER {
        return key.to_string();
    }
    let target = key
        .split(':')
        .nth(1)
        .unwrap_or("")
        .split('#')
        .next()
        .unwrap_or("");
    tags.iter()
        .map(|(t, _)| t.clone())
        .find(|t| format!("T{}", t.to_uppercase()) == target)
        .unwrap_or_else(|| key.to_string())
}

fn run_case(case: &toml::Value) -> Result<(), String> {
    let id = case["id"].as_str().unwrap();
    let d: Dialect = case["dialect"].as_str().unwrap().parse().unwrap();
    let get_bool = |k: &str| case.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let commit = case.get("commit").and_then(|v| v.as_str());
    let (text, tags) = sql(d, case["program"].as_str().unwrap(), commit);
    let art = Artifact::new(format!("{id}.sql"), d, text.clone());
    let a = Analyzed::new(&art, Side::A, 3);
    if !a.parse.diagnostics.is_empty() {
        return Err(format!(
            "{id}: parse problems {:?}\n{text}",
            a.parse.diagnostics
        ));
    }
    let cfg = ir::lower(&a);
    let mut faults = Vec::new();
    if let Some(t) = case.get("faults").and_then(|v| v.as_table()) {
        for (tag, how) in t {
            let kind = match how.as_str().unwrap() {
                "stmt" => ErrorKind::Constraint,
                "batch" => ErrorKind::Lock,
                "uncatchable" => ErrorKind::Cancel,
                "notfound" => ErrorKind::NotFound,
                x => return Err(format!("{id}: unknown fault {x}")),
            };
            let node = cfg
                .effect_nodes()
                .into_iter()
                .find(|(_, e)| tag_of(&e.key, &tags) == *tag)
                .map(|(n, _)| n)
                .ok_or_else(|| format!("{id}: no statement for tag {tag}\n{}", cfg.dump()))?;
            faults.push(Fault { node, kind });
        }
    }
    let sc = Scenario {
        caller_txn: get_bool("caller_txn"),
        xact_abort: get_bool("xact_abort"),
        implicit_transactions: get_bool("implicit_transactions"),
        commit_level: None,
        faults,
    };
    let outs = ir::explore(&cfg, &sc);
    if outs.len() != 1 {
        return Err(format!(
            "{id}: {} paths\n{text}\n{}",
            outs.len(),
            cfg.dump()
        ));
    }
    let o = &outs[0];
    let name = |k: &str| {
        let t = tag_of(k, &tags);
        if o.partial.contains(k) {
            format!("{t}.1")
        } else {
            t
        }
    };
    let got = Got {
        durable: o
            .effects
            .iter()
            .filter(|(k, f)| matches!(f, Fate::Durable | Fate::Partial) && !o.open.contains(*k))
            .map(|(k, _)| name(k))
            .collect(),
        open: o.open.iter().map(|k| name(k)).collect(),
        raised: o.raised,
        aborted: matches!(o.exit, Exit::Raised(_)),
        code: match &o.exit {
            Exit::Code(c) => c.parse().unwrap_or(-999),
            _ => 0,
        },
        tc: (d == Dialect::Tsql).then_some(o.depth),
        doomed: o.doomed,
    };
    let e = &case["expect"];
    let set = |k: &str| -> BTreeSet<String> {
        e.get(k)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().map(|x| x.as_str().unwrap().to_string()).collect())
            .unwrap_or_default()
    };
    let entry_tc = u32::from(get_bool("caller_txn"));
    let want = Got {
        durable: set("durable"),
        open: set("open"),
        raised: e.get("raised").and_then(|v| v.as_bool()).unwrap_or(false),
        aborted: e.get("aborted").and_then(|v| v.as_bool()).unwrap_or(false),
        code: e.get("code").and_then(|v| v.as_integer()).unwrap_or(0),
        tc: (d == Dialect::Tsql).then(|| {
            e.get("tc")
                .and_then(|v| v.as_integer())
                .map(|x| x as u32)
                .unwrap_or(entry_tc)
        }),
        doomed: e.get("doomed").and_then(|v| v.as_bool()).unwrap_or(false),
    };
    if got != want {
        return Err(format!(
            "{id}:\n  want {want:?}\n  got  {got:?}\n  rows {:?}\n{text}{}",
            o.rows,
            cfg.dump()
        ));
    }
    Ok(())
}

#[test]
fn formal_model_cases_replay_on_the_ir() {
    let path = std::env::var("SQLDIFF_FORMAL_CASES").unwrap_or_else(|_| {
        format!(
            "{}/../../formal/profiles/error-txn.toml",
            env!("CARGO_MANIFEST_DIR")
        )
    });
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("{path} not found; skipping the formal-model replay");
        return;
    };
    let v: toml::Value = toml::from_str(&text).unwrap();
    let cases = v["case"].as_array().unwrap();
    let failures: Vec<String> = cases.iter().filter_map(|c| run_case(c).err()).collect();
    assert!(
        failures.is_empty(),
        "{} of {} cases differ:\n\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n\n")
    );
}
