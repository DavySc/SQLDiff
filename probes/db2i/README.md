# Db2 for i probes

Some Db2 for i facts that SQLDiff relies on could not be read in IBM's documentation
(see [vendor-facts-verification.md](../../docs/vendor-facts-verification.md): V4, V9,
V15 and parts of V3, V5, V7, V14, V19). Until a real IBM i answers them, the code only
uses them as hedged assumptions (`ASM-FACT-V9`, `ASM-FACT-V14`,
`ASM-DB2-SCRIPT-COMMIT`, ...). This folder holds a script that asks a real system, and
the tooling that feeds its answers back.

**Db2 for i only (IBM i 7.x).** Nothing here applies to Db2 for z/OS or Db2 for Linux,
UNIX and Windows, and their documentation is not used as evidence. Nothing in this
folder has been run on an IBM i yet: every fact stays `unverified` until a run is
submitted.

| File | What it is |
|---|---|
| `sqldiff-db2i-probes.sql` | The one script to run on IBM i. Generated, do not edit. |
| `probes.toml` | The probes: SQL, the question, the IBM i source, and every outcome with its verdict. |
| `facts.toml` | The facts and the text in the code that hedges on each one. |
| `status.json` | Status of each fact, computed from the runs. Written by `ingest`. |
| `runs/` | One `.json` (all result rows and verdicts) and one `.md` report per submitted run. |
| `db2i_probes.py` | `build`, `check` and `ingest`. Python 3.11+, standard library only. |

## Running it (Davy, on IBM i)

The script creates the schema `SQLDFPROBE` with `CREATE SCHEMA` (so its tables are
journaled), two tables and about 35 small procedures in it, and declares a few
temporary tables in QTEMP. It reads `SYSIBMADM.ENV_SYS_INFO`, `QSYS2.GROUP_PTF_INFO`,
`QSYS2.SYSROUTINES` and `QSYS2.RECORD_LOCK_INFO`. It changes nothing outside
`SQLDFPROBE` and QTEMP. You need authority to create a schema (a library).

1. Open IBM i Access Client Solutions, Run SQL Scripts, connected with **SQL naming**
   (the script qualifies names as `SQLDFPROBE.T`).
2. If Run SQL Scripts is set to stop on errors, turn that off. The probes record
   their own errors; a statement that fails at the top level should not end the run.
3. Open `sqldiff-db2i-probes.sql` and run all of it.
4. The last result set has one column, `SQLDIFF_PROBE_LINE`, with lines starting
   `SQLDFP|1|`. Save or copy **all** of its rows to a text or CSV file. If that result
   set is lost, run the `SELECT` at the end of the script again: the rows stay in
   `SQLDFPROBE.RESULTS`.
5. Remove everything with `DROP SCHEMA SQLDFPROBE` when you are done. Running the
   script again is safe: it deletes earlier results first.

The probes deliberately run under different commit levels (each procedure has its
own `SET OPTION COMMIT`), so the job's own commit level does not decide the
results. Two probes do record the interface's behaviour (`V5-INTERFACE-COMMIT`,
`V5-ROUTINE-DEFAULT`), so say which interface you used. Running the same script
through `RUNSQLSTM ... NAMING(*SQL)` should also work, but `RUNSQLSTM` cannot show
the final `SELECT`; run that one statement in Run SQL Scripts afterwards. This has
not been tried.

The V25 probes read each probe procedure's USRPRF, DYNUSRPRF and naming from
`QSYS2.PROGRAM_INFO`: the attributes Db2 for i gave it, not yet a check that
the authority really applies. To check the behaviour as well (optional; it
needs a user profile with no authority to `SQLDFPROBE.T`): grant that user
`*EXECUTE` on the library `SQLDFPROBE` and on its four `P_V25*` programs, sign on as that user, run
`CALL SQLDFPROBE.P_V25SQL(?, ?, ?, ?, ?)` (and the same for `P_V25SYS`,
`P_V25USR`, `P_V25OWN`), and note the fourth output (`O_N2`) of each: 0 means
the read of `SQLDFPROBE.T` was allowed, 1 that it was refused. Say in your submission whether you did this.

The result lines contain SQLSTATEs, SQLCODEs, counts, Db2 messages about
`SQLDFPROBE` objects, the IBM i release and PTF group levels. No system name, user
name or business data. Look through the file before you send it anyway.

### Submitting the output

Post the saved file in the project thread, with:

- the interface (for example "ACS 1.1.9.x Run SQL Scripts" or "RUNSQLSTM COMMIT(*CHG)"),
- the IBM i release and TR level, if the `ENV-` lines are empty,
- anything odd you saw (statements that stopped the run, messages in the log).

A run on another release or TR is welcome as a separate submission: results are kept
per run, and a fact that behaves differently between runs becomes `mixed`.

## Feeding results back (maintainers)

```
python3 probes/db2i/db2i_probes.py ingest results.csv --interface acs --run 2026-10-25-v7r5-acs
```

This judges every probe against its outcomes, writes `runs/<run>.json` and
`runs/<run>.md`, and recomputes `status.json` from all runs. It warns when the
results come from an older `probes.toml`. Commit all three.

Each probe outcome has a verdict: `consistent` (the fact or assumption holds),
`refines` (it holds, a detail such as the SQLSTATE is wrong), `contradicts`, or
`open` (an open question, no expectation). Per fact, for one run:

| Status | When |
|---|---|
| confirmed | every decisive probe consistent (open ones allowed) |
| corrected | some decisive probe refines, none contradicts |
| contradicted | some decisive probe contradicts |
| observed | only open answers (V5) |
| inconclusive | the harness check or a control of the fact failed, or a result matched no outcome |

Controls (`role = "control"`) are probes whose result follows from documented
behaviour; when one fails, the probe itself is suspect and the fact stays open.
`H-COMMIT-CONTROL` checks the harness: that `*CS` procedures run under commitment
control and that the result log (`INSERT ... WITH NC`) survives a `ROLLBACK`.

Once a fact is confirmed, corrected or contradicted, `cargo test` fails until

- every hedge listed for it in `facts.toml` is rewritten from the evidence (for
  example "not checked on a real system" in `ASM-FACT-V9`), and
- its summary row in `docs/vendor-facts-verification.md` cites the probe run.

The run report lists exactly these places. A contradicted fact also means the rule
that relies on it must change, not just its wording.

## Changing the probes

Edit `probes.toml`, then:

```
python3 probes/db2i/db2i_probes.py build
python3 probes/db2i/db2i_probes.py check
python3 -m unittest discover -s probes/db2i
cargo test -p sqldiff-corpus --test db2i_probes
```

`check` and the unit tests run in CI. The Rust test parses every probe procedure and
the whole script with the Db2 for i parser; that parser is lenient inside statements,
so it catches broken block structure (BEGIN/END, IF/END IF, handlers) rather than
every grammar error. Only a run on IBM i proves the SQL. Bodies may not contain `--`
comments, because each procedure is sent to `EXECUTE IMMEDIATE` as one line.

Every expectation must name its IBM i 7.x source in `source`, or say that it is only
the assumption the code makes today.
