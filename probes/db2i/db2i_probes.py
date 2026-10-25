#!/usr/bin/env python3
"""Db2 for i probes: build the probe script, read submitted results, keep the ledger.

    python3 probes/db2i/db2i_probes.py build
        Regenerate sqldiff-db2i-probes.sql from probes.toml.
    python3 probes/db2i/db2i_probes.py check
        Fail if probes.toml or facts.toml are malformed or the script is stale.
    python3 probes/db2i/db2i_probes.py ingest RESULTS --interface acs [--run NAME]
        Read the lines the script printed (pasted text or a CSV export), judge every
        probe against its outcomes, write runs/NAME.json and runs/NAME.md, and
        recompute status.json from every run in runs/.

Only Db2 for i (IBM i 7.x). Standard library only (Python 3.11+ for tomllib).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
PROBES = HERE / "probes.toml"
FACTS = HERE / "facts.toml"
SCRIPT = HERE / "sqldiff-db2i-probes.sql"
STATUS = HERE / "status.json"
RUNS = HERE / "runs"

SCHEMA = "SQLDFPROBE"
LINE_TAG = "SQLDFP|1|"
WIDTH = 80
OUT_PARAMS = (
    "OUT O_STATE CHAR(5), OUT O_CODE INTEGER, OUT O_N1 INTEGER, "
    "OUT O_N2 INTEGER, OUT O_TEXT VARCHAR(200)"
)
ROLES = {"decisive", "control", "info"}
VERDICTS = {"consistent", "refines", "contradicts", "open"}
FIELDS = {"create", "call", "state", "code", "n1", "n2", "text"}
STATUSES = {
    "unverified", "confirmed", "corrected", "contradicted", "observed",
    "inconclusive", "mixed",
}
# A fact in one of these states has an answer from a real system: the hedges that
# say "not checked" must go (crates/sqldiff-corpus/tests/db2i_probes.rs enforces it).
RESOLVED = {"confirmed", "corrected", "contradicted"}


# ---------------------------------------------------------------- definitions


def load_probes() -> dict:
    with PROBES.open("rb") as f:
        return tomllib.load(f)


def load_facts() -> dict:
    with FACTS.open("rb") as f:
        return tomllib.load(f)


def probes_hash() -> str:
    return hashlib.sha256(PROBES.read_bytes()).hexdigest()[:12]


def validate(defs: dict, facts: dict) -> list[str]:
    """Problems in probes.toml and facts.toml, empty when both are sound."""
    errs = []
    procs = {}
    for p in defs.get("procedure", []):
        name = p.get("name", "")
        if not re.fullmatch(r"[A-Z][A-Z0-9_]{0,60}", name):
            errs.append(f"procedure name {name!r}: upper case letters, digits, _")
        if name in procs:
            errs.append(f"procedure {name} defined twice")
        procs[name] = p
        if p.get("commit", "") not in {"", "*NONE", "*CHG", "*CS", "*ALL", "*RR"}:
            errs.append(f"procedure {name}: commit {p.get('commit')!r}")
        if p.get("naming", "") not in {"", "*SYS"}:
            errs.append(f"procedure {name}: naming {p.get('naming')!r} (only \"*SYS\" may be given)")
        if p.get("naming") and p.get("top_level"):
            errs.append(f"procedure {name}: a top-level create always uses the interface's naming")
        if "COMMIT" in p.get("options", "").upper() or "NAMING" in p.get("options", "").upper():
            errs.append(f"procedure {name}: give COMMIT and naming through commit and naming, not options")
        if f"'P_{name}'" in p.get("body", "") and len("P_" + name) > 10:
            errs.append(f"procedure {name}: its body looks itself up by name, so P_{name} must fit a 10-character system name")
        body = p.get("body", "")
        if "--" in body:
            errs.append(f"procedure {name}: '--' in body (the body is sent as one line)")
        for line in body.splitlines():
            if len(line) > WIDTH - 5:
                errs.append(f"procedure {name}: body line longer than {WIDTH - 5}: {line.strip()[:40]}")
        if not body.strip().upper().startswith("BEGIN") or not body.strip().upper().endswith("END"):
            errs.append(f"procedure {name}: body must be BEGIN ... END")
    used = set()
    ids = set()
    fact_ids = {f["id"] for f in facts.get("fact", [])}
    for pr in defs.get("probe", []):
        pid = pr.get("id", "")
        if not re.fullmatch(r"[A-Z0-9][A-Z0-9-]{0,38}", pid):
            errs.append(f"probe id {pid!r}: upper case, digits, -; at most 39 characters")
        if pid in ids:
            errs.append(f"probe {pid} defined twice")
        ids.add(pid)
        if pr.get("role") not in ROLES:
            errs.append(f"probe {pid}: role {pr.get('role')!r}")
        fact = pr.get("fact")
        if fact not in fact_ids | {"HARNESS", "ENV"}:
            errs.append(f"probe {pid}: fact {fact!r} is not in facts.toml")
        has_proc = "procedure" in pr
        if has_proc == ("inline" in pr):
            errs.append(f"probe {pid}: needs exactly one of procedure and inline")
        if has_proc:
            if pr["procedure"] not in procs:
                errs.append(f"probe {pid}: unknown procedure {pr['procedure']}")
            used.add(pr["procedure"])
            nparams = len([x for x in procs.get(pr["procedure"], {}).get("params", "").split(",") if x.strip()])
            if len(pr.get("args", [])) != nparams:
                errs.append(f"probe {pid}: {len(pr.get('args', []))} args for {nparams} params")
        elif "--" in pr["inline"]:
            errs.append(f"probe {pid}: '--' in inline body")
        for key in ("question", "source"):
            if not pr.get(key):
                errs.append(f"probe {pid}: missing {key}")
        outs = pr.get("outcome", [])
        if not outs:
            errs.append(f"probe {pid}: no outcomes")
        for o in outs:
            if o.get("verdict") not in VERDICTS:
                errs.append(f"probe {pid}: outcome {o.get('label')!r} verdict {o.get('verdict')!r}")
            for k, pat in o.get("when", {}).items():
                if k not in FIELDS:
                    errs.append(f"probe {pid}: outcome {o.get('label')!r} field {k!r}")
                if not isinstance(pat, str):
                    errs.append(f"probe {pid}: outcome {o.get('label')!r} pattern for {k} must be a string")
                elif pat.startswith("regex:"):
                    try:
                        re.compile(pat[len("regex:"):])
                    except re.error as e:
                        errs.append(f"probe {pid}: outcome {o.get('label')!r} bad regex: {e}")
            if not o.get("meaning"):
                errs.append(f"probe {pid}: outcome {o.get('label')!r} has no meaning")
        if pr.get("role") == "control" and not any(o["verdict"] == "consistent" for o in outs):
            errs.append(f"probe {pid}: a control needs a consistent outcome")
    for name in procs:
        if name not in used:
            errs.append(f"procedure {name} is not used by any probe")
    for f in facts.get("fact", []):
        probes_of = [p for p in defs.get("probe", []) if p.get("fact") == f["id"]]
        if not probes_of:
            errs.append(f"fact {f['id']} has no probe")
    for link in facts.get("link", []):
        for fid in link.get("facts", []):
            if fid not in fact_ids:
                errs.append(f"link {link.get('anchor')!r}: unknown fact {fid}")
        for key in ("file", "anchor", "hedge"):
            if not link.get(key):
                errs.append(f"link {link.get('anchor')!r}: missing {key}")
    return errs


# ---------------------------------------------------------------- script


def sql_literal_pieces(text: str, width: int) -> list[str]:
    """`text` as SQL string literals of at most `width` characters each.

    Pieces end after a space where one is near, so the script stays readable.
    The text is split before escaping, so a doubled quote is never cut in two."""

    def size(chunk: str) -> int:
        return len(chunk) + chunk.count("'") + 2

    pieces, cur = [], ""
    for ch in text:
        if size(cur + ch) > width:
            cut = cur.rfind(" ")
            if cut < len(cur) // 2:
                cut = len(cur) - 1
            pieces.append(cur[: cut + 1])
            cur = cur[cut + 1 :]
        cur += ch
    pieces.append(cur)
    return ["'" + p.replace("'", "''") + "'" for p in pieces]


def one_line(body: str) -> str:
    return " ".join(line.strip() for line in body.strip().splitlines() if line.strip())


def qualifier(p: dict) -> str:
    """`.` under SQL naming, `/` for a procedure created under system naming."""
    return "/" if p.get("naming") == "*SYS" else "."


def set_option(p: dict) -> str:
    items = [f"COMMIT = {p['commit']}"] if p.get("commit") else []
    if p.get("options"):
        items.append(p["options"])
    return "SET OPTION " + ", ".join(items) if items else ""


def create_text(p: dict) -> str:
    params = p.get("params", "").strip()
    plist = f"{params}, {OUT_PARAMS}" if params else OUT_PARAMS
    opts = "LANGUAGE SQL MODIFIES SQL DATA"
    if set_option(p):
        opts += " " + set_option(p)
    return f"CREATE OR REPLACE PROCEDURE {SCHEMA}{qualifier(p)}P_{p['name']} ({plist}) {opts} {one_line(p['body'])}"


def wrap_plain(text: str, indent: str = "", width: int = WIDTH) -> list[str]:
    """Readable multi-line SQL: wrap at spaces within `width`."""
    out, cur = [], indent
    for word in text.split(" "):
        if len(cur) + len(word) + 1 > width and cur.strip():
            out.append(cur.rstrip())
            cur = indent + "    "
        cur += word + " "
    if cur.strip():
        out.append(cur.rstrip())
    return out


def comment_block(text: str) -> list[str]:
    lines = []
    for raw in text.strip().splitlines():
        for line in wrap_plain(raw.rstrip(), "", WIDTH - 3) if len(raw) + 3 > WIDTH else [raw.rstrip()]:
            lines.append(("-- " + line).rstrip())
    return lines


def commit_block() -> list[str]:
    return [
        "BEGIN",
        "  DECLARE V_IGNORED INTEGER DEFAULT 0;",
        "  DECLARE CONTINUE HANDLER FOR SQLEXCEPTION SET V_IGNORED = 1;",
        "  COMMIT;",
        "END;",
        "",
    ]


def call_block(pr: dict, procs: dict) -> list[str]:
    lines = [
        "BEGIN",
        "  DECLARE V_STATE CHAR(5) DEFAULT '00000';",
        "  DECLARE V_CODE INTEGER DEFAULT 0;",
        "  DECLARE V_MSG VARCHAR(2000) DEFAULT '';",
        "  DECLARE O_STATE CHAR(5);",
        "  DECLARE O_CODE INTEGER;",
        "  DECLARE O_N1 INTEGER;",
        "  DECLARE O_N2 INTEGER;",
        "  DECLARE O_TEXT VARCHAR(200);",
        "  DECLARE CONTINUE HANDLER FOR SQLEXCEPTION",
        "    GET DIAGNOSTICS CONDITION 1 V_STATE = RETURNED_SQLSTATE,",
        "      V_CODE = DB2_RETURNED_SQLCODE, V_MSG = MESSAGE_TEXT;",
    ]
    if "procedure" in pr:
        args, sets = [], []
        for i, arg in enumerate(pr.get("args", []), 1):
            if len(arg) > 40 and arg.startswith("'") and arg.endswith("'"):
                # A long string goes through a variable, so no literal is wrapped.
                lines.insert(9 + i - 1, f"  DECLARE V_ARG{i} VARCHAR(4000);")
                sets.append(f"  SET V_ARG{i} =")
                for j, piece in enumerate(sql_literal_pieces(arg[1:-1].replace("''", "'"), WIDTH - 8)):
                    sets.append(("    || " if j else "       ") + piece)
                sets[-1] += ";"
                args.append(f"V_ARG{i}")
            else:
                args.append(arg)
        lines += sets
        args = ", ".join(args + ["O_STATE", "O_CODE", "O_N1", "O_N2", "O_TEXT"])
        lines += wrap_plain(f"CALL {SCHEMA}.P_{pr['procedure']}({args});", "  ")
    else:
        for raw in pr["inline"].strip().splitlines():
            lines.append("  " + raw.rstrip())
    lines += [
        f"  CALL {SCHEMA}.LOG_CALL('{pr['id']}', V_STATE, V_CODE, V_MSG,",
        "    O_STATE, O_CODE, O_N1, O_N2, O_TEXT);",
        "END;",
        "",
    ]
    return lines


def create_block(probe: str, drop: str, sql: str, helper: str) -> list[str]:
    """Create through `helper`; if the CALL itself fails, log that as the create."""
    out = [
        "BEGIN",
        "  DECLARE V_SQL VARCHAR(16000);",
        "  DECLARE V_STATE CHAR(5) DEFAULT '00000';",
        "  DECLARE V_CODE INTEGER DEFAULT 0;",
        "  DECLARE V_MSG VARCHAR(2000) DEFAULT '';",
        "  DECLARE CONTINUE HANDLER FOR SQLEXCEPTION",
        "    GET DIAGNOSTICS CONDITION 1 V_STATE = RETURNED_SQLSTATE,",
        "      V_CODE = DB2_RETURNED_SQLCODE, V_MSG = MESSAGE_TEXT;",
        "  SET V_SQL =",
    ]
    for i, piece in enumerate(sql_literal_pieces(sql, WIDTH - 8)):
        out.append(("    || " if i else "       ") + piece)
    out[-1] += ";"
    out += [
        f"  CALL {SCHEMA}.{helper}('{probe}',",
        f"    '{drop}', V_SQL);",
        "  IF V_STATE <> '00000' THEN",
        f"    INSERT INTO {SCHEMA}.RESULTS (R_PROBE, R_STEP, R_STATE, R_CODE, R_MSG)",
        f"      VALUES ('{probe}', 'create', V_STATE, V_CODE,",
        f"        'CALL {helper} failed: ' || V_MSG) WITH NC;",
        "  END IF;",
        "END;",
        "",
    ]
    return out


# A RUN_DDL that runs its statements under system naming, for probes of what a
# routine created under system naming gets. Created through RUN_DDL, so a refusal
# is logged as HELPER:RUN_DDL_SYS.
RUN_DDL_SYS = (
    f"CREATE OR REPLACE PROCEDURE {SCHEMA}.RUN_DDL_SYS (IN P_PROBE VARCHAR(40), "
    "IN P_DROP VARCHAR(200), IN P_SQL VARCHAR(16000)) LANGUAGE SQL MODIFIES SQL DATA "
    "SET OPTION COMMIT = *NONE, NAMING = *SYS "
    "BEGIN DECLARE V_STATE CHAR(5) DEFAULT '00000'; DECLARE V_CODE INTEGER DEFAULT 0; "
    "DECLARE V_MSG VARCHAR(2000) DEFAULT ''; "
    "BEGIN DECLARE CONTINUE HANDLER FOR SQLEXCEPTION SET V_CODE = 0; EXECUTE IMMEDIATE P_DROP; END; "
    "BEGIN DECLARE EXIT HANDLER FOR SQLEXCEPTION GET DIAGNOSTICS CONDITION 1 "
    "V_STATE = RETURNED_SQLSTATE, V_CODE = DB2_RETURNED_SQLCODE, V_MSG = MESSAGE_TEXT; "
    "EXECUTE IMMEDIATE P_SQL; END; "
    f"INSERT INTO {SCHEMA}/RESULTS (R_PROBE, R_STEP, R_STATE, R_CODE, R_MSG) "
    "VALUES (P_PROBE, 'create', V_STATE, V_CODE, V_MSG) WITH NC; END"
)


HEADER = """\
SQLDiff probes for Db2 for i (IBM i 7.x only).

GENERATED from probes/db2i/probes.toml by probes/db2i/db2i_probes.py build.
Do not edit by hand. Probe set {hash}.

What it does: creates the schema SQLDFPROBE (CREATE SCHEMA, so its tables are
journaled), two tables and about 35 small procedures in it, runs them, and
prints one line per result. It touches nothing outside SQLDFPROBE, apart from
reading SYSIBMADM.ENV_SYS_INFO, QSYS2.GROUP_PTF_INFO, QSYS2.SYSROUTINES and
QSYS2.RECORD_LOCK_INFO, and declaring temporary tables in QTEMP.

How to run (details in probes/db2i/README.md):
  ACS Run SQL Scripts, SQL naming: open this file, Run > All. The last result
  set has one column, SQLDIFF_PROBE_LINE; save or copy all its rows.
  Run it again at any time: earlier results are deleted first.
  Remove everything afterwards with: DROP SCHEMA SQLDFPROBE

The result lines hold SQLSTATEs, counts and Db2 messages about SQLDFPROBE
objects, the IBM i release and PTF group levels: no system name, user or
data.
"""


def build_script(defs: dict) -> str:
    procs = {p["name"]: p for p in defs["procedure"]}
    probes = defs["probe"]
    h = probes_hash()
    out: list[str] = comment_block(HEADER.format(hash=h))
    out.append("")

    out += comment_block("1. Setup: schema, tables, logging procedures.")
    out += [
        "BEGIN",
        "  DECLARE V_IGNORED INTEGER DEFAULT 0;",
        "  DECLARE V_SQL VARCHAR(1000);",
        "  DECLARE CONTINUE HANDLER FOR SQLEXCEPTION SET V_IGNORED = 1;",
        f"  SET V_SQL = 'CREATE SCHEMA {SCHEMA}';",
        "  EXECUTE IMMEDIATE V_SQL;",
        "END;",
        "",
    ]
    out += commit_block()
    out += [
        "BEGIN",
        "  DECLARE V_IGNORED INTEGER DEFAULT 0;",
        "  DECLARE V_SQL VARCHAR(1000);",
        "  DECLARE CONTINUE HANDLER FOR SQLEXCEPTION SET V_IGNORED = 1;",
        f"  SET V_SQL = 'CREATE TABLE {SCHEMA}.RESULTS ('",
        "    || 'R_SEQ INTEGER GENERATED ALWAYS AS IDENTITY, '",
        "    || 'R_PROBE VARCHAR(40) NOT NULL, R_STEP VARCHAR(10) NOT NULL, '",
        "    || 'R_STATE CHAR(5), R_CODE INTEGER, R_MSG VARCHAR(2000), '",
        "    || 'R_O_STATE CHAR(5), R_O_CODE INTEGER, R_N1 INTEGER, '",
        "    || 'R_N2 INTEGER, R_TEXT VARCHAR(200))';",
        "  EXECUTE IMMEDIATE V_SQL;",
        f"  SET V_SQL = 'CREATE TABLE {SCHEMA}.T (PRB INTEGER NOT NULL, '",
        "    || 'K INTEGER NOT NULL, V INTEGER, PRIMARY KEY (PRB, K))';",
        "  EXECUTE IMMEDIATE V_SQL;",
        f"  SET V_SQL = 'DELETE FROM {SCHEMA}.RESULTS WITH NC';",
        "  EXECUTE IMMEDIATE V_SQL;",
        f"  SET V_SQL = 'DELETE FROM {SCHEMA}.T WITH NC';",
        "  EXECUTE IMMEDIATE V_SQL;",
        "END;",
        "",
    ]
    out += commit_block()
    out += [
        f"CREATE OR REPLACE PROCEDURE {SCHEMA}.LOG_CALL (",
        "    IN P_PROBE VARCHAR(40), IN P_STATE CHAR(5), IN P_CODE INTEGER,",
        "    IN P_MSG VARCHAR(2000), IN P_O_STATE CHAR(5), IN P_O_CODE INTEGER,",
        "    IN P_N1 INTEGER, IN P_N2 INTEGER, IN P_TEXT VARCHAR(200))",
        "  LANGUAGE SQL",
        "  MODIFIES SQL DATA",
        "  SET OPTION COMMIT = *NONE",
        "BEGIN",
        f"  INSERT INTO {SCHEMA}.RESULTS (R_PROBE, R_STEP, R_STATE, R_CODE, R_MSG,",
        "      R_O_STATE, R_O_CODE, R_N1, R_N2, R_TEXT)",
        "    VALUES (P_PROBE, 'call', P_STATE, P_CODE, P_MSG,",
        "      P_O_STATE, P_O_CODE, P_N1, P_N2, P_TEXT) WITH NC;",
        "END;",
        "",
        f"CREATE OR REPLACE PROCEDURE {SCHEMA}.RUN_DDL (",
        "    IN P_PROBE VARCHAR(40), IN P_DROP VARCHAR(200),",
        "    IN P_SQL VARCHAR(16000))",
        "  LANGUAGE SQL",
        "  MODIFIES SQL DATA",
        "  SET OPTION COMMIT = *NONE",
        "BEGIN",
        "  DECLARE V_STATE CHAR(5) DEFAULT '00000';",
        "  DECLARE V_CODE INTEGER DEFAULT 0;",
        "  DECLARE V_MSG VARCHAR(2000) DEFAULT '';",
        "  BEGIN",
        "    DECLARE CONTINUE HANDLER FOR SQLEXCEPTION SET V_CODE = 0;",
        "    EXECUTE IMMEDIATE P_DROP;",
        "  END;",
        "  BEGIN",
        "    DECLARE EXIT HANDLER FOR SQLEXCEPTION",
        "      GET DIAGNOSTICS CONDITION 1 V_STATE = RETURNED_SQLSTATE,",
        "        V_CODE = DB2_RETURNED_SQLCODE, V_MSG = MESSAGE_TEXT;",
        "    EXECUTE IMMEDIATE P_SQL;",
        "  END;",
        f"  INSERT INTO {SCHEMA}.RESULTS (R_PROBE, R_STEP, R_STATE, R_CODE, R_MSG)",
        "    VALUES (P_PROBE, 'create', V_STATE, V_CODE, V_MSG) WITH NC;",
        "END;",
        "",
    ]
    out += commit_block()
    out += [
        "BEGIN",
        "  DECLARE V_NO_STATE CHAR(5);",
        "  DECLARE V_NO_N INTEGER;",
        f"  CALL {SCHEMA}.LOG_CALL('META-FORMAT', '00000', 1, 'probe set',",
        f"    V_NO_STATE, V_NO_N, V_NO_N, V_NO_N, '{h}');",
        "END;",
        "",
    ]

    out += comment_block(
        "2. Create the probe procedures. Each create is logged with its SQLSTATE,\n"
        "so a procedure Db2 for i refuses is a result, not a stop."
    )
    out.append("")
    if any(p.get("naming") == "*SYS" for p in procs.values()):
        out += comment_block("RUN_DDL_SYS: RUN_DDL with SET OPTION NAMING = *SYS.")
        out += create_block("HELPER:RUN_DDL_SYS", f"DROP PROCEDURE {SCHEMA}.RUN_DDL_SYS", RUN_DDL_SYS, "RUN_DDL")
    for name, p in procs.items():
        users = ", ".join(pr["id"] for pr in probes if pr.get("procedure") == name)
        out += comment_block(f"P_{name} (probes {users})")
        out += comment_block(f"  {set_option(p)}" if set_option(p) else "  no SET OPTION: options of the creating interface")
        if p.get("naming"):
            out += comment_block(f"  created under {p['naming']} naming (through RUN_DDL_SYS)")
        for raw in p["body"].strip().splitlines():
            out.append(("-- | " + raw.rstrip()).rstrip())
        drop = f"DROP PROCEDURE {SCHEMA}{qualifier(p)}P_{name}"
        if p.get("top_level"):
            # Created by the interface itself, so the routine takes its options.
            out += [
                "BEGIN",
                "  DECLARE V_IGNORED INTEGER DEFAULT 0;",
                "  DECLARE V_SQL VARCHAR(200);",
                "  DECLARE CONTINUE HANDLER FOR SQLEXCEPTION SET V_IGNORED = 1;",
                f"  SET V_SQL = '{drop}';",
                "  EXECUTE IMMEDIATE V_SQL;",
                "END;",
                "",
            ]
            out += commit_block()
            out += wrap_plain(create_text(p) + ";")
            out.append("")
            out += commit_block()
            out += [
                "BEGIN",
                "  DECLARE V_N INTEGER DEFAULT 0;",
                "  DECLARE V_IGNORED INTEGER DEFAULT 0;",
                "  DECLARE CONTINUE HANDLER FOR SQLEXCEPTION SET V_IGNORED = 1;",
                "  SELECT COUNT(*) INTO V_N FROM QSYS2.SYSROUTINES",
                f"    WHERE ROUTINE_SCHEMA = '{SCHEMA}' AND ROUTINE_NAME = 'P_{name}';",
                f"  INSERT INTO {SCHEMA}.RESULTS (R_PROBE, R_STEP, R_STATE, R_MSG)",
                f"    VALUES ('PROC:{name}', 'create',",
                "      CASE WHEN V_N > 0 THEN '00000' ELSE 'NOTFD' END,",
                "      'created by the interface; checked in QSYS2.SYSROUTINES')",
                "    WITH NC;",
                "END;",
                "",
            ]
            continue
        out += create_block(f"PROC:{name}", drop, create_text(p), "RUN_DDL_SYS" if p.get("naming") else "RUN_DDL")
    out += commit_block()

    out += comment_block("3. Run the probes.")
    out.append("")
    for pr in probes:
        out += comment_block(f"{pr['id']} ({pr['fact']}, {pr['role']}): {pr['question']}")
        out += call_block(pr, procs)

    out += comment_block("4. The results: save or copy every row of this result set.")
    out += [
        f"SELECT '{LINE_TAG}' || TRIM(CHAR(R_SEQ)) || '|' || R_PROBE",
        "    || '|' || R_STEP",
        "    || '|' || COALESCE(R_STATE, '~')",
        "    || '|' || COALESCE(TRIM(CHAR(R_CODE)), '~')",
        "    || '|' || COALESCE(R_O_STATE, '~')",
        "    || '|' || COALESCE(TRIM(CHAR(R_O_CODE)), '~')",
        "    || '|' || COALESCE(TRIM(CHAR(R_N1)), '~')",
        "    || '|' || COALESCE(TRIM(CHAR(R_N2)), '~')",
        "    || '|' || COALESCE(REPLACE(R_TEXT, '|', '/'), '~')",
        "    || '|' || COALESCE(REPLACE(R_MSG, '|', '/'), '~')",
        "    AS SQLDIFF_PROBE_LINE",
        f"  FROM {SCHEMA}.RESULTS ORDER BY R_SEQ;",
    ]
    return "\n".join(out) + "\n"


# ---------------------------------------------------------------- results


def parse_results(text: str) -> list[dict]:
    """The result rows in pasted text or a CSV export, in order."""
    rows = []
    for raw in text.splitlines():
        at = raw.find(LINE_TAG)
        if at < 0:
            continue
        line = raw[at:].rstrip().rstrip(",;").rstrip()
        if at > 0 and raw[at - 1] == '"':
            # A CSV cell: drop the closing quote and undouble the inner ones.
            if line.endswith('"'):
                line = line[:-1]
            line = line.replace('""', '"')
        parts = line.split("|", 11)
        if len(parts) < 12:
            raise ValueError(f"result line with {len(parts)} fields: {raw!r}")
        _, _, seq, probe, step, state, code, ostate, ocode, n1, n2, rest = parts
        text_field, _, msg = rest.partition("|")

        def opt(v):
            return None if v == "~" else v

        def num(v):
            v = opt(v)
            return None if v is None else int(v)

        rows.append({
            "seq": int(seq), "probe": probe, "step": step,
            "state": opt(state), "code": num(code), "o_state": opt(ostate),
            "o_code": num(ocode), "n1": num(n1), "n2": num(n2),
            "text": opt(text_field), "msg": opt(msg),
        })
    return rows


def matches(pattern: str, value) -> bool:
    if pattern == "*":
        return True
    if pattern == "null":
        return value is None
    if value is None:
        return False
    if pattern.startswith("contains:"):
        return pattern[len("contains:"):] in str(value)
    if pattern.startswith("prefix:"):
        return str(value).startswith(pattern[len("prefix:"):])
    if pattern.startswith("regex:"):
        return re.fullmatch(pattern[len("regex:"):], str(value)) is not None
    if pattern.startswith("!"):
        return str(value) != pattern[1:]
    m = re.fullmatch(r"(>=|<=|=|>|<)(-?\d+)", pattern)
    if m:
        op, n = m.group(1), int(m.group(2))
        v = int(value)
        return {"=": v == n, ">": v > n, "<": v < n, ">=": v >= n, "<=": v <= n}[op]
    return str(value) == pattern


def observe(pr: dict, rows: list[dict]) -> dict | None:
    call = next((r for r in rows if r["probe"] == pr["id"] and r["step"] == "call"), None)
    if call is None:
        return None
    obs = {
        "create": None, "call": call["state"], "state": call["o_state"],
        "code": call["o_code"], "n1": call["n1"], "n2": call["n2"],
        "text": call["text"], "call_message": call["msg"],
    }
    if "procedure" in pr:
        c = next((r for r in rows if r["probe"] == f"PROC:{pr['procedure']}" and r["step"] == "create"), None)
        if c is None:
            return None
        obs["create"] = c["state"]
        obs["create_message"] = c["msg"]
    return obs


def judge(pr: dict, rows: list[dict]) -> dict:
    obs = observe(pr, rows)
    if obs is None:
        return {"id": pr["id"], "fact": pr["fact"], "role": pr["role"], "verdict": "missing",
                "outcome": None, "meaning": "No result line for this probe (or its create).", "observed": None}
    for o in pr["outcome"]:
        if all(matches(pat, obs.get(k)) for k, pat in o["when"].items()):
            return {"id": pr["id"], "fact": pr["fact"], "role": pr["role"], "verdict": o["verdict"],
                    "outcome": o["label"], "meaning": o["meaning"], "observed": obs}
    return {"id": pr["id"], "fact": pr["fact"], "role": pr["role"], "verdict": "unmatched",
            "outcome": None, "meaning": "The result matches none of the outcomes written down: read it by hand.",
            "observed": obs}


def fact_status(judged: list[dict], fact: str, harness_ok: bool) -> tuple[str, str]:
    mine = [j for j in judged if j["fact"] == fact]
    if not harness_ok:
        return "inconclusive", "the harness control H-COMMIT-CONTROL failed"
    bad_controls = [j["id"] for j in mine if j["role"] == "control" and j["verdict"] != "consistent"]
    if bad_controls:
        return "inconclusive", "control(s) not as expected: " + ", ".join(bad_controls)
    decisive = [j for j in mine if j["role"] == "decisive"]
    rest = decisive or [j for j in mine if j["role"] == "info"]
    unclear = [j["id"] for j in rest if j["verdict"] in {"missing", "unmatched"}]
    if unclear:
        return "inconclusive", "no recognised outcome: " + ", ".join(unclear)
    verdicts = {j["verdict"] for j in decisive}
    if "contradicts" in verdicts:
        return "contradicted", ", ".join(j["id"] for j in decisive if j["verdict"] == "contradicts")
    if "refines" in verdicts:
        return "corrected", ", ".join(j["id"] for j in decisive if j["verdict"] == "refines")
    if "consistent" in verdicts:
        return "confirmed", "all decisive probes as expected"
    return "observed", "open questions answered; nothing was expected"


def evaluate(defs: dict, facts: dict, rows: list[dict]) -> dict:
    judged = [judge(pr, rows) for pr in defs["probe"]]
    harness = next(j for j in judged if j["id"] == "H-COMMIT-CONTROL")
    harness_ok = harness["verdict"] == "consistent"
    status = {}
    for f in facts["fact"]:
        st, why = fact_status(judged, f["id"], harness_ok)
        status[f["id"]] = {"status": st, "why": why}
    meta = next((r for r in rows if r["probe"] == "META-FORMAT"), None)
    env = {j["id"]: j["observed"] for j in judged if j["fact"] == "ENV"}
    os_obs = env.get("ENV-OS") or {}
    grp = env.get("ENV-DB2-GROUP") or {}
    return {
        "probe_set": meta["text"] if meta else None,
        "current_probe_set": probes_hash(),
        "os_release": os_obs.get("text"),
        "db2_group_level": grp.get("n1"),
        "tr_group_level": grp.get("n2"),
        "harness_ok": harness_ok,
        "facts": status,
        "probes": judged,
    }


def recompute_status(facts: dict) -> dict:
    runs = []
    for path in sorted(RUNS.glob("*.json")):
        runs.append(json.loads(path.read_text()))
    out = {"format": 1, "runs": [], "facts": {}}
    for r in runs:
        out["runs"].append({k: r.get(k) for k in ("run", "interface", "os_release", "db2_group_level", "tr_group_level", "probe_set", "harness_ok")})
    for f in facts["fact"]:
        per_run = {r["run"]: r["facts"].get(f["id"], {}).get("status", "unverified") for r in runs}
        answered = {s for s in per_run.values() if s not in {"inconclusive", "unverified"}}
        if not answered:
            st = "inconclusive" if per_run else "unverified"
        elif len(answered) == 1:
            st = answered.pop()
        else:
            st = "mixed"
        out["facts"][f["id"]] = {"status": st, "runs": per_run}
    return out


def report(name: str, interface: str, ev: dict, defs: dict, facts: dict) -> str:
    links = facts.get("link", [])
    lines = [
        f"# Db2 for i probe run `{name}`",
        "",
        f"- Interface: {interface}",
        f"- IBM i release: {ev['os_release'] or 'not recorded'}; Db2 for IBM i group level: {ev['db2_group_level'] or 'not recorded'}; TR group level: {ev['tr_group_level'] or 'not recorded'}",
        f"- Probe set: {ev['probe_set']} (current: {ev['current_probe_set']})",
        f"- Harness: {'ok' if ev['harness_ok'] else 'FAILED: every fact is inconclusive'}",
        "",
        "These are observations on one IBM i system at the release above, not general facts.",
        "",
        "## Facts",
        "",
        "| Fact | Status | Why |",
        "|---|---|---|",
    ]
    for f in facts["fact"]:
        s = ev["facts"][f["id"]]
        lines.append(f"| {f['id']} {f['title']} | **{s['status']}** | {s['why']} |")
    lines += ["", "## Follow-up", ""]
    todo = False
    for f in facts["fact"]:
        st = ev["facts"][f["id"]]["status"]
        if st not in RESOLVED and st != "observed":
            continue
        todo = True
        lines.append(f"- **{f['id']} ({st})**: update the {f['id']} row and section of `docs/vendor-facts-verification.md` (the summary row must mention the probe run).")
        for link in links:
            if f["id"] in link["facts"] and st in RESOLVED:
                lines.append(f"  - `{link['file']}`, at `{link['anchor']}`: remove the hedge \"{link['hedge']}\" and state what the probe showed.")
        for path in f.get("review", []):
            lines.append(f"  - review `{path}`")
        if st == "contradicted":
            lines.append("  - the conversion or engine rule that relies on it must change, not only its wording.")
    if not todo:
        lines.append("Nothing resolved in this run.")
    lines += ["", "## Probes", "", "| Probe | Role | Observed | Outcome | Verdict |", "|---|---|---|---|---|"]
    for j in ev["probes"]:
        o = j["observed"]
        if o is None:
            seen = "no result"
        else:
            seen = ", ".join(
                f"{k}={o[k]}" for k in ("create", "call", "state", "code", "n1", "n2", "text") if o.get(k) is not None
            )
        lines.append(f"| {j['id']} | {j['role']} | {seen} | {j['outcome'] or '-'}: {j['meaning']} | {j['verdict']} |")
    msgs = [(j["id"], o.get(k)) for j in ev["probes"] if (o := j["observed"]) for k in ("create_message", "call_message") if o.get(k)]
    if msgs:
        lines += ["", "## Db2 messages", ""]
        seen = set()
        for pid, m in msgs:
            if (pid, m) not in seen:
                seen.add((pid, m))
                lines.append(f"- {pid}: {m}")
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------- commands


def cmd_build(_args) -> int:
    defs, facts = load_probes(), load_facts()
    errs = validate(defs, facts)
    if errs:
        print("\n".join(errs), file=sys.stderr)
        return 1
    SCRIPT.write_text(build_script(defs))
    print(f"wrote {SCRIPT.relative_to(HERE.parent.parent)}")
    return 0


def cmd_check(_args) -> int:
    defs, facts = load_probes(), load_facts()
    errs = validate(defs, facts)
    if not SCRIPT.exists() or SCRIPT.read_text() != build_script(defs):
        errs.append("sqldiff-db2i-probes.sql is stale: run `python3 probes/db2i/db2i_probes.py build`")
    long = [i + 1 for i, line in enumerate(build_script(defs).splitlines()) if len(line) > WIDTH]
    if long:
        errs.append(f"script lines longer than {WIDTH}: {long[:5]}")
    status = json.loads(STATUS.read_text())
    if status != recompute_status(facts):
        errs.append("status.json does not match runs/: run ingest again or restore it")
    if errs:
        print("\n".join(errs), file=sys.stderr)
        return 1
    print("ok")
    return 0


def cmd_ingest(args) -> int:
    defs, facts = load_probes(), load_facts()
    src = Path(args.results)
    rows = parse_results(src.read_text(encoding="utf-8", errors="replace"))
    if not rows:
        print(f"no '{LINE_TAG}' lines in {src}", file=sys.stderr)
        return 1
    ev = evaluate(defs, facts, rows)
    if ev["probe_set"] != ev["current_probe_set"]:
        print(f"warning: results come from probe set {ev['probe_set']}, probes.toml is now {ev['current_probe_set']}", file=sys.stderr)
    name = args.run or src.stem
    if not re.fullmatch(r"[A-Za-z0-9._-]+", name):
        print("run name: letters, digits, . _ -", file=sys.stderr)
        return 1
    RUNS.mkdir(exist_ok=True)
    record = {"run": name, "interface": args.interface, **ev, "rows": rows}
    (RUNS / f"{name}.json").write_text(json.dumps(record, indent=2) + "\n")
    (RUNS / f"{name}.md").write_text(report(name, args.interface, ev, defs, facts))
    STATUS.write_text(json.dumps(recompute_status(facts), indent=2) + "\n")
    for fid, s in ev["facts"].items():
        print(f"{fid:5} {s['status']:13} {s['why']}")
    print(f"wrote runs/{name}.json, runs/{name}.md and status.json")
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("build").set_defaults(fn=cmd_build)
    sub.add_parser("check").set_defaults(fn=cmd_check)
    ing = sub.add_parser("ingest")
    ing.add_argument("results")
    ing.add_argument("--interface", required=True, help="what ran the script, e.g. acs or runsqlstm")
    ing.add_argument("--run", help="run name (default: the results file name)")
    ing.set_defaults(fn=cmd_ingest)
    args = ap.parse_args(argv)
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())
