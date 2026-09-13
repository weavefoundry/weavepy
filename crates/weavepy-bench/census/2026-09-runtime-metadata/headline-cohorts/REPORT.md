# Reconciling the geometric means

The historical 2.91x headline and the subsequent 3.45x headline use different
benchmark sets. Both numbers express WeavePy time divided by CPython time;
lower is better.

| Timing cohort | Historical result | Latest checkpoint measurement |
|---|---:|---:|
| Original 21 fixtures, including startup | 2.91051x | 2.12062x |
| Original 20 workload timers, excluding startup | Not recomputed here | 2.16575x |
| Complete 23 workload timers, excluding startup | Not available in that recording | 3.45420x |

The historical recording belongs to commit
`14185816d284891a26c39758dc91e6ad27ca186d`. Its 21 fixture sources and work
parameters are byte-for-byte unchanged in the latest measurement. The latest
recording uses binary `8899e367`, corresponding to the runtime in checkpoint
`566c7bf2`. See [summary.json](summary.json) for full identities, input hashes,
per-row comparisons, and all four cohort definitions.

The original headline excludes `deque_ops`, `datetime_ops`, and `pickle_bench`.
The later Rust harness continued excluding those accelerator rows from its
headline, even after measuring them individually. This full census includes
them and reports startup separately. Their latest paired workload ratios are
17.20x, 121.91x, and 219.37x, respectively. Those gaps explain why the expanded
headline is larger. They remain part of the performance objective.

For the original 21 rows, use the historical ratio-of-medians calculation:
take the median WeavePy time divided by the median CPython time for each row,
then their geometric mean. The newer census instead preserves interleaved
pairing by taking each row's median of paired ratios. Using that newer
calculation gives 2.12227x on the original 21 rows, which also rounds to 2.12x.
Using ratio-of-medians on all 23 workload rows gives 3.45094x, also rounding to
3.45x. The aggregation formula's difference is small here; the cohort change
is the substantial distinction.

This is a reconstruction from existing measurements, not a fresh controlled
comparison of the historical binary. Host load, sampling, and cache protocols
differ between the recordings. The lower comparable headline does not prove
that every workload improved; individual regressions remain visible. Future
reports should identify the fixture count, inclusion of startup and accelerator
rows, execution mode, and aggregation formula alongside every geometric mean.

The original [baseline JSON](historical-baseline.json), the
[reconciliation script](reconcile.py), and the immutable
[latest full measurement](../weakref-shared-keys-followup/suite.json) preserve
the inputs to this explanation.
