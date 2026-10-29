//! Writes the abstract syntax (`abstract_syntax`) back as text, for the
//! converter's target dialect (docs/verified-conversion.md, C0 item 1).
//!
//! The printed text must mean what the tree says, in the target engine, so:
//! every operand that is not an atom is parenthesised (the text never relies
//! on a precedence table, the parser's or the engine's); a name is written
//! unquoted only when the engine reads that spelling back as the same name;
//! and anything the target cannot spell faithfully is an error, never a
//! guess. The converter's re-parse check (`parse(print(s)) == s`) is what
//! makes the printer trusted; this module only has to be right often.
//!
//! PostgreSQL only for now. String literals are written in the standard
//! form, which reads backslashes literally under
//! `standard_conforming_strings = on` (the default since PostgreSQL 9.1).

use crate::abstract_syntax::{
    is_clause_start, is_reserved, Call, ConflictAction, Expr, From, InsertSource, MergeAction,
    OrderItem, SelItem, Select, Stmt, Type, When,
};
use crate::Dialect;

pub type PrintResult = Result<String, String>;

/// The statement as `d` spells it, without a terminator.
pub fn stmt(s: &Stmt, d: Dialect) -> PrintResult {
    P::new(d)?.stmt(s)
}

/// The expression as `d` spells it.
pub fn expr(e: &Expr, d: Dialect) -> PrintResult {
    P::new(d)?.expr(e)
}

/// A query as `d` spells it.
pub fn select(q: &Select, d: Dialect) -> PrintResult {
    P::new(d)?.select(q)
}

/// The printer for one target dialect (PostgreSQL).
struct P;

fn no<T>(what: &str) -> Result<T, String> {
    Err(format!("{what} cannot be written in PostgreSQL"))
}

fn plain_word(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// Whether a single expression needs no parentheses wherever it stands.
fn atom(e: &Expr) -> bool {
    match e {
        Expr::Name(_)
        | Expr::Var(_)
        | Expr::Str(_)
        | Expr::Null
        | Expr::Star(_)
        | Expr::Call(_)
        | Expr::Cast(..)
        | Expr::Case(..)
        | Expr::Exists(_)
        | Expr::Subquery(_)
        | Expr::Tuple(_)
        | Expr::Special(_)
        | Expr::Extract(..) => true,
        // `-1` is read back as the literal, `--1` would be a comment.
        Expr::Num(n) => !n.starts_with('-'),
        _ => false,
    }
}

impl P {
    fn new(d: Dialect) -> Result<P, String> {
        match d {
            Dialect::Postgres => Ok(P),
            _ => Err(format!(
                "printing {} is not supported yet",
                d.display_name()
            )),
        }
    }

    // ---- names -------------------------------------------------------------

    /// One part of a name. A part with a leading `"` is a quoted identifier
    /// whose case matters (`abstract_syntax::quoted_name`); any other part is
    /// written as it was read, unquoted when the lexer reads it back as one
    /// word, else quoted when quoting keeps its meaning.
    fn part(&self, s: &str, first: bool) -> PrintResult {
        if let Some(q) = s.strip_prefix('"') {
            return Ok(format!("\"{}\"", q.replace('"', "\"\"")));
        }
        let up = s.to_uppercase();
        if plain_word(s) && !(first && (is_reserved(&up) || is_clause_start(&up))) {
            return Ok(s.to_string());
        }
        // Quoting keeps the name only if folding would not have changed it.
        if s == s.to_lowercase() && !s.is_empty() {
            return Ok(format!("\"{}\"", s.replace('"', "\"\"")));
        }
        Err(format!("the name `{s}` cannot be written in PostgreSQL"))
    }

    fn name(&self, parts: &[String]) -> PrintResult {
        if parts.is_empty() {
            return Err("an empty name".into());
        }
        let mut out = Vec::new();
        for (i, p) in parts.iter().enumerate() {
            if p.starts_with('#') || p.starts_with('@') {
                return no(&format!("the T-SQL name `{p}`"));
            }
            out.push(self.part(p, i == 0)?);
        }
        Ok(out.join("."))
    }

    fn ident(&self, s: &str) -> PrintResult {
        self.part(s, true)
    }

    fn idents(&self, v: &[String]) -> PrintResult {
        Ok(v.iter()
            .map(|c| self.ident(c))
            .collect::<Result<Vec<_>, _>>()?
            .join(", "))
    }

    fn ty(&self, t: &Type) -> PrintResult {
        if !t
            .name
            .split(' ')
            .all(|w| plain_word(w) && w == w.to_uppercase())
        {
            return Err(format!("the type name `{}`", t.name));
        }
        if t.args.is_empty() {
            Ok(t.name.clone())
        } else {
            Ok(format!("{}({})", t.name, t.args.join(", ")))
        }
    }

    // ---- expressions -------------------------------------------------------

    fn expr(&self, e: &Expr) -> PrintResult {
        Ok(match e {
            Expr::Name(parts) => self.name(parts)?,
            Expr::Var(v) => return no(&format!("the T-SQL variable `{v}`")),
            Expr::Num(n) => {
                if let Some(rest) = n.strip_prefix('-') {
                    // `--5`, which the parser builds from `- -5`.
                    if rest.starts_with('-') {
                        format!("- {}", self.expr(&Expr::Num(rest.to_string()))?)
                    } else {
                        n.clone()
                    }
                } else {
                    n.clone()
                }
            }
            Expr::Str(s) => format!("'{}'", s.replace('\'', "''")),
            Expr::Null => "NULL".into(),
            Expr::Star(None) => "*".into(),
            Expr::Star(Some(q)) => {
                let parts: Vec<String> = q.split('.').map(str::to_string).collect();
                format!("{}.*", self.name(&parts)?)
            }
            Expr::Call(c) => self.call(c)?,
            Expr::Bin(op, l, r) => {
                let op = match op.as_str() {
                    "AND" | "OR" | "=" | "<>" | "<" | ">" | "<=" | ">=" | "+" | "-" | "*" | "/"
                    | "%" | "||" => op.as_str(),
                    _ => return no(&format!("the operator `{op}`")),
                };
                format!("{} {op} {}", self.operand(l)?, self.operand(r)?)
            }
            Expr::Unary(op, x) => match op.as_str() {
                "NOT" => format!("NOT {}", self.operand(x)?),
                "-" => format!("- {}", self.operand(x)?),
                _ => return no(&format!("the operator `{op}`")),
            },
            Expr::IsNull(x, neg) => format!(
                "{} IS {}NULL",
                self.operand(x)?,
                if *neg { "NOT " } else { "" }
            ),
            Expr::InList(x, list, neg) => format!(
                "{} {}IN ({})",
                self.operand(x)?,
                if *neg { "NOT " } else { "" },
                self.list(list)?
            ),
            Expr::InQuery(x, q, neg) => format!(
                "{} {}IN ({})",
                self.operand(x)?,
                if *neg { "NOT " } else { "" },
                self.select(q)?
            ),
            Expr::Between(x, lo, hi, neg) => format!(
                "{} {}BETWEEN {} AND {}",
                self.operand(x)?,
                if *neg { "NOT " } else { "" },
                self.operand(lo)?,
                self.operand(hi)?
            ),
            Expr::Like(x, p, neg, ci) => format!(
                "{} {}{} {}",
                self.operand(x)?,
                if *neg { "NOT " } else { "" },
                if *ci { "ILIKE" } else { "LIKE" },
                self.operand(p)?
            ),
            Expr::Cast(x, t) => format!("CAST({} AS {})", self.expr(x)?, self.ty(t)?),
            Expr::Case(op, whens, els) => {
                let mut s = "CASE".to_string();
                if let Some(o) = op {
                    s.push(' ');
                    s.push_str(&self.expr(o)?);
                }
                for (w, t) in whens {
                    s.push_str(&format!(" WHEN {} THEN {}", self.expr(w)?, self.expr(t)?));
                }
                if let Some(e) = els {
                    s.push_str(&format!(" ELSE {}", self.expr(e)?));
                }
                s.push_str(" END");
                s
            }
            Expr::Exists(q) => format!("EXISTS ({})", self.select(q)?),
            Expr::Subquery(q) => format!("({})", self.select(q)?),
            Expr::Tuple(v) if v.len() >= 2 => format!("({})", self.list(v)?),
            Expr::Tuple(_) => return Err("a tuple of fewer than two values".into()),
            Expr::Special(s) => match s.as_str() {
                "CURRENT_TIMESTAMP" | "CURRENT_DATE" | "CURRENT_TIME" | "LOCALTIMESTAMP"
                | "CURRENT_USER" | "SESSION_USER" | "USER" | "TRUE" | "FALSE" => s.clone(),
                _ => return no(&format!("`{s}`")),
            },
            Expr::Extract(f, x) => {
                if !plain_word(f) {
                    return Err(format!("the EXTRACT field `{f}`"));
                }
                format!("EXTRACT({f} FROM {})", self.expr(x)?)
            }
        })
    }

    /// An operand of an operator or predicate: parenthesised unless an atom.
    fn operand(&self, e: &Expr) -> PrintResult {
        let s = self.expr(e)?;
        Ok(if atom(e) { s } else { format!("({s})") })
    }

    fn list(&self, v: &[Expr]) -> PrintResult {
        Ok(v.iter()
            .map(|e| self.expr(e))
            .collect::<Result<Vec<_>, _>>()?
            .join(", "))
    }

    fn call(&self, c: &Call) -> PrintResult {
        if c.over {
            return no("a window function");
        }
        if let [n] = c.name.as_slice() {
            if let Some(what) = n.strip_prefix("IS ") {
                // `x IS [NOT] TRUE | FALSE | UNKNOWN` (extended parser).
                let [x] = c.args.as_slice() else {
                    return Err(format!("`{n}` with other than one operand"));
                };
                return Ok(format!("{} IS {what}", self.operand(x)?));
            }
            if n == "NEXT VALUE FOR" {
                return no("NEXT VALUE FOR");
            }
        }
        let mut s = format!("{}(", self.name(&c.name)?);
        if c.star {
            if !c.args.is_empty() || c.distinct {
                return Err("a call with both `*` and arguments".into());
            }
            s.push('*');
        } else {
            if c.distinct {
                s.push_str("DISTINCT ");
            }
            s.push_str(&self.list(&c.args)?);
            if !c.order.is_empty() && !c.within_group {
                s.push_str(&format!(" ORDER BY {}", self.order(&c.order)?));
            }
        }
        s.push(')');
        if c.within_group {
            if c.order.is_empty() {
                return Err("WITHIN GROUP without ORDER BY".into());
            }
            s.push_str(&format!(
                " WITHIN GROUP (ORDER BY {})",
                self.order(&c.order)?
            ));
        }
        Ok(s)
    }

    fn order(&self, v: &[OrderItem]) -> PrintResult {
        let mut out = Vec::new();
        for o in v {
            let mut s = self.expr(&o.expr)?;
            if o.desc {
                s.push_str(" DESC");
            }
            match o.nulls_first {
                Some(true) => s.push_str(" NULLS FIRST"),
                Some(false) => s.push_str(" NULLS LAST"),
                None => {}
            }
            out.push(s);
        }
        Ok(out.join(", "))
    }

    // ---- queries -----------------------------------------------------------

    fn select(&self, q: &Select) -> PrintResult {
        if q.for_json {
            return no("FOR JSON");
        }
        if q.top.is_some() {
            return no("TOP");
        }
        let mut s = String::new();
        if !q.ctes.is_empty() {
            let mut parts = Vec::new();
            for c in &q.ctes {
                let cols = if c.cols.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", self.idents(&c.cols)?)
                };
                parts.push(format!(
                    "{}{cols} AS ({})",
                    self.ident(&c.name)?,
                    self.select(&c.query)?
                ));
            }
            s.push_str(&format!("WITH {} ", parts.join(", ")));
        }
        s.push_str(&self.core(q)?);
        for (op, operand) in &q.set_ops {
            if !matches!(
                op.as_str(),
                "UNION" | "UNION ALL" | "EXCEPT" | "EXCEPT ALL" | "INTERSECT" | "INTERSECT ALL"
            ) {
                return no(&format!("the set operation `{op}`"));
            }
            if !operand.ctes.is_empty()
                || !operand.set_ops.is_empty()
                || !operand.order_by.is_empty()
                || operand.limit.is_some()
                || operand.offset.is_some()
                || !operand.locking.is_empty()
                || operand.for_json
            {
                return Err("a set operand with its own WITH, ordering or paging".into());
            }
            s.push_str(&format!(" {op} {}", self.core(operand)?));
        }
        if !q.order_by.is_empty() {
            s.push_str(&format!(" ORDER BY {}", self.order(&q.order_by)?));
        }
        if let Some(l) = &q.limit {
            s.push_str(&format!(" LIMIT {}", self.operand(l)?));
        }
        if let Some(o) = &q.offset {
            s.push_str(&format!(" OFFSET {}", self.operand(o)?));
        }
        if !q.locking.is_empty() {
            if q.locking.iter().any(|w| w.starts_with('"')) {
                return Err("a locking clause naming a quoted table".into());
            }
            s.push(' ');
            s.push_str(&q.locking.join(" "));
        }
        Ok(s)
    }

    /// A query without its WITH, set operations, ordering, paging and locking.
    fn core(&self, q: &Select) -> PrintResult {
        if q.top.is_some() {
            return no("TOP");
        }
        if let Some(rows) = &q.values {
            if !q.items.is_empty() || !q.from.is_empty() || q.where_.is_some() {
                return Err("VALUES with a select list".into());
            }
            return self.values(rows);
        }
        let mut s = "SELECT ".to_string();
        if q.distinct {
            s.push_str("DISTINCT ");
        }
        let items = q
            .items
            .iter()
            .map(|i| self.item(i))
            .collect::<Result<Vec<_>, _>>()?;
        if items.is_empty() {
            return Err("an empty select list".into());
        }
        s.push_str(&items.join(", "));
        if !q.into.is_empty() {
            s.push_str(" INTO ");
            if q.into_strict {
                s.push_str("STRICT ");
            }
            s.push_str(&self.list(&q.into)?);
        } else if q.into_strict {
            return Err("INTO STRICT without targets".into());
        }
        if !q.from.is_empty() {
            s.push_str(&format!(" FROM {}", self.froms(&q.from)?));
        }
        if let Some(w) = &q.where_ {
            s.push_str(&format!(" WHERE {}", self.expr(w)?));
        }
        if !q.group_by.is_empty() {
            s.push_str(&format!(" GROUP BY {}", self.list(&q.group_by)?));
        }
        if let Some(h) = &q.having {
            s.push_str(&format!(" HAVING {}", self.expr(h)?));
        }
        Ok(s)
    }

    fn values(&self, rows: &[Vec<Expr>]) -> PrintResult {
        if rows.is_empty() {
            return Err("VALUES without rows".into());
        }
        let rows = rows
            .iter()
            .map(|r| Ok(format!("({})", self.list(r)?)))
            .collect::<Result<Vec<_>, String>>()?;
        Ok(format!("VALUES {}", rows.join(", ")))
    }

    fn item(&self, i: &SelItem) -> PrintResult {
        if let Some(v) = &i.assign {
            return no(&format!("the T-SQL assignment `{v} = ...`"));
        }
        let e = self.expr(&i.expr)?;
        Ok(match &i.alias {
            Some(a) => format!("{e} AS {}", self.alias(a)?),
            None => e,
        })
    }

    /// An alias after `AS`, where any word reads back as the alias.
    fn alias(&self, a: &str) -> PrintResult {
        if plain_word(a) {
            return Ok(a.to_string());
        }
        self.part(a, false)
    }

    fn froms(&self, v: &[From]) -> PrintResult {
        Ok(v.iter()
            .map(|f| self.from(f))
            .collect::<Result<Vec<_>, _>>()?
            .join(", "))
    }

    fn table_alias(&self, alias: &Option<String>, cols: &[String]) -> PrintResult {
        let mut s = String::new();
        if let Some(a) = alias {
            s.push_str(&format!(" AS {}", self.alias(a)?));
        }
        if !cols.is_empty() {
            if alias.is_none() {
                return Err("a column list without an alias".into());
            }
            s.push_str(&format!(" ({})", self.idents(cols)?));
        }
        Ok(s)
    }

    fn from(&self, f: &From) -> PrintResult {
        Ok(match f {
            From::Table { name, alias, hints } => {
                if !hints.is_empty() {
                    return no("table hints");
                }
                format!("{}{}", self.name(name)?, self.table_alias(alias, &[])?)
            }
            From::Sub { query, alias, cols } => {
                format!(
                    "({}){}",
                    self.select(query)?,
                    self.table_alias(alias, cols)?
                )
            }
            From::Values { rows, alias, cols } => {
                format!("({}){}", self.values(rows)?, self.table_alias(alias, cols)?)
            }
            From::Func {
                name,
                args,
                schema,
                alias,
                ..
            } => {
                if !schema.is_empty() {
                    return no("a WITH (...) result schema");
                }
                format!(
                    "{}({}){}",
                    self.name(name)?,
                    self.list(args)?,
                    self.table_alias(alias, &[])?
                )
            }
            From::DataChange { .. } => return no("a data-change table reference"),
            From::Join {
                kind,
                left,
                right,
                on,
            } => {
                if matches!(**right, From::Join { .. }) {
                    return Err("a join whose right operand is a join".into());
                }
                let k = match kind.as_str() {
                    "INNER" => "JOIN",
                    "LEFT" => "LEFT JOIN",
                    "RIGHT" => "RIGHT JOIN",
                    "FULL" => "FULL JOIN",
                    "CROSS" => "CROSS JOIN",
                    _ => return no(&format!("the join `{kind}`")),
                };
                let mut s = format!("{} {k} {}", self.from(left)?, self.from(right)?);
                match (kind.as_str(), on) {
                    ("CROSS", None) => {}
                    ("CROSS", Some(_)) => return Err("CROSS JOIN with ON".into()),
                    (_, Some(e)) => s.push_str(&format!(" ON {}", self.expr(e)?)),
                    (_, None) => return Err(format!("{kind} JOIN without ON")),
                }
                s
            }
        })
    }

    // ---- statements --------------------------------------------------------

    fn stmt(&self, s: &Stmt) -> PrintResult {
        Ok(match s {
            Stmt::Select(q) => self.select(q)?,
            Stmt::Insert {
                target,
                cols,
                source,
                returning,
                returning_into,
                on_conflict,
            } => {
                let mut s = format!("INSERT INTO {}", self.name(target)?);
                if !cols.is_empty() {
                    s.push_str(&format!(" ({})", self.idents(cols)?));
                }
                match source {
                    InsertSource::Values(rows) => {
                        s.push(' ');
                        s.push_str(&self.values(rows)?);
                    }
                    InsertSource::Query(q) => {
                        s.push(' ');
                        s.push_str(&self.select(q)?);
                    }
                    InsertSource::Default => s.push_str(" DEFAULT VALUES"),
                }
                if let Some((cols, action)) = on_conflict {
                    if cols.is_empty() {
                        return Err("ON CONFLICT without a column list".into());
                    }
                    s.push_str(&format!(" ON CONFLICT ({}) DO ", self.idents(cols)?));
                    match action {
                        ConflictAction::Nothing => s.push_str("NOTHING"),
                        ConflictAction::Update(sets, w) => {
                            s.push_str(&format!("UPDATE SET {}", self.sets1(sets)?));
                            if let Some(w) = w {
                                s.push_str(&format!(" WHERE {}", self.expr(w)?));
                            }
                        }
                    }
                }
                if !returning.is_empty() {
                    let items = returning
                        .iter()
                        .map(|i| self.item(i))
                        .collect::<Result<Vec<_>, _>>()?;
                    s.push_str(&format!(" RETURNING {}", items.join(", ")));
                    if !returning_into.is_empty() {
                        s.push_str(&format!(" INTO {}", self.list(returning_into)?));
                    }
                } else if !returning_into.is_empty() {
                    return Err("INTO without RETURNING".into());
                }
                s
            }
            Stmt::Update {
                target,
                alias,
                sets,
                from,
                where_,
            } => {
                let mut s = format!(
                    "UPDATE {}{} SET ",
                    self.name(target)?,
                    self.table_alias(alias, &[])?
                );
                let mut parts = Vec::new();
                for (cols, e) in sets {
                    parts.push(match cols.as_slice() {
                        [c] => format!("{} = {}", self.ident(c)?, self.expr(e)?),
                        [] => return Err("an assignment to no column".into()),
                        _ => format!("({}) = {}", self.idents(cols)?, self.operand(e)?),
                    });
                }
                s.push_str(&parts.join(", "));
                if !from.is_empty() {
                    s.push_str(&format!(" FROM {}", self.froms(from)?));
                }
                if let Some(w) = where_ {
                    s.push_str(&format!(" WHERE {}", self.expr(w)?));
                }
                s
            }
            Stmt::Delete {
                target,
                alias,
                from,
                where_,
            } => {
                let mut s = format!(
                    "DELETE FROM {}{}",
                    self.name(target)?,
                    self.table_alias(alias, &[])?
                );
                if !from.is_empty() {
                    s.push_str(&format!(" USING {}", self.froms(from)?));
                }
                if let Some(w) = where_ {
                    s.push_str(&format!(" WHERE {}", self.expr(w)?));
                }
                s
            }
            Stmt::Merge {
                target,
                alias,
                hints,
                source,
                on,
                whens,
            } => {
                if !hints.is_empty() {
                    return no("table hints");
                }
                let mut s = format!(
                    "MERGE INTO {}{} USING {} ON {}",
                    self.name(target)?,
                    self.table_alias(alias, &[])?,
                    self.from(source)?,
                    self.expr(on)?
                );
                for w in whens {
                    s.push(' ');
                    s.push_str(&self.when(w)?);
                }
                s
            }
            Stmt::Assign(t, e) => match t {
                Expr::Name(n) => format!("{} := {}", self.name(n)?, self.expr(e)?),
                _ => return no("an assignment to anything but a name"),
            },
            Stmt::Return(None) => "RETURN".into(),
            Stmt::Return(Some(e)) => format!("RETURN {}", self.expr(e)?),
            Stmt::Declare { name, ty, default } => {
                let Some(ty) = ty else {
                    return Err("a declaration without a type".into());
                };
                let mut s = format!("DECLARE {} {}", self.ident(name)?, self.ty(ty)?);
                if let Some(d) = default {
                    s.push_str(&format!(" := {}", self.expr(d)?));
                }
                s
            }
            Stmt::Message(_) => return no("a message statement"),
        })
    }

    fn sets1(&self, sets: &[(String, Expr)]) -> PrintResult {
        Ok(sets
            .iter()
            .map(|(c, e)| Ok(format!("{} = {}", self.ident(c)?, self.expr(e)?)))
            .collect::<Result<Vec<_>, String>>()?
            .join(", "))
    }

    fn when(&self, w: &When) -> PrintResult {
        let mut s = format!("WHEN {}MATCHED", if w.matched { "" } else { "NOT " });
        if w.by_source {
            if w.matched {
                return Err("WHEN MATCHED BY SOURCE".into());
            }
            s.push_str(" BY SOURCE");
        }
        if let Some(c) = &w.cond {
            s.push_str(&format!(" AND {}", self.operand(c)?));
        }
        s.push_str(" THEN ");
        match &w.action {
            MergeAction::Update(sets) => s.push_str(&format!("UPDATE SET {}", self.sets1(sets)?)),
            MergeAction::Delete => s.push_str("DELETE"),
            MergeAction::Insert(cols, vals) => {
                s.push_str("INSERT ");
                if !cols.is_empty() {
                    s.push_str(&format!("({}) ", self.idents(cols)?));
                }
                s.push_str(&format!("VALUES ({})", self.list(vals)?));
            }
        }
        Ok(s)
    }
}
