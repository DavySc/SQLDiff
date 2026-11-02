//! Binary variables whose value the code fixes.
//!
//! `geography::STGeomFromWKB(@b, 4326)` and PostGIS's `ST_GeomFromWKB(b,
//! 4326)` agree only for a value both engines read alike
//! (`geography::wkb`), so the engine needs the bytes. A long WKB does not fit
//! one statement, so scripts build it in pieces: `DECLARE @b varbinary(max);
//! SET @b = 0x…; SET @b += 0x…;` against `b := '\x…'::bytea; b := b ||
//! '\x…'::bytea;`. This module reads the value a variable holds when a
//! statement starts, by walking back through the statements before it, as
//! `dynamic.rs` does for the text of dynamic SQL, and only when the code
//! alone fixes it: binary constants, `+` (T-SQL) and `||` (PostgreSQL) over
//! them and over variables that are themselves fixed. A parameter, a value
//! read from a table, a variable a loop, a CATCH block, a GOTO or a
//! statement this reading does not follow may have changed, an IF that
//! writes it, a type other than `varbinary(max)` / `bytea` (T-SQL cuts a
//! value stored in `varbinary(n)` off silently) all leave it unknown.

use sqldiff_syntax::{ast as cst, Dialect, SyntaxKind, SyntaxNode};

use super::ast::{string_value, tokens, Tok, K};

/// Variables nest this deep at most (`SET @a = @b + 0x…`).
const DEPTH: usize = 16;

/// The hex (lower case) of the bytes variable `name` holds when statement
/// `at` starts; `Err` says why it is not fixed.
pub fn before(at: &SyntaxNode, name: &str, d: Dialect) -> Result<String, String> {
    if d == Dialect::Db2i {
        return Err("Db2 for i binary variables are not read".into());
    }
    jumps(at)?;
    Reader { d, depth: 0 }.value(at, name)
}

fn jumps(stmt: &SyntaxNode) -> Result<(), String> {
    let root = stmt.ancestors().last().unwrap_or_else(|| stmt.clone());
    let jump = root.descendants().any(|n| {
        n.kind() == SyntaxKind::LABEL
            || n.kind() == SyntaxKind::STATEMENT
                && cst::first_words(&n, 1).first().is_some_and(|w| w == "GOTO")
    });
    if jump {
        return Err("the code uses GOTO or labels, so what runs before is not fixed".into());
    }
    Ok(())
}

struct Reader {
    d: Dialect,
    depth: usize,
}

/// A value read: its hex and whether an operand was a `max` type, which
/// T-SQL's `+` does not cut at 8,000 bytes.
struct Val {
    hex: String,
    max: bool,
}

impl Reader {
    fn is_name(&self, t: &Tok, name: &str) -> bool {
        match self.d {
            Dialect::Tsql => t.kind == K::Var && t.text[1..].eq_ignore_ascii_case(name),
            _ => t.kind == K::Word && t.text.eq_ignore_ascii_case(name),
        }
    }

    fn mentions(&self, n: &SyntaxNode, name: &str) -> bool {
        cst::sig_tokens(n).any(|t| {
            let t = Tok::from_syntax(&t);
            self.is_name(&t, name)
        })
    }

    /// The value of `name` when `at` starts: the statements before it, newest
    /// first, up to the assignment that fixes it, with every `+=` / `||`
    /// piece after that one joined in order.
    fn value(&mut self, at: &SyntaxNode, name: &str) -> Result<String, String> {
        self.depth += 1;
        if self.depth > DEPTH {
            return Err("the value is built over too many variables".into());
        }
        self.declared(at, name)?;
        let mut pieces: Vec<String> = Vec::new();
        let mut cur = at.clone();
        loop {
            let p = self.previous(&cur, name)?;
            match self.step(&p, name)? {
                Step::Skip => cur = p,
                Step::Append(v) => {
                    pieces.push(v);
                    cur = p;
                }
                Step::Set(v) => {
                    pieces.push(v);
                    break;
                }
            }
        }
        self.depth -= 1;
        pieces.reverse();
        Ok(pieces.concat())
    }

    /// The nearest statement before `cur` that mentions `name` and may write
    /// it; a loop, CATCH block or source outside the code is an error.
    fn previous(&self, cur: &SyntaxNode, name: &str) -> Result<SyntaxNode, String> {
        let mut cur = cur.clone();
        loop {
            let mut prev = cur.prev_sibling();
            while let Some(p) = prev {
                let writes = || {
                    p.descendants()
                        .filter(|d| d.kind() == SyntaxKind::STATEMENT)
                        .any(|d| self.mentions(&d, name) && !self.reads_only(&d, name))
                        || p.descendants().any(|d| {
                            matches!(
                                d.kind(),
                                SyntaxKind::ERROR | SyntaxKind::CASE_STMT | SyntaxKind::HANDLER
                            )
                        })
                };
                if self.mentions(&p, name)
                    && (matches!(
                        p.kind(),
                        SyntaxKind::STATEMENT | SyntaxKind::DECLARE_SECTION
                    ) || writes())
                {
                    return Ok(p);
                }
                prev = p.prev_sibling();
            }
            let parent = cur
                .parent()
                .ok_or_else(|| format!("{name} is not declared before the statement"))?;
            match parent.kind() {
                SyntaxKind::BLOCK | SyntaxKind::TRY_BLOCK | SyntaxKind::TRY_CATCH => {}
                SyntaxKind::BRANCH => {
                    let owner = parent.parent().ok_or("a branch without a statement")?;
                    if owner.kind() != SyntaxKind::IF_STMT {
                        return Err(format!("{name} is read in a loop or branch not followed"));
                    }
                    cur = owner;
                    continue;
                }
                SyntaxKind::ROUTINE_BODY
                | SyntaxKind::BATCH
                | SyntaxKind::SOURCE_FILE
                | SyntaxKind::ROUTINE => {
                    return Err(format!(
                        "{name} gets its value from outside the code (a parameter or the caller)"
                    ))
                }
                k => {
                    return Err(format!(
                        "{name} is read inside {k:?}, which is not followed"
                    ))
                }
            }
            cur = parent;
        }
    }

    /// A statement that mentions `name` only to read it: a plain assignment
    /// of another variable, or (T-SQL) PRINT.
    fn reads_only(&self, s: &SyntaxNode, name: &str) -> bool {
        let ts = tokens(s);
        match (self.d, ts.as_slice()) {
            (Dialect::Tsql, [set, v, op, ..]) => {
                set.kind == K::Word
                    && set.up == "SET"
                    && v.kind == K::Var
                    && !self.is_name(v, name)
                    && matches!(op.text.as_str(), "=" | "+=")
                    && !ts.iter().any(|t| t.up == "SELECT")
            }
            (Dialect::Tsql, [p, ..]) => p.kind == K::Word && p.up == "PRINT",
            (_, [v, op, ..]) => {
                v.kind == K::Word
                    && !self.is_name(v, name)
                    && matches!(op.text.as_str(), ":=" | "=")
                    && !ts.iter().any(|t| t.up == "SELECT")
            }
            _ => false,
        }
    }

    /// What statement `p` does to `name`.
    fn step(&mut self, p: &SyntaxNode, name: &str) -> Result<Step, String> {
        if p.kind() == SyntaxKind::DECLARE_SECTION {
            return self.pg_declaration(p, name);
        }
        if p.kind() != SyntaxKind::STATEMENT {
            return Err(format!(
                "{name} may be changed inside a block before the statement"
            ));
        }
        if self.reads_only(p, name) {
            return Ok(Step::Skip);
        }
        let ts = tokens(p);
        match (self.d, ts.as_slice()) {
            (Dialect::Tsql, [set, v, op, rest @ ..])
                if set.up == "SET" && set.kind == K::Word && self.is_name(v, name) =>
            {
                let val = self.tsql_expr(p, rest)?.hex;
                match op.text.as_str() {
                    "=" => Ok(Step::Set(val)),
                    "+=" => Ok(Step::Append(val)),
                    o => Err(format!("{name} is changed with `{o}`")),
                }
            }
            (Dialect::Tsql, [d, rest @ ..]) if d.kind == K::Word && d.up == "DECLARE" => {
                let (_, init) = declarator(rest, name)
                    .ok_or_else(|| format!("{name} is declared in a form not read"))?;
                let init = init.ok_or_else(|| format!("{name} is NULL when declared"))?;
                Ok(Step::Set(self.tsql_expr(p, init)?.hex))
            }
            (Dialect::Postgres, [v, op, rest @ ..])
                if self.is_name(v, name) && matches!(op.text.as_str(), ":=" | "=") =>
            {
                let (val, appended) = self.pg_expr(p, rest, name)?;
                Ok(if appended {
                    Step::Append(val)
                } else {
                    Step::Set(val)
                })
            }
            _ => Err(format!(
                "{name} may be changed by the statement at byte {}",
                cst::sig_range(p).0
            )),
        }
    }

    /// PL/pgSQL: `name bytea [:= | = | DEFAULT value];` in the DECLARE section.
    fn pg_declaration(&mut self, sec: &SyntaxNode, name: &str) -> Result<Step, String> {
        for s in sec.children().filter(|c| c.kind() == SyntaxKind::STATEMENT) {
            let ts = tokens(&s);
            let Some(first) = ts.first() else { continue };
            if !self.is_name(first, name) {
                continue;
            }
            let eq = ts
                .iter()
                .position(|t| matches!(t.text.as_str(), ":=" | "=") || t.up == "DEFAULT");
            let ty_end = eq.unwrap_or(ts.len());
            let ty: Vec<&str> = ts[1..ty_end]
                .iter()
                .filter(|t| t.kind != K::Semi)
                .map(|t| t.up.as_str())
                .collect();
            if ty != ["BYTEA"] {
                return Err(format!("{name} is not declared as bytea"));
            }
            let Some(eq) = eq else {
                return Err(format!("{name} is NULL when declared"));
            };
            let rest: Vec<Tok> = ts[eq + 1..]
                .iter()
                .filter(|t| t.kind != K::Semi)
                .cloned()
                .collect();
            let (v, appended) = self.pg_expr(&s, &rest, name)?;
            if appended {
                return Err(format!("{name} is declared with its own value"));
            }
            return Ok(Step::Set(v));
        }
        Err(format!("{name} is not declared in this section"))
    }

    /// The declared type of `name` in the routine or batch holding `at`:
    /// `varbinary(max)` (T-SQL) or `bytea`, declared once.
    fn declared(&self, at: &SyntaxNode, name: &str) -> Result<(), String> {
        let scope = at
            .ancestors()
            .find(|n| {
                matches!(
                    n.kind(),
                    SyntaxKind::ROUTINE | SyntaxKind::BATCH | SyntaxKind::SOURCE_FILE
                )
            })
            .ok_or("no routine or batch")?;
        let mut found = 0;
        for st in scope
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::STATEMENT)
        {
            let ts = tokens(&st);
            let ty: Option<Vec<String>> = match self.d {
                Dialect::Tsql if ts.first().is_some_and(|t| t.up == "DECLARE") => {
                    declarator(&ts[1..], name)
                        .map(|(ty, _)| ty.iter().map(|t| t.up.clone()).collect())
                }
                Dialect::Postgres
                    if st
                        .parent()
                        .is_some_and(|p| p.kind() == SyntaxKind::DECLARE_SECTION)
                        && ts.first().is_some_and(|t| self.is_name(t, name)) =>
                {
                    let end = ts
                        .iter()
                        .position(|t| matches!(t.text.as_str(), ":=" | "=") || t.up == "DEFAULT")
                        .unwrap_or(ts.len());
                    Some(
                        ts[1..end]
                            .iter()
                            .filter(|t| t.kind != K::Semi)
                            .map(|t| t.up.clone())
                            .collect(),
                    )
                }
                _ => None,
            };
            if let Some(ty) = ty {
                found += 1;
                let ok = match self.d {
                    Dialect::Tsql => ty == ["VARBINARY", "(", "MAX", ")"],
                    _ => ty == ["BYTEA"],
                };
                if !ok {
                    return Err(format!(
                        "{name} is declared as `{}`, not a type this reading follows",
                        ty.join(" ").to_lowercase()
                    ));
                }
            }
        }
        match found {
            1 => Ok(()),
            0 => Err(format!("{name} is a parameter or not declared in the code")),
            _ => Err(format!("{name} is declared more than once")),
        }
    }

    /// T-SQL `term + term + …`: binary constants and fixed variables.
    fn tsql_expr(&mut self, at: &SyntaxNode, ts: &[Tok]) -> Result<Val, String> {
        let ts: Vec<&Tok> = ts.iter().filter(|t| t.kind != K::Semi).collect();
        let mut acc: Option<Val> = None;
        for part in split_top(&ts, |t| t.kind == K::Op && t.text == "+") {
            let v = self.tsql_term(at, part)?;
            acc = Some(match acc {
                None => v,
                Some(a) => {
                    let hex = a.hex + &v.hex;
                    let max = a.max || v.max;
                    // Only a `max` operand keeps more than 8,000 bytes.
                    if !max && hex.len() / 2 > 8000 {
                        return Err("the concatenation is longer than T-SQL keeps".into());
                    }
                    Val { hex, max }
                }
            });
        }
        acc.ok_or_else(|| "empty value".into())
    }

    fn tsql_term(&mut self, at: &SyntaxNode, ts: &[&Tok]) -> Result<Val, String> {
        match ts {
            [t] if t.kind == K::Num => {
                let h = t
                    .text
                    .get(2..)
                    .filter(|_| t.text[..2].eq_ignore_ascii_case("0x"))
                    .filter(|h| h.len() % 2 == 0 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                    .ok_or_else(|| format!("`{}` is not a binary constant", t.text))?;
                // A constant over 8,000 bytes is `varbinary(max)`.
                Ok(Val {
                    hex: h.to_ascii_lowercase(),
                    max: h.len() / 2 > 8000,
                })
            }
            [t] if t.kind == K::Var && !t.text.starts_with("@@") => {
                let hex = self.value(at, &t.text[1..])?;
                Ok(Val { hex, max: true })
            }
            [o, inner @ .., c] if o.kind == K::LParen && c.kind == K::RParen => {
                let inner: Vec<Tok> = inner.iter().map(|t| (*t).clone()).collect();
                self.tsql_expr(at, &inner)
            }
            _ => Err("the value is built with something else than binary constants".into()),
        }
    }

    /// PL/pgSQL `term || term …`; whether the first term is the variable
    /// itself (`b := b || …`, an append) comes back too.
    fn pg_expr(
        &mut self,
        at: &SyntaxNode,
        ts: &[Tok],
        name: &str,
    ) -> Result<(String, bool), String> {
        let ts: Vec<&Tok> = ts.iter().filter(|t| t.kind != K::Semi).collect();
        let mut hex = String::new();
        let mut appended = false;
        for (i, part) in split_top(&ts, |t| t.kind == K::Op && t.text == "||")
            .into_iter()
            .enumerate()
        {
            if i == 0 && matches!(part, [t] if self.is_name(t, name)) {
                appended = true;
                continue;
            }
            hex.push_str(&self.pg_term(at, part)?);
        }
        if hex.is_empty() && !appended {
            return Err("empty value".into());
        }
        Ok((hex, appended))
    }

    fn pg_term(&mut self, at: &SyntaxNode, ts: &[&Tok]) -> Result<String, String> {
        match ts {
            // '\x0102'::bytea
            [s, c, ty] if s.kind == K::Str && c.text == "::" && ty.up == "BYTEA" => {
                let v = string_value(&s.text)?;
                if s.text.starts_with(['E', 'e']) {
                    return Err("an escape string is not read".into());
                }
                v.strip_prefix("\\x")
                    .filter(|h| h.len() % 2 == 0 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                    .map(str::to_ascii_lowercase)
                    .ok_or_else(|| format!("`{}` is not a bytea hex constant", s.text))
            }
            [t] if t.kind == K::Word => {
                if matches!(t.up.as_str(), "NULL") {
                    return Err("NULL".into());
                }
                self.value(at, &t.text)
            }
            [o, inner @ .., c] if o.kind == K::LParen && c.kind == K::RParen => {
                let inner: Vec<Tok> = inner.iter().map(|t| (*t).clone()).collect();
                self.pg_expr(at, &inner, "").map(|(h, _)| h)
            }
            _ => Err("the value is built with something else than bytea constants".into()),
        }
    }
}

enum Step {
    /// A read of the variable that leaves it as it was.
    Skip,
    /// `name += piece` / `name := name || piece`.
    Append(String),
    /// `name = value`: the walk back ends.
    Set(String),
}

/// Splits at top-level tokens satisfying `is_sep`.
fn split_top<'a, 'b>(ts: &'b [&'a Tok], is_sep: impl Fn(&Tok) -> bool) -> Vec<&'b [&'a Tok]> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0);
    for (i, t) in ts.iter().enumerate() {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => depth = depth.saturating_sub(1),
            _ if depth == 0 && is_sep(t) => {
                parts.push(&ts[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&ts[start..]);
    parts
}

/// In T-SQL `DECLARE` tokens after the keyword: the type and initial value of
/// `@name`, when it is declared there.
fn declarator<'t>(ts: &'t [Tok], name: &str) -> Option<(&'t [Tok], Option<&'t [Tok]>)> {
    let mut depth = 0usize;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, t) in ts.iter().enumerate() {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => depth = depth.saturating_sub(1),
            K::Comma if depth == 0 => {
                parts.push(&ts[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&ts[start..]);
    let part = parts.into_iter().find(|p| {
        p.first()
            .is_some_and(|t| t.kind == K::Var && t.text[1..].eq_ignore_ascii_case(name))
    })?;
    let rest = &part[1..];
    let rest = if rest
        .first()
        .is_some_and(|t| t.kind == K::Word && t.up == "AS")
    {
        &rest[1..]
    } else {
        rest
    };
    match rest.iter().position(|t| t.kind == K::Op && t.text == "=") {
        Some(eq) => Some((&rest[..eq], Some(&rest[eq + 1..]))),
        None => Some((rest, None)),
    }
}
