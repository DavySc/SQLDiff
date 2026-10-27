//! Scripts and declared variables.
//!
//! A conversion turns each T-SQL batch that needs variables or control flow
//! into one PostgreSQL DO block or Db2 for i compound statement, and leaves
//! the statements of other batches as plain statements. `segment_pairs`
//! pairs the parts that way, so each batch is compared with its block, with
//! the variables of that batch only (they end at GO as a block's end at its
//! END).
//!
//! `locals` compares declared variable types. Statement terms leave a
//! variable's type out (`schema::Ty` folds precisions away), but the type
//! converts every value assigned to it: checked on SQL Server 2022 and
//! PostgreSQL 18 (2026-10-26), an assignment rounds like a CAST to the
//! declared type (datetime2(0) and timestamp(0) take
//! '2020-01-01 10:00:00.6' to 10:00:01; timestamp(6) takes
//! '9999-12-31 23:59:59.9999999' to 10000-01-01 00:00:00, datetime2(7) keeps
//! it), and a string longer than a T-SQL variable is cut off silently where
//! PostgreSQL raises "value too long".

use sqldiff_syntax::{ast as cst, Dialect, SyntaxKind, SyntaxNode};

use super::ast::{tokens, Expr, Stmt, Tok, Type, K};
use super::canon::asm;
use super::literal::{self, Target};
use super::lower::{self, Local, Segment, Unit};
use super::table;
use super::term::{a, n, Term};
use super::Outcome;
use crate::artifact::Analyzed;

pub const VARIABLE_RANGE: (&str, &str) = (
    "ASM-VARIABLE-RANGE",
    "Values assigned to a converted variable stay within what the original variable's type accepts: the converted variable accepts more (larger numbers, longer strings, wider date ranges), so an assignment that fails or is cut off in the original succeeds in the conversion",
);

pub const VARIABLE_LENGTH: (&str, &str) = (
    "ASM-VARIABLE-LENGTH",
    "Strings assigned to a T-SQL variable fit its declared length: SQL Server cuts a longer string off silently, PostgreSQL raises an error",
);

/// The segments of two scripts, lowered and paired in order. `None` when an
/// artifact is not a script, when neither is split (one plain segment each:
/// the whole artifact is one unit), or when the segments do not line up
/// (then the artifacts are compared whole, and the blocks stay not compared).
pub(super) fn segment_pairs(a: &Analyzed, b: &Analyzed) -> Option<Vec<(Unit, Unit)>> {
    let (sa, sb) = (lower::segments(a)?, lower::segments(b)?);
    let split = |s: &[Segment]| s.len() > 1 || s.iter().any(|x| x.block);
    if !split(&sa) && !split(&sb) {
        return None;
    }
    // A plain segment that lowers to nothing (`SET NOCOUNT ON` alone) has
    // nothing to pair with; a block always has its partner.
    let lowered = |an: &Analyzed, s: &[Segment]| -> Vec<(bool, Unit)> {
        s.iter()
            .map(|g| (g.block, lower::lower_segment(an.dialect(), g)))
            .filter(|(block, u)| *block || !nothing(u))
            .collect()
    };
    let (ua, ub) = (lowered(a, &sa), lowered(b, &sb));
    if ua.len() != ub.len() || ua.iter().zip(&ub).any(|(x, y)| x.0 != y.0) {
        return None;
    }
    Some(ua.into_iter().zip(ub).map(|(x, y)| (x.1, y.1)).collect())
}

fn nothing(u: &Unit) -> bool {
    u.stmts.is_empty()
        && u.uncovered.is_empty()
        && u.tables.is_empty()
        && u.locals.is_empty()
        && !u.txn_control
}

/// Compares the declared types of the variables the statement terms
/// identify (by declaration order, as `canon` does). A pair whose types may
/// store a value differently is compared only if every write to it, on both
/// sides, assigns a literal that converts to the same known value.
pub(super) fn locals(
    a: &Analyzed,
    b: &Analyzed,
    ua: &Unit,
    ub: &Unit,
    na: &str,
    nb: &str,
    out: &mut Outcome,
) {
    if ua.dialect == ub.dialect {
        return;
    }
    for (x, y) in local_pairs(ua, ub) {
        match local_pair(ua, ub, x, y) {
            Ok(asms) => {
                for s in asms {
                    out.assume(asm::make(s));
                }
            }
            Err(why) => out.unknown_reasons.push(format!(
                "{na} line {}: the variable {} (and {nb} line {}, {}) is not compared: {why}",
                a.line_of(&x.node),
                shown(ua.dialect, &x.name),
                b.line_of(&y.node),
                shown(ub.dialect, &y.name),
            )),
        }
    }
}

/// The variables the statement terms identify with each other: by
/// declaration order, or by name (`name_locals`).
fn local_pairs<'u>(ua: &'u Unit, ub: &'u Unit) -> Vec<(&'u Local, &'u Local)> {
    match (&ua.local_ids, &ub.local_ids) {
        (Some(ia), Some(ib)) => ua
            .locals
            .iter()
            .zip(ia)
            .filter_map(|(x, id)| ib.iter().position(|j| j == id).map(|k| (x, &ub.locals[k])))
            .collect(),
        _ => ua.locals.iter().zip(&ub.locals).collect(),
    }
}

/// A variable's name without what a conversion adds or changes: T-SQL
/// `@CurrentDateTime` and PostgreSQL `v_current_date_time` both give
/// `currentdatetime`.
fn name_key(d: Dialect, name: &str) -> String {
    let n = name.trim_start_matches('@').to_lowercase();
    let n = if d == Dialect::Tsql {
        n.as_str()
    } else {
        n.strip_prefix("v_").unwrap_or(&n)
    };
    n.replace('_', "")
}

/// Matches variables by name where declaration order would match them
/// wrongly: when a conversion leaves a declaration out (a table variable it
/// cannot convert), every later variable shifts by one position. Names are
/// used only when they are unique on each side and some name pair sits at
/// different positions; otherwise order stays (it also pairs variables a
/// conversion renamed). Any consistent matching is sound: statements count
/// as equal only under it, and `locals` checks the types of exactly these
/// pairs.
pub(super) fn name_locals(ua: &mut Unit, ub: &mut Unit) {
    let ka: Vec<String> = ua
        .locals
        .iter()
        .map(|l| name_key(ua.dialect, &l.name))
        .collect();
    let kb: Vec<String> = ub
        .locals
        .iter()
        .map(|l| name_key(ub.dialect, &l.name))
        .collect();
    let unique = |k: &[String]| {
        let mut v = k.to_vec();
        v.sort();
        v.dedup();
        v.len() == k.len()
    };
    if !unique(&ka) || !unique(&kb) {
        return;
    }
    let shifted = ka
        .iter()
        .enumerate()
        .any(|(i, k)| kb.iter().position(|x| x == k).is_some_and(|j| j != i));
    if !shifted {
        return;
    }
    let ids = |k: &[String], other: &[String], side: &str| -> Vec<String> {
        k.iter()
            .map(|x| {
                if other.contains(x) {
                    format!("l:{x}")
                } else {
                    format!("l:{side}:{x}")
                }
            })
            .collect()
    };
    ua.local_ids = Some(ids(&ka, &kb, "a"));
    ub.local_ids = Some(ids(&kb, &ka, "b"));
}

fn shown(d: Dialect, name: &str) -> String {
    if d == Dialect::Tsql {
        format!("@{name}")
    } else {
        name.to_string()
    }
}

type Asms = Vec<(&'static str, &'static str)>;

/// A T-SQL variable type, read as `table` reads column types; `sysname` is
/// nvarchar(128).
fn tsql_type(t: &Type) -> Result<table::Ty, String> {
    if t.name.eq_ignore_ascii_case("SYSNAME") && t.args.is_empty() {
        return Ok(table::Ty::Str {
            fixed: false,
            national: true,
            len: Some(128),
        });
    }
    table::tsql_type(t)
}

fn local_pair(ua: &Unit, ub: &Unit, x: &Local, y: &Local) -> Result<Asms, String> {
    let (Some(tx), Some(ty)) = (&x.decl, &y.decl) else {
        return Err("its declared type was not read".into());
    };
    let tsql_pg = match (ua.dialect, ub.dialect) {
        (Dialect::Tsql, Dialect::Postgres) => Some((tx, ty)),
        (Dialect::Postgres, Dialect::Tsql) => Some((ty, tx)),
        _ => None,
    };
    let by_type: Result<Asms, String> = match tsql_pg {
        None => Err(format!(
            "variable types of {} and {} are not compared (no verified Db2 for i type facts)",
            ua.dialect.display_name(),
            ub.dialect.display_name()
        )),
        Some((t, p)) => match (tsql_type(t), table::pg_type(p)) {
            (Ok(t), Ok(p)) => {
                let c = table::types(&t, &p);
                match c.diff {
                    Some(d) => Err(format!(
                        "the declared types store values differently: {}",
                        d.replace("{T}", "T-SQL")
                            .replace("{P}", "PostgreSQL")
                            .replace("in the column", "in the variable")
                            .replace("the column's", "the variable's")
                    )),
                    None => {
                        let mut v: Asms = c.asms;
                        if c.wider.is_some() {
                            v.push(VARIABLE_RANGE);
                        }
                        if matches!(t, table::Ty::Str { len: Some(_), .. }) {
                            v.push(VARIABLE_LENGTH);
                        }
                        Ok(v)
                    }
                }
            }
            (Err(e), _) | (_, Err(e)) => Err(e),
        },
    };
    match by_type {
        Ok(mut v) => {
            // The types agree, but a literal assigned may still convert
            // differently (T-SQL truncates 1.5 into an int, PostgreSQL rounds)
            // or not fit (T-SQL cuts a string off, PostgreSQL raises).
            match (assigned_literals(ua, x), assigned_literals(ub, y)) {
                (Some(lx), Some(ly)) if lx == ly => {}
                (Some(_), Some(_)) => {
                    return Err("the literals assigned do not convert to the same values".into())
                }
                _ => {
                    return Err(
                        "a literal assigned to it may be cut off, rounded or rejected".into(),
                    )
                }
            }
            // Only literals that both types hold unchanged are written: nothing
            // is cut off or out of range.
            let (t_unit, t_local) = if ua.dialect == Dialect::Tsql {
                (ua, x)
            } else {
                (ub, y)
            };
            if literal_only(ua, x)
                && literal_only(ub, y)
                && strings_fit(ua, x)
                && strings_fit(ub, y)
            {
                v.retain(|s| s.0 != VARIABLE_LENGTH.0);
                if literal_values(t_unit, t_local).is_some() {
                    v.retain(|s| s.0 != VARIABLE_RANGE.0);
                }
            }
            Ok(v)
        }
        Err(why) => match (literal_values(ua, x), literal_values(ub, y)) {
            (Some(vx), Some(vy)) if vx == vy => Ok(Vec::new()),
            (Some(_), Some(_)) => Err(format!(
                "{why}; the literals assigned convert to different values"
            )),
            _ => Err(why),
        },
    }
}

/// The values of the literals the unit assigns to the variable, in order, as
/// the declared type stores them; NULL is NULL on every side. None when one
/// of them is not modelled exactly: a string that does not fit a bounded
/// string type, a number into a type other than an integer or decimal, or a
/// conversion that rounds, overflows or fails.
fn assigned_literals(u: &Unit, l: &Local) -> Option<Vec<Term>> {
    let decl = l.decl.as_ref()?;
    let ty = match u.dialect {
        Dialect::Tsql => tsql_type(decl).ok(),
        Dialect::Postgres => table::pg_type(decl).ok(),
        Dialect::Db2i => None,
    };
    let target = Target::of_type(decl, u.dialect);
    let mut out = Vec::new();
    for s in &u.stmts {
        let Some((t, e)) = s.ast.as_ref().ok().and_then(Stmt::assignment) else {
            continue;
        };
        if !names(u.dialect, &t, &l.name) {
            continue;
        }
        out.push(match (e, &ty) {
            (Expr::Null, _) => a("null"),
            (Expr::Str(v), Some(table::Ty::Str { len, .. })) => {
                if len.is_some_and(|n| v.chars().count() > n as usize) {
                    return None;
                }
                n("str", vec![a(v.clone())])
            }
            // T-SQL 0xAB01 and PostgreSQL '\xAB01'::bytea: the bytes, when
            // they fill a fixed type or fit a bounded one.
            (Expr::Num(v), Some(table::Ty::Binary { fixed, len }))
                if v.len() >= 2 && v[..2].eq_ignore_ascii_case("0x") =>
            {
                bytes(&v[2..], *fixed, *len)?
            }
            (Expr::Cast(x, cty), Some(table::Ty::Binary { fixed, len }))
                if u.dialect == Dialect::Postgres && cty.name.eq_ignore_ascii_case("BYTEA") =>
            {
                match x.as_ref() {
                    Expr::Str(v) => bytes(v.strip_prefix("\\x")?, *fixed, *len)?,
                    _ => continue,
                }
            }
            (Expr::Str(v), _) => {
                literal::convert(v, target?, u.dialect, true)
                    .filter(|(_, asm)| asm.is_none())?
                    .0
            }
            (Expr::Num(v), _) => match target? {
                t @ (Target::Int { .. } | Target::Exact { .. } | Target::Bit | Target::Bool) => {
                    literal::convert(v, t, u.dialect, true)?.0
                }
                _ => return None,
            },
            (Expr::Special(w), Some(table::Ty::Bool)) if w == "TRUE" || w == "FALSE" => {
                literal::convert(w, Target::Bool, u.dialect, true)?.0
            }
            _ => continue,
        });
    }
    // bit 1 and boolean TRUE are the same value: the declared types were
    // already matched.
    let t = n("bool", vec![a("TRUE")]);
    let f = n("bool", vec![a("FALSE")]);
    Some(
        out.into_iter()
            .map(|x| {
                if x == t {
                    n("num", vec![a("1")])
                } else if x == f {
                    n("num", vec![a("0")])
                } else {
                    x
                }
            })
            .collect(),
    )
}

fn bytes(hex: &str, fixed: bool, len: Option<u32>) -> Option<Term> {
    let h = super::canon::hex_bytes(hex)?;
    let count = h.len() / 2;
    match len {
        Some(max) if count > max as usize || (fixed && count != max as usize) => None,
        _ => Some(n("bytes", vec![a(h)])),
    }
}

/// Whether every write to the variable assigns a literal or NULL.
fn literal_only(u: &Unit, l: &Local) -> bool {
    writes(u, l).is_some_and(|ws| {
        ws.iter()
            .all(|e| matches!(e, Expr::Null | Expr::Str(_) | Expr::Num(_)))
    })
}

/// Whether every string literal written to a bounded string variable fits it.
fn strings_fit(u: &Unit, l: &Local) -> bool {
    let Some(decl) = &l.decl else { return false };
    let len = match u.dialect {
        Dialect::Tsql => tsql_type(decl).ok(),
        Dialect::Postgres => table::pg_type(decl).ok(),
        Dialect::Db2i => None,
    };
    let Some(table::Ty::Str { len, .. }) = len else {
        return true;
    };
    let Some(len) = len else { return true };
    writes(u, l).is_some_and(|ws| {
        ws.iter().all(|e| match e {
            Expr::Str(s) => s.chars().count() <= len as usize,
            _ => true,
        })
    })
}

/// The values the variable holds, in the order they are assigned, when every
/// write to it is a literal whose conversion to the declared type is modelled
/// exactly (`literal::convert`, as a CAST: an assignment rounds like one).
fn literal_values(u: &Unit, l: &Local) -> Option<Vec<Term>> {
    let decl = l.decl.as_ref()?;
    let target = Target::of_type(decl, u.dialect);
    writes(u, l)?
        .iter()
        .map(|e| match e {
            Expr::Null => Some(a("null")),
            Expr::Str(s) => literal::convert(s, target?, u.dialect, true)
                .filter(|(_, asm)| asm.is_none())
                .map(|(t, _)| t),
            Expr::Num(s) => match target? {
                // A number assigned to a number type, exactly as written.
                t @ (Target::Int { .. } | Target::Exact { .. }) => {
                    literal::convert(s, t, u.dialect, true).map(|(t, _)| t)
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Option<Vec<Term>>>()
        .map(|v| {
            // Declared without a value, a variable starts as NULL on every side.
            if v.is_empty() {
                vec![n("unset", vec![])]
            } else {
                v
            }
        })
}

/// The values assigned to the variable, when every statement of the unit
/// that writes it, or may write it, is a plain assignment.
fn writes(u: &Unit, l: &Local) -> Option<Vec<Expr>> {
    let mut vals = Vec::new();
    let mut seen: Vec<SyntaxNode> = Vec::new();
    for s in &u.stmts {
        if let Some((target, v)) = s.ast.as_ref().ok().and_then(Stmt::assignment) {
            if names(u.dialect, &target, &l.name) {
                vals.push(v.clone());
                seen.push(s.node.clone());
                continue;
            }
        }
    }
    // Every other statement, lowered or not, may only read it.
    let others = u
        .stmts
        .iter()
        .map(|s| s.node.clone())
        .chain(u.uncovered.iter().map(|(n, _)| n.clone()))
        .filter(|n| !seen.contains(n));
    for node in others {
        let stmts: Vec<SyntaxNode> = if node.kind() == SyntaxKind::STATEMENT {
            vec![node.clone()]
        } else {
            node.descendants()
                .filter(|d| d.kind() == SyntaxKind::STATEMENT)
                .collect()
        };
        // A branch or loop body can write it too.
        if node.kind() != SyntaxKind::STATEMENT
            && stmts.is_empty()
            && mentions(&tokens(&node), u.dialect, &l.name)
        {
            return None;
        }
        for s in stmts {
            if may_write(&tokens(&s), u.dialect, &l.name) {
                return None;
            }
        }
    }
    Some(vals)
}

pub(super) fn names(d: Dialect, e: &Expr, name: &str) -> bool {
    match e {
        Expr::Var(v) => d == Dialect::Tsql && v.trim_start_matches('@').eq_ignore_ascii_case(name),
        Expr::Name(parts) => {
            d != Dialect::Tsql && parts.len() == 1 && parts[0].eq_ignore_ascii_case(name)
        }
        _ => false,
    }
}

fn is_name(t: &Tok, d: Dialect, name: &str) -> bool {
    match d {
        Dialect::Tsql => {
            t.kind == K::Var && t.text.trim_start_matches('@').eq_ignore_ascii_case(name)
        }
        _ => {
            matches!(t.kind, K::Word | K::Quoted)
                && cst::unquote(&t.text).eq_ignore_ascii_case(name)
        }
    }
}

fn mentions(ts: &[Tok], d: Dialect, name: &str) -> bool {
    ts.iter().any(|t| is_name(t, d, name))
}

/// Whether a statement may write the variable: it stands before `=`, `:=`,
/// OUTPUT or OUT, after INTO (also in a list), or in a statement that passes
/// it on (EXEC, CALL), loops over it (FOR), fetches or reads diagnostics.
/// Conservative: a comparison written `@v = col` counts as a write.
fn may_write(ts: &[Tok], d: Dialect, name: &str) -> bool {
    let first = ts.first().map(|t| t.up.as_str()).unwrap_or("");
    for (i, t) in ts.iter().enumerate() {
        if !is_name(t, d, name) {
            continue;
        }
        if matches!(
            first,
            "EXEC" | "EXECUTE" | "CALL" | "FOR" | "FETCH" | "GET" | "OPEN" | "PERFORM" | "FOREACH"
        ) {
            return true;
        }
        if ts.get(i + 1).is_some_and(|x| {
            matches!(x.text.as_str(), "=" | ":=") || matches!(x.up.as_str(), "OUTPUT" | "OUT")
        }) {
            return true;
        }
        // Back over a list of names to INTO.
        let mut j = i;
        while j > 0 {
            let p = &ts[j - 1];
            if p.up == "INTO" {
                return true;
            }
            if p.kind == K::Comma
                && j >= 2
                && matches!(ts[j - 2].kind, K::Var | K::Word | K::Quoted)
            {
                j -= 2;
                continue;
            }
            break;
        }
    }
    false
}

/// Whether a T-SQL statement is an assignment of a literal that cannot fail:
/// NULL, a string into a string variable or a binary constant into a binary
/// one (a longer value is cut off, no error), or a literal whose conversion to the variable's type is modelled
/// and succeeds. An overflowing literal fails and the batch goes on
/// (`DECLARE @i tinyint = 300` reports Msg 220 and runs the next
/// statement, SQL Server 2022), so it does not count.
pub(super) fn cannot_fail(u: &Unit, node: &SyntaxNode) -> bool {
    let Some(s) = u.stmts.iter().find(|s| &s.node == node) else {
        return false;
    };
    let Some((target, v)) = s.ast.as_ref().ok().and_then(Stmt::assignment) else {
        return false;
    };
    let Some(l) = u.locals.iter().find(|l| names(u.dialect, &target, &l.name)) else {
        return false;
    };
    let Some(decl) = &l.decl else { return false };
    let ty = match u.dialect {
        Dialect::Tsql => tsql_type(decl).ok(),
        _ => None,
    };
    let string = matches!(ty, Some(table::Ty::Str { .. }));
    let binary = matches!(ty, Some(table::Ty::Binary { .. }));
    match v {
        Expr::Null => true,
        Expr::Str(_) if string => true,
        // A binary constant is cut off or zero-padded to the variable's
        // length, without an error (checked on SQL Server 2022).
        Expr::Num(x) if binary && x.len() >= 2 && x[..2].eq_ignore_ascii_case("0x") => true,
        Expr::Str(x) | Expr::Num(x) => Target::of_type(decl, u.dialect)
            .and_then(|t| literal::convert(x, t, u.dialect, true))
            .is_some(),
        _ => false,
    }
}
