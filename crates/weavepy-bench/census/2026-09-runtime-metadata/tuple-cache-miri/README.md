# Tuple storage memory-safety checks

This standalone crate contains a snapshot of the production tuple-allocation
module and a small harness. The harness substitutes a small object enum (24 bytes on ARM64) and
an eight-byte atomic hash cell so Miri can exercise allocation, ownership,
weak references, unique mutation, and concurrent publication without loading
the complete VM or its native dependencies.

Three tests pass on ARM64 and 32-bit x86 with strict provenance under both
the default aliasing model and Tree Borrows. `evidence.json` records the production source checksum and
toolchain; `strict.txt`, `tree.txt`, `i686.txt`, and `i686-tree.txt` retain the outputs. These checks don't
constitute a Miri run of the complete runtime or a proof of soundness.

The implementation relies on the documented
[Arc::from_raw layout contract](https://doc.rust-lang.org/std/sync/struct.Arc.html#method.from_raw):
the initialized tuple payload and original uninitialized word slice have the
same size and alignment, and the conversion preserves the allocation's data
address. It doesn't inspect Rust's private reference-count header layout.

The retained `i686-initial.txt` records a failed earlier version: on 32-bit
x86, the atomic hash cell requires stricter alignment than a 64-bit integer.
Selecting the hash cell itself as an allocation word fixes that mismatch.

With a nightly toolchain that has Miri installed, run from this directory:

```sh
MIRIFLAGS='-Zmiri-strict-provenance' cargo +nightly miri test --lib
MIRIFLAGS='-Zmiri-strict-provenance -Zmiri-tree-borrows' cargo +nightly miri test --lib
MIRIFLAGS='-Zmiri-strict-provenance' cargo +nightly miri test --target i686-unknown-linux-gnu --lib
MIRIFLAGS='-Zmiri-strict-provenance -Zmiri-tree-borrows' cargo +nightly miri test --target i686-unknown-linux-gnu --lib
```
