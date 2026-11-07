#!/usr/bin/env python3
"""Determinism on real code: the same tree converted with any thread count,
fresh or resumed from a store another run made, into any output folder gives
byte-identical files, and reports that differ only in what the contract allows.

Runs, for each T-SQL source of corpus/real-code/sources.json (fetched and
pinned like the real-code gate):

  a  -j1 fresh                      d  -j1 resumed on b's store
  b  -j4 fresh                      e  -j2 fresh, other output folder
  c  -j4 resumed on a's store

and compares a with every other run. Allowed to differ (the same list as
crates/sqldiff-convert/tests/determinism.rs): in conversion-report.json the
top-level `run`, `src`, `out` and `ledger.path`; in the .md report the output
folder's name. Everything else, every converted file and every reason,
compares whole. The store itself (.sqldiff) is not compared here.

usage: determinism-corpus.py [--sqldiff PATH] [--work DIR] [--only NAME]
"""
import argparse, importlib.util, json, os, shutil, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)

spec = importlib.util.spec_from_file_location("gate", os.path.join(HERE, "real-code-gate.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


def convert(binary, src, out, jobs, resume):
    cmd = [binary, "convert", src, "-o", out, "--from", "tsql", "--to", "postgres", "-j", str(jobs)]
    cmd += ["--overwrite"] if resume else ["--fresh"]
    r = subprocess.run(cmd, capture_output=True, text=True)
    # 0: all converted, 1: something is not. Anything else is a crash.
    if r.returncode not in (0, 1):
        sys.exit(f"convert {' '.join(cmd[2:])}: exit {r.returncode}\n{r.stderr[-2000:]}")


def plain_report(text):
    j = json.loads(text)
    for k in ("run", "src", "out"):
        j.pop(k, None)
    if isinstance(j.get("ledger"), dict):
        j["ledger"].pop("path", None)
    return json.dumps(j, sort_keys=True)


def tree(out):
    files = {}
    for dp, dirs, fs in os.walk(out):
        dirs[:] = sorted(d for d in dirs if d != ".sqldiff")
        for f in sorted(fs):
            p = os.path.join(dp, f)
            rel = os.path.relpath(p, out).replace(os.sep, "/")
            text = open(p, "rb").read().decode("utf-8", "replace")
            if rel.startswith("conversion-report") and rel.endswith(".json"):
                text = plain_report(text)
            elif rel.startswith("conversion-report") and rel.endswith(".md"):
                text = text.replace(out, "<out>")
            files[rel] = text
    return files


def compare(name, a, b, what):
    bad = []
    for k in sorted(set(a) | set(b)):
        if k not in a or k not in b:
            bad.append(f"{k}: only in one run")
        elif a[k] != b[k]:
            at = next((i for i, (x, y) in enumerate(zip(a[k], b[k])) if x != y), min(len(a[k]), len(b[k])))
            bad.append(f"{k}: differs at {at}: ...{a[k][max(0, at - 80):at + 120]!r} vs ...{b[k][max(0, at - 80):at + 120]!r}")
    for line in bad[:20]:
        print(f"  {name} {what}: {line}")
    return len(bad)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sqldiff", default=os.path.join(ROOT, "target", "release", "sqldiff"))
    ap.add_argument("--work", default=os.path.join(ROOT, "target", "determinism"))
    ap.add_argument("--only")
    a = ap.parse_args()
    manifest = json.load(open(os.path.join(ROOT, "corpus", "real-code", "sources.json")))
    srcs = os.path.join(a.work, "src")
    bad = 0
    ran = 0
    for src in manifest["sources"]:
        if a.only and src["name"] != a.only:
            continue
        dest = gate.fetch(src, srcs)
        for p in src["paths"]:
            if p["dialect"] != "tsql":
                continue
            ran += 1
            name = f"{src['name']}/{p['path']}"
            tree_dir = os.path.normpath(os.path.join(dest, p["path"]))
            runs = os.path.join(a.work, "runs", src["name"])
            shutil.rmtree(runs, ignore_errors=True)
            out = {k: os.path.join(runs, k) for k in "abcde"}
            convert(a.sqldiff, tree_dir, out["a"], 1, resume=False)
            convert(a.sqldiff, tree_dir, out["b"], 4, resume=False)
            # c resumes a's store, d resumes b's: copy so that a and b stay as they were.
            shutil.copytree(out["a"], out["c"])
            convert(a.sqldiff, tree_dir, out["c"], 4, resume=True)
            shutil.copytree(out["b"], out["d"])
            convert(a.sqldiff, tree_dir, out["d"], 1, resume=True)
            convert(a.sqldiff, tree_dir, out["e"], 2, resume=False)
            base = tree(out["a"])
            n = 0
            for k, what in [("b", "-j4 fresh"), ("c", "-j4 resumed on the -j1 store"),
                            ("d", "-j1 resumed on the -j4 store"), ("e", "-j2 into another folder")]:
                n += compare(name, base, tree(out[k]), what)
            print(f"{name}: {len(base)} files, {n} difference(s)", flush=True)
            bad += n
            shutil.rmtree(runs, ignore_errors=True)
    if ran == 0:
        sys.exit("determinism-corpus: no source matched")
    if bad:
        sys.exit(f"{bad} difference(s): the same input gave different output")


if __name__ == "__main__":
    main()
