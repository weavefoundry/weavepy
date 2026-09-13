# Compact canonical-string intern pools

The overall performance goal remains unachieved.

Candidate a1e87c1a replaces two private String-keyed maps with sets of the
existing canonical Rc<str> objects. It removes duplicate key text and copies
on intern/status lookups. The per-thread sys pool and separate global stable-
name pool retain their boundaries, strong ownership, and object identities.
Pointer-sensitive marshal status, single-character construction, and existing
WStr behavior remain intact. No dependency, unsafe code, public API, object
representation, new cache, or pool-lifetime change is introduced.

The binary is 44,289,520 bytes, 1,280 fewer than immediate predecessor
5b299d93. The candidate includes preceding experimental JIT/call changes and
their recorded regressions. Comparisons with 5b299d93 isolate the intern work.

All 342 VM tests, formatting, Clippy, no-default-feature check, 66 targeted
release checks, 275 standard compatibility checks, and six extra CPython
sys/marshal/inspection/pathlib/copy/copyreg suites pass. The new identity test
and all nine workload oracles first passed on CPython and unchanged WeavePy
in JIT, interpreter-only, and gil=0 correctness modes. Runtime snapshots
identify all sources and binaries. GIL-disabled correctness isn't a native
parallel-performance measurement.

The nine focused controls use seven paired samples of both modes of the
candidate, immediate predecessor, retained 539c, and CPython. Warm cases
exercise lookups and marshal status; first-invocation cases grow fresh pools.
All values and per-binary frozen-cache checks pass. Lower ratios are better.

| Case | Mode | Time / predecessor | RSS / predecessor | Time / CPython | RSS / CPython |
| --- | --- | ---: | ---: | ---: | ---: |
| warm/intern_hits_16 | jit | 0.9395 | 0.9892 | 6.0153 | 1.9183 |
| warm/intern_hits_16 | interp | 0.9382 | 0.9808 | 6.1353 | 1.7741 |
| warm/intern_hits_128 | jit | 0.9411 | 0.9925 | 6.0998 | 1.9208 |
| warm/intern_hits_128 | interp | 0.9442 | 0.9821 | 6.0914 | 1.7714 |
| warm/intern_hits_1024 | jit | 0.9360 | 0.9806 | 6.8476 | 1.8844 |
| warm/intern_hits_1024 | interp | 0.9355 | 0.9743 | 6.8577 | 1.7503 |
| warm/marshal_pooled | jit | 0.9598 | 0.9887 | 3.7883 | 1.9185 |
| warm/marshal_pooled | interp | 0.9560 | 0.9867 | 3.7978 | 1.7795 |
| warm/marshal_plain | jit | 0.9711 | 0.9882 | 3.6944 | 1.9106 |
| warm/marshal_plain | interp | 0.9683 | 0.9838 | 3.7760 | 1.7661 |
| warm/type_name_hits | jit | 1.0013 | 0.9925 | 9.2156 | 1.8775 |
| warm/type_name_hits | interp | 0.9935 | 0.9834 | 9.2999 | 1.7351 |
| cold/intern_growth_short | jit | 1.0138 | 0.9624 | 5.6350 | 1.8424 |
| cold/intern_growth_short | interp | 0.9908 | 0.9562 | 5.6643 | 1.7133 |
| cold/intern_growth_long | jit | 0.9400 | 0.7527 | 3.3278 | 1.4829 |
| cold/intern_growth_long | interp | 0.9367 | 0.7423 | 3.3683 | 1.4145 |
| cold/intern_growth_unicode | jit | 0.9758 | 0.8673 | 4.7117 | 1.7760 |
| cold/intern_growth_unicode | interp | 0.9810 | 0.8592 | 4.6999 | 1.6669 |

JIT intern-hit times improve about 6 percent, with all seven pairs faster in
each size. Pooled/plain marshal times improve about 4/3 percent, also with
all pairs faster. Long-string growth uses about 25 percent less peak RSS and
Unicode growth about 13 percent less; every focused JIT RSS pair improves.
Short-string growth JIT time regresses about 1.4 percent, with six of seven
pairs slower. Type-name access has a 0.13 percent slower JIT median. These
regressions remain in the evidence; no focused CPython win is claimed.

The full suite retains all 24 unchanged fixtures with five pairs and nine
startup/import controls with 31 pairs. Its baseline is the immediate
predecessor, isolating this memory change. The startup helper disables JIT.

The 23-workload JIT mean is 1.006190 of its predecessor and
3.401746 of CPython, with 6 of 23 time wins.
The historical 21-fixture cohort is 2.102277 of CPython,
or 2.103794 using ratios of medians.

All-24 process wall/CPU/RSS ratios to its predecessor are 1.010566,
1.009626, and 0.992675.
Peak RSS is 2.059444 of CPython, with 0 wins.

A separate startup snapshot shows 29,638,656 bytes RSS for the predecessor
and 29,229,056 for the candidate, a 409,600-byte reduction. The malloc zone
reports 6344K versus 6176K allocated and 2680K versus 2416K fragmentation.
These are point-in-time region diagnostics, not peak-RSS census samples.
Readiness values agree; seeded caches stay unchanged; owned children were
terminated after inspection. An untimed datetime coverage diagnostic also
matches CPython, but its proposed assertion-exit JIT change is unapplied
and isn't evidence for this candidate.

Both timing gates qualified; every later sample and load/VM/swap observation
is retained. The reports include all process wall, CPU, workload CPU where
available, peak RSS, paired ranges, and regressions. Keep the 21/23/24 cohorts
distinct and use same-run paired ratios for change attribution. No universal,
energy, controlled build-time, portability, or free-threaded superiority is
established. The observed release build duration isn't a controlled metric.

Large raw JSON/log artifacts are compressed losslessly. The index records
both stored and uncompressed SHA-256 hashes and sizes. No measurement sample
is removed. Executables, frozen caches, and unselected temporary files stay
local and outside Git. Summaries, protocols, and code remain directly readable.
