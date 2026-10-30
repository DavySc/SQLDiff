//! The proof gate (decisions 2 and 3 in docs/verification-roadmap.md): an
//! "equal" verdict is sent to a checker whose answer is backed by a Lean
//! theorem (`formal/lean`). In `report` mode the answer is recorded; in
//! `require` mode an unconfirmed "equal" becomes "unknown", with the reason.
//!
//! The core never starts a process: a front end supplies the [`Checker`].
//! A missing checker, a failure or anything unexpected is "unconfirmed",
//! never "confirmed".

use std::fmt::Write as _;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqldiff_syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use crate::artifact::Analyzed;
use crate::model::{Report, Verdict};

/// The line protocol `sqldiff-check` speaks (`formal/lean/Main.lean`).
/// Version 2 carries the syntax schema hash ([`schema_hash`]).
pub const PROTOCOL: &str = "sqldiff-check/2";

/// The syntax schema: every kind a tree can hold, one per line, `token` or
/// `node` and the name. The Lean types are generated from it
/// ([`lean_syntax`]).
pub fn schema() -> String {
    SyntaxKind::all()
        .map(|k| {
            let what = if k.is_token() { "token" } else { "node" };
            format!("{what} {k:?}\n")
        })
        .collect()
}

/// The first 16 hex digits of the SHA-256 of [`schema`]. Requests carry it
/// and the checker refuses one whose hash is not its own: a kind added,
/// removed or renamed in Rust is unknown, never misread, until
/// `formal/lean/Sqldiff/Syntax.lean` is generated again.
pub fn schema_hash() -> String {
    Sha256::digest(schema().as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The Lean module `formal/lean/Sqldiff/Syntax.lean`, generated from the
/// schema. `crates/sqldiff-core/tests/lean_syntax.rs` fails when the file in
/// the tree is not this text.
pub fn lean_syntax() -> String {
    let kinds: Vec<SyntaxKind> = SyntaxKind::all().collect();
    let mut s = String::new();
    s.push_str("/-\nGenerated from `sqldiff_syntax::SyntaxKind` by `sqldiff_core::proof::lean_syntax`.\nDo not edit: run `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test lean_syntax`.\n-/\n\n");
    s.push_str("namespace Sqldiff.Syntax\n\n");
    s.push_str(
        "/-- The schema hash requests must carry (`sqldiff_core::proof::schema_hash`). -/\n",
    );
    let _ = writeln!(s, "def schemaHash : String := \"{}\"\n", schema_hash());
    s.push_str("/-- The kinds of SQLDiff's syntax tree. -/\ninductive Kind where\n");
    for k in &kinds {
        let _ = writeln!(s, "  | {k:?}");
    }
    s.push_str("deriving DecidableEq, Repr\n\n");
    s.push_str("/-- A kind by its Rust name; `none` for a name this schema lacks. -/\ndef Kind.ofName? : String → Option Kind\n");
    for k in &kinds {
        let _ = writeln!(s, "  | \"{k:?}\" => some .{k:?}");
    }
    s.push_str("  | _ => none\n\n");
    s.push_str(
        "/-- Whether the kind is a token (a leaf with text). -/\ndef Kind.isToken : Kind → Bool\n",
    );
    for k in &kinds {
        let _ = writeln!(s, "  | .{k:?} => {}", k.is_token());
    }
    s.push_str("\n/-- Whitespace and comments, which trees sent to the checker leave out. -/\ndef Kind.isTrivia : Kind → Bool\n");
    for k in &kinds {
        let _ = writeln!(s, "  | .{k:?} => {}", k.is_trivia());
    }
    s.push_str("\nend Sqldiff.Syntax\n");
    s
}

/// What the gate does with an "equal" verdict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The checker is not called.
    #[default]
    Off,
    /// The answer is recorded; the verdict is unchanged.
    Report,
    /// An unconfirmed "equal" or "equal under assumptions" becomes "unknown".
    Require,
}

impl std::str::FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "off" => Ok(Mode::Off),
            "report" => Ok(Mode::Report),
            "require" => Ok(Mode::Require),
            _ => Err(format!("proof mode `{s}`: expected off, report or require")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Confirmed,
    Unconfirmed,
    /// Nothing to confirm: the verdict is not "equal".
    NotApplicable,
}

/// What the gate found, as reports carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    pub status: Status,
    /// The theorem behind a confirmation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theorem: Option<String>,
    /// Why it is not confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The checker that answered (`sqldiff-check 0.2.0 (sqldiff-check/2)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checker: Option<String>,
}

/// One comparison, as the checker receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub dialect_a: String,
    pub target_a: String,
    pub tree_a: String,
    pub dialect_b: String,
    pub target_b: String,
    pub tree_b: String,
}

impl Request {
    pub fn new(a: &Analyzed, b: &Analyzed) -> Self {
        Request {
            dialect_a: a.dialect().id().to_string(),
            target_a: a.artifact.version.clone(),
            tree_a: tree_line(&a.root),
            dialect_b: b.dialect().id().to_string(),
            target_b: b.artifact.version.clone(),
            tree_b: tree_line(&b.root),
        }
    }

    /// The request line, without the line end. `None` when a field would
    /// break the tab-separated format (a target with a tab or line break),
    /// which the gate answers as unconfirmed.
    pub fn line(&self, id: u64) -> Option<String> {
        let fields = [
            &self.dialect_a,
            &self.target_a,
            &self.tree_a,
            &self.dialect_b,
            &self.target_b,
            &self.tree_b,
        ];
        if fields.iter().any(|f| f.contains(['\t', '\n', '\r'])) {
            return None;
        }
        let mut s = format!("{PROTOCOL}\t{id}\t{}", schema_hash());
        for f in fields {
            s.push('\t');
            s.push_str(f);
        }
        s.push_str("\tend");
        Some(s)
    }
}

/// The checker's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Confirmed { theorem: String },
    Unconfirmed { reason: String },
}

/// A front end's connection to `sqldiff-check`. Every failure (no binary,
/// a crash, a timeout, an answer that does not parse) is
/// [`Answer::Unconfirmed`].
pub trait Checker: Send + Sync + std::fmt::Debug {
    /// Name and version, recorded in the report.
    fn name(&self) -> String;
    fn check(&self, req: &Request) -> Answer;
    /// The C1 mirror check of a converted routine (`sqldiff-check/3`). A
    /// checker that does not speak it answers unconfirmed.
    fn check_mirror(&self, _req: &MirrorRequest) -> MirrorAnswer {
        MirrorAnswer::from(Answer::Unconfirmed {
            reason: format!("the checker does not speak {MIRROR_PROTOCOL}"),
        })
    }
}

/// The checker's answer to a mirror request: with a confirmation, whether
/// every expression was in the C2a scalar slice (`scalars` is `c2a`: the
/// operators have their meaning, and `residual` lists what the slice still
/// assumes) or not (`uninterpreted`: operators are taken to mean the same
/// in both dialects); and whether data statements were in the C3a slice
/// (`statements` is `c3a`), queries in the C3b slice (`c3b`, with or
/// without data statements), or the routine had none in its spec (`c1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorAnswer {
    pub answer: Answer,
    pub scalars: Option<String>,
    pub residual: Vec<String>,
    pub statements: Option<String>,
    /// `c4` when the routine's error handling and transaction control were
    /// in the C4 slice.
    pub errors: Option<String>,
    /// With an unconfirmed answer, the checker's reason codes
    /// (`family:detail`, ASCII lower case), one per thing it refused;
    /// empty when the checker gave none (an older checker, or a
    /// confirmation).
    pub why: Vec<String>,
}

impl From<Answer> for MirrorAnswer {
    fn from(answer: Answer) -> Self {
        MirrorAnswer {
            answer,
            scalars: None,
            residual: Vec::new(),
            statements: None,
            errors: None,
            why: Vec::new(),
        }
    }
}

/// The line protocol of the mirror check (`formal/lean/Main.lean`,
/// `formal/lean/Sqldiff/Mirror/Request.lean`).
pub const MIRROR_PROTOCOL: &str = "sqldiff-check/3";

/// One converted routine for the C1 mirror check: the source and the
/// output routine (`sqldiff_syntax::encode::routine` lines), the trusted
/// deployment facts and target catalog, and the untrusted renaming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorRequest {
    /// The observer reads rows-affected counts.
    pub row_counts: bool,
    /// T-SQL routine names (`.`-joined, as written) and the target routine
    /// each one is (trusted: the deployment's schema mapping).
    pub routines: Vec<(String, String)>,
    /// Target routines (folded names) and their parameter modes, one of
    /// `i`, `o`, `b` (IN, OUT, INOUT) per parameter.
    pub catalog: Vec<(String, String)>,
    /// T-SQL table names (`.`-joined, as written) and the target table
    /// each one is (trusted: the deployment's schema mapping, C3a).
    pub tables: Vec<(String, String)>,
    /// Target tables (trusted): sent only when both catalogs show every
    /// fact a [`MirrorTable`] asserts.
    pub table_catalog: Vec<MirrorTable>,
    /// Trusted (C2b): the T-SQL database has a binary (`_BIN`/`_BIN2`)
    /// default collation and every collation a PostgreSQL comparison of
    /// the routine can take is deterministic (`ASM-BINARY-COLLATION`).
    pub binary_collation: bool,
    /// Source variable (with `@`) to target name; the T-SQL return status
    /// is `%status`.
    pub renaming: Vec<(String, String)>,
    pub source: String,
    pub target: String,
}

/// A table of the target catalog for the C3a slice
/// (`Sqldiff.Mirror.CatTable`). Sending one asserts that both databases
/// have it with these columns (`int`, or (C3c) the string types of
/// [`MirrorColumn::str`]), nullability, primary key and (C3c) constant
/// defaults, and nothing else that changes a write: no other constraint,
/// default, identity, sequence, computed column, trigger, rule or
/// row-level security; a key neither deferrable nor `IGNORE_DUP_KEY`; no
/// inheritance children; not a temporary table or table variable; (C3c)
/// only non-unique indexes whose entries stay within the engines' size
/// limits; string columns with the database's default collation, created
/// in SQL Server with `ANSI_PADDING ON`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorTable {
    /// Folded, `.`-joined as the PL/pgSQL code names it.
    pub name: String,
    /// In declaration order.
    pub cols: Vec<MirrorColumn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorColumn {
    /// The target column (folded).
    pub name: String,
    /// Its spelling in the T-SQL table.
    pub tsql: String,
    pub not_null: bool,
    /// A column of the primary key.
    pub key: bool,
    /// (C3c) A string column: `(n, wide)` is T-SQL `varchar(n)` (`wide`
    /// false) or `nvarchar(n)` (true) against PostgreSQL `varchar(n)`.
    /// `None`: `int`/`integer`. Sent only when both DDLs show it.
    pub str: Option<(u32, bool)>,
    /// (C3c) The constant default both DDLs declare; `None` for none or
    /// `DEFAULT NULL`.
    pub default: Option<MirrorDefault>,
}

/// (C3c) A column default the checker takes as given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirrorDefault {
    Int(i32),
    Str(String),
}

fn hex(s: &str) -> String {
    s.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn pairs(v: &[(String, String)]) -> String {
    v.iter()
        .map(|(a, b)| format!("{}:{}", hex(a), hex(b)))
        .collect::<Vec<_>>()
        .join(",")
}

impl MirrorRequest {
    /// The request line, without the line end. `None` when a field would
    /// break the format (a routine line with a tab, a mode letter other
    /// than i, o, b).
    pub fn line(&self, id: u64) -> Option<String> {
        if [&self.source, &self.target]
            .iter()
            .any(|f| f.contains(['\t', '\n', '\r']))
            || self
                .catalog
                .iter()
                .any(|(_, m)| !m.chars().all(|c| matches!(c, 'i' | 'o' | 'b')))
        {
            return None;
        }
        let mut dep = String::from(if self.row_counts { "1" } else { "0" });
        if !self.routines.is_empty() {
            dep.push(',');
            dep.push_str(&pairs(&self.routines));
        }
        if !self.tables.is_empty() {
            dep.push(';');
            dep.push_str(&pairs(&self.tables));
        }
        let or_dash = |s: String| if s.is_empty() { "-".to_string() } else { s };
        let tables = self.table_catalog.iter().map(|t| {
            let cols = t
                .cols
                .iter()
                .map(|c| {
                    // SPEC (Request.lean): the type (`i`, or (C3c) `v<n>` /
                    // `w<n>`), then `n` for NOT NULL, then `k` for a key
                    // column; (C3c) the default as a fourth field.
                    let mut spec = match c.str {
                        None => "i".to_string(),
                        Some((n, false)) => format!("v{n}"),
                        Some((n, true)) => format!("w{n}"),
                    };
                    match (c.key, c.not_null) {
                        (true, _) => spec.push_str("nk"),
                        (false, true) => spec.push('n'),
                        (false, false) => {}
                    }
                    let default = match &c.default {
                        None => String::new(),
                        Some(MirrorDefault::Int(i)) => format!(".i{i}"),
                        Some(MirrorDefault::Str(v)) => format!(".s{}", hex(v)),
                    };
                    format!("{}.{spec}.{}{default}", hex(&c.name), hex(&c.tsql))
                })
                .collect::<Vec<_>>()
                .join("/");
            format!("{}={cols}", hex(&t.name))
        });
        let catalog = self
            .catalog
            .iter()
            .map(|(n, m)| format!("{}:{m}", hex(n)))
            .chain(tables)
            .chain(
                self.binary_collation
                    .then(|| "!binary-collation".to_string()),
            )
            .collect::<Vec<_>>()
            .join(",");
        Some(format!(
            "{MIRROR_PROTOCOL}\t{id}\t{}\tmirror\t{dep}\t{}\t{}\t{}\t{}\tend",
            ast_schema_hash(),
            or_dash(catalog),
            or_dash(pairs(&self.renaming)),
            self.source,
            self.target
        ))
    }
}

/// The gate's settings for one run.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub mode: Mode,
    pub checker: Option<Arc<dyn Checker>>,
}

/// A syntax tree without whitespace and comments, on one line: a node is
/// `(KIND child ...)`, a token `KIND:hex` with its text in lower-case hex.
/// Kind names are upper case with underscores and digits, so the form is
/// injective: two lines are equal only for equal trees. The Lean side
/// trusts this (formal/lean/Sqldiff/Gate.lean).
pub fn tree_line(node: &SyntaxNode) -> String {
    let mut out = String::new();
    write_node(node, &mut out);
    out
}

fn write_node(node: &SyntaxNode, out: &mut String) {
    let _ = write!(out, "({:?}", node.kind());
    for child in node.children_with_tokens() {
        match child {
            SyntaxElement::Node(n) => {
                out.push(' ');
                write_node(&n, out);
            }
            SyntaxElement::Token(t) if !t.kind().is_trivia() => {
                let _ = write!(out, " {:?}:", t.kind());
                for b in t.text().as_bytes() {
                    let _ = write!(out, "{b:02x}");
                }
            }
            SyntaxElement::Token(_) => {}
        }
    }
    out.push(')');
}

/// The first 16 hex digits of the SHA-256 of the abstract syntax schema
/// (`sqldiff_syntax::encode::SCHEMA`), which requests about abstract syntax
/// carry.
pub fn ast_schema_hash() -> String {
    Sha256::digest(sqldiff_syntax::encode::SCHEMA.as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The Lean module `formal/lean/Sqldiff/Ast.lean`, generated from
/// `sqldiff_syntax::encode::SCHEMA`: the abstract syntax as Lean types and
/// a decoder for the line encoding. `crates/sqldiff-core/tests/lean_syntax.rs`
/// fails while the file in the tree is not this text.
pub fn lean_ast() -> String {
    use sqldiff_syntax::encode::{schema_types, FieldTy};
    let types = schema_types();
    let lean_ty = |t: &FieldTy| -> String {
        fn go(t: &FieldTy) -> String {
            match t {
                FieldTy::Str => "String".into(),
                FieldTy::Bool => "Bool".into(),
                FieldTy::Named(n) => n.clone(),
                FieldTy::List(x) => format!("(List {})", go(x)),
                FieldTy::Opt(x) => format!("(Option {})", go(x)),
            }
        }
        go(t)
    };
    // The decoder expression for a field of type `t` from the value `v`.
    fn dec(t: &FieldTy, v: &str) -> String {
        match t {
            FieldTy::Str => format!("str? {v}"),
            FieldTy::Bool => format!("bool? {v}"),
            FieldTy::Named(n) => format!("decode{n} {v}"),
            FieldTy::List(x) => format!("list? {v} (fun x => {})", dec(x, "x")),
            FieldTy::Opt(x) => format!("opt? {v} (fun x => {})", dec(x, "x")),
        }
    }
    let mut s = String::new();
    s.push_str("/-\nGenerated from `sqldiff_syntax::encode::SCHEMA` by `sqldiff_core::proof::lean_ast`.\nDo not edit: run `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test lean_syntax`.\n\nThe abstract syntax the Rust parser reads, as Lean types, and the decoder\nfor its one-line encoding (`sqldiff_syntax::encode`). A line decodes only\nif it is exactly a value of the schema.\n-/\n\n");
    s.push_str("namespace Sqldiff.Ast\n\n");
    s.push_str("/-- The schema hash requests about abstract syntax carry (`sqldiff_core::proof::ast_schema_hash`). -/\n");
    let _ = writeln!(s, "def schemaHash : String := \"{}\"\n", ast_schema_hash());
    s.push_str("mutual\n");
    for t in &types {
        let _ = writeln!(s, "  inductive {} where", t.name);
        for c in &t.ctors {
            let mut line = format!("    | {}", c.name);
            for (f, ft) in &c.fields {
                let _ = write!(line, " («{f}» : {})", lean_ty(ft));
            }
            let _ = writeln!(s, "{line}");
        }
    }
    s.push_str("end\n\n");
    s.push_str(
        r#"/-- A line in its generic form (`sqldiff_syntax::encode::Value`). -/
inductive Value where
  | ctor (tag : String) (kids : List Value)
  | list (xs : List Value)
  | str (hex : String)
  | none
  | bool (b : Bool)

private def isHexDigit (c : Char) : Bool := c.isDigit || ('a' ≤ c && c ≤ 'f')

private def hexVal (c : Char) : Nat :=
  if c.isDigit then c.toNat - '0'.toNat else c.toNat - 'a'.toNat + 10

mutual
  private partial def value : List Char → Option (Value × List Char)
    | '(' :: rest =>
      let tag := rest.takeWhile Char.isAlphanum
      if tag.isEmpty then none
      else match fields (rest.drop tag.length) with
        | some (kids, ')' :: rest) => some (.ctor (String.ofList tag) kids, rest)
        | _ => none
    | '[' :: ']' :: rest => some (.list [], rest)
    | '[' :: rest =>
      match value rest with
      | some (x, rest) =>
        match fields rest with
        | some (xs, ']' :: rest) => some (.list (x :: xs), rest)
        | _ => none
      | none => none
    | 's' :: ':' :: rest =>
      let h := rest.takeWhile isHexDigit
      if h.length % 2 == 0 then some (.str (String.ofList h), rest.drop h.length) else none
    | '-' :: rest => some (.none, rest)
    | 't' :: rest => some (.bool true, rest)
    | 'f' :: rest => some (.bool false, rest)
    | _ => none

  /-- ` v` repeated. -/
  private partial def fields : List Char → Option (List Value × List Char)
    | ' ' :: rest =>
      match value rest with
      | some (x, rest) =>
        match fields rest with
        | some (xs, rest) => some (x :: xs, rest)
        | none => none
      | none => none
    | cs => some ([], cs)
end

/-- The value a whole line holds, if it is exactly one value. -/
def parseLine (s : String) : Option Value :=
  match value s.toList with
  | some (v, []) => some v
  | _ => none

private def bytes : List Char → Option (List UInt8)
  | a :: b :: rest => (bytes rest).map (fun bs => (hexVal a * 16 + hexVal b).toUInt8 :: bs)
  | [] => some []
  | _ => none

private def str? : Value → Option String
  | .str h => (bytes h.toList).bind (fun bs => String.fromUTF8? ⟨bs.toArray⟩)
  | _ => none

private def bool? : Value → Option Bool
  | .bool b => some b
  | _ => none

private def list? {α : Type} : Value → (Value → Option α) → Option (List α)
  | .list xs, f => xs.mapM f
  | _, _ => none

private def opt? {α : Type} : Value → (Value → Option α) → Option (Option α)
  | .none, _ => some none
  | v, f => (f v).map some

"#,
    );
    s.push_str("mutual\n");
    for t in &types {
        let _ = writeln!(
            s,
            "  partial def decode{} : Value → Option {}",
            t.name, t.name
        );
        for c in &t.ctors {
            let vars: Vec<String> = (0..c.fields.len()).map(|i| format!("v{i}")).collect();
            let _ = write!(s, "    | .ctor \"{}\" [{}] => ", c.name, vars.join(", "));
            if c.fields.is_empty() {
                let _ = writeln!(s, "some .{}", c.name);
                continue;
            }
            s.push_str("do\n");
            for (i, (_, ft)) in c.fields.iter().enumerate() {
                let _ = writeln!(s, "      let x{i} ← {}", dec(ft, &format!("v{i}")));
            }
            let xs: Vec<String> = (0..c.fields.len()).map(|i| format!("x{i}")).collect();
            let _ = writeln!(s, "      pure (.{} {})", c.name, xs.join(" "));
        }
        s.push_str("    | _ => none\n");
    }
    s.push_str("end\n\n");
    s.push_str("/-- A routine from its line, if the line is exactly one. -/\ndef decodeRoutineLine (s : String) : Option Routine := (parseLine s).bind decodeRoutine\n\n");
    s.push_str("end Sqldiff.Ast\n");
    s
}

/// Runs the gate on a finished diff report.
pub(crate) fn apply(r: &mut Report, a: &Analyzed, b: &Analyzed, s: &Settings) {
    if s.mode == Mode::Off {
        return;
    }
    if !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions) {
        r.proof = Some(Proof {
            status: Status::NotApplicable,
            theorem: None,
            reason: None,
            checker: None,
        });
        return;
    }
    let (answer, checker) = match &s.checker {
        None => (
            Answer::Unconfirmed {
                reason: "no checker: sqldiff-check was not found".into(),
            },
            None,
        ),
        // The phase 0 fragment needs the same dialect and target on both
        // sides; the checker would answer the same, so the trees are not sent.
        Some(c) if a.dialect() != b.dialect() || a.artifact.version != b.artifact.version => (
            Answer::Unconfirmed {
                reason: "outside the verified fragment: different dialects or targets".into(),
            },
            Some(c.name()),
        ),
        Some(c) => (c.check(&Request::new(a, b)), Some(c.name())),
    };
    match answer {
        Answer::Confirmed { theorem } => {
            r.proof = Some(Proof {
                status: Status::Confirmed,
                theorem: Some(theorem),
                reason: None,
                checker,
            });
        }
        Answer::Unconfirmed { reason } => {
            if s.mode == Mode::Require {
                r.verdict_before_proof = Some(r.verdict());
                r.unknown_reasons
                    .push(format!("not confirmed by the proof checker: {reason}"));
                r.settle();
            }
            r.proof = Some(Proof {
                status: Status::Unconfirmed,
                theorem: None,
                reason: Some(reason),
                checker,
            });
        }
    }
}

/// A checker that answers from a fixed function; for tests.
#[derive(Debug)]
pub struct FnChecker(pub fn(&Request) -> Answer);

impl Checker for FnChecker {
    fn name(&self) -> String {
        "test checker".into()
    }
    fn check(&self, req: &Request) -> Answer {
        (self.0)(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqldiff_syntax::{parse, Dialect};

    fn line(text: &str, d: Dialect) -> String {
        tree_line(&parse(text, d).syntax())
    }

    #[test]
    fn whitespace_and_comments_are_left_out() {
        let d = Dialect::Tsql;
        assert_eq!(
            line("UPDATE t SET x = 1 WHERE id = 2;", d),
            line("UPDATE t\n  SET x = 1 -- set\n  /* c */ WHERE id = 2;", d)
        );
    }

    #[test]
    fn everything_else_counts() {
        let d = Dialect::Tsql;
        let base = line("UPDATE t SET x = 1 WHERE id = 2;", d);
        for other in [
            "update t SET x = 1 WHERE id = 2;",
            "UPDATE T SET x = 1 WHERE id = 2;",
            "UPDATE t SET x = 1 WHERE id = 2",
            "UPDATE t SET x = 01 WHERE id = 2;",
            "UPDATE t SET x = 1 WHERE id = 2 ;\nGO",
        ] {
            assert_ne!(base, line(other, d), "{other}");
        }
    }

    #[test]
    fn a_batch_separator_is_not_an_alias() {
        // `GO` ends a batch only on a line of its own; elsewhere it can be a
        // column alias. The tokens are the same, the trees are not.
        let d = Dialect::Tsql;
        assert_ne!(line("SELECT 1\nGO\n", d), line("SELECT 1 GO\n", d));
    }

    #[test]
    fn request_lines_have_ten_fields() {
        let d = Dialect::Postgres;
        let r = Request {
            dialect_a: "postgres".into(),
            target_a: "18".into(),
            tree_a: line("SELECT 'a\tb';", d),
            dialect_b: "postgres".into(),
            target_b: "18".into(),
            tree_b: line("SELECT 1;", d),
        };
        let l = r.line(7).unwrap();
        assert_eq!(l.split('\t').count(), 10, "{l}");
        let head = format!("sqldiff-check/2\t7\t{}\tpostgres\t18\t(", schema_hash());
        assert!(l.starts_with(&head), "{l}");
        assert!(l.ends_with("\tend"));
        let bad = Request {
            target_a: "18\tx".into(),
            ..r
        };
        assert!(bad.line(1).is_none());
    }

    #[test]
    fn a_mirror_request_encodes_tables_as_the_checker_reads_them() {
        // The encoding of formal/lean/Sqldiff/Mirror/Request.lean, with its
        // examples: `dbo.Acct` maps to `acct`, columns id (key), qty, total.
        let col = |name: &str, tsql: &str, not_null, key| MirrorColumn {
            name: name.into(),
            tsql: tsql.into(),
            not_null,
            key,
            str: None,
            default: None,
        };
        let r = MirrorRequest {
            row_counts: false,
            routines: Vec::new(),
            catalog: vec![("p".into(), "ib".into())],
            tables: vec![("dbo.Acct".into(), "acct".into())],
            table_catalog: vec![MirrorTable {
                name: "acct".into(),
                cols: vec![
                    col("id", "Id", true, true),
                    col("qty", "Qty", false, false),
                    col("total", "Total", true, false),
                ],
            }],
            binary_collation: true,
            renaming: Vec::new(),
            source: "(R)".into(),
            target: "(R)".into(),
        };
        let l = r.line(1).unwrap();
        let f: Vec<&str> = l.split('\t').collect();
        assert_eq!(f[4], "0;64626f2e41636374:61636374");
        assert_eq!(
            f[5],
            "70:ib,61636374=6964.ink.4964/717479.i.517479/746f74616c.in.546f74616c,!binary-collation"
        );
        // C3c: string columns and defaults, as Request.lean's examples
        // decode them (`name varchar(20) NOT NULL`, `city nvarchar(30)`
        // DEFAULT 'Paris', `qty int NOT NULL DEFAULT -1`).
        let mut r = r;
        r.binary_collation = false;
        r.table_catalog[0].cols = vec![
            col("id", "id", true, true),
            MirrorColumn {
                str: Some((20, false)),
                ..col("name", "name", true, false)
            },
            MirrorColumn {
                str: Some((30, true)),
                default: Some(MirrorDefault::Str("Paris".into())),
                ..col("city", "City", false, false)
            },
            MirrorColumn {
                default: Some(MirrorDefault::Int(-1)),
                ..col("qty", "Qty", true, false)
            },
        ];
        let l = r.line(1).unwrap();
        let f: Vec<&str> = l.split('\t').collect();
        assert_eq!(
            f[5],
            "70:ib,61636374=6964.ink.6964/6e616d65.v20n.6e616d65/63697479.w30.43697479.s5061726973/\
             717479.in.517479.i-1"
        );
    }
}
