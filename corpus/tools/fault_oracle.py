#!/usr/bin/env python3
"""Fault-injection execution oracle for the corpus (design §12).

For every spec in corpus/oracle/*.toml, runs each scenario on both sides of
each listed pair on real engines (PostgreSQL 18, SQL Server 2022), once with
no fault and once per fault point, and compares what a caller can observe:

  - whether an error reached the client, and which error codes when both
    sides run on the same engine,
  - the rows the call returned (results),
  - the durable state of the observed tables, read in a fresh session.

A fault point makes every write to one table fail with a constraint violation
(the most common runtime error kind): a CHECK that no row satisfies for
"<table>" (INSERT, UPDATE, MERGE), or a foreign key from a shadow table for
"<table>:delete" (DELETE of a row that existed before the call). Both are
ordinary statement errors on both engines (SQL Server 547, PostgreSQL 23514 /
23503), not batch-aborting trigger errors. Each run gets a fresh database.

Then it checks the outcome against the corpus:

  - expected "equal" or "equal-under-assumptions" and the oracle shows a
    divergence: the corpus expectation is wrong (soundness failure);
    (`known_divergence = "..."` in the spec records an open question and turns
    this into a warning until the case is fixed);
  - expected "different" and no divergence: the witness was not reproduced,
    unless the spec says why it cannot be (`reproducible = false`);
  - with --sqldiff, the engine said "equal" where the oracle diverges.

Usage:
    python3 corpus/tools/fault_oracle.py --pg "host=localhost user=postgres password=postgres dbname=postgres" \\
        --mssql "docker exec -i mssql /opt/mssql-tools18/bin/sqlcmd -S localhost -U sa -P <pw> -C -I" \\
        [--sqldiff target/debug/sqldiff] [--json report.json] [case-prefix ...]

The --mssql command must read a script from standard input; the oracle adds
the database and output flags. Without --mssql, only pairs whose sides are all
PostgreSQL run (and the other way round).
"""

import argparse
import json
import re
import shlex
import subprocess
import sys
import tomllib
from pathlib import Path

CORPUS = Path(__file__).resolve().parent.parent
RUN_DB = "sqldiff_oracle_run"
ORACLE_DIALECTS = {"tsql", "pg"}


class SetupError(Exception):
    pass


# --- engines -----------------------------------------------------------------

class Postgres:
    dialect = "pg"

    def __init__(self, dsn: str):
        self.dsn = dsn

    def _psql(self, db: str | None, sql: str, stop: bool):
        dsn = self.dsn if db is None else f"{self.dsn} dbname={db}"
        cmd = ["psql", dsn, "-X", "-q", "-A", "-t", "-F", "|", "-P", "null=NULL",
               "-v", "VERBOSITY=verbose", "-v", f"ON_ERROR_STOP={1 if stop else 0}"]
        return subprocess.run(cmd, input=sql, capture_output=True, text=True)

    def reset(self):
        for sql in (f"DROP DATABASE IF EXISTS {RUN_DB} WITH (FORCE);",
                    f"CREATE DATABASE {RUN_DB};"):
            p = self._psql(None, sql, True)
            if p.returncode != 0:
                raise SetupError(p.stderr.strip())

    def load(self, sql: str, what: str):
        p = self._psql(RUN_DB, sql, True)
        if p.returncode != 0 or "ERROR:" in p.stderr:
            raise SetupError(f"{what}: {p.stderr.strip()}")

    def call(self, sql: str):
        p = self._psql(RUN_DB, sql, False)
        errors = [l for l in p.stderr.splitlines() if re.match(r"(psql:.*)?ERROR:", l)]
        return bool(errors), p.stdout, errors

    @staticmethod
    def codes(msgs: list[str]) -> list[str]:
        return [m.group(1) for l in msgs if (m := re.search(r"ERROR:\s+(\w{5}):", l))]

    def query(self, sql: str) -> str:
        p = self._psql(RUN_DB, sql, True)
        if p.returncode != 0:
            raise SetupError(p.stderr.strip())
        return p.stdout

    def write_fault(self, table: str) -> str:
        return f"ALTER TABLE {table} ADD CONSTRAINT sqldiff_fault CHECK (false) NOT VALID;\n"

    def delete_fault(self, table: str, key: str | None) -> str:
        sql = ""
        if key is None:
            key = "sqldiff_rowid"
            sql += (f"ALTER TABLE {table} ADD COLUMN {key} integer "
                    f"GENERATED ALWAYS AS IDENTITY UNIQUE;\n")
        return sql + (f"CREATE TABLE sqldiff_fault_ref AS SELECT {key} FROM {table};\n"
                      f"ALTER TABLE sqldiff_fault_ref ADD FOREIGN KEY ({key}) "
                      f"REFERENCES {table} ({key});\n")

    def observe(self, table: str, cols: list[str]) -> str:
        return f"SELECT {', '.join(cols)} FROM {table};"


class SqlServer:
    dialect = "tsql"

    def __init__(self, command: str):
        self.cmd = shlex.split(command)

    def _sqlcmd(self, db: str, sql: str):
        cmd = self.cmd + ["-d", db, "-h", "-1", "-W", "-s", "|", "-r1"]
        return subprocess.run(cmd, input=sql, capture_output=True, text=True)

    @staticmethod
    def _errors(stderr: str) -> list[str]:
        msgs = []
        for m in re.finditer(r"^Msg (\d+), Level (\d+).*\n(.*)$", stderr, re.M):
            if int(m.group(2)) > 10:
                msgs.append(f"Msg {m.group(1)}: {m.group(3).strip()}")
        if not msgs and stderr.strip():
            # Connection or tool errors carry no Msg header.
            if re.search(r"Sqlcmd: Error|Login failed|error", stderr, re.I):
                msgs.append(stderr.strip().splitlines()[-1])
        return msgs

    def reset(self):
        sql = (f"IF DB_ID('{RUN_DB}') IS NOT NULL BEGIN "
               f"ALTER DATABASE {RUN_DB} SET SINGLE_USER WITH ROLLBACK IMMEDIATE; "
               f"DROP DATABASE {RUN_DB}; END\nGO\n"
               f"CREATE DATABASE {RUN_DB} COLLATE Latin1_General_CI_AS;\nGO\n")
        p = self._sqlcmd("master", sql)
        if self._errors(p.stderr) or p.returncode != 0:
            raise SetupError(p.stderr.strip() or p.stdout.strip())

    def load(self, sql: str, what: str):
        p = self._sqlcmd(RUN_DB, f"SET NOCOUNT ON;\nGO\n{sql}\nGO\n")
        errs = self._errors(p.stderr)
        if errs or p.returncode != 0:
            raise SetupError(f"{what}: {' | '.join(errs) or p.stderr.strip()}")

    def call(self, sql: str):
        p = self._sqlcmd(RUN_DB, f"SET NOCOUNT ON;\n{sql}\nGO\n")
        errs = self._errors(p.stderr)
        return bool(errs), p.stdout, errs

    @staticmethod
    def codes(msgs: list[str]) -> list[str]:
        return [m.group(1) for l in msgs if (m := re.match(r"Msg (\d+):", l))]

    def query(self, sql: str) -> str:
        p = self._sqlcmd(RUN_DB, f"SET NOCOUNT ON;\n{sql}\nGO\n")
        errs = self._errors(p.stderr)
        if errs:
            raise SetupError(" | ".join(errs))
        return p.stdout

    def write_fault(self, table: str) -> str:
        return (f"ALTER TABLE dbo.{table} WITH NOCHECK "
                f"ADD CONSTRAINT sqldiff_fault CHECK (1 = 0);\n")

    def delete_fault(self, table: str, key: str | None) -> str:
        sql = ""
        if key is None:
            key = "sqldiff_rowid"
            sql += (f"ALTER TABLE dbo.{table} ADD {key} int IDENTITY(1,1) NOT NULL "
                    f"CONSTRAINT sqldiff_rowid_uq UNIQUE;\nGO\n")
        return sql + (f"SELECT {key} INTO dbo.sqldiff_fault_ref FROM dbo.{table};\n"
                      f"ALTER TABLE dbo.sqldiff_fault_ref ADD FOREIGN KEY ({key}) "
                      f"REFERENCES dbo.{table} ({key});\n")

    def observe(self, table: str, cols: list[str]) -> str:
        return f"SELECT {', '.join(cols)} FROM dbo.{table};"


# --- normalisation -----------------------------------------------------------

TS_FRACTION = re.compile(r"(\d{2}:\d{2}:\d{2})\.(\d*?)0*(?=$|\|)")


BARE_FRACTION = re.compile(r"^(-?)\.(\d+)$")


def norm_cell(cell: str) -> str:
    # sqlcmd prints decimal 0.50 as .50.
    return BARE_FRACTION.sub(r"\g<1>0.\2", cell)


def norm_line(line: str) -> str:
    line = line.rstrip()
    # 2026-01-01 10:00:00.0000000 (datetime2) and 2026-01-01 10:00:00 (timestamp).
    line = TS_FRACTION.sub(lambda m: m.group(1) + ("." + m.group(2) if m.group(2) else ""), line)
    return "|".join(norm_cell(cell.strip()) for cell in line.split("|"))


def norm_rows(text: str, ordered: bool) -> list[str]:
    rows = [norm_line(l) for l in text.splitlines() if l.strip()]
    return rows if ordered else sorted(rows)


# --- specs -------------------------------------------------------------------

OBSERVE = re.compile(r"^\s*(\w+)\s*\(([^)]*)\)\s*$")


def parse_observe(items: list[str]):
    out = []
    for item in items:
        m = OBSERVE.match(item)
        if not m:
            raise ValueError(f"observe entry {item!r}: expected 'table(col, ...)'")
        out.append((m.group(1), [c.strip() for c in m.group(2).split(",") if c.strip()]))
    return out


def parse_fault(item: str):
    """'table' (write fault) or 'table:delete[:key]'."""
    parts = item.split(":")
    if len(parts) == 1:
        return ("write", parts[0], None)
    if parts[1] == "delete":
        return ("delete", parts[0], parts[2] if len(parts) > 2 else None)
    raise ValueError(f"fault {item!r}: expected 'table' or 'table:delete[:key]'")


def load_specs(prefixes: list[str]):
    specs = []
    for path in sorted((CORPUS / "oracle").glob("*.toml")):
        spec = tomllib.loads(path.read_text("utf-8"))
        spec["_path"] = path
        if prefixes and not any(spec["case"].startswith(p) for p in prefixes):
            continue
        specs.append(spec)
    return specs


def find_case(case_id: str):
    for group in ("pairs", "mutations"):
        d = CORPUS / group / case_id
        if (d / "case.toml").exists():
            return d, tomllib.loads((d / "case.toml").read_text("utf-8"))
    raise ValueError(f"no corpus case {case_id}")


# --- running -----------------------------------------------------------------

def by_dialect(value, dialect: str, what: str, required=True):
    if value is None:
        if required:
            raise ValueError(f"missing {what}")
        return ""
    if isinstance(value, str):
        return value
    if dialect in value:
        return value[dialect]
    if required:
        raise ValueError(f"{what} has no {dialect} entry")
    return ""


def run_side(engine, case_dir: Path, side: dict, spec: dict, scenario: dict, fault):
    d = engine.dialect
    engine.reset()
    engine.load((CORPUS / f"catalog/{d}.sql").read_text("utf-8"), "catalog")
    for what, value in (("seed", spec.get("seed")), ("scenario seed", scenario.get("seed"))):
        sql = by_dialect(value, d, what, required=False)
        if sql:
            engine.load(sql, what)
    engine.load((case_dir / side["file"]).read_text("utf-8"), side["file"])
    if fault is not None:
        kind, table, key = fault
        sql = engine.write_fault(table) if kind == "write" else engine.delete_fault(table, key)
        engine.load(sql, f"fault {table}")
    error, out, msgs = engine.call(by_dialect(scenario.get("call"), d, "call"))
    state = {}
    for table, cols in parse_observe(spec.get("observe", [])):
        state[table] = norm_rows(engine.query(engine.observe(table, cols)), ordered=False)
    return {
        "error": error,
        "codes": engine.codes(msgs),
        "messages": msgs,
        "result": norm_rows(out, ordered=scenario.get("ordered", True)),
        "state": state,
    }


def compare(ra: dict, rb: dict, same_dialect: bool) -> list[str]:
    diffs = []
    if ra["error"] != rb["error"]:
        diffs.append(f"error reached client: {ra['error']} vs {rb['error']}")
    elif same_dialect and ra["codes"] != rb["codes"]:
        # Error codes are only comparable within one engine.
        diffs.append(f"error codes: {ra['codes']} vs {rb['codes']}")
    if ra["result"] != rb["result"]:
        diffs.append(f"result: {ra['result']} vs {rb['result']}")
    for t in ra["state"]:
        if ra["state"][t] != rb["state"].get(t):
            diffs.append(f"durable {t}: {ra['state'][t]} vs {rb['state'].get(t)}")
    return diffs


def engine_verdict(sqldiff: str, case_dir: Path, sa: dict, sb: dict) -> str:
    cmd = [sqldiff, "diff", "--format", "json",
           "--dialect-a", sa["dialect"], "--dialect-b", sb["dialect"],
           str(case_dir / sa["file"]), str(case_dir / sb["file"])]
    p = subprocess.run(cmd, capture_output=True, text=True)
    try:
        return json.loads(p.stdout)["verdict"]
    except (json.JSONDecodeError, KeyError):
        return f"error: {p.stderr.strip()[:200]}"


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--pg", metavar="DSN", help="PostgreSQL connection (any database)")
    ap.add_argument("--mssql", metavar="CMD", help="sqlcmd command reading standard input")
    ap.add_argument("--sqldiff", metavar="BIN", help="also check the engine's verdicts")
    ap.add_argument("--json", metavar="FILE", type=Path, help="write the full report")
    ap.add_argument("prefix", nargs="*", help="only cases whose id starts with one of these")
    args = ap.parse_args()

    engines = {}
    if args.pg:
        engines["pg"] = Postgres(args.pg)
    if args.mssql:
        engines["tsql"] = SqlServer(args.mssql)
    if not engines:
        ap.error("give --pg and/or --mssql")

    failures, warnings, report = [], [], []
    for spec in load_specs(args.prefix):
        case_dir, case = find_case(spec["case"])
        expects = {(e["a"], e["b"]): e for e in case.get("expect", [])}
        faults = [None] + [parse_fault(f) for f in spec.get("faults", [])]
        for a, b in spec["pairs"]:
            sa, sb = case["sides"][a], case["sides"][b]
            pair_id = f"{spec['case']}:{a}~{b}"
            if not {sa["dialect"], sb["dialect"]} <= ORACLE_DIALECTS:
                failures.append(f"{pair_id}: only tsql and pg sides can run on the oracle")
                continue
            if not {sa["dialect"], sb["dialect"]} <= engines.keys():
                warnings.append(f"{pair_id}: skipped, engine not given")
                continue
            expected = expects.get((a, b), {}).get("verdict")
            if expected is None:
                failures.append(f"{pair_id}: no [[expect]] for this pair in case.toml")
                continue
            divergences, runs = [], []
            for scenario in spec["scenario"]:
                for fault in faults:
                    label = f"{scenario['name']} / " + (
                        "no fault" if fault is None else f"fault {fault[1]} {fault[0]}")
                    try:
                        ra = run_side(engines[sa["dialect"]], case_dir, sa, spec, scenario, fault)
                        rb = run_side(engines[sb["dialect"]], case_dir, sb, spec, scenario, fault)
                    except (SetupError, ValueError) as e:
                        failures.append(f"{pair_id} [{label}]: setup failed: {e}")
                        continue
                    diffs = compare(ra, rb, sa["dialect"] == sb["dialect"])
                    runs.append({"run": label, "a": ra, "b": rb, "diverges": diffs})
                    if diffs:
                        divergences.append(f"[{label}] " + "; ".join(diffs))
            status = "diverges" if divergences else "agrees"
            print(f"{pair_id}: expected {expected}, oracle {status} "
                  f"({len(divergences)}/{len(runs)} runs differ)")
            for d in divergences:
                print(f"    {d}")
            known = spec.get("known_divergence")
            if divergences and expected in ("equal", "equal-under-assumptions"):
                if known:
                    warnings.append(f"{pair_id}: known divergence from the corpus: {known}")
                else:
                    failures.append(f"{pair_id}: corpus says {expected} but the oracle diverges")
            elif known:
                failures.append(f"{pair_id}: known_divergence is set but the oracle no longer "
                                f"contradicts the corpus; remove it from the spec")
            if not divergences and expected == "different":
                if spec.get("reproducible", True):
                    failures.append(f"{pair_id}: corpus says different but no run diverged")
                else:
                    warnings.append(f"{pair_id}: not reproducible here: {spec.get('why', '')}")
            verdict = None
            if args.sqldiff:
                verdict = engine_verdict(args.sqldiff, case_dir, sa, sb)
                if divergences and verdict == "equal":
                    failures.append(f"{pair_id}: UNSOUND: sqldiff says equal, oracle diverges")
            report.append({"pair": pair_id, "expected": expected, "engine": verdict,
                           "diverges": bool(divergences), "runs": runs})

    if args.json:
        args.json.write_text(json.dumps(report, indent=2) + "\n", "utf-8")
    for w in warnings:
        print(f"warning: {w}")
    for f in failures:
        print(f"FAIL: {f}", file=sys.stderr)
    print(f"{len(report)} pair(s) checked, {len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
