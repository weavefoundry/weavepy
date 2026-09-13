# Guarded pickle hook lookup

Decision: **deferred**. The performance goal remains unachieved.

Candidate 5201c72e adds a guarded native lookup for ordinary Python pickle
hooks and calls it from copyreg. It probes live MRO dictionaries and binds
exact functions, static methods, and class methods. Other descriptors,
metaclasses, C extensions, native payloads, exotic class keys, and active
type watchers decline before user callbacks. It adds no lookup cache.

The first build used a nonexistent Object::NotImplemented variant and
failed before tests. The corrected build uses the existing singleton.
It passed 342 VM tests, formatting, Clippy, the build without default
features, all 275 standard compatibility checks, 45 targeted checks,
explicit copy/copyreg suites, and four exact pickle byte/value oracles.
Instrumented real pickle calls reached the native lookup path.

Baseline preflights preserve existing differences: CPython consults
instance lookup for __getstate__ and rejects explicit None newargs hooks;
WeavePy does not. An exotic string class key invokes equality in CPython
but not in either WeavePy binary, which emits its existing warning.
These are recorded gaps, not parity successes. Both WeavePy versions
already bind ordinary Python functions as method objects. This change
does not establish removal of a closure allocation for those functions.

The strict load gate expired after 600 seconds without starting a child.
The separately declared focused and full diagnostics retain all samples,
value checks, isolated frozen-cache checks, load, VM, and swap observations.
They do not replace the last qualified headline. Small differences remain
provisional under contention. The binary grew by 1,216 bytes.

The table reports paired medians with lower values better. CPU is the
workload CPU timer, and memory is process peak RSS. Raw paired values and
ranges for every metric and both modes are in summary.json.

| Focused case | JIT time / previous | JIT CPU / previous | JIT RSS / CPython |
| --- | ---: | ---: | ---: |
| components/payload_dumps | 1.0932 | 1.0820 | 2.2931 |
| components/payload_loads | 1.0541 | 0.9756 | 2.2760 |
| components/records_dumps | 1.3046 | 1.0552 | 2.3670 |
| components/records_loads | 0.9558 | 0.9851 | 2.3974 |
| hooks/default_state | 1.0865 | 0.9384 | 1.9433 |
| hooks/function_state | 0.7517 | 0.7486 | 1.9520 |
| hooks/inherited_newargs | 0.8952 | 0.8254 | 1.9383 |
| hooks/static_state | 0.7057 | 0.7164 | 1.9384 |
| hooks/class_state | 0.7153 | 0.7189 | 1.9528 |
| hooks/descriptor_fallback | 0.9075 | 0.9088 | 1.9937 |
| hooks/metaclass_fallback | 1.0965 | 1.0955 | 1.9529 |

Function, static-method, and class-method hook workloads improved in all
seven pairs. Metaclass fallback regressed in all seven pairs in both
wall and CPU time, about 9.6% at the JIT median. The record-serialization
control had large dispersion and a worse median; preserve that result
rather than dismissing or retrying it. No focused RSS result beat CPython.

The full diagnostic uses all 24 unchanged fixtures with five pairs and
nine startup/import cases with 31 pairs. The startup helper disables JIT.
The summary separates the historical 21-case timing cohort, 23 workload
timers, and all 24 process wall/CPU/RSS comparisons. Full row-level results
and cache identities are retained in full-diagnostic.

Seven call-shape coverage probes also matched CPython values. Sparse
keyword and variadic keyword calls account for the two generic calls
per loop in the call-overhead fixture. The outer loop and ordinary
bound-method calls already compile. Coverage is not a timing measurement.

The index identifies every selected artifact by SHA-256. Complete local
executables, frozen caches, and other raw research archives stay outside
Git. These results do not establish universal, energy, controlled build,
portability, or free-threaded superiority.
