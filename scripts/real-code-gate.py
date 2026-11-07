#!/usr/bin/env python3
"""Real-code gate: `sqldiff check` on unmodified public SQL must report no
`different` finding. Every such finding on code that runs is a false positive.

usage: real-code-gate.py [--sqldiff PATH] [--work DIR] [--only NAME]

Sources and pinned commits: corpus/real-code/sources.json. Files that are not
UTF-8 (UTF-16 exports) are skipped and counted.
"""
import argparse, collections, hashlib, json, os, shutil, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def git(*args, cwd=None, hint=""):
    # Never prompt: a source that has gone private or been removed must fail
    # with a clear message, not wait for credentials (and none are ever added).
    env = {**os.environ, "GIT_TERMINAL_PROMPT": "0"}
    try:
        subprocess.run(["git", *args], cwd=cwd, check=True, stdout=subprocess.DEVNULL, env=env)
    except subprocess.CalledProcessError:
        sys.exit(f"real-code gate: `git {' '.join(args)}` failed.{hint}")


def vendored(src):
    """A source kept in the repository: no network, bytes checked against sha256."""
    dest = os.path.join(ROOT, src["local"])
    # Every .sql file the gate will read must be pinned: an added file is a failure.
    found = sorted(
        os.path.relpath(os.path.join(dp, f), dest).replace(os.sep, "/")
        for dp, _, fs in os.walk(dest) for f in fs if f.lower().endswith(".sql")
    )
    if found != sorted(src["sha256"]):
        sys.exit(f"real-code gate: vendored {src['name']} .sql files {found} != pinned {sorted(src['sha256'])}")
    for rel, want in src["sha256"].items():
        got = hashlib.sha256(open(os.path.join(dest, rel), "rb").read()).hexdigest()
        if got != want:
            sys.exit(f"real-code gate: vendored {src['name']}/{rel} differs from the pinned upstream bytes ({got} != {want})")
    return dest


def fetch(src, work):
    if src.get("local"):
        return vendored(src)
    dest = os.path.join(work, src["name"])
    head = os.path.join(dest, ".git", "HEAD")
    if os.path.exists(head):
        cur = subprocess.run(["git", "rev-parse", "HEAD"], cwd=dest, capture_output=True, text=True).stdout.strip()
        if cur == src["commit"]:
            return dest
    # Start from an empty directory: a restored cache can leave a stale tree
    # that a fresh checkout would refuse to overwrite.
    shutil.rmtree(dest, ignore_errors=True)
    os.makedirs(dest, exist_ok=True)
    git("init", "-q", cwd=dest)
    subprocess.run(["git", "remote", "remove", "origin"], cwd=dest, capture_output=True)
    git("remote", "add", "origin", src["repo"], cwd=dest)
    if src.get("sparse"):
        git("sparse-checkout", "set", src["sparse"], cwd=dest)
    git("fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", src["commit"], cwd=dest,
        hint=" The pinned source may be gone or private; drop or re-pin it in corpus/real-code/sources.json.")
    git("checkout", "-q", "FETCH_HEAD", cwd=dest)
    return dest


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sqldiff", default=os.path.join(ROOT, "target", "release", "sqldiff"))
    ap.add_argument("--work", default=os.path.join(ROOT, "target", "real-code"))
    ap.add_argument("--only")
    a = ap.parse_args()
    manifest = json.load(open(os.path.join(ROOT, "corpus", "real-code", "sources.json")))
    failures = []
    totals = collections.Counter()
    for src in manifest["sources"]:
        if a.only and src["name"] != a.only:
            continue
        dest = fetch(src, a.work)
        counts = collections.Counter()
        for p in src["paths"]:
            base = os.path.join(dest, p["path"])
            seen = 0
            for dp, dirs, fs in os.walk(base):
                dirs[:] = sorted(d for d in dirs if d != ".git")
                for f in sorted(fs):
                    if not f.lower().endswith(".sql"):
                        continue
                    seen += 1
                    path = os.path.join(dp, f)
                    rel = os.path.relpath(path, dest)
                    try:
                        open(path, encoding="utf-8").read()
                    except UnicodeDecodeError:
                        counts["skipped (not UTF-8)"] += 1
                        continue
                    r = subprocess.run(
                        [a.sqldiff, "check", path, "-d", p["dialect"], "-f", "json", "--no-cache"],
                        capture_output=True, text=True, timeout=600,
                    )
                    try:
                        rep = json.loads(r.stdout)
                    except json.JSONDecodeError:
                        failures.append(f"{src['name']}/{rel}: no report ({r.stderr.strip()[:200]})")
                        continue
                    counts[rep["verdict"]] += 1
                    for x in rep["findings"]:
                        if x["verdict"] == "different":
                            line = (x.get("a") or {}).get("start_line")
                            failures.append(f"{src['name']}/{rel}:{line}: {x['rule']}: {x['witness']}")
            # A moved or emptied path must not pass by checking nothing.
            if seen == 0:
                failures.append(f"{src['name']}/{p['path']}: no .sql files found")
        totals.update(counts)
        print(f"{src['name']}: {dict(sorted(counts.items()))}")
    print(f"total: {dict(sorted(totals.items()))}")
    if failures:
        print(f"\n{len(failures)} failure(s): `different` findings on unmodified real code, or sources with nothing to check:")
        for f in failures:
            print("  " + f)
        sys.exit(1)


if __name__ == "__main__":
    main()
