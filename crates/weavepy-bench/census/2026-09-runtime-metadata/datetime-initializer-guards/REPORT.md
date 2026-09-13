# Datetime initializer guards: unmeasured checkpoint

This isolated phase 96 candidate caches successful slot-layout validation by
class version and sends ordinary subclasses directly to their original Python
initialization path. Native payload, C-body, setter, metaclass, version, and
empty-slot guards still apply. Mutation invalidates the successful-layout cache.

Counter-based regression tests reduced full layout checks for 2,000 constructor
calls from about 2,000 to 3 through 5, depending on the datetime type. Subclass
helper attempts fell to import-time constants. These are path counts, not speed
measurements. Original failures, source versions, and corrected results remain
in the evidence archive.

Validation passed 380 VM tests, 156 C API tests with JIT enabled, 79 JIT tests,
strict lint and format checks, no-JIT checks, 99 targeted checks, 275 compatibility
checks, and all 24 census result comparisons. The 12 original constructor inputs
also passed six-engine correctness checks. The additional four ordinary-subclass
input checks and all performance measurements remain pending at the user's
requested checkpoint. No timing gate or sample has run for this revision.

The CLI is 44,119,120 bytes, SHA-256
`7f2b0647c1cf9d1454e70e36f97877296398ce7098bd8fb18e5431f5d54e4a61`.
The 340 sources, incremental patch against phase 95, methods, inputs, raw
validation, counter evidence, and timing protocol are archived. Binaries and
generated caches are excluded. Extract sources into a separate checkout and
build with `cargo build --release -p weavepy-cli`. The prepared v2 timing pipeline
keeps the four new subclass controls separate from the original twelve cases.

This candidate is not integrated into the active phase 79 runtime. Known
fold-index coercion and native-frame identity differences remain unresolved.
There is no runtime speed, peak RSS, 32-bit execution, energy, scaling, or
controlled build-time claim for this candidate.
