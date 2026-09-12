# Performance checkpoint contents

This Git checkpoint contains the implementation, regression tests, experiment reports,
selected raw timing and memory samples, validation results, and the current weakref
fixtures. It doesn't commit the entire local research directory, which contained about
20,000 files and 1.56 GB of data before this checkpoint.

Repeated source snapshots, copied standard libraries, frozen bytecode, intermediate
source drafts, bulk oracle output, disassembly, and allocation-stack logs remain on
this machine. They have not been deleted or silently replaced, but this Git commit
is not a remote backup of those omitted files. `target/` also remains local.

Historical reports describe the complete local archives and retain their original
wording, paths, and limitations. References to raw files or full archive manifests
in those reports don't imply that every referenced artifact is in Git. The exact
selected census files and hashes are listed in `checkpoint-index.json`; the local
`.gitignore` prevents an accidental bulk add of the omitted archives. Existing
immutable experiment archives have not been rewritten.

The latest implementation checkpoint is described in
[weakref-shared-keys-checkpoint/REPORT.md](weakref-shared-keys-checkpoint/REPORT.md).
Its allocation profiles, startup controls, and full-census comparison are still
pending. The preceding collector borrowed-handle change also remains separately
unmeasured. WeavePy has not achieved the objective of outperforming CPython across
every meaningful metric.

The canonical build command is `cargo build --release -p weavepy-cli`.
Use fresh per-binary frozen caches for new performance comparisons. The historical
nonisolated-cache and host-load limitations remain documented in the reports.
