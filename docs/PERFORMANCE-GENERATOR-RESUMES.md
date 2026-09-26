# Generator resume performance

Generator resumption now validates and takes a suspended generator frame under one mutable
state borrow. Previously, both inline `for` resumes and lean `send` resumes
borrowed state to inspect it, released that guard, and borrowed again to take
the frame. The new guard ends before frame restoration, execution, or recursion
error construction. Existing frame, observer, exception-state, and first-send
checks remain in place. No unsafe code or public state representation changes
are introduced.

Measurements compare the candidate with runtime `1d0f79d` on macOS x86_64,
using Rust 1.94 and optimized CPython 3.14.5. The benchmark harness interleaves
variants, discards warmup, and verifies separate nonempty frozen caches that
remain unchanged during timing. No builds, tests, or profiles overlap timing.
Ratios are medians of matched cycles; lower is better.

| Seven-cycle workload | JIT/base | Interpreter/base | JIT/CPython |
| --- | ---: | ---: | ---: |
| Standard generator pipeline | 0.954 | 0.947 | 2.784 |
| Pipeline at ten times the work | 0.977 | 0.968 | 2.870 |
| Plain `for` consumption | 0.962 | 0.958 | 3.205 |
| Four nested generator layers | 0.949 | 0.945 | 4.620 |
| Repeated `next` | 1.004 | 0.990 | 5.269 |
| Repeated `send` | 0.996 | 0.984 | 7.584 |
| Numeric generator `sum`, longer work | 0.995 | 1.003 | 0.904 |

The initial shorter `sum` interpreter ratio was 1.039; it does not repeat with
ten times the work. The new probe includes generator construction and closing
inside the timed workload and checks the result. Its selected operation is
recorded in benchmark metadata.

This small change is retained for the repeatable generator gain, with the
startup costs below left explicit. The unchanged full suite uses three cycles. Workload geometric means include
23 workloads; process means include all 24 fixtures, including startup.

| Metric | JIT/base | Interpreter/base | JIT/CPython |
| --- | ---: | ---: | ---: |
| Workload time | 1.001 | 1.000 | 1.071 |
| Process elapsed | 0.998 | 0.996 | 1.352 |
| Process CPU | 0.997 | 0.996 | 1.304 |
| Peak RSS | 1.003 | 1.001 | 1.584 |

Seven-cycle rechecks remove the initial 5.4% short JIT-loop slowdown and the
numeric-kernel CPU increase. Their JIT workload ratios are 0.989 and 0.958.
DeltaBlue's JIT workload ratio is 1.001, but its peak RSS ratio remains 1.032
following an initial 1.041; interpreter RSS repeats at 1.070 after 1.093.
At ten times the work, JIT/interpreter time ratios are 1.002/0.999 and RSS
ratios are 0.964/0.992. The memory increase does not persist across that longer
run, and neither result establishes a general memory gain. JSON's initial
interpreter RSS increase does not repeat. Spectral norm's interpreter process
elapsed/CPU ratios are 0.996/1.009 in the recheck.

Two independent 31-cycle startup sweeps give the following second-run ratios.
The first normal JIT elapsed/CPU ratios were 1.025/1.032.

| Launch | JIT elapsed/base | JIT CPU/base | JIT elapsed/CPython | JIT RSS/CPython |
| --- | ---: | ---: | ---: | ---: |
| Normal | 1.015 | 1.017 | 1.463 | 1.240 |
| No site | 1.027 | 1.003 | 0.843 | 0.763 |
| Isolated | 1.014 | 1.006 | 1.464 | 1.243 |
| Imports | 1.012 | 1.020 | 2.655 | 2.271 |

The 100-worker probe gives JIT workload/RSS ratios of 1.009/0.995 and an
interpreter workload ratio of 1.021. JIT work still takes 4.503 times CPython's
time. The executable size is unchanged at 51,814,592 bytes. These results do
not establish overall CPython parity or an overall memory improvement.

Validation passed 373 VM tests, 198 release regression runs in JIT,
interpreter-only, and GIL-disabled modes, and nine benchmark-tool tests.
VM Clippy passes with the existing local `let_and_return` and socket alignment
exceptions. The new semantic fixture also passes CPython and the accepted
baseline in all three WeavePy modes. It covers reentrant sends, tracing
transitions, suspended frame access, close/throw, recursion-error recovery,
and payload lifetime. Twenty-five small probe runs check results against
CPython and both binaries; their timings are excluded.

Source, executable, fixture, and tool hashes are retained with raw samples
under `target/performance/generator-state-borrow-experiment/` and
`target/performance/generator-borrow-*.json`. The release binary was built
with `cargo build --release -p weavepy-cli --bin weavepy` and copied only after
the build exited successfully.
