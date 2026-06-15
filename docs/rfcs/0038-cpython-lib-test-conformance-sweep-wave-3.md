# RFC 0038: CPython `Lib/test/` conformance sweep, wave 3 — binary/codec, filesystem/OS, and CLI clusters

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-06-13
- **Tracking issue**: TBD
- **Builds on**: RFC 0037 (wave 2 — root-cause clusters + verbatim module
  ports), RFC 0036 (wired a real CPython 3.13 `Lib/test/` checkout into
  `regrtest` and rewrote the touched `expectations.toml` rows from guesses
  to a *measured* baseline), RFC 0035 (faithful `re`/Unicode), RFC 0033
  (`ast`/`dis`/`marshal` introspection), RFC 0017 (OS + networking),
  RFC 0019 (numerics + serialization).

## Summary

RFC 0036 made the CPython regression suite *runnable and measured*; RFC
0037 (wave 2) attacked the shared root causes gating the largest clusters
and flipped ~30 files green. The committed baseline in
`tests/regrtest/expectations.toml` is `--check` clean and currently
records **44 `pass`, 72 `fail`, 32 `skip`** against the vendored CPython
3.13 suite.

This RFC is **wave 3 of the sweep**. It targets three *bounded, low-risk*
clusters of the remaining `fail` rows — the families where the backing
module already ships and only edge-case fidelity is missing:

- **WS-A — binary / hashing / compression / codecs**: `base64`,
  `binascii`, `hashlib`, `hmac`, `zlib`, `gzip`, `bz2`, `lzma`,
  `zipfile`, `tarfile`, `codecs`, `json`.
- **WS-B — filesystem / OS surface**: `os`, `posixpath`, `pathlib`,
  `tempfile`, `glob`, `fnmatch`, `shutil`, `stat`, `io`, `posix`,
  `fcntl`, `resource`.
- **WS-C — CLI / text tooling**: `argparse`, `optparse`, `getopt`,
  `pprint`, `csv`, `logging`, `warnings`.

The throughline is unchanged from RFC 0035/0036/0037: **where behaviour
is defined by CPython, port CPython** (verbatim Python modules where
practical; faithful semantics in the Rust accelerators otherwise) rather
than re-approximate it.

The deliverable is measured, not aspirational: every workstream names the
`expectations.toml` rows it flips, the commit is not done until a fresh
subprocess sweep is `--check` clean with those rows rewritten from `fail`
to `pass`, and each workstream lands at least one bundled regression
fixture so CI catches regressions without the full CPython checkout.

## Motivation

The README's headline promise is "a 100% compatible, drop-in replacement
for CPython … using CPython's own test suite as a guiding standard." RFC
0036 made the number auditable; RFC 0037 moved it; this RFC keeps it
moving along the lowest-risk axis.

The deliberate choice this wave is **breadth over depth on the bounded
tail**. The three clusters above share two properties that make them the
right next target:

1. **The backing module already ships and imports cleanly.** Many of the
   current `fail` reasons are explicitly "ships and imports cleanly; full
   conformance unverified pending a local `Lib/test` checkout"
   (`test_array`, `test_bz2`, `test_lzma`, `test_tarfile`, `test_marshal`).
   Now that RFC 0036 vendored the checkout, these can be *measured* and
   closed rather than guessed.
2. **The failures are edge cases, not missing subsystems.** `b85`/`a85`
   codecs, `shake`/`blake2` variable-length digests, gzip multistream,
   `Path.walk`, `argparse` subparsers — each is a contained gap in an
   existing module, not a new runtime capability. That keeps the blast
   radius small and the per-file verdict honest.

The higher-ceiling arcs RFC 0037 named — real parallel threads + faithful
`asyncio`, and CPython-ABI binary wheel loading — remain the right *big*
bets, but they are deep, cross-cutting, and a poor fit for a single
clean commit. They are sequenced after this wave (see Future work).

## CPython reference

This RFC matches CPython 3.13 behaviour as defined by the vendored
`vendor/cpython/Lib/` tree and the corresponding `Lib/test/` files:

- **Binary/codec**: `Lib/base64.py`, `Lib/binascii` (C, ported behaviour),
  `Lib/hashlib.py`, `Lib/hmac.py`, the `Lib/encodings/` codec registry,
  `Modules/_codecsmodule.c` error handlers, `Lib/json/`.
- **Compression**: `Lib/gzip.py`, `Lib/bz2.py`, `Lib/lzma.py`,
  `Lib/zipfile/`, `Lib/tarfile.py`, `zlib` flush/reuse semantics
  (`Modules/zlibmodule.c`).
- **Filesystem/OS**: `Lib/posixpath.py`, `Lib/genericpath.py`,
  `Lib/pathlib/`, `Lib/glob.py`, `Lib/fnmatch.py`, `Lib/shutil.py`,
  `Lib/tempfile.py`, `Lib/stat.py`, `Lib/_pyio.py` (buffered/text IO),
  `Modules/posixmodule.c` (`stat_result`, `scandir`).
- **CLI/text**: `Lib/argparse.py`, `Lib/optparse.py`, `Lib/getopt.py`,
  `Lib/pprint.py`, `Lib/csv.py` + `Modules/_csv.c`, `Lib/logging/`,
  `Lib/warnings.py` + `Python/_warnings.c`.

Where this RFC ports a CPython `.py` file verbatim, it is pinned to the
3.13 branch tag already vendored under `vendor/cpython/`.

## Current baseline (measured starting point)

- `cargo build --workspace` is green.
- Bundled `tests/regrtest/` suite is `--check` clean (`unexpected 0`).
- CPython `Lib/test/` allowlist in `expectations.toml`:
  **44 `pass`, 72 `fail`, 32 `skip`** (442 test files vendored).

Wave 3 targets a coherent subset of the `fail` rows (see
[§Measured targets](#measured-targets)); the full tail remains a
multi-wave effort and this RFC does **not** claim to close it.

## Detailed design

Three workstreams plus fixtures and the baseline rewrite. Each lists the
affected crate(s)/module(s), the design, and the `expectations.toml` rows
it is expected to flip. Line-count estimates are rough and include ported
CPython `.py`, Rust glue, and tests.

### WS-A — binary / hashing / compression / codecs · ~9K LOC

**hashlib / hmac.** Add the variable-length SHA-3 family
(`shake_128`/`shake_256` with the `.hexdigest(length)` / `.digest(length)`
signature), `blake2b`/`blake2s` (with `digest_size`/`key`/`salt`/`person`
parameters), the `usedforsecurity=` flag plumbed through every
constructor, and `hashlib.new(name)` name resolution against
`algorithms_available`/`algorithms_guaranteed`. For `hmac`: route
`digestmod=` through the same name lookup, expose `hmac.new`/`hmac.digest`
fast path, and ensure `compare_digest` is constant-time over both `str`
(ASCII) and bytes-like inputs.

**base64 / binascii.** Add `b85encode`/`b85decode` and
`a85encode`/`a85decode` (Ascii85, with `adobe=`/`foldspaces=`/`wrapcol=`
/`pad=` options), fix `standard_b64*` vs `urlsafe_b64*` alphabet edges,
and in `binascii` add `hexlify(data, sep, bytes_per_sep)`, the
`b2a_*`/`a2b_*` round-trips the `uu_codec` needs, and correct
`Error`/`Incomplete` exception types.

**zlib / gzip / bz2 / lzma.** Compressor/decompressor *reuse* and the
`flush(mode)` modes (`Z_SYNC_FLUSH`/`Z_FULL_FLUSH`/`Z_FINISH`),
`decompressobj().unused_data`/`unconsumed_tail`/`eof`, gzip `mtime`
preservation + multistream reads, `bz2`/`lzma` multistream + compressor
reuse, and the `lzma` raw-filter chains + `FORMAT_ALONE`.

**zipfile / tarfile.** `zipfile` ZIP64 read/write already passes for
STORED/DEFLATE; add the BZIP2/LZMA compression methods and the
`Path`/`Pathlib`-style `zipfile.Path` accessor. `tarfile`: PAX and
GNU long-name/long-link headers, stream (`r|`/`w|`) modes, and the
sparse-file extension headers.

**codecs / json.** `codecs` incremental encoder/decoder state machines
and the standard error handlers (`strict`/`ignore`/`replace`/`xmlcharrefreplace`
/`backslashreplace`/`namereplace`/`surrogatepass`/`surrogateescape`), plus
`codecs.lookup` going through the `encodings` registry. `json`: stable
`sort_keys` over non-`str` keys (CPython coerces + sorts by the coerced
key), `indent`/`separators` interaction, and `parse_constant`/`object_pairs_hook`.

**Flips:** `test_base64`, `test_binascii`, `test_hashlib`, `test_hmac`,
`test_zlib`, `test_gzip`, `test_bz2`, `test_lzma`, `test_zipfile`,
`test_tarfile`, `test_codecs`, `test_json`; contributes to `test_array`,
`test_marshal`.

### WS-B — filesystem / OS surface · ~9K LOC

**os / posix / stat.** Complete the `os.stat_result` struct-sequence
(the `st_*_ns` nanosecond fields, `st_blocks`/`st_blksize`/`st_rdev`,
and the platform extras as `n_unnamed_fields`), `os.scandir` nesting +
`DirEntry.is_dir/is_file/is_symlink/stat` caching, `os.fspath`/`PathLike`
edges, and the `stat` module's `filemode`/`S_IS*` predicates including
setgid/setuid/sticky.

**posixpath / genericpath.** `realpath(strict=True)` (raising on missing
components and on symlink loops), `commonpath`/`commonprefix` edge cases,
and the `splitroot` (3.13) helper.

**pathlib.** `Path.walk` (3.12+), `PurePath.full_match` (3.13),
`Path.relative_to(walk_up=True)` (3.12), and the `with_segments` plumbing
that the 3.13 pathlib rewrite threads through every derived path.

**glob / fnmatch.** Recursive `**` semantics (including the
`include_hidden=` flag and the "no cross into symlinked dirs unless
`recursive`" rule), `glob.translate` (3.13), and `fnmatch.translate`
producing the exact regex CPython does so `fnmatch.filter` matches.

**shutil / tempfile / io.** `shutil.copytree(symlinks=…, dirs_exist_ok=…)`,
`copystat` xattr/flags best-effort, `which` PATHEXT handling;
`tempfile.SpooledTemporaryFile` (rollover + full IO protocol) and
`NamedTemporaryFile(delete_on_close=…)` (3.12); and in `io` the
`BufferedRandom` read/write interleaving + `TextIOWrapper.detach`/`seek`
/`tell` cookie semantics.

**Flips:** `test_os`, `test_posixpath`, `test_pathlib`, `test_tempfile`,
`test_glob`, `test_fnmatch`, `test_shutil`, `test_stat`, `test_io`;
contributes to `test_posix`, `test_fcntl`, `test_resource`.

### WS-C — CLI / text tooling · ~6K LOC

**argparse / optparse / getopt.** Port the touched parts of CPython's
`argparse` verbatim where the divergence is in formatting: subparsers
(`add_subparsers` + `dest`/`required`), mutually exclusive groups,
`type=`/`choices=` error message wording, `nargs` edge values
(`?`/`*`/`+`/`REMAINDER`), and the usage/help formatter column math.
`optparse`: the help formatter + `TitledHelpFormatter`. `getopt`:
long-option *abbreviation* (unambiguous prefix matching) and the exact
`GetoptError` messages.

**pprint.** `width`/`depth`/`compact`/`sort_dicts`/`underscore_numbers`,
the recursive-structure `...` cycle guard, and the dispatch table for
`dict`/`list`/`set`/`namedtuple`/`dataclass` reprs.

**csv.** `Dialect` registration + validation, `DictReader` fieldnames
inference (first row) + `restkey`/`restval`, `Sniffer.sniff`/`has_header`,
and `QUOTE_*` modes including `QUOTE_NONNUMERIC` round-trips.

**logging / warnings.** `logging`: handler/logger hierarchy propagation,
`dictConfig` with `disable_existing_loggers`, `Logger.findCaller`
stacklevel, and `LogRecord` formatting (`%`/`{`/`$` styles). `warnings`:
filter precedence + `__warningregistry__`, `catch_warnings` re-entrancy,
`showwarning`/`formatwarning` override, and `warnings.warn(stacklevel=…)`
frame attribution.

**Flips:** `test_argparse`, `test_optparse`, `test_getopt`, `test_pprint`,
`test_csv`, `test_logging`, `test_warnings`.

### Fixtures + baseline rewrite · ~1K LOC (data)

After WS-A–WS-C, run `weavepy-conformance regrtest --cpython-dir
vendor/cpython/Lib/test --mode subprocess --jobs 8 --no-check` and rewrite
every touched row from `fail` to its **measured** status, quoting the
first remaining failure for any row that doesn't reach `pass`. The commit
is complete only when a subsequent `--check` sweep reports `unexpected 0`.
New `bundled/` regression fixtures (one per workstream) lock the behaviour
in-process so CI catches regressions without needing the full CPython
checkout.

## Measured targets

Wave 3's commit-acceptance bar is flipping the following
`expectations.toml` rows to `pass` (grouped by workstream). Anything that
runs further but still fails gets a rewritten, measured `reason` rather
than a guess. (The exact cut line is finalised against the baseline run;
files that prove deeper than estimated are rewritten to a measured
`reason` and deferred rather than expanding the commit.)

| Cluster | Target rows (→ `pass`) |
|---|---|
| WS-A binary/codec | `test_base64`, `test_binascii`, `test_hashlib`, `test_hmac`, `test_zlib`, `test_gzip`, `test_bz2`, `test_lzma`, `test_zipfile`, `test_tarfile`, `test_codecs`, `test_json` |
| WS-B filesystem/OS | `test_os`, `test_posixpath`, `test_pathlib`, `test_tempfile`, `test_glob`, `test_fnmatch`, `test_shutil`, `test_stat`, `test_io` |
| WS-C CLI/text | `test_argparse`, `test_optparse`, `test_getopt`, `test_pprint`, `test_csv`, `test_logging`, `test_warnings` |

That is **~28 files flipping `fail` → `pass`** out of the 72, plus
measured-truth rewrites for everything that advances but doesn't fully
pass. The remaining tail (concurrency: `test_threading`/`test_asyncio`/
`test_gc`; C-accelerators: `test_decimal`/`test_pyexpat`; container
`sys.getsizeof`/refcount probes; parser corners: PEP 646/695) is
explicitly **deferred to wave 4+**.

## Drawbacks

- **Breadth over depth risk.** Three clusters across ~28 files is a lot
  of surface; a regression in (say) the codec error-handler table could
  ripple into `str.encode`/`open(encoding=…)`. Mitigated by one bundled
  fixture per workstream and the `--check` baseline gate.
- **Verbatim ports carry CPython's own complexity.** `argparse`,
  `tarfile`, and `pathlib` pull in behaviour (and occasionally other
  imports) that may surface *new* gaps. We accept some scope creep within
  a workstream and cap it by deferring anything that needs an unshipped
  C-accelerator or a live network/process fixture.
- **The headline number won't hit 100%.** This is one wave; ~28 files
  flip and the long tail continues.

## Alternatives

- **Pivot to concurrency (real threads + `asyncio`).** Higher real-world
  value but deep VM surgery (GIL, cross-thread heap, GC) and a poor fit
  for a single clean commit. Sequenced as wave 4.
- **Pivot to the C-ABI binary-wheel story.** Highest ceiling (real numpy/
  pandas) but the highest risk; its own arc after concurrency.
- **Grind file-by-file in test order.** Lower coordination cost but
  repeatedly re-discovers the same root causes; RFC 0036/0037 already
  showed cluster fixes dominate.

## Prior art

- **PyPy** runs (a fork of) CPython's `Lib/test` as its compatibility bar
  and ports CPython `.py` modules largely verbatim — the same strategy
  the WS-A/WS-C verbatim ports use.
- **GraalPy/Jython** both found the long tail is dominated by edge-case
  fidelity in existing modules (codec error handlers, path semantics,
  formatter column math) rather than exotic features — matching WeavePy's
  measured clustering.

## Unresolved questions

- **`encodings` breadth.** How many codecs to register eagerly vs. lazily
  to keep startup fast and the frozen blob small (inherited from RFC 0037).
- **Native vs. ported compression.** Whether to grow the Rust
  `flate2`/`bzip2`/`xz2` glue to cover multistream/flush directly, or to
  port CPython's `gzip.py`/`bz2.py`/`lzma.py` over thinner Rust cores.
  Lean on the existing Rust cores; port the Python wrappers verbatim.
- **Exact wave-3 cut line.** If WS-C (`logging`/`argparse`) proves deeper
  than estimated, split it into wave 4 rather than expand the commit.

## Future work

- **Wave 4 — concurrency**: real parallel threads + faithful `asyncio`/
  selectors (epoll/kqueue), cycle-GC heuristics, and the container
  GC-reachability hangs (`test_list`/`test_tuple`/`test_set`/`test_weakset`).
- **C-accelerators**: `_decimal` (libmpdec-equivalent) + `pyexpat`,
  unblocking `test_decimal`/`test_xml_etree`/`test_statistics` perf.
- **CPython-ABI binary wheel loading**: the highest-ceiling drop-in lever
  (`Py_LIMITED_API`/stable-ABI wheels via `dlopen`).
