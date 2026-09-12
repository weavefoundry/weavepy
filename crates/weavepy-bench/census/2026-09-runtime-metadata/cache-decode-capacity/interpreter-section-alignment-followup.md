# Test stable alignment of the three interpreter sections

Unimplemented diagnostic lead. The compaction disassembly preserves all 44,210
opcodes in __wp_loop, __wp_step, and __wp_call, while their load-relative
addresses change. This does not prove the source of its workload regressions.
The compiler has already emitted compact_decoded_buffers out of line.

The local Apple ld manual documents -sectalign segname sectname value, with a
hexadecimal power-of-two byte alignment. A bounded link-only comparison could
align those three existing macOS/AArch64 sections to 0x80 bytes, retaining all
other flags and sources. Record symbol addresses, section sizes, instruction
comparisons, build flags, binary hashes, and every control result. Check actual
peak RSS and executable size, not only throughput. This differs from the older
all-function alignment experiment, which added substantial binary padding.
Don't adopt a linker option solely because one fixture improves. Explicitly
separate link-only experiment results from the default supported CLI build,
and revalidate any final build-script implementation. No such build has run.
