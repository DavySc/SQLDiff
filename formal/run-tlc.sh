#!/usr/bin/env bash
# Check the error/transaction model: regenerate-check the cases, then run TLC
# on both configurations.  TLA2TOOLS points at tla2tools.jar; if unset, the
# pinned release is downloaded into formal/.tools/.
set -euo pipefail
cd "$(dirname "$0")"

TLA_VERSION=1.8.0
TLA_SHA256=d4a12081bb7acd49d6d6158e818d8d69f8eb3f9a1f140cb39d20d50c4ce77f44

python3 gen_cases.py --check

jar="${TLA2TOOLS:-}"
if [[ -z "$jar" ]]; then
  jar=.tools/tla2tools-$TLA_VERSION.jar
  if [[ ! -f "$jar" ]]; then
    mkdir -p .tools
    curl -fsSL -o "$jar.tmp" \
      "https://github.com/tlaplus/tlaplus/releases/download/v$TLA_VERSION/tla2tools.jar"
    echo "$TLA_SHA256  $jar.tmp" | sha256sum -c - >/dev/null
    mv "$jar.tmp" "$jar"
  fi
fi

meta=$(mktemp -d)
trap 'rm -rf "$meta"' EXIT
for cfg in Cases Explore; do
  echo "== TLC $cfg.cfg"
  java -XX:+UseParallelGC -cp "$jar" tlc2.TLC -config "$cfg.cfg" -workers auto \
    -metadir "$meta/$cfg" ErrorTxn.tla 2>&1 \
    | grep -v '^Picked up JAVA_TOOL_OPTIONS' > "$meta/$cfg.log" || true
  if ! grep -q '^Model checking completed. No error has been found.' "$meta/$cfg.log"; then
    cat "$meta/$cfg.log"
    exit 1
  fi
  grep -E 'states generated|^Finished in' "$meta/$cfg.log" | tail -2
done
