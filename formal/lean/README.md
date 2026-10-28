# Lean checker (`sqldiff-check`)

The Lean side of [the verification roadmap](../../docs/verification-roadmap.md).
For now only the decision 2 packaging and performance spike: `Main.lean`
answers every request with "unconfirmed", and
`.github/workflows/formal-lean.yml` builds it for every release target and
runs `measure.py` (start-up, throughput, size, linked libraries).

The toolchain is pinned in `lean-toolchain`. Executable code imports only
`Init`; proofs and the specification will live in separate modules.
