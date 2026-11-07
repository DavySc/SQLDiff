# Behavioural diff, witnesses, ledger and "what if"

Design §3 Layer 5 item 4, §6.1, §6.2, §7, §9 and milestones M5a/M5b. Code:
`crates/sqldiff-core/src/ir/behaviour.rs` (paired exploration),
`ir/witness.rs` (structured witnesses), `ir/overlay.rs` (transaction overlay),
`ledger.rs` (assumption ledger). Gate tests: `crates/sqldiff-core/tests/witnesses.rs`.

## Paired exploration

Both sides are lowered to the CFG of `ir::cfg` and run by the transaction
interpreter of `ir::interp` under the same scenarios:

- every statement succeeds (baseline);
- each aligned write fails with each error kind either side's profile says it
  can raise;
- the caller already holds an open transaction.

Writes are aligned by key (kind, folded target, occurrence). A MERGE whose
branch (`THEN DELETE`, `THEN UPDATE`, `THEN INSERT`) the other side runs as a
separate statement on the same table is aligned with that statement too.

Per scenario and path, the outcome tuples are compared. Each kind of divergence
becomes one finding, anchored to both sides, with a plain-language witness and
a structured one.

A fault scenario runs exactly like the baseline until the exploration first
reaches the failing statement (the scenario's faults are consulted only
there), so `ir::Explorer` runs the baseline once, keeps checkpoints of the
whole exploration, and resumes each fault scenario from the checkpoint before
its statement. The outcomes are the ones a full run gives, in the same order
(`crates/sqldiff-core/tests/explorer.rs` checks every statement and error kind
on the corpus); this keeps routines of several hundred statements well under
a second.

A faulted run that reaches, at the same statement, a state equal to the
fault-free run's in everything the rest of the run reads (the position, the
variables, the transaction and savepoint stack, the handlers, the temporary
state, the statements still to be reported and the loop counters) does not
run on: the rest of its exploration would repeat the fault-free one step for
step, because the step function is deterministic and reads only that state.
Its outcome is the fault-free outcome with the entries the fault changed
patched in, and it is held as such (`ir/delta.rs`) so that neither making nor
comparing it costs the size of the routine. A run merges only when its fault is spent and the failing statement
cannot run again from there, and only into a fault-free tick at the same
statement with the same step count and the same number of paths started, so
the step and path budgets cut both runs at the same place and a budget-cut
statement is still reported as not proven. In a transaction the fault-free
path has a log entry for the failing statement's write and the faulted one
has none; the logs are compared without those entries and the positions
savepoints and scopes keep in them are counted without them, so a script in
one transaction merges like a script of autocommitted statements (a merge
into a path that still has such entries is refused when a loop with an
unknown condition and a commit, rollback or raise in it can be reached,
because its summary reads the log's length). A read of a temporary table
that only the fault-free path made, by a statement that cannot run again, is
not carried over. A state that differs in anything else the rest of the run
reads is not merged and runs on to its end. That includes the aborted
transaction of PostgreSQL: after a failure every earlier write is undone and
every later statement fails, so the outcome differs from the fault-free one
in every key and there is no fault-free state to merge into.
`tests/explorer.rs` compares every outcome with a full exploration, on the
corpus and on generated routines (loops, handlers, savepoints, budget edges,
data scripts), with merging active. Converting a script of 2000 `INSERT`
statements went from 41 s to 1.4 s with an identical report.

| Rule | Class | Divergence |
|---|---|---|
| `txn.statement-atomicity` | transactional | One side keeps the failing statement's rows changed before the error |
| `txn.atomicity` | transactional | Work done before the failure is undone on one side and kept on the other, including a statement split in two |
| `txn.durability` | transactional | Work is durable on one side only (also work that runs after the error on one side only) |
| `txn.caller-transaction` | transactional | Differences that exist only when the caller holds a transaction (equal under `ASM-NO-CALLER-TXN`) |
| `txn.caller-continuation` | transactional | One side reports the error and carries on with more statements, so the call returns and the caller's batch goes on and can commit; the other raises, so the calling statement fails |
| `txn.temp-state` | transactional | A statement reads temporary state that one side rolled back |
| `error.control` | semantic | Statements run after the error on one side only |
| `error.contract` | semantic | The caller sees different endings: return code, normal return or SQLSTATE |
| `concurrency.split-statement` | concurrency | One statement on one side is several on the other, each reading the source at its own time |
| `assumption.contradiction` | transactional | A declared fact contradicts the object's own source |
| `equiv.budget` | transactional | A step or path budget cut the exploration before this statement, so its behaviour was not run; the statement is not proven. The step budget counts CFG nodes, several per statement, so in a procedure it cuts at roughly the 1000th statement of 4000 steps (the budget stays fixed until state merging lands) |

`txn.caller-continuation` needs the continuing side to run at least one more
statement. A routine whose only statement fails reports an error either way, so
p08 stays equal, while p07 (T-SQL goes on to `SET @OrderId = SCOPE_IDENTITY()`)
and p01 (T-SQL goes on to `ROLLBACK; RETURN n`) diverge, as the oracle shows.

Paths are matched by the conditions they take; condition text is compared with
case, quoting, default schemas (`dbo.`, `public.`) and parameter prefixes
(`@`, `p_`) folded. A row-count test (`@@ROWCOUNT = 0`, `NOT FOUND`,
`SQLCODE = 100`) is named by the statement whose count it reads and is recorded
as "no rows from" that statement, so `IF @@ROWCOUNT = 0` on one side and
`IF FOUND ... ELSE` on the other mean the same thing in a path. When the two
sides' paths cannot be matched one to one, every combination is compared except
those no input can produce (the same condition, decided once on each path, with
two values; a condition decided more than once, as a loop test is, never rules
a pair out), and a divergence that does not hold for all of them is "unknown".

### Path joining

When the other side of a comparison makes no decision (its CFG has no
`Test::Unknown` and no `Test::NoRows` node, so every scenario has one path
with an empty signature), a side's fault-free exploration drops a path that
reaches a node with several predecessors in the same state as an earlier path
whose subtree is finished. "State" is every field of `State` but `steps`, the
trace, the signature and the names of row-count tests (`run_key`): the
run state *and* the history that reaches an outcome (`partial`, `temp_reads`,
`raised`, `rows`, `assumptions`, `untested`, `imprecise`, `cut`). Two such
paths run on identically, so the outcomes they would make are equal in every
field a comparison reads; only `trace` and `signature` differ, and the
other side's empty signature makes `pairs` pair every combination whatever
the signatures are. Dropping duplicates therefore leaves the set of distinct
comparison results, and with it every verdict and the text of every finding
(the first outcome pair showing a divergence is the earlier path's), as it
was.

* The step budget: the dropped path has used at least as many steps as the
  earlier one, or the earlier subtree was not cut by the step budget, so its
  continuation fits wherever the earlier one did. The path budget counts
  outcomes, so dropped paths free it: more of the routine is explored, and a
  routine still over the budget cuts different branches than before.
* A run with joining is bounded like one without: after 200,000 ticks it
  stops forking (an `imprecise` path budget cut). The memo holds at most
  10,000 entries; a full memo only stops *making* entries (a re-run makes
  none, and sees an entry absent before its arrival either way), so the size
  of the memo never changes where a re-run forks.
* The memo belongs to the `Explorer` and is shared with its checkpoints; an
  entry is visible from the tick its subtree finished, so a re-run from a
  checkpoint drops the same paths. No tick on the segment of a dropped path
  is a merge target for a fault replay: the joined tick and every tick opened
  after the entry's arrival (their outcome slices lack the dropped
  continuation, which is the earlier path's). The
  replay itself does not join. Joining is a property of the pair: the same
  routine against another counterpart may explore differently.

## Structured witnesses

`Report.witnesses` holds one `Witness` per behavioural finding, by fingerprint:

```json
{
  "fingerprint": "…", "rule": "txn.atomicity",
  "scenario": { "kind": "fault", "statement": "the INSERT into stock_log",
                "error_kind": "constraint", "line_a": 7, "line_b": 6,
                "caller_transaction": false, "xact_abort": false,
                "text": "If the INSERT into stock_log fails with a constraint violation (A line 7; B line 6)" },
  "a": { "control": "reports the error and goes on with the next statement (line 8)",
         "exit": "returns code 2", "rollback": "the failing statement and the UPDATE of stock",
         "transaction": "no transaction left open", "undone": ["the UPDATE of stock (A line 5)"],
         "path": [3, 4, 5, 7, 8] },
  "b": { "control": "leaves the routine with the error", "exit": "exits and the error (SQLSTATE) propagates to the caller",
         "durable": ["the UPDATE of stock (B line 5)"], "path": [5, 6], "…": "…" }
}
```

The MCP `finding` tool returns it with the finding.

## What if this fails?

`sqldiff_core::what_if(a, b, opts, side, line, kind)` runs one scenario: the
statement on (or before) `line` of `side` fails with `kind`. The statement is
aligned by its write or, without one, by kind and order. The answer lists each
path's outcome on both sides, the divergences under that scenario and notes
(no counterpart, the kind cannot be raised on one side).

```
sqldiff what-if tsql.sql pg.sql --line 7 --error constraint
sqldiff what-if tsql.sql pg.sql --line 7 --deny ASM-TSQL-CALLER-XACT-ABORT-OFF -f json
```

MCP: `what_if {pair | a, b, line, side?, error_kind?}`. GUI server:
`GET /api/what_if?line=7&side=a&error=constraint`.

## Transaction overlay

`sqldiff_core::overlay(a, b)` returns brackets per side: transactions (from
BEGIN, or the start of the body where transactions start implicitly, to every
COMMIT or ROLLBACK that ends them), who owns the transaction when the body has
no transaction control (the calling statement, the CALL, a unit of work, no
commitment control under `*NONE`), savepoints, ATOMIC compounds, blocks with
EXCEPTION, TRY blocks and handlers. Each bracket is paired with the n-th bracket
of the same family on the other side.

CLI `sqldiff overlay A B`, MCP `overlay`, GUI `GET /api/overlay`.

## Assumption ledger

Sources, strongest first: a session flip, the object's own source, composed
output, the catalog, an author's declaration, pack or project config, the
tool's default.

- **Proven from source.** `SET XACT_ABORT ON` in a T-SQL routine and
  `SET OPTION COMMIT = *X` in a Db2 for i routine discharge the matching
  default (`Report.discharged`, ledger status `discharged`).
- **Declared.** Pack `[assumptions]` entries mark an assumption declared. A
  declared `ASM-DB2-COMMIT-LEVEL` whose text names a commit level (`*CHG`,
  `*NONE`, …) is used for routines that do not state one, so their durability
  is no longer unknown.
- **Contradictions.** A declared commit level that differs from the routine's
  own `SET OPTION COMMIT` is an `assumption.contradiction` finding (verdict
  unknown).
- **Re-run on flip.** Denying `ASM-NO-CALLER-TXN` (the caller holds a
  transaction) or `ASM-TSQL-CALLER-XACT-ABORT-OFF` (the caller runs with
  XACT_ABORT ON) re-runs the behavioural analysis under that scenario, which
  gives new witnesses (the worked example's "both variants"). Any other flip
  relabels: a denied assumption makes the findings relying on it unknown.
- **Ranking.** Assumptions whose flip changes the verdict come first, then open
  ones, then by source.

`sqldiff_core::ledger::ledger(a, b, opts)` returns the report under
`opts.flips` and the ranked entries. CLI: `sqldiff assumptions A B [--deny ID]
[--confirm ID]`, and `sqldiff diff A B --deny ID` re-runs. MCP `assumptions`
and `set_assumption` re-run pairs held from `diff`; pairs from `queue` are
relabelled only.

## M5a: T-SQL ↔ PostgreSQL

Proved on SQL Server 2022 and PostgreSQL 18 with the fault-injection oracle
(`corpus/tools/fault_oracle.py --sqldiff target/debug/sqldiff`): 25 pairs
checked, 0 failures, and no "equal" from the engine where the oracle diverges.
The divergences the oracle reproduces are the ones the engine reports, pinned
in `tests/witnesses.rs`:

| Case | Oracle | Engine |
|---|---|---|
| p01 tsql~pg | constraint fault on `stock` / `stock_log`: SQL Server returns 1 / 2, PostgreSQL errors | `txn.caller-continuation`, `error.contract` |
| p07 tsql~pg | fault on `orders`: SQL Server returns `@OrderId = NULL`, PostgreSQL errors | `txn.caller-continuation` |
| p10 tsql~pg | non-positive price: SQL Server writes audit rows, PostgreSQL none | `txn.temp-state` |
| m02, m05 (T-SQL) | durable state differs after a fault / rollback | `txn.atomicity`, `txn.temp-state` |
| p02, p08, p09, p13 tsql~pg | agree | not different |

The worked example (p01 tsql~db2i) yields its three witnesses, and with
`--deny ASM-TSQL-CALLER-XACT-ABORT-OFF` the XACT_ABORT ON variant.

## M5b: Db2 for i, capped

Db2 for i has no container, so its rows come from the documentation
(`docs/vendor-facts-verification.md`). A divergence that relies on a profile
row marked `unverified` (V4: ATOMIC and UNDO under `*NONE`; also T-SQL's
batch-aborting conversion errors) is shown with its witness but capped at
"unknown", with "Not proven: it relies on vendor fact V4 (…), which is not
tested on a real system". No corpus expectation relies on such a row today;
V9 and V15 have no profile row yet, so nothing can rely on them. Db2 for i
expectations met: mutations m06, m07, m08, m09 and m12 (T-SQL → Db2), and
p01, p02, p07, p09 (both pairs), p04, p05, p11 (pg~db2i) and p12 (tsql~db2i).
The rest are listed in `crates/sqldiff-corpus/known-failing.toml` (cardinality
and binding work, WP1/WP10).
