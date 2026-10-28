"""Protocol tests for the built checker: confirmed only for identical
sides, unconfirmed for anything else, including malformed requests.

Usage: python3 test_gate.py PATH-TO-BINARY
"""

import json
import subprocess
import sys

exe = sys.argv[1]
P = "sqldiff-check/1"
cases = [
    ("1", f"{P}\t1\ttsql\t2022\t(ROOT (A x))\ttsql\t2022\t(ROOT (A x))\tend", "confirmed"),
    ("2", f"{P}\t2\ttsql\t2022\t(ROOT (A x))\ttsql\t2022\t(ROOT (A y))\tend", "unconfirmed"),
    ("3", f"{P}\t3\ttsql\t2022\t(ROOT)\ttsql\t2019\t(ROOT)\tend", "unconfirmed"),
    ("4", f"{P}\t4\ttsql\t2022\t(ROOT)\tpostgres\t2022\t(ROOT)\tend", "unconfirmed"),
    ("0", f"{P}\t5\ttsql\t2022\t(ROOT)\ttsql\t2022\t(ROOT)", "unconfirmed"),
    ("0", f"{P}\tx5\ttsql\t2022\t(ROOT)\ttsql\t2022\t(ROOT)\tend", "unconfirmed"),
    ("0", f"sqldiff-check/0\t7\ttsql\t2022\t(ROOT)\ttsql\t2022\t(ROOT)\tend", "unconfirmed"),
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
