"""Protocol tests for the built checker: confirmed only for identical
sides that decode under the checker's syntax schema, unconfirmed for
anything else, including malformed requests.

Usage: python3 test_gate.py PATH-TO-BINARY
"""

import json
import pathlib
import re
import subprocess
import sys

exe = sys.argv[1]
P = "sqldiff-check/2"
syntax = (pathlib.Path(__file__).parent / "Sqldiff" / "Syntax.lean").read_text()
H = re.search(r'def schemaHash : String := "([0-9a-f]+)"', syntax).group(1)
# `SELECT 1` and `SELECT 2` as sqldiff_core::proof::tree_line writes them.
ONE = "(SOURCE_FILE (STATEMENT IDENT:73656c656374 NUMBER:31 INFERRED_TERMINATOR:))"
TWO = "(SOURCE_FILE (STATEMENT IDENT:73656c656374 NUMBER:32 INFERRED_TERMINATOR:))"


def req(i, xa, xb, ta="2022", db="tsql", schema=H):
    return f"{P}\t{i}\t{schema}\ttsql\t{ta}\t{xa}\t{db}\t2022\t{xb}\tend"


cases = [
    ("1", req(1, ONE, ONE), "confirmed"),
    ("2", req(2, ONE, TWO), "unconfirmed"),
    ("3", req(3, ONE, ONE, ta="2019"), "unconfirmed"),
    ("4", req(4, ONE, ONE, db="postgres"), "unconfirmed"),
    # Identical sides are confirmed only when they decode under this schema.
    ("5", req(5, ONE, ONE, schema="0000000000000000"), "unconfirmed"),
    ("6", req(6, "(ROOT)", "(ROOT)"), "unconfirmed"),
    ("7", req(7, "(IDENT)", "(IDENT)"), "unconfirmed"),
    ("8", req(8, "(SOURCE_FILE BATCH:31)", "(SOURCE_FILE BATCH:31)"), "unconfirmed"),
    ("9", req(9, "(SOURCE_FILE WHITESPACE:20)", "(SOURCE_FILE WHITESPACE:20)"), "unconfirmed"),
    ("10", req(10, "(SOURCE_FILE NUMBER:3)", "(SOURCE_FILE NUMBER:3)"), "unconfirmed"),
    ("11", req(11, "(SOURCE_FILE NUMBER:4A)", "(SOURCE_FILE NUMBER:4A)"), "unconfirmed"),
    ("12", req(12, ONE + " x", ONE + " x"), "unconfirmed"),
    ("13", req(13, "(SOURCE_FILE", "(SOURCE_FILE"), "unconfirmed"),
    ("0", req(14, ONE, ONE)[: -len("\tend")], "unconfirmed"),
    ("0", req("x15", ONE, ONE), "unconfirmed"),
    ("0", f"sqldiff-check/1\t16\ttsql\t2022\t{ONE}\ttsql\t2022\t{ONE}\tend", "unconfirmed"),
    ("0", "garbage", "unconfirmed"),
]
inp = "".join(line + "\n" for _, line, _ in cases)
out = subprocess.run([exe], input=inp.encode(), capture_output=True, check=True).stdout.decode()
answers = [json.loads(line) for line in out.splitlines()]
assert len(answers) == len(cases), out
for (want_id, line, want), got in zip(cases, answers):
    assert got["id"] == want_id, (line, got)
    assert got["proof"] == want, (line, got)
    if want == "confirmed":
        assert got["theorem"] == "Sqldiff.Gate.check_sound", got
print(f"gate protocol ok ({len(cases)} cases)")
