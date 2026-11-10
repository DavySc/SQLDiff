//! The temporary tables the T-SQL table variables of a whole tree become,
//! by name (`classify::table_variable_table`), with the routine that first
//! declared one. Every routine that can be active in one session creates
//! its tables in the one temporary schema, dropping any of the same name
//! first, so two routines of the tree must never map a table variable to
//! the same name: the converter refuses the later one (`Claims::clash`).

use std::collections::BTreeMap;

use sqldiff_equiv::catalog::record::{Outcome, Query, ReadSink, Side, NO_SINK};
use sqldiff_syntax::{ast, Dialect, SyntaxKind, SyntaxNode};

/// The first table variable of the tree that maps to a temporary table name.
#[derive(Debug, Clone)]
pub struct Claim {
    /// The file it is declared in, as the tree names it; empty for a text
    /// converted on its own.
    pub file: String,
    /// The routine it is declared in, as its header writes it (`dbo.p`);
    /// none in a script.
    pub routine: Option<String>,
    /// The variable, as written (`@t`).
    pub var: String,
}

/// Temporary table names (lower case, without the schema) claimed by the
/// tree's table variables, first in tree order (file order, then position
/// in the file) wins.
#[derive(Debug, Default)]
pub struct Claims {
    by_table: BTreeMap<String, Claim>,
}

impl Claims {
    /// The claims of the T-SQL files `files` (relative path and text), in
    /// that order.
    pub fn of<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>, from: Dialect) -> Claims {
        let mut c = Claims::default();
        if from == Dialect::Tsql {
            for (rel, text) in files {
                c.add_root(&sqldiff_syntax::parse(text, from).syntax(), rel);
            }
        }
        c
    }

    /// Adds the claims of one parsed T-SQL file.
    pub fn add_root(&mut self, root: &SyntaxNode, file: &str) {
        for n in root.descendants() {
            if n.kind() != SyntaxKind::STATEMENT || !table_variable_declare(&n) {
                continue;
            }
            let var = ast::sig_tokens(&n)
                .nth(1)
                .map(|t| t.text().to_string())
                .unwrap_or_default();
            let routine = n
                .ancestors()
                .find(|a| a.kind() == SyntaxKind::ROUTINE)
                .and_then(|r| ast::routine_name(&r));
            let table = sqldiff_equiv::classify::table_variable_table(routine.as_deref(), &var)
                .to_lowercase();
            self.by_table.entry(table).or_insert(Claim {
                file: file.to_string(),
                routine,
                var,
            });
        }
    }

    /// The claim on `table` (lower case, without the schema) by a routine
    /// other than `routine`, if any: the table variable `routine` would map
    /// there clashes with it. Two definitions of one routine do not clash.
    pub fn clash(&self, table: &str, routine: Option<&str>) -> Option<&Claim> {
        self.by_table
            .get(&table.to_lowercase())
            .filter(|c| !same_routine(c.routine.as_deref(), routine))
    }
}

/// The claims as pass 1 reads them: the one question
/// ([`Claims::clash`]) goes to a sink as a [`Query::ClaimClash`] of the
/// draft side with the digest of its answer (the whole claim, or none), so
/// a stored draft can be checked against the claims of another run.
#[derive(Clone, Copy)]
pub struct ClaimReads<'a> {
    claims: &'a Claims,
    sink: &'a dyn ReadSink,
}

impl<'a> ClaimReads<'a> {
    /// The claims, recording nothing.
    pub fn new(claims: &'a Claims) -> Self {
        ClaimReads {
            claims,
            sink: &NO_SINK,
        }
    }

    /// The claims, recording every question to `sink`.
    pub fn recording(claims: &'a Claims, sink: &'a dyn ReadSink) -> Self {
        ClaimReads { claims, sink }
    }

    /// [`Claims::clash`], recorded.
    #[track_caller]
    pub fn clash(&self, table: &str, routine: Option<&str>) -> Option<&'a Claim> {
        let a = self.claims.clash(table, routine);
        if self.sink.active() && !sqldiff_equiv::catalog::record::site_skipped() {
            let q = clash_query(table, routine);
            if !self.sink.holds(Side::Draft, &q) {
                self.sink.record(Side::Draft, &q, Outcome::of(&a));
            }
        }
        a
    }
}

/// The question [`ClaimReads::clash`] records: names in lower case, as
/// `Claims::clash` compares them.
fn clash_query(table: &str, routine: Option<&str>) -> Query {
    Query::ClaimClash(
        table.to_lowercase(),
        routine.map(|r| r.to_ascii_lowercase()),
    )
}

/// Asks a recorded [`Query::ClaimClash`] of `claims` again, with the digest
/// of the answer; `None` for any other query.
pub fn replay_clash(claims: &Claims, q: &Query) -> Option<Outcome> {
    match q {
        Query::ClaimClash(table, routine) => {
            Some(Outcome::of(&claims.clash(table, routine.as_deref())))
        }
        _ => None,
    }
}

fn same_routine(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

/// `DECLARE @t [AS] TABLE (…)`, as the rules recognise it
/// (`tsql::Cx::table_variable`).
fn table_variable_declare(n: &SyntaxNode) -> bool {
    let w = ast::first_words(n, 4);
    let w = |i: usize| w.get(i).map(String::as_str).unwrap_or("");
    w(0) == "DECLARE"
        && w(1).starts_with('@')
        && (w(2) == "TABLE" || (w(2) == "AS" && w(3) == "TABLE"))
}

impl std::fmt::Display for Claim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} of ", self.var)?;
        match &self.routine {
            Some(r) => write!(f, "routine {r}")?,
            None => write!(f, "the script")?,
        }
        if !self.file.is_empty() {
            write!(f, " ({})", self.file)?;
        }
        Ok(())
    }
}
