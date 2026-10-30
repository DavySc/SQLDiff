/-
Checked examples for C2c, typed values beyond `int` and strings: integer
widths, `bit` against `boolean`, `date` and `datetime2(p)` against `date`
and `timestamp(p)`, in the forms `sqldiff convert --to postgres` writes
(eval/values, V1 to V8), with runs of the core interpreter, and what
stays outside (a `datetime2(7)`, a narrowing store, a bit stored from
`2`, a literal that is not ISO 8601, `DATEADD(hour, ...)`, a constant
beyond the `bigint` range). Every `#guard` runs on every `lake build`.
-/
import Sqldiff.Mirror.ExamplesC3b
import Sqldiff.Mirror.ExamplesDiag

namespace Sqldiff.Mirror.ExamplesC2c

open Sqldiff.Core (Obs Db SExpr IW)
open Sqldiff.Core.Calendar (daysFromCivil usPerDay)
open Sqldiff.Mirror.Examples
open Sqldiff.Mirror.ExamplesC3a (tc upd ins nocount runD)
open Sqldiff.Mirror.ExamplesC3b (sel asg als item selectT forBlock field countStarE)
open Sqldiff.Gate (Answer)

/-! ## Types and the table

```sql
-- T-SQL                                        -- PostgreSQL (the converted DDL)
CREATE TABLE dbo.Ev (                           CREATE TABLE Ev (
  Id int NOT NULL PRIMARY KEY,                    Id integer NOT NULL PRIMARY KEY,
  D date NULL,                                    D date NULL,
  T datetime2(6) NULL,                            T timestamp(6) NULL,
  B bit NOT NULL,                                 B boolean NOT NULL,
  N bigint NULL,                                  N bigint NULL,
  S smallint NULL DEFAULT 0);                     S smallint NULL DEFAULT 0);
```
-/

def date : Ast.Ty := .Ty "DATE" []
def dt2 (p : Nat) : Ast.Ty := .Ty "DATETIME2" [toString p]
def ts (p : Nat) : Ast.Ty := .Ty "TIMESTAMP" [toString p]
def bit : Ast.Ty := .Ty "BIT" []
def boolean : Ast.Ty := .Ty "BOOLEAN" []
def smallint : Ast.Ty := .Ty "SMALLINT" []
def str (s : String) : Ast.Expr := .Str s
def tru : Ast.Expr := .Special "TRUE"
def fls : Ast.Expr := .Special "FALSE"

def ev : CatTable :=
  { name := "ev"
    cols := [⟨"id", "Id", true, true, .int .i32, none⟩, ⟨"d", "D", false, false, .date, none⟩,
      ⟨"t", "T", false, false, .ts 6, none⟩, ⟨"b", "B", true, false, .bit, none⟩,
      ⟨"n", "N", false, false, .int .i64, none⟩,
      ⟨"s", "S", false, false, .int .i16, some (.int 0)⟩] }

#guard ev.wellFormed

def depv : Deployment := { rowCounts := false, routines := [], tables := [("dbo.Ev", "ev")] }
def Γv : Catalog := { routines := [], tables := [ev] }
def evDef : Core.TableDef := ev.toDef

/-! ## The pair

```sql
CREATE PROCEDURE dbo.p @id int, @big bigint, @flag bit, @when date, @ts datetime2(6) OUTPUT
AS
BEGIN
  SET XACT_ABORT ON;
  SET NOCOUNT ON;
  DECLARE @d date = '2024-01-31';
  DECLARE @b bit = 1;
  DECLARE @s smallint = 5;
  DECLARE @n bigint;
  DECLARE @t datetime2(6) = '2024-01-01T23:59:00.123456';
  DECLARE @i int;
  SET @n = @big + 1;
  SET @n = @id + @big;
  SET @b = 0;
  SET @d = DATEADD(day, 1, @d);
  SET @d = DATEADD(month, @id, @d);
  SET @d = DATEADD(year, -1, @d);
  SET @t = DATEADD(day, 2, @t);
  SET @i = DATEDIFF(day, @d, @t);
  SET @d = CAST(@t AS date);
  SET @t = @d;
  IF @b = 1 SET @i = 1;
  IF @flag = 1 AND @d < @when SET @i = 4;
  IF @d = '2024-01-01' SET @i = 5;
  IF @t >= '2024-01-01T00:00:00' SET @i = 6;
  IF @ts IS NULL SET @ts = @t;
  INSERT INTO dbo.Ev (Id, D, T, B, N) VALUES (@id, @d, @t, 1, @n);
  UPDATE dbo.Ev SET B = 0, D = DATEADD(day, 1, D) WHERE Id = @id;
  SELECT @n = N, @d = D FROM dbo.Ev WHERE Id = @id;
  SELECT @i = COUNT(*) FROM dbo.Ev;
END
```

and its conversion (the `COMMIT`s after the data statements are the
converter's, C4; the `FOR` block is C3b):

```sql
CREATE OR REPLACE PROCEDURE p(p_id integer, p_big bigint, p_flag boolean, p_when date,
                              INOUT p_ts timestamp(6))
LANGUAGE plpgsql AS $$
DECLARE
  v_d date; v_b boolean; v_s smallint; v_n bigint; v_t timestamp(6); v_i integer;
BEGIN
  v_d := '2024-01-31';
  v_b := TRUE;
  v_s := 5;
  v_t := '2024-01-01T23:59:00.123456';
  v_n := p_big + 1;
  v_n := p_id + p_big;
  v_b := FALSE;
  v_d := (v_d + 1);
  v_d := (v_d + make_interval(months => p_id))::date;
  v_d := (v_d + make_interval(years => -1))::date;
  v_t := (v_t + make_interval(days => 2));
  v_i := ((v_t)::date - (v_d)::date);
  v_d := CAST(v_t AS date);
  v_t := v_d;
  IF v_b = TRUE THEN v_i := 1; END IF;
  IF p_flag = TRUE AND v_d < p_when THEN v_i := 4; END IF;
  IF v_d = '2024-01-01' THEN v_i := 5; END IF;
  IF v_t >= '2024-01-01T00:00:00' THEN v_i := 6; END IF;
  IF p_ts IS NULL THEN p_ts := v_t; END IF;
  INSERT INTO Ev (Id, D, T, B, N) VALUES (p_id, v_d, v_t, TRUE, v_n);
  COMMIT;
  UPDATE Ev SET B = FALSE, D = (D + 1) WHERE Id = p_id;
  COMMIT;
  DECLARE sqldiff_r record;
  BEGIN
  FOR sqldiff_r IN SELECT N AS sqldiff_c1, D AS sqldiff_c2 FROM Ev WHERE Id = p_id LOOP
    v_n := sqldiff_r.sqldiff_c1;
    v_d := sqldiff_r.sqldiff_c2;
  END LOOP;
  END;
  SELECT COUNT(*) INTO v_i FROM Ev;
END; $$;
```
-/

def dateadd (part : String) (n d : Ast.Expr) : Ast.Expr :=
  fn "DATEADD" [.Name [part], n, d]
def datediff (part : String) (a b : Ast.Expr) : Ast.Expr :=
  fn "DATEDIFF" [.Name [part], a, b]
/-- `make_interval(years | months | days => n)` as the parser hands it over. -/
def mkInterval (part : String) (n : Ast.Expr) : Ast.Expr :=
  let z : Ast.Expr := .Num "0"
  fn "make_interval" [if part == "years" then n else z, if part == "months" then n else z, z,
    if part == "days" then n else z, z, z, .Num "0.0"]
def castDate (e : Ast.Expr) : Ast.Expr := .Cast e date
def ifT (c : Ast.Expr) (t : List Ast.Node) : Ast.Node := .If c t []
def declT (x : String) (ty : Ast.Ty) (v : Option Ast.Expr) : Ast.Node :=
  .NStmt (.Declare x (some ty) v)
def keyT : Ast.Expr := .Bin "=" (tc "Id") (tv "@id")
def keyP : Ast.Expr := .Bin "=" (pv "Id") (pv "p_id")

def paramsT : List Ast.Param :=
  [inT "@id", inT "@big" bigint, inT "@flag" bit, inT "@when" date, ioT "@ts" (dt2 6)]
def paramsP : List Ast.Param :=
  [inP "p_id", inP "p_big" bigint, inP "p_flag" boolean, inP "p_when" date, ioP "p_ts" (ts 6)]

def bodyT : List Ast.Node := [
  declT "@d" date (some (str "2024-01-31")),
  declT "@b" bit (some (num 1)),
  declT "@s" smallint (some (num 5)),
  declT "@n" bigint none,
  declT "@t" (dt2 6) (some (str "2024-01-01T23:59:00.123456")),
  declT "@i" int none,
  set (tv "@n") (.Bin "+" (tv "@big") (num 1)),
  set (tv "@n") (.Bin "+" (tv "@id") (tv "@big")),
  set (tv "@b") (num 0),
  set (tv "@d") (dateadd "day" (num 1) (tv "@d")),
  set (tv "@d") (dateadd "month" (tv "@id") (tv "@d")),
  set (tv "@d") (dateadd "year" (.Num "-1") (tv "@d")),
  set (tv "@t") (dateadd "day" (num 2) (tv "@t")),
  set (tv "@i") (datediff "day" (tv "@d") (tv "@t")),
  set (tv "@d") (castDate (tv "@t")),
  set (tv "@t") (tv "@d"),
  ifT (.Bin "=" (tv "@b") (num 1)) [set (tv "@i") (num 1)],
  ifT (.Bin "AND" (.Bin "=" (tv "@flag") (num 1)) (.Bin "<" (tv "@d") (tv "@when")))
    [set (tv "@i") (num 4)],
  ifT (.Bin "=" (tv "@d") (str "2024-01-01")) [set (tv "@i") (num 5)],
  ifT (.Bin ">=" (tv "@t") (str "2024-01-01T00:00:00")) [set (tv "@i") (num 6)],
  ifT (.IsNull (tv "@ts") false) [set (tv "@ts") (tv "@t")],
  ins ["dbo", "Ev"] ["Id", "D", "T", "B", "N"] [[tv "@id", tv "@d", tv "@t", num 1, tv "@n"]],
  upd ["dbo", "Ev"] [("B", num 0), ("D", dateadd "day" (num 1) (tc "D"))] (some keyT),
  selectT [asg "@n" (tc "N"), asg "@d" (tc "D")] ["dbo", "Ev"] (some keyT),
  selectT [asg "@i" countStarE] ["dbo", "Ev"] none]

def declsP : List Ast.Stmt :=
  [.Declare "v_d" (some date) none, .Declare "v_b" (some boolean) none,
   .Declare "v_s" (some smallint) none, .Declare "v_n" (some bigint) none,
   .Declare "v_t" (some (ts 6)) none, .Declare "v_i" (some integer) none]

def bodyP : List Ast.Node := [
  set (pv "v_d") (str "2024-01-31"),
  set (pv "v_b") tru,
  set (pv "v_s") (num 5),
  set (pv "v_t") (str "2024-01-01T23:59:00.123456"),
  set (pv "v_n") (.Bin "+" (pv "p_big") (num 1)),
  set (pv "v_n") (.Bin "+" (pv "p_id") (pv "p_big")),
  set (pv "v_b") fls,
  set (pv "v_d") (.Bin "+" (pv "v_d") (num 1)),
  set (pv "v_d") (castDate (.Bin "+" (pv "v_d") (mkInterval "months" (pv "p_id")))),
  set (pv "v_d") (castDate (.Bin "+" (pv "v_d") (mkInterval "years" (.Num "-1")))),
  set (pv "v_t") (.Bin "+" (pv "v_t") (mkInterval "days" (num 2))),
  set (pv "v_i") (.Bin "-" (castDate (pv "v_t")) (castDate (pv "v_d"))),
  set (pv "v_d") (castDate (pv "v_t")),
  set (pv "v_t") (pv "v_d"),
  ifT (.Bin "=" (pv "v_b") tru) [set (pv "v_i") (num 1)],
  ifT (.Bin "AND" (.Bin "=" (pv "p_flag") tru) (.Bin "<" (pv "v_d") (pv "p_when")))
    [set (pv "v_i") (num 4)],
  ifT (.Bin "=" (pv "v_d") (str "2024-01-01")) [set (pv "v_i") (num 5)],
  ifT (.Bin ">=" (pv "v_t") (str "2024-01-01T00:00:00")) [set (pv "v_i") (num 6)],
  ifT (.IsNull (pv "p_ts") false) [set (pv "p_ts") (pv "v_t")],
  ins ["Ev"] ["Id", "D", "T", "B", "N"] [[pv "p_id", pv "v_d", pv "v_t", tru, pv "v_n"]],
  .Txn "COMMIT" none,
  upd ["Ev"] [("B", fls), ("D", .Bin "+" (pv "D") (num 1))] (some keyP),
  .Txn "COMMIT" none,
  forBlock [als (pv "N") "sqldiff_c1", als (pv "D") "sqldiff_c2"] ["Ev"] (some keyP)
    [field "v_n" "sqldiff_r" "sqldiff_c1", field "v_d" "sqldiff_r" "sqldiff_c2"],
  .NStmt (.SSelect (sel [item countStarE] ["Ev"] none [pv "v_i"]))]

def procT (body : List Ast.Node := bodyT) (ps : List Ast.Param := paramsT) : Ast.Routine :=
  tsqlProc ["dbo", "p"] ps ([xactAbort, nocount] ++ body)
def procP (body : List Ast.Node := bodyP) (decls : List Ast.Stmt := declsP)
    (ps : List Ast.Param := paramsP) : Ast.Routine :=
  pgProc ["p"] ps decls body

def vCert : Cert :=
  ⟨[("@id", "p_id"), ("@big", "p_big"), ("@flag", "p_flag"), ("@when", "p_when"), ("@ts", "p_ts"),
    (statusVar, statusVar), ("@d", "v_d"), ("@b", "v_b"), ("@s", "v_s"), ("@n", "v_n"),
    ("@t", "v_t"), ("@i", "v_i")]⟩

def checkV (a b : Ast.Routine) (cert : Cert := vCert) : Answer := check depv Γv cert a b

-- Both sides lower, and the pair is confirmed in the slice.
#guard (lowerT depv Γv procT).isSome
#guard (lowerP Γv procP).isSome
#guard isC2a (checkV procT procP)
-- What it rests on: arithmetic errors (`+`), `=` (`ANSI_NULLS`), a failure
-- after a write, the two `DATEADD` facts, and the converter's `COMMIT`s.
-- (`ASM-TSQL-STRING-ARGS`: the `bit`, `date` and `datetime2(6)`
-- parameters take values of their type from a T-SQL caller.)
#guard residualOf depv Γv procT ==
  some ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-ANSI-NULLS", "ASM-STATE-AFTER-ERROR",
    "ASM-TSQL-STRING-ARGS", "ASM-DATEADD-RANGE", "ASM-DATEADD-YEAR-END"]
#guard (lowerT depv Γv procT).map (fun r => residual procT r true) ==
  some ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-ANSI-NULLS", "ASM-STATE-AFTER-ERROR",
    "ASM-TSQL-STRING-ARGS", "ASM-DATEADD-RANGE", "ASM-DATEADD-YEAR-END", "ASM-NO-CALLER-TXN"]

/-! ## Runs of the lowered routine

`@id = 7`, `@big = 10`, `@flag = 1`, `@when = 2024-06-01`, `@ts` NULL:
`@d` goes 2024-01-31, + 1 day = 2024-02-01, + 7 months = 2024-09-01,
- 1 year = 2023-09-01; `@t` = 2024-01-03T23:59:00.123456; `@i` = 124 days;
`@d` = 2024-01-03 and `@t` its midnight; `@b` is 0 so `@i` stays, then 4
(the flag and `@d < @when`), not 5, then 6; `@ts` takes `@t`; the row
(7, 2024-01-03, midnight, 1, 17) is inserted with `S` 0 by default, then
`B` becomes 0 and `D` 2024-01-04; the read gives `@n` 17 and `@d`, and
the count 1. The caller sees `@ts` and the status 0. -/

def D (y m d : Int) : V := .date (daysFromCivil y m d)
def Tm (y m d : Int) (us : Int := 0) : V := .ts (daysFromCivil y m d * usPerDay + us)
def midnight (y m d : Int) : V := Tm y m d

def args0 : List V := [I 7, I 10, .bool true, D 2024 6 1, N]
def T := lowerT depv Γv procT
def P := lowerP Γv procP
#guard runD T args0 [] ==
  some (.done [midnight 2024 1 3, I 0] none 0
    [("ev", [[I 7, D 2024 1 4, midnight 2024 1 3, .bool false, I 17, I 0]])] [])
#guard runD T args0 [] == runD P args0 []
-- `@big + 1` overflows `bigint`: an arithmetic error, nothing written.
#guard runD T [I 7, I 9223372036854775807, .bool true, D 2024 6 1, N] [] ==
  some (.failed .arithmetic [])
#guard runD T [I 7, I 9223372036854775807, .bool true, D 2024 6 1, N] [] ==
  runD P [I 7, I 9223372036854775807, .bool true, D 2024 6 1, N] []
-- With `@flag` NULL the `AND` is UNKNOWN: `@i` is 6 all the same; a
-- caller's `@ts` is kept when not NULL.
#guard runD T [I 7, I 10, N, D 2024 6 1, Tm 2020 2 2] [] ==
  some (.done [Tm 2020 2 2, I 0] none 0
    [("ev", [[I 7, D 2024 1 4, midnight 2024 1 3, .bool false, I 17, I 0]])] [])
-- A duplicate `Id`: a constraint error.
#guard runD T args0 [("ev", [[I 7, N, N, .bool true, N, N]])] == some (.failed .constraint [])

/-! ## The core's operators -/

def σ (a b : V) : Core.Store Unit := fun x => if x = "a" then a else if x = "b" then b else .null
def evl (s : SExpr) (a b : V) : Core.Res Unit := s.eval (σ a b) Core.nullStore

-- Widths: overflow at each width, `smallint` arithmetic at 16 bits (V1).
#guard evl (.arith .i16 .add (.var "a") (.var "b")) (I 32767) (I 1) == .err .overflow
#guard evl (.arith .i32 .add (.var "a") (.var "b")) (I 32767) (I 1) == .ok (I 32768)
#guard evl (.arith .i64 .add (.var "a") (.var "b")) (I 2147483647) (I 1) == .ok (I 2147483648)
#guard evl (.arith .i64 .add (.var "a") (.var "b")) (I 9223372036854775807) (I 1) == .err .overflow
#guard evl (.arith .i64 .mul (.var "a") (.int 2)) (I 9223372036854775807) N == .err .overflow
#guard evl (.arith .i64 .div (.var "a") (.int (-1))) (I (-9223372036854775808)) N == .err .overflow
#guard evl (.arith .i64 .div (.var "a") (.int 7)) (I (-100000000000)) N == .ok (I (-14285714285))
#guard evl (.neg .i16 (.var "a")) (I (-32768)) N == .err .overflow
#guard evl (.neg .i64 (.var "a")) (I (-2147483648)) N == .ok (I 2147483648)
-- Bits: the three-valued forms of `@b = 1`, `@b = 0`, `@b <> 1` (V3).
#guard evl (.var "a") (.bool true) N == .ok (.bool true)
#guard evl (.not (.var "a")) (.bool true) N == .ok (.bool false)
#guard evl (.not (.var "a")) N N == .ok N
#guard evl (.isNull (.var "a") false) N N == .ok (.bool true)
-- Dates: comparisons, a date against a timestamp at midnight (V5).
#guard evl (.cmp .lt (.var "a") (.var "b")) (D 2024 3 14) (D 2024 3 15) == .ok (.bool true)
#guard evl (.cmp .eq (.var "a") (.var "b")) N (D 2024 3 15) == .ok N
#guard evl (.cmp .eq (.toTs (.var "a")) (.var "b")) (D 2024 3 15) (Tm 2024 3 15 1) == .ok (.bool false)
#guard evl (.cmp .eq (.toTs (.var "a")) (.var "b")) (D 2024 3 15) (midnight 2024 3 15) == .ok (.bool true)
#guard evl (.cmp .lt (.var "a") (.var "b")) (D 2024 3 15) (I 1) == Core.typeErr
-- DATEADD (V6): the month-end clamp, February 29 plus a year, the time kept.
#guard evl (.dateAdd .month (.int 1) (.var "a")) (D 2024 1 31) N == .ok (D 2024 2 29)
#guard evl (.dateAdd .month (.int 13) (.var "a")) (D 2024 1 31) N == .ok (D 2025 2 28)
#guard evl (.dateAdd .month (.int (-1)) (.var "a")) (D 2024 3 31) N == .ok (D 2024 2 29)
#guard evl (.dateAdd .year (.int 1) (.var "a")) (D 2024 2 29) N == .ok (D 2025 2 28)
#guard evl (.dateAdd .year (.int (-1)) (.var "a")) (D 2024 2 29) N == .ok (D 2023 2 28)
#guard evl (.dateAdd .year (.int 4) (.var "a")) (D 2024 2 29) N == .ok (D 2028 2 29)
#guard evl (.dateAdd .day (.int 60) (.var "a")) (D 2023 12 31) N == .ok (D 2024 2 29)
#guard evl (.dateAdd .day (.int (-1)) (.var "a")) (D 2024 3 1) N == .ok (D 2024 2 29)
#guard evl (.dateAdd .month (.int 1) (.var "a")) (Tm 2024 1 31 37230123456) N ==
  .ok (Tm 2024 2 29 37230123456)
#guard evl (.dateAdd .day (.int 2) (.var "a")) (Tm 2024 1 31 37230123456) N ==
  .ok (Tm 2024 2 2 37230123456)
#guard evl (.dateAdd .day (.var "b") (.var "a")) (D 2024 1 31) N == .ok N
#guard evl (.dateAdd .day (.int 1) (.var "a")) N N == .ok N
-- Past 9999-12-31 the core keeps counting (`ASM-DATEADD-RANGE`).
#guard evl (.dateAdd .day (.int 1) (.var "a")) (D 9999 12 31) N == .ok (.date 3652059)
-- DATEDIFF (V7): day boundaries, not elapsed time.
#guard evl (.dateDiff (.var "a") (.var "b")) (Tm 2024 1 1 (86340 * 1000000)) (Tm 2024 1 2 60000000)
  == .ok (I 1)
#guard evl (.dateDiff (.var "a") (.var "b")) (Tm 2024 1 2 60000000) (Tm 2024 1 1 (86340 * 1000000))
  == .ok (I (-1))
#guard evl (.dateDiff (.var "a") (.var "b")) (Tm 2024 1 1) (Tm 2024 1 1 (usPerDay - 1)) == .ok (I 0)
#guard evl (.dateDiff (.var "a") (.var "b")) (D 2023 12 31) (D 2024 3 1) == .ok (I 61)
#guard evl (.dateDiff (.var "a") (.var "b")) (D 1 1 1) (D 9999 12 31) == .ok (I 3652058)
#guard evl (.dateDiff (.var "a") (.var "b")) N (D 2024 3 1) == .ok N
-- The date of a timestamp (V5), a date at midnight.
#guard evl (.toDate (.var "a")) (Tm 2024 3 15 (usPerDay - 1)) N == .ok (D 2024 3 15)
#guard evl (.toTs (.var "a")) (D 2024 3 15) N == .ok (midnight 2024 3 15)

/-! ## Literals -/

#guard dateLit? "2024-02-29" == some (daysFromCivil 2024 2 29)
#guard dateLit? "0001-01-01" == some 0
#guard dateLit? "9999-12-31" == some 3652058
#guard dateLit? "2023-02-29" == none
#guard dateLit? "2024-02-30" == none
#guard dateLit? "0000-01-01" == none
#guard dateLit? "20240229" == none
#guard dateLit? "2024-2-29" == none
#guard dateLit? "01/02/2024" == none
#guard dateLit? "2024-01-02T00:00:00" == none
#guard tsLit? 6 "2024-01-02T03:04:05" == some (daysFromCivil 2024 1 2 * usPerDay + 11045000000)
#guard tsLit? 6 "2024-01-02T03:04:05.5" == some (daysFromCivil 2024 1 2 * usPerDay + 11045500000)
#guard tsLit? 6 "2024-01-02T03:04:05.123456" ==
  some (daysFromCivil 2024 1 2 * usPerDay + 11045123456)
#guard tsLit? 6 "2024-01-02T03:04:05.1234567" == none
#guard tsLit? 3 "2024-01-02T03:04:05.1234" == none
#guard tsLit? 3 "2024-01-02T03:04:05.123" == some (daysFromCivil 2024 1 2 * usPerDay + 11045123000)
#guard tsLit? 0 "2024-01-02T03:04:05.1" == none
#guard tsLit? 6 "2024-01-02 03:04:05" == none
#guard tsLit? 6 "2024-01-02T24:00:00" == none
#guard tsLit? 6 "2024-01-02T03:60:05" == none
#guard tsLit? 6 "2024-01-02T03:04:60" == none
#guard tsLit? 6 "2024-01-02T03:04:05." == none
#guard tsLit? 6 "2024-01-02" == none
#guard tsLit? 6 "9999-12-31T23:59:59.999999" == some (3652059 * usPerDay - 1)

/-! ## Typing: the forms, and what stays outside -/

def envT : TyEnv := [("@id", int), ("@big", bigint), ("@s", smallint), ("@b", bit), ("@d", date),
  ("@t", dt2 6), ("@t3", dt2 3), ("@t7", dt2 7), ("@dt", .Ty "DATETIME" [])]
def envP : TyEnv := [("p_id", integer), ("p_big", bigint), ("v_s", smallint), ("v_b", boolean),
  ("v_d", date), ("v_t", ts 6), ("v_t3", ts 3), ("v_tz", .Ty "TIMESTAMPTZ" [])]
def tT (e : Ast.Expr) : Option (SExpr × STy) := typed (.tsql depv Γv) envT {} e
def tP (e : Ast.Expr) : Option (SExpr × STy) := typed (.pg Γv []) envP {} e

-- Widths: the wider operand's; comparisons across widths.
#guard tT (.Bin "+" (tv "@id") (tv "@big")) == some (.arith .i64 .add (.var "@id") (.var "@big"), .int .i64)
#guard tT (.Bin "+" (tv "@s") (tv "@s")) == some (.arith .i16 .add (.var "@s") (.var "@s"), .int .i16)
#guard tT (.Bin "*" (tv "@s") (tv "@id")) == some (.arith .i32 .mul (.var "@s") (.var "@id"), .int .i32)
#guard tT (.Bin "<" (tv "@s") (tv "@big")) == some (.cmp .lt (.var "@s") (.var "@big"), .bool)
#guard tT (.Unary "-" (tv "@s")) == some (.neg .i16 (.var "@s"), .int .i16)
#guard tP (.Bin "+" (pv "p_id") (pv "p_big")) == some (.arith .i64 .add (.var "p_id") (.var "p_big"), .int .i64)
-- Two smallints divide at 16 bits (`-32768 / -1` raises on both, V1); a
-- literal is an `int`, so `@s / 2` and `@s / -1` are `int` divisions.
#guard tT (.Bin "/" (tv "@s") (num 2)) == some (.arith .i32 .div (.var "@s") (.int 2), .int .i32)
#guard tT (.Bin "/" (tv "@s") (tv "@s")) == some (.arith .i16 .div (.var "@s") (.var "@s"), .int .i16)
#guard (tT (.Bin "/" (tv "@s") (.Num "-1"))).isSome
#guard evl (.arith .i16 .div (.var "a") (.var "b")) (I (-32768)) (I (-1)) == .err .overflow
#guard evl (.arith .i32 .div (.var "a") (.int (-1))) (I (-32768)) N == .ok (I 32768)
-- `%` keeps its literal-divisor rule (`-32768 % -1` on smallints: 8115 against 0).
#guard (tT (.Bin "%" (tv "@s") (tv "@s"))).isNone
#guard tT (.Bin "/" (tv "@big") (tv "@s")) == some (.arith .i64 .div (.var "@big") (.var "@s"), .int .i64)
-- A bigint constant: PostgreSQL types it, T-SQL only as a stored value (V2).
#guard tP (.Num "2147483648") == some (.int 2147483648, .int .i64)
#guard tP (.Num "9223372036854775807") == some (.int 9223372036854775807, .int .i64)
#guard (tP (.Num "9223372036854775808")).isNone
#guard (tT (.Num "2147483648")).isNone
#guard (tT (.Bin "+" (tv "@big") (.Num "2147483648"))).isNone
#guard tT (.Bin "=" (tv "@big") (.Num "2147483648")) ==
  some (.cmp .eq (.var "@big") (.int 2147483648), .bool)
-- `COALESCE` and `ISNULL` take the widest; `ISNULL` keeps the first's type.
#guard tT (fn "COALESCE" [tv "@s", tv "@big"]) == some (.coalesce (.var "@s") (.var "@big"), .int .i64)
#guard tT (fn "ISNULL" [tv "@big", tv "@id"]) == some (.coalesce (.var "@big") (.var "@id"), .int .i64)
#guard (tT (fn "ISNULL" [tv "@id", tv "@big"])).isNone
#guard tT (fn "ISNULL" [tv "@s", num 7]) == some (.coalesce (.var "@s") (.int 7), .int .i16)
#guard (tT (fn "ISNULL" [tv "@s", num 40000])).isNone
-- Bits: the converter's comparison forms, on either side (V3).
-- They stay comparisons, so `ASM-TSQL-ANSI-NULLS` is named (review of C2c).
#guard tT (.Bin "=" (tv "@b") (num 1)) == some (.cmp .eq (.var "@b") (.bool true), .bool)
#guard tT (.Bin "=" (num 0) (tv "@b")) == some (.cmp .eq (.var "@b") (.bool false), .bool)
#guard tT (.Bin "<>" (tv "@b") (num 1)) == some (.cmp .ne (.var "@b") (.bool true), .bool)
#guard tT (.Bin "<>" (tv "@b") (num 0)) == some (.cmp .ne (.var "@b") (.bool false), .bool)
#guard tP (.Bin "=" (pv "v_b") tru) == some (.cmp .eq (.var "v_b") (.bool true), .bool)
#guard tP (.Bin "=" fls (pv "v_b")) == some (.cmp .eq (.var "v_b") (.bool false), .bool)
#guard tP (.Bin "<>" (pv "v_b") tru) == some (.cmp .ne (.var "v_b") (.bool true), .bool)
-- Three-valued as on both engines (V3): NULL `<> 1` is UNKNOWN under `ANSI_NULLS ON`.
#guard evl (.cmp .ne (.var "a") (.bool true)) N N == .ok N
#guard evl (.cmp .ne (.var "a") (.bool true)) (.bool false) N == .ok (.bool true)
#guard evl (.cmp .eq (.var "a") (.bool false)) (.bool false) N == .ok (.bool true)
#guard (tT (.Bin "=" (tv "@b") (num 2))).isNone
#guard (tT (.Bin "<" (tv "@b") (num 1))).isNone
#guard (tT (.Bin "=" (tv "@b") (tv "@b"))).isNone
#guard (tT (.Bin "+" (tv "@b") (num 1))).isNone
#guard (tP (.Bin "=" (pv "v_b") (num 1))).isNone
#guard (tT (.Bin "=" (tv "@id") (num 1))).isSome
-- Dates: literals next to a value, comparisons, a date against a timestamp.
#guard tT (.Bin "=" (tv "@d") (str "2024-01-01")) ==
  some (.cmp .eq (.var "@d") (.date (daysFromCivil 2024 1 1)), .bool)
#guard tT (.Bin ">=" (tv "@t") (str "2024-01-01T00:00:00")) ==
  some (.cmp .ge (.var "@t") (.ts (daysFromCivil 2024 1 1 * usPerDay)), .bool)
#guard tP (.Bin ">=" (pv "v_t") (str "2024-01-01T00:00:00")) ==
  some (.cmp .ge (.var "v_t") (.ts (daysFromCivil 2024 1 1 * usPerDay)), .bool)
#guard tT (.Bin "<" (tv "@d") (tv "@t")) == some (.cmp .lt (.toTs (.var "@d")) (.var "@t"), .bool)
#guard tT (.Bin "<" (tv "@t3") (tv "@t")) == some (.cmp .lt (.var "@t3") (.var "@t"), .bool)
#guard (tT (.Bin "=" (tv "@d") (str "01/02/2024"))).isNone
#guard (tT (.Bin "=" (tv "@d") (str "20240101"))).isNone
#guard (tT (.Bin "=" (tv "@t") (str "2024-01-01"))).isNone
#guard (tT (.Bin "=" (tv "@t") (str "2024-01-01T00:00:00.1234567"))).isNone
#guard (tT (.Bin "=" (tv "@t3") (str "2024-01-01T00:00:00.1234"))).isNone
#guard (tT (.Bin "=" (tv "@d") (tv "@id"))).isNone
#guard (tT (.Bin "+" (tv "@d") (num 1))).isNone
#guard (tT (.Bin "-" (tv "@d") (tv "@d"))).isNone
#guard (tT (.Bin "=" (tv "@dt") (str "2024-01-01"))).isNone
#guard (tT (.Bin "=" (tv "@t7") (str "2024-01-01T00:00:00"))).isNone
-- Cast literals and `CAST(x AS date)`.
#guard tT (.Cast (str "2024-02-29") date) == some (.date (daysFromCivil 2024 2 29), .date)
#guard tP (.Cast (str "2024-02-29T01:02:03") (ts 6)) ==
  some (.ts (daysFromCivil 2024 2 29 * usPerDay + 3723000000), .ts 6)
#guard (tT (.Cast (str "2024-02-29T01:02:03") (dt2 7))).isNone
#guard (tT (.Cast (str "2024-02-30") date)).isNone
#guard tT (castDate (tv "@t")) == some (.toDate (.var "@t"), .date)
#guard tT (castDate (tv "@d")) == some (.var "@d", .date)
#guard (tT (.Cast (tv "@d") (dt2 6))).isNone
#guard (tT (.Cast (tv "@id") date)).isNone
-- DATEADD and the converter's forms (V6).
#guard tT (dateadd "day" (num 1) (tv "@d")) == some (.dateAdd .day (.int 1) (.var "@d"), .date)
#guard tT (dateadd "dd" (tv "@s") (tv "@d")) == some (.dateAdd .day (.var "@s") (.var "@d"), .date)
#guard tT (dateadd "mm" (tv "@id") (tv "@t3")) == some (.dateAdd .month (.var "@id") (.var "@t3"), .ts 3)
#guard tT (dateadd "yyyy" (.Num "-1") (tv "@d")) == some (.dateAdd .year (.int (-1)) (.var "@d"), .date)
#guard tP (.Bin "+" (pv "v_d") (num 1)) == some (.dateAdd .day (.int 1) (.var "v_d"), .date)
#guard tP (.Bin "-" (pv "v_d") (num 5)) == some (.dateAdd .day (.int (-5)) (.var "v_d"), .date)
#guard tP (.Bin "+" (pv "v_d") (pv "p_id")) == some (.dateAdd .day (.var "p_id") (.var "v_d"), .date)
#guard tP (castDate (.Bin "+" (pv "v_d") (mkInterval "months" (pv "p_id")))) ==
  some (.dateAdd .month (.var "p_id") (.var "v_d"), .date)
#guard tP (castDate (.Bin "+" (pv "v_d") (mkInterval "years" (.Num "-1")))) ==
  some (.dateAdd .year (.int (-1)) (.var "v_d"), .date)
#guard tP (.Bin "+" (pv "v_t") (mkInterval "days" (num 2))) ==
  some (.dateAdd .day (.int 2) (.var "v_t"), .ts 6)
#guard (tT (dateadd "hour" (num 1) (tv "@t"))).isNone
#guard (tT (dateadd "week" (num 1) (tv "@d"))).isNone
#guard (tT (dateadd "day" (tv "@big") (tv "@d"))).isNone
#guard (tT (dateadd "day" (num 1) (tv "@dt"))).isNone
#guard (tT (dateadd "day" (num 1) (str "2024-01-01"))).isNone
#guard (tP (.Bin "-" (pv "v_d") (pv "p_id"))).isNone
#guard (tP (.Bin "+" (pv "v_d") (pv "p_big"))).isNone
#guard (tP (.Bin "+" (pv "v_t") (num 1))).isNone
#guard (tP (.Bin "+" (pv "v_t") (fn "make_interval" [.Num "0", num 1, .Num "0", num 1, .Num "0",
  .Num "0", .Num "0.0"]))).isNone
#guard (tP (.Bin "+" (pv "v_t") (fn "make_interval" [.Num "0", .Num "0", .Num "0", .Num "0", num 1,
  .Num "0", .Num "0.0"]))).isNone
#guard (tP (.Bin "+" (pv "v_tz") (mkInterval "days" (num 1)))).isNone
-- DATEDIFF by day, and the converter's subtraction of dates (V7).
#guard tT (datediff "day" (tv "@d") (tv "@t")) ==
  some (.dateDiff (.var "@d") (.toDate (.var "@t")), .int .i32)
#guard tP (.Bin "-" (castDate (pv "v_t")) (castDate (pv "v_d"))) ==
  some (.dateDiff (.var "v_d") (.toDate (.var "v_t")), .int .i32)
#guard (tT (datediff "month" (tv "@d") (tv "@t"))).isNone
#guard (tT (datediff "day" (tv "@id") (tv "@t"))).isNone
#guard (tP (.Bin "-" (pv "v_t") (pv "v_t"))).isNone

/-! ## Stores: what fits -/

def storeT (x : String) (v : Ast.Expr) : Ast.Routine :=
  tsqlProc ["dbo", "s"] [inT "@id", inT "@big" bigint, inT "@flag" bit, inT "@when" date,
    inT "@t" (dt2 6), inT "@s" smallint] [xactAbort, set (tv x) v]
def storeP (x : String) (v : Ast.Expr) : Ast.Routine :=
  pgProc ["s"] [inP "p_id", inP "p_big" bigint, inP "p_flag" boolean, inP "p_when" date,
    inP "p_t" (ts 6), inP "p_s" smallint] [] [set (pv x) v]
def storeCert : Cert :=
  ⟨[("@id", "p_id"), ("@big", "p_big"), ("@flag", "p_flag"), ("@when", "p_when"), ("@t", "p_t"),
    ("@s", "p_s"), (statusVar, statusVar)]⟩
def checkS (x : String) (vt : Ast.Expr) (y : String) (vp : Ast.Expr) : Answer :=
  check depv Γv storeCert (storeT x vt) (storeP y vp)

-- Widening stores, literals that fit, and a bigint constant.
#guard isC2a (checkS "@big" (tv "@id") "p_big" (pv "p_id"))
#guard isC2a (checkS "@big" (tv "@s") "p_big" (pv "p_s"))
#guard isC2a (checkS "@id" (tv "@s") "p_id" (pv "p_s"))
#guard isC2a (checkS "@s" (num 7) "p_s" (num 7))
#guard isC2a (checkS "@s" (.Num "-32768") "p_s" (.Num "-32768"))
#guard isC2a (checkS "@big" (.Num "2147483648") "p_big" (.Num "2147483648"))
#guard isC2a (checkS "@big" (.Bin "+" (tv "@big") (num 1)) "p_big" (.Bin "+" (pv "p_big") (num 1)))
-- Narrowing stores are outside the spec (V2): an `int` into a `smallint`,
-- a `bigint` into an `int`, a literal out of range.
#guard reasonOf (checkS "@s" (tv "@id") "p_s" (pv "p_id")) == reasonOutsideT
#guard reasonOf (checkS "@id" (tv "@big") "p_id" (pv "p_big")) == reasonOutsideT
#guard reasonOf (checkS "@s" (num 40000) "p_s" (num 40000)) == reasonOutsideT
-- `@s + 1` is an `int` (the literal's type): storing it back narrows.
#guard (lowerT depv Γv (storeT "@s" (.Bin "+" (tv "@s") (num 1)))).isNone
#guard (lowerT depv Γv (storeT "@s" (.Bin "+" (tv "@s") (tv "@s")))).isSome
#guard (lowerT depv Γv (storeT "@s" (.Bin "+" (tv "@s") (tv "@id")))).isNone
-- Bits: `0`, `1`, NULL and another bit; `2`, an int, a string: outside.
#guard isC2a (checkS "@flag" (num 1) "p_flag" tru)
#guard isC2a (checkS "@flag" (num 0) "p_flag" fls)
#guard isC2a (checkS "@flag" .Null "p_flag" .Null)
#guard reasonOf (checkS "@flag" (num 1) "p_flag" fls) == "the lowered routines differ beyond the renaming"
#guard reasonOf (checkS "@flag" (num 2) "p_flag" tru) == reasonOutsideT
#guard reasonOf (checkS "@flag" (tv "@id") "p_flag" (pv "p_id")) == reasonOutsideT
#guard reasonOf (checkS "@flag" (str "TRUE") "p_flag" tru) == reasonOutsideT
#guard reasonOf (checkS "@flag" (.Bin "=" (tv "@id") (num 1)) "p_flag" (.Bin "=" (pv "p_id") (num 1)))
  == reasonOutsideT
-- A bit into an `int`: T-SQL stores 0 or 1, PL/pgSQL raises 22P02 (V3).
#guard reasonOf (checkS "@id" (tv "@flag") "p_id" (pv "p_flag")) == reasonOutsideT
#guard reasonOf (checkS "@id" (tv "@when") "p_id" (pv "p_when")) == reasonOutsideT
-- Dates: the ISO literals, a date into a timestamp; a timestamp into a
-- date, a wider precision, other literal forms: outside.
#guard isC2a (checkS "@when" (str "2024-02-29") "p_when" (str "2024-02-29"))
#guard isC2a (checkS "@t" (str "2024-02-29T23:59:59.999999") "p_t" (str "2024-02-29T23:59:59.999999"))
#guard isC2a (checkS "@t" (tv "@when") "p_t" (pv "p_when"))
#guard isC2a (checkS "@when" (castDate (tv "@t")) "p_when" (castDate (pv "p_t")))
#guard isC2a (checkS "@when" (dateadd "month" (num 1) (tv "@when")) "p_when"
  (castDate (.Bin "+" (pv "p_when") (mkInterval "months" (num 1)))))
#guard reasonOf (checkS "@when" (tv "@t") "p_when" (pv "p_t")) == reasonOutsideT
#guard reasonOf (checkS "@when" (str "01/02/2024") "p_when" (str "01/02/2024")) == reasonOutsideT
#guard reasonOf (checkS "@when" (str "2024-02-30") "p_when" (str "2024-02-30")) == reasonOutsideT
#guard reasonOf (checkS "@t" (str "2024-02-29T00:00:00.1234567") "p_t" (str "2024-02-29T00:00:00.1234567"))
  == reasonOutsideT
#guard reasonOf (checkS "@t" (str "2024-02-29") "p_t" (str "2024-02-29")) == reasonOutsideT
#guard reasonOf (checkS "@when" (tv "@id") "p_when" (pv "p_id")) == reasonOutsideT
-- A `datetime2(7)` or a `datetime` local: no value of the slice, the C1
-- path as before (a store into it is not held).
-- A `datetime2(7)` local against a `timestamp(6)` one: the declared
-- types differ (the conversion's `ASM-FRACTIONAL-SECONDS`), refused.
#guard reasonOf (check depv Γv ⟨[("@x", "v_x"), (statusVar, statusVar)]⟩
  (tsqlProc ["dbo", "s"] [] [xactAbort, declT "@x" (dt2 7) none, set (tv "@x") (fn "SYSDATETIME" [])])
  (pgProc ["s"] [] [.Declare "v_x" (some (ts 6)) none] [set (pv "v_x") (fn "sysdatetime" [])])) ==
  "the declared types of the variables do not correspond"
#guard reasonOf (check depv Γv ⟨[("@x", "v_x"), (statusVar, statusVar)]⟩
  (tsqlProc ["dbo", "s"] [] [xactAbort, declT "@x" (dt2 7) none, set (tv "@x") (str "2024-01-01T00:00:00")])
  (pgProc ["s"] [] [.Declare "v_x" (some (ts 6)) none] [set (pv "v_x") (str "2024-01-01T00:00:00")]))
  == "the declared types of the variables do not correspond"
#guard (lowerT depv Γv (tsqlProc ["dbo", "s"] [] [xactAbort, declT "@x" (.Ty "DATETIME" []) none,
  declT "@i" int none, set (tv "@i") (datediff "day" (tv "@x") (tv "@x"))])).isNone
-- Parameter defaults in the literal forms.
#guard (lowerT depv Γv (tsqlProc ["dbo", "s"] [.Param "@d" date .In (some (str "2024-01-01")),
  .Param "@b" bit .In (some (num 0)), .Param "@t" (dt2 3) .In (some (str "2024-01-01T00:00:00.5"))]
  [])).isSome
#guard (lowerT depv Γv (tsqlProc ["dbo", "s"] [.Param "@d" date .In (some (str "2024-1-1"))] [])).isNone
#guard (lowerT depv Γv (tsqlProc ["dbo", "s"] [.Param "@b" bit .In (some (num 2))] [])).isNone
#guard (lowerP Γv (pgProc ["s"] [.Param "p_b" boolean .In (some tru)] [] [])).isSome
#guard (lowerP Γv (pgProc ["s"] [.Param "p_b" boolean .In (some (num 1))] [] [])).isNone

/-! ## Signatures -/

#guard sigCorr (procT [] ) (procP [] [])
#guard !sigCorr (procT []) (procP [] [] [inP "p_id", inP "p_big" bigint, inP "p_flag" boolean,
  inP "p_when" date, ioP "p_ts" (ts 3)])
#guard !sigCorr (procT [] [inT "@id", inT "@big" bigint, inT "@flag" bit, inT "@when" date,
  ioT "@ts" (dt2 3)]) (procP [] [] [inP "p_id", inP "p_big" bigint, inP "p_flag" boolean,
  inP "p_when" date, ioP "p_ts" (ts 3)])
#guard sigCorr (procT []) (procP [] [] [inP "p_id", inP "p_big" (.Ty "INT8" []), inP "p_flag" (.Ty "BOOL" []),
  inP "p_when" date, ioP "p_ts" (.Ty "TIMESTAMP" [])])
#guard !sigCorr (procT []) (procP [] [] [inP "p_id", inP "p_big" integer, inP "p_flag" boolean,
  inP "p_when" date, ioP "p_ts" (ts 6)])

/-! ## Columns and picks -/

-- A `smallint` default on a left-out column; a bit column written from `1`
-- against `TRUE`; a bigint from a bigint; a date key with a literal.
def evKey : CatTable :=
  { name := "evk", cols := [⟨"d", "D", true, true, .date, none⟩, ⟨"b", "B", true, false, .bit, some (.bool false)⟩] }
def Γk : Catalog := { routines := [], tables := [ev, evKey] }
def depk : Deployment := { depv with tables := depv.tables ++ [("dbo.EvK", "evk")] }
#guard evKey.wellFormed
#guard isC2a (check depk Γk ⟨[("@b", "v_b"), (statusVar, statusVar)]⟩
  (tsqlProc ["dbo", "k"] [] [xactAbort, nocount, declT "@b" bit none,
    ins ["dbo", "EvK"] ["D"] [[str "2024-02-29"]],
    selectT [asg "@b" (tc "B")] ["dbo", "EvK"] (some (.Bin "=" (tc "D") (str "2024-02-29")))])
  (pgProc ["k"] [] [.Declare "v_b" (some boolean) none] [
    ins ["EvK"] ["D"] [[str "2024-02-29"]],
    forBlock [als (pv "B") "sqldiff_c1"] ["EvK"] (some (.Bin "=" (pv "D") (str "2024-02-29")))
      [field "v_b" "sqldiff_r" "sqldiff_c1"]]))
-- The inserted row carries the bit default; the pick reads it.
#guard runD (lowerT depk Γk (tsqlProc ["dbo", "k"] [] [xactAbort, nocount, declT "@b" bit none,
    ins ["dbo", "EvK"] ["D"] [[str "2024-02-29"]],
    selectT [asg "@b" (tc "B")] ["dbo", "EvK"] (some (.Bin "=" (tc "D") (str "2024-02-29")))])) [] [] ==
  some (.done [I 0] none 0 [("evk", [[D 2024 2 29, .bool false]])] [])
-- A bit key column, a `datetime2(7)` column, a bit default of `2`: not usable.
#guard !({ evKey with cols := [⟨"b", "B", true, true, .bit, none⟩] } : CatTable).wellFormed
#guard !({ evKey with cols := [⟨"t", "T", true, true, .ts 7, none⟩] } : CatTable).wellFormed
#guard ({ evKey with cols := [⟨"t", "T", true, true, .ts 6, none⟩] } : CatTable).wellFormed
#guard !({ evKey with cols := [⟨"b", "B", true, false, .bit, some (.int 2)⟩] } : CatTable).wellFormed
#guard !({ evKey with cols := [⟨"s", "S", true, false, .int .i16, some (.int 40000)⟩] } : CatTable).wellFormed
-- Column values: a bit from `2` or an int, a `bigint` into a `smallint`
-- column, a date-only literal into a timestamp column: outside.
#guard (lowerT depv Γv (procT [ins ["dbo", "Ev"] ["Id", "B"] [[num 1, num 2]]])).isNone
#guard (lowerT depv Γv (procT [ins ["dbo", "Ev"] ["Id", "B"] [[num 1, tv "@id"]]])).isNone
#guard (lowerT depv Γv (procT [ins ["dbo", "Ev"] ["Id", "B", "S"] [[num 1, num 1, tv "@big"]]])).isNone
#guard (lowerT depv Γv (procT [ins ["dbo", "Ev"] ["Id", "B", "S"] [[num 1, num 1, tv "@id"]]])).isNone
#guard (lowerT depv Γv (procT [ins ["dbo", "Ev"] ["Id", "B", "S"] [[num 1, num 1, num 7]]])).isSome
#guard (lowerT depv Γv (procT [ins ["dbo", "Ev"] ["Id", "B", "T"] [[num 1, num 1, str "2024-01-01"]]])).isNone
#guard (lowerT depv Γv (procT [ins ["dbo", "Ev"] ["Id", "B", "T"] [[num 1, num 1, tv "@when"]]])).isSome
-- `COUNT(*)` into an `int` only: into a `bigint` T-SQL still overflows at
-- 2^31 rows and PostgreSQL does not; into a `smallint` it may not fit.
#guard (lowerT depv Γv (procT [selectT [asg "@n" countStarE] ["dbo", "Ev"] none,
  declT "@n" bigint none])).isNone
#guard (lowerT depv Γv (procT [selectT [asg "@s" countStarE] ["dbo", "Ev"] none,
  declT "@s" smallint none])).isNone
#guard (lowerP Γv (pgProc ["p"] [] [.Declare "v_n" (some bigint) none]
  [.NStmt (.SSelect (sel [item countStarE] ["Ev"] none [pv "v_n"]))])).isNone
#guard (lowerP Γv (pgProc ["p"] [] [.Declare "v_n" (some integer) none]
  [.NStmt (.SSelect (sel [item countStarE] ["Ev"] none [pv "v_n"]))])).isSome
-- A pick of a bigint column into an int variable, or a timestamp into a
-- date, is outside; into its own type or wider it is in.
#guard (lowerT depv Γv (procT [selectT [asg "@i" (tc "N")] ["dbo", "Ev"] (some keyT),
  declT "@i" int none])).isNone
#guard (lowerT depv Γv (procT [selectT [asg "@when" (tc "T")] ["dbo", "Ev"] (some keyT)])).isNone
#guard (lowerT depv Γv (procT [selectT [asg "@ts" (tc "D")] ["dbo", "Ev"] (some keyT)])).isNone
#guard (lowerT depv Γv (procT [selectT [asg "@ts" (tc "T"), asg "@when" (tc "D"), asg "@flag" (tc "B")]
  ["dbo", "Ev"] (some keyT)])).isSome

/-! ## Refusal diagnostics (`why`) -/

def wT (a : Ast.Routine) : List String := whyT depv Γv a
def wP (b : Ast.Routine) : List String := whyP Γv b

#guard wT (storeT "@s" (tv "@id")) == ["store:assign:smallint"]
#guard wT (storeT "@flag" (num 2)) == ["store:assign:bit", "expr:bit-literal"]
#guard wT (storeT "@when" (str "01/02/2024")) == ["store:assign:date", "expr:date-literal"]
#guard wT (storeT "@t" (str "2024-02-29T00:00:00.1234567")) ==
  ["store:assign:datetime2_6", "expr:date-literal"]
#guard wT (storeT "@t" (dateadd "hour" (num 1) (tv "@t"))) == ["expr:dateadd:part"]
#guard wT (storeT "@id" (datediff "month" (tv "@t") (tv "@t"))) == ["expr:datediff:part"]
#guard isC2a (checkS "@s" (.Bin "/" (tv "@s") (tv "@s")) "p_s" (.Bin "/" (pv "p_s") (pv "p_s")))
#guard wT (procT [declT "@s" smallint none, ins ["dbo", "Ev"] ["Id", "B", "S"]
  [[num 1, num 1, .Bin "%" (tv "@s") (tv "@s")]]]) == ["stmt:insert:value", "expr:bin"]
#guard wT (tsqlProc ["dbo", "s"] [] [xactAbort, declT "@x" (dt2 7) none,
  set (tv "@x") (dateadd "day" (num 1) (tv "@x"))]) == ["type:datetime2_7"]
#guard wT (tsqlProc ["dbo", "s"] [] [xactAbort, declT "@x" (.Ty "DATETIME" []) none, declT "@i" int none,
  set (tv "@i") (datediff "day" (tv "@x") (tv "@x"))]) == ["type:datetime"]
#guard wP (storeP "p_flag" (num 1)) == ["store:assign:boolean", "expr:bit-literal"]
#guard wP (pgProc ["s"] [inP "p_t" (.Ty "TIMESTAMPTZ" [])] [] [upd ["Ev"]
  [("T", .Bin "+" (pv "p_t") (mkInterval "days" (num 1)))] (some (.Bin "=" (pv "Id") (num 1)))]) ==
  ["stmt:update:value", "type:timestamptz"]

end Sqldiff.Mirror.ExamplesC2c
