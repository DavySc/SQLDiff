# Vendored from IBM/ibmi-oss-examples

Upstream: https://github.com/IBM/ibmi-oss-examples (Apache-2.0, see `LICENSE`; the upstream repo has no NOTICE file).
Commit: `37f17dc6527e1f28314e055c050bb426f5cfe0f1` (2023-01-12). Every `.sql` file in the repo at that commit is here, unmodified.

Why vendored: upstream no longer serves anonymous git fetches, so the real-code gate cannot fetch it. The gate reads these files locally and checks their sha256 (corpus/real-code/sources.json). Do not edit them; they must stay byte-identical to upstream. Files are marked `-text` in .gitattributes so a Windows checkout keeps their bytes.
