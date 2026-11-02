#!/bin/bash
# usage: chk.sh seed n
cd /tmp/claude-0/w
python3 ${FUZZ:-polyfuzz.py} $1 $2 > pf$1.txt
(cd /home/claude/SQLDiff && SQLDIFF_WKB_CASES=/tmp/claude-0/w/pf$1.txt SQLDIFF_WKB_OUT=/tmp/claude-0/w/pf$1.rs CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 TMPDIR=/tmp/claude-0/tt cargo test -p sqldiff-core --lib wkb_decisions -- --ignored >/dev/null 2>&1)
python3 regions.py pf$1.txt pf$1.reg
python3 - $1 <<'P'
import sys,collections
s=sys.argv[1]
rs=dict(l.split()[:2] for l in open(f'/tmp/claude-0/w/pf{s}.rs'))
rg=dict(l.split()[:2] for l in open(f'/tmp/claude-0/w/pf{s}.reg'))
c=collections.Counter((rs[k],rg.get(k)) for k in rs)
print(dict(c))
print('FALSE EQUAL:',[k for k in rs if rs[k]=='accept' and rg.get(k)!='same'][:10])
P
