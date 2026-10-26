# Cross-dialect mutation check for expression-level verdicts.
#
# Takes a `sqldiff convert` output tree, mutates the converted side of every
# routine inside WHERE clauses (code tokens only, via eval/mutation/sqltok.py)
# and diffs the T-SQL source against each mutant.
# - Behaviour-changing mutants must never come back equal.
# - Meaning-preserving edits must never get an equiv.witness finding.
#
# Usage: xwmut.py SRC_ROOT CONV_DIR DIALECT_B SQLDIFF_BIN [PER_FILE]
import sys, os, re, json, subprocess, tempfile, collections

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "mutation"))
import sqltok  # noqa: E402

src, conv, db, S = sys.argv[1:5]
PER_FILE = int(sys.argv[5]) if len(sys.argv) > 5 else 3
rep = json.load(open(os.path.join(conv, "conversion-report.json")))["files"]


def where_regions(toks):
    """(first, last) code-token index ranges of WHERE clauses that end the
    statement at parenthesis depth 0, with the statement's first word."""
    code = [t for t in toks if t.code]
    out = []
    for i, t in enumerate(code):
        if t.kind != "word" or t.up != "WHERE":
            continue
        depth = 0
        j = i + 1
        while j < len(code):
            u = code[j]
            if u.text == "(":
                depth += 1
            elif u.text == ")":
                depth -= 1
                if depth < 0:
                    break
            elif depth == 0 and (u.text == ";" or (u.kind == "word" and u.up in (
                    "ORDER", "GROUP", "RETURNING", "OPTION", "END", "UNION", "FOR", "LIMIT",
                    "FETCH", "OFFSET", "HAVING", "INTO"))):
                break
            j += 1
        # statement verb: nearest DELETE/UPDATE/SELECT before the WHERE at depth 0
        verb = None
        d = 0
        for k in range(i - 1, -1, -1):
            if code[k].text == ")":
                d += 1
            elif code[k].text == "(":
                d -= 1
                if d < 0:
                    break
            elif d == 0 and code[k].kind == "word" and code[k].up in ("DELETE", "UPDATE", "SELECT"):
                verb = code[k].up
                break
        out.append((code, i, j, verb, depth == 0 and j < len(code) and code[j].text == ";"))
    return out


def mutants(text, dialect):
    toks = sqltok.tokenize(text, "postgres" if dialect == "postgres" else dialect)
    for code, i, j, verb, ends in where_regions(toks):
        region = code[i + 1:j]

        def edit(a, b, rep):
            return text[:a] + rep + text[b:]

        for k, t in enumerate(region):
            prev = region[k - 1] if k else None
            nxt = region[k + 1] if k + 1 < len(region) else None
            if t.kind == "op" and t.text == "=":
                yield "change", "eq-to-ne", edit(t.start, t.end, "<>")
                if prev and nxt and prev.kind in ("word", "var", "num") and nxt.kind in ("word", "var", "num") \
                        and (k + 2 >= len(region) or region[k + 2].kind == "word" and region[k + 2].up in ("AND", "OR")) \
                        and (k < 2 or region[k - 2].kind == "word" and region[k - 2].up in ("AND", "OR")):
                    yield "same", "swap-eq", text[:prev.start] + nxt.text + text[prev.end:t.start] + t.text + \
                        text[t.end:nxt.start] + prev.text + text[nxt.end:]
                    yield "same", "not-ne", text[:prev.start] + f"NOT ({prev.text} <> {nxt.text})" + text[nxt.end:]
            elif t.kind == "op" and t.text == "<>":
                yield "change", "ne-to-eq", edit(t.start, t.end, "=")
            elif t.kind == "op" and t.text == ">":
                yield "change", "gt-to-ge", edit(t.start, t.end, ">=")
            elif t.kind == "op" and t.text == "<":
                yield "change", "lt-to-le", edit(t.start, t.end, "<=")
            elif t.kind == "word" and t.up == "AND":
                yield "change", "and-to-or", edit(t.start, t.end, "OR")
            elif t.kind == "num" and t.text.isdigit():
                yield "change", "literal+1", edit(t.start, t.end, str(int(t.text) + 1))
            elif t.kind == "word" and t.up == "IS" and nxt and nxt.up == "NULL":
                yield "change", "is-not-null", edit(t.start, nxt.end, "IS NOT NULL")
        if region and ends:
            first, last = region[0], region[-1]
            yield "same", "paren-where", text[:first.start] + "(" + text[first.start:last.end] + ")" + text[last.end:]
            if verb in ("DELETE", "UPDATE"):
                w = code[i]
                yield "change", "drop-where", text[:w.start] + text[last.end:]


def diff(a, b):
    r = subprocess.run([S, "diff", a, b, "--dialect-a", "tsql", "--dialect-b", db, "-f", "json"],
                       capture_output=True, text=True)
    try:
        return json.loads(r.stdout)
    except Exception:
        return None


tmp = tempfile.mkdtemp()
res = collections.Counter()
bad_equal, bad_witness, witnessed = [], [], collections.Counter()
n = 0
for f in rep:
    p = f["path"]
    if not re.search(r"/(Stored Procedures|Functions)/", p):
        continue
    a, b = os.path.join(src, p), os.path.join(conv, p)
    base = diff(a, b)
    if base is None:
        continue
    bv = base["verdict"]
    text = open(b).read()
    per = collections.Counter()
    for group, name, new in mutants(text, db):
        if new == text or per[name] >= PER_FILE:
            continue
        per[name] += 1
        mp = os.path.join(tmp, "m.sql")
        open(mp, "w").write(new)
        j = diff(a, mp)
        if j is None:
            res[(group, name, bv, "error")] += 1
            continue
        v = j["verdict"]
        res[(group, name, bv, v)] += 1
        wit = [x for x in j["findings"] if x["rule"] == "equiv.witness"]
        if wit:
            witnessed[(group, name)] += 1
        if group == "change" and v.startswith("equal"):
            bad_equal.append((p, name))
        if group == "same" and (wit or (v == "different" and bv != "different")):
            bad_witness.append((p, name, v, wit[0]["witness"][:300] if wit else ""))
        n += 1

summary = collections.defaultdict(collections.Counter)
for (g, name, bv, v), c in res.items():
    summary[f"{g}:{name} (unmutated {bv})"][v] += c
print(json.dumps(dict(
    mutations=n,
    by_op={k: dict(v) for k, v in sorted(summary.items())},
    witnessed={f"{g}:{k}": c for (g, k), c in sorted(witnessed.items())},
    false_equal=bad_equal,
    same_meaning_different=bad_witness), indent=1))
