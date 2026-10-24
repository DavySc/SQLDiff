#!/usr/bin/env python3
"""Mutation harness for SQLDiff's same-dialect verdicts.

Real stored procedures and functions (pinned in sources.json) are edited two
ways: a mutant changes the code's behaviour (an operator, a literal, a join,
a dropped statement or predicate line) and a preserving edit only touches
layout, keyword case or comments. Every edit is made on code tokens; string
literals and comments are never changed by a mutation, and preserving edits
only touch whitespace, keyword case and new comment lines.

    mutate.py fetch EXT                 clone the pinned sources into EXT
    mutate.py gen SET EXT OUT [--seed N] [--files N]
    mutate.py run OUT RESULTS [--bin PATH] [--jobs N] [--timeout S]
    mutate.py gate RESULTS... [--min 0.80]

The gate fails on any mutant reported "equal" (unless equivalent.json lists
it as a reviewed equivalent mutant), on any preserving edit reported
"different", and when fewer than --min of the remaining mutants in a set are
reported "different".
"""

import argparse
import json
import os
import pathlib
import random
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import sqltok  # noqa: E402

SOURCES = json.load(open(os.path.join(HERE, "sources.json")))


# ---------------------------------------------------------------- fetch

def fetch(ext):
    os.makedirs(ext, exist_ok=True)
    for name, r in SOURCES["repos"].items():
        d = os.path.join(ext, name)
        if os.path.isdir(os.path.join(d, ".git")):
            head = subprocess.run(["git", "-C", d, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
            if head == r["sha"]:
                continue
        else:
            subprocess.run(["git", "init", "-q", d], check=True)
            subprocess.run(["git", "-C", d, "remote", "add", "origin", r["url"]], check=True)
        if r.get("sparse"):
            subprocess.run(["git", "-C", d, "sparse-checkout", "set", "--no-cone", *r["sparse"]], check=True)
        subprocess.run(["git", "-C", d, "fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", r["sha"]], check=True)
        subprocess.run(["git", "-C", d, "checkout", "-q", "FETCH_HEAD"], check=True)


# ---------------------------------------------------------------- edits

def line_col(src, pos):
    line = src.count("\n", 0, pos) + 1
    return line, pos - (src.rfind("\n", 0, pos) + 1) + 1


def splice(src, start, end, text):
    return src[:start] + text + src[end:]


def nxt(toks, i, skip=("ws", "comment")):
    j = i + 1
    while j < len(toks) and toks[j].kind in skip:
        j += 1
    return j


def word(toks, i, *ups):
    return i < len(toks) and toks[i].kind == "word" and toks[i].up in ups


def first_on_line(toks, i):
    """The token starts its line, apart from whitespace."""
    j = i - 1
    while j >= 0 and toks[j].kind == "ws" and "\n" not in toks[j].text:
        j -= 1
    return j < 0 or (toks[j].kind == "ws" and "\n" in toks[j].text) or (toks[j].kind == "comment" and toks[j].text.startswith("--"))


def line_span(src, toks, first_line, last_line):
    """Character span of whole lines first_line..last_line, or None when a
    token crosses either boundary (a multi-line string or comment)."""
    starts = [0] + [m.end() for m in re.finditer("\n", src)]
    if last_line >= len(starts):
        return None
    a = starts[first_line - 1]
    b = starts[last_line] if last_line < len(starts) else len(src)
    for t in toks:
        if t.kind == "ws":
            continue
        if (t.start < a < t.end) or (t.start < b < t.end):
            return None
    return a, b


def candidates(src, toks, op):
    """Yield (start, end, replacement) edits for one mutation operator."""
    for i, t in enumerate(toks):
        k, up = t.kind, t.up
        if op == "relop" and k == "op" and t.text == "=":
            yield t.start, t.end, "<>"
        elif op == "relop_gt" and k == "op" and t.text == ">":
            yield t.start, t.end, ">="
        elif op == "literal" and k == "num" and t.text.isdigit():
            yield t.start, t.end, str(int(t.text) + 1)
        elif op == "and_or" and k == "word" and up == "AND":
            yield t.start, t.end, "OR"
        elif op == "isnull_null" and k == "word" and up == "IS":
            j = nxt(toks, i)
            if word(toks, j, "NULL"):
                yield t.start, toks[j].end, "IS NOT NULL"
        elif op == "asc_desc" and k == "word" and up == "DESC":
            yield t.start, t.end, "ASC"
        elif op == "max_min" and k == "word" and up == "MAX" and i + 1 < len(toks) and toks[i + 1].text == "(":
            yield t.start, t.end, "MIN"
        elif op == "left_inner" and k == "word" and up == "LEFT":
            j = nxt(toks, i)
            if word(toks, j, "OUTER"):
                j = nxt(toks, j)
            if word(toks, j, "JOIN"):
                yield t.start, toks[j].end, "INNER JOIN"
        elif op == "plus_minus" and k == "op" and t.text == "+" and 1 <= i < len(toks) - 2:
            p, n = toks[i - 1], toks[i + 1]
            if p.text == " " and n.text == " " and i >= 2 and re.search(r"\w$", toks[i - 2].text) \
                    and toks[i - 2].kind in ("word", "num", "var", "qident") and re.match(r"[\w@(]", toks[i + 2].text):
                yield t.start, t.end, "-"
        elif op == "drop_stmt" and k == "word" and up in ("UPDATE", "DELETE", "INSERT") and first_on_line(toks, i):
            end_line = None
            for u in toks[i + 1:]:
                if u.kind == "punct" and u.text == ";":
                    end_line = u.line
                    break
                if u.kind == "ws" and "\n\n" in u.text.replace("\r", "").replace(" ", "").replace("\t", ""):
                    end_line = u.line + u.text.count("\n", 0, u.text.index("\n") + 1) - 1
                    break
            if end_line is None or end_line - t.line > 25:
                continue
            span = line_span(src, toks, t.line, end_line)
            if span:
                yield span[0], span[1], ""
        elif op == "drop_and_line" and k == "word" and up == "AND" and first_on_line(toks, i):
            span = line_span(src, toks, t.line, t.line)
            if span:
                yield span[0], span[1], ""


MUTATIONS = ["relop", "relop_gt", "literal", "and_or", "isnull_null", "asc_desc", "max_min",
             "left_inner", "plus_minus", "drop_stmt", "drop_and_line"]
KEYWORDS = set("""select from where update set insert into values delete begin end if else and or not
null is as on join inner left group by order declare return exists in""".split())


def preserve(src, toks, kind, rng):
    out = []
    if kind == "reformat":
        for i, t in enumerate(toks):
            text = t.text
            if t.kind == "ws" and "\n" in text:
                text = text.replace("\n", "  \n    ")
            out.append(text)
            if t.kind == "punct" and t.text == "," and i + 1 < len(toks) and toks[i + 1].kind != "ws":
                out.append(" ")
    elif kind == "case":
        for t in toks:
            if t.kind == "word" and t.text.lower() in KEYWORDS:
                out.append(t.text.lower() if t.text.isupper() else t.text.upper())
            else:
                out.append(t.text)
    elif kind == "comment":
        spots = [i for i, t in enumerate(toks) if t.kind == "ws" and "\n" in t.text]
        chosen = set(rng.sample(spots, min(5, len(spots))))
        for i, t in enumerate(toks):
            out.append(t.text.replace("\n", "\n-- reviewer note added here\n", 1) if i in chosen else t.text)
    return "".join(out)


PRESERVING = ["reformat", "case", "comment"]


# ---------------------------------------------------------------- gen

def read(path):
    return open(path, encoding="utf-8-sig", errors="replace").read()


def build_catalog(base, pattern, dialect, out):
    files = sorted(str(p.relative_to(base)) for p in pathlib.Path(base).glob(pattern))
    sep = "\nGO\n" if dialect == "tsql" else "\n"
    with open(out, "w") as fh:
        fh.write(sep.join(read(os.path.join(base, f)) for f in files))
    return len(files)


def gen(set_name, ext, out, seed=None, nfiles=None):
    cfg = SOURCES["sets"][set_name]
    dialect = cfg["dialect"]
    seed = cfg["seed"] if seed is None else seed
    nfiles = cfg["files"] if nfiles is None else nfiles
    base = os.path.join(ext, cfg["repo"], cfg["root"])
    os.makedirs(out, exist_ok=True)
    catalogs = {}
    for top, pattern in cfg["catalogs"].items():
        path = os.path.join(out, "catalog-%s.sql" % ("default" if top == "*" else top))
        build_catalog(base, pattern, dialect, path)
        catalogs[top] = os.path.basename(path)
    files = []
    for inc in cfg["include"]:
        for d, _, fs in os.walk(os.path.join(base, inc)):
            for f in fs:
                if f.endswith(".sql"):
                    files.append(os.path.relpath(os.path.join(d, f), base))
    files.sort()
    rng = random.Random(seed)
    rng.shuffle(files)
    cases, used = [], 0
    for rel in files:
        src = read(os.path.join(base, rel))
        toks = sqltok.tokenize(src, dialect)
        code = sqltok.code_tokens(toks)
        if len({t.line for t in code}) < 8 or not any(t.kind == "word" and t.up in ("PROCEDURE", "PROC", "FUNCTION") for t in code):
            continue
        used += 1
        if used > nfiles:
            break
        catalog = catalogs.get(rel.split("/")[0], catalogs.get("*"))
        for op in MUTATIONS:
            cands = list(candidates(src, toks, op))
            if not cands:
                continue
            s, e, text = rng.choice(cands)
            line, col = line_col(src, s)
            cases.append(dict(kind="MUT", op=op, file=rel, line=line, col=col, a=src, b=splice(src, s, e, text), catalog=catalog))
        for kind in PRESERVING:
            cases.append(dict(kind="PRES", op=kind, file=rel, line=None, col=None, a=src, b=preserve(src, toks, kind, rng), catalog=catalog))
    meta = []
    for n, c in enumerate(cases, 1):
        cid = "%04d" % n
        d = os.path.join(out, cid)
        os.makedirs(d, exist_ok=True)
        for side in ("a", "b"):
            with open(os.path.join(d, side + ".sql"), "w") as fh:
                fh.write(c.pop(side))
        c.update(id=cid, key="%s:%s:%s:%s" % (c["file"], c["op"], c["line"], c["col"]))
        meta.append(c)
    json.dump(dict(set=set_name, dialect=dialect, seed=seed, cases=meta), open(os.path.join(out, "cases.json"), "w"), indent=1)
    print("%s: %d files, %d mutants, %d preserving edits" % (set_name, min(used, nfiles), sum(c["kind"] == "MUT" for c in meta), sum(c["kind"] == "PRES" for c in meta)))


# ---------------------------------------------------------------- run

def span(loc):
    return [loc["start_line"], loc["end_line"]] if loc else None


def run(out, results, binary, jobs, timeout):
    spec = json.load(open(os.path.join(out, "cases.json")))
    dialect = spec["dialect"]

    def one(c):
        d = os.path.join(out, c["id"])
        cmd = [binary, "diff", os.path.join(d, "a.sql"), os.path.join(d, "b.sql"), "-d", dialect, "-f", "json", "--no-cache"]
        if c.get("catalog"):
            cmd += ["--catalog", os.path.join(out, c["catalog"])]
        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
            j = json.loads(r.stdout)
            verdict = j["verdict"]
            findings = [dict(rule=f["rule"], verdict=f["verdict"], witness=f.get("witness", "")[:300],
                             a=span(f.get("a")), b=span(f.get("b"))) for f in j["findings"]]
        except subprocess.TimeoutExpired:
            verdict, findings = "timeout", []
        except Exception as e:  # noqa: BLE001
            verdict, findings = "error", [dict(rule="harness", verdict="error", witness=str(e)[:200])]
        return dict(c, verdict=verdict, findings=findings)

    with ThreadPoolExecutor(jobs) as ex:
        res = list(ex.map(one, spec["cases"]))
    json.dump(dict(spec, cases=res), open(results, "w"), indent=1)


# ---------------------------------------------------------------- gate

def covers(c):
    for f in c["findings"]:
        # Identical statements nearby may be the ones the diff pairs as removed.
        if c["op"] == "drop_stmt" and f["rule"] == "stmt.removed":
            return True
        for side in ("a", "b"):
            if f.get(side) and f[side][0] <= c["line"] <= f[side][1]:
                return True
        if not f.get("a") and not f.get("b"):
            return True
    return False


def gate(paths, minimum):
    eq = json.load(open(os.path.join(HERE, "equivalent.json")))
    ok = True
    for p in paths:
        r = json.load(open(p))
        allow = eq.get(r["set"], {})
        mut = [c for c in r["cases"] if c["kind"] == "MUT"]
        pres = [c for c in r["cases"] if c["kind"] == "PRES"]
        equivalent = [c for c in mut if c["key"] in allow]
        scored = [c for c in mut if c["key"] not in allow]
        diff = [c for c in scored if c["verdict"] == "different"]
        false_equal = [c for c in scored if c["verdict"].startswith("equal")]
        false_diff = [c for c in pres if c["verdict"] == "different"]
        wrong_equiv = [c for c in equivalent if c["verdict"] == "different"]
        # A mutant whose line no finding covers was judged equal statement by
        # statement, even when the file verdict is unknown for other reasons.
        unseen = [c for c in scored if not c["verdict"].startswith("equal") and not covers(c)]
        rate = len(diff) / max(1, len(scored))
        by_op = {}
        for c in scored:
            by_op.setdefault(c["op"], [0, 0])
            by_op[c["op"]][1] += 1
            by_op[c["op"]][0] += c["verdict"] == "different"
        print("== %s (%s, seed %s)" % (r["set"], r["dialect"], r["seed"]))
        print("mutants different: %d/%d = %.1f%% (%d reviewed equivalent mutants excluded)" % (len(diff), len(scored), 100 * rate, len(equivalent)))
        print("  " + ", ".join("%s %d/%d" % (k, v[0], v[1]) for k, v in sorted(by_op.items())))
        others = {}
        for c in scored:
            if c["verdict"] != "different":
                others[c["verdict"]] = others.get(c["verdict"], 0) + 1
        print("  not different: %s" % others)
        print("preserving edits different: %d/%d" % (len(false_diff), len(pres)))
        for name, bad in (("mutant reported equal", false_equal), ("preserving edit reported different", false_diff),
                          ("reviewed equivalent mutant reported different", wrong_equiv),
                          ("no finding covers the mutated line", unseen)):
            for c in bad:
                ok = False
                print("FAIL %s: %s %s (case %s)" % (name, c["key"], c["verdict"], c["id"]))
        if rate < minimum:
            ok = False
            print("FAIL %s: %.1f%% of mutants reported different, below %.0f%%" % (r["set"], 100 * rate, 100 * minimum))
    return ok


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("fetch")
    a.add_argument("ext")
    a = sub.add_parser("gen")
    a.add_argument("set")
    a.add_argument("ext")
    a.add_argument("out")
    a.add_argument("--seed", type=int)
    a.add_argument("--files", type=int)
    a = sub.add_parser("run")
    a.add_argument("out")
    a.add_argument("results")
    a.add_argument("--bin", default=os.path.join(HERE, "..", "..", "target", "release", "sqldiff"))
    a.add_argument("--jobs", type=int, default=os.cpu_count() or 2)
    a.add_argument("--timeout", type=int, default=300)
    a = sub.add_parser("gate")
    a.add_argument("results", nargs="+")
    a.add_argument("--min", type=float, default=0.80)
    args = ap.parse_args()
    if args.cmd == "fetch":
        fetch(args.ext)
    elif args.cmd == "gen":
        gen(args.set, args.ext, args.out, args.seed, args.files)
    elif args.cmd == "run":
        run(args.out, args.results, args.bin, args.jobs, args.timeout)
    elif args.cmd == "gate":
        sys.exit(0 if gate(args.results, args.min) else 1)


if __name__ == "__main__":
    main()
