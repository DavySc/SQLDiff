#!/bin/bash
# run.sh DUMP TAG [modes]: run both Datalog versions on a dump, compare with Rust, print timings.
S=$(dirname $(readlink -f $0)); SB=${SOUFFLE:-souffle}; d=$1; t=$2; modes=${3:-"interp compiled compiled-j4"}
for f in unit reads writes touches status fspan edit dspan; do touch $d/$f.facts; done
awk '{ns+=$2; dc+=$7; u+=$3; e+=$4} END {printf "rust sweeps: %.4f s over %d calls (closure work %.4f s after taking out deciding_conditions %.4f s), %d units, %d edits\n", ns/1e9, NR, (ns-dc)/1e9, dc/1e9, u, e}' $d/rust_time.tsv
/usr/bin/time -o /tmp/lt -f "  load facts only (compiled): %e s" $S/bin/loadonly -F $d -D - >/dev/null 2>&1; cat /tmp/lt
for v in naive shared; do for m in $modes; do
  o=$S/out/$t-$v-$m; rm -rf $o; mkdir -p $o
  case $m in interp) cmd="$SB -j1 -F $d -D $o $S/dl/$v.dl";; compiled) cmd="$S/bin/$v -j1 -F $d -D $o";; compiled-j4) cmd="$S/bin/$v -j4 -F $d -D $o";; esac
  /usr/bin/time -o $o.time -f "%e s, %M KB" timeout ${TMO:-900} $cmd >/dev/null 2>&1; r=$(tail -1 $o.time)
  printf "  %-6s %-11s %-18s %s\n" $v $m "$r" "$(python3 $S/compare.py $d $o 2>&1 | tail -1)"
done; done
