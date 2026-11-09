#!/usr/bin/env python3
"""Hang gate: adversarial and audit inputs under a time limit and a memory ceiling.

Converts every file written by scripts/gen-adversarial.py (the synthetic shapes
and the audit repros, prefix n_) and every file under tests/hang/ if present, alone, -j 1, to PostgreSQL, and fails when one

  * is killed at the time limit,
  * peaks above the resident-memory ceiling,
  * dies from a signal (stack overflow, abort, out of memory), or
  * exits with a code other than 0 or 1 (1 = something is not converted).

The limits are generous; they exist to catch "runs for minutes or forever" and
"takes gigabytes", not to measure speed. Wall time is only ever compared here,
never inside the engine (the engine's budgets are counts).

  scripts/hang-gate.py [--sqldiff PATH] [--timeout SEC] [--rss-mb MB] [--only SUBSTR] [--known-open]
"""
import argparse, os, shutil, signal, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

# Per-file limits where the default is wrong: name prefix -> (seconds, MB).
# Every entry needs the measurement that justifies it.
OVERRIDES = {}

# Shapes that still run too long or take too much memory, each with the row of
# the performance plan (conversion-performance-plan.md, section 2.3)
# that fixes it, measured on main at 4 cores. The normal run skips them (it
# would only spend CI minutes being killed); `--known-open` runs only them and
# fails when one now passes, so the entry is removed in the PR that fixes it.
# An entry that finishes (slowly) rather than being killed is a tuple
# (reason, seconds, MB): the limits it is known to break, used in that mode.
# The behaviour model's work caps (unit and file, `behaviour::unit_work_cap`)
# turn a runaway there into "not proven: work budget". Statement-level work
# (alignment, the edit checker, witness search) is not capped until step B (a later PR);
# one alignment can still use up to LCS_WORK cells.
KNOWN_OPEN = {
    "j_branches_16": "the input doubles per depth (10.5 MB, 131k statements in one routine): 125 s and 3.3 GB, over the 120 s and 2 GB limits; depth 14 (26 s, 0.9 GB) is guarded in the normal run",
    "a_procs_500x20": "R1: 246 s (0.5 s per routine)",
}


def run(binary, path, out, timeout, kill_rss_mb, dialect="tsql"):
    start = time.time()
    # Db2 for i has no conversion rules yet: its repros are checked, not converted.
    cmd = ([binary, "check", path, "-d", "db2i", "-f", "json", "--no-cache"] if dialect == "db2i"
           else [binary, "convert", path, "-o", out, "--from", dialect, "--to", "postgres", "-j", "1"])
    p = subprocess.Popen(
        cmd,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    peak = 0
    status = None
    while True:
        done, st, ru = os.wait4(p.pid, os.WNOHANG)
        if done:
            status = st
            peak = max(peak, ru.ru_maxrss // 1024)
            break
        try:
            with open(f"/proc/{p.pid}/status") as f:
                for line in f:
                    if line.startswith("VmRSS"):
                        peak = max(peak, int(line.split()[1]) // 1024)
        except OSError:
            pass
        if time.time() - start > timeout or peak > kill_rss_mb * 2:
            os.killpg(p.pid, signal.SIGKILL)
            os.wait4(p.pid, 0)
            return "KILLED" if time.time() - start > timeout else "MEMORY", time.time() - start, peak
        time.sleep(0.1)
    wall = time.time() - start
    if os.WIFSIGNALED(status):
        return f"SIGNAL {os.WTERMSIG(status)}", wall, peak
    code = os.WEXITSTATUS(status)
    return ("ok" if code in (0, 1) else f"EXIT {code}"), wall, peak


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sqldiff", default=os.path.join(ROOT, "target", "release", "sqldiff"))
    ap.add_argument("--timeout", type=float, default=120)
    ap.add_argument("--rss-mb", type=int, default=2048)
    ap.add_argument("--only", default="")
    ap.add_argument("--known-open", action="store_true",
                    help="run only the known-open shapes; fail when one passes")
    a = ap.parse_args()
    work = tempfile.mkdtemp(prefix="hang-gate-")
    try:
        inputs = os.path.join(work, "in")
        subprocess.run([sys.executable, os.path.join(HERE, "gen-adversarial.py"), inputs],
                       check=True, stdout=subprocess.DEVNULL)
        files = [os.path.join(inputs, f) for f in sorted(os.listdir(inputs)) if f.endswith(".sql")]
        extra = os.path.join(ROOT, "tests", "hang")
        if os.path.isdir(extra):
            files += [os.path.join(extra, f) for f in sorted(os.listdir(extra)) if f.endswith(".sql")]
        bad = 0
        for f in files:
            name = os.path.basename(f)
            if a.only not in name:
                continue
            known = next((r for k, r in KNOWN_OPEN.items() if name.startswith(k + ".")), None)
            known_limits = None
            if isinstance(known, tuple):
                known, *known_limits = known
            if known and not a.known_open:
                print(f"{name:32} skipped (known open: {known})")
                continue
            if a.known_open and not known:
                continue
            secs, mb = a.timeout, a.rss_mb
            for prefix, (s, m) in OVERRIDES.items():
                if name.startswith(prefix + "."):
                    secs, mb = s, m
            if a.known_open and known_limits:
                secs, mb = known_limits
            out = os.path.join(work, "out", name)
            dialect = "db2i" if "db2i" in name else "tsql"
            result, wall, peak = run(a.sqldiff, f, out, secs, mb, dialect)
            if result == "ok" and peak > mb:
                result = f"MEMORY ({peak} MB > {mb} MB)"
            if a.known_open:
                # Expected to fail; passing means it is fixed and the entry must go.
                flag = "  <== NOW PASSES: remove it from KNOWN_OPEN" if result == "ok" else ""
                bad += result == "ok"
            else:
                flag = "" if result == "ok" else "  <== FAIL"
                bad += result != "ok"
            print(f"{name:32} {result:10} {wall:7.1f}s {peak:6} MB{flag}", flush=True)
            shutil.rmtree(out, ignore_errors=True)
        print(f"{bad} failing")
        return 1 if bad else 0
    finally:
        shutil.rmtree(work, ignore_errors=True)


sys.exit(main())
