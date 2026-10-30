/-
The core language: phase C1 ("Skeleton") and the first slice of phase C2
("Scalars", C2a) of docs/verified-conversion.md.

Decision 9 there: one typed core with one semantics, and each dialect's
meaning *is* its lowering into this core (`Sqldiff.Mirror.lowerT`,
`Sqldiff.Mirror.lowerP`). C1 is the control skeleton of a routine:
parameters with modes and defaults, local declarations, assignment, `if`,
`while`, `return`, blocks and calls. C2a gives expressions of one slice a
meaning of their own (`SExpr`): NULL, three-valued booleans and 32-bit
integers with their operators. Every other expression is still an
operator applied to expressions (`Expr.op`), whose meaning is a parameter
of the semantics (`ExprSem.op`). Phase C3a ("Statements", mirror depth)
adds data: tables of rows of values, described by the target catalog
(`TableDef`), and three single-table statements, `insert`, `update` and
`delete`, whose expressions may read the columns of the row at hand
(`SExpr.col`). Phase C4 adds handled errors (`tryCatch`, `raise`,
`reraise`) and `commit`. Phase C2c adds integer widths, bit/boolean,
date and timestamp values with their literals, comparisons and the
day/month/year arithmetic. Phase C3b reads one table (`query`); phase
C3d makes the read a query block (`Block`): several tables of the
catalog, their product filtered by conditions, read through columns
named by the source's position (`srcCol`).
-/

namespace Sqldiff.Core

/-- A variable name, as the lowering spells it. -/
abbrev Var := String

/-- A column, as the target catalog spells it (PostgreSQL's folded name,
`Sqldiff.Mirror.pgName`). Columns are never renamed: both sides name the
catalog's column. -/
abbrev Col := String

/-- A column of a table in the C3a slice: a 32-bit integer (T-SQL `int`,
PostgreSQL `integer`), `NOT NULL` or nullable, and whether it is part of
the primary key (a key column is `NOT NULL` whatever `notNull` says). -/
structure ColDef where
  name : Col
  notNull : Bool
  key : Bool
deriving DecidableEq, Repr

/-- A key column, or a `NOT NULL` one: NULL there is a constraint
violation. -/
def ColDef.required (c : ColDef) : Bool := c.notNull || c.key

/-- A table: its catalog name and its columns in declaration order. A row
of the table lists one value per column, in this order. -/
structure TableDef where
  name : String
  cols : List ColDef
deriving DecidableEq, Repr

def TableDef.hasKey (t : TableDef) : Bool := t.cols.any (·.key)

/-- (C3d) The core column through which a query block reads column `c`
of its source at position `i` (counted from 0 in `FROM` order): `i.c`.
Sources are told apart by position, not by table name or alias, so two
aliases of one table are two sources (a self-join) and the two lowerings
need not agree on how aliases are spelt. The block's row store
(`Sqldiff.Core.tupleStore`) gives a name to the first source that has it
(`ownerFrom`), so the semantics needs no injectivity of this naming; the
key closure checks ownership where it relies on it (`Block.determines`),
and `Sqldiff.Mirror.CatTable.wellFormed` keeps `.` out of column names
all the same. -/
def srcCol (i : Nat) (c : Col) : Col := toString i ++ "." ++ c

/-- (C3d) The columns of source `i` as the block names them. -/
def srcCols (i : Nat) (t : TableDef) : List ColDef :=
  t.cols.map fun d => { d with name := srcCol i d.name }

/-- Parameter modes. T-SQL `OUTPUT` is `inout` (the callee sees the value
the caller passed); PostgreSQL `OUT` is `out` (the callee sees NULL). -/
inductive Mode where
  | inp
  | out
  | inout
deriving DecidableEq, Repr

/-! ## The C2a slice -/

/-- The width of a signed integer (C2c): T-SQL `smallint`/`int`/`bigint`,
PostgreSQL `smallint`/`integer`/`bigint`. An arithmetic operator carries
the width of its result; a value outside it is an overflow
(`Sqldiff.Core.toIntW`). -/
inductive IW where
  | i16
  | i32
  | i64
deriving DecidableEq, Repr

/-- Arithmetic on signed integers of a width. `div` truncates toward zero
and `mod` takes the sign of the dividend (`Sqldiff.Core.arithInt`). -/
inductive AOp where
  | add
  | sub
  | mul
  | div
  | mod
deriving DecidableEq, Repr

/-- Comparisons of two integers, two dates or two timestamps of the same
kind (C2c); the result is three-valued. -/
inductive COp where
  | eq
  | ne
  | lt
  | gt
  | le
  | ge
deriving DecidableEq, Repr

/-- The part a `DATEADD` adds (C2c): days, months (the day clamped to the
result month's last day) or years (twelve months). -/
inductive DPart where
  | day
  | month
  | year
deriving DecidableEq, Repr

/-- Expressions of the C2a slice. Their meaning is fixed by the core
(`Sqldiff.Core.SExpr.eval`), not a parameter: a variable, a 32-bit integer
literal, NULL, integer arithmetic and negation, comparisons, `AND`, `OR`,
`NOT`, `IS [NOT] NULL` (`neg` is `IS NOT NULL`), `coalesce a b` (the
second argument evaluated only when the first is NULL) and `cond c a b`
(`CASE WHEN c THEN a ELSE b END`: `a` when `c` is TRUE, else `b`).
`col c` reads column `c` of the row a C3a statement is at (NULL outside
one): it is not a variable, so it is never renamed.

C2b adds strings: `str s` is a string literal, `concat a b` concatenation
(NULL if either operand is NULL), `rtrim a` the string without its
trailing spaces (U+0020 only), `streq a b` three-valued equality of two
strings as sequences of characters (no collation, no padding: the T-SQL
lowering puts the padding in with `rtrim`), and `len a` the number of
characters. Which dialect construct lowers to which is the lowering's
business (`Sqldiff.Mirror`, "The C2b slice: strings").

C2c adds typed values beyond `int` and strings: `arith` and `neg` carry
the width of their result (`IW`: 16, 32 or 64 bits), `bool b` is a bit
or boolean literal (T-SQL `bit` `0`/`1`, PostgreSQL `FALSE`/`TRUE`, the
same value as a comparison gives), `date d` a date (days since
0001-01-01, proleptic Gregorian, `Sqldiff.Core.Calendar`), `ts us` a
timestamp without time zone (microseconds since 0001-01-01T00:00:00),
`dateAdd p n d` adds `n` days, months or years to a date or timestamp,
`dateDiff a b` is the number of day boundaries from `a` to `b`
(`b`'s date minus `a`'s date), `toDate a` drops the time of a timestamp
and `toTs a` is a date at midnight (`Sqldiff.Mirror`, "The C2c slice:
typed values").

The core does not type them; the lowerings build them only from
expressions they have typed (`Sqldiff.Mirror.typed`). A variable read is
always `var`, whatever the variable's type, so a routine outside the slice
lowers as it did in C1. -/
inductive SExpr where
  | var (x : Var)
  | int (n : Int)
  | null
  | arith (w : IW) (o : AOp) (a b : SExpr)
  | neg (w : IW) (a : SExpr)
  | cmp (o : COp) (a b : SExpr)
  | and (a b : SExpr)
  | or (a b : SExpr)
  | not (a : SExpr)
  | isNull (a : SExpr) (neg : Bool)
  | coalesce (a b : SExpr)
  | cond (c a b : SExpr)
  | col (c : Col)
  | str (s : String)
  | concat (a b : SExpr)
  | rtrim (a : SExpr)
  | streq (a b : SExpr)
  | len (a : SExpr)
  | bool (b : Bool)
  | date (d : Int)
  | ts (us : Int)
  | dateAdd (p : DPart) (n d : SExpr)
  | dateDiff (a b : SExpr)
  | toDate (a : SExpr)
  | toTs (a : SExpr)
deriving DecidableEq, Repr

/-- Expressions. `sc s` is an expression of the slice. `op f args`
applies the operator named `f` (`bin:+`, `fn:coalesce`, ...) to its
arguments: the C1 path, for everything outside the slice. The core does
not know what `f` means: theorems hold for every meaning (`ExprSem.op`).
**The same `f` on both sides of a conversion is taken to mean the same
function** (`ASM-C1-OPERATORS` on the Rust side). A routine whose lowered
expressions contain no `op` does not depend on that
(`Sqldiff.Core.Routine.obs_opFree`). -/
inductive Expr where
  | sc (s : SExpr)
  | op (f : String) (args : List Expr)
deriving Repr

/-- A variable read. -/
def Expr.var (x : Var) : Expr := .sc (.var x)
/-- An integer literal. -/
def Expr.lit (n : Int) : Expr := .sc (.int n)
/-- NULL. -/
def Expr.null : Expr := .sc .null

-- `deriving DecidableEq` does not handle the nested `List Expr`.
mutual
  def Expr.decEq : (a b : Expr) → Decidable (a = b)
    | .sc s, .sc t =>
      if h : s = t then isTrue (by rw [h]) else isFalse (by intro e; cases e; exact h rfl)
    | .op f xs, .op g ys =>
      if hf : f = g then
        match Expr.decEqList xs ys with
        | isTrue h => isTrue (by rw [hf, h])
        | isFalse h => isFalse (by intro e; cases e; exact h rfl)
      else isFalse (by intro e; cases e; exact hf rfl)
    | .sc _, .op .. => isFalse (by intro e; cases e)
    | .op .., .sc _ => isFalse (by intro e; cases e)

  def Expr.decEqList : (xs ys : List Expr) → Decidable (xs = ys)
    | [], [] => isTrue rfl
    | x :: xs, y :: ys =>
      match Expr.decEq x y, Expr.decEqList xs ys with
      | isTrue h1, isTrue h2 => isTrue (by rw [h1, h2])
      | isFalse h, _ => isFalse (by intro e; cases e; exact h rfl)
      | _, isFalse h => isFalse (by intro e; cases e; exact h rfl)
    | [], _ :: _ => isFalse (by intro e; cases e)
    | _ :: _, [] => isFalse (by intro e; cases e)
end

instance : DecidableEq Expr := Expr.decEq

/-- Why an expression or a statement fails. `overflow` is T-SQL error
8115 and PostgreSQL SQLSTATE 22003 ("integer out of range"); `divByZero`
is T-SQL 8134 and SQLSTATE 22012. `constraint` is a data statement that
would leave a NULL in a `NOT NULL` or key column (T-SQL 515, SQLSTATE
23502) or two rows with one primary key (T-SQL 2627, SQLSTATE 23505).
`other` is raised by an uninterpreted operator, or by a value of the
wrong kind where the core needs an integer. `user` (C4) is an error the
routine raises itself: T-SQL `THROW number, message, state` (error
`number` ≥ 50000, severity 16) and PL/pgSQL `RAISE EXCEPTION` (SQLSTATE
P0001, `raise_exception`); neither the number nor the message is part of
the kind (decision 1 compares kinds, `ASM-THROW-ERRCODE`). -/
inductive ErrKind where
  | overflow
  | divByZero
  | constraint
  | other (code : String)
  | user
  /-- (C3d) A scalar subquery that returns more than one row: T-SQL error
  512 ("Subquery returned more than 1 value"), PostgreSQL SQLSTATE 21000
  (`cardinality_violation`, "more than one row returned by a subquery
  used as an expression"); eval/queries Q5 and Q13. -/
  | cardinality
deriving DecidableEq, Repr

/-- An argument of a call: a value, or a variable the callee writes back
(T-SQL `@x OUTPUT`; a variable in an `OUT`/`INOUT` position of a
PostgreSQL `CALL`). A `ref` argument passes the variable's current value. -/
inductive Arg where
  | val (e : Expr)
  | ref (x : Var)
deriving DecidableEq, Repr

/-- What a query statement (C3b, `Stmt.query`) does with the rows of its
table where its condition is TRUE:

* `row orNull sets`: each `(x, e)` of `sets` assigns `x` the value of `e`
  on the last such row, all evaluated on that row before any is assigned
  (T-SQL `SELECT @x = e, ... FROM t WHERE c`, the PL/pgSQL `FOR` loop the
  converter writes for it). With no such row, nothing is assigned, or,
  with `orNull`, every `x` is set to NULL (a scalar subquery, `SET @x =
  (SELECT e FROM t WHERE c)`). (C3d) With `orNull`, more than one row is
  the error `cardinality`, as both engines raise it. Without `orNull` the
  lowerings build it only where at most one row can be TRUE on a
  database that satisfies the catalog (`Block.atMostOne`,
  `Sqldiff.Core.kept_length_le_one`), so "the last" is the only one;
  which one a database with several would give is not specified by
  either vendor, and the core's choice is not a claim about them.
* `count x`: `x` is the number of such rows, a 32-bit integer
  (`SELECT @x = COUNT(*) FROM t WHERE c`); more than `intMax` rows is an
  overflow.
* `any x`: `x` is TRUE when there is such a row, else FALSE (`EXISTS`),
  never NULL. The lowerings put it into a variable no routine can name
  (`Sqldiff.Mirror.existsVar`) and test that in the `IF`. -/
inductive Pick where
  | row (orNull : Bool) (sets : List (Var × Expr))
  | count (x : Var)
  | any (x : Var)
deriving DecidableEq, Repr

/-- (C3d) A query block: the catalog tables of a `FROM` (`srcs`, in
order; a table may stand twice, under two aliases) and its conditions
(`conds`): every `ON` of an inner join and the `WHERE`, each split at its
top-level `AND`s, in `FROM` order and then the `WHERE`. Its rows are the
tuples of the product of the sources' rows (nested loops in `FROM`
order) where every condition is TRUE (`Sqldiff.Core.blockRows`). Cross
joins and comma lists add no condition. -/
structure Block where
  srcs : List TableDef
  conds : List Expr
deriving DecidableEq, Repr

/-- (C3d) A query: a block. The later parts of C3d add an ordering and a
row limit here; until then a query has neither. -/
structure Query where
  blk : Block
deriving DecidableEq, Repr

/-- Statements. A block is a sequence (`seq`); the empty block is
`skip`. Calls name another routine, which the world runs (see
`Sqldiff.Core.WorldModel`); they are never inlined.

The data statements of C3a carry the definition of the table they write
(from the catalog, the same on both sides):

* `insert t cs rows`: one row per element of `rows`, the values of the
  columns `cs` in order (`INSERT INTO t (cs) VALUES (...), ...`); every
  other column is NULL. The expressions see no columns.
* `update t sets c`: each row where `c` is TRUE (every row when `c` is
  `none`) gets, for each `(col, e)` of `sets`, the value of `e`
  evaluated on the row's old values (`UPDATE t SET col = e, ... WHERE c`).
* `delete t c`: removes each row where `c` is TRUE (every row when
  `none`).

Their meaning is `Sqldiff.Core.insertRows`, `updateRows` and
`deleteRows`.

Phase C4 adds error handling and transaction control:

* `tryCatch undo body handler`: runs `body`; if it ends with an error,
  runs `handler` with that error bound to its `reraise`s
  (`Stmt.bindErr`), from the state the error left, except that with
  `undo` the database is put back as it was when `body` began. `undo` is
  PL/pgSQL's `BEGIN ... EXCEPTION WHEN OTHERS THEN ... END` (the block is a
  subtransaction); without it, T-SQL's `BEGIN TRY ... END TRY BEGIN CATCH
  ... END CATCH`. Variables keep the values the error left in both.
* `raise k`: fails with `k` (`THROW n, m, s`, `RAISE EXCEPTION`: `k` is
  `ErrKind.user`).
* `reraise`: T-SQL `THROW;`, PL/pgSQL `RAISE;`, the error the enclosing
  handler handles. A handler's `reraise`s are replaced by `raise k` when
  it runs (`Stmt.bindErr`); one that is never bound fails with
  `other "reraise"` (the lowerings put `reraise` only directly in a
  handler).
* `commit`: a `COMMIT`: it changes nothing a finished call shows, so it
  does nothing. The mirror lowerings no longer write it: T-SQL and
  PL/pgSQL `COMMIT` lower to nothing, and a confirmation names
  `ASM-NO-CALLER-TXN` from the transaction statements of the source
  (`Sqldiff.Mirror`, "The C4 rules").

Phase C3b adds a read, which C3d widens to a query block:

* `query q p`: the rows of the block `q.blk` (the tuples of its sources'
  rows where every condition is TRUE; the expressions see the tuple's
  columns, `srcCol`), then `p` (`Pick`) assigns variables from them. It
  writes no table. Its meaning is `Sqldiff.Core.blockRows` and
  `pickRows`. -/
inductive Stmt where
  | skip
  | assign (x : Var) (e : Expr)
  | seq (s t : Stmt)
  | ite (c : Expr) (s t : Stmt)
  | while (c : Expr) (body : Stmt)
  | ret (e : Option Expr)
  | call (proc : String) (args : List Arg)
  | insert (t : TableDef) (cols : List Col) (rows : List (List Expr))
  | update (t : TableDef) (sets : List (Col × Expr)) (cond : Option Expr)
  | delete (t : TableDef) (cond : Option Expr)
  | tryCatch (undo : Bool) (body handler : Stmt)
  | raise (k : ErrKind)
  | reraise
  | commit
  | query (q : Query) (p : Pick)
deriving DecidableEq, Repr

/-- The handler of an error `k`: every `reraise` of `s` becomes `raise k`,
except in the handlers of the `tryCatch`s inside `s`, which bind their
own error. (The lowerings never put a `reraise` in the body of a
`tryCatch` inside a handler, so what this does there does not matter.) -/
def Stmt.bindErr (k : ErrKind) : Stmt → Stmt
  | .reraise => .raise k
  | .seq s t => .seq (s.bindErr k) (t.bindErr k)
  | .ite c s t => .ite c (s.bindErr k) (t.bindErr k)
  | .while c b => .while c (b.bindErr k)
  | .tryCatch u b h => .tryCatch u (b.bindErr k) h
  | s => s

/-- A parameter: name, mode, and the default used when the caller passes
no argument for it. -/
structure Param where
  name : Var
  mode : Mode
  default : Option Expr
deriving DecidableEq, Repr

/-- A routine. `locals` are the declared local variables, in declaration
order; every variable starts as NULL (see `Sqldiff.Core.initStore`), so
the list has no run-time effect and is kept so that the check compares
the declarations too. Types are not here: the lowerings use them to
decide which expressions are in the slice, and they are not observed. The
routine's own name is not here either: a caller observes nothing by it,
and which name reaches which routine is the deployment's business
(`Sqldiff.Mirror.Deployment`). -/
structure Routine where
  params : List Param
  locals : List Var
  body : Stmt
deriving DecidableEq, Repr

/-- A list of statements as one statement: `skip` when empty, the
statement itself when there is one, otherwise right-nested `seq`. -/
def seqList : List Stmt → Stmt
  | [] => .skip
  | [s] => s
  | s :: ss => .seq s (seqList ss)

/-! ## Variables and renaming -/

def SExpr.vars : SExpr → List Var
  | .var x => [x]
  | .int _ => []
  | .null => []
  | .arith _ _ a b => a.vars ++ b.vars
  | .neg _ a => a.vars
  | .cmp _ a b => a.vars ++ b.vars
  | .and a b => a.vars ++ b.vars
  | .or a b => a.vars ++ b.vars
  | .not a => a.vars
  | .isNull a _ => a.vars
  | .coalesce a b => a.vars ++ b.vars
  | .cond c a b => c.vars ++ a.vars ++ b.vars
  | .col _ => []
  | .str _ => []
  | .concat a b => a.vars ++ b.vars
  | .rtrim a => a.vars
  | .streq a b => a.vars ++ b.vars
  | .len a => a.vars
  | .bool _ | .date _ | .ts _ => []
  | .dateAdd _ n d => n.vars ++ d.vars
  | .dateDiff a b => a.vars ++ b.vars
  | .toDate a => a.vars
  | .toTs a => a.vars

mutual
  def Expr.vars : Expr → List Var
    | .sc s => s.vars
    | .op _ args => Expr.varsList args

  def Expr.varsList : List Expr → List Var
    | [] => []
    | e :: es => e.vars ++ Expr.varsList es
end

def Arg.vars : Arg → List Var
  | .val e => e.vars
  | .ref x => [x]

/-- The variables of an optional expression. -/
def Expr.varsOpt : Option Expr → List Var
  | some e => e.vars
  | none => []

/-- The variables a pick writes and reads. -/
def Pick.vars : Pick → List Var
  | .row _ sets => (sets.map fun p => p.1 :: p.2.vars).flatten
  | .count x | .any x => [x]

/-- The variables a pick writes. -/
def Pick.targets : Pick → List Var
  | .row _ sets => sets.map (·.1)
  | .count x | .any x => [x]

def Stmt.vars : Stmt → List Var
  | .skip => []
  | .assign x e => x :: e.vars
  | .seq s t => s.vars ++ t.vars
  | .ite c s t => c.vars ++ s.vars ++ t.vars
  | .while c b => c.vars ++ b.vars
  | .ret none => []
  | .ret (some e) => e.vars
  | .call _ args => (args.map Arg.vars).flatten
  | .insert _ _ rows => (rows.map fun es => (es.map Expr.vars).flatten).flatten
  | .update _ sets c => (sets.map fun p => p.2.vars).flatten ++ Expr.varsOpt c
  | .delete _ c => Expr.varsOpt c
  | .tryCatch _ b h => b.vars ++ h.vars
  | .raise _ | .reraise | .commit => []
  | .query q p => (q.blk.conds.map Expr.vars).flatten ++ p.vars

def Param.vars (p : Param) : List Var :=
  p.name :: (match p.default with | some e => e.vars | none => [])

/-- Every variable a routine names: parameters (and their defaults),
locals and the body's. -/
def Routine.vars (r : Routine) : List Var :=
  (r.params.map Param.vars).flatten ++ r.locals ++ r.body.vars

def SExpr.rename (f : Var → Var) : SExpr → SExpr
  | .var x => .var (f x)
  | .int n => .int n
  | .null => .null
  | .arith w o a b => .arith w o (a.rename f) (b.rename f)
  | .neg w a => .neg w (a.rename f)
  | .cmp o a b => .cmp o (a.rename f) (b.rename f)
  | .and a b => .and (a.rename f) (b.rename f)
  | .or a b => .or (a.rename f) (b.rename f)
  | .not a => .not (a.rename f)
  | .isNull a n => .isNull (a.rename f) n
  | .coalesce a b => .coalesce (a.rename f) (b.rename f)
  | .cond c a b => .cond (c.rename f) (a.rename f) (b.rename f)
  | .col c => .col c
  | .str s => .str s
  | .concat a b => .concat (a.rename f) (b.rename f)
  | .rtrim a => .rtrim (a.rename f)
  | .streq a b => .streq (a.rename f) (b.rename f)
  | .len a => .len (a.rename f)
  | .bool b => .bool b
  | .date d => .date d
  | .ts us => .ts us
  | .dateAdd p n d => .dateAdd p (n.rename f) (d.rename f)
  | .dateDiff a b => .dateDiff (a.rename f) (b.rename f)
  | .toDate a => .toDate (a.rename f)
  | .toTs a => .toTs (a.rename f)

mutual
  def Expr.rename (f : Var → Var) : Expr → Expr
    | .sc s => .sc (s.rename f)
    | .op g args => .op g (Expr.renameList f args)

  def Expr.renameList (f : Var → Var) : List Expr → List Expr
    | [] => []
    | e :: es => e.rename f :: Expr.renameList f es
end

def Arg.rename (f : Var → Var) : Arg → Arg
  | .val e => .val (e.rename f)
  | .ref x => .ref (f x)

def Pick.rename (f : Var → Var) : Pick → Pick
  | .row n sets => .row n (sets.map fun p => (f p.1, p.2.rename f))
  | .count x => .count (f x)
  | .any x => .any (f x)

/-- (C3d) The query with its conditions' variables renamed. -/
def Query.rename (f : Var → Var) (q : Query) : Query :=
  { blk := { q.blk with conds := q.blk.conds.map (Expr.rename f) } }

def Stmt.rename (f : Var → Var) : Stmt → Stmt
  | .skip => .skip
  | .assign x e => .assign (f x) (e.rename f)
  | .seq s t => .seq (s.rename f) (t.rename f)
  | .ite c s t => .ite (c.rename f) (s.rename f) (t.rename f)
  | .while c b => .while (c.rename f) (b.rename f)
  | .ret e => .ret (e.map (Expr.rename f))
  | .call p args => .call p (args.map (Arg.rename f))
  | .insert t cs rows => .insert t cs (rows.map (·.map (Expr.rename f)))
  | .update t sets c => .update t (sets.map fun p => (p.1, p.2.rename f)) (c.map (Expr.rename f))
  | .delete t c => .delete t (c.map (Expr.rename f))
  | .tryCatch u b h => .tryCatch u (b.rename f) (h.rename f)
  | .raise k => .raise k
  | .reraise => .reraise
  | .commit => .commit
  | .query q p => .query (q.rename f) (p.rename f)

def Param.rename (f : Var → Var) (p : Param) : Param :=
  { p with name := f p.name, default := p.default.map (Expr.rename f) }

/-- Renames every variable; callee names stay. -/
def Routine.rename (f : Var → Var) (r : Routine) : Routine :=
  { r with
    params := r.params.map (Param.rename f)
    locals := r.locals.map f
    body := r.body.rename f }

/-! ## What a routine's expressions use

`opFree`: no uninterpreted operator, so the routine's meaning does not
depend on `ExprSem.op` (`Sqldiff.Core.Routine.obs_opFree`). `mayRaise` and
`hasEqNe` decide which residual assumptions a C2a confirmation carries
(`Sqldiff.Mirror.residual`). -/

def Expr.opFree : Expr → Bool
  | .sc _ => true
  | .op .. => false

def Arg.opFree : Arg → Bool
  | .val e => e.opFree
  | .ref _ => true

/-- The expressions a pick evaluates (on the row it picks). -/
def Pick.exprs : Pick → List Expr
  | .row _ sets => sets.map (·.2)
  | .count _ | .any _ => []

def Stmt.opFree : Stmt → Bool
  | .skip => true
  | .assign _ e => e.opFree
  | .seq s t => s.opFree && t.opFree
  | .ite c s t => c.opFree && s.opFree && t.opFree
  | .while c b => c.opFree && b.opFree
  | .ret none => true
  | .ret (some e) => e.opFree
  | .call _ args => args.all Arg.opFree
  | .insert _ _ rows => rows.all (·.all Expr.opFree)
  | .update _ sets c => sets.all (·.2.opFree) && c.all Expr.opFree
  | .delete _ c => c.all Expr.opFree
  | .tryCatch _ b h => b.opFree && h.opFree
  | .raise _ | .reraise | .commit => true
  | .query q p => q.blk.conds.all Expr.opFree && p.exprs.all Expr.opFree

def Param.opFree (p : Param) : Bool :=
  match p.default with
  | some e => e.opFree
  | none => true

def Routine.opFree (r : Routine) : Bool :=
  r.params.all Param.opFree && r.body.opFree

/-- Every expression of a statement, in order. -/
def Stmt.exprs : Stmt → List Expr
  | .skip => []
  | .assign _ e => [e]
  | .seq s t => s.exprs ++ t.exprs
  | .ite c s t => c :: s.exprs ++ t.exprs
  | .while c b => c :: b.exprs
  | .ret none => []
  | .ret (some e) => [e]
  | .call _ args => args.filterMap fun | .val e => some e | .ref _ => none
  | .insert _ _ rows => rows.flatten
  | .update _ sets c => sets.map (·.2) ++ c.toList
  | .delete _ c => c.toList
  | .tryCatch _ b h => b.exprs ++ h.exprs
  | .raise _ | .reraise | .commit => []
  | .query q p => q.blk.conds ++ p.exprs

/-- Every expression of a routine: parameter defaults and the body's. -/
def Routine.exprs (r : Routine) : List Expr :=
  r.params.filterMap Param.default ++ r.body.exprs

/-- The expression contains an operator that can raise an error:
arithmetic or negation (overflow, division by zero). Conservative: a
literal divisor or a constant operand does not count as safe. -/
def SExpr.mayRaise : SExpr → Bool
  | .var _ | .int _ | .null | .col _ | .str _ | .bool _ | .date _ | .ts _ => false
  | .arith .. | .neg .. => true
  | .cmp _ a b | .and a b | .or a b | .coalesce a b | .concat a b | .streq a b
  | .dateAdd _ a b | .dateDiff a b =>
    a.mayRaise || b.mayRaise
  | .not a | .isNull a _ | .rtrim a | .len a | .toDate a | .toTs a => a.mayRaise
  | .cond c a b => c.mayRaise || a.mayRaise || b.mayRaise

/-- The expression compares with `=` or `<>`, the comparisons T-SQL's
`ANSI_NULLS OFF` changes. -/
def SExpr.hasEqNe : SExpr → Bool
  | .var _ | .int _ | .null | .col _ | .str _ | .bool _ | .date _ | .ts _ => false
  | .cmp o a b => o == .eq || o == .ne || a.hasEqNe || b.hasEqNe
  | .streq _ _ => true
  | .arith _ _ a b | .and a b | .or a b | .coalesce a b | .concat a b | .dateAdd _ a b
  | .dateDiff a b => a.hasEqNe || b.hasEqNe
  | .neg _ a | .not a | .isNull a _ | .rtrim a | .len a | .toDate a | .toTs a => a.hasEqNe
  | .cond c a b => c.hasEqNe || a.hasEqNe || b.hasEqNe

/-- Some subterm (the expression itself included) satisfies `p`. -/
def SExpr.anySub (p : SExpr → Bool) : SExpr → Bool
  | s@(.var _) | s@(.int _) | s@.null | s@(.col _) | s@(.str _) | s@(.bool _) | s@(.date _)
  | s@(.ts _) => p s
  | s@(.arith _ _ a b) | s@(.cmp _ a b) | s@(.and a b) | s@(.or a b) | s@(.coalesce a b)
  | s@(.concat a b) | s@(.streq a b) | s@(.dateAdd _ a b) | s@(.dateDiff a b) =>
    p s || a.anySub p || b.anySub p
  | s@(.neg _ a) | s@(.not a) | s@(.isNull a _) | s@(.rtrim a) | s@(.len a) | s@(.toDate a)
  | s@(.toTs a) => p s || a.anySub p
  | s@(.cond c a b) => p s || c.anySub p || a.anySub p || b.anySub p

/-- The expression concatenates strings (C2b: `ASM-CONCAT-NULL`). -/
def SExpr.hasConcat : SExpr → Bool := SExpr.anySub fun | .concat .. => true | _ => false

/-- The expression compares strings (C2b: `ASM-BINARY-COLLATION`). -/
def SExpr.hasStrEq : SExpr → Bool := SExpr.anySub fun | .streq .. => true | _ => false

/-- The expression counts characters (C2b: `ASM-TSQL-LEN-BMP`). -/
def SExpr.hasLen : SExpr → Bool := SExpr.anySub fun | .len _ => true | _ => false

/-- The expression adds to a date (C2c: `ASM-DATEADD-RANGE`, the result
stays within 0001-01-01 and 9999-12-31, where T-SQL raises and
PostgreSQL does not). -/
def SExpr.hasDateAdd : SExpr → Bool := SExpr.anySub fun | .dateAdd .. => true | _ => false

/-- The expression adds years to a date (C2c: `ASM-DATEADD-YEAR-END`, the
clamp of February 29 to February 28, which Microsoft's page does not
state). -/
def SExpr.hasYearAdd : SExpr → Bool := SExpr.anySub fun | .dateAdd .year .. => true | _ => false

/-- An uninterpreted operator counts as raising and as comparing: what it
does is unknown. -/
def Expr.test (p : SExpr → Bool) : Expr → Bool
  | .sc s => p s
  | .op .. => true

def Routine.mayRaise (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.mayRaise)

def Routine.hasEqNe (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.hasEqNe)

def Routine.hasConcat (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.hasConcat)

def Routine.hasStrEq (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.hasStrEq)

def Routine.hasLen (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.hasLen)

def Routine.hasDateAdd (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.hasDateAdd)

def Routine.hasYearAdd (r : Routine) : Bool := r.exprs.any (Expr.test SExpr.hasYearAdd)

/-! ## At most one row (C3d)

When a query block can keep at most one row, on a database that
satisfies the catalog (every row has a primary key, no two rows of a
table share one): the key closure. `Sqldiff.Core.kept_length_le_one`
(`Sqldiff/Core/Fidelity.lean`) proves that `Block.atMostOne` gives it.
The lowerings ask it of `SELECT @x = ...` and its `FOR` block (a pick
without `orNull`, where neither vendor says which of several rows is
assigned), and `Stmt.failsHere` uses it to see that a scalar subquery
cannot raise `cardinality`. -/

/-- The columns an expression reads. -/
def SExpr.cols : SExpr → List Col
  | .col c => [c]
  | .var _ | .int _ | .null | .str _ | .bool _ | .date _ | .ts _ => []
  | .arith _ _ a b | .cmp _ a b | .and a b | .or a b | .coalesce a b | .concat a b
  | .streq a b | .dateAdd _ a b | .dateDiff a b => a.cols ++ b.cols
  | .neg _ a | .not a | .isNull a _ | .rtrim a | .len a | .toDate a | .toTs a => a.cols
  | .cond c a b => c.cols ++ a.cols ++ b.cols

/-- The equalities a condition states between a column and an
expression: `n = e` and `e = n` with the core's `=` (`COp.eq`), which is
TRUE only for two values of one kind that are equal (`cmpVal`). String
equality (`streq`, under the T-SQL padding `rtrim`), a date against a
timestamp (`toTs`), `IS NULL` and every C1 operator are no such
equality. -/
def eqFixes : Expr → List (Col × SExpr)
  | .sc (.cmp .eq a b) =>
    (match a with | .col n => [(n, b)] | _ => []) ++ (match b with | .col n => [(n, a)] | _ => [])
  | _ => []

/-- The position, counted from the source at position `i`, of the first
of the sources `ts` that has a column named `n` (`srcCols`); none when no
source has it. -/
def ownerFrom : Nat → List TableDef → Col → Option Nat
  | _, [], _ => none
  | i, t :: ts, n =>
    if (srcCols i t).any (·.name == n) then some 0 else (ownerFrom (i + 1) ts n).map (· + 1)

/-- The source of the block a column name reads. -/
def Block.owner (b : Block) (n : Col) : Option Nat := ownerFrom 0 b.srcs n

/-- Source `i` of the block, table `t`, is determined once the sources
`done` are: it has a primary key, its column names are distinct, and for
every key column `k` some condition states `i.k = e` (`eqFixes`), where
the name `i.k` reads source `i` and `e` reads only columns of the sources
`done` (and variables and constants, which a query cannot change). Two
tuples that agree on the sources `done` and keep every condition TRUE
then agree on the key of source `i`, and so on its row. -/
def Block.determines (b : Block) (done : List Nat) (i : Nat) (t : TableDef) : Bool :=
  t.hasKey && decide (((srcCols i t).map (·.name)).Nodup) &&
    (t.cols.filter (·.key)).all fun d =>
      b.conds.any fun c => (eqFixes c).any fun (n, e) =>
        n == srcCol i d.name && b.owner n == some i &&
          e.cols.all fun m => match b.owner m with
            | some j => done.contains j
            | none => false

/-- The sources `is`, in this order, are each determined by the ones
before them (and `done`). -/
def Block.chain (b : Block) : List Nat → List Nat → Bool
  | _, [] => true
  | done, i :: is =>
    (match b.srcs[i]? with
      | some t => b.determines done i t
      | none => false) && b.chain (i :: done) is

/-- The key closure: the sources in the order they become determined,
each the first not yet determined that `determines` admits. -/
def Block.closure (b : Block) : List Nat := go b.srcs.length []
where
  go : Nat → List Nat → List Nat
    | 0, done => done.reverse
    | n + 1, done =>
      match (List.range b.srcs.length).find? (fun i =>
          !done.contains i && match b.srcs[i]? with
            | some t => b.determines done i t
            | none => false) with
      | some i => go n (i :: done)
      | none => done.reverse

/-- Every source of the block is determined: on a database where every
table's rows have distinct primary keys, the block keeps at most one
tuple (`Sqldiff.Core.kept_length_le_one`). -/
def Block.atMostOne (b : Block) : Bool :=
  b.chain [] b.closure && (List.range b.srcs.length).all b.closure.contains

/-! ## What a routine's data statements can do (C3a)

These decide whether a confirmation is in the slice
(`Sqldiff.Mirror.inC2a`) and which residual assumptions it carries
(`Sqldiff.Mirror.residual`). All are conservative: `true` means "might". -/

/-- The expression reads a column. -/
def SExpr.hasCol : SExpr → Bool
  | .var _ | .int _ | .null | .str _ | .bool _ | .date _ | .ts _ => false
  | .col _ => true
  | .arith _ _ a b | .cmp _ a b | .and a b | .or a b | .coalesce a b | .concat a b
  | .streq a b | .dateAdd _ a b | .dateDiff a b => a.hasCol || b.hasCol
  | .neg _ a | .not a | .isNull a _ | .rtrim a | .len a | .toDate a | .toTs a => a.hasCol
  | .cond c a b => c.hasCol || a.hasCol || b.hasCol

/-- A part of the expression that reads no column can raise. PostgreSQL
plans a data statement of PL/pgSQL with the variables' values as
constants and may fold such a part while planning, so it can raise even
when no row is written (`Sqldiff.Mirror`, "Constant parts of a data
statement"). For an expression without columns this is `mayRaise`. -/
def SExpr.constRaise : SExpr → Bool
  | .var _ | .int _ | .null | .col _ | .str _ | .bool _ | .date _ | .ts _ => false
  | .arith _ _ a b => if a.hasCol || b.hasCol then a.constRaise || b.constRaise else true
  | .neg _ a => if a.hasCol then a.constRaise else true
  | .cmp _ a b | .and a b | .or a b | .coalesce a b | .concat a b | .streq a b
  | .dateAdd _ a b | .dateDiff a b => a.constRaise || b.constRaise
  | .not a | .isNull a _ | .rtrim a | .len a | .toDate a | .toTs a => a.constRaise
  | .cond c a b => c.constRaise || a.constRaise || b.constRaise

/-- The expression can raise; an uninterpreted operator counts. -/
def Expr.raise (e : Expr) : Bool := Expr.test SExpr.mayRaise e

/-- Column `c` of `t` is `NOT NULL` or a key column. -/
def TableDef.requires (t : TableDef) (c : Col) : Bool :=
  t.cols.any fun d => d.name == c && d.required

/-- The value is never NULL on a row of `t` whose required columns are
not NULL (a state that satisfies the catalog): an integer literal, a
required column, arithmetic on such values (which may raise, but not give
NULL), and `COALESCE`/`CASE` whose result is one of them. -/
def SExpr.nonNull (t : TableDef) : SExpr → Bool
  | .int _ | .bool _ | .date _ | .ts _ => true
  | .col c => t.requires c
  | .arith _ _ a b | .dateAdd _ a b | .dateDiff a b => a.nonNull t && b.nonNull t
  | .neg _ a | .toDate a | .toTs a => a.nonNull t
  | .coalesce a b => a.nonNull t || b.nonNull t
  | .cond _ a b => a.nonNull t && b.nonNull t
  | _ => false

def Expr.nonNull (t : TableDef) : Expr → Bool
  | .sc s => s.nonNull t
  | .op .. => false

/-- The statement can violate a constraint (`NOT NULL`, primary key): an
`INSERT` into a table with a required column, an `UPDATE` that sets one
to a value that may be NULL (key columns are never set, `Sqldiff.Mirror`).
-/
def Stmt.violates : Stmt → Bool
  | .insert t _ _ => t.cols.any ColDef.required
  | .update t sets _ => sets.any fun p => t.requires p.1 && !p.2.nonNull t
  | _ => false

/-- The statement itself (not the statements inside it) can fail. A call
always can: the callee's own errors (a duplicate key, an overflow, its
`THROW`) are not in the core, where `WorldModel.call` does not fail, but
they reach the caller in both engines. So a T-SQL routine with a call is
in the C2a slice only when it aborts on errors (`Sqldiff.Mirror.inC2a`):
without `XACT_ABORT ON` SQL Server goes on after the failed `EXEC` (the
failing statement inside the callee is rolled back, and the callee and
then the caller carry on), while PostgreSQL's `CALL` fails and ends the
caller. -/
def Stmt.failsHere : Stmt → Bool
  | .assign _ e => e.raise
  | .ret (some e) => e.raise
  | .ite c .. => c.raise
  | .while c _ => c.raise
  | .call .. => true
  | .insert t cs rows => (Stmt.insert t cs rows).violates || rows.flatten.any Expr.raise
  | .update t sets c =>
    (Stmt.update t sets c).violates || sets.any (fun p => p.2.raise) || c.any Expr.raise
  | .delete _ c => c.any Expr.raise
  | .raise _ | .reraise => true
  -- (C3d) A scalar subquery raises `cardinality` on more than one row,
  -- which a block that keeps at most one cannot give on a database that
  -- satisfies the catalog.
  | .query q p =>
    q.blk.conds.any Expr.raise || p.exprs.any Expr.raise || (p matches .count _) ||
      ((p matches .row true _) && !q.blk.atMostOne)
  | _ => false

/-- Some statement of `s` can fail, and the error leave `s`: an error in
the body of a `tryCatch` is handled (C4). -/
def Stmt.mayFail : Stmt → Bool
  | .seq s t => s.mayFail || t.mayFail
  | .ite c a b => c.raise || a.mayFail || b.mayFail
  | .while c b => c.raise || b.mayFail
  | .tryCatch _ _ h => h.mayFail
  | s => s.failsHere

/-- `s` may write: a data statement, or a call (the callee may write). -/
def Stmt.hasWrite : Stmt → Bool
  | .insert .. | .update .. | .delete .. | .call .. => true
  | .seq s t | .ite _ s t | .tryCatch _ s t => s.hasWrite || t.hasWrite
  | .while _ b => b.hasWrite
  | _ => false

/-- `s` has a data statement. -/
def Stmt.hasData : Stmt → Bool
  | .insert .. | .update .. | .delete .. => true
  | .seq s t | .ite _ s t | .tryCatch _ s t => s.hasData || t.hasData
  | .while _ b => b.hasData
  | _ => false

/-- Some statement of `s` can fail after a write has happened; `w` says
whether one may have happened before `s`. A failing statement undoes its
own effect, so a data statement that fails counts only after an earlier
write. -/
def Stmt.errAfterWrite (w : Bool) : Stmt → Bool
  | .seq s t => s.errAfterWrite w || t.errAfterWrite (w || s.hasWrite)
  | .ite c a b => (w && c.raise) || a.errAfterWrite w || b.errAfterWrite w
  | .while c b =>
    let w' := w || b.hasWrite
    (w' && c.raise) || b.errAfterWrite w'
  -- An error of the body is handled; the handler starts from the
  -- database the body began with when `undo`, else from what it left.
  | .tryCatch u b h => h.errAfterWrite (w || (!u && b.hasWrite))
  -- A callee may write and then fail (`failsHere`).
  | .call .. => true
  | s => w && s.failsHere

/-- The data statements are in the C3a slice (`Sqldiff.Mirror`, "The C3a
slice: data statements"): a `WHERE` cannot raise; a statement cannot fail
both with an arithmetic error and with a constraint violation (which of
the two it reports then depends on the order rows are processed in,
which neither vendor fixes); no part of an `UPDATE`'s values that reads no
column can raise (`SExpr.constRaise`). -/
def Stmt.dmlSafe : Stmt → Bool
  | .insert t cs rows =>
    !((Stmt.insert t cs rows).violates && rows.flatten.any Expr.raise)
  | .update t sets c =>
    !(c.any Expr.raise) && !(sets.any fun p => Expr.test SExpr.constRaise p.2) &&
      !((Stmt.update t sets c).violates && sets.any fun p => p.2.raise)
  | .delete _ c => !(c.any Expr.raise)
  | .query q p => !(q.blk.conds.any Expr.raise) && !(p.exprs.any Expr.raise)
  | .seq s t | .ite _ s t | .tryCatch _ s t => s.dmlSafe && t.dmlSafe
  | .while _ b => b.dmlSafe
  | _ => true

/-- `s` reads a table (C3b): a query. -/
def Stmt.hasQuery : Stmt → Bool
  | .query .. => true
  | .seq s t | .ite _ s t | .tryCatch _ s t => s.hasQuery || t.hasQuery
  | .while _ b => b.hasQuery
  | _ => false

/-- `s` counts rows (C3b): `COUNT(*)` is an integer and can overflow,
which T-SQL reports as an error only under its arithmetic settings
(`ASM-TSQL-ARITH-ERRORS`). -/
def Stmt.hasCount : Stmt → Bool
  | .query _ (.count _) => true
  | .seq s t | .ite _ s t | .tryCatch _ s t => s.hasCount || t.hasCount
  | .while _ b => b.hasCount
  | _ => false

/-- Some run of the routine might fail: an expression can raise, a
data statement can violate a constraint, or a callee can fail. -/
def Routine.mayFail (r : Routine) : Bool :=
  r.params.any (fun p => p.default.any Expr.raise) || r.body.mayFail

def Routine.errAfterWrite (r : Routine) : Bool := r.body.errAfterWrite false

def Routine.hasData (r : Routine) : Bool := r.body.hasData

def Routine.dmlSafe (r : Routine) : Bool := r.body.dmlSafe

def Routine.hasQuery (r : Routine) : Bool := r.body.hasQuery

def Routine.hasCount (r : Routine) : Bool := r.body.hasCount

/-! ## Handlers and transactions (C4)

What decides the C4 residual assumptions (`Sqldiff.Mirror.residual`). -/

/-- `s` has a handler. -/
def Stmt.hasCatch : Stmt → Bool
  | .tryCatch .. => true
  | .seq s t | .ite _ s t => s.hasCatch || t.hasCatch
  | .while _ b => b.hasCatch
  | _ => false

/-- An error can reach a handler of `s`: the body of some `tryCatch` can
fail. -/
def Stmt.catches : Stmt → Bool
  | .tryCatch _ b h => b.mayFail || b.catches || h.catches
  | .seq s t | .ite _ s t => s.catches || t.catches
  | .while _ b => b.catches
  | _ => false

/-- `s` commits. -/
def Stmt.hasCommit : Stmt → Bool
  | .commit => true
  | .seq s t | .ite _ s t | .tryCatch _ s t => s.hasCommit || t.hasCommit
  | .while _ b => b.hasCommit
  | _ => false

/-- `s` raises an error of its own (`THROW`, `RAISE EXCEPTION`). -/
def Stmt.raisesUser : Stmt → Bool
  | .raise .user => true
  | .seq s t | .ite _ s t | .tryCatch _ s t => s.raisesUser || t.raisesUser
  | .while _ b => b.raisesUser
  | _ => false

def Routine.hasCatch (r : Routine) : Bool := r.body.hasCatch
def Routine.catches (r : Routine) : Bool := r.body.catches
def Routine.hasCommit (r : Routine) : Bool := r.body.hasCommit
def Routine.raisesUser (r : Routine) : Bool := r.body.raisesUser

end Sqldiff.Core
