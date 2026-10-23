//! Grammar-aware parser fuzzing (design §12: "error nodes bounded", §14 M2).
//!
//! [`program`] generates well-formed routines and scripts per dialect from a
//! small grammar driven by a [`Source`] of choices, so the same generator
//! serves seeded tests (`Lcg`) and coverage-guided fuzzing (`Bytes`, used by
//! the targets in `fuzz/`). [`measure`] parses a text and counts statements
//! and error nodes; the M2 gate is error nodes < 1% of statements.

use std::collections::BTreeSet;

use sqldiff_syntax::{parse, Dialect, SyntaxKind, SyntaxNode};

/// A source of choices.
pub trait Source {
    /// A number in `0..n` (`n > 0`).
    fn below(&mut self, n: usize) -> usize;
}

/// Seeded linear congruential generator.
pub struct Lcg(pub u64);

impl Source for Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

/// Choices read from fuzzer bytes; zero once exhausted, so generation ends.
pub struct Bytes<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bytes<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Bytes { data, pos: 0 }
    }
}

impl Source for Bytes<'_> {
    fn below(&mut self, n: usize) -> usize {
        let b = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b as usize % n
    }
}

/// Parse statistics of one text.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stats {
    /// Statement units (simple and compound) in the tree.
    pub units: usize,
    /// `ERROR` nodes.
    pub errors: usize,
    pub diagnostics: usize,
    /// The tree prints back to the input.
    pub round_trips: bool,
    /// Node kinds present in the tree; lets tests check that generated
    /// compounds were recognised as compounds, not just absorbed without errors.
    pub kinds: BTreeSet<SyntaxKind>,
}

impl std::ops::AddAssign for Stats {
    fn add_assign(&mut self, o: Stats) {
        self.units += o.units;
        self.errors += o.errors;
        self.diagnostics += o.diagnostics;
        self.round_trips &= o.round_trips;
        self.kinds.extend(o.kinds);
    }
}

pub fn measure(text: &str, dialect: Dialect) -> Stats {
    let p = parse(text, dialect);
    let root = p.syntax();
    let mut s = Stats {
        diagnostics: p.diagnostics.len(),
        round_trips: root.text() == text,
        ..Stats::default()
    };
    count(&root, &mut s);
    s
}

fn count(node: &SyntaxNode, s: &mut Stats) {
    let k = node.kind();
    s.kinds.insert(k);
    if k == SyntaxKind::ERROR {
        s.errors += 1;
    }
    if k.is_unit() && k != SyntaxKind::ERROR {
        s.units += 1;
    }
    for c in node.children() {
        count(&c, s);
    }
}

/// Generate one well-formed artifact in `dialect`.
pub fn program(src: &mut dyn Source, dialect: Dialect) -> String {
    let mut g = Gen {
        src,
        d: dialect,
        out: String::new(),
        indent: 0,
        labels: 0,
        top: false,
    };
    let n = 1 + g.pick(3);
    for _ in 0..n {
        g.top_level();
    }
    g.out
}

const TABLES: &[(&str, &[&str])] = &[
    ("stock", &["item_id", "qty"]),
    ("item", &["item_id", "name", "category"]),
    ("orders", &["order_id", "customer_id", "status", "total"]),
    ("item_price", &["item_id", "unit_price", "updated_at"]),
];

struct Gen<'a> {
    src: &'a mut dyn Source,
    d: Dialect,
    out: String,
    indent: usize,
    labels: usize,
    /// Outside a routine: no parameters or variables in scope.
    top: bool,
}

impl Gen<'_> {
    fn pick(&mut self, n: usize) -> usize {
        self.src.below(n)
    }

    fn one<'s>(&mut self, xs: &[&'s str]) -> &'s str {
        xs[self.pick(xs.len())]
    }

    fn line(&mut self, s: &str) {
        // T-SQL: THROW must follow a terminated statement, and END of a block
        // carries no terminator (`END; ELSE` would be invalid).
        let s = &if self.d == Dialect::Tsql
            && s.starts_with("THROW")
            && matches!(
                self.out.lines().last().map(str::trim),
                Some("END" | "END TRY" | "END CATCH")
            ) {
            format!(";{s}")
        } else {
            s.to_string()
        };
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn var(&mut self) -> String {
        let v = self.one(&["v_a", "v_b", "v_total"]);
        match self.d {
            Dialect::Tsql => format!("@{v}"),
            _ => v.to_string(),
        }
    }

    fn param(&mut self) -> String {
        let p = self.one(&["p_item", "p_qty"]);
        match self.d {
            Dialect::Tsql => format!("@{p}"),
            _ => p.to_string(),
        }
    }

    fn table(&mut self) -> (&'static str, &'static [&'static str]) {
        TABLES[self.pick(TABLES.len())]
    }

    fn qualified(&mut self, t: &str) -> String {
        let r = self.pick(6);
        match (self.d, r) {
            (Dialect::Tsql, 0..=2) => format!("dbo.{t}"),
            (Dialect::Tsql, 3) => format!("[{t}]"),
            (Dialect::Postgres, 0) => format!("public.{t}"),
            _ => t.to_string(),
        }
    }

    fn expr(&mut self, cols: &[&str], depth: usize) -> String {
        let leaf = depth == 0 || self.pick(3) == 0;
        if leaf {
            return match self.pick(6) {
                0 => cols[self.pick(cols.len())].to_string(),
                1 => self.pick(1000).to_string(),
                2 => format!("'{}'", self.one(&["A", "N", "x y", "it''s"])),
                3 if !self.top => self.param(),
                4 if !self.top => self.var(),
                3 | 4 => self.pick(100).to_string(),
                _ => "NULL".to_string(),
            };
        }
        let d = depth - 1;
        match self.pick(9) {
            0 => {
                // Comparisons are not values in T-SQL or SQL PL; they live in `cond`.
                let op = self.one(&["+", "-", "*"]);
                format!("{} {op} {}", self.expr(cols, d), self.expr(cols, d))
            }
            1 => format!("({})", self.expr(cols, d)),
            2 => {
                let f = match self.d {
                    Dialect::Tsql => self.one(&["COALESCE", "ISNULL", "UPPER", "LEN"]),
                    Dialect::Postgres => self.one(&["coalesce", "upper", "length", "greatest"]),
                    Dialect::Db2i => self.one(&["COALESCE", "UPPER", "LENGTH", "VARCHAR"]),
                };
                let n = if matches!(
                    f,
                    "UPPER" | "upper" | "LEN" | "length" | "LENGTH" | "VARCHAR"
                ) {
                    1
                } else {
                    2
                };
                let args: Vec<String> = (0..n).map(|_| self.expr(cols, d)).collect();
                format!("{f}({})", args.join(", "))
            }
            3 => format!(
                "CASE WHEN {} THEN {} ELSE {} END",
                self.cond(cols, d),
                self.expr(cols, d),
                self.expr(cols, d)
            ),
            4 => {
                let ty = match self.d {
                    Dialect::Tsql => self.one(&["int", "varchar(20)", "decimal(12,2)"]),
                    Dialect::Postgres => self.one(&["integer", "text", "numeric(12,2)"]),
                    Dialect::Db2i => self.one(&["INTEGER", "VARCHAR(20)", "DECIMAL(12,2)"]),
                };
                if self.d == Dialect::Postgres && self.pick(2) == 0 {
                    format!("{}::{ty}", self.expr(cols, 0))
                } else {
                    format!("CAST({} AS {ty})", self.expr(cols, d))
                }
            }
            5 => {
                let (t, tc) = self.table();
                let t = self.qualified(t);
                format!(
                    "(SELECT MAX({}) FROM {t} WHERE {})",
                    tc[self.pick(tc.len())],
                    self.cond(tc, 0)
                )
            }
            6 => format!("-{}", self.expr(cols, 0)),
            7 => match self.d {
                Dialect::Tsql => "SYSDATETIME()".to_string(),
                Dialect::Postgres => self
                    .one(&["now()", "statement_timestamp()", "CURRENT_TIMESTAMP"])
                    .to_string(),
                Dialect::Db2i => "CURRENT TIMESTAMP".to_string(),
            },
            _ => self.expr(cols, d),
        }
    }

    fn cond(&mut self, cols: &[&str], depth: usize) -> String {
        let c = cols[self.pick(cols.len())];
        match self.pick(if depth == 0 { 4 } else { 7 }) {
            0 => format!("{c} = {}", self.expr(cols, depth)),
            1 => format!("{c} IS {}NULL", self.one(&["", "NOT "])),
            2 => format!("{c} IN ({}, {})", self.pick(9), self.pick(9)),
            3 => format!("{c} BETWEEN 1 AND {}", 2 + self.pick(50)),
            4 => format!(
                "{} {} {}",
                self.cond(cols, depth - 1),
                self.one(&["AND", "OR"]),
                self.cond(cols, depth - 1)
            ),
            5 => format!("NOT ({})", self.cond(cols, depth - 1)),
            _ => {
                let (t, tc) = self.table();
                let t = self.qualified(t);
                format!(
                    "{}EXISTS (SELECT 1 FROM {t} x WHERE x.{} = {})",
                    self.one(&["", "NOT "]),
                    tc[0],
                    self.expr(cols, 0)
                )
            }
        }
    }

    fn select(&mut self) -> String {
        let (t, cols) = self.table();
        let tq = self.qualified(t);
        let n = 1 + self.pick(3);
        let list: Vec<String> = (0..n)
            .map(|i| {
                let e = self.expr(cols, 2);
                if self.pick(2) == 0 {
                    format!("{e} AS c{i}")
                } else {
                    e
                }
            })
            .collect();
        let mut s = format!("SELECT {} FROM {tq} t", list.join(", "));
        if self.pick(3) == 0 {
            let (u, ucols) = self.table();
            let uq = self.qualified(u);
            s.push_str(&format!(
                " {}JOIN {uq} u ON u.{} = t.{}",
                self.one(&["", "LEFT "]),
                ucols[0],
                cols[0]
            ));
        }
        if self.pick(2) == 0 {
            s.push_str(&format!(" WHERE {}", self.cond(cols, 2)));
        }
        if self.pick(3) == 0 {
            s.push_str(&format!(
                " ORDER BY {}{}",
                cols[self.pick(cols.len())],
                self.one(&["", " DESC", " ASC"])
            ));
            if self.d == Dialect::Postgres && self.pick(2) == 0 {
                s.push_str(self.one(&[" NULLS FIRST", " NULLS LAST"]));
            }
        }
        s
    }

    fn dml(&mut self) -> String {
        let (t, cols) = self.table();
        let tq = self.qualified(t);
        match self.pick(4) {
            0 => self.select(),
            1 => {
                let n = 1 + self.pick(cols.len());
                let vals: Vec<String> = (0..n).map(|_| self.expr(cols, 1)).collect();
                format!(
                    "INSERT INTO {tq} ({}) VALUES ({})",
                    cols[..n].join(", "),
                    vals.join(", ")
                )
            }
            2 => {
                let c = cols[1 + self.pick(cols.len() - 1)];
                let mut s = format!("UPDATE {tq} SET {c} = {}", self.expr(cols, 2));
                if self.pick(4) != 0 {
                    s.push_str(&format!(" WHERE {}", self.cond(cols, 2)));
                }
                s
            }
            _ => format!("DELETE FROM {tq} WHERE {}", self.cond(cols, 2)),
        }
    }

    fn top_level(&mut self) {
        match self.pick(4) {
            0 => {
                self.top = true;
                let s = self.dml();
                self.top = false;
                self.line(&format!("{s};"));
                // CREATE PROCEDURE must start its batch.
                if self.d == Dialect::Tsql {
                    self.line("GO");
                }
            }
            _ => self.routine(),
        }
    }

    fn routine(&mut self) {
        let name = self.one(&["do_work", "move_it", "apply_feed"]);
        match self.d {
            Dialect::Tsql => {
                self.line(&format!(
                    "CREATE PROCEDURE dbo.{name} @p_item int, @p_qty int AS"
                ));
                self.line("BEGIN");
                self.indent += 1;
                self.line("SET NOCOUNT ON;");
                self.line("DECLARE @v_a int, @v_b varchar(20), @v_total decimal(12,2);");
                self.body(3);
                self.indent -= 1;
                self.line("END");
                self.line("GO");
            }
            Dialect::Postgres => {
                let func = self.pick(2) == 0;
                if func {
                    self.line(&format!(
                        "CREATE OR REPLACE FUNCTION {name}(p_item integer, p_qty integer) RETURNS integer"
                    ));
                } else {
                    self.line(&format!(
                        "CREATE OR REPLACE PROCEDURE {name}(p_item integer, p_qty integer)"
                    ));
                }
                self.line("LANGUAGE plpgsql");
                self.line("AS $$");
                self.line("DECLARE");
                self.line("  v_a integer := 0;");
                self.line("  v_b text;");
                self.line("  v_total numeric(12,2);");
                self.line("BEGIN");
                self.indent += 1;
                self.body(3);
                if func {
                    self.line("RETURN v_a;");
                }
                self.indent -= 1;
                self.line("END;");
                self.line("$$;");
            }
            Dialect::Db2i => {
                self.line(&format!(
                    "CREATE PROCEDURE {name} (IN p_item INTEGER, IN p_qty INTEGER)"
                ));
                self.line("LANGUAGE SQL");
                if self.pick(2) == 0 {
                    self.line("MODIFIES SQL DATA");
                }
                if self.pick(2) == 0 {
                    let lvl = self.one(&["*NONE", "*CHG", "*CS"]);
                    self.line(&format!("SET OPTION COMMIT = {lvl}"));
                }
                let atomic = self.one(&["", " ATOMIC", " NOT ATOMIC"]);
                self.line(&format!("BEGIN{atomic}"));
                self.indent += 1;
                self.line("DECLARE v_a INTEGER DEFAULT 0;");
                self.line("DECLARE v_b VARCHAR(20);");
                self.line("DECLARE v_total DECIMAL(12,2);");
                if self.pick(2) == 0 {
                    let kind = self.one(&["CONTINUE", "EXIT"]);
                    self.line(&format!("DECLARE {kind} HANDLER FOR SQLEXCEPTION"));
                    self.line("BEGIN");
                    self.indent += 1;
                    self.line("SET v_a = -1;");
                    self.indent -= 1;
                    self.line("END;");
                }
                self.body(3);
                self.indent -= 1;
                self.line("END;");
            }
        }
    }

    fn body(&mut self, depth: usize) {
        let n = 1 + self.pick(4);
        for _ in 0..n {
            self.stmt(depth);
        }
    }

    fn stmt(&mut self, depth: usize) {
        let compound = depth > 0 && self.pick(3) == 0;
        if !compound {
            return self.simple();
        }
        let d = depth - 1;
        match (self.d, self.pick(4)) {
            (Dialect::Tsql, 0) => {
                let c = self.cond(&["@v_a", "@p_qty"], 1);
                self.line(&format!("IF {c}"));
                self.block_tsql(d);
                if self.pick(2) == 0 {
                    self.line("ELSE");
                    self.block_tsql(d);
                }
            }
            (Dialect::Tsql, 1) => {
                let c = self.cond(&["@v_a"], 0);
                self.line(&format!("WHILE {c}"));
                self.block_tsql(d);
            }
            (Dialect::Tsql, _) => {
                self.line("BEGIN TRY");
                self.indent += 1;
                self.body(d);
                self.indent -= 1;
                self.line("END TRY");
                self.line("BEGIN CATCH");
                self.indent += 1;
                self.line("IF @@TRANCOUNT > 0 ROLLBACK;");
                if self.pick(2) == 0 {
                    self.line("THROW;");
                } else {
                    self.body(d);
                }
                self.indent -= 1;
                self.line("END CATCH");
            }
            (Dialect::Postgres, 0) => {
                let c = self.cond(&["v_a", "p_qty"], 1);
                // PL/pgSQL ends the condition at the first THEN outside
                // parentheses, so a CASE in it must be wrapped.
                self.line(&format!("IF ({c}) THEN"));
                self.nested(d);
                if self.pick(2) == 0 {
                    let c = self.cond(&["v_a"], 0);
                    self.line(&format!("ELSIF ({c}) THEN"));
                    self.nested(d);
                }
                if self.pick(2) == 0 {
                    self.line("ELSE");
                    self.nested(d);
                }
                self.line("END IF;");
            }
            (Dialect::Postgres, 1) => {
                let c = self.cond(&["v_a"], 0);
                self.line(&format!("WHILE {c} LOOP"));
                self.nested(d);
                self.line("END LOOP;");
            }
            (Dialect::Postgres, _) => {
                self.line("BEGIN");
                self.nested(d);
                self.line("EXCEPTION");
                let cond = self.one(&["OTHERS", "unique_violation", "no_data_found"]);
                self.line(&format!("  WHEN {cond} THEN"));
                self.indent += 1;
                self.nested(d);
                self.indent -= 1;
                self.line("END;");
            }
            (Dialect::Db2i, 0) => {
                let c = self.cond(&["v_a", "p_qty"], 1);
                self.line(&format!("IF {c} THEN"));
                self.nested(d);
                if self.pick(2) == 0 {
                    let c = self.cond(&["v_a"], 0);
                    self.line(&format!("ELSEIF {c} THEN"));
                    self.nested(d);
                }
                if self.pick(2) == 0 {
                    self.line("ELSE");
                    self.nested(d);
                }
                self.line("END IF;");
            }
            (Dialect::Db2i, 1) => {
                let c = self.cond(&["v_a"], 0);
                if self.pick(2) == 0 {
                    self.line(&format!("WHILE {c} DO"));
                    self.nested(d);
                    self.line("END WHILE;");
                } else {
                    self.labels += 1;
                    let l = format!("lbl{}", self.labels);
                    self.line(&format!("{l}: LOOP"));
                    self.nested(d);
                    self.line(&format!("  LEAVE {l};"));
                    self.line(&format!("END LOOP {l};"));
                }
            }
            (Dialect::Db2i, _) => {
                self.line("BEGIN");
                self.indent += 1;
                self.line("DECLARE CONTINUE HANDLER FOR SQLSTATE '23505' SET v_a = 2;");
                self.body(d);
                self.indent -= 1;
                self.line("END;");
            }
        }
    }

    fn block_tsql(&mut self, depth: usize) {
        self.line("BEGIN");
        self.nested(depth);
        self.line("END");
    }

    fn nested(&mut self, depth: usize) {
        self.indent += 1;
        self.body(depth);
        self.indent -= 1;
    }

    /// DML inside a routine body. PL/pgSQL and SQL PL have no bare SELECT:
    /// PL/pgSQL discards rows with PERFORM, SQL PL needs SELECT ... INTO.
    fn body_dml(&mut self) -> String {
        if self.d == Dialect::Tsql || self.pick(4) != 0 {
            let s = self.dml();
            if !s.starts_with("SELECT ") {
                return s;
            }
        }
        match self.d {
            Dialect::Tsql => self.select(),
            Dialect::Postgres => format!("PERFORM {}", &self.select()["SELECT ".len()..]),
            Dialect::Db2i => {
                let (t, cols) = self.table();
                let t = self.qualified(t);
                let c = self.cond(cols, 2);
                format!("SELECT COUNT(*) INTO v_a FROM {t} WHERE {c}")
            }
        }
    }

    fn simple(&mut self) {
        let v = self.var();
        let s = match (self.d, self.pick(8)) {
            (_, 0..=2) => self.body_dml(),
            (Dialect::Tsql, 3) => format!("SET {v} = {}", self.expr(&["@p_qty"], 2)),
            (Dialect::Tsql, 4) => format!("SELECT {v} = qty FROM dbo.stock WHERE item_id = @p_item"),
            (Dialect::Tsql, 5) => self
                .one(&["BEGIN TRAN", "COMMIT", "ROLLBACK", "SAVE TRAN sp1"])
                .to_string(),
            (Dialect::Tsql, 6) => {
                self.one(&["THROW 50001, 'failed', 1", "RETURN 1", "PRINT 'x'"]).to_string()
            }
            (Dialect::Tsql, _) => "EXEC dbo.log_stock_change @p_item, @p_qty, 'fuzz'".to_string(),
            (Dialect::Postgres, 3) => format!("{v} := {}", self.expr(&["p_qty"], 2)),
            (Dialect::Postgres, 4) => format!(
                "SELECT qty INTO {}{v} FROM stock WHERE item_id = p_item",
                self.one(&["", "STRICT "])
            ),
            (Dialect::Postgres, 5) => {
                format!("PERFORM pg_advisory_xact_lock({})", self.pick(100))
            }
            (Dialect::Postgres, 6) => self
                .one(&[
                    "RAISE EXCEPTION 'failed'",
                    "RAISE NOTICE 'n %', v_a",
                    "GET DIAGNOSTICS v_a = ROW_COUNT",
                ])
                .to_string(),
            (Dialect::Postgres, _) => {
                "INSERT INTO stock_log (item_id, delta) VALUES (p_item, p_qty) ON CONFLICT DO NOTHING"
                    .to_string()
            }
            (Dialect::Db2i, 3) => format!("SET {v} = {}", self.expr(&["p_qty"], 2)),
            (Dialect::Db2i, 4) => {
                format!("SELECT qty INTO {v} FROM stock WHERE item_id = p_item")
            }
            (Dialect::Db2i, 5) => self.one(&["COMMIT", "ROLLBACK"]).to_string(),
            (Dialect::Db2i, 6) => {
                "SIGNAL SQLSTATE '75001' SET MESSAGE_TEXT = 'failed'".to_string()
            }
            (Dialect::Db2i, _) => "CALL log_stock_change(p_item, p_qty, 'fuzz')".to_string(),
        };
        self.line(&format!("{s};"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let a = program(&mut Lcg(7), Dialect::Postgres);
        let b = program(&mut Lcg(7), Dialect::Postgres);
        assert_eq!(a, b);
    }

    #[test]
    fn bytes_source_terminates() {
        for d in Dialect::ALL {
            let _ = program(&mut Bytes::new(&[]), d);
            let _ = program(&mut Bytes::new(&[255; 64]), d);
        }
    }
}
