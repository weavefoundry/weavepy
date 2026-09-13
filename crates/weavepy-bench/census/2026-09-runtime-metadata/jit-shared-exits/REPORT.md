# Shared terminal JIT exits: validated candidate

The candidate shares terminal local/operand writeback for exits with the same status and exact operand lanes. Each exit still carries its own bytecode position, operands, and predecessor-specific local values. The existing cold conditional edges and pre-helper writebacks remain. The change adds no runtime unsafe code, guard relaxation, or native eligibility changes.

The isolated candidate is based on phase 81 cold-reset source and does not include primary phase 79's native-buffer byte budget. Only lower.rs and engine/tests.rs differ among 340 frozen source identities. Current identities are in jit-shared-exits/candidate-source-hashes-03.json and the narrow final patch is changes-03.patch. Main sources and CLI remain unchanged.

The initial state-oracle run exposed two incorrect test expectations: normal returns do not write back dead locals. The deoptimization assertions passed. Corrected tests passed against both phase 81 and shared-exit lowering. An equivalent inclusive-range expression then satisfied strict Clippy. Initial sources, failed runs, and both correction records remain preserved. No performance samples were collected from those drafts.

Final validation passed 64 JIT tests, 351 VM tests, 156 C API tests, strict JIT and runtime lint, no-JIT compilation, 99 targeted checks across JIT/interpreter/GIL-0 modes, all 44 expected native paths, and 275 compatibility checks. Four new state tests cover arithmetic exits at distinct assignment points, mixed float/int snapshots, bool/int/opaque pin tags, and loop exits from both function and mid-loop entries. The existing frame-identity limitation is unchanged and explicitly recorded.

The immutable runtime is target/release/weavepy-runtime-jit-shared-exits, SHA-256 2218be3f68ccf27c185c5214f667c8eeafac599d36ebc05a89deab20df4fa2f0, 44,097,888 bytes. It is 560 bytes larger than phase 81. Its build uses cargo build --release -p weavepy-cli --bin weavepy with locked offline dependencies in a separate copy-on-write target.

A standalone allocation diagnostic ran three alternating pairs with widths 4, 32, 128, 4, and 128. Every per-variant record was identical across repetitions; all older compiled entries and exact overflow exits remained valid. Peak requested Rust allocation dropped 12.4% to 29.2%. At width 128, peak bytes fell from 50,254,573 to 44,008,334, while allocation requests increased from 14,919 to 16,622. Small-function retained requested bytes rose by 1,656; wide-function retained bytes were unchanged. Full raw counts and limitations are in jit-shared-exits/allocation-probe/REPORT.md and report.json.

Static ARM64 prologues show compile_tfunc's frame rising from 1,568 to 1,680 bytes. Native helper frames remain 1,040 bytes for nested calls, 960 for direct calls, and 448 for dynamic wrappers. Ordinary compiler cleanup remains 64 bytes; the rare reset remains 10,592. These are individual static frames, not cumulative stack peaks or RSS.

These findings establish correctness within the tested suite and an instrumented compiler-heap tradeoff. They do not establish runtime speed, whole-VM peak RSS, or superiority over CPython. Controlled timing has its own frozen protocol, per-binary caches, checksum/native-path checks, serial lease, load telemetry, and unfiltered samples. The broad user goal remains unachieved.

The first runtime timing gate closed with status not_started and exit code 75 after 600 seconds. It launched no child and collected no samples because host load remained above the fixed threshold. The unchanged telemetry and zero-sample deferral are preserved. Runtime comparisons remain outstanding; a separate early-guard-sealing experiment follows this candidate.
