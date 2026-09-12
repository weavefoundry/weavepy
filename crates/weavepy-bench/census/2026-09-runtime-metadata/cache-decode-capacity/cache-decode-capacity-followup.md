# Allocate decoded code buffers at their final lengths

Unimplemented follow-up to cached-code compaction. The completed compaction
census retains speed regressions, so its shrink-after-decoding approach isn't
yet an accepted resolution of the ownership release's memory regression.

Local source inspection identifies avoidable decoder capacity directly:

- `cpython_code::decode_linetable` reserves `raws.len()` entries, then expands
  fused instructions into multiple positions. It can use the already computed
  decoded instruction length as the exact reservation for both output tables.
  The preceding diagnostic attributed 1,243,008 spare bytes to those tables.
- `marshal_mod::tuple_of_strings` collects a fallible iterator. The tuple length
  is already available, so an explicitly sized vector and ordered fallible
  pushes can preserve errors without spare growth or an intermediate copy.
- `decode_full` partitions local names into four vectors. Counting applicable
  flags across the same zipped input first permits exact reservations, including
  the shared LOCAL|CELL and hidden-local cases.
- All-plain wire marks are cleared without releasing their allocated buffer.
  An empty replacement can release that capacity at decode time.

These are hypotheses about allocation, not measured speed or RSS improvements.
Do not edit sources until all active compaction diagnostics finish. Compare
metadata and existing malformed-marshal diagnostics, add a codec regression
covering fused instructions and source positions, and measure matched as well
as relocated caches. Prefer a focused candidate screen before another full
census. Preserve the current candidate and its measured regressions.
