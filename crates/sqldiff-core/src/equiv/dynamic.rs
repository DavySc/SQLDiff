//! Dynamic SQL whose text is known before it runs.
//!
//! T-SQL `EXEC (@sql)` and `EXEC sp_executesql @sql` run a string as a
//! batch. The string is a value, so nothing can be compared until every
//! text it can hold is known. This module reads the text when it is fixed
//! by the code alone: string literals, `NCHAR`/`CHAR` of a number,
//! `QUOTENAME` and `+` over those, and variables whose last assignment
//! before the statement is such an expression, found by walking back
//! through the statements that run before it. Anything else (a parameter,
//! a value read from a table, a variable that a loop, a CATCH block or a
//! statement this reading does not follow may have changed) leaves the
//! text unknown, and the statement is not compared.
//!
//! The converter runs the same reading (it emits `EXECUTE '<converted
//! text>'`), and the engine compares the statement inside the string with
//! the one inside the PostgreSQL `EXECUTE` like any other pair. The gate is
//! unchanged: an inner statement the engine would not prove on its own is
//! not proven inside a string either.
//!
//! Facts used (checked on SQL Server 2022 and PostgreSQL 18.6, 2026-10-29;
//! `docs/cross-dialect-proofs.md`, "Dynamic SQL"):
//! - `EXEC (NULL)`, `EXEC (N'')` and `sp_executesql NULL` do nothing and
//!   raise nothing. PostgreSQL `EXECUTE NULL` raises an error; `EXECUTE ''`
//!   does nothing.
//! - `sp_executesql` takes only nchar/nvarchar/ntext text: a varchar
//!   argument fails the call.
//! - Assigning to `nvarchar(n)` cuts the value to n characters without an
//!   error. Concatenating strings that are not `(n)varchar(max)` cuts the
//!   result to 4000 (nvarchar) or 8000 (varchar) characters, evaluated left
//!   to right; a literal longer than that is already `max`.
//! - A `#temp` table created in the dynamic batch is dropped when the batch
//!   ends; the batch cannot see the caller's variables; without XACT_ABORT a
//!   failing statement inside it lets the next one run.
//! - `sp_executesql` converts each value to its parameter's type as it
//!   passes it (2.7 to `int` gives 2) and fails when a declared parameter is
//!   not passed; PostgreSQL `EXECUTE … USING` types `$n` by the value (`USING
//!   2.7` stored in an integer column gives 3) and lets `$n` go unread. So
//!   only variables of exactly the parameter's type are compared as values.

use sqldiff_syntax::{ast as cst, Dialect, SyntaxKind, SyntaxNode};

use super::ast::{string_value, tokens, Tok, K};

/// The text a dynamic SQL statement runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Text {
    Known(String),
    /// The text is NULL: T-SQL runs nothing.
    Null,
    /// Why the text is not known.
    Unknown(String),
}

/// How far back the reading follows assignments.
const DEPTH: usize = 64;

/// A T-SQL dynamic SQL statement: `EXEC[UTE] (…)` or `EXEC[UTE]
/// [sys.]sp_executesql …`. `EXEC @rc = proc` and `EXEC @proc` are not:
/// they run a procedure.
pub fn is_tsql(stmt: &SyntaxNode) -> bool {
    stmt.kind() == SyntaxKind::STATEMENT && exec_form(&tokens(stmt)).is_some()
}

enum Form {
    /// `EXEC (expr)`: the token range of `expr`.
    Paren(usize, usize),
    /// `EXEC sp_executesql arg`: the argument's token range.
    ExecuteSql(usize, usize),
}

fn exec_form(ts: &[Tok]) -> Option<Form> {
    let first = ts.first()?;
    if !(first.kind == K::Word && matches!(first.up.as_str(), "EXEC" | "EXECUTE")) {
        return None;
    }
    let next = ts.get(1)?;
    if next.kind == K::LParen {
        let close = close_of(ts, 1)?;
        return Some(Form::Paren(2, close));
    }
    // `sp_executesql`, `sys.sp_executesql`.
    let mut i = 1;
    if matches!(next.kind, K::Word | K::Quoted)
        && cst::unquote(&next.text).eq_ignore_ascii_case("sys")
        && ts.get(2).is_some_and(|t| t.kind == K::Dot)
    {
        i = 3;
    }
    let name = ts.get(i)?;
    if matches!(name.kind, K::Word | K::Quoted)
        && cst::unquote(&name.text).eq_ignore_ascii_case("sp_executesql")
    {
        return Some(Form::ExecuteSql(i + 1, ts.len()));
    }
    None
}

fn close_of(ts: &[Tok], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, t) in ts.iter().enumerate().skip(open) {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Positions of the commas outside parentheses.
fn top_commas(ts: &[Tok]) -> Vec<usize> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    for (i, t) in ts.iter().enumerate() {
        match t.kind {
            K::LParen => depth += 1,
            K::RParen => depth = depth.saturating_sub(1),
            K::Comma if depth == 0 => out.push(i),
            _ => {}
        }
    }
    out
}

fn split_top(ts: &[Tok]) -> Vec<&[Tok]> {
    let mut parts = Vec::new();
    let mut start = 0;
    for c in top_commas(ts) {
        parts.push(&ts[start..c]);
        start = c + 1;
    }
    parts.push(&ts[start..]);
    parts
}

fn shown(ts: &[Tok]) -> String {
    ts.iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A parameter `sp_executesql` declares for its text.
#[derive(Debug, Clone)]
pub struct Param {
    /// Lower case, without `@`.
    pub name: String,
    /// The type as declared.
    pub ty: Vec<Tok>,
}

/// What `sp_executesql` declares and passes after its text: the
/// parameters, and the value each receives, in parameter order.
#[derive(Debug, Clone)]
pub struct Passed {
    pub params: Vec<Param>,
    pub args: Vec<Vec<Tok>>,
}

/// The parameters of `EXEC sp_executesql text, N'@a int, …', @a = …`:
/// `None` without any. Only plain input parameters, each passed exactly
/// once: no OUTPUT, no defaults (SQL Server fails the call when a declared
/// parameter is not passed, PostgreSQL `USING` does not).
pub fn exec_params(stmt: &SyntaxNode) -> Result<Option<Passed>, String> {
    let ts = tokens(stmt);
    let Some(Form::ExecuteSql(s, e)) = exec_form(&ts) else {
        return Ok(None);
    };
    let parts = split_top(&ts[s..e]);
    if parts.len() == 1 {
        return Ok(None);
    }
    let named = |p: &[Tok], name: &str| -> bool {
        p.first()
            .is_some_and(|t| t.kind == K::Var && t.text[1..].eq_ignore_ascii_case(name))
            && p.get(1).is_some_and(|t| t.kind == K::Op && t.text == "=")
    };
    let decl = if named(parts[1], "params") {
        &parts[1][2..]
    } else {
        parts[1]
    };
    jumps(stmt)?;
    let v = Reader { depth: 0 }.expr(stmt, decl)?;
    let Some(text) = v.s else {
        return Err("the parameter list is NULL".into());
    };
    if !v.unicode {
        return Err("sp_executesql is given a varchar parameter list, which it rejects".into());
    }
    let root = sqldiff_syntax::parse(&format!("DECLARE {text}"), Dialect::Tsql).syntax();
    let dts = tokens(&root);
    let mut params: Vec<Param> = Vec::new();
    for p in split_top(&dts[1..]) {
        let (Some(v), rest) = (
            p.first().filter(|t| t.kind == K::Var),
            p.get(1..).unwrap_or(&[]),
        ) else {
            return Err(format!("the parameter list `{text}` is not read"));
        };
        let rest = match rest.first() {
            Some(t) if t.kind == K::Word && t.up == "AS" => &rest[1..],
            _ => rest,
        };
        if rest.is_empty()
            || rest.iter().any(|t| {
                t.kind == K::Op && t.text == "="
                    || t.kind == K::Word && matches!(t.up.as_str(), "OUTPUT" | "OUT" | "READONLY")
            })
        {
            return Err(format!(
                "the parameter `{}` has a default, OUTPUT or READONLY",
                shown(p)
            ));
        }
        let name = v.text[1..].to_lowercase();
        if params.iter().any(|q| q.name == name) {
            return Err(format!("the parameter @{name} is declared twice"));
        }
        params.push(Param {
            name,
            ty: rest.to_vec(),
        });
    }
    let mut args: Vec<Option<Vec<Tok>>> = vec![None; params.len()];
    let mut by_name = false;
    for (k, a) in parts[2..].iter().enumerate() {
        if a.iter()
            .any(|t| t.kind == K::Word && matches!(t.up.as_str(), "OUTPUT" | "OUT"))
        {
            return Err("sp_executesql passes an OUTPUT parameter".into());
        }
        let (at, value) = match a {
            [v, eq, rest @ ..] if v.kind == K::Var && eq.kind == K::Op && eq.text == "=" => {
                by_name = true;
                let n = v.text[1..].to_lowercase();
                let i = params
                    .iter()
                    .position(|q| q.name == n)
                    .ok_or_else(|| format!("sp_executesql passes @{n}, which is not declared"))?;
                (i, rest)
            }
            _ if by_name => return Err("a positional value after a named one".into()),
            _ if k < params.len() => (k, *a),
            _ => return Err("sp_executesql passes more values than parameters".into()),
        };
        if value.is_empty() || value.iter().any(|t| t.kind == K::Word && t.up == "DEFAULT") {
            return Err("sp_executesql passes DEFAULT or nothing".into());
        }
        if args[at].replace(value.to_vec()).is_some() {
            return Err(format!("@{} is passed twice", params[at].name));
        }
    }
    let args = args
        .into_iter()
        .zip(&params)
        .map(|(a, p)| {
            a.ok_or_else(|| {
                format!(
                    "@{} is declared and not passed, which fails the call",
                    p.name
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(Passed { params, args }))
}

/// The type `@name` is declared with in the routine or batch holding `at`,
/// as a parameter of the routine or in its one DECLARE.
pub fn declared_type(at: &SyntaxNode, name: &str) -> Option<Vec<Tok>> {
    let scope = at.ancestors().find(|n| {
        matches!(
            n.kind(),
            SyntaxKind::ROUTINE | SyntaxKind::BATCH | SyntaxKind::SOURCE_FILE
        )
    })?;
    let mut found: Vec<Vec<Tok>> = Vec::new();
    for n in scope
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::STATEMENT)
    {
        let ts = tokens(&n);
        if ts
            .first()
            .is_some_and(|t| t.kind == K::Word && t.up == "DECLARE")
        {
            if let Some((ty, _)) = declarator(&ts[1..], name) {
                found.push(ty.to_vec());
            }
        }
    }
    if scope.kind() == SyntaxKind::ROUTINE {
        // The header up to the AS that starts the body.
        let ts = tokens(&scope);
        let end = (1..ts.len())
            .find(|&j| ts[j].kind == K::Word && ts[j].up == "AS" && ts[j - 1].kind != K::Var)
            .unwrap_or(ts.len());
        for p in split_top(&ts[..end]) {
            let Some(i) = p
                .iter()
                .position(|t| t.kind == K::Var && t.text[1..].eq_ignore_ascii_case(name))
            else {
                continue;
            };
            let mut rest = &p[i + 1..];
            if rest
                .first()
                .is_some_and(|t| t.kind == K::Word && t.up == "AS")
            {
                rest = &rest[1..];
            }
            let stop = rest
                .iter()
                .position(|t| {
                    t.kind == K::Op && t.text == "="
                        || t.kind == K::Word
                            && matches!(t.up.as_str(), "OUTPUT" | "OUT" | "READONLY")
                })
                .unwrap_or(rest.len());
            found.push(rest[..stop].to_vec());
        }
    }
    match found.as_slice() {
        [t] if !t.is_empty() => Some(t.clone()),
        _ => None,
    }
}

/// Two T-SQL types written the same way, letter case and spaces aside.
pub fn same_type(x: &[Tok], y: &[Tok]) -> bool {
    let key = |ts: &[Tok]| ts.iter().map(|t| t.up.clone()).collect::<Vec<_>>();
    key(x) == key(y)
}

/// The text a T-SQL dynamic SQL statement runs, when the code alone fixes it.
pub fn tsql_text(stmt: &SyntaxNode) -> Text {
    let ts = tokens(stmt);
    match tsql_text_of(stmt, &ts) {
        Ok(Some(s)) => Text::Known(s),
        Ok(None) => Text::Null,
        Err(e) => Text::Unknown(e),
    }
}

fn tsql_text_of(stmt: &SyntaxNode, ts: &[Tok]) -> Result<Option<String>, String> {
    let form = exec_form(ts).ok_or("not dynamic SQL")?;
    jumps(stmt)?;
    let mut r = Reader { depth: 0 };
    match form {
        Form::Paren(s, e) => {
            if e + 1 != ts.len() {
                return Err(format!(
                    "`{}` after the text changes how it runs",
                    ts[e + 1].text
                ));
            }
            Ok(r.expr(stmt, &ts[s..e])?.s)
        }
        Form::ExecuteSql(s, e) => {
            let mut arg = &ts[s..e];
            if arg
                .first()
                .is_some_and(|t| t.kind == K::Var && t.up == "@STMT")
                && arg.get(1).is_some_and(|t| t.text == "=")
            {
                arg = &arg[2..];
            }
            // The parameters and their values follow: `exec_params`.
            let arg = &arg[..top_commas(arg).first().copied().unwrap_or(arg.len())];
            let [t] = arg else {
                return Err("sp_executesql without one text argument".into());
            };
            let v = r.expr(stmt, std::slice::from_ref(t))?;
            if !v.unicode {
                // SQL Server: "Procedure expects parameter '@statement' of
                // type 'ntext/nchar/nvarchar'".
                return Err("sp_executesql is given varchar text, which it rejects".into());
            }
            Ok(v.s)
        }
    }
}

/// A routine with labels or GOTO can reach a statement from anywhere.
fn jumps(stmt: &SyntaxNode) -> Result<(), String> {
    let root = stmt.ancestors().last().unwrap_or_else(|| stmt.clone());
    let jump = root.descendants().any(|n| {
        n.kind() == SyntaxKind::LABEL
            || n.kind() == SyntaxKind::STATEMENT
                && cst::first_words(&n, 1).first().is_some_and(|w| w == "GOTO")
    });
    if jump {
        return Err(
            "the code uses GOTO or labels, so what runs before the statement is not fixed".into(),
        );
    }
    if root.descendants().any(|n| {
        n.kind() == SyntaxKind::STATEMENT
            && cst::first_words(&n, 2) == ["SET", "CONCAT_NULL_YIELDS_NULL"]
    }) {
        return Err(
            "the code sets CONCAT_NULL_YIELDS_NULL, which changes how strings concatenate".into(),
        );
    }
    Ok(())
}

/// A value that may be NULL.
#[derive(Debug, Clone)]
struct Opt {
    s: Option<String>,
    unicode: bool,
    max: bool,
}

/// UTF-16 length: T-SQL counts nvarchar characters in UTF-16 code units.
/// Only text inside the Basic Multilingual Plane is read, so that is also
/// the number of characters.
fn len(s: &str) -> usize {
    s.chars().count()
}

fn bmp(s: &str) -> Result<(), String> {
    if s.chars().any(|c| c as u32 > 0xFFFF) {
        return Err("the text holds characters outside the Basic Multilingual Plane".into());
    }
    if s.contains('\0') {
        return Err("the text holds a NUL character".into());
    }
    Ok(())
}

/// A varchar holds only what its code page can; ASCII fits every one.
fn ascii(s: &str) -> Result<(), String> {
    if !s.is_ascii() {
        return Err("varchar text with characters outside ASCII depends on the code page".into());
    }
    Ok(())
}

/// The declared type of a string variable: (unicode, length or None for max).
#[derive(Debug, Clone, Copy)]
struct Decl {
    unicode: bool,
    len: Option<usize>,
}

struct Reader {
    depth: usize,
}

impl Reader {
    /// `term + term + …`, left to right as T-SQL evaluates it.
    fn expr(&mut self, at: &SyntaxNode, ts: &[Tok]) -> Result<Opt, String> {
        let mut terms: Vec<&[Tok]> = Vec::new();
        let mut depth = 0usize;
        let mut start = 0;
        for (i, t) in ts.iter().enumerate() {
            match t.kind {
                K::LParen => depth += 1,
                K::RParen => depth = depth.checked_sub(1).ok_or("unbalanced parentheses")?,
                K::Op if depth == 0 && t.text == "+" => {
                    terms.push(&ts[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }
        terms.push(&ts[start..]);
        let mut acc: Option<Opt> = None;
        for term in terms {
            let v = self.term(at, term)?;
            acc = Some(match acc {
                None => v,
                Some(a) => concat(a, v)?,
            });
        }
        acc.ok_or_else(|| "empty text".into())
    }

    fn term(&mut self, at: &SyntaxNode, ts: &[Tok]) -> Result<Opt, String> {
        let shown = || {
            ts.iter()
                .map(|t| t.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        match ts {
            [] => Err("empty term in the text".into()),
            [t] if t.kind == K::Str => {
                let unicode = t.text.starts_with(['N', 'n']);
                let s = string_value(&t.text)?;
                bmp(&s)?;
                if !unicode {
                    ascii(&s)?;
                }
                let max = len(&s) > if unicode { 4000 } else { 8000 };
                Ok(Opt {
                    s: Some(s),
                    unicode,
                    max,
                })
            }
            [t] if t.kind == K::Word && t.up == "NULL" => Ok(Opt {
                s: None,
                unicode: false,
                max: false,
            }),
            [t] if t.kind == K::Var && !t.text.starts_with("@@") => {
                let name = t.text[1..].to_lowercase();
                self.var(at, &name)
            }
            [f, open, inner @ .., close]
                if f.kind == K::Word && open.kind == K::LParen && close.kind == K::RParen =>
            {
                match (f.up.as_str(), inner) {
                    ("NCHAR" | "CHAR", [n]) if n.kind == K::Num => {
                        let code: u32 = n
                            .text
                            .parse()
                            .map_err(|_| format!("`{}` is not a character code", n.text))?;
                        let unicode = f.up == "NCHAR";
                        let ok = if unicode {
                            (1..0xD800).contains(&code) || (0xE000..=0xFFFF).contains(&code)
                        } else {
                            (1..128).contains(&code)
                        };
                        let c = char::from_u32(code).filter(|_| ok).ok_or_else(|| {
                            format!(
                                "{}({code}) depends on the code page or is not one character",
                                f.up
                            )
                        })?;
                        Ok(Opt {
                            s: Some(c.to_string()),
                            unicode,
                            max: false,
                        })
                    }
                    ("QUOTENAME", arg) if !arg.iter().any(|t| t.kind == K::Comma) => {
                        let v = self.expr(at, arg)?;
                        // nvarchar(258); NULL for more than 128 characters.
                        let s =
                            v.s.filter(|s| len(s) <= 128)
                                .map(|s| format!("[{}]", s.replace(']', "]]")));
                        Ok(Opt {
                            s,
                            unicode: true,
                            max: false,
                        })
                    }
                    _ => Err(format!("the text is built with `{}`", shown())),
                }
            }
            [open, inner @ .., close] if open.kind == K::LParen && close.kind == K::RParen => {
                self.expr(at, inner)
            }
            _ => Err(format!("the text is built with `{}`", shown())),
        }
    }

    /// The value variable `name` holds when statement `at` starts.
    fn var(&mut self, at: &SyntaxNode, name: &str) -> Result<Opt, String> {
        self.depth += 1;
        if self.depth > DEPTH {
            return Err("the text is built over too many assignments".into());
        }
        let decl = declared(at, name)?;
        let v = self.before(at, name)?;
        // Stored in the variable: converted to its type.
        let s = match v.s {
            Some(s) => {
                bmp(&s)?;
                if !decl.unicode {
                    ascii(&s)?;
                }
                Some(match decl.len {
                    Some(n) if len(&s) > n => s.chars().take(n).collect(),
                    _ => s,
                })
            }
            None => None,
        };
        Ok(Opt {
            s,
            unicode: decl.unicode,
            max: decl.len.is_none(),
        })
    }

    /// Walks back from `at` to the assignment of `name` that reaches it.
    fn before(&mut self, at: &SyntaxNode, name: &str) -> Result<Opt, String> {
        let mut cur = at.clone();
        loop {
            let mut prev = cur.prev_sibling();
            while let Some(p) = prev {
                if mentions(&p, name) && (p.kind() == SyntaxKind::STATEMENT || writes(&p, name)) {
                    return self.assigned(&p, name);
                }
                prev = p.prev_sibling();
            }
            let parent = cur
                .parent()
                .ok_or_else(|| format!("@{name} is not declared before the statement"))?;
            match parent.kind() {
                SyntaxKind::BLOCK | SyntaxKind::TRY_BLOCK => {}
                // An IF's condition reads, it does not assign; continue
                // before the IF.
                SyntaxKind::BRANCH => {
                    let owner = parent.parent().ok_or("a branch without a statement")?;
                    if owner.kind() != SyntaxKind::IF_STMT {
                        return Err(format!(
                            "@{name} is read in a loop, where an earlier pass may have changed it"
                        ));
                    }
                    cur = owner;
                    continue;
                }
                SyntaxKind::TRY_CATCH => {}
                SyntaxKind::CATCH_BLOCK => {
                    return Err(format!(
                        "@{name} is read in a CATCH block, after an unknown part of the TRY ran"
                    ))
                }
                SyntaxKind::LOOP_STMT => {
                    return Err(format!(
                        "@{name} is read in a loop, where an earlier pass may have changed it"
                    ))
                }
                SyntaxKind::ROUTINE_BODY
                | SyntaxKind::BATCH
                | SyntaxKind::SOURCE_FILE
                | SyntaxKind::ROUTINE => {
                    return Err(format!(
                        "@{name} gets its value from outside the code (a parameter or the caller)"
                    ))
                }
                k => {
                    return Err(format!(
                        "@{name} is read inside {k:?}, which is not followed"
                    ))
                }
            }
            cur = parent;
        }
    }

    /// The value statement `p`, which mentions `name`, leaves in it.
    fn assigned(&mut self, p: &SyntaxNode, name: &str) -> Result<Opt, String> {
        if p.kind() != SyntaxKind::STATEMENT {
            return Err(format!(
                "@{name} may be changed inside the {} before the statement",
                match p.kind() {
                    SyntaxKind::IF_STMT => "IF",
                    SyntaxKind::LOOP_STMT => "loop",
                    SyntaxKind::TRY_CATCH => "TRY/CATCH",
                    _ => "block",
                }
            ));
        }
        let ts = tokens(p);
        let is_var = |t: &Tok| t.kind == K::Var && t.text[1..].eq_ignore_ascii_case(name);
        match ts.as_slice() {
            [set, v, op, rest @ ..] if set.kind == K::Word && set.up == "SET" && is_var(v) => {
                match op.text.as_str() {
                    "=" => self.expr(p, rest),
                    "+=" => {
                        let old = self.var(p, name)?;
                        let add = self.expr(p, rest)?;
                        concat(old, add)
                    }
                    _ => Err(format!("@{name} is changed with `{}`", op.text)),
                }
            }
            [d, ..] if d.kind == K::Word && d.up == "DECLARE" => {
                let (_, init) = declarator(&ts[1..], name)
                    .ok_or_else(|| format!("@{name} is declared in a form not read"))?;
                match init {
                    Some(init) => self.expr(p, init),
                    // DECLARE runs no code: without an initial value the
                    // variable is NULL from the start of the batch, and no
                    // loop lies between (`before` stops at loops).
                    None => Ok(Opt {
                        s: None,
                        unicode: false,
                        max: false,
                    }),
                }
            }
            // The text run again, or printed: that does not change it.
            _ if reads_only(p, name) => self.before(p, name),
            _ => Err(format!(
                "@{name} may be changed by the statement at byte {}",
                cst::sig_range(p).0
            )),
        }
    }
}

/// `a + b` for strings: NULL if either is NULL, cut to 4000 / 8000
/// characters unless one side is `max`.
fn concat(a: Opt, b: Opt) -> Result<Opt, String> {
    let unicode = a.unicode || b.unicode;
    let max = a.max || b.max;
    let s = match (a.s, b.s) {
        (Some(x), Some(y)) => Some(x + &y),
        _ => None,
    };
    if let Some(s) = &s {
        if !max && len(s) > if unicode { 4000 } else { 8000 } {
            return Err("the concatenation is longer than T-SQL keeps without a max type".into());
        }
    }
    Ok(Opt { s, unicode, max })
}

/// A statement that mentions `@name` only to read it in a way this module
/// knows: running it as dynamic SQL (no OUTPUT) or printing it.
fn reads_only(p: &SyntaxNode, name: &str) -> bool {
    let ts = tokens(p);
    let output = ts
        .iter()
        .any(|t| t.kind == K::Word && matches!(t.up.as_str(), "OUTPUT" | "OUT"));
    (is_tsql(p) && !output && exec_mentions_only_text(&ts, name))
        || ts
            .first()
            .is_some_and(|t| t.kind == K::Word && t.up == "PRINT")
}

/// In a dynamic SQL statement, `@name` appears only in the text argument.
fn exec_mentions_only_text(ts: &[Tok], name: &str) -> bool {
    let range = match exec_form(ts) {
        Some(Form::Paren(s, e)) | Some(Form::ExecuteSql(s, e)) => (s, e),
        None => return false,
    };
    ts.iter().enumerate().all(|(i, t)| {
        !(t.kind == K::Var && t.text[1..].eq_ignore_ascii_case(name))
            || (range.0..range.1).contains(&i)
    })
}

/// A compound statement (IF, loop, TRY/CATCH, block) holds a statement that
/// may change `@name`: one that mentions it other than to read it.
fn writes(n: &SyntaxNode, name: &str) -> bool {
    n.descendants()
        .filter(|d| d.kind() == SyntaxKind::STATEMENT)
        .any(|d| mentions(&d, name) && !reads_only(&d, name))
        // A label or a statement form not lowered as STATEMENT.
        || n.descendants().any(|d| matches!(d.kind(), SyntaxKind::ERROR | SyntaxKind::CASE_STMT | SyntaxKind::HANDLER))
}

/// Any token of `n` is variable `@name`.
fn mentions(n: &SyntaxNode, name: &str) -> bool {
    cst::sig_tokens(n)
        .any(|t| t.kind() == SyntaxKind::VARIABLE && t.text()[1..].eq_ignore_ascii_case(name))
}

/// In `DECLARE` tokens after the keyword: the type and initial value of
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

/// The string type `@name` is declared with in the routine or batch that
/// holds `at`: exactly one declaration.
fn declared(at: &SyntaxNode, name: &str) -> Result<Decl, String> {
    let scope = at
        .ancestors()
        .find(|n| {
            matches!(
                n.kind(),
                SyntaxKind::ROUTINE | SyntaxKind::BATCH | SyntaxKind::SOURCE_FILE
            )
        })
        .ok_or("no routine or batch")?;
    let mut found = None;
    for n in scope
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::STATEMENT)
    {
        let ts = tokens(&n);
        if !ts
            .first()
            .is_some_and(|t| t.kind == K::Word && t.up == "DECLARE")
        {
            continue;
        }
        if let Some((ty, _)) = declarator(&ts[1..], name) {
            if found.is_some() {
                return Err(format!("@{name} is declared more than once"));
            }
            found = Some(string_type(ty).ok_or_else(|| {
                format!(
                    "@{name} is declared as `{}`, not a string type this reading follows",
                    ty.iter()
                        .map(|t| t.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            })?);
        }
    }
    found.ok_or_else(|| format!("@{name} is a parameter or not declared in the code"))
}

/// `nvarchar(n|max)`, `varchar(n|max)`, `sysname`. A length left out is 1,
/// as in a T-SQL declaration.
fn string_type(ty: &[Tok]) -> Option<Decl> {
    let name = ty.first().filter(|t| t.kind == K::Word)?;
    let unicode = match name.up.as_str() {
        "SYSNAME" if ty.len() == 1 => {
            return Some(Decl {
                unicode: true,
                len: Some(128),
            })
        }
        "NVARCHAR" => true,
        "VARCHAR" => false,
        _ => return None,
    };
    let len = match &ty[1..] {
        [] => Some(1),
        [o, n, c] if o.kind == K::LParen && c.kind == K::RParen => {
            if n.kind == K::Word && n.up == "MAX" {
                None
            } else if n.kind == K::Num {
                Some(n.text.parse().ok()?)
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(Decl { unicode, len })
}

/// PostgreSQL PL/pgSQL `EXECUTE '<text>'`: the statement it runs, when the
/// text is a plain string constant with nothing after it (no INTO, no USING).
/// `None` when the statement is not an `EXECUTE`.
pub fn pg_text(stmt: &SyntaxNode) -> Option<Result<String, String>> {
    let toks: Vec<_> = cst::sig_tokens(stmt)
        .filter(|t| t.kind() != SyntaxKind::SEMICOLON)
        .collect();
    let first = toks.first()?;
    if !(first.kind() == SyntaxKind::IDENT && first.text().eq_ignore_ascii_case("EXECUTE")) {
        return None;
    }
    // `EXECUTE '…' USING a, b`: the values are read by `pg_using`.
    let using = toks
        .iter()
        .position(|t| t.kind() == SyntaxKind::IDENT && t.text().eq_ignore_ascii_case("USING"));
    let toks = match using {
        Some(u) if u + 1 < toks.len() => &toks[..u],
        _ => &toks[..],
    };
    Some(match toks {
        [_, s] if s.kind() == SyntaxKind::STRING => {
            let raw = s.text();
            if !raw.starts_with('\'') {
                Err(format!(
                    "the text is a `{}` string",
                    &raw[..raw.find('\'').unwrap_or(0)]
                ))
            } else {
                string_value(raw)
            }
        }
        [_, s] if s.kind() == SyntaxKind::DOLLAR_STRING => {
            let raw = s.text();
            let tag_end = raw[1..].find('$').map(|i| i + 2).unwrap_or(raw.len());
            let tag = &raw[..tag_end];
            raw.get(tag_end..raw.len().saturating_sub(tag.len()))
                .filter(|_| raw.len() >= 2 * tag.len() && raw.ends_with(tag))
                .map(str::to_string)
                .ok_or_else(|| "unterminated dollar-quoted string".into())
        }
        _ => Err("EXECUTE of an expression, or with INTO".into()),
    })
}

/// The values of PL/pgSQL `EXECUTE '…' USING a, b`, in order; empty
/// without USING.
pub fn pg_using(stmt: &SyntaxNode) -> Vec<Vec<Tok>> {
    let ts: Vec<Tok> = tokens(stmt)
        .into_iter()
        .filter(|t| t.kind != K::Semi)
        .collect();
    match ts.iter().position(|t| t.kind == K::Word && t.up == "USING") {
        Some(u) if u == 2 => split_top(&ts[u + 1..])
            .into_iter()
            .map(<[Tok]>::to_vec)
            .collect(),
        _ => Vec::new(),
    }
}

/// The name the comparison gives PostgreSQL `$n` in a procedure body.
pub const PG_PARAM: &str = "sqldiff_dynamic_";

/// The PostgreSQL text reads `$n` parameters.
pub fn pg_has_params(root: &SyntaxNode) -> bool {
    root.descendants_with_tokens()
        .any(|e| e.kind() == SyntaxKind::PARAM)
}

/// The PostgreSQL text with each `$n` (1 ≤ n ≤ `count`) renamed to
/// `PG_PARAM` + n, to run as a procedure body taking them.
pub fn pg_bind_params(root: &SyntaxNode, count: usize) -> Result<String, String> {
    let mut out = String::new();
    for e in root.descendants_with_tokens() {
        let Some(t) = e.as_token() else { continue };
        if t.text().to_ascii_lowercase().contains(PG_PARAM) {
            return Err(format!("the PostgreSQL text uses the name {PG_PARAM}"));
        }
        if t.kind() == SyntaxKind::PARAM {
            let n: usize = t.text()[1..]
                .parse()
                .map_err(|_| format!("the parameter {} is not read", t.text()))?;
            if n == 0 || n > count {
                return Err(format!(
                    "the PostgreSQL text reads {}, and EXECUTE passes {count} value(s)",
                    t.text()
                ));
            }
            out.push_str(&format!("{PG_PARAM}{n}"));
        } else {
            out.push_str(t.text());
        }
    }
    Ok(out)
}

/// Why the T-SQL text of a dynamic batch cannot be compared as one
/// statement against a PostgreSQL `EXECUTE`, if it cannot: more or less than
/// one statement, the caller's variables, `#temp` tables (dropped when the
/// batch ends), result sets (PostgreSQL `EXECUTE` discards them), session
/// settings, database switches, transaction control and batch-level
/// statements.
pub fn tsql_inner_refusal(text: &str) -> Option<String> {
    tsql_inner_refusal_with(text, &[])
}

/// `tsql_inner_refusal`, where the text may read the parameters `params`
/// (lower case, without `@`) that `sp_executesql` passes.
pub fn tsql_inner_refusal_with(text: &str, params: &[String]) -> Option<String> {
    let p = sqldiff_syntax::parse(text, Dialect::Tsql);
    if !p.diagnostics.is_empty() {
        return Some("the text does not parse as T-SQL".into());
    }
    let root = p.syntax();
    let units: Vec<SyntaxNode> = root
        .descendants()
        .filter(|n| {
            n.kind().is_unit()
                && n.parent().is_some_and(|p| {
                    matches!(p.kind(), SyntaxKind::SOURCE_FILE | SyntaxKind::BATCH)
                })
        })
        .collect();
    let [stmt] = units.as_slice() else {
        return Some(format!(
            "the text holds {} statements: only one is compared, since without XACT_ABORT a failing statement lets the next one run",
            units.len()
        ));
    };
    if stmt.kind() != SyntaxKind::STATEMENT {
        return Some("the text holds control flow or a routine".into());
    }
    if root
        .descendants_with_tokens()
        .any(|e| e.kind() == SyntaxKind::BATCH_SEP)
    {
        return Some("the text holds GO".into());
    }
    let ts = tokens(stmt);
    if let Some(v) = ts
        .iter()
        .find(|t| t.kind == K::Var && !params.contains(&t.text[1..].to_lowercase()))
    {
        return Some(format!(
            "the text reads {}, which the dynamic batch cannot see",
            v.text
        ));
    }
    if ts
        .iter()
        .any(|t| matches!(t.kind, K::Word | K::Quoted) && cst::unquote(&t.text).starts_with('#'))
    {
        return Some(
            "the text uses a #temp table, which T-SQL drops when the dynamic batch ends".into(),
        );
    }
    let w = |i: usize| ts.get(i).map(|t| t.up.as_str()).unwrap_or("");
    match w(0) {
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "CREATE" | "ALTER" | "DROP" | "TRUNCATE"
        | "EXEC" | "EXECUTE" | "GRANT" | "REVOKE" | "DENY" => {}
        "SELECT" | "WITH" => {
            return Some("the text is a query: T-SQL returns its rows to the client, PostgreSQL EXECUTE discards them".into())
        }
        w => return Some(format!("the text is a `{w}` statement, which is not compared inside dynamic SQL")),
    }
    if ts.iter().any(|t| t.kind == K::Word && t.up == "OUTPUT") {
        return Some("the text has an OUTPUT clause, which returns rows".into());
    }
    if is_tsql(stmt) {
        return Some("the text runs dynamic SQL itself".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec_at(src: &str, nth: usize) -> SyntaxNode {
        let root = sqldiff_syntax::parse(src, Dialect::Tsql).syntax();
        root.descendants()
            .filter(is_tsql)
            .nth(nth)
            .expect("dynamic SQL statement")
    }

    fn text(src: &str) -> Text {
        tsql_text(&exec_at(src, 0))
    }

    fn proc(body: &str) -> String {
        format!("CREATE PROCEDURE dbo.p @p sysname AS\nBEGIN\n{body}\nEND\n")
    }

    fn known(s: &str) -> Text {
        Text::Known(s.into())
    }

    fn unknown(t: &Text, part: &str) -> bool {
        matches!(t, Text::Unknown(w) if w.contains(part))
    }

    #[test]
    fn literal() {
        assert_eq!(
            text(&proc("EXEC (N'DROP TABLE dbo.t');")),
            known("DROP TABLE dbo.t")
        );
        assert_eq!(
            text(&proc("EXECUTE ('DROP ' + N'TABLE dbo.t');")),
            known("DROP TABLE dbo.t")
        );
    }

    #[test]
    fn variable_set_before() {
        let src = proc("DECLARE @s nvarchar(max);\nSET @s = N'DROP TABLE dbo.t;';\nEXECUTE (@s);");
        assert_eq!(text(&src), known("DROP TABLE dbo.t;"));
        let src = proc("DECLARE @s nvarchar(max) = N'DROP TABLE dbo.t;';\nEXEC sp_executesql @s;");
        assert_eq!(text(&src), known("DROP TABLE dbo.t;"));
        let src = proc("DECLARE @s nvarchar(max) = N'x';\nEXEC sys.sp_executesql @stmt = @s;");
        assert_eq!(text(&src), known("x"));
    }

    #[test]
    fn last_assignment_wins_and_reruns_do_not_change() {
        let src = proc(
            "DECLARE @s nvarchar(max) = N'a';\nSET @s = N'b';\nEXEC (@s);\nPRINT @s;\nSET @s += N'c';\nEXEC (@s);",
        );
        assert_eq!(tsql_text(&exec_at(&src, 0)), known("b"));
        assert_eq!(tsql_text(&exec_at(&src, 1)), known("bc"));
    }

    #[test]
    fn through_if_and_try_but_not_loops_or_catch() {
        let src = proc(
            "DECLARE @s nvarchar(max) = N'a';\nIF @p IS NULL\nBEGIN\n  EXEC (@s);\nEND\nBEGIN TRY\n  EXEC (@s);\nEND TRY\nBEGIN CATCH\n  EXEC (@s);\nEND CATCH\nWHILE 1 = 1\nBEGIN\n  EXEC (@s);\n  BREAK;\nEND",
        );
        assert_eq!(tsql_text(&exec_at(&src, 0)), known("a"));
        assert_eq!(tsql_text(&exec_at(&src, 1)), known("a"));
        assert!(unknown(&tsql_text(&exec_at(&src, 2)), "CATCH"));
        assert!(unknown(&tsql_text(&exec_at(&src, 3)), "loop"));
    }

    #[test]
    fn an_if_that_may_assign_stops_the_reading() {
        let src =
            proc("DECLARE @s nvarchar(max) = N'a';\nIF @p IS NULL SET @s = N'b';\nEXEC (@s);");
        assert!(unknown(&text(&src), "IF"));
    }

    #[test]
    fn parameters_tables_and_other_writes_are_unknown() {
        assert!(unknown(&text(&proc("EXEC (@p);")), "parameter"));
        let src = proc("DECLARE @s nvarchar(max);\nSELECT @s = name FROM dbo.t;\nEXEC (@s);");
        assert!(unknown(&text(&src), "may be changed"));
        let src = proc("DECLARE @s nvarchar(max) = N'DROP TABLE ' + @p;\nEXEC (@s);");
        assert!(unknown(&text(&src), "parameter"));
        let src = proc("DECLARE @s nvarchar(max) = N'x';\nEXEC dbo.q @s OUTPUT;\nEXEC (@s);");
        assert!(unknown(&text(&src), "may be changed"));
    }

    #[test]
    fn null_text_and_null_concatenation() {
        let src = proc("DECLARE @s nvarchar(max);\nEXEC (@s);");
        assert_eq!(text(&src), Text::Null);
        let src =
            proc("DECLARE @s nvarchar(max), @t nvarchar(10);\nSET @s = N'a' + @t;\nEXEC (@s);");
        assert_eq!(text(&src), Text::Null);
    }

    #[test]
    fn declared_length_cuts_the_value() {
        let src = proc("DECLARE @s nvarchar(5) = N'abcdefgh';\nEXEC (@s);");
        assert_eq!(text(&src), known("abcde"));
        // A length left out is 1.
        let src = proc("DECLARE @s nvarchar = N'abc';\nEXEC (@s);");
        assert_eq!(text(&src), known("a"));
        let src = proc("DECLARE @s int = 1;\nEXEC (@s);");
        assert!(unknown(&text(&src), "not a string type"));
    }

    #[test]
    fn long_concatenation_without_max_is_unknown() {
        let a = "a".repeat(3000);
        let src = proc(&format!("EXEC (N'{a}' + N'{a}');"));
        assert!(unknown(&text(&src), "longer"));
        let src = proc(&format!(
            "DECLARE @s nvarchar(max) = N'';\nSET @s = @s + N'{a}' + N'{a}';\nEXEC (@s);"
        ));
        assert!(matches!(text(&src), Text::Known(s) if s.len() == 6000));
    }

    #[test]
    fn quotename_nchar_and_varchar() {
        let src = proc(
            "DECLARE @n sysname = N'a]b';\nDECLARE @s nvarchar(max) = N'DROP TABLE ' + QUOTENAME(@n) + NCHAR(13) + NCHAR(10);\nEXEC (@s);",
        );
        assert_eq!(text(&src), known("DROP TABLE [a]]b]\r\n"));
        let src = proc("DECLARE @s varchar(max) = N'é';\nEXEC (@s);");
        assert!(unknown(&text(&src), "ASCII"));
        let src = proc("DECLARE @s varchar(max) = 'x';\nEXEC sp_executesql @s;");
        assert!(unknown(&text(&src), "rejects"));
    }

    #[test]
    fn goto_and_concat_null_setting_stop_the_reading() {
        let src = proc("DECLARE @s nvarchar(max) = N'x';\nl:\nEXEC (@s);\nGOTO l;");
        assert!(unknown(&text(&src), "GOTO"));
        let src = proc("SET CONCAT_NULL_YIELDS_NULL OFF;\nEXEC (N'x');");
        assert!(unknown(&text(&src), "CONCAT_NULL"));
    }

    #[test]
    fn not_dynamic() {
        let root =
            sqldiff_syntax::parse(&proc("EXEC @r = dbo.q;\nEXEC dbo.q;"), Dialect::Tsql).syntax();
        assert!(!root.descendants().any(|n| is_tsql(&n)));
    }

    #[test]
    fn pg_execute() {
        let root = sqldiff_syntax::parse(
            "DO $$\nBEGIN\n  EXECUTE 'DROP TABLE t';\n  EXECUTE $q$DROP TABLE 'x'$q$;\n  EXECUTE v_sql;\nEND\n$$;",
            Dialect::Postgres,
        )
        .syntax();
        let got: Vec<_> = root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::STATEMENT)
            .filter_map(|n| pg_text(&n))
            .collect();
        assert_eq!(got[0], Ok("DROP TABLE t".into()));
        assert_eq!(got[1], Ok("DROP TABLE 'x'".into()));
        assert!(got[2].is_err());
    }

    #[test]
    fn inner_refusals() {
        assert_eq!(tsql_inner_refusal("DROP TABLE dbo.t;"), None);
        assert!(tsql_inner_refusal("DROP TABLE dbo.t; DROP TABLE dbo.u;")
            .is_some_and(|w| w.contains("2 statements")));
        assert!(tsql_inner_refusal("SELECT 1").is_some_and(|w| w.contains("query")));
        assert!(tsql_inner_refusal("INSERT #t VALUES (1)").is_some_and(|w| w.contains("#temp")));
        assert!(tsql_inner_refusal("UPDATE dbo.t SET a = @x").is_some_and(|w| w.contains("@x")));
        assert!(tsql_inner_refusal("SET NOCOUNT ON").is_some());
        assert!(tsql_inner_refusal("USE master").is_some());
        assert_eq!(
            tsql_inner_refusal_with("UPDATE dbo.t SET a = @V", &["v".into()]),
            None
        );
    }

    fn params(src: &str) -> Result<Option<Passed>, String> {
        exec_params(&exec_at(src, 0))
    }

    fn names(p: &Passed) -> Vec<String> {
        p.args
            .iter()
            .map(|a| {
                a.iter()
                    .map(|t| t.text.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    #[test]
    fn sp_executesql_parameters() {
        let p = params(&proc(
            "EXEC sp_executesql N'x', N'@a int, @B nvarchar(10)', @b = @p, @a = @q;",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(
            p.params.iter().map(|q| q.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(names(&p), ["@q", "@p"]);
        assert_eq!(p.params[1].ty.len(), 4);
        // Positional; a variable holding the list.
        let p = params(&proc("DECLARE @l nvarchar(100) = N'@a int';\nEXEC sp_executesql @stmt = N'x', @params = @l, @p;"))
            .unwrap()
            .unwrap();
        assert_eq!(names(&p), ["@p"]);
        assert!(params(&proc("EXEC sp_executesql N'x';")).unwrap().is_none());
        for (call, why) in [
            ("N'x', N'@a int', @a = @p OUTPUT", "OUTPUT"),
            ("N'x', N'@a int OUTPUT', @a = @p", "OUTPUT"),
            ("N'x', N'@a int = 1'", "default"),
            ("N'x', N'@a int, @b int', @a = @p", "not passed"),
            ("N'x', N'@a int', @a = @p, @a = @p", "twice"),
            ("N'x', N'@a int', @c = @p", "not declared"),
            ("N'x', N'@a int', @p, @p", "more values"),
            ("N'x', '@a int', @p", "varchar"),
            ("N'x', N'@a int', @a = @p, @p", "positional"),
        ] {
            let r = params(&proc(&format!("EXEC sp_executesql {call};")));
            assert!(r.as_ref().is_err_and(|e| e.contains(why)), "{call}: {r:?}");
        }
    }

    #[test]
    fn declared_types() {
        let src =
            proc("DECLARE @a int, @b nvarchar (20);\nEXEC sp_executesql N'x', N'@v int', @v = @a;");
        let at = exec_at(&src, 0);
        let up = |ts: Vec<Tok>| {
            ts.iter()
                .map(|t| t.up.clone())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(declared_type(&at, "a").map(up).as_deref(), Some("INT"));
        assert_eq!(
            declared_type(&at, "B").map(up).as_deref(),
            Some("NVARCHAR ( 20 )")
        );
        assert_eq!(declared_type(&at, "p").map(up).as_deref(), Some("SYSNAME"));
        assert!(declared_type(&at, "zz").is_none());
    }

    #[test]
    fn pg_using_and_parameters() {
        let root = sqldiff_syntax::parse(
            "DO $$ BEGIN EXECUTE 'UPDATE t SET a = $1 WHERE id = $2' USING v_a, p_x; END $$;",
            Dialect::Postgres,
        )
        .syntax();
        let ex = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::STATEMENT && pg_text(n).is_some())
            .unwrap();
        assert_eq!(
            pg_text(&ex),
            Some(Ok("UPDATE t SET a = $1 WHERE id = $2".into()))
        );
        assert_eq!(pg_using(&ex).len(), 2);
        let body =
            sqldiff_syntax::parse("UPDATE t SET a = $1 WHERE id = $2", Dialect::Postgres).syntax();
        assert!(pg_has_params(&body));
        assert_eq!(
            pg_bind_params(&body, 2).unwrap(),
            "UPDATE t SET a = sqldiff_dynamic_1 WHERE id = sqldiff_dynamic_2"
        );
        assert!(pg_bind_params(&body, 1).is_err_and(|e| e.contains("$2")));
    }
}
