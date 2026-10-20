# Vendored `libmimalloc-sys`

This is `libmimalloc-sys` 0.1.49 from crates.io (bundling mimalloc 3.3.2,
`MI_MALLOC_VERSION 30302`) with one patch. The workspace's
`[patch.crates-io]` table points at it, so the `weavepy` binary's allocator
(`crates/weavepy-cli/src/alloc.rs`) builds from here on every platform.

## Why it's vendored

mimalloc returns freed memory to the system by purging it. On macOS,
upstream purges with `madvise(MADV_FREE_REUSABLE)`, which lowers the
process's footprint, but the pages stay in its resident size (and so in the
peak `ru_maxrss`) until the system reclaims them under memory pressure.
`MADV_DONTNEED` and `MADV_FREE` behave the same way. Only unmapping the
range, or mapping fresh pages over it, takes them out. With 64 MB of touched
pages, the resident size stayed at 66 MB after each of those advices and
fell to 0.6 MB once fresh pages were mapped over the range.

With the upstream purge, nothing freed ever left a WeavePy process's RSS,
and no runtime option helps. `purge_decommits`, `purge_delay`,
`page_full_retain`, `page_commit_on_demand` and `arena_reserve` made no
difference. `disallow_arena_alloc` gets the RSS back, but it maps and
unmaps every page: 0.55% more instructions over the benchmark suite. The
patch, with a 10 ms purge delay set in `alloc.rs` (macOS only), brought the
suite's peak RSS down about 7% for 0.22% more instructions.

On Linux, mimalloc already purges with `MADV_DONTNEED`, which drops resident
pages there, and Windows decommits. Neither path changes.

## What differs from upstream

- `c_src/mimalloc/v3/src/prim/unix/prim.c`: the patch below.
- Removed, as nothing here compiles them: mimalloc 2's sources
  (`c_src/mimalloc/v2`, so the crate's `v2` feature no longer builds) and,
  under `c_src/mimalloc/v3`, everything but `include/`, `src/` and
  `LICENSE`. Also removed: the published crate's `Cargo.toml.orig`,
  `Cargo.lock`, `.cargo_vcs_info.json` and `.gitignore`.

Everything else (`Cargo.toml`, `build.rs`, `src/`, `LICENSE.txt` and the
compiled C sources) is byte-for-byte upstream.

```diff
--- libmimalloc-sys-0.1.49/c_src/mimalloc/v3/src/prim/unix/prim.c
+++ vendor/libmimalloc-sys/c_src/mimalloc/v3/src/prim/unix/prim.c
@@ -492,10 +492,16 @@
 
 int _mi_prim_decommit(void* start, size_t size, bool* needs_recommit) {
   int err = 0;
   #if defined(__APPLE__) && defined(MADV_FREE_REUSABLE)
-    // decommit on macOS: use MADV_FREE_REUSABLE as it does immediate rss accounting (issue #1097)
-    err = unix_madvise(start, size, MADV_FREE_REUSABLE);
-    if (err) { err = unix_madvise(start, size, MADV_DONTNEED); }
+    // decommit on macOS: map fresh zero pages over the range (WeavePy patch).
+    // MADV_FREE_REUSABLE only lowers the footprint: the pages stay in the
+    // resident size (and `ru_maxrss`) until the system reclaims them. If the
+    // mapping fails, the range is left as it was and marked reusable.
+    void* p = mmap(start, size, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, unix_mmap_fd(), 0);
+    if (p != start) {
+      err = unix_madvise(start, size, MADV_FREE_REUSABLE);
+      if (err) { err = unix_madvise(start, size, MADV_DONTNEED); }
+    }
   #else
     // decommit: use MADV_DONTNEED as it decreases rss immediately (unlike MADV_FREE)
     err = unix_madvise(start, size, MADV_DONTNEED);
```

The mapping keeps the range readable and writable, as upstream's purge
does, so `needs_recommit` stays false in release builds. A purged range's
contents are undefined to mimalloc either way, so zero pages are a valid
result. The mapping uses mimalloc's own VM tag (`unix_mmap_fd()`).

## Updating

To move to a newer `libmimalloc-sys`:

1. Copy the published crate over this directory.
2. Remove the files listed above.
3. Reapply the patch to `_mi_prim_decommit`, and update this file.

Cargo doesn't rebuild the C sources when only they change, so touch
`build.rs` after editing them.
