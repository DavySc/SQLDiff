#!/usr/bin/env python3
"""Generate ProfileCases.tla from profiles/error-txn.toml.

Usage:
  gen_cases.py            write ProfileCases.tla next to this script
  gen_cases.py --check    fail if ProfileCases.tla is out of date

Besides generating the module, this checks that every modelled cell of the
section 7 tables is covered by at least one case, that every case covers
only known cells of its own dialect, and that each program only uses
statements its dialect has.
"""

import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
PROFILE = HERE / "profiles" / "error-txn.toml"
OUT = HERE / "ProfileCases.tla"

DIALECTS = {"tsql", "postgres", "db2i"}
DIALECT_FAULTS = {
    "tsql": {"stmt", "batch", "uncatchable", "notfound"},
    "postgres": {"stmt", "uncatchable", "notfound"},
    "db2i": {"stmt", "notfound"},
}
# Statements each dialect has (block kinds included).
ALLOWED = {
    "tsql": {"write", "begin", "commit", "rollback", "savepoint", "rollback_to",
             "throw", "raiserror", "onerror", "onnotfound", "return", "try"},
    "postgres": {"write", "commit", "rollback", "throw", "notice", "onnotfound",
                 "return", "exc"},
    "db2i": {"write", "commit", "rollback", "savepoint", "rollback_to", "throw",
             "onerror", "onnotfound", "return", "compound"},
}
EXPECT_KEYS = {"durable", "open", "raised", "aborted", "code", "tc", "doomed"}
CASE_KEYS = {"id", "covers", "dialect", "xact_abort", "implicit_transactions",
             "commit", "caller_txn", "verified", "faults", "program", "expect"}


class Error(Exception):
    pass


def stmt(op, **kw):
    s = {"op": op, "id": "", "n": 0, "rows": 1, "notatomic": False,
         "rollback": False, "kind": "none", "handler": "none", "atomic": False,
         "body": [], "onerr": []}
    s.update(kw)
    return s


def parse_program(text, where):
    lines = []
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if line:
            lines.append(line)
    pos = 0

    def block(closers):
        nonlocal pos
        out = []
        while pos < len(lines):
            line = lines[pos]
            if line.startswith("}"):
                if line not in closers:
                    raise Error(f"{where}: unexpected '{line}'")
                return out, line
            pos += 1
            out.append(statement(line))
        if closers:
            raise Error(f"{where}: missing '}}'")
        return out, None

    def body_then(closers):
        nonlocal pos
        body, closer = block(closers)
        pos += 1
        return body, closer

    def statement(line):
        w = line.split()
        head, args = w[0], w[1:]
        if head == "write":
            if not args:
                raise Error(f"{where}: write needs a tag")
            s = stmt("write", id=args[0])
            for a in args[1:]:
                if a == "rows=2":
                    s["rows"] = 2
                elif a == "notatomic":
                    s["notatomic"] = True
                else:
                    raise Error(f"{where}: unknown write option '{a}'")
            return s
        if head in ("begin", "commit", "rollback", "notice", "raiserror") and not args:
            return stmt(head)
        if head in ("throw", "raise", "signal") and not args:
            return stmt("throw", id=head)
        if head == "savepoint" and len(args) == 1:
            return stmt("savepoint", id=args[0])
        if head == "rollback-to" and len(args) == 1:
            return stmt("rollback_to", id=args[0])
        if head == "return" and len(args) == 1:
            return stmt("return", n=int(args[0]))
        if head == "on-notfound" and args[:1] == ["return"] and len(args) == 2:
            return stmt("onnotfound", n=int(args[1]))
        if head == "on-error":
            rb = args[:1] == ["rollback"]
            rest = args[1:] if rb else args
            if rest[:1] != ["return"] or len(rest) != 2:
                raise Error(f"{where}: on-error [rollback] return N")
            return stmt("onerror", rollback=rb, n=int(rest[1]))
        if head == "try" and args == ["{"]:
            body, _ = body_then({"} catch {"})
            onerr, _ = body_then({"}"})
            return stmt("block", kind="try", handler="catch", body=body, onerr=onerr)
        if head == "begin-exception" and args == ["{"]:
            body, _ = body_then({"} when-others {"})
            onerr, _ = body_then({"}"})
            return stmt("block", kind="exc", handler="others", body=body, onerr=onerr)
        if head == "compound" and args[-1:] == ["{"]:
            opts = args[:-1]
            atomic = "atomic" in opts
            hs = [o for o in opts if o in ("continue", "exit", "undo")]
            if len(hs) > 1 or set(opts) - {"atomic", "continue", "exit", "undo"}:
                raise Error(f"{where}: bad compound options {opts}")
            handler = hs[0] if hs else "none"
            if handler == "undo" and not atomic:
                raise Error(f"{where}: UNDO handler needs ATOMIC")
            body, closer = body_then({"}", "} handler {"})
            onerr = []
            if closer == "} handler {":
                if handler == "none":
                    raise Error(f"{where}: handler body without a handler kind")
                onerr, _ = body_then({"}"})
            elif handler != "none":
                raise Error(f"{where}: {handler} handler needs a handler body")
            return stmt("block", kind="compound", handler=handler, atomic=atomic,
                        body=body, onerr=onerr)
        raise Error(f"{where}: cannot parse '{line}'")

    prog, _ = block(set())
    return prog


def walk(prog):
    for s in prog:
        yield s
        yield from walk(s["body"])
        yield from walk(s["onerr"])


def validate_program(prog, dialect, where, in_atomic=False):
    for s in prog:
        op = s["kind"] if s["op"] == "block" else s["op"]
        if op not in ALLOWED[dialect]:
            raise Error(f"{where}: '{op}' is not a {dialect} statement")
        if s["notatomic"] and dialect != "db2i":
            raise Error(f"{where}: notatomic is a Db2 for i MERGE option")
        if dialect == "db2i" and in_atomic and (
                s["op"] in ("commit", "rollback") or (s["op"] == "onerror" and s["rollback"])):
            raise Error(f"{where}: COMMIT/ROLLBACK not allowed inside ATOMIC")
        validate_program(s["body"], dialect, where, in_atomic or s["atomic"])
        validate_program(s["onerr"], dialect, where, in_atomic or s["atomic"])


def tla_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def tla_bool(b):
    return "TRUE" if b else "FALSE"


def tla_set(xs):
    return "{" + ", ".join(tla_str(x) for x in sorted(xs)) + "}"


def tla_stmt(s, indent):
    fields = [
        f"op |-> {tla_str(s['op'])}", f"id |-> {tla_str(s['id'])}",
        f"n |-> {s['n']}", f"rows |-> {s['rows']}",
        f"notatomic |-> {tla_bool(s['notatomic'])}",
        f"rollback |-> {tla_bool(s['rollback'])}",
        f"kind |-> {tla_str(s['kind'])}", f"handler |-> {tla_str(s['handler'])}",
        f"atomic |-> {tla_bool(s['atomic'])}",
        f"body |-> {tla_seq(s['body'], indent + 4)}",
        f"onerr |-> {tla_seq(s['onerr'], indent + 4)}",
    ]
    return "[" + ", ".join(fields) + "]"


def tla_seq(prog, indent):
    if not prog:
        return "<<>>"
    pad = " " * indent
    inner = (",\n" + pad).join(tla_stmt(s, indent) for s in prog)
    return "<<\n" + pad + inner + ">>"


def build():
    data = tomllib.loads(PROFILE.read_text())
    cells = {}
    for cell in data.get("cell", []):
        cid = cell["id"]
        if cid in cells:
            raise Error(f"duplicate cell {cid}")
        dialect = cid.rsplit(".", 1)[-1]
        if dialect not in DIALECTS:
            raise Error(f"cell {cid}: id must end in a dialect")
        status = cell.get("status")
        if status not in ("modelled", "not-modelled", "n/a"):
            raise Error(f"cell {cid}: bad status {status!r}")
        if status == "not-modelled" and not cell.get("reason"):
            raise Error(f"cell {cid}: not-modelled needs a reason")
        cells[cid] = cell

    covered = set()
    ids = []
    entries = []
    for case in data.get("case", []):
        cid = case["id"]
        where = f"case {cid}"
        unknown = set(case) - CASE_KEYS
        if unknown:
            raise Error(f"{where}: unknown keys {sorted(unknown)}")
        if cid in ids:
            raise Error(f"duplicate case {cid}")
        ids.append(cid)
        dialect = case["dialect"]
        if dialect not in DIALECTS:
            raise Error(f"{where}: unknown dialect {dialect}")
        if not case.get("covers"):
            raise Error(f"{where}: covers no cell")
        for cov in case["covers"]:
            if cov not in cells:
                raise Error(f"{where}: unknown cell {cov}")
            if not cov.endswith("." + dialect):
                raise Error(f"{where}: covers {cov} of another dialect")
            if cells[cov]["status"] != "modelled":
                raise Error(f"{where}: covers {cov}, which is {cells[cov]['status']}")
            covered.add(cov)

        commit = case.get("commit", "")
        if dialect == "db2i":
            if commit not in ("NONE", "CHG"):
                raise Error(f"{where}: db2i needs commit = NONE or CHG")
        elif commit:
            raise Error(f"{where}: commit is a Db2 for i setting")
        xact_abort = case.get("xact_abort", False)
        implicit = case.get("implicit_transactions", False)
        if (xact_abort or implicit) and dialect != "tsql":
            raise Error(f"{where}: xact_abort/implicit_transactions are T-SQL settings")
        caller_txn = case.get("caller_txn", False)
        if caller_txn and commit == "NONE":
            raise Error(f"{where}: no caller transaction under *NONE")

        prog = parse_program(case["program"], where)
        validate_program(prog, dialect, where)
        writes = [s for s in walk(prog) if s["op"] == "write"]
        tags = [s["id"] for s in writes]
        if len(set(tags)) != len(tags):
            raise Error(f"{where}: write tags must be unique")
        if "caller" in tags:
            raise Error(f"{where}: 'caller' is reserved")
        if any("." in t for t in tags):
            raise Error(f"{where}: write tags may not contain '.' (rows=2 adds .1/.2)")
        faults = case.get("faults", {})
        for tag, kind in faults.items():
            if tag not in tags:
                raise Error(f"{where}: fault on unknown write {tag}")
            if kind not in DIALECT_FAULTS[dialect]:
                raise Error(f"{where}: fault kind {kind} not in {dialect}")

        exp = case.get("expect", {})
        unknown = set(exp) - EXPECT_KEYS
        if unknown:
            raise Error(f"{where}: unknown expect keys {sorted(unknown)}")
        init_tc = 1 if dialect == "tsql" and caller_txn else 0
        expect = (
            f"[durable |-> {tla_set(exp.get('durable', []))}, "
            f"open |-> {tla_set(exp.get('open', []))}, "
            f"raised |-> {tla_bool(exp.get('raised', False))}, "
            f"aborted |-> {tla_bool(exp.get('aborted', False))}, "
            f"code |-> {exp.get('code', 0)}, "
            f"tc |-> {exp.get('tc', init_tc)}, "
            f"doomed |-> {tla_bool(exp.get('doomed', False))}]"
        )
        if faults:
            fexpr = " @@ ".join(f"({tla_str(t)} :> {tla_str(k)})" for t, k in sorted(faults.items()))
        else:
            fexpr = "[w \\in {} |-> \"ok\"]"
        multi = [s["id"] for s in writes if s["rows"] > 1]
        notatomic = [s["id"] for s in writes if s["notatomic"]]
        entries.append(
            f"    \\* {cid}{'' if case.get('verified', True) else '  (rests on an unverified fact)'}\n"
            f"    ({tla_str(cid)} :> [\n"
            f"        dialect |-> {tla_str(dialect)},\n"
            f"        xact_abort |-> {tla_bool(xact_abort)},\n"
            f"        implicit_transactions |-> {tla_bool(implicit)},\n"
            f"        commit |-> {tla_str(commit)},\n"
            f"        caller_txn |-> {tla_bool(caller_txn)},\n"
            f"        verified |-> {tla_bool(case.get('verified', True))},\n"
            f"        faults |-> {fexpr},\n"
            f"        writes |-> {tla_set(tags)},\n"
            f"        multi |-> {tla_set(multi)},\n"
            f"        notatomic |-> {tla_set(notatomic)},\n"
            f"        expect |-> {expect},\n"
            f"        program |-> {tla_seq(prog, 12)}])"
        )

    missing = sorted(c for c, cell in cells.items()
                     if cell["status"] == "modelled" and c not in covered)
    if missing:
        raise Error("modelled cells without a case: " + ", ".join(missing))

    out = [
        "---------------------------- MODULE ProfileCases ----------------------------",
        "\\* GENERATED by gen_cases.py from profiles/error-txn.toml. Do not edit.",
        "EXTENDS TLC",
        "",
        "FaultKinds(d) ==",
        "    CASE " + "\n      [] ".join(
            f"d = {tla_str(d)} -> {tla_set(k)}" for d, k in sorted(DIALECT_FAULTS.items())),
        "",
        "CaseIds == " + tla_set(ids),
        "",
        "Cases ==",
        "\n    @@\n".join(entries),
        "",
        "=============================================================================",
        "",
    ]
    stats = {
        "cells": len(cells),
        "modelled": sum(1 for c in cells.values() if c["status"] == "modelled"),
        "not-modelled": sum(1 for c in cells.values() if c["status"] == "not-modelled"),
        "cases": len(ids),
    }
    return "\n".join(out), stats


def main():
    try:
        text, stats = build()
    except (Error, KeyError, ValueError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    if "--check" in sys.argv[1:]:
        if not OUT.exists() or OUT.read_text() != text:
            print("ProfileCases.tla is out of date; run formal/gen_cases.py", file=sys.stderr)
            return 1
    else:
        OUT.write_text(text)
    print(f"{stats['cases']} cases; {stats['modelled']} of {stats['cells']} cells modelled, "
          f"{stats['not-modelled']} not modelled")
    return 0


if __name__ == "__main__":
    sys.exit(main())
