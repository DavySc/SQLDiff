#!/usr/bin/env python3
"""Validate the SQLDiff test corpus.

Checks every case.toml against the format in corpus/README.md, the text hygiene
of every fixture, mutation-catalog coverage and the manifest. With --pg, also
creates every PostgreSQL fixture on a live server (inside a rolled-back
transaction, on top of catalog/pg.sql), and with --mssql every T-SQL fixture
on a SQL Server (in a scratch database, on top of catalog/tsql.sql). With --deny, scans the corpus for terms
from a deny-list that must never appear in fixtures (design §5.6).

Usage:
    python3 corpus/tools/check_corpus.py
    python3 corpus/tools/check_corpus.py --pg "host=/tmp port=5432 user=postgres dbname=corpus"
    python3 corpus/tools/check_corpus.py --mssql "sqlcmd -S localhost -U sa -P <pw> -C -b -I"
    python3 corpus/tools/check_corpus.py --deny /path/to/deny-list.txt
"""

import argparse
import re
import shlex
import subprocess
import sys
import tomllib
from pathlib import Path

CORPUS = Path(__file__).resolve().parent.parent

DIALECTS = {"tsql", "pg", "db2i"}
VERDICTS = {"equal", "equal-under-assumptions", "different", "unknown"}
CLASSES = {
    "cosmetic", "structural", "semantic", "cardinality", "transactional",
    "concurrency", "binding", "residue", "limit",
}
DETECTED_BY = {"parse-lint", "early-check", "behavioural"}
MILESTONES = {"M2", "M3", "M4", "M5a", "M5b", "M6"}
FACT_ID = re.compile(r"^V\d+$")

CASE_KEYS = {"id", "title", "refs", "description", "tags", "sides", "expect",
             "note", "substitutes", "pack"}
MUTATION_KEYS = {"catalog_item", "detected_by", "milestone"}
SIDE_KEYS = {"dialect", "file", "create"}
EXPECT_KEYS = {"a", "b", "verdict", "classes", "assumptions", "discharged",
               "facts", "witnesses"}

errors: list[str] = []


def err(where, msg):
    errors.append(f"{where}: {msg}")


def check_text(path: Path):
    data = path.read_bytes()
    rel = path.relative_to(CORPUS)
    if data.startswith(b"\xef\xbb\xbf"):
        err(rel, "has a UTF-8 BOM")
    try:
        data.decode("utf-8")
    except UnicodeDecodeError as e:
        err(rel, f"not UTF-8: {e}")
    if b"\r" in data:
        err(rel, "contains CR; use LF line endings")
    if data and not data.endswith(b"\n"):
        err(rel, "does not end with a newline")
    if re.search(rb"[ \t]+\n", data):
        err(rel, "has trailing whitespace")


def check_case(case_dir: Path, kind: str):
    rel = case_dir.relative_to(CORPUS)
    path = case_dir / "case.toml"
    if not path.exists():
        err(rel, "missing case.toml")
        return None
    try:
        case = tomllib.loads(path.read_text("utf-8"))
    except tomllib.TOMLDecodeError as e:
        err(rel, f"case.toml: {e}")
        return None

    allowed = CASE_KEYS | (MUTATION_KEYS if kind == "mutation" else set())
    for key in case.keys() - allowed:
        err(rel, f"unknown key {key!r}")
    for key in ("id", "title", "refs", "sides", "expect"):
        if key not in case:
            err(rel, f"missing key {key!r}")
    if case.get("id") != case_dir.name:
        err(rel, f"id {case.get('id')!r} does not match the directory name")

    sides = case.get("sides", {})
    for name, side in sides.items():
        for key in side.keys() - SIDE_KEYS:
            err(rel, f"side {name}: unknown key {key!r}")
        if side.get("dialect") not in DIALECTS:
            err(rel, f"side {name}: dialect must be one of {sorted(DIALECTS)}")
        if side.get("create", "ok") not in {"ok", "error"}:
            err(rel, f"side {name}: create must be 'ok' or 'error'")
        file = side.get("file")
        if not file or not (case_dir / file).is_file():
            err(rel, f"side {name}: file {file!r} not found")
    listed = {s.get("file") for s in sides.values()}
    for f in case_dir.glob("*.sql"):
        if f.name not in listed:
            err(rel, f"{f.name} is not referenced by any side")

    for i, exp in enumerate(case.get("expect", [])):
        where = f"{rel} expect[{i}]"
        for key in exp.keys() - EXPECT_KEYS:
            err(where, f"unknown key {key!r}")
        for key in ("a", "b"):
            if exp.get(key) not in sides:
                err(where, f"{key} = {exp.get(key)!r} is not a side")
        verdict = exp.get("verdict")
        if verdict not in VERDICTS:
            err(where, f"verdict must be one of {sorted(VERDICTS)}")
        for c in exp.get("classes", []):
            if c not in CLASSES:
                err(where, f"unknown class {c!r}")
        for f in exp.get("facts", []):
            if not FACT_ID.match(f):
                err(where, f"fact {f!r} is not a design §13 id like V12")
        if verdict == "different" and not exp.get("witnesses"):
            err(where, "a 'different' verdict needs at least one witness")
        if verdict == "equal-under-assumptions" and not exp.get("assumptions"):
            err(where, "an 'equal-under-assumptions' verdict needs assumptions")
        if verdict == "equal" and exp.get("assumptions"):
            err(where, "an 'equal' verdict cannot carry assumptions")

    for sub in case.get("substitutes", []):
        if sub.get("side") not in sides:
            err(rel, f"substitute side {sub.get('side')!r} is not a side")

    if "pack" in case and not (case_dir / case["pack"]).resolve().is_file():
        err(rel, f"pack {case['pack']!r} not found")

    if kind == "mutation":
        if set(sides) != {"original", "mutated"}:
            err(rel, "a mutation has exactly the sides 'original' and 'mutated'")
        if case.get("detected_by") not in DETECTED_BY:
            err(rel, f"detected_by must be one of {sorted(DETECTED_BY)}")
        if case.get("milestone") not in MILESTONES:
            err(rel, f"milestone must be one of {sorted(MILESTONES)}")
        if not any(e.get("a") == "original" and e.get("b") == "mutated"
                   and e.get("verdict") == "different"
                   for e in case.get("expect", [])):
            err(rel, "a mutation must expect original vs mutated to be 'different'")
    return case


def check_mutation_catalog(mutations: dict):
    catalog = tomllib.loads((CORPUS / "mutations/CATALOG.toml").read_text("utf-8"))
    listed = {}
    for entry in catalog.get("mutation", []):
        listed[entry["case"]] = entry["item"]
        if entry["case"] not in mutations:
            err("mutations/CATALOG.toml", f"no case directory for {entry['case']!r}")
    for name, case in mutations.items():
        if name not in listed:
            err(f"mutations/{name}", "not listed in mutations/CATALOG.toml")
        elif case and case.get("catalog_item") != listed[name]:
            err(f"mutations/{name}", "catalog_item differs from mutations/CATALOG.toml")


def check_manifest():
    manifest = tomllib.loads((CORPUS / "MANIFEST.toml").read_text("utf-8"))
    paths = {f["path"] for f in manifest.get("fixture", [])}
    expected = {"catalog", "packs", "oracle"}
    expected |= {f"pairs/{d.name}" for d in (CORPUS / "pairs").iterdir() if d.is_dir()}
    expected |= {f"mutations/{d.name}" for d in (CORPUS / "mutations").iterdir() if d.is_dir()}
    for p in sorted(expected - paths):
        err("MANIFEST.toml", f"{p} has no entry")
    for p in sorted(paths - expected):
        err("MANIFEST.toml", f"{p} does not exist")


def check_oracle_specs(cases: dict):
    """corpus/oracle/*.toml name an existing case and sides it has (tools/fault_oracle.py)."""
    by_id = {d.name: c for d, c in cases.items() if c}
    for path in sorted((CORPUS / "oracle").glob("*.toml")):
        rel = path.relative_to(CORPUS)
        try:
            spec = tomllib.loads(path.read_text("utf-8"))
        except tomllib.TOMLDecodeError as e:
            err(rel, str(e))
            continue
        case = by_id.get(spec.get("case"))
        if case is None:
            err(rel, f"case {spec.get('case')!r} does not exist")
            continue
        if path.stem != spec["case"]:
            err(rel, "file name must be the case id")
        sides = case.get("sides", {})
        expects = {(e.get("a"), e.get("b")) for e in case.get("expect", [])}
        if not spec.get("pairs"):
            err(rel, "no pairs")
        for pair in spec.get("pairs", []):
            if len(pair) != 2:
                err(rel, f"pair {pair!r} must name two sides")
                continue
            for side in pair:
                if side not in sides:
                    err(rel, f"case has no side {side!r}")
                elif sides[side].get("dialect") not in {"tsql", "pg"}:
                    err(rel, f"side {side!r}: only tsql and pg run on the oracle")
            if tuple(pair) not in expects:
                err(rel, f"case has no [[expect]] for {pair[0]}~{pair[1]}")
        if not spec.get("scenario"):
            err(rel, "no [[scenario]]")
        for sc in spec.get("scenario", []):
            if "name" not in sc or "call" not in sc:
                err(rel, "every [[scenario]] needs name and call")


def engine_jobs(dialect: str, cases: dict):
    """(path, text, expected outcome) for the catalog and every side in a dialect."""
    jobs = [(Path(f"catalog/{dialect}.sql"), "", "ok")]
    for case_dir, case in cases.items():
        for side in (case or {}).get("sides", {}).values():
            if side.get("dialect") == dialect:
                path = case_dir / side["file"]
                jobs.append((path.relative_to(CORPUS), path.read_text("utf-8"),
                             side.get("create", "ok")))
    return jobs


def run_jobs(engine: str, cmd: list[str], jobs, wrap):
    for rel, sql, expected in jobs:
        proc = subprocess.run(cmd, input=wrap(sql), capture_output=True, text=True)
        outcome = "ok" if proc.returncode == 0 else "error"
        if outcome != expected:
            output = (proc.stderr + proc.stdout).strip().splitlines()
            detail = " | ".join(output[-3:])
            err(rel, f"{engine} create: expected {expected}, got {outcome}: {detail}")
    print(f"{engine}: created {len(jobs)} files")


def check_pg(dsn: str, cases: dict):
    catalog = (CORPUS / "catalog/pg.sql").read_text("utf-8")
    run_jobs("PostgreSQL", ["psql", dsn, "-X", "-q", "-v", "ON_ERROR_STOP=1"],
             engine_jobs("pg", cases),
             lambda sql: f"BEGIN;\n{catalog}\n{sql}\nROLLBACK;\n")


def check_mssql(command: str, cases: dict):
    # Each fixture gets a fresh database with a case-insensitive collation,
    # as catalog/tsql.sql assumes. The command must stop on the first error
    # (sqlcmd -b) and read the script from standard input.
    catalog = (CORPUS / "catalog/tsql.sql").read_text("utf-8")
    db = "corpus_check"
    setup = (f"USE master;\nGO\nIF DB_ID('{db}') IS NOT NULL DROP DATABASE {db};\nGO\n"
             f"CREATE DATABASE {db} COLLATE Latin1_General_CI_AS;\nGO\nUSE {db};\nGO\n")
    run_jobs("SQL Server", shlex.split(command), engine_jobs("tsql", cases),
             lambda sql: f"{setup}{catalog}\nGO\n{sql}\nGO\nUSE master;\nGO\n"
                         f"DROP DATABASE {db};\nGO\n")


def check_deny(deny_file: Path):
    terms = [t.strip() for t in deny_file.read_text("utf-8").splitlines()
             if t.strip() and not t.startswith("#")]
    for path in sorted(CORPUS.rglob("*")):
        if path.is_file() and path.suffix in {".sql", ".toml", ".md"}:
            text = path.read_text("utf-8").lower()
            for term in terms:
                if term.lower() in text:
                    err(path.relative_to(CORPUS), "contains a deny-listed term")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--pg", metavar="DSN", help="create PostgreSQL fixtures on this server")
    ap.add_argument("--mssql", metavar="CMD",
                    help="create T-SQL fixtures by piping scripts to this sqlcmd command")
    ap.add_argument("--deny", metavar="FILE", type=Path, help="deny-list, one term per line")
    args = ap.parse_args()

    for path in sorted(CORPUS.rglob("*")):
        if path.is_file() and path.suffix in {".sql", ".toml", ".md", ".py"}:
            check_text(path)

    cases = {}
    mutations = {}
    for d in sorted((CORPUS / "pairs").iterdir()):
        if d.is_dir():
            cases[d] = check_case(d, "pair")
    for d in sorted((CORPUS / "mutations").iterdir()):
        if d.is_dir():
            cases[d] = mutations[d.name] = check_case(d, "mutation")
    check_mutation_catalog(mutations)
    check_manifest()
    check_oracle_specs(cases)
    if args.pg:
        check_pg(args.pg, cases)
    if args.mssql:
        check_mssql(args.mssql, cases)
    if args.deny:
        check_deny(args.deny)

    n_pairs = len(cases) - len(mutations)
    print(f"{n_pairs} pairs, {len(mutations)} mutations")
    for e in errors:
        print(f"error: {e}", file=sys.stderr)
    sys.exit(1 if errors else 0)


if __name__ == "__main__":
    main()
