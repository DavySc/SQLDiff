/-
The lowering of T-SQL and PL/pgSQL routines, as the Rust parser reads them
(`Sqldiff.Ast`, generated), into the core. Decision 9
(docs/verified-conversion.md): this file *is* the specification of the C1
constructs, of the C2a scalar slice with the C2b strings, of the C3a data statements, of
the C3b single-table queries and of the C4 error handling and transaction control in both
dialects; there is no other T-SQL or PL/pgSQL semantics to prove it
against. One abstract syntax serves both dialects and the dialect is a
parameter. The lowering is partial: `none` means "outside the spec"
(queries outside C3b, `MERGE`, data statements outside C3a, handlers and
transaction statements outside C4, other settings, ...), and a routine
outside it is never confirmed.

Errors (C2a): an expression of the slice can fail (overflow, division by
zero), and the core then ends the routine with an error observation. That
is what PL/pgSQL does without an `EXCEPTION` block, and what T-SQL does
under `SET XACT_ABORT ON`; a T-SQL statement that fails without it is
rolled back on its own and the procedure goes on. So the T-SQL lowering is
faithful for every run only when the routine aborts on errors
(`abortsT`), and otherwise for the runs that raise none, as in C1. A C2a
confirmation (`Sqldiff.Mirror.inC2a`) asks for one or the other. Errors
from elsewhere (a missing argument without a default, `OUTPUT` on a
parameter that is not one) are not modelled: the rule says what happens
otherwise. A failing callee is not modelled either (`WorldModel.call`
does not fail), but a call counts as a statement that can fail
(`Core.Stmt.failsHere`), so a T-SQL routine with a call is in the C2a
slice only when it aborts on errors: then a callee's error ends both
routines, as any error does. C4 handles errors (below) but still does
not model a callee's errors, so a call is outside the spec where an error
would be handled.

Data (C3a): single-table `INSERT ... VALUES`, `UPDATE` and `DELETE` on a
table the target catalog describes, with the columns of the row in scope
(section "The C3a slice: data statements", each rule with its vendor
citation and probe status). A data statement that fails has no effect
(statement atomicity) and then ends the routine like any error.

Queries (C3b): T-SQL `SELECT @x = col` by primary key against the
converter's PL/pgSQL `FOR` block, `IF [NOT] EXISTS`, a scalar subquery by
key, and `COUNT(*)` into a variable, on a catalog table (section "The C3b
slice: queries", each rule with its citation and probe status).

Tables (C3c): a catalog table may have `varchar(n)`/`nvarchar(n)`
columns, read and written under the C2b bounds, and constant defaults,
which an `INSERT` that leaves the column out stores (section "The C3c
tables", each rule with its citation and probe status).

Errors and transactions (C4): T-SQL `TRY`/`CATCH` and PL/pgSQL
exception blocks, `THROW` and `RAISE`, `BEGIN TRAN`/`COMMIT` in a
bracket, the rollback-and-rethrow idiom, and PL/pgSQL `COMMIT` (section
"The C4 rules", each rule with its citation and probe status).

Read top to bottom: the trusted inputs, names in data statements, the C2a
slice with its vendor citations, the C2b strings, expressions, the C3a
data statements and the C3c tables, the C3b queries, statements, calls, the C4 rules, blocks, the two routines, and the test for "every
expression is in the slice".
-/
import Sqldiff.Ast
import Sqldiff.Core.Semantics
import Sqldiff.Mirror.Names

namespace Sqldiff.Mirror

open Sqldiff.Core (Var Stmt Arg seqList SExpr AOp COp IW DPart)

/-! ## Trusted inputs -/

/-- Facts about the T-SQL deployment, from the deployment profile
(docs/verified-conversion.md, C0 item 6). Trusted: the check cannot see
the databases. -/
structure Deployment where
  /-- The observer profile reads rows-affected counts (decision 1; not
  observed across dialects by default, `ASM-OBSERVER-ROWCOUNT`). -/
  rowCounts : Bool
  /-- Each T-SQL routine a call may reach, `.`-joined as written, and the
  target routine it corresponds to (`dbo.p` to `p`: `ASM-DBO-SCHEMA`).
  Whether two names reach routines that behave alike is a fact about the
  two databases, so it is an input here, not a certificate hint. -/
  routines : List (String × String)
  /-- Each T-SQL table a data statement may name, `.`-joined as written,
  and the target table it corresponds to (`dbo.t` to `t`: the schema
  mapping of `ASM-DBO-SCHEMA`, and for an unqualified name the default
  schema of `ASM-DEFAULT-SCHEMA`), as for routines. Phase C3a. -/
  tables : List (String × String) := []

/-- The type of a catalog column: an integer of a width (C3a: the 32-bit
one; C2c: `smallint`, `bigint`), (C3c) a string column, T-SQL
`varchar(n)` (`wide = false`) or `nvarchar(n)` (`wide = true`) and
PostgreSQL `varchar(n)` with the same `n`, `1 ≤ n ≤ 4000`
(`CatTable.wellFormed`), (C2c) T-SQL `bit` against `boolean`, `date`,
and `datetime2(p)` against `timestamp(p)`, `p ≤ 6` ("The C2c slice:
typed values"). -/
inductive ColKind where
  | int (w : IW)
  | str (n : Nat) (wide : Bool)
  | bit
  | date
  | ts (p : Nat)
deriving DecidableEq, Repr

/-- A column of a target table (C3a): its name as the target catalog
spells it (folded as `pgName` folds; the core's column), its spelling in
the T-SQL table it corresponds to, and its constraints. (C3c) Its type
and its default, see "The C3c tables" below; without them, as in C3a and
C3b, the column is a 32-bit integer (T-SQL `int`, PostgreSQL `integer`)
without a default. -/
structure CatCol where
  name : Core.Col
  tsql : String
  notNull : Bool
  key : Bool
  /-- The column's type (`ColKind`); the 32-bit integer when not given,
  as in C3a and C3b. -/
  kind : ColKind := .int .i32
  /-- (C3c) The default both tables declare for the column, a constant:
  an integer literal (`SExpr.int`), a string literal (`SExpr.str`),
  (C2c) a bit (`SExpr.bool`), a date (`SExpr.date`) or a timestamp
  (`SExpr.ts`) literal. `none`: no default, or `DEFAULT NULL`, which an
  `INSERT` treats alike (the column is NULL). -/
  dflt : Option SExpr := none
deriving Repr

/-- (C3d part 2) A column of a read-only table whose type is outside the
slice (T-SQL `decimal`, `char(n)`, `datetime2(7)`, a computed column, a
column with a `COLLATE` clause, ...): a name only. Its name as the
target catalog spells it (folded), its spelling in the T-SQL table, and
whether it is part of the primary key (then no column of the table is a
key, `CatTable.keyVisible`). It takes part in name resolution
(`Scope.holders`), but no expression may read it ("The C3d tables"). -/
structure OpqCol where
  name : Core.Col
  tsql : String
  key : Bool
deriving Repr

/-- A target table and how the T-SQL table it corresponds to spells its
columns (the state correspondence of decision 1: rows related column by
column, the identity on values). What the catalog asserts about the table
and about its T-SQL counterpart, which the check cannot see: both have
exactly these columns, of these types (`int`/`integer`, or (C3c) the
string types `CatCol.str` names), with these `NOT NULL` and primary key
constraints and (C3c) these constant defaults; no other constraint
(`CHECK`, `UNIQUE`, foreign keys), no other default, no identity,
sequence, computed or generated column, no trigger, rule or row-level
security policy; the primary key is not `DEFERRABLE INITIALLY DEFERRED`
in PostgreSQL and its index does not `IGNORE_DUP_KEY` in SQL Server; and
the PostgreSQL table has no inheritance children or partitions. (C3c)
Indexes other than the primary key's are non-unique, unfiltered and on
plain columns, and no row of the slice can exceed their key size limits
(see "The C3c tables"): they then change no result. String columns have
the database's default collation (no `COLLATE` clause) and, in SQL Server,
were created with `ANSI_PADDING ON`. (Review of C3d part 2) For every
table, plain or read-only: no temporary table with its name exists in the
calling session (in PostgreSQL a caller's `TEMP TABLE t1` shadows `t1`,
since `pg_temp` is searched first, and the converter writes T-SQL `#T1` as
`CREATE TEMP TABLE T1`). The Rust side sends a table only
when its catalogs show all of this; the calling session is
`ASM-TABLES-AS-DECLARED`'s to state.

(C3d part 2) A **read-only** table (`readOnly`) is one the two routines
only read: no lowering writes it (`writeTableOf`). For it the catalog
asserts less, only what a read can see ("The C3d tables" below): both
tables have the columns `cols` and `opaqueCols`, no others, with these names
(an opaque column is a name only, of any type); each column of `cols` has
the type `kind` names, holds no NULL when `notNull` says so, and (a string
column) has the database's default collation; when every primary-key
column, on both sides, is in `cols` with a key type, the primary key is
exactly the columns with `key` and is neither `DEFERRABLE` nor
`INITIALLY DEFERRED` in PostgreSQL;
neither table is under a row-level security policy (SQL Server security
policy, PostgreSQL policy with row security enabled), a rule or dynamic
data masking, and the PostgreSQL table has no inheritance children or
partitions. Everything
else is allowed, because no read sees it: other constraints (`CHECK`,
`UNIQUE`, foreign keys), defaults of any kind, identity, sequences,
computed and generated columns (opaque), indexes, triggers, system
versioning (a plain `FROM` reads the current rows: eval/queries Q17). -/
structure CatTable where
  name : String
  cols : List CatCol
  /-- (C3d part 2) The columns whose type is outside the slice, by name
  only (`OpqCol`); only a read-only table has any. -/
  opaqueCols : List OpqCol := []
  /-- (C3d part 2) The routines only read the table. -/
  readOnly : Bool := false
deriving Repr

/-- The target catalog (trusted like every catalog fact; its hash goes in
the certificate): the parameter modes of the PostgreSQL routines a `CALL`
can reach (PL/pgSQL writes back to the variable in each `OUT` and `INOUT`
position, so the lowering of `CALL` needs them), and the tables data
statements may write (C3a). -/
structure Catalog where
  routines : List (String × List Ast.Mode)
  tables : List CatTable := []
  /-- (C2b) A fact about both databases, from the deployment profile:
  the T-SQL database that holds the routine has a binary default
  collation (a `_BIN` or `_BIN2` collation), and every collation a
  PostgreSQL comparison in the routine can take is deterministic (the
  database's default, and the collation of every argument a caller
  passes). (C3c) A string column of a catalog table has the database's
  default collation in both (the catalog asserts it, `CatTable`), so the
  fact covers comparisons with columns too. Without it, `=` and `<>` on
  strings are outside the slice. The
  answer names it as `ASM-BINARY-COLLATION` where a comparison uses it.
  See "The C2b slice: strings". -/
  binaryCollation : Bool := false
  /-- (C2c) The declared parameter types of the PostgreSQL routines a
  `CALL` can reach, as their DDL spells them, by folded name. A call is in
  the spec only when every argument is shown to have exactly its
  parameter's type ("Calls" below); a routine without an entry here takes
  no arguments in the spec. -/
  routineTys : List (String × List Ast.Ty) := []

/-- The empty catalog. -/
instance : EmptyCollection Catalog := ⟨{ routines := [] }⟩

/-- A dialect and its trusted inputs. `vars` are the PL/pgSQL routine's
variable names (folded), which a name in a data statement may clash with
(`resolveName`). `quals` (C3d) are the other names a PL/pgSQL qualifier
`q.c` may not be: the routine's own name, which PL/pgSQL takes as the
label of the outermost block, so that `f.x` reads the parameter `x`
(`resolveQual`, eval/queries Q12). -/
inductive Dialect where
  | tsql (dep : Deployment) (Γ : Catalog)
  | pg (Γ : Catalog) (vars : List Var) (quals : List String)

def dotted (parts : List String) : String := ".".intercalate parts

/-- A PostgreSQL name as PostgreSQL resolves it. The Rust parser keeps
names as written, except that a quoted name whose case matters arrives
with a leading `"` (`"Foo"` as `"Foo`; a quoted all-lower-case name
arrives without the quote, `sqldiff_syntax::abstract_syntax::quoted_name`).
PostgreSQL folds unquoted names to lower case, so `X` and `x` are one
variable: a name with the leading `"` is kept, every other name is
lower-cased. PostgreSQL folds only ASCII letters, and so does
`String.toLower`; non-ASCII names are outside the spec anyway
(`asciiNames`), since an unquoted `Ä` and a quoted `"Ä"` are one name to
PostgreSQL but not to this rule. Catalog keys are names folded by this
rule. -/
def pgName (n : String) : String :=
  if n.startsWith "\"" then n else n.toLower

/-! ## Names in data statements (C3a)

A data statement brings columns into scope (`Scope`): the target table's,
in `UPDATE` and `DELETE`; none in `INSERT ... VALUES`, whose values are
evaluated before any row exists. A one-part name (`Ast.Expr.Name [x]`) in
an expression resolves as follows (`resolveName`).

* **T-SQL**: a variable is spelt `@x` (`Ast.Expr.Var`), so a name is a
  column, or outside the spec when it is none of the target's. A name
  matches a column only when it is spelt exactly as the catalog's T-SQL
  spelling of the column (`CatCol.tsql`); `wellFormed` asks those to be
  ASCII and distinct up to case, so the match is the same under a
  case-sensitive and a case-insensitive collation. Microsoft Learn,
  "Database identifiers" (identifier comparison follows the collation).
  Not probed.
* **PL/pgSQL**: a name is folded (`pgName`). If it is a column of the
  target and also a variable of the routine (a parameter, a local, or
  `found`: "FOUND is a local variable within each PL/pgSQL function",
  PL/pgSQL "Obtaining the Result Status"), it is outside the spec: "By default, PL/pgSQL will
  report an error if a name in an SQL statement could refer to either a
  variable or a table column" (PL/pgSQL "Variable Substitution"; the
  `plpgsql.variable_conflict` setting or a `#variable_conflict` option
  would resolve it either way instead, so refusing every such name makes
  the lowering independent of that setting and no
  `ASM-PG-VARIABLE-CONFLICT-ERROR` is needed). A column that is not a
  variable is the column; a name that is the table's own reference name
  (the last part of the target's name) and not a column is a whole-row
  reference, outside the spec (and an ambiguity error, too, when a
  variable has that name); (C3d part 2, review) so is, in an `UPDATE` or
  `DELETE`, a system column's name (`pgSystemCols`: `tableoid`, `xmin`,
  `cmin`, `xmax`, `cmax`, `ctid`), which every table has (42702 for a
  variable so spelt, probed, eval/queries Q20); any other name is a
  variable, which must be declared (`wellScoped`). Not probed.
* In both dialects, the column list of `INSERT` and the column a `SET`
  assigns are columns, never variables ("Variable Substitution": in
  `INSERT INTO dest (col) ...`, "col must be a column of dest"), resolved
  the same way
  (`colOf`). A two-part name (`t.c`) is outside the spec in a data
  statement; (C3d) in a query it names a column of the block (below,
  "Names in a query block"). -/

/-- (C3d) A source of a query block: its reference name (the alias, else
the last part of the table's name; T-SQL as written, PostgreSQL folded)
and its columns, each as a statement spells it with the block's core
column (`Core.srcCol`). (C3d part 2) `opaqueNames`: the spellings of its
opaque columns (`OpqCol`), names that resolve to nothing a statement may
read but that count for every name the block resolves (`holders`). -/
structure Src where
  ref : String
  cols : List (String × Core.Col)
  opaqueNames : List String := []
deriving Repr

/-- The columns in scope: each as a statement may write it (the T-SQL
spelling, or the folded PostgreSQL name) and the core column; and, for
PostgreSQL, the target's reference name. Empty outside data statements.
(C3d) In a query, the sources of its block instead (`srcs`), of which an
expression may name those in `vis`: an `ON` sees only the sources of its
own join. -/
structure Scope where
  cols : List (String × Core.Col) := []
  rel : Option String := none
  /-- (C3c, C2c) The kinds of the columns (`CatCol.kind`); a column not
  listed is the 32-bit integer. -/
  colTys : List (Core.Col × ColKind) := []
  /-- (C3d) The sources of a query block, in `FROM` order. -/
  srcs : List Src := []
  /-- (C3d) The positions of the sources an expression may name. -/
  vis : List Nat := []

/-- (C3d) The sources of a block with a column spelt `x` (T-SQL: up to
ASCII case, which is what a case-insensitive collation compares), with
their positions. (C3d part 2) An opaque column counts as any other: the
engines resolve a name against every column of every source, whatever
its type, so a name that is an opaque column of another source is
ambiguous (T-SQL 209, PostgreSQL 42702) and a PL/pgSQL variable spelt as
one is the column-variable conflict (42702; eval/queries Q19). The one
holder of an opaque name resolves to nothing (`resolveName`: it is not in
`Src.cols`), so the name is outside the spec. -/
def Scope.holders (d : Dialect) (sc : Scope) (x : String) : List (Src × Nat) :=
  sc.srcs.zipIdx.filter fun (s, _) => (s.cols.map (·.1) ++ s.opaqueNames).any fun k => match d with
    | .tsql _ _ => k.toLower == x.toLower
    | .pg _ _ _ => k == x

/-- (C3d part 2, review) PostgreSQL's system columns, which every table
has without declaring them (PostgreSQL 18 manual, 5.6 "System Columns",
from memory): a PL/pgSQL variable spelt as one, in a query over a table or
in an `UPDATE`'s or `DELETE`'s `WHERE`, is the column-variable conflict
(42702; probed for all six, eval/queries Q20). In a scope with a table such
a name resolves to nothing (`resolveName`). No catalog column has such a
name: PostgreSQL refuses a user column that does. -/
def pgSystemCols : List String := ["tableoid", "xmin", "cmin", "xmax", "cmax", "ctid"]

/-- A one-part name: a column (`inl`), a variable (`inr`), or outside the
spec (`none`). Outside data statements a T-SQL name is outside the spec
and a PostgreSQL name is a variable, as in C1. (C3d) In a query block,
see "Names in a query block" below. (C3d part 2, review) A PostgreSQL
name that is a system column (`pgSystemCols`) is outside the spec in a
query block and in an `UPDATE` or `DELETE`: every table has it. -/
def resolveName (d : Dialect) (sc : Scope) (x : String) : Option (Sum Core.Col Var) :=
  if !sc.srcs.isEmpty then
    match d with
    | .tsql _ _ => match sc.holders d x with
      | [(s, i)] => if sc.vis.contains i then (lookup s.cols x).map .inl else none
      | _ => none
    | .pg _ vars _ =>
      let n := pgName x
      if n == "sqlstate" || n == "sqlerrm" || pgSystemCols.contains n || sc.srcs.any (·.ref == n)
      then none else
      match sc.holders d n with
      | [] => some (.inr n)
      | [(s, i)] =>
        if vars.contains n || !sc.vis.contains i then none else (lookup s.cols n).map .inl
      | _ => none
  else
  match d with
  | .tsql _ _ => (lookup sc.cols x).map .inl
  | .pg _ vars _ =>
    let n := pgName x
    -- C4: the error variables of a handler are outside the spec.
    if n == "sqlstate" || n == "sqlerrm" then none else
    -- C3d part 2, review: a system column of the statement's table.
    if sc.rel.isSome && pgSystemCols.contains n then none else
    match lookup sc.cols n with
    | some c => if vars.contains n then none else some (.inl c)
    | none => if sc.rel == some n then none else some (.inr n)

/-- (C3d) A two-part name `q.c` in a query block: column `c` of the one
source whose reference name is `q` (see "Names in a query block" below);
outside a query, nothing. -/
def resolveQual (d : Dialect) (sc : Scope) (q c : String) : Option Core.Col :=
  match d with
  | .tsql _ _ =>
    match sc.srcs.zipIdx.filter (fun (s, _) => s.ref.toLower == q.toLower) with
    | [(s, i)] => if s.ref == q && sc.vis.contains i then lookup s.cols c else none
    | _ => none
  | .pg _ vars quals =>
    let qn := pgName q
    if vars.contains qn || quals.contains qn then none else
    match sc.srcs.zipIdx.filter (fun (s, _) => s.ref == qn) with
    | [(s, i)] => if sc.vis.contains i then lookup s.cols (pgName c) else none
    | _ => none

/-! ## The C2a slice: types

Each rule of the slice below states its claim, the vendor text it rests
on (Microsoft Learn page title for T-SQL, PostgreSQL 18 manual section
for PostgreSQL), and whether the repository's engine probes cover it.
"eval/casts" is `eval/casts/{mssql,postgres}.sql` and their `.out` files,
run on SQL Server 2022 and PostgreSQL 18 on 2026-10-26 with constant
operands in `SELECT`; "not probed" means no engine run in the repository
shows it. The rules are trusted content (decision 5 of
docs/verification-roadmap.md): an engine run can refute one, not confirm
it.

**Types.** The slice has one value type, the 32-bit signed integer, plus
the three-valued booleans of conditions and the NULL literal.

* T-SQL `int`, and its synonym `integer`, is a 32-bit integer from
  -2^31 to 2^31-1 (Microsoft Learn, "int, bigint, smallint, and tinyint
  (Transact-SQL)"; "Data type synonyms (Transact-SQL)": `integer` is
  `int`). The Rust parser upper-cases type names
  (`sqldiff_syntax::abstract_syntax::Parser::ty`); the match is
  case-insensitive. Not probed as a variable type.
* PostgreSQL `integer`, `int` and `int4` are one type with that range
  (PostgreSQL manual 8.1.1 "Integer Types", Table 8.2 and Table 8.1
  aliases). Not probed as a variable type. A user type named `int4`
  earlier in `search_path` than `pg_catalog` would shadow the last
  spelling; `pg_catalog` is searched first unless the path names it
  later (5 "Data Definition", "Schemas", "The System Catalog Schema"),
  which this rule assumes.
* Type arguments (`int(4)`) put a declaration outside the slice. -/

/-- The type of an expression of the slice. `int w` is a signed integer
of width `w` (C2c: `smallint`, `int`, `bigint`; before C2c the one
integer type was the 32-bit one). `nul` is the NULL literal,
which takes the type from where it stands (a value position:
assignment, `COALESCE`/`ISNULL` argument, `CASE` result). `str n wide`
is a string (C2b, "The C2b slice: strings"): `n` bounds the length of
every value it can have (`none`: no bound), in T-SQL in bytes for
`varchar` and in UTF-16 code units for `nvarchar` (so also in
characters), in PostgreSQL in characters; `wide` says that a T-SQL value
may hold characters outside the database's code page (it comes from an
`nvarchar`); it is always `false` in PostgreSQL. (C2c) `bit` is a T-SQL
`bit` or a PostgreSQL `boolean` value (a three-valued boolean that can
be stored; `bool` is a condition), `date` a date and `ts p` a timestamp
without time zone with `p` fractional digits of its seconds ("The C2c
slice: typed values"). -/
inductive STy where
  | int (w : IW)
  | bool
  | nul
  | str (n : Option Nat) (wide : Bool)
  | bit
  | date
  | ts (p : Nat)
deriving DecidableEq, Repr

/-- The declared types of a routine's variables, by lowered name. -/
abbrev TyEnv := List (Var × Ast.Ty)

/-- The width of a declared integer type of the slice (C2c: "Integer
widths" below), `none` for any other type. -/
def intWidth : Dialect → Ast.Ty → Option IW
  | .tsql _ _, .Ty n [] =>
    let u := n.toUpper
    if u == "INT" || u == "INTEGER" then some .i32
    else if u == "BIGINT" then some .i64
    else if u == "SMALLINT" then some .i16
    else none
  | .pg _ _ _, .Ty n [] =>
    let u := n.toUpper
    if u == "INT" || u == "INTEGER" || u == "INT4" then some .i32
    else if u == "BIGINT" || u == "INT8" then some .i64
    else if u == "SMALLINT" || u == "INT2" then some .i16
    else none
  | _, _ => none

/-- The declared type is an integer type of the slice (any width). -/
def intType (d : Dialect) (ty : Ast.Ty) : Bool := (intWidth d ty).isSome

/-- The width of an integer variable. -/
def intVarW (d : Dialect) (env : TyEnv) (x : Var) : Option IW :=
  (lookup env x).bind (intWidth d)

def intVar (d : Dialect) (env : TyEnv) (x : Var) : Bool := (intVarW d env x).isSome

/-- A value position that T-SQL converts to `int` (the return status, a
call argument) takes an integer of at most 32 bits or the NULL literal:
a `smallint` widens without a conversion that could fail. -/
def valTy : STy → Bool
  | .int .i16 | .int .i32 => true
  | .nul => true
  | _ => false

/-- The widths, ordered: a value of a narrower width fits a wider one. -/
def _root_.Sqldiff.Core.IW.rank : IW → Nat
  | .i16 => 16
  | .i32 => 32
  | .i64 => 64

def _root_.Sqldiff.Core.IW.le (a b : IW) : Bool := a.rank ≤ b.rank

/-- The width of an arithmetic result: the wider operand's. -/
def _root_.Sqldiff.Core.IW.join (a b : IW) : IW := if a.le b then b else a

/-! ## The C2a slice: literals

An integer literal is in the slice when it is decimal digits, optionally
after a `-` (the Rust parser folds a unary minus on a number into the
number's text, `-5` as `Num "-5"`), without a leading zero, and in the
32-bit range.

* T-SQL: an integer constant in the `int` range is `int`; one outside it
  is `numeric` (`decimal`), so `2147483648` is outside the slice
  (Microsoft Learn, "Constants (Transact-SQL)", integer constants).
  Probed (eval/casts: `2147483647` and `-2147483648` are `int`,
  `2147483648` is `numeric(10,0)`).
* PostgreSQL: "A numeric constant that contains neither a decimal point
  nor an exponent is initially presumed to be type integer if its value
  fits in type integer (32 bits)" (4.1.2.6 "Numeric Constants"); larger
  ones are `bigint` or `numeric`. Probed (eval/casts: `2147483647` and
  `-2147483648` are `integer`, `2147483648` is `bigint`).
* Underscores (PostgreSQL 16 and later accept `1_000`), hexadecimal,
  exponents and leading zeros are outside the slice: `String.toInt?`
  accepts underscores, so this rule reads the digits itself.
* (C2c) PostgreSQL: a constant outside the 32-bit range that fits 64
  bits "is presumed to be type bigint" (4.1.2.6), typed `int .i64`
  (`int64Lit?`). Probed, eval/values V2 (`2147483648` is `bigint`). In
  T-SQL such a constant is `numeric(p,0)` ("Constants (Transact-SQL)":
  "Integer constants greater than 2,147,483,647 are converted to the
  decimal data type, not the bigint data type"; probed, eval/values V2:
  `2147483648` is `numeric(10,0)`), outside the slice as an operand
  (decimals are a later part), except as the whole value stored into a
  `bigint` or compared with one (`coerce`), where the `numeric` to
  `bigint` conversion is exact and the comparison converts the `bigint`
  to `numeric` without a change of value ("Data type precedence"). -/

def digitsVal : List Char → Nat → Option Nat
  | [], acc => some acc
  | c :: cs, acc =>
    if '0' ≤ c && c ≤ '9' then digitsVal cs (acc * 10 + (c.toNat - '0'.toNat)) else none

/-- A decimal integer constant of at most `maxDigits` digits, optionally
after `-`, without a leading zero, within the width. -/
def intLitW? (w : IW) (maxDigits : Nat) (s : String) : Option Int :=
  let (neg, ds) : Bool × List Char := match s.toList with
    | '-' :: ds => (true, ds)
    | ds => (false, ds)
  if ds.isEmpty || ds.length > maxDigits || (ds.length > 1 && ds.head? == some '0') then none
  else (digitsVal ds 0).bind fun n =>
    let v : Int := if neg then -(n : Int) else n
    if w.fits v then some v else none

def intLit? (s : String) : Option Int := intLitW? .i32 10 s

/-- (C2c) A constant of the 64-bit range (PostgreSQL `bigint`). -/
def int64Lit? (s : String) : Option Int := intLitW? .i64 19 s

/-! ## The C2a slice: operators

**Constant subexpressions.** PostgreSQL's planner evaluates a
subexpression without variables when it plans the statement, even in a
`CASE` branch that is never taken: "`CASE WHEN x > 0 THEN 1 ELSE 1/0 END`
will likely result in a division-by-zero failure due to the planner
trying to simplify the constant subexpression" (9.18.1 "CASE"; 4.2.14
"Expression Evaluation Rules"). T-SQL makes no such promise either way. So
an expression without variables whose evaluation fails is outside the
slice (`closedSafe`); a constant one that does not fail means the same
folded or not. PL/pgSQL evaluates a simple expression with a generic plan,
in which variables are parameters, not constants, so only expressions
without variables are folded. Not probed.

**Evaluation order.** Neither vendor fixes the order in which operands
are evaluated, nor that an operand whose value is not needed is skipped:
PostgreSQL: "The order of evaluation of subexpressions is not defined"
and "if the result of an expression can be determined by evaluating only
some parts of it, then other subexpressions might not be evaluated at
all" (4.2.14). The core evaluates both operands of every strict operator,
left to right. That is safe because (1) the only errors in the slice are
arithmetic ones, which decision 1 compares by kind (`ErrClass`), so which
of two failing operands fails first is not observed, and (2) `AND` and
`OR`, the operators whose result one operand can decide, are in the slice
only when neither operand can raise (`SExpr.mayRaise`), so skipping one
changes nothing. `CASE`, `COALESCE` and `ISNULL` are lazy (below).

**Arithmetic** (`+ - * / %`) on two `int` operands is `int`, NULL if
either operand is NULL, and raises on overflow.

* T-SQL: "+ (Addition)", "- (Subtraction)", "* (Multiplication)" and
  "/ (Divide) (Transact-SQL)": the result has "the data type of the
  argument with the higher precedence" ("Data type precedence
  (Transact-SQL)"), so `int` for two `int`s; an `int` result out of range
  is "Arithmetic overflow error converting expression to data type int"
  (error 8115). Probed (eval/casts: `2147483647 + 1` and
  `CAST(-2147483648 AS int) / -1` raise 8115).
* PostgreSQL: Table 9.4 in 9.3 "Mathematical Functions and Operators";
  `integer` arithmetic raises "integer out of range" (SQLSTATE 22003).
  Probed (eval/casts: the same two raise it).

**Division** truncates toward zero; a zero divisor raises.

* T-SQL: "If an integer dividend is divided by an integer divisor, the
  result is an integer that has any fractional part of the result
  truncated" ("/ (Divide) (Transact-SQL)"); division by zero is error
  8134. Probed (eval/casts: `7 / 2` is 3, `-7 / 2` is -3, `1 / 0` raises
  8134).
* PostgreSQL: "Division (for integral types, division truncates the
  result towards zero)" (Table 9.4); SQLSTATE 22012 "division by zero".
  Probed (eval/casts: the same results).

**Remainder** takes the sign of the dividend. In the slice only with a
literal divisor other than 0 and -1, so it never raises.

* T-SQL: "% (Modulus) (Transact-SQL)". PostgreSQL: Table 9.4, "Modulo
  (remainder)". Probed (eval/casts: `7 % -2` is 1, `-7 % 2` is -1 on
  both).
* Why the divisor is restricted: `-2147483648 % -1` is 0 in PostgreSQL
  (`int4mod` special-cases -1) and not probed in SQL Server, and `x % 0`
  is not probed in either. With a variable divisor `%` stays on the C1
  path (`bin:%`).

**Negation** `-x` of an `int` is `int`; `-(-2147483648)` overflows.
T-SQL: "- (Negative) (Transact-SQL)"; PostgreSQL: Table 9.4 (`- integer`)
and 8.1.1. Not probed (the parser folds `-5` into a literal; this is
`-x` on a variable or an expression).

*Precedence.* The rule applies to the tree the parser builds, so the
parser must bind unary minus as each dialect does. T-SQL puts unary minus
at the level of binary `+` and `-`, below `*`, `/` and `%` ("Operator
precedence (Transact-SQL)": level 2 is `*` (Multiplication), `/`
(Division), `%` (Modulus); level 3 is `+` (Positive), `-` (Negative), `+`
(Addition), `+` (Concatenation), `-` (Subtraction), `&`, `^`, `|`; from
memory), so T-SQL `-@a * @b` is `-(@a * @b)`. PostgreSQL binds unary
minus tighter than `*` (4.1.6 "Operator Precedence", Table 4.2: unary
`+ -` above `* / %`), so `-a * b` is `(-a) * b`. The two trees differ in
when they overflow: on SQL Server `-@a * @b` with `@a = -2147483648` and
`@b = 0` returns 0 (probed, 2026-10-28), where `(-@a) * @b` would raise
8115 on the negation. The Rust parser reads T-SQL `-@a * @b` as
`-(@a * @b)` (`sqldiff_syntax`, being fixed at the time of writing) and
PostgreSQL `-a * b` as `(-a) * b`, so a conversion that copies the text
unchanged gives two different trees and is refused; `-(a * b)` on the
PostgreSQL side is confirmed. The negation rule itself needs no change:
it is the same in both dialects once the tree is right.

**Comparisons** `= <> < > <= >=` of two `int`s are three-valued: UNKNOWN
(NULL) when either side is NULL. The NULL literal as an operand is outside
the slice (`x = NULL` is UNKNOWN under the default settings, but PostgreSQL's
`transform_null_equals` and T-SQL's `ANSI_NULLS OFF` change it).

* T-SQL: "Comparison Operators (Transact-SQL)" and "SET ANSI_NULLS
  (Transact-SQL)": with `ANSI_NULLS ON`, a comparison with a NULL value
  is UNKNOWN. A routine keeps the `ANSI_NULLS` setting it was created with
  ("SET ANSI_NULLS": "the setting ... at the time the stored procedure is
  created"); `ON` is assumed (`ASM-TSQL-ANSI-NULLS` in a C2a answer that
  has `=` or `<>`). `!=` arrives as `<>`. Not probed.
* PostgreSQL: "Ordinary comparison operators yield null (signifying
  “unknown”), not true or false, when either input is null" (9.2
  "Comparison Functions and Operators"). Not probed.

**`AND`, `OR`, `NOT`** follow the three-valued truth tables: `AND` is
FALSE if either side is FALSE, else UNKNOWN if either is UNKNOWN; `OR`
dually; `NOT UNKNOWN` is UNKNOWN. T-SQL: "AND (Transact-SQL)", "OR
(Transact-SQL)", "NOT (Transact-SQL)" (their result tables);
PostgreSQL: 9.1 "Logical Operators" (the truth table). Operands that can
raise put the expression outside the slice (evaluation order, above).
Not probed.

**`IS NULL`, `IS NOT NULL`** of an `int` are TRUE or FALSE, never
UNKNOWN. T-SQL: "IS NULL (Transact-SQL)"; PostgreSQL: 9.2. Not probed.

**`COALESCE(x1, ..., xn)`**, n ≥ 2, every argument `int` or the NULL
literal and at least one `int`: the first argument that is not NULL, NULL
if all are; an argument after the first non-NULL one is not evaluated.
Lowered as `coalesce x1 (coalesce x2 ...)`, which means exactly that.

* T-SQL: "COALESCE (Transact-SQL)": the result type is "the data type of
  expression with the highest data type precedence", `int` here; "the
  COALESCE expression is a syntactic shortcut for the CASE expression"
  (`CASE WHEN x1 IS NOT NULL THEN x1 ... ELSE xn END`), so it evaluates
  in order and stops; `x1` may be evaluated twice, which changes nothing
  for an expression without side effects; all-NULL-literal arguments are
  an error, hence "at least one `int`". Not probed for `int` (eval/
  bit-boolean probes `bit` and the type rule).
* PostgreSQL: 9.18.2 "COALESCE": "Like a CASE expression, COALESCE only
  evaluates the arguments that are needed to determine the result"; the
  result type by 10.5 "UNION, CASE, and Related Constructs": `integer`
  for `integer` and unknown-type NULL arguments. Not probed.

**T-SQL `ISNULL(x, y)`** with `x` an `int` expression (not the NULL
literal), `y` an `int` expression or the NULL literal, and `y` unable to
raise (`SExpr.mayRaise`): the same core term as `COALESCE(x, y)`. "ISNULL
(Transact-SQL)": "Returns the same type as check_expression" and
replaces NULL with replacement_value, which "must be of a type that is
implicitly convertible to the type of check_expression". With `x` and
`y` both `int` there is no conversion, so the value is `x` if not NULL,
else `y`: what `COALESCE` gives. The page does not say whether `y` is
evaluated when `x` is not NULL; when `y` cannot raise and has no side
effects that is not observable, hence the precondition. (With other
types `ISNULL` converts `y` to `x`'s type and `COALESCE` does not:
`ASM-ISNULL-TYPE`, outside the slice.) Not probed for `int`
(eval/bit-boolean shows the type rule with `bit`).

**Searched `CASE WHEN c1 THEN r1 ... [ELSE e] END`**, every `ci` a
condition of the slice, every `ri` and `e` an `int` or the NULL literal,
at least one `int`: the first `ri` whose `ci` is TRUE (FALSE and UNKNOWN
go on), else `e`, else NULL. Lowered as `cond c1 r1 (cond c2 r2 ...)`.

* T-SQL: "CASE (Transact-SQL)": "Evaluates, in the order specified,
  Boolean_expression for each WHEN clause" and returns the
  result_expression of the first that is TRUE, else the ELSE value, else
  NULL; "The CASE expression evaluates its conditions sequentially and
  stops with the first condition whose condition is satisfied" (the
  exceptions it lists are aggregates, outside the slice); the result type
  is the highest-precedence type, `int` here. Not probed.
* PostgreSQL: 9.18.1 "CASE" (same rule; the constant-folding caveat is
  handled above) and 10.5 for the type. Not probed.
* The simple form `CASE x WHEN v THEN ...` stays on the C1 path.

**`IF` and `WHILE`** run their statements only when the condition is
TRUE: FALSE and UNKNOWN take the `ELSE` branch or leave the loop
(`Sqldiff.Core.isTrue`). T-SQL: "IF...ELSE (Transact-SQL)" and "WHILE
(Transact-SQL)" (the statement runs when the Boolean_expression is TRUE).
PostgreSQL: PL/pgSQL "Control Structures", "Conditionals" ("The
statements between THEN and END IF will be executed if the condition is
true. Otherwise, they are skipped") and "Simple Loops" (`WHILE` repeats
"so long as the boolean-expression evaluates to true"). Not probed.

**An error ends the routine.** PostgreSQL: "By default, any error
occurring in a PL/pgSQL function aborts execution of the function and the
surrounding transaction" (PL/pgSQL "Control Structures", "Trapping
Errors"). T-SQL only under `SET XACT_ABORT ON` (`lowerSetting`). The
caller observes the error's kind, not its number (decision 1; overflow
and division by zero are both `arithmetic`). Not probed in a routine
(eval/casts probes the errors in plain `SELECT`s). -/

/-! ## The C2b slice: strings

C2b adds character strings to the slice: variables and parameters of a
string type, string literals, concatenation, `=` and `<>` (under a
trusted collation fact only), `RTRIM`, `LEN`, `IS [NULL]`, and
`COALESCE`, `ISNULL` and `CASE` over strings. Each rule states its claim,
the vendor text, and its probe status, as for C2a. **Every vendor quote
in this section was written from memory**: the vendor pages could not be
reached when it was written (2026-10-28), so reviewers check each quote
against the page named before this section is relied on; "from memory"
below marks the ones the author is least sure of. A value is a sequence
of characters (`Sqldiff.Core.Val.str`); the empty string is not NULL in
either dialect (incidentally probed: eval/casts/assign, `''` into an
`int` column is 0 in SQL Server and an input error in PostgreSQL, not
NULL). Strings never meet integers in the slice: an operator,
assignment or `COALESCE` that would convert between them is outside it.

**Types, T-SQL.** `varchar(n)` (also spelt `char varying(n)`,
`character varying(n)`) and `nvarchar(n)`, `1 ≤ n ≤ 4000`. For `varchar`
`n` is "the string size in bytes" ("char and varchar (Transact-SQL)"),
for `nvarchar` "the string size in byte-pairs" ("nchar and nvarchar
(Transact-SQL)"); a character takes at least one byte or byte-pair, so
`n` also bounds the number of characters, and a `varchar` value holds
only characters of the code page of the database's collation. The cap of
4000 (not 8000 for `varchar`) keeps every value of the slice under both
concatenation limits below. Outside: `max`, no length (it means 1 in a
declaration: "When n isn't specified in a data definition or variable
declaration statement, the default length is 1"), `char(n)`/`nchar(n)`
(blank-padded), `sysname`, `text`/`ntext`. Probed: eval/casts
(`CAST('abcdef' AS varchar(3))` is `abc`, `CAST(N'abcdef' AS
nvarchar(3))` is `abc`; `CAST(N'éééé' AS varchar(3))` is `ééé` under the
probe server's single-byte collation).

**Types, PostgreSQL.** `varchar(n)`, `character varying(n)`, `char
varying(n)`, `1 ≤ n ≤ 10485760`: "strings up to n characters (not
bytes) in length"; `varchar` without `n` and `text`: "the type accepts
strings of any size" (8.3 "Character Types"). Outside: `char(n)`,
`bpchar` and PostgreSQL `N'..'` (type `character`, blank-padded; the Rust
parser refuses those literals), `name`, `citext`. A user type named
`text` or `varchar` earlier in `search_path` than `pg_catalog` would
shadow these names, as for `int4` above (assumed not to exist). Not
probed as variable types.

**PostgreSQL parameters carry no length.** "Parenthesized type modifiers
(e.g., the precision field for type numeric) are discarded by CREATE
FUNCTION" (CREATE FUNCTION, Notes; from memory, and that CREATE PROCEDURE
does the same): a `varchar(10)` parameter holds any string. So a
PostgreSQL parameter is read as having no length bound (`envP`), and a
write to one is still held to its declared length (`envPDecl`), which is
sound whether or not the modifier is kept. Probed on PostgreSQL 18.6
(2026-10-28, "String stores, on every path"): parameters, results and
parameter defaults keep no length; locals do.

**T-SQL parameters truncate.** A string argument longer than the
parameter is silently truncated (probed: eval/casts/assign, `EXEC #q
'abcdef'` with `@x varchar(3)` sees `abc`), and a `varchar` parameter
replaces characters outside its code page; PostgreSQL does neither. The
core binds arguments as they are, so the T-SQL lowering is faithful only
for arguments that fit: `ASM-TSQL-STRING-ARGS` (`Sqldiff.Mirror.residual`)
is the caller's promise that every string argument of a T-SQL string
parameter (including the value passed to an `OUTPUT` parameter) has at
most `n` bytes (`varchar(n)`) or byte-pairs (`nvarchar(n)`) and, for
`varchar`, only characters of the database's code page. Strings passed
to a call, and `OUTPUT` arguments that write back into a string
variable, are outside the slice (the conversions at the callee's
boundary are the callee's and the dialects differ there).

**Assignment converts, differently.** T-SQL truncates a string that is
too long for its target, silently (probed: eval/casts/assign, `DECLARE
@v varchar(3) = 'abcdef'` gives `abc`; "char and varchar": "values that
are too long for the new data type are truncated"), and replaces
characters a `varchar` cannot hold (from memory: with `?`); PostgreSQL
raises "value too long for type character varying(3)" (probed:
eval/casts/assign, `DECLARE v varchar(3) := 'abcdef'`), or truncates
when the excess characters are all spaces (8.3). So a string is stored
(`SET`/`:=`, a `DECLARE` default, a parameter default, a function's
`RETURN`) only when its bound is at most the target's length
(`fitsTy`), and in T-SQL a value that may hold characters outside the
code page (`wide`) is never stored in a `varchar`: then neither engine
changes the value. A PostgreSQL target without a length takes any
string. A function's declared return length is checked the same way on
both sides (for PostgreSQL that is sound whether or not the modifier is
kept, as for parameters).

**Literals.** `'...'` whose characters are all printable ASCII
(U+0020 to U+007E) and not a backslash, with bound `max 1 length` (an
empty T-SQL literal is typed `varchar(1)`, from memory; the bound only
needs to be at least the value's length). The Rust parser reads T-SQL
`'ab'` and `N'ab'` to the same `Str` (`sqldiff_syntax::abstract_syntax::
string_value`), so a literal must mean the same as either: ASCII
characters are themselves in every code page SQL Server uses and in
Unicode, and take one byte or byte-pair each (from memory; reviewers
check the code pages in use). PostgreSQL: with
`standard_conforming_strings` on (the default) a backslash is an
ordinary character and with it off it escapes (4.1.2.1, 4.1.2.2);
without backslashes both read the same. A doubled quote is one quote in
both. Probed: eval/casts (`N'ab'` is `nvarchar` in T-SQL and `character`
in PostgreSQL).

**Concatenation.** T-SQL `a + b` and PostgreSQL `a || b`, both operands
strings (not the NULL literal): the characters of `a` then those of `b`,
NULL if either is NULL.

* T-SQL: "+ (String Concatenation) (Transact-SQL)"; "If the result of
  the concatenation of strings exceeds the limit of 8,000 bytes, the
  result is truncated" (from memory), so every string expression of the
  slice has a bound of at most 4000, which is under 8000 bytes and 4000
  byte-pairs. NULL: "When SET CONCAT_NULL_YIELDS_NULL is ON,
  concatenating a null value with a string yields a NULL result"; with
  it OFF a NULL operand is an empty string ("SET CONCAT_NULL_YIELDS_NULL
  (Transact-SQL)", from memory). It is a session setting fixed at run time, not
  stored with the routine, so a confirmation that concatenates rests on
  `ASM-CONCAT-NULL`, unless the routine starts with `SET
  CONCAT_NULL_YIELDS_NULL ON` (`lowerSetting`, `concatNullOn`). The
  result type is the operands' type with the higher precedence
  (`nvarchar` over `varchar`: a lossless conversion). T-SQL `||` (SQL
  Server 2025) is outside. Not probed.
* PostgreSQL: 9.4 "String Functions and Operators", `text || text →
  text`; `varchar` operands are `text` (binary coercible); the operator's
  function is strict, so a NULL operand gives NULL (from memory, also for
  the `anynonarray` variants, which the slice never selects since both
  operands are strings). No length limit applies below 1 GB. Not probed.

**`=` and `<>` on strings**, only when the catalog says
`binaryCollation` (trusted; `ASM-BINARY-COLLATION` in the answer's
residual):

* T-SQL: variables, parameters and literals take the database's default
  collation ("Collation precedence (Transact-SQL)": coercible-default),
  which the fact says is `_BIN` or `_BIN2`: characters compare by code
  point (BIN2) or, for equality, by identity (BIN). Before comparing,
  SQL Server pads the shorter operand with spaces, whatever the
  collation, so `'a' = 'a '` is TRUE ("SQL Server follows the ANSI/ISO
  SQL-92 specification ... on how to compare strings with spaces",
  Microsoft support article "How SQL Server compares strings with
  trailing spaces"; from memory). Two strings are equal after padding
  exactly when they are equal without their trailing spaces, so T-SQL
  `a = b` is `streq (rtrim a) (rtrim b)`. A `varchar` operand is
  converted to `nvarchar` when the other is one, which keeps its
  characters. Not probed.
* PostgreSQL: on `text` and `varchar`, trailing spaces are
  "semantically significant" (8.3, from memory), and under a deterministic collation
  strings are equal "only if they consist of the same byte sequence"
  ("Collation Support", "Nondeterministic Collations"): `a = b` is
  `streq a b`. A
  PL/pgSQL parameter compares under the collation of the caller's
  argument ("Collation of PL/pgSQL Variables"), which is why the fact
  covers callers. Not probed.
* So T-SQL `@a = @b` matches PostgreSQL `rtrim(a) = rtrim(b)` and not
  `a = b`. `<>` is `NOT (... = ...)` on both sides. `mkRtrim` folds the
  `rtrim` of a literal and of an `rtrim` (`mkRtrim_eval`), so
  `@a = 'x'` matches `rtrim(a) = 'x'`. `ANSI_NULLS` applies as for
  integers (`ASM-TSQL-ANSI-NULLS`). `<`, `>`, `LIKE`, and every
  comparison under a case- or accent-insensitive collation (T-SQL's
  default) are outside the slice: their result depends on collation
  tables the core does not have (`ASM-CI-COLLATION` on the Rust side).

**`RTRIM(x)`**, one string argument: `x` without its trailing spaces
(U+0020 only). T-SQL: "RTRIM (Transact-SQL)": "Returns a character
string after truncating all trailing spaces" (from memory; the two-argument form of
SQL Server 2022 is outside). PostgreSQL: 9.4, `rtrim(string text [,
characters text])`: "Removes the longest string containing only
characters in characters (a space by default) from the end of string"
(one argument only). Not probed.

**`LEN(x)`** (T-SQL) and **`length(x)`**, `char_length`,
`character_length` (PostgreSQL), one string argument, an `int`. T-SQL:
"LEN (Transact-SQL)": "Returns the number of characters of the specified
string expression, excluding trailing spaces", and "If you use SC
collations, the returned integer value counts UTF-16 surrogate pairs as
a single character" (otherwise as two; from memory): lowered `len (rtrim x)`, which
counts code points, so a confirmation that uses `LEN` rests on
`ASM-TSQL-LEN-BMP` (no character outside the Basic Multilingual Plane
reaches `LEN`, or the collation is supplementary-character aware).
PostgreSQL: 9.4, `length(text)` "Returns the number of characters in the
string": lowered `len x`. So T-SQL `LEN(x)` matches PostgreSQL
`length(rtrim(x))`, which the converter writes. Not probed (eval/casts
uses `LEN` only to measure strings of `x`).

**`COALESCE`, `CASE`** over strings: as for integers, every value
argument a string or the NULL literal, at least one a string; the value
is the chosen argument's, unchanged. T-SQL: the result type is the
argument type of the highest precedence and, for strings of one type,
the largest length (from memory: the pages state the precedence, not
the length), so no argument is truncated. PostgreSQL: 10.5 resolves
`text`/`varchar`/unknown arguments to `text` or `varchar`, without a
length. The bound is the largest argument's bound. Not probed.

**T-SQL `ISNULL(x, y)`** over strings: "Returns the same type as
check_expression", so `y` is converted to `x`'s type and a longer `y` is
truncated ("ISNULL (Transact-SQL)": "replacement_value can be truncated
if replacement_value is longer than check_expression", from memory;
`ASM-ISNULL-TYPE` on the Rust side).
In the slice only when `x` is a variable or parameter, whose type is its
declaration (`varchar(n)`/`nvarchar(n)`), and `y` is the NULL literal or
a string that cannot raise, whose bound is at most `n`, and, when `x` is
a `varchar`, that is not `wide`: then the conversion keeps `y` as it is
and `ISNULL(x, y)` is `COALESCE(x, y)`. The result has `x`'s type. Not
probed for strings.

**`IS [NOT] NULL`** of a string: TRUE or FALSE, as for integers. -/

/-! ## The C2c slice: typed values

C2c adds values beyond `int` and strings: the integer widths `smallint`
and `bigint`, T-SQL `bit` against PostgreSQL `boolean`, and `date` and
`datetime2(p)` (`p ≤ 6`) against PostgreSQL `date` and `timestamp(p)`,
with their literals, comparisons, stores and the day/month/year
arithmetic, in exactly the forms `sqldiff convert --to postgres` writes.
Each rule states its claim, the vendor text and the probe. "eval/values
Vn" is probe `Vn` of `eval/values/{mssql,postgres}.sql` with their
`.out` files, run on SQL Server 2022 and PostgreSQL 18 on 2026-10-30.
**Every vendor quote in this section was written from memory** (the
vendor sites were not reachable when it was written) and needs checking
against the page named; the probes are what the rules rest on.
`decimal`, `datetime`, `smalldatetime`, `datetimeoffset`, `time`,
`datetime2(7)`, `datetime2` without a precision (which is `datetime2(7)`)
and `tinyint` stay outside (an `ASM-FRACTIONAL-SECONDS` or `ASM-TINYINT`
conversion is not an identity).

**Integer widths** (`intWidth`). T-SQL `smallint` (16 bits), `int`
(32) and `bigint` (64) against PostgreSQL `smallint`/`int2`,
`integer`/`int`/`int4` and `bigint`/`int8`: the same ranges ("int,
bigint, smallint, and tinyint (Transact-SQL)"; PostgreSQL 8.1.1, Table
8.2). A value is an integer of its width (`STy.int w`,
`Sqldiff.Core.Val.int`); the width is the operator's business.

* **Arithmetic** `+ - * /` on two integers has the wider operand's width
  and raises on overflow, in both dialects ("Data type precedence
  (Transact-SQL)": the operand of higher precedence, `bigint` over `int`
  over `smallint`; PostgreSQL 10.2 "Operators": the operator whose types
  the operands convert to implicitly, `int2 + int4` being `int4`,
  `int4 + int8` being `int8`). Two `smallint`s give a `smallint` in both
  (not an `int`): probed, eval/values V1 (`smallint + smallint` is
  `smallint` on both, `32767 + 32767` raises 8115 against 22003; `int +
  bigint` is `bigint`; `bigint + 1`, `* 2` and `min / -1` overflow with
  8115 against 22003; `bigint / 7` truncates alike; `bigint / 0` is 8134
  against 22012). The core computes at the result's width
  (`Sqldiff.Core.arithInt`, `toIntW`). **Division of two `smallint`s**
  is a `smallint` division and raises on `-32768 / -1` on both engines
  (probed, V1: 8115 against 22003), as the core's 16-bit overflow does.
  A literal divisor is an `int`, so `@s / -1` is an `int` division and
  gives 32768 on both (probed, V1). `%` keeps its literal-divisor rule
  at every width (`smallint % smallint` of `-32768 % -1` is 8115 against
  0, `bigint % -1` of the minimum is 8115 against 0, probed V1).
* **Negation** `-x` keeps the width and raises on `-(-32768)` for a
  `smallint` in both (probed, V1: 8115 against 22003).
* **Comparisons** of two integers of any widths are the slice's
  comparisons: the narrower operand converts to the wider without a
  change of value.
* **Stores between widths** (`fitsSTy`). A value stored into a wider
  type keeps its value (probed, V2); into a narrower one both engines
  convert and raise when it does not fit (probed, V2: `int` 40000 into
  `smallint` is error 220 against 22003, `bigint` 3000000000 into `int`
  8115 against 22003). The core stores a value as it is, so a narrowing
  store is in the spec only when the value provably fits: a literal in
  the target's range, or a value of the target's width or narrower; the
  rest is outside, on every path (as for strings, "String stores, on
  every path"). `COUNT(*)` is a T-SQL `int` and a PostgreSQL `bigint`
  (probed, V2), so it stores into an `int` variable only (C3b rule 4, as
  before): into a `bigint` T-SQL still raises 8115 beyond 2^31-1 rows
  and PostgreSQL does not (the 2^31 rows are not probed; the types
  are), into a `smallint` both may raise. A `bit`, date or timestamp
  value stored into an integer is outside too: T-SQL stores a bit as 0
  or 1 and PL/pgSQL raises 22P02 (probed, V3).
* `COALESCE`, `ISNULL`, `CASE` over integers of several widths take the
  widest (the highest precedence in T-SQL; PostgreSQL 10.5 resolves to
  the common type); `ISNULL(x, y)` keeps `x`'s type, so `y` must fit it.
* Parameters and results: `bigint` and `smallint` correspond to
  themselves (`tyCorr`). A call argument must have exactly its callee's
  parameter type, which the request names ("Argument types" in
  "Calls"): PostgreSQL has no implicit `integer` to `smallint` or
  `boolean` conversion in procedure resolution (42883), and an `INOUT`
  variable that needs one is not writable. The declared types of the
  variables the certificate pairs must correspond too (`localTyCorr`,
  `Check.lean`), and a slice value is not stored into a variable of a
  type outside the slice (`storeOkTy`): both engines convert on the
  store, differently (a `datetime2(6)` value into T-SQL `datetime` and
  PostgreSQL `timestamp`: `.123333` against `.123456`).

**`bit` against `boolean`** (`bitType`). The converter maps T-SQL `bit`
to `boolean` (crates/sqldiff-convert/src/tsql/bits.rs). A `bit` holds
0, 1 or NULL; a `boolean` FALSE, TRUE or NULL; the correspondence is
0 to FALSE and 1 to TRUE (`Sqldiff.Core.Val.bool`, the same value a
comparison gives). "bit (Transact-SQL)": "The string values TRUE and
FALSE can be converted to bit values"; "Converting to bit promotes any
nonzero value to 1". Probed, V3 (`2`, `-5`, `'TRUE'` store 1; `0`,
`'FALSE'` store 0).

* **Stores** into a `bit`/`boolean` variable or column: the T-SQL
  literals `0` and `1` against PostgreSQL `FALSE` and `TRUE`, NULL, or
  another `bit` value (`coerce`, `fitsSTy`). Any other value (`2`, an
  `int` expression, a string) is outside the spec: T-SQL converts it
  ("any nonzero value to 1") where PostgreSQL has no assignment cast
  from the numbers (the converter writes `(v <> 0)` for those, outside
  this slice).
* **Comparisons** `@b = 1`, `@b = 0`, `@b <> 1`, `@b <> 0` (either
  operand order) against `b = TRUE`, `b = FALSE`, `b <> TRUE`,
  `b <> FALSE`, the forms the converter writes (`tsql.bit.compare`):
  T-SQL converts the `bit` to `int` ("Data type precedence": `int` over
  `bit`; probed, V3: `@b + 0` is `int`) and compares, so `@b = 1` is
  TRUE for 1, FALSE for 0, UNKNOWN for NULL; PostgreSQL `b = TRUE` is
  `b` itself, three-valued (9.1 "Logical Operators"). Both lower to the
  comparison `cmp eq|ne b (bool k)` (`bitCmp`), which keeps
  `ASM-TSQL-ANSI-NULLS` in the residual: under `ANSI_NULLS OFF` a NULL
  bit `<> 1` is TRUE in T-SQL, UNKNOWN in PostgreSQL (review of C2c:
  SQL Server 1, PostgreSQL 0). Probed, V3: the three-valued tables agree
  on both engines for 1, 0 and NULL, and `NOT NULL` is UNKNOWN. `@b = 2` (never TRUE), a
  comparison of two `bit`s, `bit` in `COALESCE`/`ISNULL` with a number,
  and arithmetic on a `bit` are outside.
* `IS [NOT] NULL` as for integers. A `bit` is never a key column
  (`CatCol.ok`).

**`date` and `datetime2(p)` against `date` and `timestamp(p)`**
(`dateType`, `tsType`). Values are days of the proleptic Gregorian
calendar (`Sqldiff.Core.Calendar`, `Val.date`) and microseconds
(`Val.ts`); `datetime2(p)` keeps `p` fractional digits, `timestamp(p)`
the same ("datetime2 (Transact-SQL)": "fractional seconds precision ...
0 to 7"; PostgreSQL 8.5: `timestamp [(p)]`, "p ... from 0 to 6"); a
PostgreSQL `timestamp` without `p` is `timestamp(6)` (8.5: "the
default is 6"; `tsType`). `p = 7` has no mirror and is outside, so is
`datetime2` without a precision ("the default precision is 7"). The
ranges: T-SQL 0001-01-01 to 9999-12-31 for both types ("date
(Transact-SQL)", "datetime2 (Transact-SQL)"); PostgreSQL's is wider
(8.5, Table 8.9: 4713 BC to 5874897 AD for `date`). A value a T-SQL
caller can pass is in both ranges, and a value a routine makes is held
in range by `ASM-DATEADD-RANGE` below; the core holds every day number.

* **Literals** (`dateLit?`, `tsLit?`, `coerce`). A string literal in a
  position whose type is `date` is in the slice only as
  `'YYYY-MM-DD'` (four, two and two digits, a date of the calendar);
  in a `datetime2(p)`/`timestamp(p)` position only as
  `'YYYY-MM-DDThh:mm:ss[.f]'` with `hh < 24`, `mm < 60`, `ss < 60` and
  at most `p` fractional digits. T-SQL: these are the ISO 8601 forms,
  which "aren't affected by the SET LANGUAGE and SET DATEFORMAT session
  locale settings" ("datetime2 (Transact-SQL)", ISO 8601; "date
  (Transact-SQL)": the `YYYY-MM-DD` form is ISO 8601 and not affected
  either). Probed, V4: under `DATEFORMAT ydm`, `dmy` and `LANGUAGE
  British` a `date` and a `datetime2` read `2024-01-02` as January 2
  (a `datetime`, outside the spec, reads it as February 1 under `ydm`).
  PostgreSQL: `YYYY-MM-DD` is "ISO 8601; January 8 in any mode" (8.5.2,
  Table 8.10), and the `T` separator of ISO 8601 is accepted in time
  stamp input (8.5.1.3); probed, V4 under `DateStyle` `SQL, DMY`,
  `Postgres, YMD` and `German, DMY`. A literal with more fractional
  digits than `p` is outside: both engines round, but not alike at a
  half (probed, V4: `.1234565` into six digits is `.123457` in SQL
  Server and `.123456` in PostgreSQL). An invalid date is an error on
  both (V4: `2024-02-30` is 241 against 22008) and outside. Every other
  form (`'20240102'`, `'01/02/2024'`, a date-only literal where a
  timestamp is needed, a literal with a time zone) is outside. The
  explicit `CAST('...' AS date)` and `CAST('...' AS datetime2(p))`
  (`timestamp(p)`) of such a literal is the same value (V4: the types).
* **Comparisons** `= <> < <= > >=` of two dates, or of two timestamps
  (any precisions), are the slice's three-valued comparisons by day or
  microsecond number ("date (Transact-SQL)", "datetime2 (Transact-SQL)":
  the types support the comparison operators; PostgreSQL 9.9, Table 9.33;
  `ASM-TSQL-ANSI-NULLS` as for integers). A date against a timestamp
  compares the date at midnight (`toTs`): T-SQL converts the `date` to
  `datetime2` ("Data type precedence": `datetime2` over `date`; "date
  (Transact-SQL)", "Converting date to other date and time types": the
  time is "00:00:00.0000000"), PostgreSQL casts `date` to `timestamp`
  (8.5: a date is a timestamp "at midnight"). Probed, V5.
* **Stores**: a date into a `date`, a timestamp of at most `p` digits
  into a `datetime2(p)`/`timestamp(p)` (a wider precision keeps the
  value exactly: probed V4, `.123` into six digits), a date into a
  timestamp as midnight (`toTs`; probed V5), NULL. A timestamp into a
  `date` by assignment is outside: both engines truncate (probed V5),
  but the core would store the timestamp (write `CAST(x AS date)`). A
  timestamp into a narrower precision is outside: both engines round
  (probed V4, `.1235` into three digits is `.124` on both), whether
  alike at every half is not probed.
* **`CAST(x AS date)`** of a timestamp drops the time (`toDate`;
  "date (Transact-SQL)", "Converting other date and time types to the
  date data type": "the time part is truncated"; PostgreSQL 9.9, "date
  (timestamp)" truncates; probed, V5: `23:59:59.999999` gives the same
  day). Of a date it is the date.
* **`DATEADD(day | month | year, n, d)`** (T-SQL; the dateparts also as
  `dd`, `d`, `mm`, `m`, `yy`, `yyyy`, "DATEADD (Transact-SQL)") against
  the converter's forms (`tsql.dateadd`, crates/sqldiff-convert/src/
  tsql.rs): on a `date`, `(d + n)` and `(d - m)` for days and
  `(d + make_interval(months => n))::date`, `(d + make_interval(years
  => n))::date`; on a `datetime2(p)`, `(d + make_interval(days | months
  | years => n))`; `n` an `int` (or `smallint`) expression of the slice.
  `date + make_interval(...)` without the `::date` is a `timestamp` in
  PostgreSQL (probed, V6: `pg_typeof`; `(d + make_interval(months =>
  1)) + 1` raises 42883), so it is typed `ts 6` (`pgIntervalAddTy`) and
  only the cast makes it a date again (`mkToDate`, whose fold is
  `mkToDate_dateAdd_eval`); the converter always writes the cast
  (probed, V6: a nested `DATEADD` gives 2024-03-01 and 59 days on both).
  Both lower to `dateAdd part n d`: days are added to the day number;
  months and years move the month, keeping the day of the month or
  clamping it to the result month's last day ("DATEADD (Transact-SQL)":
  "If the day of the month in the return month is less than the day of
  the month in date, the last day of the return month is returned";
  PostgreSQL 9.9: `date + interval` and `timestamp + interval`, "adding
  a month ... the last day of February"); the time of day of a
  timestamp is kept. Probed, V6 (2024-01-31 + 1 month = 2024-02-29, +
  13 months = 2025-02-28, 2024-03-31 - 1 month = 2024-02-29, 2024-02-29
  + 1 year = 2025-02-28, - 1 year = 2023-02-28, + 4 years = 2028-02-29,
  days across February, the time kept, NULL for a NULL operand, the
  result types; the same on both engines). Microsoft's page states the
  clamp for months only, so a confirmation with `DATEADD(year, ...)`
  names `ASM-DATEADD-YEAR-END`, as the converter does; V6 shows it. The
  result is not held to a range: T-SQL raises 517 when it leaves
  0001-01-01 to 9999-12-31 and PostgreSQL returns the value (probed,
  V6: `9999-12-31 + 1 day`, `0001-06-01 - 1 year`, and `+ 1 month` on
  a `datetime2` at the end, 517 against `10000-01-01`, `0001-06-01 BC`
  and `10000-01-31`; `+ 2147483647 days` is 517 against 22008), so a
  confirmation with `DATEADD` names `ASM-DATEADD-RANGE` (the result
  stays in the range), as the converter does. `DATEADD` with any other
  datepart (`week`, `quarter`, `hour`, ...), on another type, with a
  `bigint` number, a string date or a date-only literal, is outside, and
  so is a PostgreSQL `make_interval` with another argument set.
* **`DATEDIFF(day, a, b)`** (also `dd`, `d`) against the converter's
  `((b)::date - (a)::date)` (`tsql.datediff`): the number of day
  boundaries crossed from `a` to `b`, the day number of `b`'s date minus
  that of `a`'s, an `int` ("DATEDIFF (Transact-SQL)": "the count ... of
  the specified datepart boundaries crossed"; PostgreSQL 9.9: `date -
  date → integer`, "Subtract dates, producing the number of days
  elapsed"). Both lower to `dateDiff a' b'` with each argument's date
  (`toDate` on a timestamp, `mkToDate`). Probed, V7
  (`2024-01-01T23:59` to `2024-01-02T00:01` is 1, reversed -1, a day
  of 23 hours 59 minutes is 0, 61 days across February, 3652058 days
  from 0001-01-01 to 9999-12-31, NULL; the same on both). The count
  cannot overflow within the range. Other dateparts are outside.
* `IS [NOT] NULL`, `COALESCE` and `CASE` over two dates or two
  timestamps (the widest precision), as for integers. Reads of the
  clock (`SYSDATETIME()`, `GETDATE()`, `now()`, `clock_timestamp()`),
  `DATEPART`, `EOMONTH`, formatting and string conversions are outside.
* Parameters and results: `date` corresponds to `date`;
  `datetime2(6)` to `timestamp(6)` or `timestamp` only (`tyCorr`):
  PostgreSQL drops a parameter's and a result's type modifier ("Parameters
  carry no length", C2b), so a `timestamp(3)` parameter keeps six
  digits where a `datetime2(3)` one rounds an argument to three.
  Locals may have any `p ≤ 6`. A `date` or `datetime2(p)` column may be
  a key column (exact equality on both, `Core.keyVal`); probed, V8 (a
  duplicate date key is 2627 against 23505, and typed columns store and
  read back their values, defaults included, on both).

**Catalog columns** (`ColKind`, "The C3c tables"): `bigint`, `smallint`,
`bit`/`boolean`, `date` and `datetime2(p)`/`timestamp(p)` columns, with
constant defaults in the literal forms above (`0`/`1` against
`FALSE`/`TRUE` for a bit column, the ISO literals for dates; `CatCol.ok`).
Probed, V8. -/

/-- Digits as a number; nothing else (no sign, no `MAX`). -/
def natArg? (s : String) : Option Nat :=
  if s.isEmpty then none else digitsVal s.toList 0

/-- The largest length of a T-SQL string in the slice. -/
def tsqlMaxLen : Nat := 4000

/-- The largest `n` of PostgreSQL `varchar(n)`. -/
def pgMaxLen : Nat := 10485760

/-- A T-SQL string type: its length and whether it is `nvarchar`. -/
def strTypeT : Ast.Ty → Option (Option Nat × Bool)
  | .Ty n [a] =>
    let u := n.toUpper
    (natArg? a).bind fun k =>
      if 1 ≤ k ∧ k ≤ tsqlMaxLen then
        if u == "VARCHAR" || u == "CHAR VARYING" || u == "CHARACTER VARYING" then some (some k, false)
        else if u == "NVARCHAR" then some (some k, true)
        else none
      else none
  | _ => none

def pgVarchar (u : String) : Bool :=
  u == "VARCHAR" || u == "CHAR VARYING" || u == "CHARACTER VARYING"

/-- A PostgreSQL string type: its length (`none`: any), never `wide`. -/
def strTypeP : Ast.Ty → Option (Option Nat × Bool)
  | .Ty n [] =>
    let u := n.toUpper
    if u == "TEXT" || pgVarchar u then some (none, false) else none
  | .Ty n [a] =>
    if pgVarchar n.toUpper then
      (natArg? a).bind fun k => if 1 ≤ k ∧ k ≤ pgMaxLen then some (some k, false) else none
    else none
  | _ => none

/-- The declared type is a string type of the slice: its length bound and
whether it is `wide`. -/
def strType : Dialect → Ast.Ty → Option (Option Nat × Bool)
  | .tsql _ _, t => strTypeT t
  | .pg _ _ _, t => strTypeP t

def strVar (d : Dialect) (env : TyEnv) (x : Var) : Option (Option Nat × Bool) :=
  (lookup env x).bind (strType d)

/-- (C2c) The declared type is T-SQL `bit` or PostgreSQL `boolean`. -/
def bitType : Dialect → Ast.Ty → Bool
  | .tsql _ _, .Ty n [] => n.toUpper == "BIT"
  | .pg _ _ _, .Ty n [] => n.toUpper == "BOOLEAN" || n.toUpper == "BOOL"
  | _, _ => false

/-- (C2c) The declared type is `date` (both dialects). -/
def dateType : Dialect → Ast.Ty → Bool
  | _, .Ty n [] => n.toUpper == "DATE"
  | _, _ => false

/-- (C2c) The declared type is T-SQL `datetime2(p)` or PostgreSQL
`timestamp(p)` (also `timestamp(p) without time zone`, and without
`(p)`: six digits), `p ≤ 6`: its precision. -/
def tsType : Dialect → Ast.Ty → Option Nat
  | .tsql _ _, .Ty n [a] =>
    if n.toUpper == "DATETIME2" then (natArg? a).bind fun p => if p ≤ 6 then some p else none
    else none
  | .pg _ _ _, .Ty n args =>
    let u := n.toUpper
    if u == "TIMESTAMP" || u == "TIMESTAMP WITHOUT TIME ZONE" then
      match args with
      | [] => some 6
      | [a] => (natArg? a).bind fun p => if p ≤ 6 then some p else none
      | _ => none
    else none
  | _, _ => none

/-- The slice type of a declared type, if it has one (C2a integers, C2b
strings, C2c widths, bit, date and timestamps). -/
def valTypeOf (d : Dialect) (ty : Ast.Ty) : Option STy :=
  match intWidth d ty with
  | some w => some (.int w)
  | none =>
    match strType d ty with
    | some (n, w) => some (.str n w)
    | none =>
      if bitType d ty then some .bit
      else if dateType d ty then some .date
      else (tsType d ty).map .ts

/-- The type of a variable of the slice. -/
def varTy (d : Dialect) (env : TyEnv) (x : Var) : Option STy :=
  (lookup env x).bind (valTypeOf d)

/-- The declared type of a variable, as a slice type, for a store into it. -/
def wantOf (d : Dialect) (env : TyEnv) (x : Var) : Option STy := varTy d env x

/-- A string literal of the slice: printable ASCII, no backslash. -/
def strLitOk (s : String) : Bool :=
  s.toList.all fun c => ' ' ≤ c && c ≤ '~' && c != '\\'

def addB : Option Nat → Option Nat → Option Nat
  | some a, some b => some (a + b)
  | _, _ => none

def maxB : Option Nat → Option Nat → Option Nat
  | some a, some b => some (max a b)
  | _, _ => none

/-- A T-SQL string expression must stay within `tsqlMaxLen`. -/
def boundOk (d : Dialect) (n : Option Nat) : Bool :=
  match d, n with
  | .tsql _ _, some k => k ≤ tsqlMaxLen
  | .tsql _ _, none => false
  | .pg _ _ _, _ => true

/-- A value whose bound is `m` fits a target whose length is `n`. -/
def fitsLen : Option Nat → Option Nat → Bool
  | none, _ => true
  | some n, some m => m ≤ n
  | some _, none => false

/-- (C2c) `n` decimal digits as a number. -/
def fixedDigits (n : Nat) (cs : List Char) : Option Nat :=
  if cs.length == n then digitsVal cs 0 else none

/-- (C2c) `'YYYY-MM-DD'`, a date of the calendar within 0001-01-01 and
9999-12-31, as its day number ("Literals" in "The C2c slice"). -/
def dateLit? (s : String) : Option Int :=
  match s.splitOn "-" with
  | [y, m, d] => do
    let y ← fixedDigits 4 y.toList
    let m ← fixedDigits 2 m.toList
    let d ← fixedDigits 2 d.toList
    if Core.Calendar.validDate y m d then some (Core.Calendar.daysFromCivil y m d) else none
  | _ => none

/-- (C2c) `'YYYY-MM-DDThh:mm:ss[.f]'` with at most `p` fractional digits,
as microseconds since 0001-01-01T00:00:00. -/
def tsLit? (p : Nat) (s : String) : Option Int :=
  match s.splitOn "T" with
  | [d, t] => do
    let day ← dateLit? d
    let (hms, frac) : String × List Char := match t.splitOn "." with
      | [hms] => (hms, [])
      | [hms, f] => (hms, f.toList)
      | _ => ("", ['x'])
    let (h, mi, se) ← match hms.splitOn ":" with
      | [h, mi, se] => do pure (← fixedDigits 2 h.toList, ← fixedDigits 2 mi.toList,
          ← fixedDigits 2 se.toList)
      | _ => none
    if h ≥ 24 || mi ≥ 60 || se ≥ 60 || frac.isEmpty && (t.splitOn ".").length > 1 ||
        frac.length > p then none
    else
    let us ← digitsVal (frac ++ List.replicate (6 - frac.length) '0') 0
    some (day * Core.Calendar.usPerDay + (((h * 60 + mi) * 60 + se) * 1000000 + us : Nat))
  | _ => none

/-- (C2c) A literal read by the type of the position it stands in
(`want`): the T-SQL `bit` literals `0` and `1` and the PostgreSQL
`FALSE`/`TRUE` (the latter also typed on their own, `typed`), and the
ISO date and timestamp literals, with `CAST(... AS type)` around them or
not. `none`: no such literal; the position then takes what `typed`
gives. -/
def coerce (d : Dialect) (want : STy) : Ast.Expr → Option (SExpr × STy)
  | .Num "0" => if (d matches .tsql _ _) && want == .bit then some (.bool false, .bit) else none
  | .Num "1" => if (d matches .tsql _ _) && want == .bit then some (.bool true, .bit) else none
  -- T-SQL: a constant beyond the `int` range is `numeric(p,0)`; stored into
  -- a `bigint`, or compared with one, it is the integer it spells ("Integer
  -- widths": the conversion is exact), and nothing else is done with it.
  | .Num s => match d, want with
    | .tsql _ _, .int .i64 => if (intLit? s).isNone then (int64Lit? s).map fun n => (.int n, .int .i64) else none
    | _, _ => none
  | .Str s => match want with
    | .date => (dateLit? s).map fun v => (.date v, .date)
    | .ts p => (tsLit? p s).map fun v => (.ts v, .ts p)
    | _ => none
  | .Cast (.Str s) ty =>
    if dateType d ty then (dateLit? s).map fun v => (.date v, .date)
    else match tsType d ty with
      | some p => (tsLit? p s).map fun v => (.ts v, .ts p)
      | none => none
  | _ => none

/-- `rtrim`, folded on a literal and on an `rtrim`
(`Sqldiff.Mirror.mkRtrim_eval`: the same meaning). Both lowerings use it,
so a trimmed literal is a literal on both sides. -/
def mkRtrim : SExpr → SExpr
  | .str s => .str (Core.rtrimSp s)
  | .rtrim a => .rtrim a
  | a => .rtrim a

theorem dropWhile_idem {α : Type} (p : α → Bool) (l : List α) :
    (l.dropWhile p).dropWhile p = l.dropWhile p := by
  induction l with
  | nil => rfl
  | cons x xs ih =>
    by_cases h : p x
    · simp [h, ih]
    · simp [h]

theorem rtrimSp_idem (s : String) : Core.rtrimSp (Core.rtrimSp s) = Core.rtrimSp s := by
  simp [Core.rtrimSp, dropWhile_idem]

theorem mkRtrim_eval {U : Type} (σ ρ : Core.Store U) (a : SExpr) :
    (mkRtrim a).eval σ ρ = (SExpr.rtrim a).eval σ ρ := by
  cases a with
  | str s => rfl
  | rtrim b =>
    simp only [mkRtrim, SExpr.eval]
    cases b.eval σ ρ with
    | err k => rfl
    | ok v => cases v <;> simp [rtrimSp_idem, Core.typeErr]
  | _ => rfl

/-- (C3c, C2c) The slice type of a column of a kind: a string of the
column's length is `wide` for a T-SQL `nvarchar` ("The C3c tables"). -/
def kindTy (d : Dialect) : ColKind → STy
  | .int w => .int w
  | .str n w => .str (some n) (w && (d matches .tsql _ _))
  | .bit => .bit
  | .date => .date
  | .ts p => .ts p

/-- (C3c) The type of a column read. -/
def colTy (d : Dialect) (sc : Scope) (c : Core.Col) : STy :=
  match lookup sc.colTys c with
  | some k => kindTy d k
  | none => .int .i32

/-- The type `COALESCE` or `CASE` gives two value arguments, or `none`
when they do not go together (an integer and a string, or a boolean;
(C2c) integers of two widths take the wider, timestamps the wider
precision). -/
def joinTy : STy → STy → Option STy
  | .nul, .nul => some .nul
  | .nul, .int w | .int w, .nul => some (.int w)
  | .int w, .int v => some (.int (w.join v))
  | .nul, .str n w | .str n w, .nul => some (.str n w)
  | .str n w, .str m v => some (.str (maxB n m) (w || v))
  | .nul, .bit | .bit, .nul | .bit, .bit => some .bit
  | .nul, .date | .date, .nul | .date, .date => some .date
  | .nul, .ts p | .ts p, .nul => some (.ts p)
  | .ts p, .ts q => some (.ts (max p q))
  | _, _ => none

/-- A value type: an integer, a string, (C2c) a bit, a date, a
timestamp, or the NULL literal. -/
def isVal : STy → Bool
  | .bool => false
  | _ => true

/-- (C2c) A value `v` (its slice form and type) can be stored into a slot
of slice type `want` without a conversion that could change it or
fail, and the form stored (a date into a timestamp slot at midnight,
`toTs`): NULL into anything; an integer into a width it fits (its own
width or wider, or a literal in the target's range: "Stores between
widths"); a string that fits ("Assignment converts, differently"); a bit
into a bit; a date into a date; a timestamp of at most the slot's
precision, or a date, into a timestamp. -/
def fitValue (want : STy) : SExpr × STy → Option SExpr
  | (v, .nul) => some v
  | (v, .int w) => match want with
    | .int t => if w.le t then some v else match v with
      | .int n => if t.fits n then some v else none
      | _ => none
    | _ => none
  | (v, .str m w) => match want with
    | .str n wt => if fitsLen n m && (wt || !w) then some v else none
    | _ => none
  | (v, .bit) => if want == .bit then some v else none
  | (v, .date) => match want with
    | .date => some v
    | .ts _ => some (.toTs v)
    | _ => none
  | (v, .ts q) => match want with
    | .ts p => if q ≤ p then some v else none
    | _ => none
  | (_, .bool) => none

def fitsSTy (want : STy) (v : SExpr × STy) : Bool := (fitValue want v).isSome

/-- A value can be stored in a variable, parameter or result declared
`ty` without a conversion the core does not show (`fitValue`). -/
def fitsTy (d : Dialect) (ty : Ast.Ty) (v : SExpr × STy) : Bool :=
  match valTypeOf d ty with
  | some want => fitsSTy want v
  | none => false

def arithOp? : String → Option AOp
  | "+" => some .add
  | "-" => some .sub
  | "*" => some .mul
  | "/" => some .div
  | "%" => some .mod
  | _ => none

def cmpOp? : String → Option COp
  | "=" => some .eq
  | "<>" => some .ne
  | "<" => some .lt
  | ">" => some .gt
  | "<=" => some .le
  | ">=" => some .ge
  | _ => none

/-- An expression without variables (and without columns) is in the slice
only if evaluating it does not fail (constant subexpressions, above). -/
def closedSafe (s : SExpr) : Bool :=
  !s.vars.isEmpty || s.hasCol ||
    match s.eval (U := Unit) Core.nullStore Core.nullStore with
    | .err _ => false
    | .ok _ => true

def guardC (r : Option (SExpr × STy)) : Option (SExpr × STy) :=
  r.bind fun p => if closedSafe p.1 then some p else none

/-- String `=` or `<>` (C2b), given the operands as each side compares
them. -/
def eqForm (o : String) (e : SExpr) : Option (SExpr × STy) :=
  if o == "=" then some (e, .bool)
  else if o == "<>" then some (.not e, .bool)
  else none

/-- (C2c) The `bit` comparison forms: T-SQL `@b = 1` and PostgreSQL
`b = TRUE` are `cmp eq b (bool true)`, `= 0`/`= FALSE` the same with
`false`, `<>` with `ne`; the literal on either side, put on the right.
They stay comparisons (not `b` or `NOT b`, which mean the same under
`ANSI_NULLS ON`), so `SExpr.hasEqNe` sees them and a confirmation names
`ASM-TSQL-ANSI-NULLS`: under `ANSI_NULLS OFF` a NULL bit `<> 1` is TRUE in
T-SQL (SQL Server 1, PostgreSQL 0, eval/values V3). -/
def bitCmp (o : String) (x : SExpr) (lit : Bool) : Option (SExpr × STy) :=
  if o == "=" then some (.cmp .eq x (.bool lit), .bool)
  else if o == "<>" then some (.cmp .ne x (.bool lit), .bool)
  else none

def binTy (d : Dialect) (o : String) : SExpr × STy → SExpr × STy → Option (SExpr × STy)
  | (a, .int w), (b, .int v) =>
    let r := w.join v
    match arithOp? o, cmpOp? o with
    | some .mod, _ => match b with
      | .int k => if k = 0 ∨ k = -1 then none else some (.arith r .mod a b, .int r)
      | _ => none
    | some op, _ => some (.arith r op a b, .int r)
    | none, some c => some (.cmp c a b, .bool)
    | none, none => none
  -- C2c: dates and timestamps compare; a date against a timestamp at midnight.
  | (a, .date), (b, .date) => (cmpOp? o).map fun c => (.cmp c a b, .bool)
  | (a, .ts _), (b, .ts _) => (cmpOp? o).map fun c => (.cmp c a b, .bool)
  | (a, .date), (b, .ts _) => (cmpOp? o).map fun c => (.cmp c (.toTs a) b, .bool)
  | (a, .ts _), (b, .date) => (cmpOp? o).map fun c => (.cmp c a (.toTs b), .bool)
  -- C2c: the `bit` forms the converter writes, with the literal on either side.
  | (.bool k, .bit), (x, .bit) => bitCmp o x k
  | (x, .bit), (.bool k, .bit) => bitCmp o x k
  -- C2c, PostgreSQL: `d + n` and `d - n` on a date, `b - a` on two dates.
  | (a, .date), (b, .int v) =>
    match d with
    | .pg _ _ _ =>
      if o == "+" && v.le .i32 then some (.dateAdd .day b a, .date)
      -- Only an `int` literal: `d - 2147483648` is `date - bigint`.
      else if o == "-" && v.le .i32 then match b with
        | .int k => some (.dateAdd .day (.int (-k)) a, .date)
        | _ => none
      else none
    | .tsql _ _ => none
  | (a, .bool), (b, .bool) =>
    if a.mayRaise || b.mayRaise then none
    else if o.toUpper == "AND" then some (.and a b, .bool)
    else if o.toUpper == "OR" then some (.or a b, .bool)
    else none
  | (a, .str n w), (b, .str m v) =>
    match d with
    | .tsql _ Γ =>
      if o == "+" then
        if boundOk d (addB n m) then some (.concat a b, .str (addB n m) (w || v)) else none
      else if Γ.binaryCollation then eqForm o (.streq (mkRtrim a) (mkRtrim b))
      else none
    | .pg Γ _ _ =>
      if o == "||" then some (.concat a b, .str (addB n m) false)
      else if Γ.binaryCollation then eqForm o (.streq a b)
      else none
  | _, _ => none

def unTy (o : String) : SExpr × STy → Option (SExpr × STy)
  | (a, .int w) => if o == "-" then some (.neg w a, .int w) else none
  | (a, .bool) => if o.toUpper == "NOT" then some (.not a, .bool) else none
  | _ => none

/-- The type of a list of value arguments (`COALESCE`, `CASE` results),
starting from the NULL literal's. -/
def joinAll : List STy → Option STy
  | [] => some .nul
  | t :: ts => (joinAll ts).bind (joinTy t)

/-- A value type with a value: an integer, a string, (C2c) a bit, a date
or a timestamp, not only NULL. -/
def typedVal : STy → Bool
  | .int _ | .str .. | .bit | .date | .ts _ => true
  | _ => false

/-- `coalesce x1 (coalesce x2 ... xn)`. -/
def coalesceBuild : List SExpr → Option SExpr
  | [] => none
  | [a] => some a
  | a :: rest => (coalesceBuild rest).map (.coalesce a)

/-- A function call of the slice. `raw` are the arguments as written
(T-SQL `ISNULL` over strings asks its first one to be a variable). -/
def callTy (d : Dialect) (name : String) (raw : List Ast.Expr) (args : List (SExpr × STy)) :
    Option (SExpr × STy) :=
  match name.toUpper, args, d with
  | "COALESCE", _, _ =>
    if args.length ≥ 2 then
      (joinAll (args.map (·.2))).bind fun t =>
        if typedVal t then (coalesceBuild (args.map (·.1))).map (·, t) else none
    else none
  | "ISNULL", [(x, .int w), (y, ty)], .tsql _ _ =>
    if fitsSTy (.int w) (y, ty) && !y.mayRaise then some (.coalesce x y, .int w) else none
  | "ISNULL", [(x, .str (some n) wx), (y, ty)], .tsql _ _ =>
    let isVar := match raw with
      | .Var _ :: _ => true
      | _ => false
    let yOk := match ty with
      | .nul => true
      | .str m wy => fitsLen (some n) m && (wx || !wy)
      | _ => false
    if isVar && yOk && !y.mayRaise then some (.coalesce x y, .str (some n) wx) else none
  | "RTRIM", [(x, .str n w)], _ => some (mkRtrim x, .str n w)
  | "LEN", [(x, .str _ _)], .tsql _ _ => some (.len (mkRtrim x), .int .i32)
  | "LENGTH", [(x, .str _ _)], .pg _ _ _ => some (.len x, .int .i32)
  | "CHAR_LENGTH", [(x, .str _ _)], .pg _ _ _ => some (.len x, .int .i32)
  | "CHARACTER_LENGTH", [(x, .str _ _)], .pg _ _ _ => some (.len x, .int .i32)
  | _, _, _ => none

/-- (C2c) A T-SQL datepart of `DATEADD`/`DATEDIFF` the slice knows, with
its abbreviations ("DATEADD (Transact-SQL)", the datepart table). -/
def datePart? (e : Ast.Expr) : Option DPart :=
  match e with
  | .Name [n] =>
    let u := n.toLower
    if u == "day" || u == "dd" || u == "d" then some .day
    else if u == "month" || u == "mm" || u == "m" then some .month
    else if u == "year" || u == "yy" || u == "yyyy" then some .year
    else none
  | _ => none

/-- (C2c) The date of a date or timestamp value: itself, or `toDate`. -/
def mkToDate : SExpr × STy → Option SExpr
  | (a, .date) => some a
  -- PostgreSQL `(d + make_interval(...))::date` with `d` a date: the
  -- timestamp at midnight moved by the interval, then its date, which is
  -- the date moved (`mkToDate_dateAdd_eval`).
  | (.dateAdd p n (.toTs a), .ts _) => some (.dateAdd p n a)
  | (a, .ts _) => some (.toDate a)
  | _ => none

/-- (C2c) The fold of `mkToDate` means the same on a date (or NULL): the
date at midnight moved by days, months or years, then its date, is the
date moved. Which values a date variable can hold is the typing's
business (`typed` puts `toTs` only on date-typed expressions). -/
theorem mkToDate_dateAdd_eval {U : Type} (σ ρ : Core.Store U) (p : DPart) (n a : SExpr)
    (ha : a.eval σ ρ = .ok .null ∨ ∃ d, a.eval σ ρ = .ok (.date d)) :
    (SExpr.toDate (.dateAdd p n (.toTs a))).eval σ ρ = (SExpr.dateAdd p n a).eval σ ρ := by
  simp only [SExpr.eval]
  rcases ha with ha | ⟨d, ha⟩
  · rw [ha]; cases n.eval σ ρ with
    | err k => rfl
    | ok v => cases v <;> rfl
  · rw [ha]; cases hn : n.eval σ ρ with
    | err k => rfl
    | ok v =>
      cases v <;> simp [Core.bind2, Core.dateAddVal, Core.dayVal, Core.typeErr,
        Core.Calendar.dayOf_addToTs]

/-- (C2c) The one part a PostgreSQL `make_interval(...)` call sets, with
its number, as the parser hands the call over: seven positional
arguments, the unset ones `0` (`0.0` for the seconds); only
`years`, `months` or `days`, one of them. -/
def intervalPart (args : List Ast.Expr) : Option (DPart × Nat) :=
  let zero (e : Ast.Expr) : Bool := e matches .Num "0"
  match args with
  | [y, mo, w, dd, h, mi, se] =>
    if !(zero w && zero h && zero mi && (se matches .Num "0.0")) then none
    else if !zero y && zero mo && zero dd then some (.year, 0)
    else if zero y && !zero mo && zero dd then some (.month, 1)
    else if zero y && zero mo && !zero dd then some (.day, 3)
    else none
  | _ => none

/-- (C2c) `DATEADD`'s result on a date or timestamp `dt` with the number
`n` (an `int` or `smallint` of the slice). -/
def dateAddTy (p : DPart) : SExpr × STy → SExpr × STy → Option (SExpr × STy)
  | (n, .int w), (dt, .date) => if w.le .i32 then some (.dateAdd p n dt, .date) else none
  | (n, .int w), (dt, .ts q) => if w.le .i32 then some (.dateAdd p n dt, .ts q) else none
  | _, _ => none

/-- (C2c) PostgreSQL `dt + make_interval(...)`: on a timestamp, a
timestamp; on a date, also a timestamp (`date + interval →
timestamp`, PostgreSQL 9.9, Table 9.33; probed: `pg_typeof(date
'2024-01-31' + make_interval(months => 1))` is `timestamp without time
zone`), the date at midnight moved, of six fractional digits. Only an
explicit `::date` or `CAST(... AS date)` makes it a date again
(`mkToDate`); `+ 1`, `- k` and `- date` on it are outside the slice
(PostgreSQL: no `timestamp + integer`; `timestamp - date` is an
`interval`). -/
def pgIntervalAddTy (p : DPart) : SExpr × STy → SExpr × STy → Option (SExpr × STy)
  | (n, .int w), (dt, .date) => if w.le .i32 then some (.dateAdd p n (.toTs dt), .ts 6) else none
  | n, dt => dateAddTy p n dt

/-- `cond c1 r1 (cond c2 r2 ... e)`. -/
def caseBuild : List ((SExpr × STy) × (SExpr × STy)) → SExpr → Option SExpr
  | [], e => some e
  | ((c, .bool), (r, tr)) :: ws, e => if isVal tr then (caseBuild ws e).map (.cond c r) else none
  | _, _ => none

def caseTy (ws : List ((SExpr × STy) × (SExpr × STy))) (els : Option (SExpr × STy)) :
    Option (SExpr × STy) :=
  let tys := ws.map (·.2.2) ++ (els.map (·.2)).toList
  if ws.isEmpty then none
  else (joinAll tys).bind fun t =>
    if typedVal t then (caseBuild ws ((els.map (·.1)).getD .null)).map (·, t) else none

/-- (C2c) The type a literal next to a value of type `t`, under the
operator `o`, is read at: a bit, a date or a timestamp; a `bigint` only
under a comparison (a T-SQL constant beyond the `int` range is `numeric`
in arithmetic); nothing for the rest. -/
def literalWant (o : String) : STy → Option STy
  | .bit => some .bit
  | .date => some .date
  | .ts p => some (.ts p)
  | .int .i64 => if (cmpOp? o).isSome then some (.int .i64) else none
  | _ => none

mutual
  /-- The slice form of an expression and its type, or `none` when the
  expression is outside the slice. A variable is in the slice when it is
  declared with a type of the slice (`env`). -/
  def typed (d : Dialect) (env : TyEnv) (sc : Scope) : Ast.Expr → Option (SExpr × STy)
    | .Var v => match d with
      | .tsql _ _ => (varTy d env v).map (.var v, ·)
      | .pg _ _ _ => none
    | .Name [x] => match resolveName d sc x with
      | some (.inl c) => some (.col c, colTy d sc c)
      | some (.inr v) => (varTy d env v).map (.var v, ·)
      | none => none
    | .Name [q, c] => (resolveQual d sc q c).map fun c' => (.col c', colTy d sc c')
    | .Num s => match intLit? s with
      | some n => some (.int n, .int .i32)
      | none => match d with
        -- C2c: a bigint constant in PostgreSQL; numeric in T-SQL, outside.
        | .pg _ _ _ => (int64Lit? s).map fun n => (.int n, .int .i64)
        | .tsql _ _ => none
    | .Str s => if strLitOk s then some (.str s, .str (some (max 1 s.length)) false) else none
    | .Null => some (.null, .nul)
    -- C2c: PostgreSQL `TRUE`/`FALSE` are bit values.
    | .Special n => match d with
      | .pg _ _ _ =>
        if n.toUpper == "TRUE" then some (.bool true, .bit)
        else if n.toUpper == "FALSE" then some (.bool false, .bit)
        else none
      | .tsql _ _ => none
    -- C2c, PostgreSQL: `d + make_interval(...)` on a date or timestamp,
    -- the call's seven arguments as the parser hands them over.
    | .Bin "+" dt rhs@(.CallE (.CallF [f] [y, mo, w, dd, h, mi, se] false [] false false false)) =>
      if (d matches .pg _ _ _) && f.toLower == "make_interval" then do
        let (p, i) ← intervalPart [y, mo, w, dd, h, mi, se]
        let dt' ← typed d env sc dt
        let n' ← match i with
          | 0 => typed d env sc y
          | 1 => typed d env sc mo
          | _ => typed d env sc dd
        guardC (pgIntervalAddTy p n' dt')
      else do
        let a' ← typed d env sc dt
        let b' ← typed d env sc rhs
        guardC (binTy d "+" a' b')
    | .Bin o a b => do
      -- C2c: a literal takes the type of the other operand (a bit against
      -- `0`/`1`, a date against `'YYYY-MM-DD'`, a T-SQL constant beyond the
      -- `int` range compared with a `bigint`).
      let a0 := typed d env sc a
      let b0 := typed d env sc b
      let lit (t : STy) (e : Ast.Expr) : Option (SExpr × STy) :=
        (literalWant o t).bind fun w => coerce d w e
      let (a', b') ← match a0, b0 with
        | some a', some b' =>
          match lit b'.2 a with
          | some a'' => some (a'', b')
          | none => match lit a'.2 b with
            | some b'' => some (a', b'')
            | none => some (a', b')
        | some a', none => (lit a'.2 b).map (a', ·)
        | none, some b' => (lit b'.2 a).map (·, b')
        | none, none => none
      -- C2c, PostgreSQL: `(b)::date - (a)::date` is DATEDIFF(day, a, b).
      match d, o, a', b' with
      | .pg _ _ _, "-", (x, .date), (y, .date) => guardC (some (.dateDiff y x, .int .i32))
      | _, _, _, _ => guardC (binTy d o a' b')
    | .Unary o a => do
      let a' ← typed d env sc a
      guardC (unTy o a')
    | .IsNull x isNot => do
      let (x', tx) ← typed d env sc x
      if typedVal tx then guardC (some (.isNull x' isNot, .bool)) else none
    -- C2c: `CAST(x AS date)` of a timestamp, and the cast literals.
    | .Cast x ty => match coerce d .nul (.Cast x ty) with
      | some v => some v
      | none => do
        let x' ← typed d env sc x
        if dateType d ty then (mkToDate x').map (·, .date) else none
    -- C2c, T-SQL: DATEADD and DATEDIFF with their datepart as written.
    | .CallE (.CallF [n] [part, a, b] false [] false false false) =>
      if (d matches .tsql _ _) && n.toUpper == "DATEADD" then do
        let p ← datePart? part
        let a' ← typed d env sc a
        let b' ← typed d env sc b
        guardC (dateAddTy p a' b')
      else if (d matches .tsql _ _) && n.toUpper == "DATEDIFF" then do
        if datePart? part != some .day then none else
        let a' ← typed d env sc a
        let b' ← typed d env sc b
        let x ← mkToDate a'
        let y ← mkToDate b'
        guardC (some (.dateDiff x y, .int .i32))
      else do
        let args' ← typedList d env sc [part, a, b]
        guardC (callTy d n [part, a, b] args')
    | .CallE (.CallF [n] args false [] false false false) => do
      let args' ← typedList d env sc args
      guardC (callTy d n args args')
    | .Case none whens els => do
      let ws ← typedWhens d env sc whens
      let e ← typedOpt d env sc els
      guardC (caseTy ws e)
    | _ => none

  def typedList (d : Dialect) (env : TyEnv) (sc : Scope) :
      List Ast.Expr → Option (List (SExpr × STy))
    | [] => some []
    | e :: es => do
      let e' ← typed d env sc e
      let es' ← typedList d env sc es
      pure (e' :: es')

  def typedOpt (d : Dialect) (env : TyEnv) (sc : Scope) :
      Option Ast.Expr → Option (Option (SExpr × STy))
    | none => some none
    | some e => (typed d env sc e).map some

  def typedWhens (d : Dialect) (env : TyEnv) (sc : Scope) :
      List Ast.When2 → Option (List ((SExpr × STy) × (SExpr × STy)))
    | [] => some []
    | .When2 c r :: ws => do
      let c' ← typed d env sc c
      let r' ← typed d env sc r
      let ws' ← typedWhens d env sc ws
      pure ((c', r') :: ws')
end

/-- The slice form of `e` in a position of type `want` (C2c): a literal
the position reads (`coerce`), else what `e` is on its own. -/
def typedTo (d : Dialect) (env : TyEnv) (sc : Scope) (want : Option STy) (e : Ast.Expr) :
    Option (SExpr × STy) :=
  match want.bind fun w => coerce d w e with
  | some v => some v
  | none => typed d env sc e

/-! ## Expressions

An expression of the slice lowers to its slice form (`typed`). Every
other expression is the C1 path: a variable read, an integer literal and
NULL are as in the slice; any other form is an operator of the core,
named by its syntactic form and its non-expression fields (`bin:+`,
`un:-`, `fn:coalesce`, `isnull:false`, `cast:int()`, `str:abc`,
`num:2147483648`, ...), applied to its sub-expressions, each lowered the
same way; distinct forms get distinct names, so two expressions outside
the slice lower to the same term exactly when they are the same
expression up to the renaming of variables.

The core leaves those operators uninterpreted, so a confirmation that
uses one is relative to **the same operator name meaning the same function
in both dialects** (`ASM-C1-OPERATORS`). T-SQL `@a + @b` and PostgreSQL
`a + b` on `varchar` variables agree on the C1 path; whether they agree
is later C2 work. Expressions that read data (subqueries, `EXISTS`,
`IN (SELECT ...)`, `*`, aggregates and window calls) are outside the
spec here; C3b reads the few whole-statement shapes of "The C3b slice:
queries" (a scalar subquery as the whole assigned value, `EXISTS` as the
whole condition of an `IF`). -/

def flag (b : Bool) : String := if b then "1" else "0"

/-- T-SQL functions that read the error a `CATCH` handles, or the
transaction state (C4): outside the spec. -/
def errorFns : List String :=
  ["ERROR_NUMBER", "ERROR_MESSAGE", "ERROR_SEVERITY", "ERROR_STATE", "ERROR_LINE",
   "ERROR_PROCEDURE", "XACT_STATE"]

def tyName : Ast.Ty → String
  | .Ty n args => n.toLower ++ "(" ++ ",".intercalate args ++ ")"

/-- The slice form of `e` if it has one, else `c1`. -/
def withSlice (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) (c1 : Option Core.Expr) :
    Option Core.Expr :=
  match typed d env sc e with
  | some (s, _) => some (.sc s)
  | none => c1

mutual
  def lowerExpr (d : Dialect) (env : TyEnv) (sc : Scope) : Ast.Expr → Option Core.Expr
    | .Var v => match d with
      | .tsql _ _ => if v.startsWith "@@" then none else some (.var v) -- `@@ERROR` etc.: C4
      | .pg _ _ _ => none
    | .Name [x] => match resolveName d sc x with
      | some (.inl c) => some (.sc (.col c))
      | some (.inr v) => some (.var v)
      | none => none
    | .Name [q, c] => (resolveQual d sc q c).map fun c' => .sc (.col c')
    | .Name _ => none
    | .Num s => some (match intLit? s with
      | some n => .lit n
      | none => .op ("num:" ++ s) [])
    | .Str s => withSlice d env sc (.Str s) (some (.op ("str:" ++ s) []))
    | .Null => some .null
    | .Star _ => none
    | .CallE (.CallF n args distinct order withinGroup over star) =>
      -- C4: the error functions are outside the spec ("The C4 rules").
      if (d matches .tsql _ _) && errorFns.contains (dotted n).toUpper then none else
      withSlice d env sc (.CallE (.CallF n args distinct order withinGroup over star))
        (if distinct || !order.isEmpty || withinGroup || over || star then none
        else (lowerExprs d env sc args).map (.op ("fn:" ++ (dotted n).toLower)))
    | .Bin o a b => withSlice d env sc (.Bin o a b) do
      let a' ← lowerExpr d env sc a
      let b' ← lowerExpr d env sc b
      pure (.op ("bin:" ++ o.toLower) [a', b'])
    | .Unary o a => withSlice d env sc (.Unary o a)
      ((lowerExpr d env sc a).map fun a' => .op ("un:" ++ o.toLower) [a'])
    | .IsNull x neg => withSlice d env sc (.IsNull x neg)
      ((lowerExpr d env sc x).map fun x' => .op ("isnull:" ++ flag neg) [x'])
    | .InList x xs neg => do
      let x' ← lowerExpr d env sc x
      let xs' ← lowerExprs d env sc xs
      pure (.op ("in:" ++ flag neg) (x' :: xs'))
    | .InQuery .. => none
    | .Between x lo hi neg => do
      let x' ← lowerExpr d env sc x
      let lo' ← lowerExpr d env sc lo
      let hi' ← lowerExpr d env sc hi
      pure (.op ("between:" ++ flag neg) [x', lo', hi'])
    | .Like x p neg ci => do
      let x' ← lowerExpr d env sc x
      let p' ← lowerExpr d env sc p
      pure (.op ("like:" ++ flag neg ++ flag ci) [x', p'])
    | .Cast x ty => (lowerExpr d env sc x).map fun x' => .op ("cast:" ++ tyName ty) [x']
    | .Case operand whens els => withSlice d env sc (.Case operand whens els) do
      -- `case:OE`: whether there is an operand and an `ELSE`; the `WHEN`
      -- pairs fill the arguments between them.
      let o' ← lowerOpt d env sc operand
      let ws ← lowerWhens d env sc whens
      let e' ← lowerOpt d env sc els
      pure (.op ("case:" ++ flag o'.isSome ++ flag e'.isSome) (o'.toList ++ ws ++ e'.toList))
    | .Exists _ | .Subquery _ => none
    | .Tuple xs => (lowerExprs d env sc xs).map (.op "tuple")
    | .Special n => some (.op ("special:" ++ n.toLower) [])
    | .Extract f x => (lowerExpr d env sc x).map fun x' => .op ("extract:" ++ f.toLower) [x']

  def lowerExprs (d : Dialect) (env : TyEnv) (sc : Scope) : List Ast.Expr → Option (List Core.Expr)
    | [] => some []
    | e :: es => do
      let e' ← lowerExpr d env sc e
      let es' ← lowerExprs d env sc es
      pure (e' :: es')

  /-- An optional part: `some none` when absent, `none` when unsupported. -/
  def lowerOpt (d : Dialect) (env : TyEnv) (sc : Scope) :
      Option Ast.Expr → Option (Option Core.Expr)
    | none => some none
    | some e => (lowerExpr d env sc e).map some

  def lowerWhens (d : Dialect) (env : TyEnv) (sc : Scope) : List Ast.When2 → Option (List Core.Expr)
    | [] => some []
    | .When2 c r :: ws => do
      let c' ← lowerExpr d env sc c
      let r' ← lowerExpr d env sc r
      let ws' ← lowerWhens d env sc ws
      pure (c' :: r' :: ws')
end

/-- (C2c) A value stored into a slot of slice type `want` (a variable, a
parameter default, a column): a literal the slot reads (`coerce`), then
the value fitted to the slot (`fitValue`, which may add `toTs`); when
the value is outside the slice, or the slot has no slice type, the C1
path (`lowerExpr`), where "String stores, on every path" and the
narrowing rule hold the store. -/
def lowerValue (d : Dialect) (env : TyEnv) (sc : Scope) (want : Option STy) (e : Ast.Expr) :
    Option Core.Expr :=
  match want, typedTo d env sc want e with
  | some w, some v => match fitValue w v with
    | some s => some (.sc s)
    | none => lowerExpr d env sc e
  | _, _ => lowerExpr d env sc e

/-- The variable an assignment or an `OUTPUT` argument names. -/
def target : Dialect → Ast.Expr → Option Var
  | .tsql _ _, .Var v => if v.startsWith "@@" then none else some v
  | .pg _ _ _, .Name [x] => some (pgName x)
  | _, _ => none

/-! ## The T-SQL return status

A T-SQL procedure returns an integer status to its caller (`EXEC @rc =
p`): the value of `RETURN e`, and 0 when it ends by `RETURN` without a
value or by reaching the end; `RETURN NULL` returns 0 with a warning
(Microsoft Learn, "RETURN (Transact-SQL)": "If no value is specified, 0 is
returned"; "Procedures ... cannot return a null value. If a procedure
tries to return a null value ... a warning message is generated and a
value of 0 is returned").

In the core the status is one more parameter, last, `inout`, named
`statusVar` (a name no T-SQL variable can have): the body starts with
`status := 0`, and `RETURN e` is `status := coalesce(e, 0); return`, with
the slice's `coalesce` when `e` is in the slice and the C1 operator
`fn:coalesce` otherwise. So a caller observes the status as the last out
value. Its entry value is never read, so its mode and default have no
effect; they are `inout` and `NULL` to match the converter's `INOUT
return_code integer DEFAULT NULL`. The PL/pgSQL mirror must therefore set
`return_code := 0` first and write `return_code := COALESCE(e, 0)`: a
bare `return_code := e` differs when `e` is NULL, and without the first
assignment a procedure that ends without `RETURN e` leaves the caller's
value (NULL by default), not 0. -/

def statusVar : Var := "%status"

def statusParam : Core.Param := ⟨statusVar, .inout, some .null⟩

def statusExpr (d : Dialect) (env : TyEnv) (e : Ast.Expr) (e' : Core.Expr) : Core.Expr :=
  match typed d env {} e with
  | some (s, t) => if valTy t then .sc (.coalesce s (.int 0)) else .op "fn:coalesce" [e', .lit 0]
  | none => .op "fn:coalesce" [e', .lit 0]

/-! ## The C3a slice: data statements

Single-table `INSERT ... VALUES`, `UPDATE` and `DELETE` in a procedure,
on a table of the target catalog (`CatTable`), lowered to the core's
`insert`, `update` and `delete` (`Sqldiff.Core.insertRows`,
`updateRows`, `deleteRows`). Expressions are lowered as everywhere else,
with the columns in scope (`Scope`, `resolveName`); a column of the
catalog is an `int` in the slice's sense. Each rule states the claim, the
vendor text and the probe status, as for C2a. "eval/casts/assign" is
`eval/casts/assign-{mssql,postgres}.sql` with their `.out` files, and
"eval/bit-boolean" is `eval/bit-boolean/{mssql,postgres}.sql`; both
write tables only incidentally, to probe conversions, so most rules below
are not probed.

**Tables.** A T-SQL table name, `.`-joined as written (`dbo.t`), reaches
the target table the deployment maps it to (`Deployment.tables`,
`ASM-DBO-SCHEMA`); a PostgreSQL table name is folded (`pgName`) and
`.`-joined, and must be a catalog table of that name (the routine's
`search_path` finds it: the same trust as for `CALL`). A catalog table is
usable only if its column names are ASCII, distinct, and their T-SQL
spellings distinct up to case (`CatTable.wellFormed`). Not probed.

**Where data statements may stand.** Only in a procedure: a T-SQL
function cannot change a table ("CREATE FUNCTION (Transact-SQL)":
user-defined functions "can't be used to perform actions that modify the
database state"), so a T-SQL function with one is not a function SQL
Server accepts, and its PostgreSQL mirror has no counterpart.

**Row counts.** A T-SQL `INSERT`, `UPDATE` or `DELETE` sends its
rows-affected count unless `SET NOCOUNT ON` ("SET NOCOUNT
(Transact-SQL)"); a PL/pgSQL procedure sends none (a `CALL` reports only
its own command tag). So when the deployment's observer reads row counts
(`Deployment.rowCounts`), every T-SQL data statement is outside the spec
(and C1 keeps `SET NOCOUNT` outside then too); when it does not, counts
are not observed (`ASM-OBSERVER-ROWCOUNT`, the deployment's choice).

**`INSERT INTO t (c1, ..., cn) VALUES (e1, ..., en), ...`**: a column
list of distinct columns of `t`, one or more rows of exactly `n` values;
each row gets the values in the listed columns and NULL in every other
column ((C3c) or the column's default: "The C3c tables"). No query source, `DEFAULT VALUES`, `RETURNING`, `ON CONFLICT` or
column list left out (the order of a table's columns is not in the
correspondence).

* T-SQL: "INSERT (Transact-SQL)": "If a column isn't in column_list, the
  Database Engine must be able to provide a value based on the definition
  of the column ... [it] automatically provides a value for the column if
  the column: ... Is nullable. A null value is used." (the other cases,
  identity, default, `timestamp`, computed, are excluded by the catalog).
  A column without a value that is `NOT NULL` is error 515. Incidentally
  probed: eval/casts/assign (`INSERT INTO #a (dt) VALUES (...)` leaves
  the unlisted `dt2` NULL).
* PostgreSQL: "INSERT": "Each column not present in the explicit or
  implicit column list will be filled with a default value, either its
  declared default value or null if there is none." Not probed.
* The values see no columns: T-SQL refuses a column name in `VALUES`,
  and PostgreSQL resolves it to nothing ("there is a column named ... but
  it cannot be referenced from this part of the query"), so in PL/pgSQL a
  name there is a variable even when the table has a column of that name
  (`resolveName` with the empty scope). Not probed.

**`UPDATE t SET c1 = e1, ... [WHERE p]`**: distinct one-part columns of
`t`, none a key column; no alias, no `FROM` (T-SQL `UPDATE ... FROM`, which
takes an unspecified source row, is outside C3a), no `OUTPUT`/`RETURNING`.
Each row where `p` is TRUE gets every `ei` evaluated on the row's old
values; FALSE and UNKNOWN leave the row as it is.

* All assignments see the old values. PostgreSQL: "UPDATE", `expression`:
  "The expression can use the old values of this and other columns in the
  table." T-SQL: "UPDATE (Transact-SQL)", on `@variable = column =
  expression`: "This differs from SET @variable = column, column =
  expression, which sets the variable to the pre-update value of the
  column", i.e. a `SET` list reads the row as it was before the statement
  (the "all-at-once" rule; the page says it for variables, not in so many
  words for columns: reviewers check). Probed, eval/errors-txn P13: `SET n
  = m, m = n` swaps the two columns on both engines.
* `WHERE` keeps the rows where it is TRUE. PostgreSQL: "UPDATE":
  "Only rows for which this expression returns true will be updated."
  T-SQL: "UPDATE (Transact-SQL)", `WHERE`: "Specifies the conditions that
  limit the rows that are updated", with "Search condition
  (Transact-SQL)" for UNKNOWN. Incidentally probed: eval/bit-boolean
  (`UPDATE #f SET c = 7 WHERE id = 2` changes row 2 only, SQL Server).
* A key column is never set: PostgreSQL checks a non-deferrable primary
  key "immediately whenever a row is inserted or modified", not at the
  end of the statement as the standard says, so `SET k = k + 1` can fail
  part-way depending on the order rows are visited ("CREATE TABLE",
  "Compatibility", "Non-Deferred Uniqueness Constraints"), where SQL
  Server checks the statement's result. Outside the spec.

**`DELETE FROM t [WHERE p]`**: no alias, no T-SQL second `FROM`, no
PostgreSQL `USING`, no `OUTPUT`/`RETURNING`; removes the rows where `p` is
TRUE. PostgreSQL: "DELETE": "Only rows for which this expression returns
true will be deleted." T-SQL: "DELETE (Transact-SQL)", `WHERE`. Not
probed.

**Constraint violations.** A NULL in a `NOT NULL` or key column (T-SQL
515, SQLSTATE 23502 `not_null_violation`) and a key that two rows share
(T-SQL 2627, SQLSTATE 23505 `unique_violation`) are errors of the kind
`constraint` (decision 1 compares the kind, so which of two violations is
reported is not observed). PostgreSQL checks `NOT NULL` and a
non-deferrable primary key for each row as it is written ("CREATE
TABLE"); for `INSERT` that fails exactly when the statement's result has
a NULL there or a duplicate key, which is when SQL Server fails. Not
probed.

**Statement atomicity.** A data statement that fails has no effect: T-SQL
rolls back at least the failing statement ("SET XACT_ABORT
(Transact-SQL)": "When SET XACT_ABORT is OFF, in some cases only the
Transact-SQL statement that raised the error is rolled back and the
transaction continues processing"; with it `ON`, "the entire transaction
is terminated and rolled back"); PostgreSQL "treats every SQL statement
as being executed within a transaction" ("Transactions", 3.4) and an
error aborts it. The core keeps the state from before the statement
(`Sqldiff.Core.exec_error_atomic`). The error then ends the routine (C2a:
under `XACT_ABORT ON` in T-SQL, `abortsT`); what the caller sees of the
database afterwards differs between the dialects and is not observed
(`Sqldiff.Core.Obs`; `ASM-STATE-AFTER-ERROR` in `residual`). Probed,
eval/errors-txn P12: a multi-row `INSERT` with a duplicate in its second
row inserts nothing, an `UPDATE` overflowing on its second row changes
nothing, on both engines.

**Row order and which error.** Neither vendor fixes the order in which a
statement visits rows. A statement whose rows could fail with errors of
two kinds (an arithmetic error on one row, a constraint violation on
another) reports whichever comes first, so it is outside the slice
(`Sqldiff.Core.Stmt.dmlSafe`); within one kind the core's order is not
observed.

**Where the expressions are evaluated.** A `WHERE` that can raise is
outside the slice: which rows an engine tests depends on the access path
(an index on `k` tests `k = @k AND 1/c > 0` only on the rows with that
key). The values of `SET` are evaluated, in the core, only on the rows the
`WHERE` keeps. For PostgreSQL that is SQL's logical order: "The WHERE
Clause" (7.2.2) discards the rows for which the condition is not true,
and what is computed from the rows (the select list, 7.3; an `UPDATE`'s
values) is computed on what remains; the manual does not say it of
`UPDATE` in so many words (reviewers check). SQL Server does not promise
it, but the core's choice is one it may make, and T-SQL is the reference
side, whose possible outcomes the core needs only to include one of. Not
probed.

**Values written to a column** are `int` expressions of the slice or the
NULL literal, in both dialects ((C3c) into a string column: a string of
the slice that fits, or NULL, "The C3c tables"); anything else is outside
the spec, even on the C1 path. The column is `int`, so no conversion happens on the
write. Of another type, the value would be converted on assignment, and
the dialects convert differently (T-SQL truncates a `decimal` to `int`,
PostgreSQL rounds: probed, eval/casts/assign, `INSERT` and `UPDATE`),
which an uninterpreted operator does not show. The `WHERE` of `UPDATE`
and `DELETE` may be any condition (an uninterpreted operator on the C1
path).

**Constant parts of a data statement.** PL/pgSQL runs a data statement
through a prepared plan and may plan it with the variables' current
values as constants (PL/pgSQL "Plan Caching"; "PREPARE", custom plans),
and the planner folds constant subexpressions, raising their errors even
when no row is written (9.18.1 "CASE", 4.2.14 "Expression Evaluation
Rules"). So a part of an `UPDATE`'s values that reads no column and can
raise is outside the slice (`Sqldiff.Core.SExpr.constRaise`). `INSERT ...
VALUES` evaluates every value in any case, and a `WHERE` cannot raise.
Not probed. -/

/-! ## The C3c tables: string columns, constant defaults, indexes

Phase C3c widens what a catalog table (`CatTable`) may be, without a new
statement in the core: a string column is a column whose values are
strings (`Core.Val.str`), and a default is a value the lowering writes
where an `INSERT` leaves the column out. Each rule states the claim, the
vendor text and the probe. "eval/tables Tn" is probe `Tn` of
`eval/tables/{mssql,postgres}.sql` with their `.out` files, run on SQL
Server 2022 and PostgreSQL 18.6 on 2026-10-29 with the tables as
`sqldiff convert --to postgres` writes them. **Every vendor quote here
was written from memory** (the vendor sites were not reachable when this
was written) and needs checking against the page; the probes are what
the rules rest on.

**String column types** (`CatCol.str`). T-SQL `varchar(n)` or
`nvarchar(n)`, `1 ≤ n ≤ 4000`, against PostgreSQL `varchar(n)` with the
same `n` (the converter writes `varchar(n)` for both,
crates/sqldiff-convert/src/tsql.rs, `ty_pg`): the C2b types ("The C2b
slice: strings", with its citations: `n` bytes for `varchar`, byte-pairs
for `nvarchar`, characters for PostgreSQL). `max`, `char(n)`,
`nchar(n)`, `text` and every other type stay outside (the Rust side does
not send such a table). A string column is never a key column (below).
Probed, eval/tables T1: a value that fits is stored and read back as it
is, trailing spaces included, and `''` is not NULL, on both engines.

**Reads.** A column read in a `WHERE`, an `UPDATE`'s values or a C3b
pick has the column's type: a string of bound `n`, `wide` when the T-SQL
column is `nvarchar` (`colTy`). The bound holds for every value the
column can hold: at most `n` bytes or byte-pairs in SQL Server, at most
`n` characters in PostgreSQL, and both databases hold the same rows (the
state correspondence, decision 1). A string column is no integer, so it
never meets an integer operator, and every C2b rule applies to it as to
a string variable: `=`/`<>` only under the binary-collation fact (T-SQL
pads: `a = b` is `rtrim(a) = rtrim(b)`), `RTRIM`, `LEN`, concatenation,
`COALESCE`, `CASE`, `IS [NOT] NULL`. A column without a `COLLATE` clause
takes the database's default collation ("Set or change the column
collation" for SQL Server; PostgreSQL "Collation Support": a column's
collation defaults to the database's; both from memory), so the fact
covers it (`Catalog.binaryCollation`). Probed, eval/tables T5: in a
`Latin1_General_100_BIN2` database a column created without `COLLATE`
has that collation, and `v = 'a'` keeps `'a'` and `'a '` (2 rows) where
PostgreSQL keeps `'a'` only (1 row) and `rtrim(v) = rtrim('a')` keeps
both; under the server's default collation SQL Server also keeps `'A'`.
A pick (C3b rules 1 and 2) may now assign a string column to a string
variable: the stored value must fit the variable's declared type
("String stores, on every path": SQL Server truncates, PostgreSQL raises
22001; probed, eval/tables T9).

**Writes** (`colValue`, `colFits`). `INSERT ... VALUES` and `UPDATE ...
SET` store into a string column only the NULL literal or a string of the
slice whose bound is at most `n` and which, into a T-SQL `varchar`, is
not `wide`: then neither engine changes the value. A value that may be
too long is outside the spec: SQL Server raises 2628 ("String or binary
data would be truncated in table ..., column ...") only under
`ANSI_WARNINGS ON` and truncates silently under `OFF` ("SET
ANSI_WARNINGS (Transact-SQL)"; both from memory), a session setting the
routine does not fix; PostgreSQL raises 22001 ("value too long for type
character varying(n)"); and both truncate silently when the excess is
spaces only (PostgreSQL 8.3: "An attempt to store a longer string into a
column of these types will result in an error, unless the excess
characters are all spaces, in which case the string will be truncated to
the maximum length", from memory). Probed, eval/tables T2: 2628 against
22001 for `INSERT` and `UPDATE`, nothing written; `'abcde   '` into
`varchar(5)` stored as `'abcde'` on both; under `ANSI_WARNINGS OFF` SQL
Server stores `'abcde'` for `'abcdef'`. An integer is never stored into a
string column, nor a string into an integer column (both convert).

**`ANSI_PADDING`.** A T-SQL `varchar` column created under `SET
ANSI_PADDING OFF` trims trailing spaces from every value written to it;
an `nvarchar` column keeps them ("SET ANSI_PADDING (Transact-SQL)", from
memory). Probed, eval/tables T3: `'ab  '` is stored as 2 bytes in such a
`varchar` column (`is_ansi_padded` 0), as 8 bytes in the `nvarchar` one.
The catalog asserts that the T-SQL string columns were created with
`ANSI_PADDING ON` (the default); the Rust side refuses a T-SQL tree that
turns it off and names `ASM-TSQL-ANSI-PADDING` for a routine that writes
a table with a `varchar` column (the setting is stored per column, which
the DDL alone does not show).

**String keys are outside.** A key column is an integer (`CatCol.ok`):
SQL Server compares key values as `=` does, padding trailing spaces under
every collation and ignoring case under the usual ones, while PostgreSQL
compares exactly. Probed, eval/tables T6: in a BIN2 database `'a'` and
`'a '` are a duplicate key (2627); PostgreSQL stores both.
(`Core.keyOf` reads integer keys only.)

**Constant defaults** (`CatCol.dflt`). A column may have a default both
tables declare: an integer literal in the 32-bit range for an integer
column, or a string literal of the slice (printable ASCII, no backslash)
that fits for a string column (`CatCol.ok`); `DEFAULT NULL` is no
default. An `INSERT` whose column list leaves such a column out stores
the default in it. T-SQL "INSERT (Transact-SQL)": a column not in the
column list that "Has a default" gets it ("The default for the column is
used", from memory); PostgreSQL "INSERT": "Each column not present in the
explicit or implicit column list will be filled with a default value,
either its declared default value or null if there is none". Both
lowerings add the left-out columns that have a default to the
statement's column list, with the literal on every row (`defaultsFor`,
`lowerStmt`), so the core statement is the C3a `insert` and the two sides
get the same one. A literal cannot fail, so where it is evaluated does
not matter. Probed, eval/tables T4: an omitted column gets its default
(integer, negative integer, string, `N'x'` in SQL Server against `'x'`,
`NOT NULL` with a default), a listed column gets its value (also NULL),
an `UPDATE` leaves the default alone, `DEFAULT NULL` and no default give
NULL; a default that does not fit is accepted by `CREATE TABLE` and fails
the `INSERT` (2628 against 22001), which is why the default must fit.
Expressions, functions (`GETDATE()`, `NEWID()`, `NEXT VALUE FOR`,
`nextval`), a default only one table has, and two different defaults are
outside (the Rust side does not send the table). `UPDATE ... SET c =
DEFAULT` and `DEFAULT VALUES` stay outside the spec.

**Indexes.** A non-unique index (not `UNIQUE`, no filter, plain columns,
in PostgreSQL `btree`) does not change what a read or a write gives, the
rows, the error kinds and the values being those of the table without
it, except that each engine limits the size of an index entry and fails
a write whose entry is too large. Probed, eval/tables T7 (duplicates and
NULLs accepted, the same rows after `INSERT`, `UPDATE` and `DELETE`, both
engines) and T8: SQL Server warns at `CREATE INDEX` and fails the write
with 1946 for a key over 1700 bytes (nonclustered) or 900 bytes
(clustered); PostgreSQL fails it with 54000 for an entry over about 2704
bytes, `INCLUDE` columns counted. The check sees no index: the Rust side
sends a table with an index only when no row the table can hold exceeds
the limits, counting declared lengths (SQL Server: `int` 4, `varchar(n)`
`n` and `nvarchar(n)` `2n` bytes over the key columns, at most 900 for a
clustered and 1700 for a nonclustered index; PostgreSQL: `integer` 8 and
`varchar(n)` `4n + 8` bytes over the key and `INCLUDE` columns, at most
2600), and the catalog asserts it (`ASM-TABLES-AS-DECLARED`). A `UNIQUE`
or filtered index, any other access method, an expression or an option
leaves the table out. -/

/-- (C3c) A column's type and default can be used: a string column of
length `1 ≤ n ≤ 4000` that is not a key column; a default that is an
integer of the 32-bit range for an integer column, or a string literal of
the slice that fits for a string column. -/
def CatCol.ok (c : CatCol) : Bool :=
  (match c.kind with
    | .int _ | .date => true
    | .str n _ => decide (1 ≤ n ∧ n ≤ tsqlMaxLen) && !c.key
    | .bit => !c.key
    | .ts p => p ≤ 6) &&
  match c.dflt, c.kind with
  | none, _ => true
  | some (.int i), .int w => w.fits i
  | some (.str s), .str n _ => strLitOk s && decide (s.length ≤ n)
  | some (.bool _), .bit => true
  | some (.date v), .date => decide (0 ≤ v ∧ v ≤ 3652058)
  | some (.ts us), .ts p =>
    decide (0 ≤ us ∧ us < 3652059 * Core.Calendar.usPerDay) && us % (10 ^ (6 - p) : Nat) == 0
  | _, _ => false

/-- (C3c) A value of type `vt` can be stored in column `c` as it is: NULL;
an integer into an integer column; a string whose bound is at most the
column's length into a string column, and into a T-SQL `varchar` only
one that is not `wide` ("Writes" above). -/
def colFits (d : Dialect) (c : CatCol) (v : SExpr × STy) : Option SExpr :=
  fitValue (kindTy d c.kind) v

/-- (C3c) The left-out columns of an `INSERT` into `t` with column list
`cs` that have a default, each with its literal ("Constant defaults"). -/
def defaultsFor (t : CatTable) (cs : List Core.Col) : List (Core.Col × Core.Expr) :=
  t.cols.filterMap fun c =>
    if cs.contains c.name then none else c.dflt.map fun v => (c.name, .sc v)

/-- Spelling of a column as a statement of the dialect writes it. -/
def colKey (d : Dialect) (c : CatCol) : String :=
  match d with
  | .tsql _ _ => c.tsql
  | .pg _ _ _ => c.name

/-- The table can be used: names ASCII and distinct; T-SQL spellings
distinct up to case (also two equal spellings, which `caseSafe` alone
lets through); (C3c) every column's type and default as "The C3c
tables" admits them (`CatCol.ok`). (C3d part 2) The opaque columns'
names too, among the others; only a read-only table has opaque columns,
and in one a key column may have any type of the slice (a string or bit
key, like an opaque one, only leaves the table without a key,
`keyVisible`). -/
def CatTable.wellFormed (t : CatTable) : Bool :=
  let names := t.cols.map (·.name) ++ t.opaqueCols.map (·.name)
  !t.cols.isEmpty && names.all (fun n => n.toList.all fun (ch : Char) => ch.toNat < 128) &&
    -- (C3d) No `.` in a column's name, so that a block's `i.c` (`Core.srcCol`)
    -- reads as one source's column.
    names.all (fun n => !n.toList.contains '.') &&
    distinct names &&
    caseSafe (t.cols.map (·.tsql) ++ t.opaqueCols.map (·.tsql)) &&
    distinct ((t.cols.map (·.tsql) ++ t.opaqueCols.map (·.tsql)).map (·.toLower)) &&
    (t.readOnly || t.opaqueCols.isEmpty) &&
    t.cols.all fun c => (if t.readOnly then { c with key := false } else c).ok

/-- (C3d part 2) The primary key is visible: no opaque column is a key
column, and every key column has a type `Core.keyOf` reads (an integer, a
date, a timestamp). Always so for a table that is not read-only and
`wellFormed` (`CatCol.ok`). Otherwise the table has no key in the core
(`toDef`): the key closure cannot determine it, and a key of the slice
columns alone would be a part of the real key, which two rows may share
("The C3d tables"). -/
def CatTable.keyVisible (t : CatTable) : Bool :=
  t.opaqueCols.all (!·.key) &&
    t.cols.all fun c => !c.key || match c.kind with
      | .int _ | .date | .ts _ => true
      | _ => false

/-- The catalog table a statement names, if it can be used. A data
statement asks `writeTableOf`, which refuses a read-only table. -/
def tableOf (d : Dialect) (target : List String) : Option CatTable :=
  let (Γ, key) : Catalog × Option String := match d with
    | .tsql dep Γ => (Γ, lookup dep.tables (dotted target))
    | .pg Γ _ _ => (Γ, some (dotted (target.map pgName)))
  key.bind fun n =>
    (Γ.tables.find? (·.name == n)).bind fun t => if t.wellFormed then some t else none

/-- (C3d part 2) The catalog table a data statement (`INSERT`, `UPDATE`,
`DELETE`) writes: one `tableOf` admits that is not read-only. Every write
in the spec goes through it ("The C3d tables"). -/
def writeTableOf (d : Dialect) (target : List String) : Option CatTable :=
  (tableOf d target).bind fun t => if t.readOnly then none else some t

/-- (C3d part 2) `writeTableOf` never gives a read-only table. That no
lowered data statement writes one is `lowerStmt_writes_not_readOnly`. -/
theorem writeTableOf_not_readOnly {d : Dialect} {target : List String} {t : CatTable}
    (h : writeTableOf d target = some t) : t.readOnly = false := by
  unfold writeTableOf at h
  cases ho : tableOf d target with
  | none => simp [ho] at h
  | some u =>
    cases hr : u.readOnly <;> simp_all

/-- A column named in an `INSERT` column list or as a `SET` target. -/
def colOf (d : Dialect) (t : CatTable) (x : String) : Option CatCol :=
  let x := match d with
    | .tsql _ _ => x
    | .pg _ _ _ => pgName x
  lookup (t.cols.map fun c => (colKey d c, c)) x

/-- The columns an `UPDATE` or `DELETE` brings into scope, and for
PostgreSQL the table's reference name (the last part of its name). -/
def scopeOf (d : Dialect) (t : CatTable) (target : List String) : Scope :=
  { cols := t.cols.map fun c => (colKey d c, c.name)
    rel := match d with
      | .tsql _ _ => none
      | .pg _ _ _ => target.getLast?.map pgName
    colTys := t.cols.map fun c => (c.name, c.kind) }

/-- The table as the core sees it. (C3d part 2) For a read-only table, its
projection on the slice columns `cols` (the opaque ones dropped), with the
key only when it is visible (`keyVisible`; "The C3d tables"). -/
def CatTable.toDef (t : CatTable) : Core.TableDef :=
  ⟨t.name, t.cols.map fun c => ⟨c.name, c.notNull, c.key && t.keyVisible⟩⟩

/-- (C3d part 2) The core table has a key only when the catalog's key is
wholly visible (`keyVisible`): no opaque key column, every key column of
a type `Core.keyOf` reads. -/
theorem CatTable.keyVisible_of_hasKey {t : CatTable} (h : t.toDef.hasKey = true) :
    t.keyVisible = true := by
  simp only [CatTable.toDef, Core.TableDef.hasKey, List.any_map, List.any_eq_true,
    Function.comp_def, Bool.and_eq_true] at h
  obtain ⟨_, _, _, hv⟩ := h
  exact hv

def CatTable.isKey (t : CatTable) (c : Core.Col) : Bool :=
  t.cols.any fun d => d.name == c && d.key

/-- A value written to column `c`: an `int` expression of the slice or
the NULL literal into an integer column ("Values written to a column"
above), (C3c) a string of the slice that fits or the NULL literal into a
string column (`colFits`, "The C3c tables"), else outside the spec. -/
def colValue (d : Dialect) (env : TyEnv) (sc : Scope) (c : CatCol) (e : Ast.Expr) :
    Option Core.Expr :=
  match typedTo d env sc (some (kindTy d c.kind)) e with
  | some v => (colFits d c v).map .sc
  | none => none

/-- A data statement may stand here: in a procedure, and in T-SQL only
when row counts are not observed. -/
def dataOk (d : Dialect) (k : Ast.RoutineKind) : Bool :=
  (k matches .Procedure) &&
    match d with
    | .tsql dep _ => !dep.rowCounts
    | .pg _ _ _ => true

/-! ## The C3b slice: queries

Phase C3b reads one table of the target catalog, in exactly the shapes
`sqldiff convert --to postgres` writes, lowered to the core's `query`
(`Sqldiff.Core.Stmt.query`, `Pick`, `blockRows`, `pickRows`). Each rule
states the claim, the vendor text and the probe status, as for C2a.
"eval/queries Qn" is probe `Qn` of `eval/queries/{mssql,postgres}.sql`
with their `.out` files, run on SQL Server 2022 and PostgreSQL 18 on
2026-10-29 with the converter's own PL/pgSQL shapes. The vendor quotes
below were written **from memory** (the vendor sites were not reachable
when this was written) and need checking against the pages; the probes
are what the rules rest on.

**The shapes.** A query is a plain `SELECT items FROM t [WHERE c]`: one
table of the catalog (`tableOf`, as for C3a; (C3d) or a block of several,
"The C3d blocks" below), no table hints, `DISTINCT`, `TOP`, `GROUP BY`,
`HAVING`, `ORDER BY`, paging, locking clause, set operation, CTE or `FOR
JSON` (`selParts`). Its
`WHERE` sees the table's columns, resolved by `resolveName` exactly as in
an `UPDATE` (C3a, "Names in data statements"): a T-SQL name is a column
spelt as the catalog spells it, a PL/pgSQL name that is both a column and
a variable is outside the spec (probed, eval/queries Q9: PostgreSQL
raises 42702 "column reference is ambiguous" in a `FOR` query and in
`EXISTS`). The `WHERE` keeps the rows where it is TRUE, FALSE and
UNKNOWN skip them (as for C3a; probed, eval/queries Q1 and Q4: `id =
@k` with `@k` NULL matches no row on both engines, under `ANSI_NULLS ON`,
`ASM-TSQL-ANSI-NULLS`). Queries stand only in procedures (a function's
reads would add PostgreSQL's volatility and snapshot rules to the spec).

1. **`SELECT @x1 = e1, ..., @xn = en FROM t WHERE c`** (T-SQL), and
   its conversion, the PL/pgSQL block

   ```
   DECLARE r record;
   BEGIN
   FOR r IN SELECT e1 AS a1, ..., en AS an FROM t WHERE c LOOP
     x1 := r.a1; ...; xn := r.an;
   END LOOP;
   END;
   ```

   lower to `query t c (.row false [(x1, e1), ...])`. T-SQL assigns
   for each row the query returns, so with no row every variable keeps
   its value, and with several the values of one of them, which one not
   specified ("SELECT @local_variable (Transact-SQL)": "If the SELECT
   statement returns no rows, the variable retains its present value";
   "If ... returns more than one value, the variable is assigned the last
   value that is returned"; from memory). The `FOR` loop assigns once per
   row, in the order the query returns them, and runs no iteration for
   no row (PL/pgSQL "Looping Through Query Results"; from memory). Probed,
   eval/queries Q1 (one row assigns, a NULL column assigns NULL, no row
   keeps the value, two targets, both engines) and Q2 (three matching
   rows: T-SQL ends with one of them; PostgreSQL iterates three times and
   ends with one of them).

   Which row "last" is depends on the plan on both sides, so the rule
   asks that **at most one row can match** (`Core.Block.atMostOne`): `c` is a
   conjunction (`AND`, in the slice) with, for every column of the
   table's primary key, a conjunct `k = e` or `e = k` where `e` reads no
   column. Every row `c` keeps then has the key `e` gives, the same
   value for every row (no variable the query assigns is read by `c`,
   below), and the primary key (`ASM-TABLES-AS-DECLARED`, a fact of both
   catalogs: not deferrable in PostgreSQL, so it holds at every statement
   boundary, "CREATE TABLE"; checked on every write in SQL Server) admits
   at most one such row in every database state an engine can be in.
   (The core's `Refines` quantifies over every list of rows, including
   ones that break the key; on those it takes the last row in list
   order, which is a choice of the core and not a claim about either
   engine.) Without such a `WHERE` the pair is outside the spec.

   The values `ei` are `int` expressions of the slice (or NULL) that
   read no variable and cannot raise (`pickValue`): T-SQL may compute
   them on rows the `WHERE` later discards (the plan may place a compute
   scalar below the filter), and a variable read there would see the
   assignments of earlier rows (`SELECT @x = @x + c` is a running
   total in T-SQL, never in PostgreSQL, whose query parameters are fixed
   when it starts: PL/pgSQL "Variable Substitution", from memory). The
   targets are distinct `int` variables, so the store converts nothing
   (the field `r.ai` is `integer`, from an `integer` column or
   expression), and `c` reads none of them. The `FOR` block is exactly
   this shape: its one declaration is a `record` variable `r` without a
   default, it holds only the loop, the loop's body is exactly the
   assignments `xi := r.ai` in the order of the select list, each `ai` an
   alias of its item, distinct. So `r` is read nowhere else, its scope
   is the block, and the block's other effect, `FOUND`, is never read (a
   routine that reads `found` is outside the spec, `wellScoped`). `r` is
   not a name of the routine (no shadowing), and is a variable for the
   query's name resolution (`resolveName`).

2. **`SET @x = (SELECT e FROM t WHERE c)`** (T-SQL) and **`x := (SELECT e
   FROM t WHERE c)`** (PL/pgSQL), a scalar subquery as the whole value,
   lower to `query t c (.row true [(x, e)])`: NULL when no row matches,
   the row's value when one does, and an error when several do (T-SQL
   512, PostgreSQL 21000 `cardinality_violation`, "Scalar Subqueries",
   4.2.11; "Subqueries" (Transact-SQL); from memory). Probed,
   eval/queries Q5 (300, NULL, 512 against 21000 with the variable kept,
   both engines). The same rules for `e` and `x`. (C3d) The key-fixing
   `WHERE` is no longer required: the error is the core's
   `cardinality` ("The C3d blocks").

3. **`IF [NOT] EXISTS (SELECT 1 FROM t [WHERE c]) ... [ELSE ...]`**, the
   same text in both dialects (`SELECT *` too), lowers to `query t c
   (.any existsVar)` followed by the `IF` on that variable (`NOT`: on
   its negation). `EXISTS` is TRUE when the subquery returns a row and
   FALSE otherwise, never UNKNOWN ("EXISTS (Transact-SQL)"; PostgreSQL
   9.23.1 "EXISTS"; from memory). Probed, eval/queries Q4. The select
   list is not evaluated: probed on both engines, eval/queries Q7
   (`EXISTS (SELECT 1/0 ...)` is TRUE, no error); the rule takes only `1`
   and `*` there all the same. No row count is needed, so any `WHERE` is
   allowed (a C1 `WHERE` puts the routine outside C2a as for C3a).
   `existsVar` is declared as a local of both lowered routines, set
   right before its one test and never read elsewhere; the certificate
   need not name it (`check`). A routine can spell it: T-SQL variables
   start with `@`, but a PL/pgSQL `"%exists"` arrives unquoted, as this
   very name. A routine that declares it and uses `EXISTS` is refused
   as a duplicate local (`wellScoped`), and a renaming that maps a
   source name onto it is refused by the injectivity test (`InjOn`). Only the whole condition of an `IF` (or `ELSE IF`); `WHILE
   EXISTS` and `EXISTS` inside a larger condition are outside the spec.

4. **`SELECT @x = COUNT(*) FROM t [WHERE c]`** (T-SQL), **`SELECT
   COUNT(*) INTO x FROM t [WHERE c]`** (PL/pgSQL, not `STRICT`) and the
   scalar-subquery form `SET @x = (SELECT COUNT(*) FROM t [WHERE c])` / `x
   := (...)` lower to `query t c (.count x)`: an aggregate without `GROUP
   BY` returns exactly one row, so the assignment always happens, with
   the number of rows `c` keeps (0 for none). T-SQL `COUNT` is `int` and
   raises 8115 beyond 2^31-1 ("COUNT (Transact-SQL)", from memory);
   PostgreSQL `count` is `bigint`, converted to the `integer` target on
   assignment, raising 22003 beyond it (PL/pgSQL "Assignment"; from
   memory): both the core's overflow (`arithmetic`), whose T-SQL side
   rests on `ASM-TSQL-ARITH-ERRORS` like every overflow. Probed,
   eval/queries Q6 (0 on no row, 1, 3; `int` and `bigint`); the overflow
   is not probed (2^31 rows). The target is an `int` variable that `c`
   does not read.

**Row counts.** A T-SQL `SELECT @x = ...`, `IF EXISTS` and `SET @x =
(subquery)` send no rows-affected count even without `SET NOCOUNT ON`
(probed, eval/queries Q3: only the `UPDATE` control prints one), so the
deployment's `rowCounts` does not matter here.

**Errors.** A query fails only by its `WHERE` or its values (and `COUNT`
by overflow); it writes no table, so statement atomicity is trivial, and
a failed query leaves the variables as they were (the core assigns after
evaluating everything). A `WHERE` or a value that can raise is outside
the C2a slice (`Core.Stmt.dmlSafe`), as for C3a: which rows an engine
tests depends on the access path.

**What a query sees.** The database as the routine's own earlier
statements left it (probed, eval/queries Q8: a read after an `UPDATE` in
the same procedure sees it, both engines); other sessions are not
modelled (decision 1: serial, one session). -/

/-! ## The C3d blocks: several tables, joins, aliases, qualified names

Phase C3d, part 1, reads a query block instead of one table, in the
same four statement shapes as C3b: `SELECT @x = ...` against the
converter's `FOR` block, a scalar subquery as the whole assigned value,
`COUNT(*)` into a variable, `IF [NOT] EXISTS (...)` as the whole
condition of an `IF`; procedures only. The block is
`Sqldiff.Core.Block` (`blockRows`). "eval/queries Qn" for `n` from 11
on is probe `Qn` of `eval/queries/blocks-{mssql,postgres}.sql` with
their `.out` files, run on SQL Server 2022 (16.0.4305.5) and PostgreSQL
18.6 on 2026-10-30, with the converter's own PL/pgSQL shapes. **The
vendor quotes below were written from memory** (the vendor sites were
not reachable when this was written) and need checking against the
pages; the probes are what the rules rest on.

**The FROM** (`readFrom`, `readJoin`). A list of items separated by
commas, each a catalog table (`tableOf`, as in C3a; with or without an
alias, no table hints) or a join of such: `[INNER] JOIN ... ON c` and
`CROSS JOIN`. The block's sources are the tables in the order they are
written; its conditions are every `ON` and then the `WHERE`, each split
at its top-level `AND`s (`splitConj`). Its rows are the tuples of the
product where every condition is TRUE. T-SQL "FROM clause plus JOIN,
APPLY, PIVOT (Transact-SQL)": `INNER` "Specifies all matching pairs of
rows are returned", `CROSS JOIN` "Specifies the cross-product of two
tables"; for an inner join a predicate's placement in `ON` or `WHERE`
"does not make a difference" (it does for outer joins). PostgreSQL
7.2.1.1 "Joined Tables": "For every possible combination of rows from T1
and T2 (i.e., a Cartesian product), the joined table will contain a
row" (`CROSS JOIN`), "FROM T1 CROSS JOIN T2 is equivalent to FROM T1
INNER JOIN T2 ON TRUE. It is also equivalent to FROM T1, T2", and an
inner join has a row for each pair "that satisfies the join condition".
Probed, eval/queries Q11: the same count with the condition in `ON`, in
`WHERE` with a comma list, with `CROSS JOIN`, and all of it in the `ON`
(2 on both); products of two and three tables (12, 36); a NULL join key
matches nothing, also NULL against NULL in a self-join (0 and 0); a
self-join through two aliases (3 and 4 rows); `EXISTS` over a join and a
cross join; the same on both engines. Order is not observed (the C3a
bag reading); which tuple a pick takes is ruled below. `LEFT`, `RIGHT`,
`FULL` joins, `APPLY`, derived tables, `VALUES`, table functions,
`LATERAL`, `NATURAL` and `USING` stay outside: the parser keeps
`LEFT`/`RIGHT`/`FULL` as join kinds, which `readJoin` refuses, refuses
`APPLY`, `NATURAL`, `USING` and a parenthesised join outright
(`crates/sqldiff-syntax/tests/routine_roundtrip.rs`), and whatever else it
reads `readJoin` refuses: it takes catalog tables and inner and cross
joins of them only.

**Where an `ON` looks.** An `ON` may name only the sources of its own
join (the tables joined so far in that `FROM` item): T-SQL raises 4104
("The multi-part identifier ... could not be bound") for a table joined
later or another comma item, PostgreSQL 42P01 ("invalid reference to
FROM-clause entry for table"), probed eval/queries Q12 (both cases, both
engines); a `JOIN` binds more tightly than a comma (PostgreSQL 7.2.1.1,
from memory). The lowering resolves each `ON` with only those sources
visible (`Scope.vis`) and refuses a name of another.

**Reference names** (`refName`). A source's reference name is its alias,
or, without one, the last part of the table's name; an aliased table's
own name is no reference name. T-SQL "FROM clause": the alias is used
"to distinguish a table or view in a self-join or subquery", and the
table's name can no longer qualify a column (error 4104); PostgreSQL
7.2.1.2 "Table and Column Aliases": "the alias becomes the new name of
the table reference so far as the current query is concerned — it is
not allowed to refer to the table by the original name elsewhere in the
query" (42P01). Probed, eval/queries Q12: 4104 and 42P01; two aliases
that swap the two tables' names resolve to the aliased tables (`FROM a
AS b, b AS a`: `b.n` is `a`'s column, both engines). The reference names
of a block are ASCII and distinct: T-SQL up to case (two sources with
one exposed name raise 1013, "The objects ... have the same exposed
names"; whether `X` and `x` are one depends on the collation), PostgreSQL
after folding (42712, "table name specified more than once"); probed,
eval/queries Q12 (1013, 42712).

**Names in a query block** (`resolveName`, `resolveQual`).

* A qualified `q.c` names column `c` of the one source whose reference
  name is `q`. T-SQL: `q` must be spelt exactly as the reference name
  (no other reference name equal to it up to case), and `c` exactly as
  the catalog's T-SQL spelling: in a case-sensitive database a qualifier
  or a column in another case does not resolve (probed, eval/queries Q12
  in a `Latin1_General_100_CS_AS` database: 4104 and 207), so the exact
  spelling resolves alike under every collation. PostgreSQL: `q` and `c`
  folded (probed: alias `X` read as `x.id`). A PostgreSQL qualifier must
  not be a variable of the routine (a parameter, a local, `found`, the
  `FOR` block's record) or the routine's name: PL/pgSQL "Variable
  Substitution" reads `q.c` as a field of a record variable or as the
  parameter `c` qualified by the function's name ("the function's name
  ... can be used to qualify references to its parameters", from
  memory), and either raises 42702 against the column. Probed,
  eval/queries Q12: a record variable as qualifier 42702, the loop's own
  record 55000, the routine's name with a parameter of the column's name
  42702; an integer variable as qualifier reads the column (refused all
  the same, as the simpler rule). Three-part names are outside.
* An unqualified `c` names the column of the one source that has it:
  a name that is a column of two sources is ambiguous, T-SQL 209
  ("Ambiguous column name"), PostgreSQL 42702 (probed, eval/queries Q12);
  T-SQL counts the sources up to case (refused in a case-sensitive
  database too). In PostgreSQL a name that is a column of any source and
  also a variable is outside (C3a's rule, now over every source; probed,
  eval/queries Q12: 42702 for a column of the second table), and so is a
  reference name (a whole-row reference) and (C3d part 2, review) a
  system column's name (`pgSystemCols`), a column of every table (probed,
  eval/queries Q20: 42702 for each of the six as a variable, in the
  converter's query and in an `UPDATE`'s `WHERE`); a name that is no
  column of any source is a variable. T-SQL names are never variables.

**The picks.**

* **`SELECT @x = e, ...`** and its `FOR` block (C3b rule 1) assign from
  the last tuple, `Pick.row false`, only on a block that keeps at most
  one tuple on every database the engines can be in: the **key
  closure** `Core.Block.atMostOne`, whose meaning is
  `Sqldiff.Core.kept_length_le_one` (`Sqldiff/Core/Fidelity.lean`). A
  source is determined when each of its primary-key columns `k` has a
  top-level conjunct `k = e` or `e = k` (`Core.eqFixes`: the core's `=`
  between two values of one kind, an integer of any width, a date or a
  timestamp, the comparisons the slice types as `cmp`) where `e` reads
  only columns of sources already determined, variables the pick does
  not assign (no condition reads a target) and constants; every source
  must be determined. String equality (`streq`, under T-SQL's padding),
  `IS NULL`, a key of a `UNIQUE` index (not in the catalog) and nullable
  columns are never steps; a key column is `NOT NULL`. A date compared
  with a timestamp is `toTs` of the date (C2c): a timestamp key column
  equated with a date is a step (`k = toTs e`; both engines widen the
  date to midnight, exactly; checked by the review of C3d on both
  engines), a date key column equated with a timestamp is not (the
  column is the side wrapped in `toTs`). T-SQL compares an `int` key with a `bigint`
  value by widening the `int` and PostgreSQL by its cross-type `=`
  operator, both exact, as the core's integers are (C2c; the C3b rule
  already took a `bigint` variable against an `int` key). Probed,
  eval/queries Q11: by key across a join (30 and 200), no row when the
  join key is NULL (the variables keep -1), through a self-join, and a
  two-column key fixed from another table's column, both engines.
* **A scalar subquery** `SET @x = (SELECT e FROM ...)`, `x := (SELECT e
  FROM ...)` (C3b rule 2) lowers to `Pick.row true` on any block: NULL
  on no row, the value on one, and on more than one the core's
  `cardinality` error, T-SQL 512 ("Subquery returned more than 1
  value"), PostgreSQL 21000 ("more than one row returned by a subquery
  used as an expression"), with the variable kept ("Subqueries
  (Transact-SQL)"; PostgreSQL 4.2.11 "Scalar Subqueries": "It is an
  error to use a query that returns more than one row ... as a scalar
  subquery"; from memory). Probed, eval/queries Q13: NULL, 200, 512
  against 21000 with the variable kept, and under `XACT_ABORT ON` the
  error ends the procedure and reaches the caller (both engines). Such a
  statement counts as able to fail unless its block is closed
  (`Core.Stmt.failsHere`), so a T-SQL routine without `SET XACT_ABORT
  ON` (where SQL Server goes on after the 512) is then not a C2a
  confirmation.
* `COUNT(*)` and `EXISTS` (C3b rules 3 and 4) over any block: the number
  of tuples, and whether there is one (probed, eval/queries Q11).

**Evaluation.** Every value a pick assigns reads no variable and cannot
raise (`pickValue`, as in C3b); a condition that can raise puts the
routine outside the C2a slice (`Core.Stmt.dmlSafe`): which tuples an
engine tests, and in which order its predicates run, depend on the plan,
and PostgreSQL may fold a constant part while planning. `ORDER BY`,
`TOP`, aggregates other than `COUNT(*)`, `GROUP BY`, `DISTINCT`, outer
joins, subqueries inside conditions, and writes from queries stay
outside (later parts of C3d). -/

/-! ## The C3d tables: read-only tables

Phase C3d, part 2, lets a query read a table that the C3a, C3b and C3c
rules keep out because of what it carries besides its columns: foreign
keys, `UNIQUE` and `CHECK` constraints, identity, defaults of any kind,
computed columns, system versioning, and columns of types outside the
slice. Such a table is **read-only** (`CatTable.readOnly`): no write of
either routine names it. "eval/queries Qn" for `n` from 16 on is probe
`Qn` of `eval/queries/tables-{mssql,postgres}.sql` with their `.out`
files, run on SQL Server 2022 (16.0.4305.5) and PostgreSQL 18.6 on
2026-10-30. **The vendor quotes below were written from memory** and
need checking against the pages; the probes are what the rules rest on.

**Only reads.** `INSERT`, `UPDATE` and `DELETE` find their table through
`writeTableOf`, which refuses a read-only one (reason code
`stmt:<kind>:table:read-only`); `MERGE`, `TRUNCATE` and every other
write are outside the spec in any case. Theorem
`lowerStmt_writes_not_readOnly`: a data statement `lowerStmt` lowers
names a table that is not read-only, and data statements reach the core
only through `lowerStmt`. The slice check (`sliceStmt`) and the
diagnostics find the table through `writeTableOf` too, by construction
(`writeTableOf_not_readOnly` covers `writeTableOf` alone).
A call is the world's effect (`Core.WorldModel.call`), which
may change any table, a read-only one included, as it may change tables
outside the catalog: the opaque columns are, like those tables, part of
the database the world sees and the core does not (a callee's effect on
the projection may depend on them; `Refines` quantifies over every world
model). So the routines' own statements never write the table, and its
rows change only by the world, alike on both sides.

**The projection.** The core's table is the projection of the real one
on the columns of the slice (`CatTable.toDef`): each real row gives one
core row, multiplicity kept (tables are lists, read as bags, "The
database" in `Core/Semantics.lean`). A read of the core table gives
what the engines' read gives exactly when no expression reads a column
outside the projection and nothing in the read depends on the row as a
whole: the select list of `SELECT @x = ...`, the scalar subquery and
the `FOR` block's query are slice expressions over named columns
(`pickValue`), `COUNT(*)` counts rows, `EXISTS` tests for one and its
select list (`1` or `*`) is not evaluated (eval/queries Q7, and Q16 for
`SELECT *` over a computed column that raises), every condition reads
named columns, and a whole-row reference (a PL/pgSQL reference name read
as a value) is outside the spec ("Names in a query block"). An opaque
column is never in `Src.cols`, so neither resolution of a name
(`resolveName`, `resolveQual`) gives it: a statement that names one is
outside the spec, on the C1 path too (`stmt:select:column:opaque`).

**What reads do not see.** A `CHECK`, `UNIQUE` or foreign-key
constraint, a default, an identity property, a trigger and an index act
only when a row is written: SQL Server and PostgreSQL check a constraint
"when data is inserted or updated" and fire DML triggers on `INSERT`,
`UPDATE` and `DELETE` only ("CREATE TABLE (Transact-SQL)", "CREATE
TRIGGER (Transact-SQL)"; PostgreSQL 5.4 "Constraints", "CREATE
TRIGGER"; from memory). A T-SQL computed column that is not persisted is
computed when a query reads it: a read that does not name it does not
compute it ("Specify computed columns in a table", from memory). Probed,
eval/queries Q16: a table with a foreign key, `UNIQUE`, `CHECK`,
identity, an expression default, an index, `decimal` and `datetime2(7)`
columns, a persisted computed column and a non-persisted one whose
expression raises (`1/0` on every row) gives the same `COUNT`, `EXISTS`
(also `SELECT *`), pick by key across a join and scalar subquery as the
plain table with the same rows, and the raising column raises (8134) only
in a query that names it, also in a `WHERE` (Q19); PostgreSQL likewise,
with identity, `numeric`, `timestamp`, a stored generated column and a
virtual one (PostgreSQL 18's default kind) that raises 22012 only where
named. A system-versioned table read with a plain
`FROM` (no `FOR SYSTEM_TIME`, which the parser does not read) gives its
current rows only, never its history ("Temporal tables", from memory);
probed, eval/queries Q17 (after an `UPDATE` and a `DELETE` of three
rows, `COUNT` is 2, the pick by key gives the updated value, the deleted
key no row, and the history table holds the 2 old versions).

**What does change a read, and keeps a table out** (the Rust side,
`cert.rs`, sends no such table; the catalog asserts it). A SQL Server
security policy's filter predicate and a PostgreSQL row-level security
policy hide rows from a read ("Row-level security", "CREATE SECURITY
POLICY (Transact-SQL)"; PostgreSQL 5.9 "Row Security Policies", from
memory). Probed, eval/queries Q18: under a filter predicate (`owner =
USER_NAME()`) SQL Server's `COUNT` drops from 3 to 1, for `dbo` and for
another user; PostgreSQL's from 3 to 1 for the table's owner once row
security is enabled and forced (enabled alone, the owner still reads 3;
a superuser bypasses it). A PostgreSQL rule, inheritance child or partition changes what
`FROM t` reads; dynamic data masking changes what a read of the masked
column gives (such a column is opaque), as does a `COLLATE` clause under
the binary-collation fact (such a column is opaque too); a statement that
adds, drops, renames or retypes a column, drops the table or the key, or
runs as dynamic SQL makes the DDL no description of the table.

**Names** (`Scope.holders`). An opaque column is a column for name
resolution: an unqualified name that is an opaque column of one source
and a column of another is ambiguous (T-SQL 209, PostgreSQL 42702), and
a PL/pgSQL variable spelt as an opaque column of a source conflicts with
it (42702): the engines resolve names before they look at types. Probed,
eval/queries Q19: 209 and 42702 for a name that is a `decimal`
(`numeric`) column of one table and an integer column of the other,
none once qualified; 42702 for a parameter spelt as a `numeric` column
in the converter's `FOR` query. So the
holder of a name counts every column, opaque or not, and an opaque
holder resolves to nothing (`resolveName`).

**Keys.** The key closure (`Core.Block.atMostOne`) and its meaning
`Core.kept_length_le_one` ask that the core's rows have distinct keys.
The projection keeps the real rows' key values only when the whole
primary key is in it: a key column that is opaque, or a string or `bit`
column (outside `Core.keyOf`), leaves the table without a key in the core
(`CatTable.keyVisible`), since two rows may share the visible part of a
key (Q16: two rows with one `ord_id` and two `line_no`). A table
without a key is still read: `COUNT(*)`, `EXISTS` and the scalar
subquery (over a block it cannot close, the scalar subquery counts as
able to fail, `Core.Stmt.failsHere`) need no key; only `SELECT @x = ...`
and its `FOR` block need the closure. A `UNIQUE` constraint is not used
as a key (outside C3d part 2). The Rust side sends key flags only when
both tables declare the same primary key in their `CREATE TABLE`, on
columns of key types on both sides, with no `DEFERRABLE` or `INITIALLY`
anywhere in the statement (`INITIALLY DEFERRED` alone makes a PostgreSQL
key deferrable; review of C3d part 2) (a PostgreSQL
deferred key may hold two equal keys until the transaction commits).

**Nullability.** `notNull` is claimed only where both DDLs say `NOT NULL`
(or the column is a key column of both): a column nullable on one side
only is read as nullable, the weaker claim (`kept_length_le_one` then
asks fewer columns to be non-NULL; no write reaches the table). A T-SQL
column that says neither `NULL` nor `NOT NULL` is read as nullable,
whatever `ANSI_NULL_DFLT_ON` made it. -/

/-- The variable an `EXISTS` test is put in. A PL/pgSQL `"%exists"` can
spell it; `wellScoped` and the renaming's injectivity refuse the
collision (see the C3b rules). -/
def existsVar : Var := "%exists"

/-- The clauses of a plain `SELECT`: its items, its `INTO` targets and
`STRICT`, its `FROM` list and its `WHERE`. Anything else in the query
(`WITH`, `FOR JSON`, a set operation, `DISTINCT`, `TOP`, `GROUP BY`,
`HAVING`, `ORDER BY`, paging, a locking clause, `VALUES`) puts it outside
the spec. -/
def selParts : Ast.Select →
    Option (List Ast.SelItem × List Ast.Expr × Bool × List Ast.From × Option Ast.Expr)
  | .Select [] false [] false none items into strict froms wh [] none [] none none [] none =>
    some (items, into, strict, froms, wh)
  | _ => none

/-- A query as every reader of it sees it: the lowering (`lowerSelectT`,
`lowerSelectP`, `lowerScalar`, `lowerExists`, `lowerForBlock`), the store
check (`itemTy`, `pickStoresOk`, `forStoresOk`), the slice check
(`querySlice`) and the diagnostics (`Sqldiff.Mirror.whySpec`) all read a
query through `querySpec`, so they cannot disagree about its tables, the
names in scope or its conditions. -/
structure QSpec where
  items : List Ast.SelItem
  into : List Ast.Expr
  strict : Bool
  /-- The catalog tables of the `FROM`, in order. -/
  tabs : List CatTable
  /-- The names the select list and the `WHERE` see. -/
  sc : Scope
  /-- The conditions with the names each sees, the `WHERE` last. -/
  conds : List (Scope × Ast.Expr)

/-- (C3d) A source's reference name: its alias, else the last part of
the table's name; T-SQL as written, PostgreSQL folded ("Reference
names"). -/
def refName (d : Dialect) (tgt : List String) (alias : Option String) : Option String :=
  (match alias with
    | some a => some a
    | none => tgt.getLast?).map fun n => match d with
      | .tsql _ _ => n
      | .pg _ _ _ => pgName n

/-- (C3d) One item of the `FROM` list, its sources counted from position
`i`: its catalog tables with their reference names, and its `ON`
conditions, each with the positions of the sources it may name (those of
its own join, "Where an `ON` looks"). `INNER JOIN ... ON` and `CROSS
JOIN` only. -/
def readJoin (d : Dialect) (i : Nat) :
    Ast.From → Option (List (CatTable × String) × List (List Nat × Ast.Expr))
  | .Table tgt alias [] => do
    let t ← tableOf d tgt
    let r ← refName d tgt alias
    pure ([(t, r)], [])
  | .Join kind l r on => do
    let (ls, lc) ← readJoin d i l
    let (rs, rc) ← readJoin d (i + ls.length) r
    let vis := List.range' i (ls.length + rs.length)
    match kind, on with
    | "INNER", some c => pure (ls ++ rs, lc ++ rc ++ [(vis, c)])
    | "CROSS", none => pure (ls ++ rs, lc ++ rc)
    | _, _ => none
  | _ => none

/-- (C3d) The items of a `FROM` list, separated by commas, the sources
counted from position `i`. -/
def readItems (d : Dialect) :
    Nat → List Ast.From → Option (List (CatTable × String) × List (List Nat × Ast.Expr))
  | _, [] => some ([], [])
  | i, f :: fs => do
    let (ss, cs) ← readJoin d i f
    let (ss', cs') ← readItems d (i + ss.length) fs
    pure (ss ++ ss', cs ++ cs')

/-- (C3d) The names of a block's sources: each source's reference name
and columns (`Core.srcCol`), the kinds of all columns, and the sources an
expression may name. -/
def blockScope (d : Dialect) (srcs : List (CatTable × String)) (vis : List Nat) : Scope :=
  { srcs := srcs.zipIdx.map fun ((t, r), i) =>
      { ref := r, cols := t.cols.map fun c => (colKey d c, Core.srcCol i c.name)
        opaqueNames := t.opaqueCols.map fun c => match d with
          | .tsql _ _ => c.tsql
          | .pg _ _ _ => c.name }
    colTys := (srcs.zipIdx.map fun ((t, _), i) =>
      t.cols.map fun c => (Core.srcCol i c.name, c.kind)).flatten
    vis := vis }

/-- (C3d) The reference names of a block are ASCII and distinct: T-SQL up
to case (1013), PostgreSQL after folding (42712; "Reference names"). -/
def refsOk (d : Dialect) (refs : List String) : Bool :=
  refs.all (fun x => x.toList.all fun (c : Char) => c.toNat < 128) && match d with
    | .tsql _ _ => distinct (refs.map (·.toLower))
    | .pg _ _ _ => distinct refs

/-- The `FROM` list as a query block ("The C3d blocks"): its tables, the
names the select list and the `WHERE` see (every source), and the `ON`
conditions with the names each sees. -/
def readFrom (d : Dialect) (froms : List Ast.From) :
    Option (List CatTable × Scope × List (Scope × Ast.Expr)) := do
  if froms.isEmpty then none else
  let (srcs, ons) ← readItems d 0 froms
  if !refsOk d (srcs.map (·.2)) then none else
  let sc := blockScope d srcs (List.range srcs.length)
  pure (srcs.map (·.1), sc, ons.map fun (vis, c) => ({ sc with vis := vis }, c))

/-- The one reader of a query (see `QSpec`). -/
def querySpec (d : Dialect) (q : Ast.Select) : Option QSpec := do
  let (items, into, strict, froms, wh) ← selParts q
  let (tabs, sc, ons) ← readFrom d froms
  pure { items, into, strict, tabs, sc, conds := ons ++ wh.toList.map (sc, ·) }

/-- `COUNT(*)`. -/
def countStar : Ast.Expr → Bool
  | .CallE (.CallF [n] [] false [] false false true) => n.toUpper == "COUNT"
  | _ => false

/-- The select list of an `EXISTS`: `1` or `*`. -/
def existsItems : List Ast.SelItem → Bool
  | [.SelItem (.Num "1") none none] | [.SelItem (.Star none) none none] => true
  | _ => false

/-- A value a query assigns from the row it picks, with its type: an
`int` expression of the slice, (C3c) a string expression of the slice,
or NULL, reading no variable, that cannot raise. -/
def pickValue (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) :
    Option (Core.Expr × STy) := do
  let (s, t) ← typed d env sc e
  let e' : Core.Expr := .sc s
  if isVal t && e'.vars.isEmpty && !e'.raise then some (e', t) else none

/-- (C3b, C3c) A variable a pick may assign a value of type `t` to: an
`int` variable for an integer, a string variable for a string (whether
the string fits is a store, held by `storesRoutine`: "String stores, on
every path"), either for NULL. -/
def pickTarget (d : Dialect) (env : TyEnv) (x : Var) : STy → Bool
  | .int w => match intVarW d env x with
    | some t => w.le t
    | none => false
  | .str .. => (strVar d env x).isSome
  | .nul => (varTy d env x).isSome
  | .bool => false
  | .bit => varTy d env x == some .bit
  | .date => varTy d env x == some .date
  | .ts p => match varTy d env x with
    | some (.ts q) => p ≤ q
    | _ => false

/-- The conjuncts of a condition. -/
def conjuncts : SExpr → List SExpr
  | .and a b => conjuncts a ++ conjuncts b
  | s => [s]

/-- A lowered condition split at its top-level `AND`s (a slice `AND` of
two conditions that cannot raise keeps a tuple exactly when both keep it:
`Core.and3`); a condition on the C1 path stays whole. -/
def splitConj : Core.Expr → List Core.Expr
  | .sc s => (conjuncts s).map .sc
  | e => [e]

/-- The lowered query block: its tables and its lowered conditions, each
split at its `AND`s (`Core.Block`). -/
def lowerBlock (d : Dialect) (env : TyEnv) (s : QSpec) : Option Core.Block := do
  let cs ← s.conds.mapM fun (sc, c) => lowerExpr d env sc c
  pure { srcs := s.tabs.map CatTable.toDef, conds := cs.flatMap splitConj }

/-- The variables a block's conditions read. -/
def blockVars (b : Core.Block) : List Var := (b.conds.map Core.Expr.vars).flatten

/-- `.row`: distinct targets of the values' types (`pickTarget`) that the
conditions do not read. Without `orNull` (`SELECT @x = ...`, the `FOR`
block), a block that keeps at most one row on every database the engines
can be in (`Core.Block.atMostOne`: the key closure; its meaning is
`Sqldiff.Core.kept_length_le_one`, `Sqldiff/Core/Fidelity.lean`); with
`orNull` (a scalar subquery), any block: more than one row is the core's
`cardinality` error (C3d, "The C3d blocks"). -/
def rowPick (d : Dialect) (env : TyEnv) (b : Core.Block) (orNull : Bool)
    (sets : List (Var × Core.Expr × STy)) : Option Stmt :=
  let xs := sets.map (·.1)
  if !sets.isEmpty && distinct xs && sets.all (fun (x, _, ty) => pickTarget d env x ty) &&
      !(xs.any (blockVars b).contains) && (orNull || b.atMostOne) then
    some (.query ⟨b⟩ (.row orNull (sets.map fun (x, e, _) => (x, e))))
  else none

/-- `.count`: an `int` target that the condition does not read. (C2c) Not
a `bigint` target: T-SQL `COUNT(*)` is `int` and raises 8115 beyond
2^31-1 rows whatever the target, PostgreSQL's `count` is `bigint` and
fits a `bigint` target (eval/values V2: the types), so the two differ
there, which the core's `count` (an overflow on both) does not show. -/
def countPick (d : Dialect) (env : TyEnv) (b : Core.Block) (x : Var) : Option Stmt :=
  if intVarW d env x == some .i32 && !(blockVars b).contains x then
    some (.query ⟨b⟩ (.count x))
  else none

/-- The T-SQL picks `@x = e` of a select list. -/
def assignItems (d : Dialect) (env : TyEnv) (sc : Scope) (items : List Ast.SelItem) :
    Option (List (Var × Core.Expr × STy)) :=
  items.mapM fun
    | .SelItem e none (some v) => do pure (← target d (.Var v), ← pickValue d env sc e)
    | _ => none

/-- T-SQL `SELECT @x = e, ... FROM t WHERE c` and `SELECT @x = COUNT(*) FROM
t [WHERE c]` (rules 1 and 4). -/
def lowerSelectT (d : Dialect) (env : TyEnv) (sel : Ast.Select) : Option Stmt := do
  let s ← querySpec d sel
  if !s.into.isEmpty || s.strict then none else
  let b ← lowerBlock d env s
  let row : Option Stmt := do rowPick d env b false (← assignItems d env s.sc s.items)
  match s.items with
  | [.SelItem e none (some v)] =>
    if countStar e then do countPick d env b (← target d (.Var v)) else row
  | _ => row

/-- PL/pgSQL `SELECT COUNT(*) INTO x FROM t [WHERE c]` (rule 4). -/
def lowerSelectP (d : Dialect) (env : TyEnv) (sel : Ast.Select) : Option Stmt := do
  let s ← querySpec d sel
  match s.items, s.into, s.strict with
  | [.SelItem e none none], [x], false =>
    if countStar e then do
      let x ← target d x
      let b ← lowerBlock d env s
      countPick d env b x
    else none
  | _, _, _ => none

/-- `x := (SELECT e FROM t WHERE c)` in either dialect (rules 2 and 4). -/
def lowerScalar (d : Dialect) (env : TyEnv) (x : Var) (sel : Ast.Select) : Option Stmt := do
  let s ← querySpec d sel
  if !s.into.isEmpty || s.strict then none else
  let b ← lowerBlock d env s
  match s.items with
  | [.SelItem e none none] =>
    if countStar e then countPick d env b x
    else rowPick d env b true [(x, ← pickValue d env s.sc e)]
  | _ => none

/-- The condition of an `IF` that is `EXISTS (q)` (`false`) or `NOT EXISTS
(q)` (`true`). -/
def existsCond : Ast.Expr → Option (Ast.Select × Bool)
  | .Exists q => some (q, false)
  | .Unary o (.Exists q) => if o.toUpper == "NOT" then some (q, true) else none
  | _ => none

/-- `EXISTS (SELECT 1 FROM t [WHERE c])` (rule 3). -/
def lowerExists (d : Dialect) (env : TyEnv) (sel : Ast.Select) : Option Stmt := do
  let s ← querySpec d sel
  if !s.into.isEmpty || s.strict || !existsItems s.items then none else
  let b ← lowerBlock d env s
  pure (.query ⟨b⟩ (.any existsVar))

/-- (C3b) The dialect a `FOR` loop's query is read in: its record is a
variable there (`lowerForBlock`). -/
def withRecord (d : Dialect) (r : String) : Dialect :=
  match d with
  | .pg Γ vars ns => .pg Γ (vars ++ [pgName r]) ns
  | d => d

/-- A `record` declaration's type. -/
def recordType : Ast.Ty → Bool
  | .Ty n [] => n.toUpper == "RECORD"
  | _ => false

/-- `x := r.a` in the body of the converter's `FOR` loop over `r`: the
target and the field, folded. -/
def fieldAssign (r : String) : Ast.Node → Option (Var × String)
  | .NStmt (.Assign (.Name [x]) (.Name [r', a])) =>
    if pgName r' == r then some (pgName x, pgName a) else none
  | _ => none

/-- The aliased items `e AS a` of a `FOR` loop's query, folded. -/
def aliasItems (d : Dialect) (env : TyEnv) (sc : Scope) (items : List Ast.SelItem) :
    Option (List (String × Core.Expr × STy)) :=
  items.mapM fun
    | .SelItem e (some a) none => do pure (pgName a, ← pickValue d env sc e)
    | _ => none

/-- The PL/pgSQL block `DECLARE r record; BEGIN FOR r IN SELECT e1 AS a1,
... FROM t WHERE c LOOP x1 := r.a1; ... END LOOP; END;` (rule 1). -/
def lowerForBlock (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) (r : String) (ty : Ast.Ty)
    (r' : String) (sel : Ast.Select) (body : List Ast.Node) : Option (List Stmt) :=
  match d with
  | .tsql _ _ => none
  | .pg Γ vars ns => do
    let r := pgName r
    if !(k matches .Procedure) || !recordType ty || pgName r' != r || vars.contains r then none
    else
    -- The record is a variable for the query's names.
    let d' := Dialect.pg Γ (vars ++ [r]) ns
    let s ← querySpec d' sel
    if !s.into.isEmpty || s.strict then none else
    let b ← lowerBlock d' env s
    let cols ← aliasItems d' env s.sc s.items
    let asg ← body.mapM (fieldAssign r)
    if asg.length != cols.length || !distinct (cols.map (·.1)) ||
        !(asg.zip cols).all (fun (p, q) => p.2 == q.1) || (asg.map (·.1)).contains r then none
    else
    let st ← rowPick d' env b false ((asg.zip cols).map fun (p, q) => (p.1, q.2))
    pure [st]

/-! ## Statements -/

/-- One statement, as a list of core statements (a block contributes its
statements in place, so the result does not depend on how `BEGIN ... END`
nests).

* Assignment (T-SQL `SET @x = e`, PL/pgSQL `x := e`) of an `int`
  expression or NULL to an `int` variable stores the value as it is: no
  conversion between equal types (Microsoft Learn, "SET @local_variable
  (Transact-SQL)"; PostgreSQL manual, PL/pgSQL "Basic Statements",
  "Assignment": the value is converted only "if the expression's result
  data type doesn't match the variable's data type"). Not probed. If `e` fails, the variable keeps its value and the error
  ends the routine (see the file header for T-SQL).
* T-SQL `DECLARE @x t` does nothing at run time: every variable is NULL
  from the start of the routine, and a `DECLARE` inside a loop does not
  reset it. `DECLARE @x t = e` assigns, where it stands, each time it
  runs. So the PL/pgSQL mirror declares `x` at the top without a default
  and assigns `x := e` at the same position.
* PL/pgSQL declarations belong to the `DECLARE` section (`lowerP`).
* `RETURN e` in a function returns `e`; in a T-SQL procedure it sets the
  status (above); a PostgreSQL procedure cannot return a value.
* `INSERT ... VALUES`, `UPDATE` and `DELETE` as in "The C3a slice: data
  statements" above; every other form of them is outside the spec.
* (C3b) The queries of "The C3b slice: queries": T-SQL `SELECT @x = ...`,
  PL/pgSQL `SELECT COUNT(*) INTO x`, and an assignment whose whole value
  is a scalar subquery, in procedures only.
* Other queries, `MERGE` and messages are outside the spec. -/
def lowerStmt (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) : Ast.Stmt → Option (List Stmt)
  | .Assign t (.Subquery q) => do
    if !(k matches .Procedure) then none else
    let x ← target d t
    let s ← lowerScalar d env x q
    pure [s]
  | .SSelect q => do
    if !(k matches .Procedure) then none else
    let s ← match d with
      | .tsql _ _ => lowerSelectT d env q
      | .pg _ _ _ => lowerSelectP d env q
    pure [s]
  | .Assign t v => do
    let x ← target d t
    let v' ← lowerValue d env {} (wantOf d env x) v
    pure [.assign x v']
  | .Return none => some [.ret none]
  | .Return (some e) => do
    let e' ← lowerExpr d env {} e
    match d, k with
    | _, .Function => pure [.ret (some e')]
    | .tsql _ _, .Procedure => pure [.assign statusVar (statusExpr d env e e'), .ret none]
    | .pg _ _ _, .Procedure => none
  | .Declare x ty dflt => match d, dflt with
    | .tsql _ _, none => some []
    | .tsql _ _, some e => do
      let e' ← lowerValue d env {} (ty.bind (valTypeOf d)) e
      pure [.assign x e']
    | .pg _ _ _, _ => none
  | .Insert tgt cols (.SrcValues rows) [] [] none => do
    if !dataOk d k then none else
    let t ← writeTableOf d tgt
    let ccs ← cols.mapM (colOf d t)
    let cs := ccs.map (·.name)
    if cs.isEmpty || !distinct cs || rows.isEmpty || !rows.all (·.length == cs.length) then none
    else
      let rows' ← rows.mapM fun r => (ccs.zip r).mapM fun (c, e) => colValue d env {} c e
      -- C3c: the left-out columns with a default get it.
      let dfl := defaultsFor t cs
      pure [.insert t.toDef (cs ++ dfl.map (·.1)) (rows'.map (· ++ dfl.map (·.2)))]
  | .Update tgt none sets [] wh => do
    if !dataOk d k then none else
    let t ← writeTableOf d tgt
    let sc := scopeOf d t tgt
    let sets' ← sets.mapM fun
      | .Set [c] e => do
        let c' ← colOf d t c
        if t.isKey c'.name then none else
          let e' ← colValue d env sc c' e
          pure (c'.name, e')
      | _ => none
    if sets'.isEmpty || !distinct (sets'.map (·.1)) then none
    else
      let wh' ← lowerOpt d env sc wh
      pure [.update t.toDef sets' wh']
  | .Delete tgt none [] wh => do
    if !dataOk d k then none else
    let t ← writeTableOf d tgt
    let wh' ← lowerOpt d env (scopeOf d t tgt) wh
    pure [.delete t.toDef wh']
  | .Insert .. | .Update .. | .Delete .. | .Merge .. | .Message .. => none

/-- (C3d part 2) The table a data statement writes, as it names it. -/
def writtenTable : Ast.Stmt → Option (List String)
  | .Insert tgt .. | .Update tgt .. | .Delete tgt .. | .Merge tgt .. => some tgt
  | _ => none

/-- What `writeTableOf` gives is the catalog's table, and not read-only. -/
theorem writeTableOf_spec {d : Dialect} {target : List String} {t : CatTable}
    (h : writeTableOf d target = some t) : tableOf d target = some t ∧ t.readOnly = false := by
  unfold writeTableOf at h
  cases ho : tableOf d target with
  | none => simp [ho] at h
  | some u =>
    cases hr : u.readOnly <;> simp_all

/-- (C3d part 2) **No lowered statement writes a read-only table.** A data
statement (`INSERT`, `UPDATE`, `DELETE`, `MERGE`) that `lowerStmt` lowers
names a table of the catalog (`tableOf`) that is not read-only. Data
statements reach the core only through `lowerStmt` (the `NStmt` case of
`lowerNode`), so no routine the check lowers writes a read-only table.
The slice check (`sliceStmt`) and the diagnostics (`Diagnose.lean`) find
the table through `writeTableOf` too, by construction. -/
theorem lowerStmt_writes_not_readOnly {d : Dialect} {env : TyEnv} {k : Ast.RoutineKind}
    {s : Ast.Stmt} {ss : List Stmt} {tgt : List String}
    (h : lowerStmt d env k s = some ss) (hw : writtenTable s = some tgt) :
    ∃ t, tableOf d tgt = some t ∧ t.readOnly = false := by
  cases hwt : writeTableOf d tgt with
  | some t => exact ⟨t, writeTableOf_spec hwt⟩
  | none =>
    exfalso
    cases s with
    | Insert tgt' cols src ret ri oc =>
      simp only [writtenTable, Option.some.injEq] at hw; subst hw
      cases src <;> cases ret <;> cases ri <;> cases oc <;> simp [lowerStmt, hwt] at h
    | Update tgt' a sets fr wh =>
      simp only [writtenTable, Option.some.injEq] at hw; subst hw
      cases a <;> cases fr <;> simp [lowerStmt, hwt] at h
    | Delete tgt' a fr wh =>
      simp only [writtenTable, Option.some.injEq] at hw; subst hw
      cases a <;> cases fr <;> simp [lowerStmt, hwt] at h
    | Merge => simp [lowerStmt] at h
    | _ => simp [writtenTable] at hw

/-! ## Calls -/

/-- T-SQL `EXEC p a, @x OUTPUT`: positional arguments only; an `OUTPUT`
argument is a variable that receives the parameter's final value; without
`OUTPUT` nothing is copied back, even to an `OUTPUT` parameter. The callee
is renamed through the deployment. `EXEC @rc = p` is outside the spec: the
status is the callee's *last* parameter in the core (above), so placing
`@rc` needs the callee's arity, which the check does not take as input. -/
def lowerTsqlArgShape (d : Dialect) (env : TyEnv) : Ast.Arg → Option Arg
  | .Arg (some _) _ _ => none -- named arguments: the core binds by position
  | .Arg none e true => (target d e).map .ref
  | .Arg none e false => (lowerExpr d env {} e).map .val

/-! ### Argument types (C2c)

The core binds an argument to a parameter as it is, and neither engine
does: T-SQL converts an argument to the parameter's type (an `int` into a
`smallint` parameter is narrowed, raising 220 when it does not fit; a
`bit` parameter takes any number as 0 or 1), and PostgreSQL resolves a
`CALL` by the arguments' types, with implicit casts only: an `integer`
argument finds no procedure with a `smallint` or `boolean` parameter
(42883, "procedure cs(integer) does not exist"), and a variable in an
`INOUT` position that needs a cast is "not writable" (probed on
PostgreSQL 18 with the converter's output, review of C2c). Before C2c the
callee's types were not looked at, which was sound only while every
callee confirmed was an `int` routine; with `smallint`, `bigint`, `bit`
and dates in signatures (`tyCorr`) it is not. So every argument must be
shown to have exactly its parameter's type (`argKindOk`): the request
names each callee's parameter types (`Catalog.routineTys`, PostgreSQL's
DDL; the callee's own confirmation ties them to its T-SQL types by
`sigCorr`, `ASM-CALLEE-CONFIRMED`), and a value argument must be a slice
value of that type (or NULL; the T-SQL literals a `bit`, `date` or
`datetime2` parameter reads, as for a store: `coerce`), an `OUTPUT` or
`INOUT` variable must be declared with that type. A string parameter
takes a string (any lengths, as `sigCorr`); a parameter of a type
outside the slice takes no argument in the spec. -/

/-- The slice type of a callee's parameter, from its PostgreSQL type. -/
def paramWant (ty : Ast.Ty) : Option STy := valTypeOf (.pg ∅ [] []) ty

/-- A value of type `t` is exactly of the parameter's type `want`: the
same slice type, or the NULL literal; for a string, a string that fits.
T-SQL truncates a string argument longer than the callee's parameter on
entry (and a `varchar` parameter replaces characters outside its code
page), PostgreSQL keeps it (it drops a parameter's length): callee
`dbo.q @s varchar(3)` given a `varchar(10)` variable holding `'abcdef'`
sees `abc` in SQL Server, `abcdef` in PostgreSQL (re-review of C2c). So
in T-SQL a string argument must have a bound at most the parameter's
length and come from no `nvarchar` (the PostgreSQL DDL does not say
whether the T-SQL parameter is `varchar` or `nvarchar`; `tyCorr` ties its
length to the T-SQL one); an unbounded parameter, or an argument whose
bound is unknown, is outside the spec. PostgreSQL keeps any string. -/
def sameKind (d : Dialect) (want t : STy) : Bool :=
  match want, t with
  | _, .nul => true
  | .str n _, .str m w => match d with
    | .tsql _ _ => n.isSome && fitsLen n m && !w
    | .pg _ _ _ => true
  | w, t => w == t

/-- A value argument of the parameter's type. -/
def argValueOk (d : Dialect) (env : TyEnv) (want : STy) (e : Ast.Expr) : Bool :=
  match typedTo d env {} (some want) e with
  | some (_, t) => sameKind d want t
  | none => false

/-- An `OUTPUT`/`INOUT` variable declared with the parameter's type (its
value is passed in: for a string, its declared length must fit). -/
def argRefOk (d : Dialect) (env : TyEnv) (want : STy) (x : Var) : Bool :=
  match varTy d env x with
  | some t => t != .nul && sameKind d want t
  | none => false

/-- The argument in a position whose parameter is declared `ty` has its
type (`argValueOk`, `argRefOk`). -/
def argKindOk (d : Dialect) (env : TyEnv) (ty : Ast.Ty) (a : Ast.Arg) (ref : Bool) : Bool :=
  match paramWant ty, a with
  | none, _ => false
  | some w, .Arg _ e _ =>
    if ref then (target d e).any (argRefOk d env w) else argValueOk d env w e

/-- A value argument, lowered with the parameter's type (a `bit` literal
`1` is the boolean, as `TRUE` is). -/
def lowerArgValue (d : Dialect) (env : TyEnv) (ty : Ast.Ty) (e : Ast.Expr) : Option Core.Expr :=
  lowerValue d env {} (paramWant ty) e

def lowerTsqlArg (d : Dialect) (env : TyEnv) (ty : Ast.Ty) (a : Ast.Arg) : Option Arg :=
  match a with
  | .Arg (some _) _ _ => none
  | .Arg none e out =>
    if !argKindOk d env ty a out then none
    else if out then (target d e).map .ref
    else (lowerArgValue d env ty e).map .val

/-- PostgreSQL `CALL p(args)`: positional arguments only; an argument in
an `OUT` or `INOUT` position (the catalog says which) must be a variable,
and receives the parameter's final value; trailing parameters without an
argument take their defaults. -/
def lowerPgArgShape (d : Dialect) (env : TyEnv) : Ast.Mode × Ast.Arg → Option Arg
  | (_, .Arg (some _) _ _) => none
  | (_, .Arg none _ true) => none -- `OUTPUT` is T-SQL syntax
  | (.In, .Arg none e false) => (lowerExpr d env {} e).map .val
  | (_, .Arg none e false) => (target d e).map .ref

def lowerPgArg (d : Dialect) (env : TyEnv) (ty : Ast.Ty) : Ast.Mode × Ast.Arg → Option Arg
  | (m, a) => match lowerPgArgShape d env (m, a) with
    | none => none
    | some (.ref x) => if argKindOk d env ty a true then some (.ref x) else none
    | some (.val _) => match a with
      | .Arg _ e _ =>
        if argKindOk d env ty a false then (lowerArgValue d env ty e).map .val else none

/-- The parameter types of a callee (`Catalog.routineTys`); none when the
request does not give them. -/
def calleeTys (Γ : Catalog) (callee : String) : List Ast.Ty :=
  (lookup Γ.routineTys callee).getD []

def lowerCall (d : Dialect) (env : TyEnv) (n : List String) (args : List Ast.Arg)
    (ret : Option Ast.Expr) : Option Stmt :=
  match d with
  | .tsql dep Γ => do
    if ret.isSome then none else
    let callee ← lookup dep.routines (dotted n)
    let tys := calleeTys Γ callee
    if args.length > tys.length then none else
    let args' ← (tys.zip args).mapM fun (ty, a) => lowerTsqlArg d env ty a
    pure (.call callee args')
  | .pg Γ _ _ => do
    let callee := dotted (n.map pgName)
    let modes ← lookup Γ.routines callee
    let tys := calleeTys Γ callee
    if args.length ≤ modes.length && args.length ≤ tys.length && ret.isNone then
      let args' ← ((modes.zip tys).zip args).mapM fun ((m, ty), a) => lowerPgArg d env ty (m, a)
      pure (.call callee args')
    else none

/-- `SET NOCOUNT ON|OFF` only changes rows-affected counts, of this
routine and of what it calls, so it is nothing when the observer does not
read them and outside the spec when it does.

`SET XACT_ABORT ON` makes a run-time error end the batch ("SET XACT_ABORT
(Transact-SQL)": "When SET XACT_ABORT is ON, if a Transact-SQL statement
raises a run-time error, the entire transaction is terminated and rolled
back"), which is what the core does with every error, so it lowers to
nothing. Where it is the routine's first statement (`abortsT`), every
error of the routine ends it; elsewhere it changes nothing for runs that
raise no error, and a C2a confirmation then needs a routine that cannot
raise. What the caller's transaction sees is "The C4 rules" (`ASM-NO-CALLER-TXN`). `XACT_ABORT OFF`
and other settings (`ARITHABORT`, `ANSI_WARNINGS`, ...) change error
behaviour: outside the spec.

`SET CONCAT_NULL_YIELDS_NULL ON` (C2b) makes `+` on a NULL string give
NULL from there on, which is what the core's `concat` always does, so it
lowers to nothing; at the start of the routine it discharges
`ASM-CONCAT-NULL` (`concatNullOn`). `OFF` is outside the spec. -/
def lowerSetting (d : Dialect) (name : String) (on : Bool) : Option (List Stmt) :=
  match d with
  | .tsql dep _ =>
    if name.toUpper = "NOCOUNT" && !dep.rowCounts then some []
    else if name.toUpper = "XACT_ABORT" && on then some []
    else if name.toUpper = "CONCAT_NULL_YIELDS_NULL" && on then some []
    else none
  | .pg _ _ _ => none

/-! ## The C4 rules: errors and transactions

Phase C4 adds T-SQL `TRY`/`CATCH`, `THROW` and `BEGIN TRAN`/`COMMIT`,
and PL/pgSQL exception blocks, `RAISE` and `COMMIT`, lowered to the
core's `tryCatch`, `raise`, `reraise` and `commit`
(`Sqldiff.Core.Stmt`). Each rule states its claim, the vendor text and
the probe status, as for C2a. "V10", "V17" and "V18" are the vendor facts
of docs/vendor-facts-verification.md, checked against the vendor pages on
2026-10-23/24; **every other vendor quote in this section was written
from memory** (the vendor sites were not reachable when it was written,
2026-10-28) and reviewers check it against the page named. **Probed** on SQL
Server 2022 (16.0.4305.5) and PostgreSQL 18.6, 2026-10-28:
`eval/errors-txn/{mssql,postgres}.sql` with their `.out` files, probes
P1 to P13 named below; every behaviour each rule claims was observed, and
P9d found one the rules did not state (the doomed caller transaction,
under "Transactions, T-SQL").

**Which errors exist.** The core's errors are integer overflow and
division by zero (`arithmetic`), `NOT NULL` and primary key violations
(`constraint`), errors the routine raises (`user`, below) and an
uninterpreted operator's (`other`, C1 path only). Both handlers catch
every one of them: T-SQL "TRY...CATCH (Transact-SQL)": "A TRY...CATCH
construct catches all execution errors that have a severity higher than
10 that do not close the database connection" (from memory); 8115, 8134,
515 and `THROW` have severity 16, 2627 severity 14 (probed, P6: all five
caught, with those severities).
PL/pgSQL: "The special condition name OTHERS matches every error type
except QUERY_CANCELED and ASSERT_FAILURE" (V18; probed, P6: 22012, 22003,
23502, 23505 and P0001 caught, an `ASSERT` failure (P0004) not). What either engine does
not catch (T-SQL: severity 20 and above, attentions, `KILL`, and at the
same level compile, recompilation and deferred-name-resolution errors,
V10; PL/pgSQL: `query_canceled`, `assert_failure`, FATAL and PANIC, V18)
comes from events the core does not model (cancellation, timeouts,
resource limits, a catalog other than the one given): a confirmation of
a routine with a handler rests on `ASM-CATCH-CORE-ERRORS`, that inside a
handled block no error is raised that the core does not model, or that
such an error is caught, or not, alike by both. A T-SQL routine with
`RAISERROR` is outside the spec (below).

**`TRY`/`CATCH` against `EXCEPTION WHEN OTHERS`.**
T-SQL `BEGIN TRY b END TRY BEGIN CATCH h END CATCH` lowers to
`tryCatch false b h`; PL/pgSQL `BEGIN b EXCEPTION WHEN OTHERS THEN h END`
(a nested block without `DECLARE`, one handler, condition `OTHERS`)
lowers to `tryCatch true b h`. Any other condition list (`WHEN
division_by_zero`, `SQLSTATE '22012'`, several handlers) is outside the
spec.

* Control: when `b` raises, the rest of `b` is skipped and `h` runs;
  when `b` finishes, `h` is skipped. T-SQL: "If there is an error in the
  code that is enclosed in a TRY block, control passes to the first
  statement in the associated CATCH block", and after a TRY block without
  error, "control passes to the statement immediately after the
  associated END CATCH statement" (from memory). PL/pgSQL "Trapping
  Errors": "if an error occurs within the statements, further processing
  of the statements is abandoned, and control passes to the EXCEPTION
  list ... the corresponding handler_statements are executed, and then
  control passes to the next statement after END" (V18's page; quoted
  from memory). Probed, P1, both engines.
* An error inside `h` is not caught by the same handler and leaves it:
  T-SQL "TRY...CATCH": errors in a CATCH block go to an enclosing TRY or
  to the caller (from memory); PL/pgSQL: "If a new error occurs within
  the selected handler_statements, it cannot be caught by this EXCEPTION
  clause, but is propagated out" (from memory). Handlers nest in both, as
  `tryCatch` does. Probed, P2, both engines.
* Variables keep the values the error left in both: PL/pgSQL "the local
  variables of the PL/pgSQL function remain as they were when the error
  occurred" (V18); T-SQL variables are not transactional (from memory:
  "Transactions (Transact-SQL)", table variables and variables are not
  affected by a rollback). Probed, P3, both engines.
* **The database differs**: PL/pgSQL "all changes to persistent database
  state within the block are rolled back" (V18), which is `undo`; T-SQL
  rolls back only the failing statement, and keeps what `b` wrote
  before it, committed when no transaction is open. With `XACT_ABORT ON`
  in an open transaction, the error makes the transaction uncommittable
  instead (`XACT_STATE() = -1`: "An error can make the transaction
  uncommittable", V10), and every write of `h` then fails (error 3930).
  Probed: P4 (autocommit, `XACT_ABORT` OFF and ON alike: the row written
  before the error is kept, `XACT_STATE() = 0` and `@@TRANCOUNT = 0` in
  `CATCH`, a write in `CATCH` succeeds; PostgreSQL rolls the block's row
  back and keeps the handler's), P5 (a caller transaction under
  `XACT_ABORT ON`: `XACT_STATE() = -1` in `CATCH`, the write fails with
  3930; with `XACT_ABORT OFF` the transaction stays committable, P5b). So the T-SQL lowering is faithful only when no
  transaction is open when the routine is called and the routine opens
  none around a handled error: `ASM-NO-CALLER-TXN` (the caller holds no
  open transaction, and `IMPLICIT_TRANSACTIONS` is `OFF` so that a
  statement does not open one), named whenever an error can reach a
  handler; and `BEGIN TRAN` is outside the spec inside a `TRY` (below).
  The two lowerings then differ only by `undo`, and the check settles it
  (`Sqldiff.Core.Stmt.settle`, theorem `Sqldiff.Core.exec_settle`): a
  T-SQL `TRY` whose body cannot fail after it may have written
  (`Sqldiff.Core.Stmt.undoSafe`) means the same with `undo`, and is
  confirmed against an `EXCEPTION` block; one whose body can, is refused.
* A call inside a handled body (at any depth) is outside the spec: a
  callee's own errors are not in the core (`WorldModel.call` does not
  fail), and its writes before its error are kept by T-SQL and rolled
  back by PL/pgSQL.

**`THROW` and `RAISE EXCEPTION`** raise an error of the kind `user`.

* T-SQL `THROW n, m, s` with `n` an integer literal from 50000 to
  2147483647, `m` a string literal and `s` an integer literal from 0 to
  255: "THROW (Transact-SQL)": error_number "must be greater than or
  equal to 50000 and less than or equal to 2147483647", state is
  `tinyint`, "The severity is always set to 16", and "If a TRY...CATCH
  construct is not available, the statement batch is terminated" (from
  memory); so it ends the routine whatever `XACT_ABORT` says, which the
  core does with every error. Probed: P6 (severity 16, the number and
  state given), P7 (ends the procedure with `XACT_ABORT OFF`; a statement
  error there does not, P7d), P7c (49999 is refused with error 35100). Variables as arguments are outside the
  spec (a NULL or out-of-range value fails differently).
* PL/pgSQL `RAISE [EXCEPTION] 'f'` with `f` without `%`, or `RAISE
  [EXCEPTION] USING MESSAGE = 'm'`, optionally with `ERRCODE = 'P0001'`:
  "Errors and Messages": the level `EXCEPTION` is the default and "raises
  an error (which normally aborts the current transaction)"; "If no
  condition name nor SQLSTATE is specified in a RAISE EXCEPTION command,
  the default is to use raise_exception (P0001)" (from memory; probed, P6: `RAISE EXCEPTION
  'm'`, `RAISE 'm'` and `USING MESSAGE = 'm', ERRCODE = 'P0001'` all raise
  P0001 with message `m`). Other
  levels (`NOTICE`, ...) send a message, not an error: outside the spec.
  A `%` in the format, arguments, or any other option (`DETAIL`,
  `HINT`, another `ERRCODE`) are outside it too. The code must be spelt
  `'P0001'` exactly: PostgreSQL reads an `ERRCODE` of five digits and
  upper-case letters as an SQLSTATE and anything else as a condition
  name, so `'p0001'` is an unknown condition and the `RAISE` fails with
  42704 ("unrecognized exception condition"; probed on PostgreSQL 18.6,
  2026-10-28: `RAISE EXCEPTION USING MESSAGE = 'm', ERRCODE = 'p0001'`
  fails with `unrecognized exception condition "p0001"`).
* The caller sees error number `n` from T-SQL and SQLSTATE P0001 from
  PostgreSQL, with the same message: decision 1 compares the kind, not
  the number or the text, so a confirmation that raises rests on
  `ASM-THROW-ERRCODE` (callers do not tell user errors apart by number).
* `RAISERROR` is outside the spec: its severity decides whether it is an
  error at all (0 to 10 are messages), and it does not end the routine
  under `XACT_ABORT ON` ("THROW honours XACT_ABORT; RAISERROR does not",
  V10). The converter refuses it too.

**Re-raising.** T-SQL `THROW;` and PL/pgSQL `RAISE;` re-raise the error
the handler handles, with its kind: "THROW (Transact-SQL)": "If the
THROW statement is specified without parameters, it must appear inside a
CATCH block. This causes the caught exception to be raised" (from
memory); PL/pgSQL: the form without parameters "can only be used inside
a BEGIN block's EXCEPTION clause; it causes the error currently being
handled to be re-thrown" (from memory). They lower to `reraise` only
directly in a handler, at any depth of `IF`/`WHILE` but not inside a
`TRY` or block nested in the handler (which error it means there is not
written down for both: outside the spec). Probed, P8: the caller sees
the original error (8134 severity 16; SQLSTATE 22012).

**Error functions.** `ERROR_NUMBER()`, `ERROR_MESSAGE()`,
`ERROR_SEVERITY()`, `ERROR_STATE()`, `ERROR_LINE()`,
`ERROR_PROCEDURE()`, `XACT_STATE()` and `@@ERROR` (T-SQL), and `SQLSTATE`
and `SQLERRM` (PL/pgSQL) are outside the spec wherever they stand: the
converter maps `ERROR_NUMBER()` to `SQLSTATE`, a number against a
five-character code, and no mapping between them is exact.

**Transactions, T-SQL.** `BEGIN TRAN` and `COMMIT` (without a name) only
in a T-SQL routine that starts with `SET XACT_ABORT ON` (`abortsT`), and
only as a bracket (`txnBracket`): in the routine's own statement list
(or in the body of the `TRY` of the idiom below), each `BEGIN TRAN`
followed in the same list by a `COMMIT`, not nested, and between them no
`RETURN`, call, `TRY` or other transaction statement at any depth. Under
`ASM-NO-CALLER-TXN` the transaction is then the routine's own, from
`BEGIN TRAN` (`@@TRANCOUNT` 0 to 1, "BEGIN TRANSACTION (Transact-SQL)",
from memory) to `COMMIT` (1 to 0, which "makes all data modifications
... a permanent part of the database", "COMMIT TRANSACTION
(Transact-SQL)", from memory); an error between them under `XACT_ABORT
ON` rolls it back and ends the routine ("SET XACT_ABORT": "the entire
transaction is terminated and rolled back", V10), which the core shows as
the failed call it is, the database after it not observed
(`ASM-STATE-AFTER-ERROR` as before). Probed: P9c, a caller without
`TRY`: the batch ends and the transaction is rolled back (the statements
before `BEGIN TRAN` stay). **But** P9 and P9d: a caller that catches the
error gets control back with the routine's transaction still open and
doomed (`@@TRANCOUNT = 1`, `XACT_STATE() = -1`, the routine's rows still
visible to it, its own writes failing with 3930 until it rolls back; the
batch rolls it back at its end, error 3998). PostgreSQL leaves no
transaction open after a failed `CALL` (P9). A failed observation does
not show the caller's transaction state, so the Rust side names this for
every routine with a `BEGIN TRAN` outside a `TRY`
(`ASM-TSQL-DOOMED-TXN`, crates/sqldiff-convert/src/cert.rs). A finished call shows the database
with everything committed, as without the transaction, so `BEGIN TRAN`
and `COMMIT` lower to nothing (a confirmation names `ASM-NO-CALLER-TXN`
from the transaction statements themselves, `tsqlTxn`). Outside the spec: a `COMMIT` without its `BEGIN TRAN` (error
3902), a `BEGIN TRAN` without its `COMMIT` (error 266 at the end of the
procedure; probed, P10, with `@@TRANCOUNT = 1` left to the caller), nesting (`@@TRANCOUNT` 2), a `RETURN` between
them, `ROLLBACK` except in the idiom below, `SAVE TRAN`, named
transactions, and any of them inside `TRY`, `CATCH`, `IF`, `WHILE` or a
nested `BEGIN ... END` (the idiom below included: it stands only in the
routine's own statement list or in another idiom's body, `Ctx.txn`).

**The rollback-and-rethrow idiom.** T-SQL `BEGIN TRY b END TRY BEGIN
CATCH IF @@TRANCOUNT > 0 ROLLBACK; THROW; END CATCH` (or `ROLLBACK;
THROW;` when `b` is exactly one bracket: it begins with `BEGIN TRAN`,
ends with `COMMIT`, and has no other transaction statement, so that a
transaction is open whenever `b` fails), not inside any other `TRY` or
`CATCH`, and not inside an `IF`, a `WHILE` or a nested block, lowers to
`b` itself, at transaction level (its `BEGIN TRAN` and `COMMIT` as
above). With two brackets in `b`, an error between them (after the first
`COMMIT`, before the second `BEGIN TRAN`) finds no transaction open: the
unguarded `ROLLBACK` then raises 3903 ("The ROLLBACK TRANSACTION request
has no corresponding BEGIN TRANSACTION") and `THROW;` never runs, so the
caller sees 3903, not the body's error (probed on SQL Server during
review, 2026-10-28: `BEGIN TRAN; INSERT; COMMIT; SET @x = 10/@x; BEGIN TRAN;
INSERT; COMMIT` with `@x = 0`); that form is outside the spec. When `b` fails, the handler rolls back the open
transaction ("ROLLBACK TRANSACTION (Transact-SQL)": back to the
outermost `BEGIN TRAN`, `@@TRANCOUNT` to 0, from memory) and re-raises
the same error, so the routine fails with that error's kind, which is
what the core's `b` does; the rollback changes only the database after a
failed call, which a failed observation does not carry. When `b` does
not fail, the handler does not run. The converter writes `b` without the
handler (rule `idiom.rollback-rethrow`). Probed, P11: the caller sees
2627 with `@@TRANCOUNT = 0` and no row of `b` left, so no doomed
transaction reaches it.

**Transactions, PL/pgSQL.** `COMMIT` lowers to `commit`, only in a
procedure that runs with its caller's privileges (not `SECURITY
DEFINER`), outside every exception block and handler: "A transaction
cannot be ended inside a block with exception handlers"; "A SECURITY
DEFINER procedure cannot execute transaction control statements";
"If CALL is executed in a transaction block, then the called procedure
cannot execute transaction control statements" (V17). The last is
`ASM-NO-CALLER-TXN` again: the procedure is called by a top-level `CALL`
outside a transaction block, from a chain of `CALL`s and `DO`s only.
Then `COMMIT` cannot fail and changes nothing a finished call shows, so
it lowers to nothing, wherever the converter puts it: after a T-SQL
transaction's last statement, or after each statement T-SQL commits on
its own (autocommit, rule `tsql.txn.autocommit`). What a `COMMIT` keeps
when a later statement fails is the database after a failed call, which
a failed observation does not carry (`ASM-STATE-AFTER-ERROR`). A
confirmation names `ASM-NO-CALLER-TXN` from the `COMMIT`s themselves
(`pgTxn`).
`ROLLBACK`, `COMMIT AND CHAIN` and the rest are outside the spec.
Probed: P9 (an error ends a top-level `CALL` and rolls back all its
writes), P9b (a `COMMIT` keeps what came before it), P13 (`COMMIT` in a
block with a handler: "cannot commit while a subtransaction is active";
in a `SECURITY DEFINER` procedure and in a `CALL` inside a transaction
block: "invalid transaction termination").

**Only in procedures.** A T-SQL function cannot contain `TRY`/`CATCH`,
`THROW` or transaction statements ("CREATE FUNCTION (Transact-SQL)": the
statements valid in a function include "Control-of-flow statements
except TRY...CATCH statements", from memory), so in a T-SQL function
they are outside the spec. A PL/pgSQL function may raise and handle
errors, but its T-SQL counterpart is a function too (the check's header
test), which has neither, so the lowered routines differ; PL/pgSQL
`COMMIT` is refused in a function by V17 (`Ctx.commit`). -/

/-- Where a node stands. -/
structure Ctx where
  /-- Inside the body of a `TRY` or an exception block, at any depth
  (handlers of blocks nested in it included): an error there is handled. -/
  handled : Bool := false
  /-- Directly in a handler (not inside a block or `TRY` nested in it):
  `THROW;` and `RAISE;` re-raise the error it handles. -/
  inHandler : Bool := false
  /-- Inside any `TRY`, `CATCH`, or exception block or handler. -/
  nested : Bool := false
  /-- (T-SQL) In a list whose `BEGIN TRAN` and `COMMIT` form a bracket
  (`txnBracket`). -/
  txn : Bool := false
  /-- (PL/pgSQL) `COMMIT` may stand here: a procedure with the caller's
  privileges, outside every exception block. -/
  commit : Bool := false

/-- Inside an `IF`, a `WHILE` or a block: no transaction level. -/
def Ctx.inner (cx : Ctx) : Ctx := { cx with txn := false }

/-- The body of a `TRY` or an exception block. -/
def Ctx.body (cx : Ctx) : Ctx :=
  { cx with handled := true, inHandler := false, nested := true, txn := false, commit := false }

/-- A handler. -/
def Ctx.handler (cx : Ctx) : Ctx :=
  { cx with inHandler := true, nested := true, txn := false, commit := false }

/-- A node in which nothing is a transaction statement, a `RETURN`, a call
or a `TRY`, at any depth: what may stand inside an open T-SQL
transaction. -/
def txnInner : Ast.Node → Bool
  | .NStmt (.Return _) => false
  | .NStmt _ => true
  | .Setting .. => true
  | .If _ t e => txnInnerList t && txnInnerList e
  | .While _ b => txnInnerList b
  | .ForQuery _ _ b => txnInnerList b
  | .NBlock (.Block _ b) => txnInnerList b
  | .Call .. | .Try .. | .Txn .. => false
  | .Throw _ | .Raise .. => true
where
  txnInnerList : List Ast.Node → Bool
    | [] => true
    | n :: ns => txnInner n && txnInnerList ns

def isTxn (op : String) : Ast.Node → Bool
  | .Txn o none => o.toUpper == op
  | _ => false

/-- The `BEGIN TRAN`s and `COMMIT`s of a T-SQL statement list form a
bracket: each `BEGIN TRAN` is closed by a `COMMIT` later in the list,
none is nested, every `COMMIT` closes one, and what stands between them
may stand in an open transaction (`txnInner`). Transaction statements
deeper in the list are refused where they stand (`Ctx.txn`). -/
def txnBracket (ns : List Ast.Node) : Bool :=
  go false ns
where
  go (open_ : Bool) : List Ast.Node → Bool
    | [] => !open_
    | n :: ns =>
      if isTxn "BEGIN" n then !open_ && go true ns
      else if isTxn "COMMIT" n then open_ && go false ns
      else (!open_ || txnInner n) && go open_ ns

/-- `@@TRANCOUNT > 0`. -/
def trancountPos : Ast.Expr → Bool
  | .Bin ">" (.Var v) (.Num "0") => v.toUpper == "@@TRANCOUNT"
  | _ => false

/-- `b` is exactly one bracket: `BEGIN TRAN` first, `COMMIT` last, and no
other transaction statement in the list (deeper ones are refused by
`txnBracket`), so a transaction is open wherever `b` can fail. -/
def oneBracket (b : List Ast.Node) : Bool :=
  (b.head?.map (isTxn "BEGIN")).getD false && (b.getLast?.map (isTxn "COMMIT")).getD false &&
    (b.filter fun | .Txn .. => true | _ => false).length == 2

/-- The handler of the rollback-and-rethrow idiom, for a `TRY` with body
`b`: `IF @@TRANCOUNT > 0 ROLLBACK; THROW;`, or `ROLLBACK; THROW;` when
`b` is exactly one bracket (`oneBracket`). -/
def rollbackRethrow (b h : List Ast.Node) : Bool :=
  match h with
  | [.If c [r] [], .Throw []] => trancountPos c && isTxn "ROLLBACK" r
  | [r, .Throw []] => isTxn "ROLLBACK" r && oneBracket b
  | _ => false

/-- The handlers of a `TRY` with body `b` are the idiom's. -/
def idiomHandler (b : List Ast.Node) : List Ast.Handler → Bool
  | [.Handler [] h] => rollbackRethrow b h
  | _ => false

/-- T-SQL `THROW n, m, s` of the spec. -/
def throwOk : List Ast.Expr → Bool
  | [.Num n, .Str _, .Num s] =>
    (match intLit? n with
      | some v => decide (50000 ≤ v)
      | none => false) &&
    (match intLit? s with
      | some v => decide (0 ≤ v ∧ v ≤ 255)
      | none => false)
  | _ => false

/-- The options of a PL/pgSQL `RAISE EXCEPTION` of the spec: `MESSAGE`
a string literal, `ERRCODE` the literal `'P0001'`, spelt exactly so (an
SQLSTATE in `ERRCODE` is case-sensitive: `'p0001'` names no condition and
PostgreSQL raises 42704 instead), each at most once, nothing else. The
option names are keywords, matched without case. -/
def raiseOptsOk (fmt : Option String) (opts : List Ast.RaiseOpt) : Bool :=
  let names := opts.map fun | .RaiseOpt n _ => n.toUpper
  distinct names && !(fmt.isSome && names.contains "MESSAGE") &&
    opts.all fun
      | .RaiseOpt n (.Str v) =>
        n.toUpper == "MESSAGE" || (n.toUpper == "ERRCODE" && v == "P0001")
      | _ => false

/-- PL/pgSQL `RAISE [EXCEPTION] ...` of the spec (not the bare re-raise). -/
def raiseOk (lvl fmt : Option String) (args : List Ast.Expr) (opts : List Ast.RaiseOpt) : Bool :=
  (lvl.map (·.toUpper == "EXCEPTION")).getD true && args.isEmpty &&
    (fmt.map fun f => !f.toList.contains '%').getD true &&
    (fmt.isSome || !opts.isEmpty) && raiseOptsOk fmt opts

/-- A transaction statement. -/
def lowerTxn (d : Dialect) (cx : Ctx) (op : String) (name : Option String) : Option (List Stmt) :=
  if name.isSome then none
  else match d, op.toUpper with
    | .tsql _ _, "BEGIN" => if cx.txn then some [] else none
    | .tsql _ _, "COMMIT" => if cx.txn then some [] else none
    | .pg _ _ _, "COMMIT" => if cx.commit then some [] else none
    | _, _ => none

/-- `THROW` (T-SQL) and `RAISE` (PL/pgSQL). -/
def lowerThrow (d : Dialect) (cx : Ctx) (args : List Ast.Expr) : Option (List Stmt) :=
  match d, args with
  | .tsql _ _, [] => if cx.inHandler then some [.reraise] else none
  | .tsql _ _, args => if throwOk args then some [.raise .user] else none
  | .pg _ _ _, _ => none

def lowerRaise (d : Dialect) (cx : Ctx) (lvl fmt : Option String) (args : List Ast.Expr)
    (opts : List Ast.RaiseOpt) : Option (List Stmt) :=
  match d, lvl, fmt, args, opts with
  | .pg _ _ _, none, none, [], [] => if cx.inHandler then some [.reraise] else none
  | .pg _ _ _, lvl, fmt, args, opts => if raiseOk lvl fmt args opts then some [.raise .user] else none
  | .tsql _ _, _, _, _, _ => none

mutual
  /-- Some node of a T-SQL statement tree is a transaction statement. -/
  def hasTxnNode : Ast.Node → Bool
    | .Txn .. => true
    | .If _ t e => hasTxnNodes t || hasTxnNodes e
    | .While _ b => hasTxnNodes b
    | .ForQuery _ _ b => hasTxnNodes b
    | .NBlock (.Block _ b) => hasTxnNodes b
    | .Try b hs => hasTxnNodes b || hasTxnHandlers hs
    | _ => false

  def hasTxnNodes : List Ast.Node → Bool
    | [] => false
    | n :: ns => hasTxnNode n || hasTxnNodes ns

  def hasTxnHandlers : List Ast.Handler → Bool
    | [] => false
    | .Handler _ b :: hs => hasTxnNodes b || hasTxnHandlers hs
end

/-- The settings a body starts with, in order. -/
def leadingSettings : List Ast.Node → List (String × Bool)
  | .Setting n on :: ns => (n.toUpper, on) :: leadingSettings ns
  | _ => []

/-! ## Blocks -/

mutual
  def lowerNode (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) (cx : Ctx) :
      Ast.Node → Option (List Stmt)
    | .NStmt s => lowerStmt d env k s
    | .NBlock b => lowerInner d env k cx.inner b
    | .If c t e => do
      let t' ← lowerNodes d env k cx.inner t
      let e' ← lowerNodes d env k cx.inner e
      match existsCond c with
      | some (q, neg) =>
        -- C3b: `IF [NOT] EXISTS (q)`, the test put in `existsVar` first.
        if !(k matches .Procedure) then none else
        let s ← lowerExists d env q
        let v : Core.Expr := .sc (.var existsVar)
        pure [s, .ite (if neg then .sc (.not (.var existsVar)) else v) (seqList t') (seqList e')]
      | none =>
        let c' ← lowerExpr d env {} c
        pure [.ite c' (seqList t') (seqList e')]
    | .While c b => do
      let c' ← lowerExpr d env {} c
      let b' ← lowerNodes d env k cx.inner b
      pure [.while c' (seqList b')]
    -- C3b: a `FOR` loop only in the converter's block (`lowerInner`).
    | .ForQuery .. => none
    | .Call n args ret =>
      -- C4: no call in a handled body.
      if cx.handled then none else (lowerCall d env n args ret).map fun s => [s]
    | .Setting n on => lowerSetting d n on
    | .Try b hs => match d, hs with
      | .tsql _ _, [.Handler [] h] =>
        if !(k matches .Procedure) then none
        else if !cx.nested && cx.txn && rollbackRethrow b h then
          -- The rollback-and-rethrow idiom: the body, at transaction level,
          -- only where a transaction statement may stand (`Ctx.txn`: not
          -- inside `IF`, `WHILE` or a nested block).
          if txnBracket b then lowerNodes d env k { cx with txn := true } b else none
        else do
          let b' ← lowerNodes d env k cx.body b
          let h' ← lowerNodes d env k cx.handler h
          pure [.tryCatch false (seqList b') (seqList h')]
      | .pg _ _ _, [.Handler ["others"] h] => do
        let b' ← lowerNodes d env k cx.body b
        let h' ← lowerNodes d env k cx.handler h
        pure [.tryCatch true (seqList b') (seqList h')]
      | _, _ => none
    | .Throw args => if k matches .Procedure then lowerThrow d cx args else none
    | .Raise lvl fmt args opts => lowerRaise d cx lvl fmt args opts
    | .Txn op name => if k matches .Procedure then lowerTxn d cx op name else none

  def lowerNodes (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) (cx : Ctx) :
      List Ast.Node → Option (List Stmt)
    | [] => some []
    | n :: ns => do
      let s ← lowerNode d env k cx n
      let ss ← lowerNodes d env k cx ns
      pure (s ++ ss)

  /-- A block inside the body. With declarations it is outside the spec: a
  PL/pgSQL nested `DECLARE` re-initialises on every entry and can shadow.
  Without, it is its statements (T-SQL `BEGIN ... END` has none). The one
  exception (C3b) is the converter's block around a `FOR` loop over a
  query, which declares only the loop's record (`lowerForBlock`). -/
  def lowerInner (d : Dialect) (env : TyEnv) (k : Ast.RoutineKind) (cx : Ctx) :
      Ast.Block → Option (List Stmt)
    | .Block [] body => lowerNodes d env k cx body
    | .Block [.Declare r (some ty) none] [.ForQuery r' q body] =>
      lowerForBlock d env k r ty r' q body
    | .Block (_ :: _) _ => none
end

/-- The names a list of statements declares, in order, with their types. -/
def declTys (ss : List Ast.Stmt) : List (String × Option Ast.Ty) :=
  ss.filterMap fun
    | .Declare x ty _ => some (x, ty)
    | _ => none

mutual
  /-- The names T-SQL `DECLARE`s anywhere in a node, in order, with their
  types. -/
  def declsNode : Ast.Node → List (String × Option Ast.Ty)
    | .NStmt (.Declare x ty _) => [(x, ty)]
    | .NBlock b => declsBlock b
    | .If _ t e => declsNodes t ++ declsNodes e
    | .While _ b => declsNodes b
    | .ForQuery _ _ b => declsNodes b
    | .Try b hs => declsNodes b ++ declsHandlers hs
    | .NStmt _ | .Call .. | .Setting .. | .Throw _ | .Raise .. | .Txn .. => []

  def declsNodes : List Ast.Node → List (String × Option Ast.Ty)
    | [] => []
    | n :: ns => declsNode n ++ declsNodes ns

  def declsHandlers : List Ast.Handler → List (String × Option Ast.Ty)
    | [] => []
    | .Handler _ b :: hs => declsNodes b ++ declsHandlers hs

  def declsBlock : Ast.Block → List (String × Option Ast.Ty)
    | .Block ds body => declTys ds ++ declsNodes body
end

/-- The typed names, for `TyEnv`. -/
def typedNames (xs : List (String × Option Ast.Ty)) : TyEnv :=
  xs.filterMap fun (x, t) => t.map (x, ·)

/-! ## Routines -/

def lowerMode : Ast.Mode → Core.Mode
  | .In => .inp
  | .Out => .out
  | .InOut => .inout

/-- A parameter default of the spec: none, the NULL literal, or a value of
the slice that fits the parameter's declared type (`fitsTy`, as
`valueFits` below), so that neither engine converts it ("Parameter and
return types" below). -/
def dfltOk (d : Dialect) (env : TyEnv) (ty : Ast.Ty) : Option Ast.Expr → Bool
  | none => true
  | some .Null => true
  | some e => match typedTo d env {} (valTypeOf d ty) e with
    | some v => fitsTy d ty v
    | none => false

def lowerParam (d : Dialect) (env : TyEnv) : Ast.Param → Option Core.Param
  | .Param name ty mode dflt =>
    let name := match d with
      | .tsql _ _ => name
      | .pg _ _ _ => pgName name
    if !dfltOk d env ty dflt then none else
    -- A default that reads a variable is refused by both engines.
    match dflt.map (lowerValue d env {} (valTypeOf d ty)) with
    | none => some ⟨name, lowerMode mode, none⟩
    | some (some e) => if e.vars = [] then some ⟨name, lowerMode mode, some e⟩ else none
    | some none => none

/-- Every name is ASCII. -/
def asciiNames (xs : List String) : Bool :=
  xs.all fun x => x.toList.all fun (c : Char) => c.toNat < 128

/-- Every variable the routine reads or writes is a parameter or a local,
and none is declared twice. In both engines an undeclared name is an
error (SQL Server refuses the `CREATE`; PL/pgSQL fails at run time with
"column ... does not exist"), while the core would read NULL; T-SQL
system functions such as `@@ROWCOUNT` are not variables either. -/
def wellScoped (r : Core.Routine) : Bool :=
  let declared := r.params.map (·.name) ++ r.locals
  distinct declared && r.vars.all declared.contains

/-- (C3b) The variable of `EXISTS` tests (`existsVar`), as a local of the
lowered routine when its body uses it. -/
def hiddenLocals (s : Stmt) : List Var :=
  if s.vars.contains existsVar then [existsVar] else []

/-- The declared types of a T-SQL routine's variables. -/
def envT : Ast.Routine → TyEnv
  | .Routine _ _ params _ _ _ body =>
    params.map (fun | .Param n ty _ _ => (n, ty)) ++ typedNames (declsBlock body)

/-! ## String stores, on every path

The C2b slice holds every string it stores to the target's declared
length (`fitsTy`), but a routine outside the slice (the C1 path) used to
store strings unchecked, and the core stores a value as it is. Both
engines convert on a store, differently: T-SQL silently truncates a
string longer than its target (probed: eval/casts/assign, `DECLARE @v
varchar(3) = 'abcdef'` gives `abc`) and turns characters a `varchar`
cannot hold into others; PostgreSQL raises "value too long" (probed:
eval/casts/assign) unless the excess is spaces. "Assignment converts,
differently" in "The C2b slice: strings" has the citations. So on every
path (checker 0.9.0) a store into a target that may hold a string is in
the spec only when no conversion can change the value (`storeOk`):

* the value is the NULL literal; or
* the target is the slice's integer type, or a type that is certainly not
  a character type (`nonCharType`: the numeric, date and time, boolean,
  binary, `uuid`, `xml` and `json` types by name; a conversion there
  changes no length, and what it does change is the C1 path's
  `ASM-C1-OPERATORS` caveat as before); or
* the target is a string type without a length that holds every value:
  PostgreSQL `text` or `varchar` without a length, T-SQL `nvarchar(max)`
  (2 GB, beyond any value of a routine; from memory); or
* the target has a length the value provably fits (`storeTy`, `fitsLen`):
  the value is a string of the slice (`typed`) whose bound is at most the
  target's length and, into a T-SQL `varchar` (with or without `max`),
  not `wide` (no character outside the code page).

Everything else is outside the spec: a value outside the slice (a
function result, a cast, an integer) into a bounded string, `char(n)`
and `nchar(n)` (blank-padded), T-SQL `text`, `sysname`, user and alias
types (they may be strings), and a variable declared without a type.
PostgreSQL drops the length of a parameter and of a function result and
keeps that of a local: probed on PostgreSQL 18.6, 2026-10-28 (a
procedure `(a varchar(3), INOUT r varchar(3))` doing `r := a || 'xyz'`
called with `'abcdef'` returns `abcdefxyz`; a function `RETURNS
varchar(3)` returning the same gives `abcdefxyz`; a parameter `s
varchar(3) DEFAULT 'abcdef'` holds `abcdef`; a local `x varchar(3)`
assigned `'abcdef'` raises "value too long for type character
varying(3)"). So here a PostgreSQL parameter or result takes any string
(`envP`, `stripMod`) and a local is held to its length. (The C2b slice
still holds parameters to their declared length, `envPDecl`, which is
only more conservative.)

The stores (`storesNodes`): `SET @x = e` / `x := e`; T-SQL `DECLARE @x t
= e` and a PL/pgSQL declaration's default; a function's `RETURN e`
against its declared result type; and the copy-back of an `OUTPUT`
argument (T-SQL) or of an argument in an `OUT`/`INOUT` position of the
catalog (PL/pgSQL) into a variable, whose value comes from the callee
and has no bound here, so the variable must be an integer, of a
certainly non-character type, or a string type without a length that
holds every value (`copyOk`). Parameter defaults are held by `dfltOk`.
Data statements write `int` columns and (C3c) string columns only with
values that fit (`colValue`, `colFits`); a C3b pick's string store is
held here (`pickStoresOk`). A procedure's
`RETURN e` sets the integer status (`statusExpr`), not a string. Not
covered here: the conversion of a call's argument into the callee's
parameter (the callee's boundary; the catalog does not give its types),
and what a T-SQL caller does with an `OUTPUT` value it receives. -/

/-- A type that is certainly not a character type, by name. -/
def nonCharType (ty : Ast.Ty) : Bool :=
  match ty with
  | .Ty n _ => ["INT", "INTEGER", "INT2", "INT4", "INT8", "BIGINT", "SMALLINT", "TINYINT", "DECIMAL",
      "NUMERIC", "DEC", "MONEY", "SMALLMONEY", "FLOAT", "FLOAT4", "FLOAT8", "REAL",
      "DOUBLE PRECISION", "BIT", "BOOLEAN", "BOOL", "DATE", "TIME", "DATETIME", "DATETIME2",
      "SMALLDATETIME", "DATETIMEOFFSET", "TIMESTAMP", "TIMESTAMPTZ", "INTERVAL", "UNIQUEIDENTIFIER",
      "UUID", "BINARY", "VARBINARY", "BYTEA", "XML", "JSON", "JSONB"].contains n.toUpper

/-- The length and width of a string target: the slice's string types,
and T-SQL `varchar(max)` and `nvarchar(max)` without a bound. -/
def storeTy (d : Dialect) (ty : Ast.Ty) : Option (Option Nat × Bool) :=
  match strType d ty, d, ty with
  | some r, _, _ => some r
  | none, .tsql _ _, .Ty n [a] =>
    if a.toUpper == "MAX" then
      if n.toUpper == "NVARCHAR" then some (none, true)
      else if pgVarchar n.toUpper then some (none, false)
      else none
    else none
  | none, _, _ => none

/-- A store into a target of a type outside the slice: a certainly
non-character type takes a value outside the slice (the C1 path), a
string type without a slice length the strings that fit (C2b). -/
def storeNonSlice (d : Dialect) (ty : Ast.Ty) (vt : Option (SExpr × STy)) : Bool :=
      if nonCharType ty then true else
      match storeTy d ty with
      | none => false
      | some (n, wt) =>
        -- No length and every character: any value is kept.
        if n.isNone && (wt || (d matches .pg _ _ _)) then true else
        match vt with
        | some (_, .nul) => true
        | some (_, .str m w) => fitsLen n m && (wt || !w)
        | _ => false

/-- A store into a target declared `ty` of the NULL literal (`isNull`),
or of a value whose slice type is `vt` (`none`: outside the slice),
changes no value. -/
def storeOkTy (d : Dialect) (ty : Option Ast.Ty) (isNull : Bool) (vt : Option (SExpr × STy)) :
    Bool :=
  if isNull then true else
  match ty with
  | none => false
  | some ty =>
    -- A string target without a length that takes every value.
    let unbounded : Bool := match strType d ty with
      | some (none, wt) => wt || (d matches .pg _ _ _)
      | _ => false
    match valTypeOf d ty, vt with
    -- C2c: an integer into an integer slot must fit it (a narrowing to a
    -- width it may not fit: "Stores between widths"); a value into a
    -- bit, date or timestamp slot must be of its type (a timestamp into a
    -- date, a wider precision, a number into a bit: outside).
    | some (.int t), some (v, .int w) => fitsSTy (.int t) (v, .int w)
    | some (.int _), some (_, .nul) => true
    -- A string into an integer: the C1 path, as before (C2b).
    | some (.int _), some (_, .str ..) => true
    -- (C2c) A bit, a date, a timestamp or a condition into an integer:
    -- outside. T-SQL stores a bit as 0 or 1, PL/pgSQL raises 22P02
    -- (eval/values V3); a date into an integer fails in both, differently.
    | some (.int _), some _ => false
    | some (.str n wt), some (_, v) =>
      -- No length and every character: any value is kept.
      if n.isNone && (wt || (d matches .pg _ _ _)) then true else
      match v with
      | .nul => true
      | .str m w => fitsLen n m && (wt || !w)
      | _ => false
    | some want, some v => fitsSTy want v
    -- A value outside the slice into an integer or a certainly
    -- non-character type, or an unbounded string: the C1 path, as before.
    | some _, none => intType d ty || nonCharType ty || unbounded
    -- (C2c) A value of a slice type other than a string into a variable of
    -- a type outside the slice converts (a `datetime2(6)` into a
    -- `datetime` rounds to 1/300 s, a `timestamp` keeps it): outside.
    -- A string into a type outside the slice: only into a string type
    -- (T-SQL `varchar(max)`, `nvarchar(max)`), never into one T-SQL and
    -- PostgreSQL read strings into by their own rules (`time`, a date
    -- type, `numeric`: re-review of C2c).
    | none, some (_, t) =>
      if t == .nul then true
      else if (t matches .str ..) && (storeTy d ty).isSome then storeNonSlice d ty vt
      else false
    | none, none => storeNonSlice d ty vt

/-- A store of `v` into a target declared `ty` changes no value. (C2c) A
string literal stored into a date or timestamp is in the spec only in
the ISO forms the slice reads (`coerce`): T-SQL reads any other form by
the session's `DATEFORMAT` and `LANGUAGE`, PostgreSQL by its
`DateStyle`, so no C1 operator stands for it. -/
def storeOk (d : Dialect) (env : TyEnv) (ty : Option Ast.Ty) (v : Ast.Expr) : Bool :=
  let want := ty.bind (valTypeOf d)
  match want, v with
  | some .date, .Str _ | some (.ts _), .Str _ | some .date, .Cast (.Str _) _
  | some (.ts _), .Cast (.Str _) _ => (want.bind fun w => coerce d w v).isSome
  | _, _ => storeOkTy d ty (v matches .Null) (typedTo d env {} want v)

/-- (C3c) The select item `e` of the query `q`, typed with the query's
table in scope: the value a pick stores. -/
def itemTy (d : Dialect) (env : TyEnv) (q : Ast.Select) (e : Ast.Expr) : Option (SExpr × STy) := do
  let s ← querySpec d q
  typed d env s.sc e

/-- (C3c) The stores of a query's picks (C3b rules 1, 2 and 4): each
target variable with the select item it gets. A string column read into
a string variable must fit it (eval/tables T9: SQL Server truncates,
PostgreSQL raises 22001). -/
def pickStoresOk (d : Dialect) (env wenv : TyEnv) (q : Ast.Select) (ts : List (Var × Ast.Expr)) :
    Bool :=
  ts.all fun (x, e) => storeOkTy d (lookup wenv x) false (itemTy d env q e)

/-- (C3c) The stores of the converter's `FOR` block over record `r`
(C3b rule 1): every statement of the loop's body is `x := r.a` for an
item `e AS a` of the query, and `e` fits `x`. Any other body is refused
here; `lowerForBlock` refuses it too. -/
def forStoresOk (d : Dialect) (env wenv : TyEnv) (r : String) (q : Ast.Select)
    (body : List Ast.Node) : Bool :=
  let items := match querySpec (withRecord d r) q with
    | some s => s.items
    | none => []
  body.all fun n => match fieldAssign (pgName r) n with
    | some (x, a) =>
      match items.find? (fun | .SelItem _ (some a') _ => pgName a' == a | _ => false) with
      | some (.SelItem e _ _) => storeOkTy d (lookup wenv x) false (itemTy (withRecord d r) env q e)
      | none => false
    | none => false

/-- The variable a copy-back writes can take any value of its type: an
integer, a certainly non-character type, or a string type without a
length that holds every value. -/
def copyOk (d : Dialect) (wenv : TyEnv) (x : Var) : Bool :=
  match lookup wenv x with
  | some ty =>
    intType d ty || nonCharType ty ||
      match storeTy d ty with
      | some (none, wt) => wt || (d matches .pg _ _ _)
      | _ => false
  | none => false

/-- The copy-backs of a call's arguments. -/
def callStoresOk (d : Dialect) (wenv : TyEnv) (n : List String) (args : List Ast.Arg) : Bool :=
  match d with
  | .tsql _ _ => args.all fun
    | .Arg _ e true => match target d e with
      | some x => copyOk d wenv x
      | none => false
    | .Arg _ _ false => true
  | .pg Γ _ _ =>
    let modes := (lookup Γ.routines (dotted (n.map pgName))).getD []
    (modes.zip args).all fun
      | (.In, _) => true
      | (_, .Arg _ e _) => match target d e with
        | some x => copyOk d wenv x
        | none => false

/-- The stores of a `SELECT` statement: each `@x = e` of the select list
(T-SQL), each `INTO` target with the item in its position (PL/pgSQL). -/
def pickStores (d : Dialect) (s : QSpec) : List (Var × Ast.Expr) :=
  let assigned := s.items.filterMap fun
    | .SelItem e _ (some v) => (target d (.Var v)).map (·, e)
    | _ => none
  let intos := (s.into.zip s.items).filterMap fun
    | (x, .SelItem e _ _) => (target d x).map (·, e)
  assigned ++ intos

def storesStmt (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) : Ast.Stmt → Bool
  -- C3c: a scalar subquery's one item, and the picks of `SELECT @x = e` and
  -- `SELECT ... INTO x`.
  | .Assign t (.Subquery q) => match target d t, querySpec d q with
    | some x, some { items := [.SelItem e _ _], .. } => pickStoresOk d env wenv q [(x, e)]
    | some x, _ => storeOk d env (lookup wenv x) (.Subquery q)
    | none, _ => true -- refused by `lowerStmt`
  | .SSelect q => match querySpec d q with
    | some s => pickStoresOk d env wenv q (pickStores d s)
    | none => true -- refused by `lowerStmt`
  | .Assign t v => match target d t with
    | some x => storeOk d env (lookup wenv x) v
    | none => true -- refused by `lowerStmt`
  | .Declare _ ty (some e) => storeOk d env ty e
  | .Return (some e) => match ret with
    | some ty => storeOk d env (some ty) e
    | none => true -- a procedure's status
  | _ => true

mutual
  def storesNode (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) : Ast.Node → Bool
    | .NStmt s => storesStmt d env wenv ret s
    | .NBlock (.Block ds body) =>
      ds.all (storesStmt d env wenv ret) && storesNodes d env wenv ret body
    | .If _ t e => storesNodes d env wenv ret t && storesNodes d env wenv ret e
    | .While _ b => storesNodes d env wenv ret b
    | .ForQuery r q b => forStoresOk d env wenv r q b
    | .Call n args _ => callStoresOk d wenv n args
    | .Try b hs => storesNodes d env wenv ret b && storesHandlers d env wenv ret hs
    | .Setting .. | .Throw _ | .Raise .. | .Txn .. => true

  def storesNodes (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) : List Ast.Node → Bool
    | [] => true
    | n :: ns => storesNode d env wenv ret n && storesNodes d env wenv ret ns

  def storesHandlers (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) :
      List Ast.Handler → Bool
    | [] => true
    | .Handler _ b :: hs => storesNodes d env wenv ret b && storesHandlers d env wenv ret hs
end

/-- Every string store of the routine keeps its value (`storeOk`).
`retTy` is the result type as the dialect keeps it (PostgreSQL drops a
result's length, `stripMod`). -/
def storesRoutine (d : Dialect) (env wenv : TyEnv) (retTy : Ast.Ty → Ast.Ty) : Ast.Routine → Bool
  | .Routine kind _ _ returns _ _ (.Block ds body) =>
    let ret := match kind with
      | .Function => returns.map retTy
      | .Procedure => none
    ds.all (storesStmt d env wenv ret) && storesNodes d env wenv ret body

/-- A T-SQL routine. Outside the spec, besides the statements above:

* a name declared twice, or a local with a parameter's name (SQL Server
  refuses both, error 134), or a variable never declared (`wellScoped`);
* two different names equal up to case, or a non-ASCII name: whether
  `@a` and `@A` are one variable depends on the instance's collation, so
  the lowering, which keeps names as written, is faithful only when no
  two names of the routine could be confused;
* (C4) a transaction statement in a routine that does not start with `SET
  XACT_ABORT ON`, or `BEGIN TRAN` and `COMMIT` that do not form a bracket
  in the routine's statement list ("The C4 rules");
* a string store that could change the value ("String stores, on every
  path"). -/
def lowerT (dep : Deployment) (Γ : Catalog) (a : Ast.Routine) : Option Core.Routine :=
  match a with
  | .Routine kind _ params _ _ _ body => do
    let d := Dialect.tsql dep Γ
    let env := envT a
    let ps ← params.mapM (lowerParam d env)
    let nodes ← match body with
      | .Block [] ns => some ns
      | .Block (_ :: _) _ => none
    if hasTxnNodes nodes && !(leadingSettings nodes).contains ("XACT_ABORT", true) then none else
    if !txnBracket nodes then none else
    if !storesRoutine d env env id a then none else
    let stmts ← lowerNodes d env kind { txn := true } nodes
    let (ps, stmts) : List Core.Param × List Stmt := match kind with
      | .Procedure => (ps ++ [statusParam], Stmt.assign statusVar (.lit 0) :: stmts)
      | .Function => (ps, stmts)
    let r : Core.Routine :=
      { params := ps, locals := (declsBlock body).map (·.1) ++ hiddenLocals (seqList stmts),
        body := seqList stmts }
    if wellScoped r && caseSafe r.vars then some r else none

/-- The deployment convention for the T-SQL return status of a converted
procedure (`ASM-RETURN-CODE`): callers read it from the trailing `INOUT
return_code` parameter, and a procedure without one has status 0, which
is the status of a T-SQL procedure that never says `RETURN e`. So a
PL/pgSQL procedure whose last parameter is `INOUT return_code` lowers as
written (the certificate pairs `statusVar` with it), and one without is
given the status parameter, set to 0 on entry and never changed. A
procedure that does have the parameter must set it to 0 itself before
anything else can end the routine. -/
def statusName : Var := "return_code"

def hasStatus (ps : List Core.Param) : Bool :=
  match ps.getLast? with
  | some p => p.name == statusName && p.mode == .inout
  | none => false

/-- A PL/pgSQL declaration `x t [:= e]`: the name, and the assignment the
block runs on entry, in declaration order, when there is a default. -/
def lowerDecl (d : Dialect) (env : TyEnv) : Ast.Stmt → Option (Var × List Stmt)
  | .Declare x _ none => some (pgName x, [])
  | .Declare x ty (some e) =>
    (lowerValue d env {} (ty.bind (valTypeOf d)) e).map fun e' => (pgName x, [.assign (pgName x) e'])
  | _ => none

/-- The declared types of a PL/pgSQL routine's variables, folded: what a
write to them must fit (C2b). -/
def envPDecl : Ast.Routine → TyEnv
  | .Routine _ _ params _ _ _ (.Block decls _) =>
    params.map (fun | .Param n ty _ _ => (pgName n, ty)) ++
      typedNames ((declTys decls).map fun (x, t) => (pgName x, t))

/-- A parameter's type as PostgreSQL keeps it: without the length of a
`varchar` ("PostgreSQL parameters carry no length", C2b). -/
def stripMod : Ast.Ty → Ast.Ty
  | .Ty n args => if pgVarchar n.toUpper then .Ty n [] else .Ty n args

/-- The types of a PL/pgSQL routine's variables as reads see them,
folded: parameters without a `varchar` length (C2b). The lowering types
expressions with these. -/
def envP : Ast.Routine → TyEnv
  | .Routine _ _ params _ _ _ (.Block decls _) =>
    params.map (fun | .Param n ty _ _ => (pgName n, stripMod ty)) ++
      typedNames ((declTys decls).map fun (x, t) => (pgName x, t))

/-- The names that are variables in a PL/pgSQL routine, folded: its
parameters, its declarations, and `found` (`resolveName`). -/
def pgVars : Ast.Routine → List Var
  | .Routine _ _ params _ _ _ (.Block decls _) =>
    params.map (fun | .Param n _ _ _ => pgName n) ++ (declTys decls).map (pgName ·.1) ++ ["found"]

/-- (C3d) The names a PL/pgSQL qualifier may not be besides the variables:
the routine's own name, folded (`Dialect.pg`, `resolveQual`). -/
def pgQuals : Ast.Routine → List String
  | .Routine _ name _ _ _ _ _ => name.getLast?.map pgName |>.toList

/-- A PL/pgSQL routine: one block, its `DECLARE` section, its statements.
Every name is folded by `pgName`. Outside the spec, besides the
statements above: a non-ASCII name; a name declared twice, or a local
with a parameter's name (PostgreSQL lets the local shadow the parameter;
the core has one variable per name); a variable never declared
(`wellScoped`). -/
def lowerP (Γ : Catalog) (b : Ast.Routine) : Option Core.Routine :=
  match b with
  | .Routine kind _ params _ sec _ (.Block decls stmts) => do
    let d := Dialect.pg Γ (pgVars b) (pgQuals b)
    let env := envP b
    let ps ← params.mapM (lowerParam d env)
    let ds ← decls.mapM (lowerDecl d env)
    -- Every string store keeps its value ("String stores, on every path").
    if !storesRoutine d env env stripMod b then none else
    -- C4: `COMMIT` only in a procedure with its caller's privileges.
    let cx : Ctx := { commit := (kind matches .Procedure) && (sec matches .Caller) }
    let body ← lowerNodes d env kind cx stmts
    let body := (ds.map (·.2)).flatten ++ body
    -- The status of a procedure without `return_code` (see below).
    let (ps, body) : List Core.Param × List Stmt :=
      if kind matches .Procedure && !hasStatus ps then
        (ps ++ [statusParam], Stmt.assign statusVar (.lit 0) :: body)
      else (ps, body)
    let r : Core.Routine :=
      { params := ps, locals := ds.map (·.1) ++ hiddenLocals (seqList body), body := seqList body }
    if wellScoped r && asciiNames r.vars then some r else none

/-- The routine kind and the privileges it runs with: the check asks both
sides to agree on them. -/
def header : Ast.Routine → Bool × Bool
  | .Routine kind _ _ _ sec _ _ =>
    (match kind with | .Procedure => true | .Function => false,
     match sec with | .Caller => true | .Owner => false)

/-! ## Parameter and return types

The lowered routines carry no types: the core binds a value as it is. So
the check also asks each T-SQL parameter and the PL/pgSQL parameter in
its position, and the two return types of a function, to be declared
with corresponding types (`sigCorr`), else the pair is outside the spec.
Without this, T-SQL `f(@a varchar(10)) RETURNS varchar(10)` and
PL/pgSQL `f(a integer) RETURNS integer` lower alike, though a caller
passing `'x'` gets a value from one and a conversion error from the
other. Corresponding types (`tyCorr`), as the converter writes them:

* T-SQL `int`/`integer` and PostgreSQL `integer`/`int`/`int4` (the C2a
  integer type, "The C2a slice: types");
* a T-SQL string type of the slice (`varchar(n)`, `char varying(n)`,
  `character varying(n)`, `nvarchar(n)`, `1 ≤ n ≤ 4000`) and a
  PostgreSQL one (`varchar(m)`, `character varying(m)`, `char
  varying(m)`, `varchar`, `text`), the C2b types (`strTypeT`,
  `strTypeP`); the converter writes `varchar(n)` for both T-SQL types
  (crates/sqldiff-convert/src/tsql.rs, `ty_pg`). The lengths need not
  agree: PostgreSQL drops the length of a parameter and of a function
  result (below), so `varchar(m)`, `varchar` and `text` there all take
  and give any string; what a body stores in either is held to that
  side's declared length by the slice (`fitsTy`), and an argument to the
  T-SQL length by `ASM-TSQL-STRING-ARGS`. (On the C1 path a stored string
  is not held to a length on either side; that is the C1 path's
  `ASM-C1-OPERATORS` caveat, as for a C2b assignment that does not fit;
  `ExamplesC2b`.) `max` lengths, `char(n)`, `bpchar` and the rest are
  outside;
* (C2c) T-SQL `bigint` and PostgreSQL `bigint` or `int8`, `smallint`
  and `smallint` or `int2`, without type arguments: the same range of
  integers in both ("int, bigint, smallint, and tinyint (Transact-SQL)";
  PostgreSQL 8.1.1, Table 8.2), so neither side converts an argument the
  other passes through; `bit` and `boolean`/`bool`; `date` (a T-SQL
  caller's dates are in PostgreSQL's wider range); `datetime2(6)` and
  `timestamp(6)` or `timestamp` ("The C2c slice: typed values"). Every
  other type is
  outside the spec as a parameter or return type, also where the
  converter maps it: `tinyint` to `smallint` (the range widens),
  `decimal(p,s)`/`numeric(p,s)` (PostgreSQL drops a parameter's
  type modifiers, so T-SQL rounds an argument to scale `s` and PostgreSQL
  does not), `datetime`, `datetime2(p)` with `p < 6` (the same dropped
  modifier: T-SQL rounds an argument to `p` digits, PostgreSQL keeps
  six), `datetime2(7)`, `time` (precision), `char(n)`
  (padding), `varchar(max)` to
  `text`, `uniqueidentifier` to `uuid` (text form), and user types.

The trailing PL/pgSQL `INOUT return_code` parameter of a procedure (the
T-SQL status, "The T-SQL return status") has no T-SQL counterpart; it
must be the integer type, as the status is.

**Parameter defaults** (`dfltOk`, in `lowerParam`) must be the NULL
literal or a value of the slice that fits the parameter's declared type
(`fitsTy`: an integer or a string literal within the declared length),
else the routine is outside the spec. T-SQL converts a default to the
parameter's type, truncating a string (`@s varchar(3) = 'abcdef'` is
`'abc'` in the body, as for `DECLARE`, "Assignment converts,
differently"); PostgreSQL drops a parameter's length modifier ("Parenthesized
type modifiers ... are discarded by CREATE FUNCTION", CREATE FUNCTION,
Notes; from memory), so `s varchar(3) DEFAULT 'abcdef'` keeps all six
characters. A default that fits is the same value in both. For
arguments the same dropped modifier is why a PostgreSQL parameter is read
as unbounded ("PostgreSQL parameters carry no length", C2b): an argument
longer than `n` reaches the PostgreSQL body whole and the T-SQL body
truncated, which `ASM-TSQL-STRING-ARGS` excludes (callers pass values
that fit). Probed on PostgreSQL 18.6 (2026-10-28): `s varchar(3) DEFAULT
'abcdef'` holds `abcdef`; the T-SQL truncation is the C2b `DECLARE` probe. -/

/-- A T-SQL parameter or return type and a PostgreSQL one correspond
(C2c: integers of the same width, `bit` and `boolean`, `date`, and
`datetime2(6)` with `timestamp(6)` or `timestamp`; a T-SQL
`datetime2(p)` with `p < 6` rounds an argument that PostgreSQL, having
dropped the modifier, keeps, so it is outside). -/
def tyCorr (ta tp : Ast.Ty) : Bool :=
  let dt : Dialect := .tsql { rowCounts := false, routines := [] } ∅
  let dp : Dialect := .pg ∅ [] []
  -- A string type of the slice on both, the PostgreSQL one without a
  -- length or with the T-SQL one (re-review of C2c: a caller's argument is
  -- held to the PostgreSQL DDL's length, `sameKind`, which must then be
  -- the T-SQL parameter's).
  let str : Bool := match strTypeT ta, strTypeP tp with
    | some (n, _), some (m, _) => m.isNone || m == n
    | _, _ => false
  str ||
    match valTypeOf dt ta, valTypeOf dp tp with
    | some (.int w), some (.int v) => w == v
    | some .bit, some .bit => true
    | some .date, some .date => true
    | some (.ts 6), some (.ts 6) => true
    | _, _ => false

def paramTy : Ast.Param → Ast.Ty
  | .Param _ t _ _ => t

/-- The parameters in the same position have corresponding types, any
further PostgreSQL parameter (only the status `return_code` can lower
alike) is an integer, and the return types correspond. -/
def sigCorr : Ast.Routine → Ast.Routine → Bool
  | .Routine _ _ pa ra _ _ _, .Routine _ _ pb rb _ _ _ =>
    pa.length ≤ pb.length &&
    (pa.zip pb).all (fun (x, y) => tyCorr (paramTy x) (paramTy y)) &&
    (pb.drop pa.length).all (fun y => tyCorr (.Ty "INT" []) (paramTy y)) &&
    match ra, rb with
    | none, none => true
    | some x, some y => tyCorr x y
    | _, _ => false

/-! ## Every expression in the slice (C2a, C2b)

A routine is in the C2a slice when every expression it evaluates is a
slice expression of the right kind and every value it stores keeps its
type: conditions are booleans of the slice; assigned values, `RETURN`
values and defaults fit their target (`fitsTy`: an integer or NULL into
an integer, a string that fits or NULL into a string type); call
arguments are `int` expressions or NULL, and `OUTPUT` arguments `int`
variables; a function returns `int` or a string type. Then its lowering
has no uninterpreted operator, and no conversion happens where the core
does not see one. `env` types the expressions (reads); `wenv` gives the
declared type of a target (writes), which differs from `env` only for
PostgreSQL parameters ("PostgreSQL parameters carry no length"). `ret`
is a function's return type, `none` in a procedure (whose `RETURN e`
sets the integer status). -/

/-- A value position: an `int` expression or the NULL literal. -/
def valueOk (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) : Bool :=
  match typed d env sc e with
  | some (_, t) => valTy t
  | none => false

/-- (C3c) A value position of a data statement or a query: an integer, a
string or the NULL literal of the slice (the column or the target is
held to it by `colValue` and `storesRoutine`). -/
def anyValueOk (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) : Bool :=
  match typed d env sc e with
  | some (_, t) => isVal t
  | none => false

/-- A value stored in something declared `ty`. -/
def valueFits (d : Dialect) (env : TyEnv) (sc : Scope) (ty : Ast.Ty) (e : Ast.Expr) : Bool :=
  match typedTo d env sc (valTypeOf d ty) e with
  | some v => fitsTy d ty v
  | none => false

/-- (C2c) A call argument of the slice: a value of a slice type other
than a string (`sliceNode`). -/
def callArgOk (d : Dialect) (env : TyEnv) (e : Ast.Expr) : Bool :=
  match typed d env {} e with
  | some (_, t) => isVal t && !(t matches .str ..)
  | none => false

/-- A condition: a three-valued boolean of the slice. -/
def condOk (d : Dialect) (env : TyEnv) (sc : Scope) (e : Ast.Expr) : Bool :=
  match typed d env sc e with
  | some (_, .bool) => true
  | _ => false

/-- An assignment `t := v`: the value fits the target's declared type. -/
def assignOk (d : Dialect) (env wenv : TyEnv) (t v : Ast.Expr) : Bool :=
  match target d t with
  | some x => match lookup wenv x with
    | some ty => valueFits d env {} ty v
    | none => false
  | none => false

/-- (C3b) A query whose `WHERE`, if any, is a condition of the slice and
whose items are `COUNT(*)`, the `1` or `*` of `EXISTS`, or values of the
slice. The lowering checks the rest of the shape (`lowerSelectT`,
`lowerScalar`, `lowerExists`, `lowerForBlock`). -/
def querySlice (d : Dialect) (env : TyEnv) (q : Ast.Select) : Bool :=
  match querySpec d q with
  | some s =>
    s.conds.all (fun (sc, c) => condOk d env sc c) &&
      (existsItems s.items ||
        s.items.all fun | .SelItem e _ _ => countStar e || anyValueOk d env s.sc e)
  | none => false

def sliceStmt (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) : Ast.Stmt → Bool
  | .Assign _ (.Subquery q) => querySlice d env q
  | .SSelect q => querySlice d env q
  | .Assign t v => assignOk d env wenv t v
  | .Return none => true
  | .Return (some e) => match ret with
    | some ty => valueFits d env {} ty e
    | none => valueOk d env {} e
  | .Declare _ ty dflt => match dflt with
    | none => true
    | some e => (ty.map fun t => valueFits d env {} t e).getD false
  -- C3c: string values too; `colValue` holds them to the column.
  | .Insert _ _ (.SrcValues rows) _ _ _ => rows.all (·.all (anyValueOk d env {}))
  | .Update tgt _ sets _ wh => match writeTableOf d tgt with
    | some t =>
      let sc := scopeOf d t tgt
      sets.all (fun | .Set _ e => anyValueOk d env sc e) && wh.all (condOk d env sc)
    | none => false
  | .Delete tgt _ _ wh => match writeTableOf d tgt with
    | some t => wh.all (condOk d env (scopeOf d t tgt))
    | none => false
  | _ => false

mutual
  def sliceNode (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) : Ast.Node → Bool
    | .NStmt s => sliceStmt d env wenv ret s
    | .NBlock b => sliceBlock d env wenv ret b
    | .If c t e =>
      (match existsCond c with
        | some (q, _) => querySlice d env q
        | none => condOk d env {} c) &&
        sliceNodes d env wenv ret t && sliceNodes d env wenv ret e
    | .While c b => condOk d env {} c && sliceNodes d env wenv ret b
    -- C3b: the loop's body is the record's fields, checked by the lowering.
    | .ForQuery r q _ => querySlice (withRecord d r) env q
    -- (C2c) Any value of the slice but a string: the lowering holds each
    -- argument to its parameter's type (`argKindOk`); a string may be too
    -- long for the T-SQL callee's parameter, which truncates it.
    | .Call _ args _ => args.all fun | .Arg _ e _ => callArgOk d env e
    | .Setting .. => true
    -- The handler of the rollback-and-rethrow idiom is not lowered.
    | .Try b hs =>
      sliceNodes d env wenv ret b && (idiomHandler b hs || sliceHandlers d env wenv ret hs)
    | .Throw _ | .Raise .. | .Txn .. => true

  def sliceNodes (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) : List Ast.Node → Bool
    | [] => true
    | n :: ns => sliceNode d env wenv ret n && sliceNodes d env wenv ret ns

  def sliceHandlers (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) :
      List Ast.Handler → Bool
    | [] => true
    | .Handler _ b :: hs => sliceNodes d env wenv ret b && sliceHandlers d env wenv ret hs

  def sliceBlock (d : Dialect) (env wenv : TyEnv) (ret : Option Ast.Ty) : Ast.Block → Bool
    | .Block ds body => ds.all (sliceStmt d env wenv ret) && sliceNodes d env wenv ret body
end

def sliceParam (d : Dialect) (env : TyEnv) : Ast.Param → Bool
  | .Param _ _ _ none => true
  | .Param _ _ _ (some .Null) => true
  | .Param _ ty _ (some e) => valueFits d env {} ty e

/-- A type a function of the slice may return. -/
def retTypeOk (d : Dialect) (ty : Ast.Ty) : Bool := (valTypeOf d ty).isSome

def sliceRoutine (d : Dialect) (env wenv : TyEnv) : Ast.Routine → Bool
  | .Routine kind _ params returns _ _ body =>
    let ret := match kind with
      | .Function => returns
      | .Procedure => none
    params.all (sliceParam d env) && sliceBlock d env wenv ret body &&
      match kind with
      | .Function => (returns.map (retTypeOk d)).getD false
      | .Procedure => true

def sliceT (dep : Deployment) (Γ : Catalog) (a : Ast.Routine) : Bool :=
  sliceRoutine (.tsql dep Γ) (envT a) (envT a) a

def sliceP (Γ : Catalog) (b : Ast.Routine) : Bool :=
  sliceRoutine (.pg Γ (pgVars b) (pgQuals b)) (envP b) (envPDecl b) b

/-- The T-SQL routine aborts on every error: `SET XACT_ABORT ON` comes
before any statement that could fail. -/
def abortsT : Ast.Routine → Bool
  | .Routine _ _ _ _ _ _ (.Block _ body) => (leadingSettings body).contains ("XACT_ABORT", true)

/-- The T-SQL routine starts by setting `CONCAT_NULL_YIELDS_NULL ON`, so
every concatenation runs under it (C2b). -/
def concatNullOn : Ast.Routine → Bool
  | .Routine _ _ _ _ _ _ (.Block _ body) =>
    (leadingSettings body).contains ("CONCAT_NULL_YIELDS_NULL", true)

/-- The T-SQL routine has a parameter of a type of the slice (C2b: a
string; C2c: every type, `int` and `bigint` too): the confirmation rests
on every argument a caller passes being a value of the parameter's type
(`ASM-TSQL-STRING-ARGS`: a longer string is truncated, a `datetime2(7)`
or `datetime` value rounded, a `1.5` passed to an `int` truncated to 1,
an `int` narrowed or made 0/1 on entry, where PostgreSQL's caller passes
the value of its own type or finds no procedure). -/
def typedArgParam (ty : Ast.Ty) : Bool :=
  (valTypeOf (.tsql { rowCounts := false, routines := [] } ∅) ty).isSome

def tsqlStringParams : Ast.Routine → Bool
  | .Routine _ _ params _ _ _ _ => params.any fun | .Param _ ty _ _ => typedArgParam ty

/-- The T-SQL routine has a transaction statement (C4): `BEGIN TRAN`,
`COMMIT`, or the `ROLLBACK` of the rollback-and-rethrow idiom. -/
def tsqlTxn : Ast.Routine → Bool
  | .Routine _ _ _ _ _ _ (.Block _ body) => hasTxnNodes body

/-- The PL/pgSQL routine has a transaction statement (C4): a `COMMIT`. -/
def pgTxn : Ast.Routine → Bool
  | .Routine _ _ _ _ _ _ (.Block _ body) => hasTxnNodes body

end Sqldiff.Mirror
