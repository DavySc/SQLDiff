//! The abstract syntax as one line of text for the Lean checker
//! (docs/verified-conversion.md, C0: the AST the proof is about).
//!
//! Every value is written with its constructor and every field, so the form
//! is injective: two lines are equal only for equal trees. Grammar:
//!
//! ```text
//! v    := (Tag v*)        a constructor and its fields, in declaration order
//!       | [v*]            a list
//!       | s:HEX           a string, its UTF-8 bytes in lower-case hex
//!       | -               None
//!       | t | f           a bool
//! ```
//!
//! An `Option<T>` that is `Some(x)` is written as `x` (no tag): a field is
//! either always optional or never, so this stays injective. Tags are the
//! Rust constructor names; [`SCHEMA`] lists them with their fields, and the
//! Lean decoder is written against that list; requests carry a hash of it
//! (`sqldiff_core::proof`).

use std::fmt::Write as _;

use crate::abstract_syntax::{
    Call, ConflictAction, Cte, Expr, From, InsertSource, MergeAction, OrderItem, SelItem, Select,
    Stmt, Type, When,
};
use crate::routine::{Arg, Block, Mode, Node, Param, Routine, RoutineKind, Security};

/// Every constructor the encoding writes, with its fields. A change to the
/// Rust types that changes the encoding must change this text, which
/// changes the hash requests carry.
pub const SCHEMA: &str = "\
Routine kind name:[s] params:[Param] returns:Type? security volatility:s? body:Block
Procedure | Function
Param name:s ty:Type mode default:Expr?
In | Out | InOut
Caller | Owner
Block decls:[Stmt] body:[Node]
NStmt Stmt | NBlock Block | If cond:Expr then:[Node] els:[Node] | While cond:Expr body:[Node]
Call name:[s] args:[Arg] ret:Expr? | Setting name:s on:b
Arg name:s? value:Expr output:b
Select ctes:[Cte] for_json:b set_ops:[SetOp] distinct:b top:Expr? items:[SelItem] into:[Expr] into_strict:b from:[From] where:Expr? group_by:[Expr] having:Expr? order_by:[OrderItem] offset:Expr? limit:Expr? locking:[s] values:[[Expr]]?
SetOp op:s query:Select
Cte name:s cols:[s] query:Select
SelItem expr:Expr alias:s? assign:s?
OrderItem expr:Expr desc:b nulls_first:b?
Type name:s args:[s]
Table name:[s] alias:s? hints:[s] | Sub query:Select alias:s? cols:[s] | Values rows:[[Expr]] alias:s? cols:[s]
Func name:[s] args:[Expr] schema:[Col] alias:s? | DataChange kind:s stmt:Stmt alias:s? | Join kind:s left:From right:From on:Expr?
Col name:s ty:Type
Name [s] | Var s | Num s | Str s | Null | Star s? | CallE Call | Bin op:s Expr Expr | Unary op:s Expr
IsNull Expr neg:b | InList Expr [Expr] neg:b | InQuery Expr Select neg:b | Between Expr Expr Expr neg:b
Like Expr Expr neg:b ci:b | Cast Expr Type | Case Expr? [When2] Expr? | Exists Select | Subquery Select
Tuple [Expr] | Special s | Extract s Expr
When2 Expr Expr
CallF name:[s] args:[Expr] distinct:b order:[OrderItem] within_group:b over:b star:b
SSelect Select | Insert target:[s] cols:[s] source:Src returning:[SelItem] returning_into:[Expr] on_conflict:Conflict?
Update target:[s] alias:s? sets:[Set] from:[From] where:Expr? | Delete target:[s] alias:s? from:[From] where:Expr?
Merge target:[s] alias:s? hints:[s] source:From on:Expr whens:[When] | Assign Expr Expr | Return Expr?
Declare name:s ty:Type? default:Expr? | Message Expr
SrcValues [[Expr]] | SrcQuery Select | SrcDefault
Conflict cols:[s] action:Action
Nothing | DoUpdate [Set1] where:Expr?
Set cols:[s] Expr | Set1 col:s Expr
When matched:b by_source:b cond:Expr? action:MAction
MUpdate [Set1] | MDelete | MInsert cols:[s] [Expr]
";

struct W(String);

impl W {
    fn open(&mut self, tag: &str) {
        let _ = write!(self.0, "({tag}");
    }
    fn close(&mut self) {
        self.0.push(')');
    }
    fn sep(&mut self) {
        self.0.push(' ');
    }
    fn s(&mut self, s: &str) {
        self.sep();
        self.0.push_str("s:");
        for b in s.as_bytes() {
            let _ = write!(self.0, "{b:02x}");
        }
    }
    fn b(&mut self, b: bool) {
        self.sep();
        self.0.push(if b { 't' } else { 'f' });
    }
    fn none(&mut self) {
        self.sep();
        self.0.push('-');
    }
    fn list<T>(&mut self, v: &[T], f: impl Fn(&mut W, &T)) {
        self.sep();
        self.0.push('[');
        let at = self.0.len();
        for x in v {
            f(self, x);
        }
        // Items are space-separated: none before the first.
        if self.0[at..].starts_with(' ') {
            self.0.remove(at);
        }
        self.0.push(']');
    }
    fn opt<T>(&mut self, v: &Option<T>, f: impl Fn(&mut W, &T)) {
        match v {
            Some(x) => f(self, x),
            None => self.none(),
        }
    }
    fn strs(&mut self, v: &[String]) {
        self.list(v, |w, x| w.s(x));
    }
    fn os(&mut self, v: &Option<String>) {
        self.opt(v, |w, x| w.s(x));
    }
    fn tag(&mut self, tag: &str) {
        self.sep();
        self.0.push_str(tag);
    }
}

/// A routine as one line.
pub fn routine(r: &Routine) -> String {
    let mut w = W(String::new());
    w.routine(r);
    w.0.trim_start().to_string()
}

/// A statement as one line.
pub fn stmt(s: &Stmt) -> String {
    let mut w = W(String::new());
    w.stmt(s);
    w.0.trim_start().to_string()
}

/// An expression as one line.
pub fn expr(e: &Expr) -> String {
    let mut w = W(String::new());
    w.expr(e);
    w.0.trim_start().to_string()
}

impl W {
    fn routine(&mut self, r: &Routine) {
        self.sep();
        self.open("Routine");
        self.tag(match r.kind {
            RoutineKind::Procedure => "Procedure",
            RoutineKind::Function => "Function",
        });
        self.strs(&r.name);
        self.list(&r.params, |w, p| w.param(p));
        self.opt(&r.returns, |w, t| w.ty(t));
        self.tag(match r.security {
            Security::Caller => "Caller",
            Security::Owner => "Owner",
        });
        self.os(&r.volatility);
        self.block(&r.body);
        self.close();
    }

    fn param(&mut self, p: &Param) {
        self.sep();
        self.open("Param");
        self.s(&p.name);
        self.ty(&p.ty);
        self.tag(match p.mode {
            Mode::In => "In",
            Mode::Out => "Out",
            Mode::InOut => "InOut",
        });
        self.opt(&p.default, |w, e| w.expr(e));
        self.close();
    }

    fn block(&mut self, b: &Block) {
        self.sep();
        self.open("Block");
        self.list(&b.decls, |w, s| w.stmt(s));
        self.list(&b.body, |w, n| w.node(n));
        self.close();
    }

    fn node(&mut self, n: &Node) {
        self.sep();
        match n {
            Node::Stmt(s) => {
                self.open("NStmt");
                self.stmt(s);
            }
            Node::Block(b) => {
                self.open("NBlock");
                self.block(b);
            }
            Node::If { cond, then, els } => {
                self.open("If");
                self.expr(cond);
                self.list(then, |w, n| w.node(n));
                self.list(els, |w, n| w.node(n));
            }
            Node::While { cond, body } => {
                self.open("While");
                self.expr(cond);
                self.list(body, |w, n| w.node(n));
            }
            Node::Call { name, args, ret } => {
                self.open("Call");
                self.strs(name);
                self.list(args, |w, a| w.arg(a));
                self.opt(ret, |w, e| w.expr(e));
            }
            Node::Setting { name, on } => {
                self.open("Setting");
                self.s(name);
                self.b(*on);
            }
        }
        self.close();
    }

    fn arg(&mut self, a: &Arg) {
        self.sep();
        self.open("Arg");
        self.os(&a.name);
        self.expr(&a.value);
        self.b(a.output);
        self.close();
    }

    fn ty(&mut self, t: &Type) {
        self.sep();
        self.open("Type");
        self.s(&t.name);
        self.strs(&t.args);
        self.close();
    }

    fn exprs(&mut self, v: &[Expr]) {
        self.list(v, |w, e| w.expr(e));
    }

    fn rows(&mut self, v: &[Vec<Expr>]) {
        self.list(v, |w, r| w.exprs(r));
    }

    fn oe(&mut self, e: &Option<Expr>) {
        self.opt(e, |w, e| w.expr(e));
    }

    fn expr(&mut self, e: &Expr) {
        self.sep();
        match e {
            Expr::Name(p) => {
                self.open("Name");
                self.strs(p);
            }
            Expr::Var(v) => {
                self.open("Var");
                self.s(v);
            }
            Expr::Num(n) => {
                self.open("Num");
                self.s(n);
            }
            Expr::Str(s) => {
                self.open("Str");
                self.s(s);
            }
            Expr::Null => self.open("Null"),
            Expr::Star(q) => {
                self.open("Star");
                self.os(q);
            }
            Expr::Call(c) => {
                self.open("CallE");
                self.call(c);
            }
            Expr::Bin(op, l, r) => {
                self.open("Bin");
                self.s(op);
                self.expr(l);
                self.expr(r);
            }
            Expr::Unary(op, x) => {
                self.open("Unary");
                self.s(op);
                self.expr(x);
            }
            Expr::IsNull(x, neg) => {
                self.open("IsNull");
                self.expr(x);
                self.b(*neg);
            }
            Expr::InList(x, v, neg) => {
                self.open("InList");
                self.expr(x);
                self.exprs(v);
                self.b(*neg);
            }
            Expr::InQuery(x, q, neg) => {
                self.open("InQuery");
                self.expr(x);
                self.select(q);
                self.b(*neg);
            }
            Expr::Between(x, lo, hi, neg) => {
                self.open("Between");
                self.expr(x);
                self.expr(lo);
                self.expr(hi);
                self.b(*neg);
            }
            Expr::Like(x, p, neg, ci) => {
                self.open("Like");
                self.expr(x);
                self.expr(p);
                self.b(*neg);
                self.b(*ci);
            }
            Expr::Cast(x, t) => {
                self.open("Cast");
                self.expr(x);
                self.ty(t);
            }
            Expr::Case(op, whens, els) => {
                self.open("Case");
                self.opt(op, |w, e| w.expr(e));
                self.list(whens, |w, (c, t)| {
                    w.sep();
                    w.open("When2");
                    w.expr(c);
                    w.expr(t);
                    w.close();
                });
                self.opt(els, |w, e| w.expr(e));
            }
            Expr::Exists(q) => {
                self.open("Exists");
                self.select(q);
            }
            Expr::Subquery(q) => {
                self.open("Subquery");
                self.select(q);
            }
            Expr::Tuple(v) => {
                self.open("Tuple");
                self.exprs(v);
            }
            Expr::Special(s) => {
                self.open("Special");
                self.s(s);
            }
            Expr::Extract(f, x) => {
                self.open("Extract");
                self.s(f);
                self.expr(x);
            }
        }
        self.close();
    }

    fn call(&mut self, c: &Call) {
        self.sep();
        self.open("CallF");
        self.strs(&c.name);
        self.exprs(&c.args);
        self.b(c.distinct);
        self.list(&c.order, |w, o| w.order(o));
        self.b(c.within_group);
        self.b(c.over);
        self.b(c.star);
        self.close();
    }

    fn order(&mut self, o: &OrderItem) {
        self.sep();
        self.open("OrderItem");
        self.expr(&o.expr);
        self.b(o.desc);
        self.opt(&o.nulls_first, |w, b| w.b(*b));
        self.close();
    }

    fn item(&mut self, i: &SelItem) {
        self.sep();
        self.open("SelItem");
        self.expr(&i.expr);
        self.os(&i.alias);
        self.os(&i.assign);
        self.close();
    }

    fn cte(&mut self, c: &Cte) {
        self.sep();
        self.open("Cte");
        self.s(&c.name);
        self.strs(&c.cols);
        self.select(&c.query);
        self.close();
    }

    fn select(&mut self, q: &Select) {
        self.sep();
        self.open("Select");
        self.list(&q.ctes, |w, c| w.cte(c));
        self.b(q.for_json);
        self.list(&q.set_ops, |w, (op, s)| {
            w.sep();
            w.open("SetOp");
            w.s(op);
            w.select(s);
            w.close();
        });
        self.b(q.distinct);
        self.oe(&q.top);
        self.list(&q.items, |w, i| w.item(i));
        self.exprs(&q.into);
        self.b(q.into_strict);
        self.list(&q.from, |w, f| w.from(f));
        self.oe(&q.where_);
        self.exprs(&q.group_by);
        self.oe(&q.having);
        self.list(&q.order_by, |w, o| w.order(o));
        self.oe(&q.offset);
        self.oe(&q.limit);
        self.strs(&q.locking);
        self.opt(&q.values, |w, r| w.rows(r));
        self.close();
    }

    fn from(&mut self, f: &From) {
        self.sep();
        match f {
            From::Table { name, alias, hints } => {
                self.open("Table");
                self.strs(name);
                self.os(alias);
                self.strs(hints);
            }
            From::Sub { query, alias, cols } => {
                self.open("Sub");
                self.select(query);
                self.os(alias);
                self.strs(cols);
            }
            From::Values { rows, alias, cols } => {
                self.open("Values");
                self.rows(rows);
                self.os(alias);
                self.strs(cols);
            }
            From::Func {
                name,
                args,
                schema,
                alias,
            } => {
                self.open("Func");
                self.strs(name);
                self.exprs(args);
                self.list(schema, |w, (c, t)| {
                    w.sep();
                    w.open("Col");
                    w.s(c);
                    w.ty(t);
                    w.close();
                });
                self.os(alias);
            }
            From::DataChange { kind, stmt, alias } => {
                self.open("DataChange");
                self.s(kind);
                self.stmt(stmt);
                self.os(alias);
            }
            From::Join {
                kind,
                left,
                right,
                on,
            } => {
                self.open("Join");
                self.s(kind);
                self.from(left);
                self.from(right);
                self.oe(on);
            }
        }
        self.close();
    }

    fn set1(&mut self, (c, e): &(String, Expr)) {
        self.sep();
        self.open("Set1");
        self.s(c);
        self.expr(e);
        self.close();
    }

    fn stmt(&mut self, s: &Stmt) {
        self.sep();
        match s {
            Stmt::Select(q) => {
                self.open("SSelect");
                self.select(q);
            }
            Stmt::Insert {
                target,
                cols,
                source,
                returning,
                returning_into,
                on_conflict,
            } => {
                self.open("Insert");
                self.strs(target);
                self.strs(cols);
                self.sep();
                match source {
                    InsertSource::Values(r) => {
                        self.open("SrcValues");
                        self.rows(r);
                    }
                    InsertSource::Query(q) => {
                        self.open("SrcQuery");
                        self.select(q);
                    }
                    InsertSource::Default => self.open("SrcDefault"),
                }
                self.close();
                self.list(returning, |w, i| w.item(i));
                self.exprs(returning_into);
                self.opt(on_conflict, |w, (cols, action)| {
                    w.sep();
                    w.open("Conflict");
                    w.strs(cols);
                    w.sep();
                    match action {
                        ConflictAction::Nothing => w.open("Nothing"),
                        ConflictAction::Update(sets, wh) => {
                            w.open("DoUpdate");
                            w.list(sets, |w, s| w.set1(s));
                            w.oe(wh);
                        }
                    }
                    w.close();
                    w.close();
                });
            }
            Stmt::Update {
                target,
                alias,
                sets,
                from,
                where_,
            } => {
                self.open("Update");
                self.strs(target);
                self.os(alias);
                self.list(sets, |w, (cols, e)| {
                    w.sep();
                    w.open("Set");
                    w.strs(cols);
                    w.expr(e);
                    w.close();
                });
                self.list(from, |w, f| w.from(f));
                self.oe(where_);
            }
            Stmt::Delete {
                target,
                alias,
                from,
                where_,
            } => {
                self.open("Delete");
                self.strs(target);
                self.os(alias);
                self.list(from, |w, f| w.from(f));
                self.oe(where_);
            }
            Stmt::Merge {
                target,
                alias,
                hints,
                source,
                on,
                whens,
            } => {
                self.open("Merge");
                self.strs(target);
                self.os(alias);
                self.strs(hints);
                self.from(source);
                self.expr(on);
                self.list(whens, |w, x| w.when(x));
            }
            Stmt::Assign(t, e) => {
                self.open("Assign");
                self.expr(t);
                self.expr(e);
            }
            Stmt::Return(e) => {
                self.open("Return");
                self.oe(e);
            }
            Stmt::Declare { name, ty, default } => {
                self.open("Declare");
                self.s(name);
                self.opt(ty, |w, t| w.ty(t));
                self.oe(default);
            }
            Stmt::Message(e) => {
                self.open("Message");
                self.expr(e);
            }
        }
        self.close();
    }

    fn when(&mut self, x: &When) {
        self.sep();
        self.open("When");
        self.b(x.matched);
        self.b(x.by_source);
        self.oe(&x.cond);
        self.sep();
        match &x.action {
            MergeAction::Update(sets) => {
                self.open("MUpdate");
                self.list(sets, |w, s| w.set1(s));
            }
            MergeAction::Delete => self.open("MDelete"),
            MergeAction::Insert(cols, vals) => {
                self.open("MInsert");
                self.strs(cols);
                self.exprs(vals);
            }
        }
        self.close();
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abstract_syntax::{parse_stmt_ext, tokens};
    use crate::{parse, Dialect};

    fn enc(sql: &str, d: Dialect) -> String {
        let p = parse(sql, d);
        stmt(&parse_stmt_ext(&tokens(&p.syntax()), d).unwrap())
    }

    #[test]
    fn statements_encode_with_every_field() {
        assert_eq!(
            enc("x := 'a'", Dialect::Postgres),
            "(Assign (Name [s:78]) (Str s:61))"
        );
        assert_eq!(
            enc("SET @x = NULL", Dialect::Tsql),
            "(Assign (Var s:4078) (Null))"
        );
    }

    #[test]
    fn different_trees_encode_differently() {
        let d = Dialect::Postgres;
        let all = [
            "SELECT a FROM t",
            "SELECT a AS a FROM t",
            "SELECT DISTINCT a FROM t",
            "SELECT a FROM t WHERE a = 1",
            "SELECT a FROM t ORDER BY a",
            "SELECT a FROM t ORDER BY a DESC",
            "SELECT a FROM t ORDER BY a NULLS FIRST",
            "SELECT a FROM t LIMIT 1",
            "SELECT a FROM t OFFSET 1",
            "SELECT 'a'",
            "SELECT 'b'",
            "SELECT NULL",
            "SELECT a IS NULL",
            "SELECT a IS NOT NULL",
        ];
        let encs: Vec<String> = all.iter().map(|s| enc(s, d)).collect();
        for i in 0..encs.len() {
            for j in i + 1..encs.len() {
                assert_ne!(encs[i], encs[j], "{} / {}", all[i], all[j]);
            }
        }
    }
}
