DIAGNOSTIC COMPLETE: thin-LTO build succeeds, linked libraries exactly match the recorded release-layout libraries. The 56 prepared relocated import-cache artifacts contain2703codeobjects and1,621,236spareVecbytes: coltable932256;linetable310752;names185304;varnames122160;other70764 See exact CSV/JSON. These are requested spare vector bytes, not actual RSS. The next trial can compact these buffers while retaining decoded ownership; no compaction runtime edits yet.

# Reuse decoded code without retaining spare vector capacity

Measured in a6ff ownership stage: isolated relocated startup elapsed ratio
0.98125 and RSS 1.01068 versus 337; relocated imports elapsed 0.97052 and RSS
1.03362. Matched startup/imports are essentially unchanged. Every standard
cold/warm control has roughly 1 percent higher RSS; full-census aggregate RSS
is 1.00513 (JIT) and 1.01241 (interpreter) versus 337. Do not hide this tradeoff.

Hypothesis, not yet established: cloning decoded CodeObject values previously
compacted Vec buffers to their lengths. Taking ownership avoids clones but
retains the decoder's spare capacity, particularly when every nested code object
used to be cloned during filename relocation. A diagnostic harness to count
actual len/capacity bytes in the exact release's decoded artifacts is prepared;
its first link failed because Apple's linker couldn't read Rust LLVM22 bitcode.
Retry uses Rust thin LTO. No runtime code has changed to compact buffers.

If diagnostics support this, consider a private pycache ownership helper that
unwraps-or-clones the root and shrinks its decoded Vec buffers. Before recursing
into a nested code object during filename relocation, shrink that uniquely
owned/copy-on-write object's buffers too. This preserves the old compaction
benefit while avoiding copies of all names and constants. Cover instructions,
constants, names, varnames, freevars, cellvars, exception tables, line/column
vectors, no-interrupt jumps, wire marks, hidden locals, and constant identifiers.
Do not assume this removes all allocator fragmentation or guarantees lower RSS.

Keep content, bytecode/cache semantics, shared-code isolation, nested filenames,
and current traversal behavior unchanged. Existing two ownership tests plus the
cached-code-relocation integration fixture should remain. Test over-reserved
metadata with actual code behavior and repeat targeted startup/import measurements
against BOTH 337 and a6ff. Only an actual timing/RSS comparison can establish
whether compaction retains speed and repairs the measured memory regressions.
Do not edit runtime sources while profiles/oracles/timing are active.
