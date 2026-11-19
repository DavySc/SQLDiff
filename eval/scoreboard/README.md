# Goal scoreboard

One JSON and one table per commit for the numbers the project is measured by
(plan/goals-and-plan-2026-11-12.md, section 1). The `scoreboard` workflow runs
on every push to `main`, on pull requests that touch the engine or `eval/`,
nightly, and on demand.

```sh
cargo build --release -p sqldiff-cli
python3 eval/scoreboard/scoreboard.py run --bin target/release/sqldiff --work /tmp/sb --out /tmp/sb/out
```

Takes about a minute on four cores. It clones the pinned corpora
(`sources.json`: WideWorldImporters and AdventureWorks from
microsoft/sql-server-samples, Ola Hallengren, Sakila), converts the 14 runs
(7 corpora, each to PostgreSQL and Db2 for i, `convert --isolated --fresh`),
runs `eval/cross-dialect-witness/xwmut.py` on the converted WWI OLTP trees and
writes `scoreboard.json`, `scoreboard.md` and `baseline.candidate.json`.

## What is counted

- **Units clean** = converted + converted under assumptions, over all units
  (SQL Server-only features marked impossible, decision D1, are left out of
  both sides). **Files clean** = files whose own status is one of those two.
- **Blockers** are the engine's own rule id (`rules[0]` of the unit) with the
  unit's status, never a regex over message text.
- **Cross-dialect drift** = behaviour-changing WHERE mutants of the converted
  side that the engine reports as different, over all such mutants, plus the
  share among pairs that were equal before the edit.
- **Wall time** is a sum over runs, two runs at a time; not gated.

Only wall time varies between runs of the same binary.

## The gate

The job fails on

1. a **false equal**: a behaviour-changing mutant reported equal;
2. a **witness on a meaning-preserving edit**;
3. a **drop in clean units** of any run, or in different cross-dialect
   mutants, against `baseline.json`.

A change that lowers a number on purpose (a soundness fix: the earlier
number was too high) lowers `baseline.json` in the same PR, with the reason
in the PR description. `baseline.candidate.json` from the job's artifact is
the file to copy. A PR that raises a number should raise the baseline too, so
the gain cannot be lost later.
