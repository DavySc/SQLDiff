"""Fails unless every theorem the gate relies on uses only allowed axioms.

Usage: lake env lean Axioms.lean | python3 check_axioms.py
Lean's three standard axioms are allowed; vendor-fact axioms join the list
only through a signed SPEC.lock (decision 5), not by editing this file.
"""

import re
import sys

ALLOWED = {"propext", "Classical.choice", "Quot.sound"}

text = sys.stdin.read()
print(text, end="")
seen = 0
for m in re.finditer(r"'([^']+)' (does not depend on any axioms|depends on axioms: \[([^\]]*)\])", text):
    seen += 1
    used = {a.strip() for a in (m.group(3) or "").split(",") if a.strip()}
    extra = used - ALLOWED
    if extra:
        sys.exit(f"{m.group(1)} uses axioms outside the allowlist: {sorted(extra)}")
    if "sorryAx" in used:
        sys.exit(f"{m.group(1)} uses sorry")
if seen == 0:
    sys.exit("no #print axioms output found")
print(f"axioms ok ({seen} theorem(s))")
