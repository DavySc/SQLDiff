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

An unconfirmed `/3` answer whose reason is "the T-SQL routine is outside
the C1 spec" or "the PL/pgSQL routine is outside the C1 spec" has one
more field after `reason` (0.13.0, `Sqldiff.Mirror.Request.extra`):

  {"id":ID,"proof":"unconfirmed","reason":TEXT,"why":["stmt:select:from:join","expr:subquery"]}

`why` names the constructs the lowering refused in that routine, as reason
codes `family:detail` (the table is in README.md, "Refusal diagnostics");
it is untrusted, informational, never empty there and absent everywhere
else.

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

0.11.0 (C3c) takes wider tables: string columns (a column SPEC `v<n>` or
`w<n>`, T-SQL `varchar(n)`/`nvarchar(n)` against PostgreSQL `varchar(n)`)
and constant defaults (a fourth column field, `i<int>` or `s<hex>`), "The
C3c tables" in `Sqldiff/Mirror/Lower.lean`. A request without either
decodes as before, so the format stays backward compatible; an older
checker answers a request with them "the catalog does not decode". No new
answer field or residual: a confirmation over string columns is a
`"c3a"`/`"c3b"` answer whose residual names the C2b facts it uses.

0.12.0: T-SQL and PL/pgSQL `COMMIT` lower to nothing (they changed
nothing a finished call shows before either), so a conversion with a
`COMMIT` after each statement T-SQL commits on its own (autocommit) is
read like one without. The residual and the `"errors":"c4"` field now
also come from the PL/pgSQL routine's `COMMIT`s, so such a confirmation
still names `ASM-NO-CALLER-TXN`. Request format unchanged.

0.13.0: an unconfirmed answer whose routine is outside the spec carries
`"why"`, the reason codes above (`Sqldiff/Mirror/Diagnose.lean`). No
answer changes: the codes are computed after the check, from the
untrusted diagnostic, and no theorem depends on them. Every reason text,
every other field and the request format are unchanged.

0.14.0 (C2c) reads typed values (integer widths, `bit` against `boolean`,
`date` and `datetime2(p)` against `timestamp(p)`) and typed catalog
columns (SPEC `j`, `h`, `b`, `d`, `t<p>`). A ROUTINE entry may carry its
parameters' types (`NAME:MODES:TYPES`); a call's arguments must have
exactly those types, so a request that names a callee without them has
no call in the spec (an older client's calls are refused, never
confirmed). A pair whose variables' declared types differ is answered
"the declared types of the variables do not correspond". New reason
codes `type:<name>`, `expr:date-literal`, `expr:bit-literal`,
`expr:dateadd:part`, `expr:datediff:part`, `node:call:type`.

0.15.0 names one more residual, `ASM-TSQL-ROWCOUNT-0`, in a `"c2a"`
answer whose T-SQL routine has a data statement or a query (`"statements"`
`"c3a"` or `"c3b"`): callers run the routine with `SET ROWCOUNT 0`, the
default. A caller's `SET ROWCOUNT n` is inherited by the routine and
limits the rows its `INSERT`, `UPDATE`, `DELETE` and `SELECT` statements
touch or read, and PostgreSQL has no such setting (eval/session). No
other answer changes; a reader that does not know the id must not treat
the answer as a C2a one. Request format unchanged.

0.16.0 (C3d part 1) reads query blocks in the C3b statement shapes:
several catalog tables, `[INNER] JOIN ... ON` and `CROSS JOIN`, comma
lists, aliases (self-joins too) and qualified names ("The C3d blocks" in
`Sqldiff/Mirror/Lower.lean`). `SELECT @x = ...` and its `FOR` block need
the key closure (`Core.Block.atMostOne`, `Core.kept_length_le_one`); a
scalar subquery stands over any block, its more-than-one-row error (512,
21000) the new error class `cardinality`. A confirmation that reads
tables still carries `"statements":"c3b"`. New reason codes
`stmt:select:many-rows` (in place of `stmt:select:where:not-key`),
`stmt:select:from:join:<kind>` (in place of `stmt:select:from:join`),
`stmt:select:from:qualifier`, `stmt:select:from:ref`, `name:ambiguous`,
`name:column-variable`; `stmt:select:from:alias` and
`stmt:select:from:multi` are gone. Review of C3d: `ASM-NO-CALLER-TXN` is
also named in a `"c2a"` answer whose routine can fail (an error under
`XACT_ABORT ON` dooms a caller's T-SQL transaction, eval/queries Q15),
and a table is sent only when spelt as its DDL declares it. Request
format unchanged.

0.17.0 (C3d part 2) reads read-only tables: a CATALOG TABLE entry may end
in `=r` (`CatTable.readOnly`), and its columns may have the SPEC `o`, an
opaque column (`OpqCol`: a type outside the slice, a name only). No
`INSERT`, `UPDATE` or `DELETE` writes a read-only table (`writeTableOf`,
`writeTableOf_not_readOnly`); the core reads its projection on the slice
columns, with its primary key only when the key is wholly visible
(`CatTable.keyVisible`, `CatTable.keyVisible_of_hasKey`); an opaque column
counts for name resolution (ambiguity, the PL/pgSQL column-variable
conflict) and resolves to nothing ("The C3d tables" in
`Sqldiff/Mirror/Lower.lean`). In a read-only table a string key column
is usable and leaves the table without a key. A confirmation that reads
one still carries `"statements":"c3b"`. New reason codes
`stmt:insert:table:read-only`, `stmt:update:table:read-only`,
`stmt:delete:table:read-only` and `stmt:select:column:opaque`. An older
checker answers a request with `=r` or `o` "the catalog does not
decode"; this checker reads every older request as before.
-/
import Sqldiff.Gate
import Sqldiff.Mirror.Request

open Sqldiff.Gate

def protocol : String := "sqldiff-check/2"

def mirrorProtocol : String := "sqldiff-check/3"

/-- The JSON answer. `id` is digits and the other strings are constants
of this program (the `why` codes are ASCII `[a-z0-9:_/.-]`), so nothing
needs escaping. -/
def answerLine (id : String) (a : Answer) (extra : String := "") : String :=
  match a with
  | .confirmed t => s!"\{\"id\":\"{id}\",\"proof\":\"confirmed\",\"theorem\":\"{t}\"{extra}}"
  | .unconfirmed r => s!"\{\"id\":\"{id}\",\"proof\":\"unconfirmed\",\"reason\":\"{r}\"{extra}}"

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
        answerLine id a (Sqldiff.Mirror.Request.extra ta xa tb xb a)
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
    IO.println s!"sqldiff-check 0.17.0 ({protocol}, {mirrorProtocol})"
    return 0
  serve (← IO.getStdin) (← IO.getStdout)
  return 0
