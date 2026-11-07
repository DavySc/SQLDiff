//! Prints surface terms as SQL of one dialect.
//!
//! The printer only spells shared constructs. Anything that would be wrong
//! in the target (a T-SQL-only clause, a function the target does not know,
//! raw tokens of another dialect) is an error, so a statement is either
//! converted completely or not at all.

use std::cell::RefCell;
use std::collections::HashSet;

use sqldiff_syntax::Dialect;

use crate::equiv::term::Term;

type R<T> = Result<T, String>;

/// FROM entries in scope, innermost last: (table name term, alias).
type Scopes = RefCell<Vec<Vec<(Term, Option<String>)>>>;

pub struct Printer<'a> {
    pub d: Dialect,
    /// Spelling of a T-SQL `@variable` (or any `(var v)`) in the output.
    pub vars: &'a dyn Fn(&str) -> String,
    /// Functions the output may call; `None` accepts every name.
    pub funcs: Option<&'a HashSet<String>>,
    scope: Scopes,
}

fn atom(t: &Term) -> R<&str> {
    t.atom()
        .ok_or_else(|| format!("expected an atom, found {t}"))
}

const BINARY: &[(&str, u8)] = &[
    ("OR", 1),
    ("AND", 2),
    ("=", 4),
    ("<>", 4),
    ("!=", 4),
    ("<", 4),
    (">", 4),
    ("<=", 4),
    (">=", 4),
    ("||", 5),
    ("+", 6),
    ("-", 6),
    ("*", 7),
    ("/", 7),
    ("%", 7),
];

fn binary_prec(t: &Term) -> Option<u8> {
    if t.kids().len() != 2 || t.atom().is_some() {
        return None;
    }
    BINARY
        .iter()
        .find(|(op, _)| op.eq_ignore_ascii_case(t.head()))
        .map(|(_, p)| *p)
}

fn prec(t: &Term) -> u8 {
    if let Some(p) = binary_prec(t) {
        return p;
    }
    match t.head() {
        "not" => 3,
        "is-null" | "is-not-null" | "in" | "not-in" | "in-q" | "not-in-q" | "between"
        | "not-between" | "like" | "not-like" | "ilike" | "not-ilike" => 4,
        _ => 9,
    }
}

pub fn quote_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

impl<'a> Printer<'a> {
    pub fn new(
        d: Dialect,
        vars: &'a dyn Fn(&str) -> String,
        funcs: Option<&'a HashSet<String>>,
    ) -> Self {
        Printer {
            d,
            vars,
            funcs,
            scope: RefCell::new(Vec::new()),
        }
    }

    fn pg(&self) -> bool {
        self.d == Dialect::Postgres
    }

    fn ident(&self, s: &str) -> String {
        if let Some(q) = s.strip_prefix('"') {
            return format!("\"{}\"", q.replace('"', "\"\""));
        }
        let plain = s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
            && !s.starts_with(|c: char| c.is_ascii_digit());
        if plain && !(self.pg() && reserved_pg(s)) {
            s.to_string()
        } else if self.d == Dialect::Tsql {
            format!("[{s}]")
        } else {
            format!("\"{s}\"")
        }
    }

    fn name(&self, t: &Term) -> R<String> {
        if !t.is("name") {
            return Err(format!("expected a name, found {t}"));
        }
        Ok(t.kids()
            .iter()
            .map(|p| atom(p).map(|s| self.ident(s)))
            .collect::<R<Vec<_>>>()?
            .join("."))
    }

    fn var(&self, t: &Term) -> R<String> {
        let v = atom(t.kid(0).ok_or("empty var")?)?;
        if v.starts_with("@@") && self.d != Dialect::Tsql {
            return Err(format!("`{v}` has no {} spelling", self.d.display_name()));
        }
        Ok((self.vars)(v))
    }

    /// A column qualifier: the alias of the table in scope, or the name.
    fn qualifier(&self, q: &Term) -> R<String> {
        if q.is("alias") {
            return Ok(self.ident(atom(&q.kids()[0])?));
        }
        for frame in self.scope.borrow().iter().rev() {
            if let Some((_, al)) = frame
                .iter()
                .find(|(n, _)| crate::rules::pattern::same(n, q))
            {
                return match al {
                    Some(al) => Ok(self.ident(al)),
                    None => Ok(self.name(q)?),
                };
            }
        }
        self.name(q)
    }

    pub fn expr(&self, t: &Term) -> R<String> {
        if let Some(p) = binary_prec(t) {
            let (l, r) = (&t.kids()[0], &t.kids()[1]);
            let ls = self.wrap(l, prec(l) < p)?;
            let rs = self.wrap(r, prec(r) <= p && !(p <= 2 && prec(r) == p))?;
            let op = t.head().to_uppercase();
            return Ok(format!("{ls} {op} {rs}"));
        }
        let k = t.kids();
        let e = |i: usize| -> R<String> {
            self.expr(k.get(i).ok_or_else(|| format!("missing operand in {t}"))?)
        };
        Ok(match t.head() {
            "col" => match k {
                [c] => self.ident(atom(c)?),
                [q, c] => format!("{}.{}", self.qualifier(q)?, self.ident(atom(c)?)),
                _ => return Err(format!("bad column {t}")),
            },
            "var" => self.var(t)?,
            "num" => num(atom(&k[0])?),
            "str" => quote_str(atom(&k[0])?),
            "null" => "NULL".into(),
            "bool" => atom(&k[0])?.to_uppercase(),
            "star" => match k {
                [] => "*".into(),
                [q] => format!("{}.*", self.ident(atom(q)?)),
                _ => return Err(format!("bad star {t}")),
            },
            "typed" => format!("{} {}", atom(&k[0])?, quote_str(atom(&k[1])?)),
            "call" => self.call(t)?,
            "not" => format!("NOT {}", self.wrap(&k[0], prec(&k[0]) < 3)?),
            "neg" => {
                let x = self.wrap(&k[0], prec(&k[0]) < 9)?;
                // `--` would start a line comment.
                if x.starts_with('-') {
                    format!("-({x})")
                } else {
                    format!("-{x}")
                }
            }
            "is-null" => format!("{} IS NULL", self.wrap(&k[0], prec(&k[0]) <= 4)?),
            "is-not-null" => format!("{} IS NOT NULL", self.wrap(&k[0], prec(&k[0]) <= 4)?),
            "in" | "not-in" => format!(
                "{} {}IN ({})",
                self.wrap(&k[0], prec(&k[0]) <= 4)?,
                if t.is("not-in") { "NOT " } else { "" },
                self.list(k[1].kids())?
            ),
            "in-q" | "not-in-q" => format!(
                "{} {}IN ({})",
                self.wrap(&k[0], prec(&k[0]) <= 4)?,
                if t.is("not-in-q") { "NOT " } else { "" },
                self.query(&k[1])?
            ),
            "between" | "not-between" => format!(
                "{} {}BETWEEN {} AND {}",
                self.wrap(&k[0], prec(&k[0]) <= 4)?,
                if t.is("not-between") { "NOT " } else { "" },
                self.wrap(&k[1], prec(&k[1]) <= 4)?,
                self.wrap(&k[2], prec(&k[2]) <= 4)?
            ),
            "like" | "not-like" | "ilike" | "not-ilike" => {
                let (not, op) = match t.head() {
                    "like" => ("", "LIKE"),
                    "not-like" => ("NOT ", "LIKE"),
                    "ilike" => ("", "ILIKE"),
                    _ => ("NOT ", "ILIKE"),
                };
                if op == "ILIKE" && !self.pg() {
                    return Err("ILIKE is PostgreSQL only".into());
                }
                format!(
                    "{} {not}{op} {}",
                    self.wrap(&k[0], prec(&k[0]) <= 4)?,
                    self.wrap(&k[1], prec(&k[1]) <= 4)?
                )
            }
            "cast" => format!("CAST({} AS {})", e(0)?, self.ty(&k[1])?),
            "case" => {
                let mut s = "CASE".to_string();
                for x in k {
                    match x.head() {
                        "operand" => s += &format!(" {}", self.expr(&x.kids()[0])?),
                        "when" => {
                            s += &format!(
                                " WHEN {} THEN {}",
                                self.expr(&x.kids()[0])?,
                                self.expr(&x.kids()[1])?
                            )
                        }
                        "else" => s += &format!(" ELSE {}", self.expr(&x.kids()[0])?),
                        _ => return Err(format!("bad CASE part {x}")),
                    }
                }
                s + " END"
            }
            "exists" => format!("EXISTS ({})", self.query(&k[0])?),
            "subq" => format!("({})", self.query(&k[0])?),
            "tuple" => format!("({})", self.list(k)?),
            "special" => atom(&k[0])?.to_string(),
            "extract" => format!("EXTRACT({} FROM {})", atom(&k[0])?, e(1)?),
            "paren" => format!("({})", e(0)?),
            h => return Err(format!("`{h}` has no {} spelling", self.d.display_name())),
        })
    }

    fn wrap(&self, t: &Term, paren: bool) -> R<String> {
        let s = self.expr(t)?;
        Ok(if paren { format!("({s})") } else { s })
    }

    fn list(&self, ts: &[Term]) -> R<String> {
        Ok(ts
            .iter()
            .map(|x| self.expr(x))
            .collect::<R<Vec<_>>>()?
            .join(", "))
    }

    fn ty(&self, t: &Term) -> R<String> {
        let k = t.kids();
        let name = atom(&k[0])?;
        Ok(if k.len() > 1 {
            let args: Vec<&str> = k[1..].iter().map(atom).collect::<R<_>>()?;
            format!("{name}({})", args.join(", "))
        } else {
            name.to_string()
        })
    }

    fn call(&self, t: &Term) -> R<String> {
        let k = t.kids();
        let name = atom(&k[0])?;
        if let Some(f) = self.funcs {
            if !f.contains(name) {
                return Err(format!(
                    "no rule converts `{name}` to {}",
                    self.d.display_name()
                ));
            }
        }
        let mut args = Vec::new();
        let mut distinct = "";
        let mut tail = String::new();
        for x in &k[1..] {
            match x.head() {
                "distinct" if x.atom().is_none() && x.kids().is_empty() => distinct = "DISTINCT ",
                "star" if x.kids().is_empty() => args.push("*".to_string()),
                "order" if x.atom().is_none() => {
                    if !self.pg() {
                        return Err("ORDER BY inside a call has no T-SQL spelling".into());
                    }
                    tail = format!(" ORDER BY {}", self.order(x.kids())?);
                }
                "within-group" if x.atom().is_none() => {
                    let o = self.order(x.kids())?;
                    let s = format!(
                        "{name}({distinct}{}) WITHIN GROUP (ORDER BY {o})",
                        args.join(", ")
                    );
                    return Ok(s);
                }
                _ => args.push(self.expr(x)?),
            }
        }
        Ok(format!("{name}({distinct}{}{tail})", args.join(", ")))
    }

    fn order(&self, items: &[Term]) -> R<String> {
        let mut out = Vec::new();
        for o in items {
            let mut s = self.expr(&o.kids()[0])?;
            if o.is("desc") {
                s += " DESC";
            }
            match o.kid(1).and_then(Term::atom) {
                Some("nulls-first") => s += " NULLS FIRST",
                Some("nulls-last") => s += " NULLS LAST",
                _ => {}
            }
            if s.contains(" NULLS ") && self.d == Dialect::Tsql {
                return Err("NULLS FIRST/LAST has no T-SQL spelling".into());
            }
            out.push(s);
        }
        Ok(out.join(", "))
    }

    fn push_scope(&self, from: &[Term]) {
        let mut frame = Vec::new();
        for f in from {
            collect(f, &mut frame);
        }
        self.scope.borrow_mut().push(frame);
    }

    fn pop_scope(&self) {
        self.scope.borrow_mut().pop();
    }

    fn table(&self, t: &Term) -> R<String> {
        let mut s = self.name(&t.kids()[0])?;
        for x in &t.kids()[1..] {
            match x.head() {
                "alias" => s += &format!(" AS {}", self.ident(atom(&x.kids()[0])?)),
                "hints" => {
                    if self.d != Dialect::Tsql {
                        return Err(format!(
                            "table hints have no {} spelling",
                            self.d.display_name()
                        ));
                    }
                    let hs: Vec<String> = x
                        .kids()
                        .iter()
                        .map(|h| atom(h).map(str::to_uppercase))
                        .collect::<R<_>>()?;
                    s += &format!(" WITH ({})", hs.join(", "));
                }
                _ => return Err(format!("bad table {t}")),
            }
        }
        Ok(s)
    }

    fn source(&self, f: &Term) -> R<String> {
        let k = f.kids();
        Ok(match f.head() {
            "table" => self.table(f)?,
            "sub" => {
                let mut s = format!("({})", self.query(&k[0])?);
                for x in &k[1..] {
                    match x.head() {
                        "alias" => s += &format!(" AS {}", self.ident(atom(&x.kids()[0])?)),
                        "cols" => s += &format!(" ({})", self.idents(x.kids())?),
                        _ => return Err(format!("bad derived table {f}")),
                    }
                }
                s
            }
            "values-src" => {
                let mut rows = Vec::new();
                let mut tail = String::new();
                for x in k {
                    match x.head() {
                        "row" => rows.push(format!("({})", self.list(x.kids())?)),
                        "alias" => tail += &format!(" AS {}", self.ident(atom(&x.kids()[0])?)),
                        "cols" => tail += &format!(" ({})", self.idents(x.kids())?),
                        _ => return Err(format!("bad VALUES source {f}")),
                    }
                }
                format!("(VALUES {}){tail}", rows.join(", "))
            }
            "join" => {
                let kind = atom(&k[0])?.to_uppercase();
                let mut s = format!(
                    "{} {kind} JOIN {}",
                    self.source(&k[1])?,
                    self.source(&k[2])?
                );
                if let Some(on) = k.get(3) {
                    s += &format!(" ON {}", self.expr(on)?);
                }
                s
            }
            h => return Err(format!("`{h}` is not a FROM item")),
        })
    }

    fn idents(&self, ts: &[Term]) -> R<String> {
        Ok(ts
            .iter()
            .map(|c| atom(c).map(|s| self.ident(s)))
            .collect::<R<Vec<_>>>()?
            .join(", "))
    }

    pub fn query(&self, q: &Term) -> R<String> {
        if q.is("values") {
            let rows: Vec<String> = q
                .kids()
                .iter()
                .map(|r| self.list(r.kids()).map(|s| format!("({s})")))
                .collect::<R<_>>()?;
            return Ok(format!("VALUES {}", rows.join(", ")));
        }
        if !q.is("select") {
            return Err(format!("expected a query, found {}", q.head()));
        }
        let part = |h: &str| q.kids().iter().find(|x| x.is(h));
        let from: Vec<Term> = part("from").map(|f| f.kids().to_vec()).unwrap_or_default();
        self.push_scope(&from);
        let r = self.query_inner(q, &from);
        self.pop_scope();
        r
    }

    fn query_inner(&self, q: &Term, from: &[Term]) -> R<String> {
        let mut s = "SELECT".to_string();
        let mut limit = None;
        let mut offset = None;
        for x in q.kids() {
            let k = x.kids();
            match x.head() {
                "distinct" => s += " DISTINCT",
                "top" => {
                    if self.d != Dialect::Tsql {
                        return Err(format!("TOP has no {} spelling", self.d.display_name()));
                    }
                    s += &format!(" TOP ({})", self.expr(&k[0])?);
                }
                "items" => {
                    let mut items = Vec::new();
                    for it in k {
                        items.push(match it.head() {
                            "item" => {
                                let e = self.expr(&it.kids()[0])?;
                                match it.kid(1) {
                                    Some(al) => format!("{e} AS {}", self.ident(atom(al)?)),
                                    None => e,
                                }
                            }
                            "assign-item" if self.d == Dialect::Tsql => format!(
                                "{} = {}",
                                self.var(&it.kids()[0])?,
                                self.expr(&it.kids()[1])?
                            ),
                            h => {
                                return Err(format!(
                                    "select item `{h}` has no {} spelling",
                                    self.d.display_name()
                                ))
                            }
                        });
                    }
                    s += &format!(" {}", items.join(", "));
                }
                "into" | "into-strict" => {
                    if self.d == Dialect::Tsql {
                        return Err("SELECT ... INTO a variable has no T-SQL spelling".into());
                    }
                    let strict = if x.is("into-strict") { "STRICT " } else { "" };
                    s += &format!(" INTO {strict}{}", self.list(k)?);
                }
                "from" => {
                    let fs: Vec<String> = from.iter().map(|f| self.source(f)).collect::<R<_>>()?;
                    s += &format!(" FROM {}", fs.join(", "));
                }
                "where" => s += &format!(" WHERE {}", self.expr(&k[0])?),
                "group" => s += &format!(" GROUP BY {}", self.list(k)?),
                "having" => s += &format!(" HAVING {}", self.expr(&k[0])?),
                "order" => s += &format!(" ORDER BY {}", self.order(k)?),
                "offset" => offset = Some(self.expr(&k[0])?),
                "limit" => limit = Some(self.expr(&k[0])?),
                "lock" => {
                    let words: Vec<&str> = k.iter().map(atom).collect::<R<_>>()?;
                    let w = words.join(" ");
                    if self.pg() && !w.to_uppercase().starts_with("FOR ") {
                        return Err(format!("locking clause `{w}` has no PostgreSQL spelling"));
                    }
                    s += &format!(" {w}");
                }
                h => {
                    return Err(format!(
                        "query part `{h}` has no {} spelling",
                        self.d.display_name()
                    ))
                }
            }
        }
        match self.d {
            Dialect::Tsql => {
                if limit.is_some() || offset.is_some() {
                    s += &format!(" OFFSET {} ROWS", offset.unwrap_or_else(|| "0".into()));
                    if let Some(l) = limit {
                        s += &format!(" FETCH NEXT {l} ROWS ONLY");
                    }
                }
            }
            _ => {
                if let Some(l) = limit {
                    s += &format!(" LIMIT {l}");
                }
                if let Some(o) = offset {
                    s += &format!(" OFFSET {o}");
                }
            }
        }
        Ok(s)
    }

    fn set_list(&self, set: &Term) -> R<String> {
        let mut out = Vec::new();
        for kv in set.kids() {
            let (lhs, rhs) = (&kv.kids()[0], &kv.kids()[1]);
            let l = match lhs.head() {
                "col" if lhs.kids().len() == 1 => self.ident(atom(&lhs.kids()[0])?),
                "cols" => format!("({})", self.idents(lhs.kids())?),
                _ => return Err(format!("bad SET target {lhs}")),
            };
            out.push(format!("{l} = {}", self.expr(rhs)?));
        }
        Ok(out.join(", "))
    }

    /// One statement with its terminator, indented by `ind`.
    pub fn stmt(&self, t: &Term, ind: usize) -> R<String> {
        let pad = "  ".repeat(ind);
        let k = t.kids();
        let line = |s: String| format!("{pad}{s};");
        Ok(match t.head() {
            "select" | "values" => line(self.query(t)?),
            "insert" => line(self.insert(t)?),
            "update" => {
                let target = &k[0];
                let from: Vec<Term> = k
                    .iter()
                    .find(|x| x.is("from"))
                    .map(|f| f.kids().to_vec())
                    .unwrap_or_default();
                if !from.is_empty() {
                    let tname = &target.kids()[0];
                    if from.iter().any(|f| mentions_table(f, tname)) {
                        return Err(
                            "UPDATE ... FROM that lists the target table again is not converted"
                                .into(),
                        );
                    }
                }
                let mut all = vec![target.clone()];
                all.extend(from.iter().cloned());
                self.push_scope(&all);
                let r: R<String> = (|| {
                    let mut s = format!(
                        "UPDATE {} SET {}",
                        self.table(target)?,
                        self.set_list(&k[1])?
                    );
                    for x in &k[2..] {
                        match x.head() {
                            "from" => {
                                let fs: Vec<String> =
                                    x.kids().iter().map(|f| self.source(f)).collect::<R<_>>()?;
                                s += &format!(" FROM {}", fs.join(", "));
                            }
                            "where" => s += &format!(" WHERE {}", self.expr(&x.kids()[0])?),
                            h => return Err(format!("UPDATE part `{h}`")),
                        }
                    }
                    Ok(s)
                })();
                self.pop_scope();
                line(r?)
            }
            "delete" => {
                let target = &k[0];
                if k.iter().any(|x| x.is("from")) {
                    return Err("DELETE with a FROM/USING list is not converted".into());
                }
                self.push_scope(std::slice::from_ref(target));
                let r: R<String> = (|| {
                    let mut s = format!("DELETE FROM {}", self.table(target)?);
                    if let Some(w) = k.iter().find(|x| x.is("where")) {
                        s += &format!(" WHERE {}", self.expr(&w.kids()[0])?);
                    }
                    Ok(s)
                })();
                self.pop_scope();
                line(r?)
            }
            "assign" => {
                let target = match k[0].head() {
                    "var" => self.var(&k[0])?,
                    _ => self.expr(&k[0])?,
                };
                match self.d {
                    Dialect::Tsql => line(format!("SET {target} = {}", self.expr(&k[1])?)),
                    Dialect::Postgres => line(format!("{target} := {}", self.expr(&k[1])?)),
                    Dialect::Db2i => line(format!("SET {target} = {}", self.expr(&k[1])?)),
                }
            }
            "return" => match k.first() {
                Some(e) => line(format!("RETURN {}", self.expr(e)?)),
                None => line("RETURN".into()),
            },
            "perform" if self.pg() => line(format!("PERFORM {}", self.expr(&k[0])?)),
            "if" => {
                let c = self.expr(&k[0])?;
                let body = |b: &Term| -> R<String> {
                    let items: Vec<&Term> = if b.is("block") {
                        b.kids().iter().collect()
                    } else {
                        vec![b]
                    };
                    Ok(items
                        .iter()
                        .map(|x| self.stmt(x, ind + 1))
                        .collect::<R<Vec<_>>>()?
                        .join("\n"))
                };
                match self.d {
                    Dialect::Tsql => {
                        let mut s = format!("{pad}IF {c}\n{pad}BEGIN\n{}\n{pad}END", body(&k[1])?);
                        if let Some(e) = k.get(2) {
                            s += &format!("\n{pad}ELSE\n{pad}BEGIN\n{}\n{pad}END", body(e)?);
                        }
                        s
                    }
                    _ => {
                        let mut s = format!("{pad}IF {c} THEN\n{}", body(&k[1])?);
                        if let Some(e) = k.get(2) {
                            s += &format!("\n{pad}ELSE\n{}", body(e)?);
                        }
                        s + &format!("\n{pad}END IF;")
                    }
                }
            }
            "block" => {
                let inner: Vec<String> = k.iter().map(|x| self.stmt(x, ind)).collect::<R<_>>()?;
                inner.join("\n")
            }
            "raw" => {
                let d = atom(&k[0])?;
                if d != self.d.id() {
                    return Err(format!(
                        "no rule converts this {} statement",
                        d.parse::<Dialect>().map(|x| x.display_name()).unwrap_or(d)
                    ));
                }
                let toks: Vec<&str> = k[1..].iter().map(atom).collect::<R<_>>()?;
                line(toks.join(" "))
            }
            h => {
                return Err(format!(
                    "statement `{h}` has no {} spelling",
                    self.d.display_name()
                ))
            }
        })
    }

    fn insert(&self, t: &Term) -> R<String> {
        let k = t.kids();
        self.push_scope(&k[..1]);
        let r: R<String> = (|| {
            let mut s = format!("INSERT INTO {}", self.table(&k[0])?);
            if !k[1].kids().is_empty() {
                s += &format!(" ({})", self.idents(k[1].kids())?);
            }
            let mut returning = String::new();
            for x in &k[2..] {
                match x.head() {
                    "values" => {
                        let rows: Vec<String> = x
                            .kids()
                            .iter()
                            .map(|r| self.list(r.kids()).map(|s| format!("({s})")))
                            .collect::<R<_>>()?;
                        s += &format!(" VALUES {}", rows.join(", "));
                    }
                    "query" => s += &format!(" {}", self.query(&x.kids()[0])?),
                    "default-values" => s += " DEFAULT VALUES",
                    "on-conflict" if self.pg() => {
                        let keys = self.idents(x.kids()[0].kids())?;
                        let act = &x.kids()[1];
                        s += &format!(" ON CONFLICT ({keys}) ");
                        match act.head() {
                            "do-nothing" => s += "DO NOTHING",
                            "do-update" => {
                                s += &format!("DO UPDATE SET {}", self.set_list(&act.kids()[0])?);
                                if let Some(w) = act.kid(1) {
                                    s += &format!(" WHERE {}", self.expr(&w.kids()[0])?);
                                }
                            }
                            h => return Err(format!("ON CONFLICT action `{h}`")),
                        }
                    }
                    "returning" if self.d != Dialect::Tsql => {
                        let items: Vec<String> = x
                            .kids()
                            .iter()
                            .map(|it| {
                                let e = self.expr(&it.kids()[0])?;
                                Ok(match it.kid(1) {
                                    Some(al) => format!("{e} AS {}", self.ident(atom(al)?)),
                                    None => e,
                                })
                            })
                            .collect::<R<_>>()?;
                        returning = format!(" RETURNING {}", items.join(", "));
                    }
                    "returning-into" if self.pg() => {
                        returning += &format!(" INTO {}", self.list(x.kids())?)
                    }
                    h => {
                        return Err(format!(
                            "INSERT part `{h}` has no {} spelling",
                            self.d.display_name()
                        ))
                    }
                }
            }
            Ok(s + &returning)
        })();
        self.pop_scope();
        r
    }
}

fn collect(f: &Term, out: &mut Vec<(Term, Option<String>)>) {
    let alias = |f: &Term| {
        f.kids()
            .iter()
            .find(|x| x.is("alias"))
            .and_then(|x| x.kid(0))
            .and_then(Term::atom)
            .map(str::to_string)
    };
    match f.head() {
        "table" => out.push((f.kids()[0].clone(), alias(f))),
        "join" => {
            collect(&f.kids()[1], out);
            collect(&f.kids()[2], out);
        }
        _ => {}
    }
}

fn mentions_table(f: &Term, name: &Term) -> bool {
    match f.head() {
        "table" => crate::rules::pattern::same(&f.kids()[0], name),
        "join" => mentions_table(&f.kids()[1], name) || mentions_table(&f.kids()[2], name),
        _ => false,
    }
}

/// PostgreSQL reserved words that cannot be bare identifiers.
fn reserved_pg(s: &str) -> bool {
    const W: &[&str] = &[
        "all",
        "analyse",
        "analyze",
        "and",
        "any",
        "array",
        "as",
        "asc",
        "asymmetric",
        "both",
        "case",
        "cast",
        "check",
        "collate",
        "column",
        "constraint",
        "create",
        "current_date",
        "current_role",
        "current_time",
        "current_timestamp",
        "current_user",
        "default",
        "deferrable",
        "desc",
        "distinct",
        "do",
        "else",
        "end",
        "except",
        "false",
        "fetch",
        "for",
        "foreign",
        "from",
        "grant",
        "group",
        "having",
        "in",
        "initially",
        "intersect",
        "into",
        "lateral",
        "leading",
        "limit",
        "localtime",
        "localtimestamp",
        "not",
        "null",
        "offset",
        "on",
        "only",
        "or",
        "order",
        "placing",
        "primary",
        "references",
        "returning",
        "select",
        "session_user",
        "some",
        "symmetric",
        "table",
        "then",
        "to",
        "trailing",
        "true",
        "union",
        "unique",
        "user",
        "using",
        "variadic",
        "when",
        "where",
        "window",
        "with",
    ];
    W.contains(&s.to_ascii_lowercase().as_str())
}

/// A numeric literal; the parser folds signs into it (`-(-1)` is `--1`),
/// and `--` would start a line comment.
fn num(s: &str) -> String {
    match s.strip_prefix('-') {
        Some(rest) if rest.starts_with('-') => format!("-({})", num(rest)),
        _ => s.to_string(),
    }
}
