/-
Checked examples for C2b, strings: confirmed and refused pairs, the
residual assumptions each confirmation names, and runs of the core
interpreter showing NULL against the empty string, concatenation, T-SQL's
trailing-space padding against PostgreSQL's exact comparison, `LEN`
against `length`, and why `ISNULL` must not truncate. Every `#guard` runs
on every `lake build`.
-/
import Sqldiff.Mirror.Examples

namespace Sqldiff.Mirror.ExamplesC2b

open Sqldiff.Core (SExpr)
open Sqldiff.Mirror.Examples
open Sqldiff.Gate (Answer)

/-! ## Building blocks -/

def vc (n : Nat) : Ast.Ty := .Ty "VARCHAR" [toString n]
def nvc (n : Nat) : Ast.Ty := .Ty "NVARCHAR" [toString n]
def text : Ast.Ty := .Ty "TEXT" []
def S (s : String) : V := .str s
def str (s : String) : Ast.Expr := .Str s
def tr : Ast.Expr := tv "@r"
def pr : Ast.Expr := pv "r"

/-- The target catalog with the collation fact (`Catalog.binaryCollation`). -/
def Γb : Catalog := { routines := [], binaryCollation := true }

def concatNull : Ast.Node := .Setting "CONCAT_NULL_YIELDS_NULL" true

/-- T-SQL `r @a, @b, @r OUTPUT`: `@a` and `@b` of type `ta`, `@r` of type
`trt`. PL/pgSQL `r(a, b, INOUT r)`: `a` and `b` of type `pat`, `r` of
type `prt`. -/
def sT (body : List Ast.Node) (ta : Ast.Ty := vc 10) (trt : Ast.Ty := vc 20) : Ast.Routine :=
  tsqlProc ["r"] [inT "@a" ta, inT "@b" ta, ioT "@r" trt] body
def sP (body : List Ast.Node) (decls : List Ast.Stmt := []) (pat : Ast.Ty := text)
    (prt : Ast.Ty := text) : Ast.Routine :=
  pgProc ["r"] [inP "a" pat, inP "b" pat, ioP "r" prt] decls body

def cs (g : Catalog) (a b : Ast.Routine) : Answer := check dep g pairCert a b
def runT (g : Catalog) (a : Ast.Routine) (args : List V) := run (lowerT dep g a) args 30
def runP (g : Catalog) (b : Ast.Routine) (args : List V) := run (lowerP g b) args 30

/-! ## Concatenation

```sql
CREATE PROCEDURE r @a varchar(10), @b varchar(10), @r varchar(20) OUTPUT
AS SET @r = @a + @b;
-- PostgreSQL
CREATE PROCEDURE r(a text, b text, INOUT r text) ... r := a || b;
```

`@a + @b` has at most 20 bytes, so `@r` keeps it whole, and `text`
takes any string. -/

def catT : Ast.Routine := sT [set tr (.Bin "+" ta tb)]
def catP : Ast.Routine := sP [set pr (.Bin "||" pa pb)]

#guard isC2a (cs ∅ catT catP)
#guard residualOf dep ∅ catT == some ["ASM-CONCAT-NULL", "ASM-TSQL-STRING-ARGS"]
#guard runT ∅ catT [S "ab", S "cd", N] == some (.done [S "abcd", I 0] none 0 [] [])
#guard runP ∅ catP [S "ab", S "cd", N] == runT ∅ catT [S "ab", S "cd", N]
-- NULL in either operand: NULL (`CONCAT_NULL_YIELDS_NULL ON`).
#guard runT ∅ catT [N, S "cd", S "x"] == some (.done [N, I 0] none 0 [] [])
#guard runP ∅ catP [N, S "cd", S "x"] == runT ∅ catT [N, S "cd", S "x"]
-- The empty string is not NULL.
#guard runT ∅ catT [S "", S "", N] == some (.done [S "", I 0] none 0 [] [])
#guard runP ∅ catP [S "", S "", N] == runT ∅ catT [S "", S "", N]
-- `SET CONCAT_NULL_YIELDS_NULL ON` first discharges `ASM-CONCAT-NULL`.
def catOnT : Ast.Routine := sT [concatNull, set tr (.Bin "+" ta tb)]
#guard isC2a (cs ∅ catOnT catP)
#guard residualOf dep ∅ catOnT == some ["ASM-TSQL-STRING-ARGS"]
-- Not first: it does not cover what runs before it.
def catLateT : Ast.Routine := sT [set tr (.Bin "+" ta tb), concatNull]
#guard residualOf dep ∅ catLateT == some ["ASM-CONCAT-NULL", "ASM-TSQL-STRING-ARGS"]
-- `OFF` (NULL is the empty string) is outside the spec.
#guard reasonOf (cs ∅ (sT [.Setting "CONCAT_NULL_YIELDS_NULL" false, set tr (.Bin "+" ta tb)])
    catP) == "the T-SQL routine is outside the C1 spec"

/-! ### Refused or outside the slice -/

-- `@r varchar(15)` cannot hold 20 bytes: T-SQL would truncate where
-- PostgreSQL keeps (or, into `varchar(15)`, raises). Outside the spec
-- (0.9.0, "String stores, on every path"); before, a C1 answer, which was
-- wrong for long arguments.
#guard reasonOf (cs ∅ (sT [set tr (.Bin "+" ta tb)] (trt := vc 15)) catP) == "the T-SQL routine is outside the C1 spec"
-- A PostgreSQL parameter carries no length, so `a || b` has no bound and
-- is not stored in a parameter declared `varchar(20)` (held to its
-- declared length in the slice, to be safe either way): a C1 answer. On
-- the C1 path the store is fine: PostgreSQL keeps no length on a
-- parameter (probed, "String stores, on every path").
#guard isC1 (cs ∅ catT (sP [set pr (.Bin "||" pa pb)] (prt := .Ty "VARCHAR" ["20"])))
-- `varchar` without a length is `text` in PostgreSQL.
#guard isC2a (cs ∅ catT (sP [set pr (.Bin "||" pa pb)] (prt := .Ty "VARCHAR" [])))
-- Over 4000 the T-SQL `+` is outside the slice (8000-byte truncation):
-- `+` and `||` stay two operators, and a value outside the slice is not
-- stored in a bounded string (0.9.0).
#guard reasonOf (cs ∅ (sT [set tr (.Bin "+" ta tb)] (ta := nvc 4000) (trt := nvc 4000)) catP) ==
  "the T-SQL routine is outside the C1 spec"
-- `nvarchar(2000) + nvarchar(2000)` is 4000: in.
#guard isC2a (cs ∅ (sT [set tr (.Bin "+" ta tb)] (ta := nvc 2000) (trt := nvc 4000)) catP)
-- `max` types are outside: not even a parameter type the check pairs with
-- `text` (and `+` would stay an operator, whose value is not stored in a
-- `varchar(max)`: it might not fit the code page).
#guard reasonOf (cs ∅ (sT [set tr (.Bin "+" ta tb)] (ta := .Ty "VARCHAR" ["MAX"])
  (trt := .Ty "VARCHAR" ["MAX"])) catP) == "the T-SQL routine is outside the C1 spec"
-- `nvarchar(max)` takes any value, so the store is fine; the types are not.
#guard reasonOf (cs ∅ (sT [set tr ta] (ta := .Ty "VARCHAR" ["MAX"])
  (trt := .Ty "NVARCHAR" ["MAX"])) (sP [set pr pa])) == "the parameter or return types do not correspond"
-- T-SQL `varchar` without a length (1 in a declaration) is outside.
#guard (strTypeT (.Ty "VARCHAR" [])).isNone
#guard strTypeT (.Ty "VARCHAR" ["4000"]) == some (some 4000, false)
#guard (strTypeT (.Ty "VARCHAR" ["4001"])).isNone
#guard (strTypeT (.Ty "CHAR" ["10"])).isNone
#guard (strTypeP (.Ty "CHAR" ["10"])).isNone
#guard strTypeP (.Ty "CHARACTER VARYING" ["10"]) == some (some 10, false)
-- PostgreSQL `+` on strings is not concatenation.
#guard reasonOf (cs ∅ catT (sP [set pr (.Bin "+" pa pb)])) ==
  "the lowered routines differ beyond the renaming"
-- T-SQL `||` (SQL Server 2025) is outside the slice.
#guard (typed (.tsql dep ∅) [("@a", vc 10), ("@b", vc 10)] {} (.Bin "||" ta tb)).isNone
-- A NULL literal operand: outside.
#guard (typed (.tsql dep ∅) [("@a", vc 10)] {} (.Bin "+" ta .Null)).isNone
-- A string and an integer: an implicit conversion, outside.
#guard (typed (.tsql dep ∅) [("@a", vc 10), ("@i", int)] {} (.Bin "+" ta (tv "@i"))).isNone

/-! ## Assignment and literals -/

-- `DECLARE @x varchar(5) = 'abc'` and `x varchar(5) := 'abc'`.
def litT : Ast.Routine :=
  tsqlProc ["r"] [inT "@a" (vc 10), inT "@b" (vc 10), ioT "@r" (vc 20)]
    [.NStmt (.Declare "@x" (some (vc 5)) (some (str "abc"))), set tr (.Bin "+" (tv "@x") ta)]
def litP : Ast.Routine :=
  sP [set pr (.Bin "||" (pv "x") pa)] [.Declare "x" (some (.Ty "VARCHAR" ["5"])) (some (str "abc"))]
def litCert : Cert := ⟨pairCert.vars ++ [("@x", "x")]⟩
#guard isC2a (check dep ∅ litCert litT litP)
#guard run (lowerT dep ∅ litT) [S "d", N, N] 30 == some (.done [S "abcd", I 0] none 0 [] [])
#guard run (lowerP ∅ litP) [S "d", N, N] 30 == run (lowerT dep ∅ litT) [S "d", N, N] 30
/- `'abcdef'` into `varchar(3)`: T-SQL gives `abc` (probed, eval/casts/
assign), PostgreSQL raises (probed). Outside the slice on both sides. -/
def longT : Ast.Routine :=
  tsqlProc ["r"] [] [.NStmt (.Declare "@x" (some (vc 3)) (some (str "abcdef")))]
def longP : Ast.Routine :=
  pgProc ["r"] [] [.Declare "x" (some (.Ty "VARCHAR" ["3"])) (some (str "abcdef"))] []
#guard !sliceT dep ∅ longT
#guard !sliceP ∅ longP
-- An `nvarchar` value is not stored in a `varchar` (characters outside
-- the code page would be replaced); the other way round is fine.
#guard !sliceT dep ∅ (sT [set tr ta] (ta := nvc 10) (trt := vc 20))
#guard sliceT dep ∅ (sT [set tr ta] (ta := vc 10) (trt := nvc 20))
-- Literals: printable ASCII without a backslash.
#guard strLitOk "it''s" && strLitOk "" && strLitOk "a b~"
#guard !strLitOk "é" && !strLitOk "a\\b" && !strLitOk "tab\t"
-- A non-ASCII literal stays an uninterpreted literal form (`str:é`), with
-- no bound or width the check knows: not stored in a `varchar` (0.9.0;
-- before, a C1 answer), but stored in a `text`.
#guard reasonOf (cs ∅ (sT [set tr (str "é")]) (sP [set pr (str "é")])) == "the T-SQL routine is outside the C1 spec"
#guard (lowerP ∅ (sP [set pr (str "é")])).isSome
-- A string is never stored in an integer, nor an integer in a string.
#guard !sliceT dep ∅ (tsqlProc ["r"] [ioT "@r"] [set tr (str "5")])
#guard !sliceT dep ∅ (sT [set tr (num 5)])

/-! ## `ISNULL` against `COALESCE`

`ISNULL(x, y)` has `x`'s type: a longer `y` is cut to `x`'s length. -/

-- `ISNULL(@a, 'none')`, `@a varchar(10)`: `'none'` fits, so it is `COALESCE`.
#guard isC2a (cs ∅ (sT [set tr (fn "ISNULL" [ta, str "none"])])
  (sP [set pr (fn "COALESCE" [pa, str "none"])]))
#guard isC2a (cs ∅ (sT [set tr (fn "ISNULL" [ta, tb])])
  (sP [set pr (fn "COALESCE" [pa, pb])]))
#guard runT ∅ (sT [set tr (fn "ISNULL" [ta, str ""])]) [N, N, N] ==
  some (.done [S "", I 0] none 0 [] [])
/- `ISNULL(@a, 'abcdef')` with `@a varchar(3)` is `abc` when `@a` is
NULL, `COALESCE` gives `abcdef`: outside the slice, so `ISNULL` and
`COALESCE` stay two operators, and the T-SQL value, outside the slice, is
not stored in `@r varchar(20)` (0.9.0; before, refused as differing). -/
#guard reasonOf (cs ∅ (sT [set tr (fn "ISNULL" [ta, str "abcdef"])] (ta := vc 3))
    (sP [set pr (fn "COALESCE" [pa, str "abcdef"])])) ==
  "the T-SQL routine is outside the C1 spec"
-- `@b varchar(10)` into `@a varchar(5)`: may not fit.
#guard (typed (.tsql dep ∅) [("@a", vc 5), ("@b", vc 10)] {} (fn "ISNULL" [ta, tb])).isNone
-- `nvarchar` into `varchar`: outside; `varchar` into `nvarchar`: in.
#guard (typed (.tsql dep ∅) [("@a", vc 10), ("@b", nvc 5)] {} (fn "ISNULL" [ta, tb])).isNone
#guard (typed (.tsql dep ∅) [("@a", nvc 10), ("@b", vc 5)] {} (fn "ISNULL" [ta, tb])).isSome
-- The first argument must be a variable: an expression's type length is
-- not taken on trust.
#guard (typed (.tsql dep ∅) [("@a", vc 10), ("@b", vc 10)] {}
  (fn "ISNULL" [.Bin "+" ta tb, str "x"])).isNone
-- `COALESCE` and `CASE` over strings.
#guard isC2a (cs ∅ (sT [set tr (fn "COALESCE" [ta, tb, str ""])])
  (sP [set pr (fn "COALESCE" [pa, pb, str ""])]))
#guard isC2a (cs ∅ (sT [set tr (.Case none [.When2 (.IsNull ta false) (str "none")] (some ta))])
  (sP [set pr (.Case none [.When2 (.IsNull pa false) (str "none")] (some pa))]))
-- A string and an integer do not go together.
#guard (typed (.tsql dep ∅) [("@a", vc 10)] {} (fn "COALESCE" [ta, num 0])).isNone

/-! ## Comparison

```sql
IF @a = @b SET @r = 'eq' ELSE SET @r = 'ne';
-- PostgreSQL, the provable form
IF rtrim(a) = rtrim(b) THEN r := 'eq'; ELSE r := 'ne'; END IF;
```

Only with the collation fact. T-SQL pads with spaces, so `'x' = 'x '` is
TRUE; PostgreSQL compares `text` exactly. -/

def eqT : Ast.Routine := sT [.If (.Bin "=" ta tb) [set tr (str "eq")] [set tr (str "ne")]]
def eqP (c : Ast.Expr) : Ast.Routine := sP [.If c [set pr (str "eq")] [set pr (str "ne")]]
def rt (e : Ast.Expr) : Ast.Expr := fn "rtrim" [e]

#guard isC2a (cs Γb eqT (eqP (.Bin "=" (rt pa) (rt pb))))
#guard residualOf dep Γb eqT ==
  some ["ASM-TSQL-ANSI-NULLS", "ASM-BINARY-COLLATION", "ASM-TSQL-STRING-ARGS"]
#guard runT Γb eqT [S "x", S "x  ", N] == some (.done [S "eq", I 0] none 0 [] [])
#guard runP Γb (eqP (.Bin "=" (rt pa) (rt pb))) [S "x", S "x  ", N] ==
  runT Γb eqT [S "x", S "x  ", N]
-- Plain `a = b` is not the T-SQL comparison: refused, and it does differ.
#guard reasonOf (cs Γb eqT (eqP (.Bin "=" pa pb))) ==
  "the lowered routines differ beyond the renaming"
#guard runP Γb (eqP (.Bin "=" pa pb)) [S "x", S "x  ", N] == some (.done [S "ne", I 0] none 0 [] [])
-- Leading spaces and tabs count on both sides.
#guard runT Γb eqT [S " x", S "x", N] == some (.done [S "ne", I 0] none 0 [] [])
#guard runT Γb eqT [S "x\t", S "x", N] == some (.done [S "ne", I 0] none 0 [] [])
-- Case counts (a binary collation).
#guard runT Γb eqT [S "X", S "x", N] == some (.done [S "ne", I 0] none 0 [] [])
-- NULL: UNKNOWN, the `ELSE` branch; the empty string equals spaces.
#guard runT Γb eqT [N, N, N] == some (.done [S "ne", I 0] none 0 [] [])
#guard runT Γb eqT [S "", S "   ", N] == some (.done [S "eq", I 0] none 0 [] [])
-- A literal is trimmed when the pair is lowered: `@a = 'x '` is
-- `rtrim(a) = 'x'`.
#guard isC2a (cs Γb (sT [.If (.Bin "=" ta (str "x ")) [set tr (str "eq")] []])
  (sP [.If (.Bin "=" (rt pa) (str "x")) [set pr (str "eq")] []]))
-- `<>` is `NOT =`.
#guard isC2a (cs Γb (sT [.If (.Bin "<>" ta tb) [set tr (str "ne")] []])
  (sP [.If (.Bin "<>" (rt pa) (rt pb)) [set pr (str "ne")] []]))
/- Without the fact, string `=` is not in the slice: T-SQL's and
PostgreSQL's `=` stay one uninterpreted operator, so the plain pair is a
C1 mirror only (under `ASM-C1-OPERATORS`, which no deployment can
accept), and the `rtrim` form is refused. -/
#guard isC1 (cs ∅ eqT (eqP (.Bin "=" pa pb)))
#guard reasonOf (cs ∅ eqT (eqP (.Bin "=" (rt pa) (rt pb)))) ==
  "the lowered routines differ beyond the renaming"
-- `<` and `LIKE` stay outside even with the fact.
#guard (typed (.tsql dep Γb) [("@a", vc 10), ("@b", vc 10)] {} (.Bin "<" ta tb)).isNone
#guard isC1 (cs Γb (sT [.If (.Like ta tb false false) [set tr (str "eq")] []])
  (sP [.If (.Like pa pb false false) [set pr (str "eq")] []]))
-- `IS NULL` of a string needs no fact.
#guard isC2a (cs ∅ (sT [.If (.IsNull ta false) [set tr (str "null")] []])
  (sP [.If (.IsNull pa false) [set pr (str "null")] []]))

/-! ## `LEN` against `length`

T-SQL `LEN` leaves out trailing spaces, PostgreSQL `length` does not:
`LEN(@a)` is `length(rtrim(a))`. -/

def lenT : Ast.Routine :=
  tsqlProc ["r"] [inT "@a" (vc 10), inT "@b" (vc 10), ioT "@r" int] [set tr (fn "LEN" [ta])]
def lenP (e : Ast.Expr) : Ast.Routine :=
  pgProc ["r"] [inP "a" text, inP "b" text, ioP "r" integer] [] [set pr e]

#guard isC2a (cs ∅ lenT (lenP (fn "length" [rt pa])))
#guard isC2a (cs ∅ lenT (lenP (fn "char_length" [rt pa])))
#guard residualOf dep ∅ lenT == some ["ASM-TSQL-LEN-BMP", "ASM-TSQL-STRING-ARGS"]
#guard runT ∅ lenT [S "ab  ", N, N] == some (.done [I 2, I 0] none 0 [] [])
#guard runP ∅ (lenP (fn "length" [rt pa])) [S "ab  ", N, N] == runT ∅ lenT [S "ab  ", N, N]
#guard runT ∅ lenT [S "  ", N, N] == some (.done [I 0, I 0] none 0 [] [])
#guard runT ∅ lenT [N, N, N] == some (.done [N, I 0] none 0 [] [])
-- `length(a)` counts the trailing spaces: refused, and it differs.
#guard reasonOf (cs ∅ lenT (lenP (fn "length" [pa]))) ==
  "the lowered routines differ beyond the renaming"
#guard runP ∅ (lenP (fn "length" [pa])) [S "ab  ", N, N] == some (.done [I 4, I 0] none 0 [] [])

/-! ## Functions, calls and parameters -/

-- A function returning `varchar(10)`: `@a + 'x'` has at most 6 bytes.
def fT (ret : Ast.Ty) : Ast.Routine :=
  .Routine .Function ["f"] [inT "@a" (vc 5)] (some ret) .Caller none
    (.Block [] [.NStmt (.Return (some (.Bin "+" ta (str "x"))))])
def fP : Ast.Routine :=
  .Routine .Function ["f"] [inP "a" (vc 5)] (some text) .Caller none
    (.Block [] [.NStmt (.Return (some (.Bin "||" pa (str "x"))))])
#guard isC2a (check dep ∅ fnCert (fT (vc 10)) fP)
-- Returning `varchar(5)`: T-SQL would truncate. Outside the spec (0.9.0;
-- before, a C1 answer, which was wrong).
#guard reasonOf (check dep ∅ fnCert (fT (vc 5)) fP) == "the T-SQL routine is outside the C1 spec"
-- A string passed to a call, or an `OUTPUT` argument into a string
-- variable, is outside the slice: the conversions at the callee's
-- boundary differ between the dialects.
#guard !sliceT dep Γ (sT [.Call ["dbo", "log_value"] [arg ta false, arg tr true] none])
-- A parameter default must fit too.
#guard !sliceT dep ∅ (tsqlProc ["r"] [.Param "@a" (vc 3) .In (some (str "abcdef"))] [])
#guard sliceT dep ∅ (tsqlProc ["r"] [.Param "@a" (vc 3) .In (some (str "abc"))] [])

/-! ## Parameter and return types, parameter defaults

T-SQL `f(@a varchar(10)) RETURNS varchar(10)` against PL/pgSQL
`f(a integer) RETURNS integer`: the bodies lower alike (`RETURN @a`), but
a caller passing `'x'` gets `'x'` from one and an error from the other.
The check compares the declared types (`sigCorr`). -/

def sigT : Ast.Routine :=
  .Routine .Function ["dbo", "f"] [inT "@a" (vc 10)] (some (vc 10)) .Caller none
    (.Block [] [ret (some ta)])
def sigP (pt rt : Ast.Ty) : Ast.Routine :=
  .Routine .Function ["f"] [inP "a" pt] (some rt) .Caller none (.Block [] [ret (some pa)])
#guard reasonOf (check dep ∅ fnCert sigT (sigP integer integer)) ==
  "the parameter or return types do not correspond"
-- One of the two differing is enough.
#guard reasonOf (check dep ∅ fnCert sigT (sigP (vc 10) integer)) ==
  "the parameter or return types do not correspond"
#guard reasonOf (check dep ∅ fnCert sigT (sigP integer text)) ==
  "the parameter or return types do not correspond"
-- String types of the slice correspond whatever their lengths (PostgreSQL
-- drops them on parameters and results). With `varchar(10)` on the
-- PostgreSQL side the parameter is read unbounded, so `RETURN a` is not
-- known to fit the declared result in the slice: a C1 answer (PostgreSQL
-- keeps no length on a result, probed); with `text`, C2a.
#guard isC1 (check dep ∅ fnCert sigT (sigP (vc 10) (vc 10)))
#guard isC2a (check dep ∅ fnCert sigT (sigP text text))
#guard tyCorr (nvc 10) (vc 10) && tyCorr (vc 10) (.Ty "CHARACTER VARYING" ["20"])
#guard tyCorr int (.Ty "INT4" []) && tyCorr (.Ty "INTEGER" []) integer
-- (C2c) `bigint` and `smallint` with themselves, `bit` with `boolean`,
-- `date`, and `datetime2(6)` with `timestamp(6)` or `timestamp`.
#guard tyCorr bigint (.Ty "INT8" []) && tyCorr (.Ty "SMALLINT" []) (.Ty "INT2" [])
#guard !tyCorr int bigint && !tyCorr bigint int && !tyCorr (.Ty "TINYINT" []) (.Ty "SMALLINT" [])
#guard !tyCorr (.Ty "DECIMAL" ["10", "2"]) (.Ty "NUMERIC" ["10", "2"])
#guard tyCorr (.Ty "BIT" []) (.Ty "BOOLEAN" []) && tyCorr (.Ty "DATE" []) (.Ty "DATE" [])
#guard tyCorr (.Ty "DATETIME2" ["6"]) (.Ty "TIMESTAMP" ["6"]) &&
  tyCorr (.Ty "DATETIME2" ["6"]) (.Ty "TIMESTAMP" [])
#guard !tyCorr (.Ty "DATETIME2" ["3"]) (.Ty "TIMESTAMP" ["3"]) &&
  !tyCorr (.Ty "DATETIME2" []) (.Ty "TIMESTAMP" []) && !tyCorr (.Ty "BIT" []) (.Ty "INT2" [])
#guard !tyCorr (.Ty "VARCHAR" ["MAX"]) text && !tyCorr (.Ty "CHAR" ["10"]) (.Ty "CHAR" ["10"])
-- A procedure's trailing `return_code` must be the integer type.
#guard reasonOf (check dep Γ cert srcT (match mirrorP with
    | .Routine k n ps r s v b =>
      .Routine k n (ps.dropLast ++ [.Param "return_code" text .InOut (some .Null)]) r s v b)) ==
  "the parameter or return types do not correspond"

/-! A default T-SQL truncates and PostgreSQL keeps: `@s varchar(3) =
'abcdef'` is `'abc'` in the T-SQL body, while PostgreSQL drops the
parameter's length and keeps all six characters. Outside the spec on both
sides, also on the C1 path (`dfltOk`). -/

def dfltT (d : Ast.Expr) : Ast.Routine :=
  tsqlProc ["dbo", "pd"] [ioT "@r" (vc 10), .Param "@s" (vc 3) .In (some d)] [set tr (tv "@s")]
def dfltP (d : Ast.Expr) : Ast.Routine :=
  pgProc ["pd"] [ioP "r" text, .Param "s" (vc 3) .In (some d)] [] [set pr (pv "s")]
def dfltCert : Cert := ⟨[("@r", "r"), ("@s", "s"), (statusVar, statusVar)]⟩
#guard reasonOf (check dep ∅ dfltCert (dfltT (str "abcdef")) (dfltP (str "abcdef"))) ==
  "the T-SQL routine is outside the C1 spec"
#guard (lowerP ∅ (dfltP (str "abcdef"))).isNone
-- A default that fits, and NULL, are in the slice.
#guard isC2a (check dep ∅ dfltCert (dfltT (str "abc")) (dfltP (str "abc")))
#guard isC2a (check dep ∅ dfltCert (dfltT .Null) (dfltP .Null))
-- A default outside the slice (an integer into a string, a non-slice
-- literal) is outside the spec, even on the C1 path.
#guard (lowerT dep ∅ (dfltT (num 7))).isNone
-- (C2c) A `bigint` default beyond the `int` range is the integer it spells.
#guard (lowerT dep ∅ (tsqlProc ["dbo", "pd"] [.Param "@n" bigint .In (some (.Num "2147483648"))] [])).isSome
#guard (lowerT dep ∅ (tsqlProc ["dbo", "pd"] [.Param "@n" bigint .In (some (.Num "9223372036854775808"))] [])).isNone
#guard (lowerT dep ∅ (tsqlProc ["dbo", "pd"] [.Param "@n" int .In (some (.Num "2147483648"))] [])).isNone
#guard (lowerT dep ∅ (tsqlProc ["dbo", "pd"] [.Param "@n" bigint .In (some .Null)] [])).isSome

/-! ## The core's string operators (`SExpr.eval`) -/

def ev2 (s : SExpr) (a b : V) : Core.Res Unit := s.eval (σ2 a b) Core.nullStore
#guard Core.rtrimSp "ab  " == "ab"
#guard Core.rtrimSp "  " == ""
#guard Core.rtrimSp " a\t" == " a\t"
#guard ev2 (.concat (.var "a") (.var "b")) (S "a") (S "b") == .ok (S "ab")
#guard ev2 (.concat (.var "a") (.var "b")) (S "a") N == .ok N
#guard ev2 (.streq (.var "a") (.var "b")) (S "a") (S "a ") == .ok (.bool false)
#guard ev2 (.streq (mkRtrim (.var "a")) (mkRtrim (.var "b"))) (S "a") (S "a ") == .ok (.bool true)
#guard ev2 (.streq (.var "a") (.var "b")) N (S "a") == .ok N
#guard ev2 (.len (.var "a")) (S "héé") N == .ok (I 3)
#guard ev2 (.isNull (.var "a") false) (S "") N == .ok (.bool false)
-- `mkRtrim` folds literals and repeated trims (`mkRtrim_eval`).
#guard mkRtrim (.str "x  ") == .str "x"
#guard mkRtrim (mkRtrim (.var "a")) == .rtrim (.var "a")

/-! ## String stores on the C1 path (0.9.0)

Outside the slice a store used to be lowered as it is, though T-SQL
truncates and PostgreSQL raises or keeps. Now every store into a target
that may hold a string must provably keep its value (`storeOk`), on every
path. One guard per kind of store. -/

def tOut : String := "the T-SQL routine is outside the C1 spec"
def pOut : String := "the PL/pgSQL routine is outside the C1 spec"
/-- The pair with a C1 operator elsewhere (`UPPER` into an `int`
nowhere: the body below is what matters). -/
def c1Pair (t : List Ast.Node) (p : List Ast.Node) : Answer := cs ∅ (sT t) (sP p)

-- `SET`: a literal longer than `@r varchar(20)` (T-SQL truncates).
def long26 : Ast.Expr := str "abcdefghijklmnopqrstuvwxyz"
#guard reasonOf (c1Pair [set tr long26] [set pr long26]) == tOut
-- `SET`: a value outside the slice (`UPPER(@a)`) has no known bound.
#guard reasonOf (c1Pair [set tr (fn "UPPER" [ta])] [set pr (fn "UPPER" [pa])]) == tOut
-- ... but into `@x nvarchar(max)` or `r text` it is kept.
#guard (lowerT dep ∅ (sT [.NStmt (.Declare "@x" (some (.Ty "NVARCHAR" ["MAX"])) none),
  set (tv "@x") (fn "UPPER" [ta])])).isSome
#guard (lowerP ∅ (sP [set pr (fn "UPPER" [pa])])).isSome
-- `varchar(max)` keeps any length but not every character: outside.
#guard (lowerT dep ∅ (sT [.NStmt (.Declare "@x" (some (.Ty "VARCHAR" ["MAX"])) none),
  set (tv "@x") (fn "UPPER" [ta])])).isNone
-- `SET`: a literal that fits is still in (here the C2a slice).
#guard isC2a (c1Pair [set tr (str "abc")] [set pr (str "abc")])
-- PL/pgSQL `x varchar(3)`: `x := 'abcdef'` raises in PostgreSQL.
#guard (lowerP ∅ (sP [set (pv "x") (str "abcdef")] [.Declare "x" (some (vc 3)) none])).isNone
-- An integer into a bounded string converts: outside.
#guard reasonOf (c1Pair [set tr (num 12345)] [set pr (num 12345)]) == tOut

-- `DECLARE @x varchar(3) = 'abcdef'` (T-SQL gives `abc`) and
-- `x varchar(3) := 'abcdef'` (PostgreSQL raises).
#guard (lowerT dep ∅ (sT [.NStmt (.Declare "@x" (some (vc 3)) (some (str "abcdef")))])).isNone
#guard (lowerP ∅ (sP [] [.Declare "x" (some (vc 3)) (some (str "abcdef"))])).isNone
#guard (lowerT dep ∅ (sT [.NStmt (.Declare "@x" (some (vc 3)) (some (str "abc")))])).isSome
-- `char(n)` pads, and `sysname` or an alias type may be any string: a
-- string store there is outside, NULL is not.
#guard (lowerT dep ∅ (sT [.NStmt (.Declare "@x" (some (.Ty "CHAR" ["5"])) (some (str "a")))])).isNone
#guard (lowerT dep ∅ (sT [.NStmt (.Declare "@x" (some (.Ty "SYSNAME" [])) (some (str "a")))])).isNone
#guard (lowerT dep ∅ (sT [.NStmt (.Declare "@x" (some (.Ty "dbo.Name" [])) (some (str "a")))])).isNone
#guard (lowerT dep ∅ (sT [.NStmt (.Declare "@x" (some (.Ty "CHAR" ["5"])) (some .Null))])).isSome
-- A certainly non-character type is not held to a length.
#guard (lowerT dep ∅ (sT [.NStmt (.Declare "@x" (some bigint) (some (num 5)))])).isSome

-- `RETURN` in a function, against the declared result: T-SQL truncates
-- (`fT (vc 5)` above, and here `'abcdef'` from `RETURNS varchar(3)`);
-- PostgreSQL keeps the whole value whatever the declared length (probed).
#guard (lowerT dep ∅ (.Routine .Function ["f"] [inT "@a" (vc 5)] (some (vc 3)) .Caller none
    (.Block [] [.NStmt (.Return (some (str "abcdef")))]))).isNone
#guard (lowerP ∅ (.Routine .Function ["f"] [inP "a" (vc 5)] (some (vc 3)) .Caller none
    (.Block [] [.NStmt (.Return (some (str "abcdef")))]))).isSome

-- `OUTPUT` copy-back into a string variable: the callee's value has no
-- known bound. T-SQL `EXEC dbo.log_value @a, @r OUTPUT` with `@r
-- varchar(20)`; PL/pgSQL `CALL log_value(a, x)` with `x` in an `INOUT`
-- position, a local `varchar(20)`; into a parameter (no length kept) or
-- a `text` it is kept.
#guard (lowerT dep Γ (sT [.Call ["dbo", "log_value"] [arg ta false, arg tr true] none])).isNone
#guard (lowerP Γ (sP [.Call ["log_value"] [arg pa false, arg (pv "x") false] none]
    [.Declare "x" (some (vc 20)) none])).isNone
#guard (lowerP Γ (sP [.Call ["log_value"] [arg pa false, arg (pv "x") false] none]
    [.Declare "x" (some text) none])).isSome
#guard (lowerP Γ (sP [.Call ["log_value"] [arg pa false, arg pr false] none]
    (prt := vc 20))).isSome
-- Into an integer it is kept (the C1 pair at the top of `Examples`).
#guard isConfirmed (check dep Γ cert srcT mirrorP)

-- `INSERT`/`UPDATE`: a string value is never written to an `int` column
-- (`colValue`), on either path; (C3c) a string column takes one that fits
-- (`ExamplesC3c`).
#guard (colValue (.tsql dep ∅) [] {} ⟨"c", "c", false, false, .int .i32, none⟩ (str "a")).isNone
#guard (colValue (.tsql dep ∅) [] {} ⟨"c", "c", false, false, .str 1 false, none⟩ (str "a")).isSome

end Sqldiff.Mirror.ExamplesC2b
