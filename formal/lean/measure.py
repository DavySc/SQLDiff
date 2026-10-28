"""Measure the spike checker: start-up, throughput, size, linked libraries.

Usage: python3 measure.py PATH-TO-BINARY
Prints one JSON object; CI keeps it as the run's result.
"""

import json
import os
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

requests = 20000
payload = "".join('{"a":"x","b":"y"}\n' for _ in range(requests)).encode()
t = time.perf_counter()
out = subprocess.run([exe], input=payload, check=True, capture_output=True).stdout
batch_ms = (time.perf_counter() - t) * 1000
answers = out.decode().splitlines()
assert len(answers) == requests, len(answers)
assert all('"proof":"unconfirmed"' in a for a in answers)

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
    "linked": libs.strip().splitlines(),
}, indent=2))
