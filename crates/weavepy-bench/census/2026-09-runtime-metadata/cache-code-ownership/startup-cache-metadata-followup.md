# Startup cache metadata and benchmark launch paths

Unimplemented/unmeasured lead. stdlib_tree::build_id already reads a build-time
constant; it does NOT hash or page in every embedded source at startup. Don't
propose that already-completed optimization again.

The warm cache path calls refresh_install_metadata. It reads build-details.json
and, when base_interpreter differs from program_exe(), rewrites executable-
dependent installation metadata through write_install_json. The cache prefix is
keyed by embedded-source build ID. Several performance releases differ only in
Rust code and can share that build ID while having different executable paths.
Our paired benchmark variants use the same WEAVEPY_STDLIB_CACHE root. Source
inspection therefore suggests alternating renamed binaries may trigger metadata
rewrites that repeated launches of one installed binary don't. No diagnostic or
measurement has yet established the frequency or cost; don't alter or relabel
completed samples on this hypothesis.

After the current census finishes, use identical executable copies and bounded
startup probes to compare a target launch preceded by itself versus another
pathname, with shared/per-binary cache roots. Verify actual metadata contents and
writes and retain executable hashes, launch context, sources, raw timing/CPU/RSS.
Keep preparation outside timed regions and alternate paired order. If evidence
supports an effect, record cache layout as an explicit measurement condition and
preserve old comparisons. Cold extraction is a separate meaningful workload;
per-binary warm caches must not be presented as cold startup. No runtime or
benchmark implementation has changed for this lead.

Also control executable pathname length when investigating small regressions:
current artifacts have descriptive names of different lengths, and exe-derived
strings are allocated during initialization. Use copies of the SAME binary with
both equal-length and deliberately different-length paths, plus identical
fixtures, before attributing a small control shift to code layout or allocator
behavior. This has not been measured. It is a calibration question, not evidence
that any observed regression is noise or should be discarded. Standardizing
paths/caches later requires recording the changed measurement condition and
keeping all historical results intact.
