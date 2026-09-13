# Explicit raise exits and pickle coverage

The corrected candidate is 539c6dd3, built from checkpoint 566c7bf plus the
preserved source delta and untracked regression file. The comparison binary
8899e367 is the checkpoint runtime. The binary grew by 224 bytes to 44,290,560.
The runtime goal remains unachieved.

The JIT now compiles normal paths in functions containing explicit raises.
Exception exits preserve operands, locals, and the exact instruction address
for the interpreter. An initial prototype incorrectly restored a call marker
from the other branch of a conditional constructor argument. It failed nine
I/O checks; retain that prototype's delta, environment, validation, and
failure diagnostic. The corrected analyzer excludes proven marker-free raise
exits from call spans and removes duplicate spans. A separate exception fix
clears the reused exception instance's __cause__ slot for `raise from None`.

The corrected build passed 56 JIT tests, 342 VM tests, formatting, Clippy,
a build without default features, 57 targeted checks, and all 275 compatibility
checks. Twelve focused inputs passed 72 mode/oracle checks. Four pickle
components matched CPython's bytes and reconstructed values. Record decoding
additionally compiled `read`; the main pickle drivers still encountered other
unsupported instructions. Source snapshots establish identity, not validation.

Both strict load gates expired without starting measurements. Subsequent
focused and full results are separate busy-host diagnostics, with all raw
samples and load observations retained. The first focused diagnostic launcher
failed before any benchmark sample because of an empty Bash array; its failure
is preserved. No sample was filtered or retried. Small timing changes remain
provisional. These measurements do not replace the earlier quiet-host headline.

The focused guarded integer, float, and list loops took about 1/61, 1/38, and
1/12 of checkpoint workload time. Those three workload timers beat CPython,
but peak RSS stayed near twice CPython. Guarded calls improved about 2.9-fold
but still took 2.9 times CPython workload time. Warm always-raising calls
regressed 9.3%, and cold always-raising calls regressed 6.3%; all seven paired
workload ratios were worse. Ordinary controls and half-raising cases include
mixed results and outliers. `summary.json` retains every comparison and range.

The full diagnostic contains 24 unchanged standard fixtures, five paired
samples each, plus nine startup/import cases with 31 pairs. Startup/import
helper measurements use JIT disabled. The full summary keeps the historical
21-row timing cohort separate from the complete 23-workload timing cohort;
all 24 process wall, CPU, and RSS rows remain present. See `full-diagnostic`
for exact cohort results, sample counts, row values, and frozen-cache checks.

Nested native calls do not apply the ordinary frame's deopt retirement budget.
Coverage recorded 5,374 native-call exits in the always-raising case versus
ten in the checkpoint. A simple permanent retirement was left unapplied
because an early error burst could disable profitable later normal calls.
Native CPU samples for pickle mainly showed interpreter dispatch and object
allocation/cloning; they do not identify a particular Python helper's cost or
establish a performance gain. Raw profiles remain in local target archives.

This is evidence for these workloads on the local host, not universal
superiority. CPython still wins many time and memory comparisons. Energy,
controlled build latency, portability, and free-threaded performance are not
established. The index identifies every selected artifact by checksum; full
local caches, executables, and raw sampling captures remain outside Git.
