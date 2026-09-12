# Trailing-default correctness fix

The generic binder, ordinary native binder, and certified scalar-leaf binder
now select the trailing defaults suffix when a retained or assigned tuple
outnumbers the function's positional parameters.

The release executable is target/release/weavepy-runtime-default-suffix,
SHA25655f24c7ecf4c222b7c1fb093aba3c1c95856826495243d8a05d1807db6dff8a8,
44,177,312 bytes. Debug and release regression runs pass against CPython
in JIT, interpreter, and GIL-disabled modes. Separate release traces prove
12,998 native calls through each frame shape; the scalar proof uses12,998
scalar-leaf entries, while the ordinary proof uses12,997 nonscalar entries.

The original diagnostic reproduces the bug in the checkpoint and both prior
cache candidates. This is a correctness fix, with no separate full-suite or
performance census. The lazy-cache census remains the latest complete timing
report. The next candidate will run the full217-check compatibility suite.

The stale-binary-check directory records an invalid initial release check
that copied2efdd before linking finished. Its snapshot does not correspond
to its executable and must not be used to reproduce a candidate. The valid
inputs directory was frozen only after the new release build completed.
