/-
The mirror check as a `sqldiff-check/3` request (see `Main.lean`):
decoding of its fields and the answer. A field that does not decode, a
routine that does not decode, or an abstract syntax schema other than
`Sqldiff.Ast.schemaHash` is answered "unconfirmed".

Field encodings, all ASCII without tabs; NAME is the UTF-8 bytes of a
string in lower-case hex, two digits per byte:

  DEPLOYMENT  R or R,PAIR,PAIR,...   R is 1 when row counts are observed, else 0;
                                     PAIR is NAME:NAME, a T-SQL routine name
                                     (`.`-joined) and its target routine
              then optionally        (C3a) `;` and TPAIR,TPAIR,...: TPAIR is
                                     NAME:NAME, a T-SQL table name (`.`-joined
                                     as written, `dbo.t`) and its target table
                                     (a catalog TABLE name)
  CATALOG     - or ENTRY,ENTRY,...   ENTRY is a routine, (C3a) a table, or
                                     (C2b) a fact:
                ROUTINE NAME:MODES   a target routine (folded as
                                     `Sqldiff.Mirror.pgName` folds) and one
                                     letter per parameter: i IN, o OUT, b INOUT
                        or NAME:MODES:TYPES (C2c) and the parameters'
                                     declared PostgreSQL types, `/`-separated,
                                     each the NAME of the type name then `.`
                                     and the NAME of each type argument; one
                                     per mode letter. A routine without
                                     TYPES takes no argument in the spec
                                     (`Catalog.routineTys`)
                TABLE NAME=COL/COL/... a target table (folded, `.`-joined as
                                     the PL/pgSQL code names it) and its
                                     columns in declaration order, at least one
                COL NAME.SPEC, NAME.SPEC.NAME or NAME.SPEC.NAME.DEFAULT
                                     the column (folded), its type and
                                     constraints, optionally its spelling
                                     in the T-SQL table (default: the folded
                                     name without a leading `"`), and (C3c)
                                     optionally its default
                SPEC TYPE then optionally n, then optionally k
                                     n: NOT NULL; k: a column of the primary
                                     key (NOT NULL in any case). So C3a's
                                     i, in, ik, ink decode as before
                TYPE i               `int`/`integer`
                     v DIGITS        (C3c) T-SQL `varchar(DIGITS)`, PostgreSQL
                                     `varchar(DIGITS)`
                     w DIGITS        (C3c) T-SQL `nvarchar(DIGITS)`, PostgreSQL
                                     `varchar(DIGITS)`
                                     (`CatCol.kind`; a string key column, a
                                     length outside 1..4000, decodes but the
                                     table is not usable: `CatTable.wellFormed`)
                     j               (C2c) `bigint` (PostgreSQL `bigint`/`int8`)
                     h               (C2c) `smallint` (`smallint`/`int2`)
                     b               (C2c) T-SQL `bit`, PostgreSQL `boolean`
                                     (a bit key column decodes but the table
                                     is not usable)
                     d               (C2c) `date`
                     t DIGIT         (C2c) T-SQL `datetime2(DIGIT)`, PostgreSQL
                                     `timestamp(DIGIT)`, DIGIT 0..6 (7 decodes
                                     but the table is not usable)
                DEFAULT i INT        (C3c) an integer literal, `-` allowed:
                                     `i0`, `i-1` (`Sqldiff.Mirror.intLit?`;
                                     (C2c) for a `j` column the 64-bit range,
                                     `int64Lit?`; any literal decodes, one
                                     outside its column's range makes the
                                     table unusable)
                        s HEX        (C3c) a string literal, its UTF-8 bytes
                                     as for NAME (`s` alone: the empty string)
                        b0, b1       (C2c) a bit default, T-SQL `0`/`1` and
                                     PostgreSQL `FALSE`/`TRUE`
                        d HEX        (C2c) a date literal `YYYY-MM-DD`, its
                                     bytes as for NAME (`dateLit?`)
                        t HEX        (C2c) a timestamp literal
                                     `YYYY-MM-DDThh:mm:ss[.f]` with at most
                                     six fractional digits (`tsLit?`; more
                                     digits than the column keeps make the
                                     table unusable)
                                     Both tables declare the default; DEFAULT
                                     NULL is sent as no DEFAULT (`CatCol.dflt`)
                FACT !binary-collation (C2b) `Catalog.binaryCollation`: the
                                     T-SQL database that holds the routine
                                     has a `_BIN`/`_BIN2` default collation,
                                     and every collation a PostgreSQL
                                     comparison in the routine can take (the
                                     database default, the collations of the
                                     arguments callers pass) is deterministic.
                                     Any other `!` entry does not decode
  CERT        - or PAIR,PAIR,...     PAIR is NAME:NAME, a T-SQL variable and its
                                     PL/pgSQL name

A request without `;` in DEPLOYMENT and without TABLE entries is a C1/C2a
request as before; its routines cannot have data statements in the spec.
A request without a FACT entry has no string comparison in the slice
(C2b): `=` and `<>` on strings stay uninterpreted operators. A checker
older than C2b answers a request with a FACT "the catalog does not
decode": the Rust side sends the fact only when the deployment profile
declares it, and records `ASM-BINARY-COLLATION` for every routine it
sends it with.
A TABLE entry asserts every fact `Sqldiff.Mirror.CatTable` lists (no
other constraint, default, trigger, ...): the Rust side sends a table only
when the catalogs show all of them. (C3c) A checker older than C3c
answers a request with a `v`/`w` column or a DEFAULT "the catalog does
not decode"; a C3c checker reads every older request as before. (C2c) A
checker older than C2c answers a request with a `j`, `h`, `b`, `d` or
`t` column, or a `b`/`d`/`t` DEFAULT, "the catalog does not decode"; a
C2c checker reads every older request as before.

The answer: `check`, plus the fields `extra` adds to a confirmed answer
(`"scalars"`, `"residual"` for a C2a answer, `"statements"` and
`"errors"`; see `Main.lean`), and (0.13.0) the `"why"` codes it adds to
an unconfirmed answer whose routine is outside the spec.
-/
import Sqldiff.Ast
import Sqldiff.Mirror.Check

namespace Sqldiff.Mirror.Request

open Sqldiff.Gate (Answer)

private def hexDigit (c : Char) : Option Nat :=
  if c.isDigit then some (c.toNat - '0'.toNat)
  else if 'a' ≤ c && c ≤ 'f' then some (c.toNat - 'a'.toNat + 10)
  else none

private def hexBytes : List Char → Option (List UInt8)
  | a :: b :: rest => do
    let x ← hexDigit a
    let y ← hexDigit b
    let bs ← hexBytes rest
    pure ((x * 16 + y).toUInt8 :: bs)
  | [] => some []
  | [_] => none

/-- A NAME: lower-case hex of UTF-8 bytes. -/
def name? (s : String) : Option String :=
  (hexBytes s.toList).bind fun bs => String.fromUTF8? ⟨bs.toArray⟩

/-- `-` is the empty list; otherwise comma-separated items. -/
def items? {α : Type} (s : String) (f : String → Option α) : Option (List α) :=
  if s = "-" then some [] else (s.splitOn ",").mapM f

def pair? (s : String) : Option (String × String) :=
  match s.splitOn ":" with
  | [a, b] => do pure (← name? a, ← name? b)
  | _ => none

def mode? : Char → Option Ast.Mode
  | 'i' => some .In
  | 'o' => some .Out
  | 'b' => some .InOut
  | _ => none

def deployment? (s : String) : Option Deployment := do
  let (main, tables) ← match s.splitOn ";" with
    | [m] => some (m, [])
    | [m, t] => (t.splitOn ",").mapM pair? |>.map (m, ·)
    | _ => none
  match main.splitOn "," with
  | r :: ps => do
    let rc ← match r with
      | "0" => some false
      | "1" => some true
      | _ => none
    let routines ← ps.mapM pair?
    pure { rowCounts := rc, routines, tables }
  | [] => none

/-- The flags after a column's type: `(notNull, key)`. -/
def flags? : List Char → Option (Bool × Bool)
  | [] => some (false, false)
  | ['n'] => some (true, false)
  | ['k'] => some (false, true)
  | ['n', 'k'] => some (true, true)
  | _ => none

/-- A column's type (`ColKind`) and constraints: `(kind, notNull, key)`.
`i` with the C3a flags decodes as before. -/
def spec? (s : String) : Option (ColKind × Bool × Bool) :=
  let plain (k : ColKind) (fl : List Char) : Option (ColKind × Bool × Bool) :=
    (flags? fl).map fun (n, key) => (k, n, key)
  match s.toList with
  | 'i' :: fl => plain (.int .i32) fl
  | 'j' :: fl => plain (.int .i64) fl
  | 'h' :: fl => plain (.int .i16) fl
  | 'b' :: fl => plain .bit fl
  | 'd' :: fl => plain .date fl
  | 't' :: p :: fl => if p.isDigit then plain (.ts (p.toNat - '0'.toNat)) fl else none
  | c :: rest =>
    if c == 'v' || c == 'w' then
      let ds := rest.takeWhile Char.isDigit
      if ds.isEmpty then none else do
        let len ← digitsVal ds 0
        let (n, k) ← flags? (rest.drop ds.length)
        pure (.str len (c == 'w'), n, k)
    else none
  | [] => none

/-- (C3c) A column's default: `i` and an integer literal, `s` and the hex
of a string; (C2c) `b0`/`b1`, `d` and the hex of a date literal, `t` and
the hex of a timestamp literal. -/
def dflt? (s : String) : Option Core.SExpr :=
  match s.toList with
  | 'i' :: ds => (int64Lit? (String.ofList ds)).map .int
  | 's' :: hs => (name? (String.ofList hs)).map .str
  | ['b', '0'] => some (.bool false)
  | ['b', '1'] => some (.bool true)
  | 'd' :: hs => (name? (String.ofList hs)).bind fun t => (dateLit? t).map .date
  | 't' :: hs => (name? (String.ofList hs)).bind fun t => (tsLit? 6 t).map .ts
  | _ => none

def col? (s : String) : Option CatCol := do
  let (n, sp, t, df) ← match s.splitOn "." with
    | [n, sp] => some (n, sp, none, none)
    | [n, sp, t] => some (n, sp, some t, none)
    | [n, sp, t, df] => some (n, sp, some t, some df)
    | _ => none
  let name ← name? n
  let (kind, notNull, key) ← spec? sp
  let tsql ← match t with
    | some t => name? t
    | none => some (if name.startsWith "\"" then String.ofList (name.toList.drop 1) else name)
  let dflt ← match df with
    | some df => (dflt? df).map some
    | none => some none
  pure { name, tsql, notNull, key, kind, dflt }

/-- (C2c) A callee parameter's type: the hex of its name, then `.` and the
hex of each type argument (`74494d455354414d50.36` is `TIMESTAMP(6)`). -/
def ty? (s : String) : Option Ast.Ty :=
  match s.splitOn "." with
  | n :: as => do pure (.Ty (← name? n) (← as.mapM name?))
  | [] => none

/-- (C2c) The parameter types of a routine entry, `/`-separated; the
empty text is no parameter. -/
def tys? (s : String) : Option (List Ast.Ty) :=
  if s.isEmpty then some [] else (s.splitOn "/").mapM ty?

/-- A catalog entry: a routine (with (C2c) its parameter types, when
given), a table, or (C2b) the collation fact. -/
inductive Entry where
  | routine (n : String) (modes : List Ast.Mode) (tys : Option (List Ast.Ty))
  | table (t : CatTable)
  | binaryCollation

def entry? (e : String) : Option Entry :=
  if e.startsWith "!" then
    if e == "!binary-collation" then some .binaryCollation else none
  else
  match e.splitOn "=" with
  | [n, cs] => do
    let cols ← (cs.splitOn "/").mapM col?
    pure (.table { name := ← name? n, cols })
  | [_] => match e.splitOn ":" with
    | [n, ms] => do pure (.routine (← name? n) (← ms.toList.mapM mode?) none)
    | [n, ms, ts] => do
      let modes ← ms.toList.mapM mode?
      let tys ← tys? ts
      if tys.length != modes.length then none else
      pure (.routine (← name? n) modes (some tys))
    | _ => none
  | _ => none

def catalog? (s : String) : Option Catalog :=
  (items? s entry?).map fun es =>
    { routines := es.filterMap fun | .routine n ms _ => some (n, ms) | _ => none
      routineTys := es.filterMap fun | .routine n _ (some ts) => some (n, ts) | _ => none
      tables := es.filterMap fun | .table t => some t | _ => none
      binaryCollation := es.any fun | .binaryCollation => true | _ => false }

def cert? (s : String) : Option Cert := (items? s pair?).map Cert.mk

/-- The answer to a well-formed `mirror` request. -/
def answer (schema dep cat cert ra rb : String) : Answer :=
  if schema != Sqldiff.Ast.schemaHash then
    .unconfirmed "the request's abstract syntax schema is not this checker's: generate formal/lean/Sqldiff/Ast.lean again and rebuild"
  else
    match deployment? dep, catalog? cat, cert? cert with
    | some d, some g, some c =>
      match Ast.decodeRoutineLine ra, Ast.decodeRoutineLine rb with
      | some a, some b => check d g c a b
      | none, _ => .unconfirmed "routine A does not decode"
      | _, none => .unconfirmed "routine B does not decode"
    | none, _, _ => .unconfirmed "the deployment does not decode"
    | _, none, _ => .unconfirmed "the catalog does not decode"
    | _, _, none => .unconfirmed "the certificate does not decode"

/-- What a confirmed `mirror` answer adds to the `/3` JSON line, after
`theorem` (`Main.lean`); nothing for an unconfirmed one. Fields are only
ever added, so a reader of the C1 answer keeps working.

* `"scalars"`: `"c2a"` when the answer names
  `Sqldiff.Mirror.check_sound_c2a` (both routines in the C2a slice: the
  confirmation does not rest on `ASM-C1-OPERATORS`), `"uninterpreted"`
  when it names `Sqldiff.Mirror.check_sound` (it does).
* `"residual"`, with `"c2a"` only: the facts the confirmation still
  rests on (`Sqldiff.Mirror.residual`), an array of assumption names,
  possibly empty.
* `"statements"`: `"c3b"` when the routines read a table (C3b: queries,
  with or without data statements), else `"c3a"` when they have data
  statements (C3a: the confirmation covers the target catalog's tables
  named in the request, under the facts a TABLE entry asserts), `"c1"`
  when they have only the control statements of C1. Added in C3a, last,
  so a reader of the C2a answer keeps working; `"c3b"` added in C3b.

* `"errors"`: `"c4"` when the routines handle errors, raise them or
  control transactions (C4: `TRY`/`CATCH` against an exception block,
  `THROW` against `RAISE`, `BEGIN TRAN`/`COMMIT`, the rollback-and-rethrow
  idiom, or a PL/pgSQL `COMMIT` alone, after each statement T-SQL
  commits on its own); absent otherwise. Added in C4, last, so a reader of the C3a
  answer keeps working. Its residuals (`ASM-CATCH-CORE-ERRORS`,
  `ASM-NO-CALLER-TXN`, `ASM-THROW-ERRCODE`) are in `"residual"`, so a
  reader that does not know them already does not treat the answer as a
  C2a one.

C2b adds no field: a confirmation whose routines use strings is a C2a
answer (the slice grew; the theorem is the same), and its `"residual"`
names the string facts it rests on (`ASM-CONCAT-NULL`,
`ASM-BINARY-COLLATION`, `ASM-TSQL-LEN-BMP`, `ASM-TSQL-STRING-ARGS`;
`Sqldiff.Mirror.residual`). A reader that does not know one of them must
not treat the answer as a C2a one.

* `"why"` (0.13.0), on an **unconfirmed** answer whose reason is "the
  T-SQL routine is outside the C1 spec" or "the PL/pgSQL routine is
  outside the C1 spec" only: the reason codes of that routine
  (`Sqldiff.Mirror.why`, `Sqldiff/Mirror/Diagnose.lean`; the table is in
  `README.md`, "Refusal diagnostics"), an array of at most 24 distinct
  ASCII strings `family:detail`, never empty. Untrusted and
  informational: it names the constructs the lowering refused, it is
  computed only after the answer is decided, and a reader may ignore it.
  Other unconfirmed answers carry no `"why"`. -/
def whyField (codes : List String) : String :=
  ",\"why\":[" ++ ",".intercalate (codes.map fun c => "\"" ++ c ++ "\"") ++ "]"

def extra (dep cat ra rb : String) (ans : Answer) : String :=
  match ans with
  | .unconfirmed r =>
    -- 0.13.0: the reason codes of the routine that does not lower.
    if r == reasonOutsideT || r == reasonOutsideP then
      match deployment? dep, catalog? cat, Ast.decodeRoutineLine ra, Ast.decodeRoutineLine rb with
      | some d, some g, some a, some b => whyField (why d g a b)
      | _, _, _, _ => ""
    else ""
  | .confirmed t =>
    let a := Ast.decodeRoutineLine ra
    -- The conversion's own `COMMIT`s, which lower to nothing.
    let bTxn := ((Ast.decodeRoutineLine rb).map pgTxn).getD false
    let ca := match deployment? dep, catalog? cat, a with
      | some d, some g, some a => lowerT d g a
      | _, _, _ => none
    let stmts :=
      if (ca.map Core.Routine.hasQuery).getD false then "c3b"
      else if (ca.map Core.Routine.hasData).getD false then "c3a" else "c1"
    let errs := match a, ca with
      | some a, some ca => if usesC4 a ca bTxn then ",\"errors\":\"c4\"" else ""
      | _, _ => ""
    let tail := ",\"statements\":\"" ++ stmts ++ "\"" ++ errs
    if t == theoremC2a then
      let names := match a, ca with
        | some a, some ca => residual a ca bTxn
        | _, _ => []
      let items := ",".intercalate (names.map fun n => "\"" ++ n ++ "\"")
      ",\"scalars\":\"c2a\",\"residual\":[" ++ items ++ "]" ++ tail
    else ",\"scalars\":\"uninterpreted\"" ++ tail

#guard name? "40785f31" == some "@x_1"
#guard name? "4" == none
#guard name? "4G" == none
#guard (cert? "4078:78,4079:79").map (·.vars) == some [("@x", "x"), ("@y", "y")]
#guard (cert? "-").map (·.vars) == some []
#guard (catalog? "70:ib").isSome
#guard (catalog? "70:ix").isNone
-- C2c: parameter types, `integer` and `timestamp(6)`; one per mode.
#guard (catalog? "70:ib:494e5445474552/54494d455354414d50.36").map
    (fun g => g.routineTys.map fun (n, ts) => (n, ts.map tyName)) ==
  some [("p", ["integer()", "timestamp(6)"])]
#guard (catalog? "70::").map (fun g => (g.routines.length, g.routineTys.map (·.2.length))) ==
  some (1, [0])
#guard (catalog? "70:ib:494e5445474552").isNone
#guard (catalog? "70:i:4").isNone
#guard (catalog? "70:ib").map (·.routineTys.length) == some 0
#guard (deployment? "0").map (·.rowCounts) == some false
#guard (deployment? "2").isNone
#guard (deployment? "0;64626f2e74:74").map (·.tables) == some [("dbo.t", "t")]
#guard (deployment? "0,70:70;74:74").map (·.routines) == some [("p", "p")]
#guard (deployment? "0;").isNone
#guard (deployment? "0;74:74;74:74").isNone
-- A table `t` with key `id` and nullable `qty`, spelt `Qty` in T-SQL, and a routine.
#guard (catalog? "74=6964.ink/717479.i.517479,70:ib").map
    (fun g => (g.routines.length, g.tables.map fun t => t.cols.map fun c => (c.name, c.tsql, c.notNull, c.key))) ==
  some (1, [[("id", "id", true, true), ("qty", "Qty", false, false)]])
-- A quoted target name `"Qty`: its T-SQL spelling defaults to `Qty`.
#guard (catalog? "74=22517479.in").map (fun g => g.tables.map fun t => t.cols.map (·.tsql)) ==
  some [["Qty"]]
#guard (catalog? "74=6964.ix").isNone
-- C2b: the collation fact, alone or with other entries; nothing else after `!`.
#guard (catalog? "!binary-collation").map (·.binaryCollation) == some true
#guard (catalog? "70:ib,!binary-collation").map (fun g => (g.routines.length, g.binaryCollation)) ==
  some (1, true)
#guard (catalog? "70:ib").map (·.binaryCollation) == some false
#guard (catalog? "!binary").isNone
#guard (catalog? "!").isNone
-- C3c: string columns and defaults. `name varchar(20) NOT NULL`, `city
-- nvarchar(30)` with DEFAULT 'Paris', `qty int` NOT NULL DEFAULT -1.
#guard (catalog? ("74=6964.ink/6e616d65.v20n/63697479.w30.43697479.s5061726973/" ++
    "717479.in.517479.i-1")).map
    (fun g => g.tables.map fun t => t.cols.map fun c => (c.name, c.tsql, c.notNull, c.key, c.kind,
      c.dflt)) ==
  some [[("id", "id", true, true, .int .i32, none),
    ("name", "name", true, false, .str 20 false, none),
    ("city", "City", false, false, .str 30 true, some (.str "Paris")),
    ("qty", "Qty", true, false, .int .i32, some (.int (-1)))]]
-- The line `crates/sqldiff-core/src/proof.rs` writes for the same table
-- (its test `a_mirror_request_encodes_tables_as_the_checker_reads_them`).
#guard (catalog? ("70:ib,61636374=6964.ink.6964/6e616d65.v20n.6e616d65/" ++
    "63697479.w30.43697479.s5061726973/717479.in.517479.i-1")).map
    (fun g => g.tables.map fun t => (t.wellFormed, t.cols.map fun c => (c.name, c.kind, c.dflt))) ==
  some [(true, [("id", .int .i32, none), ("name", .str 20 false, none),
    ("city", .str 30 true, some (.str "Paris")), ("qty", .int .i32, some (.int (-1)))])]
-- The empty string as a default; a string key decodes (and is refused by `wellFormed`).
#guard (catalog? "74=63.v5.63.s").map (fun g => g.tables.map fun t => t.cols.map (·.dflt)) ==
  some [[some (.str "")]]
#guard (catalog? "74=63.v5k").map (fun g => g.tables.map fun t => t.wellFormed) == some [false]
#guard (catalog? "74=6964.ik/63.v5n").map (fun g => g.tables.map fun t => t.wellFormed) == some [true]
#guard (catalog? "74=63.v").isNone
#guard (catalog? "74=63.vn").isNone
#guard (catalog? "74=63.v5x").isNone
#guard (catalog? "74=63.x5").isNone
-- A 32-bit column with a bigint-sized default decodes; the table is unusable.
#guard (catalog? "74=63.i.63.i2147483648").map (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=63.j.63.i2147483648").map (fun g => g.tables.map (·.wellFormed)) == some [true]
#guard (catalog? "74=63.i.63.i9223372036854775808").isNone
#guard (catalog? "74=63.i.63.x1").isNone
#guard (catalog? "74=63.i.63.s4").isNone
#guard (catalog? "74=63.i.63.i1.0").isNone
-- A default of the wrong kind, too long, or out of the slice decodes; the table is unusable.
#guard (catalog? "74=63.i.63.s61").map (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=63.v2.63.s616263").map (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=63.v3.63.s616263").map (fun g => g.tables.map (·.wellFormed)) == some [true]
#guard (catalog? "74=63.v3.63.i1").map (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=63.v3.63.s5c").map (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=63.v0").map (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=63.v4001").map (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=63.w4000").map (fun g => g.tables.map (·.wellFormed)) == some [true]
#guard (catalog? "74=").isNone
#guard (catalog? "74=6964").isNone
-- C2c: typed columns. `d date PRIMARY KEY`, `n bigint`, `s smallint NOT NULL
-- DEFAULT 0`, `b bit NOT NULL DEFAULT 0`, `t datetime2(6) DEFAULT
-- '2000-01-01T00:00:00'` (eval/values V8), as proof.rs writes them.
#guard (catalog? ("6576=64.dnk.64/6e.j.6e/73.hn.73.i0/62.bn.62.b0/" ++
    "74.t6.74.t323030302d30312d30315430303a30303a3030")).map
    (fun g => g.tables.map fun t => (t.wellFormed, t.cols.map fun c => (c.name, c.kind, c.dflt))) ==
  some [(true, [("d", .date, none), ("n", .int .i64, none), ("s", .int .i16, some (.int 0)),
    ("b", .bit, some (.bool false)),
    ("t", .ts 6, some (.ts (Core.Calendar.daysFromCivil 2000 1 1 * Core.Calendar.usPerDay)))])]
-- A date default (`'2024-02-29'`), and the unusable ones: a bit key, a
-- `datetime2(7)`, a smallint default out of range, a default of another
-- kind, a timestamp default with more digits than the column keeps.
#guard (catalog? "74=64.d.64.d323032342d30322d3239").map
    (fun g => g.tables.map fun t => (t.wellFormed, t.cols.map (·.dflt))) ==
  some [(true, [some (.date (Core.Calendar.daysFromCivil 2024 2 29))])]
#guard (catalog? "74=62.bnk").map (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=74.t7").map (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=73.h.73.i40000").map (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=73.h.73.i-32768").map (fun g => g.tables.map (·.wellFormed)) == some [true]
#guard (catalog? "74=62.b.62.i1").map (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=64.d.64.s32303234").map (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=74.t3.74.t323032342d30312d30315430303a30303a30302e31323334").map
    (fun g => g.tables.map (·.wellFormed)) == some [false]
#guard (catalog? "74=74.t3.74.t323032342d30312d30315430303a30303a30302e313233").map
    (fun g => g.tables.map (·.wellFormed)) == some [true]
-- Malformed: a non-ISO date default, a bit default other than 0/1, no digit after `t`.
#guard (catalog? "74=64.d.64.d32303234").isNone
#guard (catalog? "74=62.b.62.b2").isNone
#guard (catalog? "74=74.t").isNone
#guard (catalog? "74=74.tx").isNone

end Sqldiff.Mirror.Request
