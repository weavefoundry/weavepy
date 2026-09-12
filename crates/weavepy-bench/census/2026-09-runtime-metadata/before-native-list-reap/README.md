# Tagged appends before native list cleanup

The executable is `target/release/weavepy-runtime-tagged-append`. Its checksum
appears in each measurement file. All 267 VM unit tests, 38 JIT tests, Clippy,
and the targeted builder checks pass. This intermediate stage did not run
the complete compatibility suite.

The four checked warm builders take 2.6–4.9 percent of the aligned previous
build's workload time. They take 33–57 percent of CPython's workload time.
These are workload timers; process startup and peak RSS remain higher than
CPython's. The standard byte-scrambler fixture takes 5.5 percent of the original
checkpoint's time and 66.6 percent of CPython's time.

The byte builder increases peak RSS from roughly 30.3 MiB to 40.8 MiB. A
diagnostic run after warmup and a full collection leaves 200 dead lists of
length 1,024 after 200 compiled calls. Interpreter mode and CPython leave none;
an explicit collection releases the retained lists. `drain_runtime_pins`
skips `Pin::List`, leaving each temporary held by the collector's strong handle.
The next stage fixes this lifetime error. `test_jit_list_lifetime.py` fails on
this executable with `AssertionError: (0, 200)` and passes on CPython.
