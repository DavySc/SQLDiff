"""Measure the checker: start-up, throughput on one-statement and
400-statement requests, size, linked libraries.

Usage: python3 measure.py PATH-TO-BINARY
Prints one JSON object; CI keeps it as the run's result.
"""

import json
import os
import pathlib
import re
import platform
import statistics
import subprocess
import sys
import time

exe = sys.argv[1]
starts = []
for _ in range(30):
    t = time.perf_counter()
    subprocess.run([exe, "--version"], check=True, capture_output=True)
    starts.append((time.perf_counter() - t) * 1000)

syntax = (pathlib.Path(__file__).parent / "Sqldiff" / "Syntax.lean").read_text(encoding="utf-8")
H = re.search(r'def schemaHash : String := "([0-9a-f]+)"', syntax).group(1)


def hexs(s):
    return s.encode().hex()


def stmt(i):
    words = ["update", "t", "set", "x"]
    toks = " ".join(f"IDENT:{hexs(w)}" for w in words)
    return f"(STATEMENT {toks} OP:3d NUMBER:{hexs(str(i))} SEMICOLON:3b)"


def tree(n):
    return "(SOURCE_FILE " + " ".join(stmt(i) for i in range(n)) + ")"


def run(n_requests, statements):
    t_ = tree(statements)
    line = f"sqldiff-check/2\t1\t{H}\ttsql\t2022\t{t_}\ttsql\t2022\t{t_}\tend\n"
    payload = (line * n_requests).encode()
    t = time.perf_counter()
    out = subprocess.run([exe], input=payload, check=True, capture_output=True).stdout
    ms = (time.perf_counter() - t) * 1000
    answers = out.decode().splitlines()
    assert len(answers) == n_requests, len(answers)
    assert all('"proof":"confirmed"' in a for a in answers), answers[0]
    return ms


requests = 20000
batch_ms = run(requests, 1)
large = 50
large_ms = run(large, 400)

libs = ""
for tool in (["ldd", exe], ["otool", "-L", exe]):
    try:
        libs = subprocess.run(tool, capture_output=True, text=True).stdout
        break
    except FileNotFoundError:
        continue

print(json.dumps({
    "platform": f"{platform.system()} {platform.machine()}",
    "size_bytes": os.path.getsize(exe),
    "startup_ms_median": round(statistics.median(starts), 2),
    "startup_ms_max": round(max(starts), 2),
    "requests": requests,
    "batch_ms": round(batch_ms, 1),
    "per_request_us": round(batch_ms * 1000 / requests, 2),
    "per_400_statement_request_ms": round(large_ms / large, 2),
    "linked": libs.strip().splitlines(),
}, indent=2))
