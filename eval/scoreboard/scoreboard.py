#!/usr/bin/env python3
"""Goal scoreboard: one JSON and one table per commit (plan/goals-and-plan-2026-11-12.md, item 1).

    scoreboard.py run --bin BIN --work DIR --out DIR [--commit SHA] [--baseline FILE]
    scoreboard.py table RESULT.json            print the markdown table of a result

`run` fetches the pinned corpora (sources.json), converts the 14 benchmark runs
(WideWorldImporters OLTP/DW/samples, AdventureWorks OLTP/DW, Ola Hallengren,
Sakila, each to PostgreSQL and Db2 for i, `convert --isolated`), runs the
cross-dialect mutation check (eval/cross-dialect-witness/xwmut.py) on the
converted WWI OLTP trees, and writes scoreboard.json and scoreboard.md to --out.

Everything except wall times is a function of the pinned inputs and the
binary. Counting: a unit is clean when its status is `converted` or
`under assumptions`; units of a SQL Server-only feature marked `impossible`
are left out of both counts (decision D1). A file is clean when its own
status is converted or under assumptions. Reasons are the engine's own rule id
(`rules[0]`) of every unit that is not clean, never a regex over text.

Exit status 1 when a mutant of a cross-dialect pair came back equal (a false
equal), when a meaning-preserving edit got a witness, or when clean units of
a run fall below the baseline (baseline.json) without a note there; lowering
the baseline is the PR note and must say why.
"""
import argparse, collections, concurrent.futures, glob, json, os, shutil, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
CLEAN = ("converted", "assumptions")
CORPORA = ["wwi-oltp", "wwi-dw", "wwi-samples", "ola", "sakila", "aw-oltp", "aw-dw"]
TARGETS = ["postgres", "db2i"]


def sh(args, **kw):
    return subprocess.run(args, check=True, **kw)


def fetch(ext):
    os.makedirs(ext, exist_ok=True)
    for name, r in json.load(open(os.path.join(HERE, "sources.json")))["repos"].items():
        d = os.path.join(ext, name)
        if not os.path.isdir(os.path.join(d, ".git")):
            sh(["git", "init", "-q", d])
            sh(["git", "-C", d, "remote", "add", "origin", r["url"]])
        elif subprocess.run(["git", "-C", d, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip() == r["sha"]:
            continue
        if r.get("sparse"):
            sh(["git", "-C", d, "sparse-checkout", "set", "--no-cone", *r["sparse"]])
        sh(["git", "-C", d, "fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", r["sha"]])
        sh(["git", "-C", d, "checkout", "-q", "FETCH_HEAD"])


def prepare(ext, work):
    """The seven source trees, as in the burndown (unconverted-burndown/scripts/run.sh)."""
    src = os.path.join(work, "src")
    wwi = os.path.join(ext, "sql-server-samples/samples/databases/wide-world-importers")
    aw = os.path.join(ext, "sql-server-samples/samples/databases/adventure-works")
    ola = os.path.join(ext, "sql-server-maintenance-solution")
    sak = os.path.join(ext, "sakila/sql-server-sakila-db")
    for c in CORPORA:
        os.makedirs(os.path.join(src, c), exist_ok=True)
    for c, d in (("wwi-oltp", "wwi-ssdt"), ("wwi-dw", "wwi-dw-ssdt"), ("wwi-samples", "sample-scripts")):
        link = os.path.join(src, c)
        os.rmdir(link) if os.path.isdir(link) and not os.path.islink(link) and not os.listdir(link) else None
        if not os.path.lexists(link):
            os.symlink(os.path.join(wwi, d), link)
    for f in "CommandLog CommandExecute DatabaseBackup DatabaseIntegrityCheck IndexOptimize Queue QueueDatabase".split():
        sh(["cp", os.path.join(ola, f + ".sql"), os.path.join(src, "ola")])
    for f in ("sql-server-sakila-schema.sql", "sql-server-sakila-drop-objects.sql"):
        sh(["cp", os.path.join(sak, f), os.path.join(src, "sakila")])
    # AdventureWorks install scripts run in SQLCMD mode upstream; fill the two variables, drop :setvar
    for c, db, sub, f, path in (
            ("aw-oltp", "AdventureWorks", "oltp-install-script", "instawdb.sql", "AdventureWorks"),
            ("aw-dw", "AdventureWorksDW", "data-warehouse-install-script", "instawdbdw.sql", "AdventureWorksDW")):
        t = open(os.path.join(aw, sub, f), encoding="utf-8", errors="surrogateescape", newline="").read()
        t = t.replace("$(DatabaseName)", db).replace("$(SqlSamplesSourceDataPath)", "C:\\Samples\\" + path + "\\")
        t = "".join(l for l in t.splitlines(True) if not l.lstrip().startswith(":setvar"))
        open(os.path.join(src, c, f), "w", encoding="utf-8", errors="surrogateescape", newline="").write(t)
    return src


def convert(binp, src, out, c, t, cats):
    o = os.path.join(out, f"{c}-{t}")
    cmd = [binp, "convert", os.path.join(src, c) + "/", "-o", o, "--to", t, "--isolated", "--fresh"]
    for f in cats if c == "wwi-samples" else []:
        cmd += ["--catalog", f]
    t0 = time.monotonic()
    p = subprocess.run(cmd, capture_output=True, text=True)
    wall = time.monotonic() - t0
    open(o + ".log", "w").write(p.stdout + p.stderr)
    return c, t, o, wall, p.returncode


def summarise(rep):
    st = collections.Counter(); fst = collections.Counter(); reasons = collections.Counter()
    imp = collections.Counter(); files_clean = 0
    for f in rep["files"]:
        fst[f["status"]] += 1
        files_clean += f["status"] in CLEAN
        st["converted"] += len(f.get("converted_units", []))
        for u in f["units"]:
            st[u["status"]] += 1
            if u["status"] in CLEAN:
                continue
            if u.get("impossible"):
                imp[u["impossible"]] += 1
                continue
            reasons[f'{u["status"]}|{(u["rules"] or ["?"])[0]}'] += 1
    total = sum(st.values()); impossible = sum(imp.values())
    clean = sum(st[s] for s in CLEAN)
    return dict(units=total, impossible=impossible, counted=total - impossible, clean=clean,
                clean_pct=round(100 * clean / (total - impossible), 1) if total > impossible else None,
                by_status=dict(st), files=len(rep["files"]), files_clean=files_clean, files_by_status=dict(fst),
                reasons=dict(reasons.most_common()))


def xw_rate(j):
    tot = dif = et = ed = 0
    for k, v in j["by_op"].items():
        if k.startswith("change:"):
            t = sum(v.values()); tot += t; dif += v.get("different", 0)
            if "equal_under_assumptions" in k:
                et += t; ed += v.get("different", 0)
    return dict(mutants=tot, different=dif, rate=round(100 * dif / tot, 1) if tot else None,
                equal_pair_mutants=et, equal_pair_different=ed, false_equal=j["false_equal"],
                false_witness=j["same_meaning_different"])


def run(a):
    ext = os.path.join(a.work, "ext"); out = os.path.join(a.work, "out")
    shutil.rmtree(out, ignore_errors=True)  # every run converts from scratch
    os.makedirs(out, exist_ok=True); os.makedirs(a.out, exist_ok=True)
    fetch(ext)
    src = prepare(ext, a.work)
    cats = sorted(glob.glob(os.path.join(src, "wwi-oltp/**/*.sql"), recursive=True) +
                  glob.glob(os.path.join(src, "wwi-dw/**/*.sql"), recursive=True))
    jobs = [(c, t) for c in CORPORA for t in TARGETS]
    res = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as ex:
        for c, t, o, wall, rc in ex.map(lambda j: convert(a.bin, src, out, *j, cats), jobs):
            r = os.path.join(o, "conversion-report.json")
            if rc not in (0, 1) or not os.path.exists(r):  # 1: not every file is clean
                sys.exit(f"convert {c} -> {t} failed (exit {rc}); see {o}.log")
            s = summarise(json.load(open(r))); s["wall_s"] = round(wall, 2)
            res[f"{c}-{t}"] = s
    xw = {}
    for t in TARGETS:
        p = subprocess.run([sys.executable, os.path.join(REPO, "eval/cross-dialect-witness/xwmut.py"),
                            os.path.join(src, "wwi-oltp") + "/", os.path.join(out, f"wwi-oltp-{t}"),
                            "postgres" if t == "postgres" else "db2i", a.bin], capture_output=True, text=True)
        if p.returncode:
            sys.exit(f"xwmut {t} failed: {p.stderr[-2000:]}")
        xw[t] = xw_rate(json.loads(p.stdout))
    commit = a.commit or subprocess.run(["git", "-C", REPO, "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip()
    tot = {}
    for t in TARGETS:
        rs = [v for k, v in res.items() if k.endswith("-" + t)]
        tot[t] = dict(units=sum(r["units"] for r in rs), counted=sum(r["counted"] for r in rs),
                      impossible=sum(r["impossible"] for r in rs), clean=sum(r["clean"] for r in rs),
                      files=sum(r["files"] for r in rs), files_clean=sum(r["files_clean"] for r in rs),
                      wall_s=round(sum(r["wall_s"] for r in rs), 2))
        tot[t]["clean_pct"] = round(100 * tot[t]["clean"] / tot[t]["counted"], 1)
        rc = collections.Counter()
        for r in rs:
            rc.update(r["reasons"])
        tot[t]["top_reasons"] = dict(rc.most_common(15))
    doc = dict(commit=commit, totals=tot, cross_dialect=xw, runs=res)
    json.dump(doc, open(os.path.join(a.out, "scoreboard.json"), "w"), indent=1, sort_keys=True)
    json.dump(dict(note="Copy to eval/scoreboard/baseline.json and say in the PR why a number fell or rose.",
                   clean_units={k: v["clean"] for k, v in res.items()},
                   cross_dialect_different={t: x["different"] for t, x in xw.items()}),
              open(os.path.join(a.out, "baseline.candidate.json"), "w"), indent=1, sort_keys=True)
    md = table(doc)
    problems = gate(doc, a.baseline)
    if problems:
        md += "\n### Gate failures\n\n" + "\n".join("- " + p for p in problems) + "\n"
    open(os.path.join(a.out, "scoreboard.md"), "w").write(md)
    print(md)
    return 1 if problems else 0


def gate(doc, baseline):
    bad = []
    for t, x in doc["cross_dialect"].items():
        if x["false_equal"]:
            bad.append(f"false equal ({t}): {x['false_equal'][:5]}")
        if x["false_witness"]:
            bad.append(f"witness on a meaning-preserving edit ({t}): {len(x['false_witness'])}")
    if baseline and os.path.exists(baseline):
        b = json.load(open(baseline))
        for k, v in b["clean_units"].items():
            cur = doc["runs"].get(k, {}).get("clean")
            if cur is None:
                bad.append(f"{k}: run missing")
            elif cur < v:
                bad.append(f"{k}: clean units fell from {v} to {cur}; lower eval/scoreboard/baseline.json with a note saying why")
        for t, v in b.get("cross_dialect_different", {}).items():
            cur = doc["cross_dialect"][t]["different"]
            if cur < v:
                bad.append(f"cross-dialect {t}: different mutants fell from {v} to {cur}")
    return bad


def table(d):
    L = [f"## Scoreboard for `{d['commit'][:9]}`", "",
         "| Target | Units clean | Files clean | Wall (s, 2 runs at once) |", "|---|---|---|---|"]
    for t, v in d["totals"].items():
        L.append(f"| {t} | **{v['clean_pct']}%** ({v['clean']:,} / {v['counted']:,}; {v['impossible']:,} impossible left out) "
                 f"| {v['files_clean']:,} / {v['files']:,} | {v['wall_s']} |")
    L += ["", "### Cross-dialect drift (WWI OLTP, behaviour-changing WHERE mutants)", "",
          "| Target | Different | Of equal pairs | False equal | False witness |", "|---|---|---|---|---|"]
    for t, x in d["cross_dialect"].items():
        L.append(f"| {t} | **{x['rate']}%** ({x['different']} / {x['mutants']}) | {x['equal_pair_different']} / "
                 f"{x['equal_pair_mutants']} | {len(x['false_equal'])} | {len(x['false_witness'])} |")
    L += ["", "### Runs", "", "| Run | Units clean | Files clean | Converted | Assumptions | Not proven | Differs | Not converted | Decision | Wall (s) |",
          "|---|---|---|---|---|---|---|---|---|---|"]
    for k, r in d["runs"].items():
        s = r["by_status"]
        L.append(f"| {k} | {r['clean']:,} / {r['counted']:,} ({r['clean_pct']}%) | {r['files_clean']} / {r['files']} | "
                 f"{s.get('converted', 0)} | {s.get('assumptions', 0)} | {s.get('unproven', 0)} | {s.get('differs', 0)} | "
                 f"{s.get('not_converted', 0)} | {s.get('decision', 0)} | {r['wall_s']} |")
    for t, v in d["totals"].items():
        L += ["", f"### Top blockers, {t} (status, engine rule id)", "", "| Units | Status | Rule |", "|---|---|---|"]
        for k, n in list(v["top_reasons"].items())[:10]:
            s, r = k.split("|", 1)
            L.append(f"| {n:,} | {s} | `{r}` |")
    return "\n".join(L) + "\n"


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    sp = ap.add_subparsers(dest="cmd", required=True)
    r = sp.add_parser("run")
    r.add_argument("--bin", required=True); r.add_argument("--work", required=True); r.add_argument("--out", required=True)
    r.add_argument("--commit"); r.add_argument("--baseline", default=os.path.join(HERE, "baseline.json"))
    t = sp.add_parser("table"); t.add_argument("result")
    a = ap.parse_args()
    if a.cmd == "run":
        sys.exit(run(a))
    print(table(json.load(open(a.result))))
