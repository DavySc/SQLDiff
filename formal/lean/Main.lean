/-
`sqldiff-check`: answers one request per line on stdin with one JSON line
on stdout. A request is tab-separated:

  sqldiff-check/2 ID SCHEMA DIALECT-A TARGET-A TREE-A DIALECT-B TARGET-B TREE-B end

ID is digits; SCHEMA is `sqldiff_core::proof::schema_hash`, which must be
`Sqldiff.Syntax.schemaHash`; the trees come from
`sqldiff_core::proof::tree_line` and contain no tab.

The mirror check of phases C1, C2a (with the C2b strings), C3a, C3b and C4 (`Sqldiff.Mirror.check`,
theorems `Sqldiff.Mirror.check_sound` and `Sqldiff.Mirror.check_sound_c2a`)
is a second request kind:

  sqldiff-check/3 ID AST-SCHEMA mirror DEPLOYMENT CATALOG CERT ROUTINE-A ROUTINE-B end

AST-SCHEMA must be `Sqldiff.Ast.schemaHash`; ROUTINE-A (T-SQL, the
reference) and ROUTINE-B (PL/pgSQL) are `sqldiff_syntax::encode` routine
lines, decoded by `Sqldiff.Ast.decodeRoutineLine`; DEPLOYMENT, CATALOG and
CERT are encoded as `Sqldiff.Mirror.Request` documents (C3a adds table
mappings to DEPLOYMENT and table entries to CATALOG, both optional; C2b
adds the optional CATALOG fact `!binary-collation`, which a checker
before 0.6.0 does not decode).
Anything else is answered "unconfirmed".

Answers. `/2` and an unconfirmed `/3`:

  {"id":ID,"proof":"confirmed","theorem":NAME}
  {"id":ID,"proof":"unconfirmed","reason":TEXT}

A confirmed `/3` answer has more fields after `theorem`
(`Sqldiff.Mirror.Request.extra`); a reader that ignores unknown fields
reads it as before:

  {"id":ID,"proof":"confirmed","theorem":"Sqldiff.Mirror.check_sound_c2a",
   "scalars":"c2a","residual":["ASM-TSQL-ARITH-ERRORS",...],"statements":"c3a"}
  {"id":ID,"proof":"confirmed","theorem":"Sqldiff.Mirror.check_sound",
   "scalars":"uninterpreted","statements":"c1"}

`"scalars":"c2a"` means every expression of both routines is in the C2a
slice and every data statement in the C3a slice, so the confirmation does
not rest on operators meaning the same "by name" (`ASM-C1-OPERATORS`);
`residual` lists the facts it rests on instead (possibly none): the T-SQL
session facts `ASM-TSQL-ARITH-ERRORS` and `ASM-TSQL-ANSI-NULLS`, and
(C3a) `ASM-STATE-AFTER-ERROR` when the routine can fail after a write,
and (C2b, strings) `ASM-CONCAT-NULL`, `ASM-BINARY-COLLATION`,
`ASM-TSQL-LEN-BMP` and `ASM-TSQL-STRING-ARGS`. A reader that does not
know a residual must not treat the answer as a C2a one. `"uninterpreted"` means it does rest on `ASM-C1-OPERATORS`.
`"statements"` (C3a) is `"c3a"` when the routines have `INSERT`, `UPDATE`
or `DELETE` statements, whose confirmation also rests on the TABLE entries
of CATALOG and the table mappings of DEPLOYMENT, and `"c1"` otherwise.
`"errors":"c4"` (C4, 0.7.0) follows it when the routines handle errors
(`TRY`/`CATCH` against an exception block), raise them (`THROW` against
`RAISE EXCEPTION`) or control transactions; it is absent otherwise, and
the C4 residuals (`ASM-CATCH-CORE-ERRORS`, `ASM-NO-CALLER-TXN`,
`ASM-THROW-ERRCODE`) are in `residual`. The request format is unchanged;
the abstract syntax schema hash changed with the C4 nodes, so an older
checker answers a C4-era request "unconfirmed" (schema mismatch), and
this checker answers an older client's request the same way.

0.8.0 answers more conservatively, with no new field or residual: a pair
whose parameter or return types do not correspond is unconfirmed ("the
parameter or return types do not correspond", `Sqldiff.Mirror.sigCorr`);
a parameter default that is not NULL or a slice value fitting its type is
outside the spec; a call can fail (a callee's error), so a T-SQL routine
with a call is a `"c2a"` answer only under `SET XACT_ABORT ON`, and names
`ASM-STATE-AFTER-ERROR`; `ERRCODE` must be `'P0001'` exactly; the
unguarded `ROLLBACK; THROW;` idiom needs a body that is exactly one
bracket, and the idiom stands only where transaction statements may.

0.9.0, again more conservative, no new field or residual: every store
into a target that may hold a string (assignment, `DECLARE` default, a
function's `RETURN`, an `OUTPUT`/`INOUT` copy-back) must provably keep
its value, also on the C1 path ("String stores, on every path" in
`Sqldiff/Mirror/Lower.lean`), else the routine is outside the spec.

0.10.0 (C3b) reads tables: T-SQL `SELECT @x = col, ... FROM t WHERE` the
whole primary key `=` against the converter's PL/pgSQL `FOR` block, `IF
[NOT] EXISTS (SELECT 1 FROM t [WHERE c])`, `SET @x = (SELECT col FROM t
WHERE key)` against `x := (...)`, and `COUNT(*)` into a variable ("The
C3b slice: queries" in `Sqldiff/Mirror/Lower.lean`). `"statements"` is
`"c3b"` when the routines read a table (whether or not they also write
one); a routine that counts rows names `ASM-TSQL-ARITH-ERRORS`. The
abstract syntax gained the PL/pgSQL `FOR` loop over a query
(`Node.ForQuery`), so the schema hash changed and an older checker
answers "unconfirmed" (schema mismatch). The CATALOG now also lists the
tables a routine only reads; the request format is unchanged.
-/
import Sqldiff.Gate
import Sqldiff.Mirror.Request

open Sqldiff.Gate

def protocol : String := "sqldiff-check/2"

def mirrorProtocol : String := "sqldiff-check/3"

/-- The JSON answer. `id` is digits and the other strings are constants
of this program, so nothing needs escaping. -/
def answerLine (id : String) (a : Answer) (extra : String := "") : String :=
  match a with
  | .confirmed t => s!"\{\"id\":\"{id}\",\"proof\":\"confirmed\",\"theorem\":\"{t}\"{extra}}"
  | .unconfirmed r => s!"\{\"id\":\"{id}\",\"proof\":\"unconfirmed\",\"reason\":\"{r}\"}"

/-- The answer for a well-formed request. -/
def decide (schema da ta xa db tb xb : String) : Answer :=
  if schema != Sqldiff.Syntax.schemaHash then
    .unconfirmed "the request's syntax schema is not this checker's: generate formal/lean/Sqldiff/Syntax.lean again and rebuild"
  else
    match Sqldiff.Tree.decode xa, Sqldiff.Tree.decode xb with
    | some ta', some tb' => check ⟨da, ta, ta'⟩ ⟨db, tb, tb'⟩
    | none, _ => .unconfirmed "tree A does not decode"
    | _, none => .unconfirmed "tree B does not decode"

def handle (line : String) : String :=
  match line.splitOn "\t" with
  | [p, id, schema, da, ta, xa, db, tb, xb, fin] =>
    if !id.isEmpty && id.all Char.isDigit && fin.startsWith "end" then
      if p == protocol then
        answerLine id (decide schema da ta xa db tb xb)
      else if p == mirrorProtocol && da == "mirror" then
        -- The same ten fields: kind, deployment, catalog, certificate, routines.
        let a := Sqldiff.Mirror.Request.answer schema ta xa db tb xb
        answerLine id a (Sqldiff.Mirror.Request.extra ta xa tb a)
      else
        answerLine "0" (.unconfirmed "malformed request")
    else
      answerLine "0" (.unconfirmed "malformed request")
  | _ => answerLine "0" (.unconfirmed "malformed request")

partial def serve (inp out : IO.FS.Stream) : IO Unit := do
  let line ← inp.getLine
  if line.isEmpty then
    return
  out.putStrLn (handle line)
  out.flush
  serve inp out

def main (args : List String) : IO UInt32 := do
  if args == ["--version"] then
    IO.println s!"sqldiff-check 0.10.0 ({protocol}, {mirrorProtocol})"
    return 0
  serve (← IO.getStdin) (← IO.getStdout)
  return 0
