# Soufflé benchmark of SQLDiff's Datalog-shaped code (2026-11-09)

Davy's question: write the Datalog-shaped parts in Soufflé and compare, to see whether our own implementation is up to par with an industry-proven engine. This folder is a benchmark only. Nothing here is in a shipped build: the Rust fact dumps compile only with `RUSTFLAGS="--cfg datalog_bench"`, and Soufflé is not a dependency of any crate.

Base: main 3df9fc6. Soufflé 2.4.1, built from source (64-bit domain). Machine: 4 cores, 15 GB.

## Verdict

**Our code is up to par: on the edit checker it is much faster than Soufflé, and it gives the same answers.** Soufflé's recursive evaluation is good, about as fast as our sweeps on the closure itself. It loses everywhere else because the queries around the closure are interval and prefix questions ("units that end before this edit", "units overlapping this span", "first changed unit after here"). Datalog can only state those as joins over every pair, while the Rust code answers them with sorted arrays and a segment tree. Do not adopt Soufflé.

The benchmark did find two places where our Rust is clearly slower than it needs to be. Neither is a Datalog question, and both have exact fixes (same output):

1. **`Routine::deciding_conditions` (`crates/sqldiff-equiv/src/equiv/edit.rs`) is quadratic.** For every edit it walks every earlier sibling of every ancestor. It takes 98% of the edit sweeps' time on Ola (177 of 180 ms) and 2.26 of 3.03 s on a 20,000-statement procedure, while the closure work it feeds takes 3 ms and 0.77 s. Answering it per parent once, or with a prefix of "may jump" siblings, would remove most of the sweeps' cost.
2. **`callees::resolve` (`crates/sqldiff-convert/src/callees.rs`) re-runs whole rounds.** On a 6,001-file tree it takes 0.27–0.28 s, about 11 full rounds of rescanning unit text, while Soufflé's stratified evaluation of the same rules takes 0.20 s including process start and loading the facts. Going once in callee-first order (cycles are already refused), with `calling_units` computed once, would bring it to about one pass (0.026 s). In absolute terms this is small: 2% of a 13.7 s conversion.

There is also one hygiene finding. Rust leaves "transient" assumption notes on units that end up not proven: a caller that is visited before its callee goes bad keeps the callee's assumption notes from the earlier round. This affects 4,381 of 28,002 units on the 6,001-file tree, always on units that end up not proven. Statuses are unaffected, and the conversion report shows only the top-status rules, so I could not see it in any output (a three-file check of report, markdown and queue differed only in sort order). The one-pass fix above would remove it as well.

## What was written in Soufflé

| Rust | Soufflé | Notes |
|---|---|---|
| `TaintSweep` + `ReachSweep` (`edit.rs`): for each edit, does a changed unit before it reach what it reads, and does what it writes reach changed code after it | `dl/naive.dl`: the textbook version, one closure per edit | |
| same | `dl/shared.dl`: closures shared across edits, like the Rust sweeps | A unit fires on any one tainted read, so a derivation is a path. Each name keeps only its best "time" (subsumption, `<=` rules): the smallest largest-unit-end for taint, the largest smallest-unit-start for reach. An edit then reads its answer with one comparison. |
| `callees::resolve` | `dl/callees.dl` | Stratified: failed calls (recursive through "bad" files), then holding calls carry assumption notes transitively, then the status lattice with `max`. |
| cycle check in `callees::resolve` | `dl/cyccheck.dl` | Transitive closure of the call graph, checked against Rust's per-file DFS. |

Both edit programs answer exactly what the Rust sweeps answer per edit: first change, first jump, the hit name (or "any"), and the reach answer when Rust asked for one.

## Agreement

| Input | Edits compared | Reach answers | Mismatches |
|---|---|---|---|
| Ola (7 files, every 7th integer literal changed) | 436 | 20 | 0 |
| WWI SSDT (147 routine files, same mutation) | 190 | 122 | 0 |
| synthetic `t1` (2,000 statements, every 5th edited) | 800 | 4 | 0 |
| synthetic `sparse20k` (20,000 statements, every 200th) | 200 | 4 | 0 |
| synthetic `r20k` (20,000 statements, every 5th) | 7,998 | 4 | 0 |
| **Total** | **9,624** | **154** | **0** |

Callee statuses: 1,122 / 5,602 / 28,002 units on 241 / 1,201 / 6,001-file trees, 0 status mismatches and 0 cycle mismatches. The note-set differences are the transient notes described above, and they are all one way: Rust has extra notes, Soufflé never does.

Soufflé's output was byte-identical across interpreted, compiled, `-j4` and repeated runs.

## Timings

Rust times are measured inside the process, around the sweeps (`SideEdits::new`) or the propagation loop. "Closure only" subtracts the time of the `deciding_conditions` calls, which the Soufflé side receives precomputed as facts. Soufflé times are the whole process, including start-up, loading facts (at most 0.01 s) and writing output. Inputs are dumped once and the same facts go to both.

### Edit checker sweeps

| Input | Units / edits | Rust total | Rust closure only | Soufflé compiled (best version) | Soufflé interpreted |
|---|---|---|---|---|---|
| WWI | 2,874 / 190 (112 calls) | 2.5 ms | 2.2 ms | 0.06 s (naive) | 0.09 s |
| Ola | 7,220 / 436 | 180 ms | 2.8 ms | 0.49 s | 0.65 s |
| Ola, largest procedure alone | 1,809 / 122 | 66 ms | ~1 ms | 0.05 s (shared) | – |
| t1 | 4,106 / 800 | 22.5 ms | 2.1 ms | 0.30 s (shared) | 0.55 s |
| sparse20k | 40,106 / 200 | 141 ms | 50 ms | 1.22 s (shared) | 1.38 s |
| r20k | 40,106 / 7,998 | 3.03 s | 0.77 s | 82 s, 3.3 GB (shared) | ~94 s |

Running Soufflé with `-j4` gave no gain (within 0.03 s everywhere). The naive version cannot run r20k at all, because one closure per edit means 8,000 × 40,000 offered-unit tuples.

Where Soufflé's time goes on r20k, from its profiler: the "first change / first jump before each edit" aggregates take 85 s (68 M tuples), the "changed" and "needed" interval joins take 6.7 s, and the actual recursive closure (`tt`, `tf`, `rr`, `rf`) takes about 0.4 s. The recursion is fine. The pairwise interval work is what costs.

### Callee propagation

| Files | Rust loop | One pass of `calling_units` | Soufflé compiled | Soufflé interpreted | Soufflé cycle check |
|---|---|---|---|---|---|
| 241 | 6.3 ms | – | <0.01 s | 0.03 s | <0.01 s |
| 1,201 | 42–45 ms | 5 ms | 0.03 s | 0.09 s | 0.03 s |
| 6,001 | 267–281 ms | 26 ms | 0.20 s | – | 0.48 s |

Rust's cycle check runs before its loop and is not in the "Rust loop" time. Soufflé's check is a full transitive closure, so it is quadratic in the number of files and slower than the per-file DFS.

## Caveats

- These are single runs. The Rust numbers are milliseconds and noisy at the low end, but every gap that matters for the verdict is 10x or more.
- Soufflé gets some help for free: unit facts, `deciding_conditions` spans and the text-based calling units come from Rust, because building them is parsing, not Datalog.
- I tuned the join order of the reach rules by hand and restricted reach to the edits Rust asks about. Before that, Soufflé was 3–10x slower again. A Soufflé expert might do better with pre-sorted, ranked edits that turn the prefix aggregates into a chain, but that moves sorting out of Datalog.
- The `shared.dl` correctness rests on a path argument (one tainted read fires a unit). It matched on every input, but it is not proven.

## Reproducing

```sh
# Soufflé 2.4.1: git clone --branch 2.4.1 https://github.com/souffle-lang/souffle; cmake -DSOUFFLE_DOMAIN_64BIT=ON; make install
RUSTFLAGS="--cfg datalog_bench" cargo build --release -p sqldiff-cli
rm -rf ~/.local/share/sqldiff/cache            # the report cache skips the sweeps on a repeat diff
SQLDIFF_DATALOG_DUMP=dump/t1 sqldiff diff a.sql b.sql --dialect tsql   # writes *.facts and rust_*.tsv
(cd dl && souffle -o ../bin/shared shared.dl)  # likewise naive, loadonly, callees, cyccheck
SOUFFLE=souffle ./run.sh dump/t1 t1            # runs both versions, compares with Rust, prints timings
python3 gen/gen_tree.py 6000 tree && SQLDIFF_DATALOG_DUMP=d sqldiff convert --fresh --out o --to postgres tree
bin/callees -F d/callees -D out && python3 compare_callees.py d/callees out
```

`gen/chain.py` ports the loop-chain bench generator (`perf-plan/measurements/2026-11-07/loop-chain-bench.rs`), `gen/mutate.py` changes every Nth integer literal of real code, and `diffset.sh` holds the shell helpers used for Ola and WWI.
