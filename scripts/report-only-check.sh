#!/usr/bin/env bash
# Report-only check (perf plan W6c step 1): a branch that only adds a report
# must leave every status and every converted file byte-identical, whatever
# the thread count. Usage:
#   scripts/report-only-check.sh BASE_SQLDIFF NEW_SQLDIFF WORKDIR NAME=SRC...
# Converts each SRC to PostgreSQL with the base binary (-j 4) and with the new
# binary (-j 1 and -j 4), and compares the output trees and the conversion
# reports (seconds and reuse counters left out). Exit 1 on any difference.
set -u
base=$1; new=$2; work=$3; shift 3
bad=0
mkdir -p "$work"
for c in "$@"; do
  name=${c%%=*}; src=${c#*=}
  "$base" convert "$src" -o "$work/base-$name" --to postgres -j 4 >"$work/base-$name.log" 2>&1
  for j in 1 4; do
    "$new" convert "$src" -o "$work/new$j-$name" --to postgres -j "$j" >"$work/new$j-$name.log" 2>&1
    if ! python3 - "$work/base-$name" "$work/new$j-$name" <<'PY'
import json, os, sys
a, b = sys.argv[1:3]
def files(d):
    out = {}
    for dp, ds, fs in os.walk(d):
        ds[:] = [x for x in ds if x != ".sqldiff"]
        for f in fs:
            p = os.path.join(dp, f)
            if f.endswith(".sql"):
                out[os.path.relpath(p, d)] = open(p, "rb").read()
    return out
def norm(d):
    def walk(x):
        if isinstance(x, dict):
            return {k: walk(v) for k, v in x.items() if k not in ("seconds", "reused", "elapsed", "run")}
        if isinstance(x, list):
            return [walk(v) for v in x]
        if isinstance(x, str):
            return x.replace(d, "OUT")
        return x
    return walk(json.load(open(os.path.join(d, "conversion-report.json"))))
fa, fb = files(a), files(b)
ok = fa == fb and norm(a) == norm(b)
print("identical" if ok else "DIFFERENT", len(fa), "files")
sys.exit(0 if ok else 1)
PY
    then bad=1; fi
  done
done
exit $bad
