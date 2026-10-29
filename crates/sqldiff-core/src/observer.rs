//! The observer profile (decision 1 in docs/verification-roadmap.md): side
//! channels that only some callers read. Rows-affected counts (what T-SQL's
//! `SET NOCOUNT` controls, and `ExecuteNonQuery` returns) are observed within
//! one dialect and not across dialects unless a profile says otherwise; an
//! unobserved channel that could differ is listed as an assumption.

use serde::{Deserialize, Serialize};
use sqldiff_syntax::{ast, Dialect};

use crate::artifact::Analyzed;
use crate::classify::{stmt_kind, StmtKind};
use crate::model::Assumption;

/// When rows-affected counts are observed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RowCounts {
    /// Within one dialect, not across dialects (PostgreSQL and Db2 for i
    /// procedures send none, so across dialects they could never be equal).
    #[default]
    SameDialect,
    Always,
    Never,
}

/// The observer profile of a run (`[observer]` in `sqldiff.toml` or a pack).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observer {
    #[serde(default)]
    pub row_counts: RowCounts,
}

pub fn rowcount_assumption() -> Assumption {
    Assumption::default(
        "ASM-OBSERVER-ROWCOUNT",
        "The caller does not read rows-affected counts (T-SQL sends one per statement unless SET NOCOUNT ON; PostgreSQL and Db2 for i procedures send none)",
    )
}

/// Whether an artifact can send a rows-affected count for a statement that
/// returns or changes rows. T-SQL sends one unless `SET NOCOUNT ON` is
/// certainly in force by then, in document order (a setting inside a
/// conditional block, loop or handler may not run, so it does not count).
/// PostgreSQL and Db2 for i send one for a statement run on its own (the
/// command tag, the driver's update count) and none inside a routine or an
/// anonymous block.
pub fn sends_counts(a: &Analyzed) -> bool {
    let d = a.dialect();
    let rows = |k: StmtKind| k.is_write() || k == StmtKind::Select;
    if d != Dialect::Tsql {
        return a
            .root
            .descendants()
            .filter(|n| n.kind().is_unit())
            .any(|n| {
                rows(stmt_kind(&n, d))
                    && !n
                        .ancestors()
                        .skip(1)
                        .any(|p| matches!(stmt_kind(&p, d), StmtKind::Routine | StmtKind::Block))
            });
    }
    let conditional = |n: &sqldiff_syntax::SyntaxNode| {
        n.ancestors().skip(1).any(|p| {
            matches!(
                stmt_kind(&p, d),
                StmtKind::If
                    | StmtKind::Loop
                    | StmtKind::Case
                    | StmtKind::TryCatch
                    | StmtKind::Handler
                    | StmtKind::ExceptionSection
            )
        })
    };
    let mut on = false;
    for n in a.root.descendants().filter(|n| n.kind().is_unit()) {
        let kind = stmt_kind(&n, d);
        let w = ast::first_words(&n, 3);
        let word = |i: usize| w.get(i).map(String::as_str);
        match kind {
            StmtKind::SessionSetting if word(1) == Some("NOCOUNT") => {
                if word(2) == Some("ON") {
                    on = on || !conditional(&n);
                } else {
                    on = false;
                }
            }
            k if rows(k) && !on => return true,
            _ => {}
        }
    }
    false
}

/// Whether a statement's text is a T-SQL `SET NOCOUNT ON` or `OFF`.
pub fn is_nocount(text: &str) -> bool {
    let w: Vec<String> = text
        .split(|c: char| c.is_whitespace() || c == ';')
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_uppercase)
        .collect();
    matches!(
        w.iter().map(String::as_str).collect::<Vec<_>>().as_slice(),
        ["SET", "NOCOUNT", "ON" | "OFF"]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::Artifact;
    use crate::model::Side;

    fn sends(text: &str) -> bool {
        sends_in(Dialect::Tsql, text)
    }

    fn sends_in(d: Dialect, text: &str) -> bool {
        let art = Artifact::new("a.sql", d, text);
        sends_counts(&Analyzed::new(&art, Side::A, 1))
    }

    #[test]
    fn postgres_sends_counts_only_outside_routines() {
        let p = Dialect::Postgres;
        assert!(sends_in(p, "UPDATE t SET x = 1;"));
        assert!(!sends_in(
            p,
            "CREATE PROCEDURE q() LANGUAGE plpgsql AS $$\nBEGIN\n  UPDATE t SET x = 1;\nEND $$;"
        ));
        assert!(!sends_in(p, "DO $$\nBEGIN\n  UPDATE t SET x = 1;\nEND $$;"));
    }

    fn body(s: &str) -> String {
        format!("CREATE PROCEDURE dbo.p AS\nBEGIN\n{s}\nEND\n")
    }

    #[test]
    fn nocount_must_come_first_and_unconditionally() {
        assert!(sends(&body("UPDATE t SET x = 1;")));
        assert!(!sends(&body("SET NOCOUNT ON;\nUPDATE t SET x = 1;")));
        assert!(sends(&body("UPDATE t SET x = 1;\nSET NOCOUNT ON;")));
        assert!(sends(&body(
            "IF 1 = 1\n  SET NOCOUNT ON;\nUPDATE t SET x = 1;"
        )));
        assert!(sends(&body("SET NOCOUNT ON;\nSET NOCOUNT OFF;\nSELECT 1;")));
        assert!(!sends(&body("SET NOCOUNT ON;\nDECLARE @x int = 1;")));
    }

    #[test]
    fn nocount_statements_are_recognised() {
        assert!(is_nocount("SET NOCOUNT ON;"));
        assert!(is_nocount("set   nocount off"));
        assert!(!is_nocount("SET XACT_ABORT ON;"));
        assert!(!is_nocount("SET NOCOUNT ON; UPDATE t SET x = 1;"));
    }
}
