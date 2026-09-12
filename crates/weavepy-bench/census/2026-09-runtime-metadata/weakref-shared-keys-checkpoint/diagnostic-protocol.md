# Shared weakref keys: background-load diagnostic

The original quiet gate expired after 600 seconds without launching a benchmark child or collecting samples. Preserve weakref-shared-keys-screen-load.json unchanged. All correctness checks and all 114 fixture preflight checks passed before this amendment.

Before any timed samples, declare a separate diagnostic with one- and five-minute load at most six for three consecutive ten-second observations, with the same 600-second deadline. The eight-CPU host remained above the original threshold of four. This amended gate does not establish a quiet host. Timing differences are provisional, and later load increases cannot be grounds for excluding or repeating samples. RSS is also an OS observation under the recorded memory conditions.

Keep the exact nineteen fixtures, seven paired cycles, alternating order, warm cycle, verification, and separate stable per-binary caches from the original screen protocol. Use fresh diagnostic output names and retain the original protocol and failed gate. Record ongoing load and raw VM/swap context before and after the run. No samples are excluded or automatically retried. The baseline is36989234372e807ab95dc79686d9e5dd3bb51b458a9f7a1d92992357c30373a0, candidate8899e3679267f571bc5d50167197c8a558e19c5f3278bf3e74df0a8607213284, and CPython3.14.7.

Use these observations with exact source and allocation evidence; do not present them as universal or definitive timing gains. A candidate retained after the diagnostic still needs startup and full-census controls. The goal of outperforming CPython on every meaningful metric remains unmet.
