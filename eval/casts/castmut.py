#!/usr/bin/env python3
"""Cast mutation set: T-SQL against PostgreSQL conversions, checked on the
real engines first and then with `sqldiff diff`.

Each case is a T-SQL expression over table M and its PostgreSQL forms:

- `eq`: a conversion that gives the same results (checked on every row);
- `ne`: forms that give different results on at least one row: wrong
  conversions a converter or a person could write, and conversions kept as
  written where the dialects disagree.

`ground` evaluates every form row by row on SQL Server 2022 and PostgreSQL 18
(docker containers `mssql` and `pg`, see README.md) and fails when a label is
wrong. `run` writes each pair as two views and runs sqldiff with catalogs:
no `ne` form may be equal, and no `eq` form may be different.

    python3 eval/casts/castmut.py ground eval/casts/ground.json
    python3 eval/casts/castmut.py run target/release/sqldiff eval/casts/ground.json out.json
"""

import json
import os
import subprocess
import sys
import tempfile
from datetime import datetime
from decimal import Decimal, InvalidOperation

MS_DDL = """CREATE TABLE dbo.M (Id int NOT NULL PRIMARY KEY, D decimal(10,2) NULL,
  D0 decimal(10,0) NULL, F float NULL, R real NULL, S varchar(20) NULL,
  T datetime2(3) NULL, Tm time(3) NULL, Q int NULL, Ti tinyint NULL, Day date NULL);"""
PG_DDL = """CREATE TABLE M (Id integer NOT NULL PRIMARY KEY, D numeric(10,2) NULL,
  D0 numeric(10,0) NULL, F double precision NULL, R real NULL, S varchar(20) NULL,
  T timestamp(3) NULL, Tm time(3) NULL, Q integer NULL, Ti smallint NULL, Day date NULL);"""

ROWS = [
    "(1, 1.50, 7, 1.5, 0.1, '12', '2026-01-01 10:00:00.600', '10:00:00.600', 12345, 200, '2026-01-02')",
    "(2, -2.50, -3, -2.5, 1.0, '', '1999-12-31 23:59:59.500', '23:59:59.500', -7, 100, '1999-12-31')",
    "(3, 2.49, 0, 1234567, 2.5, ' 7 ', '2026-01-02 03:04:00.000', '03:04:00.000', 7, 5, '2026-01-02')",
    "(4, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)",
    "(5, 0.50, 1, 2.675, 16777217, '1e3', '2026-06-30 12:00:00.250', '12:00:00.250', 1000000, 255, '2026-06-30')",
]
IDS = [1, 2, 3, 4, 5]
# Cases whose result is text: compared character by character.
TEXT = {"int-text", "dec-text", "float-text", "float-literal-text", "ts-text"}

CASES = [
    # Decimal and float to integer: T-SQL truncates, PostgreSQL rounds.
    ("dec-to-int", "CAST(D AS int)", ["CAST(trunc(D) AS integer)"],
     ["CAST(D AS integer)", "CAST(round(D) AS integer)", "D::integer"]),
    ("float-to-bigint", "CAST(F AS bigint)", ["CAST(trunc(F) AS bigint)"],
     ["CAST(F AS bigint)", "CAST(round(F) AS bigint)"]),
    ("convert-float-to-int", "CONVERT(int, F)", ["CAST(trunc(F) AS integer)"], ["CAST(F AS integer)"]),
    ("literal-to-int", "CAST(1.5 AS int)", ["CAST(trunc(1.5) AS integer)", "1"], ["CAST(1.5 AS integer)"]),
    ("neg-literal-to-int", "CAST(-2.5 AS int)", ["CAST(trunc(-2.5) AS integer)"], ["CAST(-2.5 AS integer)"]),
    ("dec0-to-int", "CAST(D0 AS int)", ["CAST(D0 AS integer)"], ["CAST(D0 + 1 AS integer)"]),
    # Decimal scale: both round half away from zero.
    ("dec-scale", "CAST(D AS decimal(10,1))", ["CAST(D AS numeric(10,1))"],
     ["CAST(trunc(D, 1) AS numeric(10,1))", "CAST(D AS numeric(10,2))"]),
    ("int-to-dec", "CAST(Q AS decimal(12,2))", ["CAST(Q AS numeric(12,2))"], ["CAST(Q + 1 AS numeric(12,2))"]),
    # Numbers as text.
    ("int-text", "CAST(Q AS varchar(11))", ["CAST(Q AS varchar(11))", "Q::text"], ["CAST(Q AS varchar(3))"]),
    ("dec-text", "CAST(D AS varchar(30))", ["CAST(D AS varchar(30))"],
     ["CAST(D AS varchar(4))", "CAST(CAST(D AS numeric(10,1)) AS varchar(30))"]),
    ("float-text", "CAST(F AS varchar(30))", [], ["CAST(F AS varchar(30))"]),
    ("float-literal-text", "CAST(1234567e0 AS varchar(30))", ["'1.23457e+006'"], ["CAST(1234567e0 AS varchar(30))"]),
    # Text as numbers.
    ("text-int", "CAST(S AS int)", [], ["CAST(S AS integer)"]),
    ("text-dec", "CAST(S AS decimal(10,2))", [], ["CAST(S AS numeric(10,2))"]),
    # Literal types.
    ("dec-plus", "Q + 1.5", ["Q + 1.5"], ["Q + 1.4", "Q + 1"]),
    ("int-div", "Q / 2", ["Q / 2"], ["Q / 2.0", "Q / 3"]),
    ("big-literal-div", "2147483648 / 7", ["306783378.285714"], ["2147483648 / 7"]),
    # Real against decimal, tinyint arithmetic.
    ("real-cmp", "CASE WHEN R = 0.1 THEN 1 ELSE 0 END", ["CASE WHEN R = CAST(0.1 AS real) THEN 1 ELSE 0 END"],
     ["CASE WHEN R = 0.1 THEN 1 ELSE 0 END"]),
    ("tinyint-add", "Ti + Ti", [], ["Ti + Ti"]),
    ("dec-add", "D + D0", ["D + D0", "D0 + D"], ["D - D0", "D + D0 + 0.01"]),
    # Times.
    ("ts-date", "CAST(T AS date)", ["CAST(T AS date)"], ["CAST(T AS timestamp(0))"]),
    ("date-ts", "CAST(Day AS datetime2(3))", ["CAST(Day AS timestamp(3))"], ["CAST(Day AS timestamp(3)) + interval '1 day'"]),
    ("ts-round", "CAST(T AS datetime2(0))", [], ["CAST(T AS timestamp(0))"]),
    ("time-round", "CAST(Tm AS time(0))", [], ["CAST(Tm AS time(0))"]),
    ("ts-text", "CAST(T AS varchar(40))", [], ["CAST(T AS varchar(40))"]),
]


def sh(cmd, inp):
    return subprocess.run(cmd, input=inp, capture_output=True, text=True).stdout


def ms_eval(exprs):
    """Values of each expression per row on SQL Server ('ERR' on error)."""
    lines = ["SET NOCOUNT ON;", "IF OBJECT_ID('dbo.M') IS NOT NULL DROP TABLE dbo.M;", MS_DDL, "GO"]
    lines += [f"INSERT INTO dbo.M VALUES {r};" for r in ROWS] + ["GO"]
    for k, e in enumerate(exprs):
        for i in IDS:
            lines += [f"PRINT '@@{k}|{i}';", f"SELECT {e} FROM dbo.M WHERE Id = {i};", "GO"]
    out = sh(["docker", "exec", "-i", "mssql", "/opt/mssql-tools18/bin/sqlcmd", "-S", "localhost",
              "-U", "sa", "-P", "Corpus_Passw0rd!", "-C", "-I", "-W", "-h", "-1"], "\n".join(lines) + "\n")
    return parse(out, lambda l: l.startswith("Msg "))


def pg_eval(exprs):
    lines = ["DROP TABLE IF EXISTS M;", PG_DDL]
    lines += [f"INSERT INTO M VALUES {r};" for r in ROWS]
    for k, e in enumerate(exprs):
        for i in IDS:
            lines += [f"\\echo @@{k}|{i}", f"SELECT {e} FROM M WHERE Id = {i};"]
    out = sh(["docker", "exec", "-i", "pg", "psql", "-U", "postgres", "-At", "-f", "-"], "\n".join(lines) + "\n")
    return parse(out, lambda l: l.startswith("ERROR") or "ERROR:" in l)


def parse(out, is_err):
    res, key = {}, None
    for line in out.splitlines():
        if line.startswith("@@"):
            key = line[2:].strip()
            res[key] = None
        elif key is not None and res[key] is None:
            res[key] = "ERR" if is_err(line) else line.strip()
    return res


def same(a, b):
    """Values compared as values: numbers by value, times by instant."""
    if a == b:
        return True
    if a is None or b is None or "ERR" in (a, b):
        return False
    if a == "NULL" or b == "NULL":
        return False
    try:
        if Decimal(a) == Decimal(b):
            return True
        return ("e" in a.lower() or "e" in b.lower() or len(a) > 16 or len(b) > 16) and float(a) == float(b)
    except (InvalidOperation, ValueError):
        pass
    for fmt in ("%Y-%m-%d %H:%M:%S.%f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d", "%H:%M:%S.%f", "%H:%M:%S"):
        try:
            if datetime.strptime(a, fmt) == datetime.strptime(b, fmt):
                return True
        except ValueError:
            continue
    try:
        return parse_time(a) == parse_time(b)
    except ValueError:
        return False


def parse_time(s):
    for fmt in ("%Y-%m-%d %H:%M:%S.%f", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d", "%H:%M:%S.%f", "%H:%M:%S"):
        try:
            return datetime.strptime(s, fmt)
        except ValueError:
            continue
    raise ValueError(s)


def norm_null(v):
    return "NULL" if v in ("", None) and v is not None else v


def ground(path):
    ms = ms_eval([c[1] for c in CASES])
    pg_forms = [(ci, f, kind) for ci, c in enumerate(CASES) for kind, fs in (("eq", c[2]), ("ne", c[3])) for f in fs]
    pg = pg_eval([f for _, f, _ in pg_forms])
    bad, out = [], []
    for k, (ci, form, kind) in enumerate(pg_forms):
        name, tsql = CASES[ci][0], CASES[ci][1]
        rows = []
        for i in IDS:
            a = ms.get(f"{ci}|{i}")
            b = pg.get(f"{k}|{i}")
            b = "NULL" if b == "" else b
            rows.append((i, a, b, a == b if name in TEXT else same(a, b)))
        agree = all(r[3] for r in rows)
        if (kind == "eq") != agree:
            bad.append(f"{name}: {kind} form `{form}` " + ("differs" if kind == "eq" else "agrees on every row")
                       + f": {[(i, a, b) for i, a, b, s in rows if not s] or rows}")
        out.append({"case": name, "tsql": tsql, "pg": form, "kind": kind,
                    "rows": [{"id": i, "tsql": a, "pg": b} for i, a, b, _ in rows]})
    json.dump(out, open(path, "w"), indent=1)
    print(f"{len(out)} forms checked on both engines, {len(bad)} mislabelled")
    for b in bad:
        print("  " + b)
    return 1 if bad else 0


def run(binary, ground_path, out_path):
    forms = json.load(open(ground_path))
    tmp = tempfile.mkdtemp()
    open(os.path.join(tmp, "cat.tsql.sql"), "w").write(MS_DDL + "\n")
    open(os.path.join(tmp, "cat.pg.sql"), "w").write(PG_DDL + "\n")
    results, false_equal, false_diff = [], [], []
    for k, f in enumerate(forms):
        a = os.path.join(tmp, f"a{k}.sql")
        b = os.path.join(tmp, f"b{k}.sql")
        open(a, "w").write(f"CREATE VIEW dbo.V AS SELECT Id, {f['tsql']} AS x FROM dbo.M;\n")
        open(b, "w").write(f"CREATE OR REPLACE VIEW V AS SELECT Id, {f['pg']} AS x FROM M;\n")
        p = subprocess.run([binary, "diff", "--no-cache", "--format", "json", "--dialect-a", "tsql",
                            "--dialect-b", "postgres", "--catalog-a", os.path.join(tmp, "cat.tsql.sql"),
                            "--catalog-b", os.path.join(tmp, "cat.pg.sql"), a, b], capture_output=True, text=True)
        r = json.loads(p.stdout)
        verdict = r["verdict"]
        rules = sorted({x["rule"] for x in r.get("findings", [])})
        results.append({**{x: f[x] for x in ("case", "tsql", "pg", "kind")}, "verdict": verdict, "rules": rules})
        if f["kind"] == "ne" and verdict.startswith("equal"):
            false_equal.append(results[-1])
        if f["kind"] == "eq" and verdict == "different":
            false_diff.append(results[-1])
    json.dump(results, open(out_path, "w"), indent=1)
    by = {}
    for r in results:
        by.setdefault((r["kind"], r["verdict"]), 0)
        by[(r["kind"], r["verdict"])] += 1
    for (kind, v), n in sorted(by.items()):
        print(f"{kind:3} {v:24} {n}")
    for r in false_equal:
        print(f"FALSE EQUAL: {r['case']}: {r['tsql']}  vs  {r['pg']}")
    for r in false_diff:
        print(f"FALSE DIFFERENT: {r['case']}: {r['tsql']}  vs  {r['pg']} {r['rules']}")
    return 1 if false_equal or false_diff else 0


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "ground":
        sys.exit(ground(sys.argv[2]))
    if len(sys.argv) >= 5 and sys.argv[1] == "run":
        sys.exit(run(sys.argv[2], sys.argv[3], sys.argv[4]))
    print(__doc__)
    sys.exit(2)
