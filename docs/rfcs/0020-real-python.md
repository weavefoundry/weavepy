# RFC 0020: Real `python(1)` — the drop-in CLI, REPL, packaging, and conformance loop

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-22
- **Tracking issue**: TBD

## Summary

Close the gap between "WeavePy can execute Python end-to-end" (post
RFC 0019) and "**I can replace `python3` with `weavepy` in my shebang,
my CI, and my venv, and never notice.**" After this RFC lands:

- The `weavepy` CLI accepts the full set of CPython 3.13 command-line
  flags that real users and tools reach for: `-O` / `-OO`, `-X k=v`,
  `-W filter`, `-i`, `-u`, `-q`, `-b` / `-bb`, `-B`, `-I`, `-S`, `-E`,
  `-x`, `-d`, `-v`, plus the long-form spellings (`--help`,
  `--version`). The corresponding environment variables (`PYTHONPATH`,
  `PYTHONSTARTUP`, `PYTHONDONTWRITEBYTECODE`, `PYTHONUNBUFFERED`,
  `PYTHONHASHSEED`, `PYTHONNOUSERSITE`, `PYTHONHOME`, `PYTHONIOENCODING`,
  `PYTHONUTF8`, `PYTHONOPTIMIZE`, `PYTHONWARNINGS`) are read and
  applied.
- A **real interactive REPL** ships behind `weavepy` (no arguments) or
  `weavepy -i script.py`. Line editing, history (persistent under
  `~/.weavepy_history`), multi-line `... ` continuation, `_` bound to
  the last result, customizable `sys.ps1` / `sys.ps2`, and
  `PYTHONSTARTUP` execution all work. Built on `rustyline`. A
  fallback "dumb" REPL ships for non-tty stdin.
- A working **`__pycache__`** layer: imported source files are
  compiled to `<dir>/__pycache__/<name>.weavepy-3.13.pyc` on first
  import via the marshal core that RFC 0019 shipped, and re-loaded
  from cache on subsequent runs when mtimes match. `-B` and
  `PYTHONDONTWRITEBYTECODE` disable writes; reads always happen.
- The **`site` module** runs at interpreter start: it discovers
  `sys.prefix` / `sys.exec_prefix` / `sys.base_prefix` from the
  binary's location, builds the default `site-packages` paths, walks
  `.pth` files, and respects `PYTHONNOUSERSITE` /
  `--no-user-site`. `-S` disables site initialisation entirely.
- An **`importlib`** package with the modern submodule shape:
  `importlib.machinery`, `importlib.util`, `importlib.abc`,
  `importlib.metadata`, `importlib.resources`, plus `pkgutil`. Enough
  surface for `pip`, `setuptools`, `pytest`, `pluggy`, and the rest of
  the packaging ecosystem to reach for it.
- A **`venv` module**: `weavepy -m venv .venv` produces a working
  virtual environment whose `bin/python` points at WeavePy, whose
  `pyvenv.cfg` is shaped like CPython's, and whose `site-packages`
  layout the loader will find.
- An **`ensurepip`** module that bootstraps a bundled minimal `pip`
  (frozen Python) capable of `pip install <wheel>` and
  `pip install <package>` against a `--index-url` for pure-Python
  wheels. Built on top of `urllib`, `ssl`, `zipfile`, `hashlib`, and
  `sqlite3` (all shipped in RFC 0017 / 0019).
- **`pdb`** and **`bdb`** ship as frozen Python on top of RFC 0018's
  frame / traceback introspection. `python -m pdb script.py`,
  `pdb.set_trace()`, post-mortem debugging, and the canonical
  `next` / `step` / `continue` / `return` / `where` / `up` / `down`
  / `list` / `print` / `pp` / `b` / `cl` commands all work.
- A real **`regrtest`** subcommand: `cargo run -p weavepy-conformance
  -- regrtest` walks a configurable corpus of CPython
  `Lib/test/test_*.py` files, runs each unmodified under WeavePy,
  and gates the result against an `expectations.toml` file that
  records the known-passing set. CI fails if a passing test
  regresses *or* a newly-passing test is left un-promoted.
- The existing conformance comparator (`crates/weavepy-conformance/
  src/normalize.rs`) is rebuilt: tokens drop the leading `ENCODING`
  delta, `dis` output gains a real `format_cpython_dis` shape that
  matches CPython's `dis.dis` line layout, and `ast.dump`'s wrappers
  agree on field ordering. Result: the conformance harness moves
  from 0% / 4% / 0% to a real, monotonically improving number.
- A new **`pdb` / `pprint` / `tomllib` / `configparser` / `getopt`
  / `optparse` / `cProfile` / `profile` / `pstats` / `timeit` /
  `webbrowser` / `array` / `plistlib` / `zoneinfo`** family lands as
  frozen Python — the long tail of stdlib modules a real CLI user
  reaches for.
- A handful of remaining **parser / compiler gaps** close:
  starred assignment targets (`a, *b, c = xs`), `**dict` literal
  spread, top-level `await` (PEP 685), `*starred` expressions in
  collection literals.
- **15 new end-to-end fixtures** cover the CLI / REPL / site / pdb
  / venv / pip surface, plus a `regrtest` baseline of ~60 CPython
  `Lib/test/` files that pass today.

The combination is what the project calls "Option A" in the roadmap:
type `weavepy` at a shell prompt, get a REPL; type `weavepy -m venv
.venv && .venv/bin/pip install requests`, get a venv with `requests`
installed; type `weavepy -m pdb my_script.py`, get an interactive
debugger; type `weavepy -m unittest discover`, get a working test
runner. Every one of these works against CPython; after this RFC,
every one works against WeavePy too.

## Motivation

After RFC 0019, the *interpreter and stdlib* were essentially
complete for a drop-in replacement:

- The compiler runs the full language surface (with the small gaps
  this RFC closes).
- The VM runs OOP, dunders, descriptors, metaclasses, `__slots__`,
  exceptions, exception groups, generators, async, pattern matching,
  f-strings — every modern feature.
- ~38 Rust-backed stdlib modules + ~67 frozen Python modules ship,
  covering the data-interchange / OS / networking / introspection /
  test-infrastructure surface.

What did *not* exist was the **layer around the interpreter** that
turns "this binary executes Python" into "this binary is `python`":

- The CLI accepted four flags (`-c`, `-m`, `-V`, script). It rejected
  every other flag and every environment variable a real CPython
  installation honours.
- `weavepy` with no arguments printed `(REPL not yet implemented...)`
  and exited. Every interactive use was blocked.
- There was no `site` module, so `site-packages` was undiscoverable
  and `pip install`'s output went into a directory the interpreter
  never looked at.
- There was no `importlib.metadata`, so `importlib.metadata.version`
  raised `ModuleNotFoundError`. Every modern packaging stack
  (Poetry, hatch, pip ≥ 22, pytest plugins) calls into it.
- There was no `venv`, so the "isolated working environment" pattern
  that every Python developer uses didn't exist.
- There was no `pip` story. The entire third-party ecosystem
  (anything you `pip install`) was unreachable.
- There was no `pdb`, so every error required `print`-debugging.
  RFC 0018 shipped the frame introspection `pdb` needs; the
  consumer never landed.
- The conformance harness's `regrtest` subcommand was a placeholder
  that exited cleanly without doing anything. The project's stated
  acceptance criterion — "the CPython test suite passes" — was
  literally not measured.
- The `.pyc` cache wasn't wired, so every `weavepy` invocation
  recompiled ~25K LOC of frozen Python from scratch. Startup time
  on a cold cache was painful.

Each of these is small individually. Together they're the difference
between *a research project* and *a Python distribution*.

Down-tree, this RFC unblocks:

- **Real-world usage** — every workflow that begins with `python -m
  venv .venv && .venv/bin/pip install …` now works.
- **The C-API arc** (future RFC) — Stage A of "load `numpy.so`"
  requires a working `pip install`, a working `__pycache__`, and a
  working `importlib.metadata` to negotiate wheel selection.
- **The performance arc** (future RFC) — the adaptive specialization
  / inline cache work needs a stable bytecode format on disk
  (`__pycache__`) and a working benchmarking workflow (`timeit`,
  `cProfile`, both shipped here).
- **The acceptance criterion** — `regrtest` going from "placeholder"
  to "real" is what lets us *measure* "drop-in replacement" instead
  of *claim* it.

## CPython reference

This RFC tracks **CPython 3.13**:

- **Command-line interface** — the `python(1)` manpage, the
  documentation chapter "Command line and environment", and
  `Modules/main.c` + `Modules/_testcapi/`. We follow the exact
  argv-handling semantics: `-c "src"` sets `argv[0] = "-c"`,
  `-m pkg.mod` sets `argv[0]` to the module's `__file__`, the
  script path sets `argv[0]` to the script, `-` reads stdin and
  sets `argv[0] = "-"`, etc.
- **REPL** — `Lib/code.py` + `Lib/codeop.py` + `Modules/_curses_panel.c`
  for the line-editing path (we substitute `rustyline` for
  `readline`). The PEP 3120 utf-8 default and PEP 3140 `repr`
  defaults apply.
- **`site` module** — `Lib/site.py` and the `Modules/getpath.c`
  prefix-discovery dance. We follow PEP 405 (virtual environments).
- **`importlib` package** — `Lib/importlib/__init__.py`,
  `Lib/importlib/abc.py`, `Lib/importlib/machinery.py`,
  `Lib/importlib/util.py`, `Lib/importlib/metadata.py`,
  `Lib/importlib/resources/__init__.py`. PEP 451 (module specs),
  PEP 491 (wheels), PEP 503 (simple repository API),
  PEP 566 (metadata 2.1).
- **`venv` module** — `Lib/venv/__init__.py` and PEP 405.
- **`ensurepip` + `pip`** — `Lib/ensurepip/__init__.py` and the
  PyPA `pip` source (we ship a minimal subset compatible with
  pip's CLI surface, not the full implementation).
- **`pdb` / `bdb`** — `Lib/pdb.py` and `Lib/bdb.py`. The
  user-visible command set tracks CPython's.
- **`__pycache__`** — PEP 3147 (cache layout) and PEP 488 (the
  `.cpython-*` / `.pypy-*` / `.weavepy-*` tag).
- **`pprint`** — `Lib/pprint.py`. The full `PrettyPrinter` surface.
- **`tomllib`** — `Lib/tomllib/__init__.py` (CPython 3.11+).
  The `loads` / `load` / `TOMLDecodeError` surface.
- **`configparser`** — `Lib/configparser.py`.
  `ConfigParser` / `RawConfigParser` / `SafeConfigParser`.
- **`getopt`** — `Lib/getopt.py`. The `getopt` / `gnu_getopt` pair.
- **`optparse`** — `Lib/optparse.py`. Deprecated upstream but
  still in heavy use; we ship the subset most CLI code touches.
- **`cProfile` / `profile` / `pstats`** — `Lib/cProfile.py`,
  `Lib/profile.py`, `Lib/pstats.py`. Profile-mode timing via
  `sys.setprofile`, which gains a real implementation here.
- **`timeit`** — `Lib/timeit.py`. The `Timer` class plus
  `python -m timeit` CLI.
- **`webbrowser`** — `Lib/webbrowser.py`. Cross-platform
  default-browser launch.
- **`array`** — `Modules/arraymodule.c`. We ship a Rust core
  (`_array`) plus a frozen Python wrapper; the type-code surface
  matches CPython.
- **`plistlib`** — `Lib/plistlib.py`. macOS-style binary and XML
  property lists.
- **`zoneinfo`** — `Lib/zoneinfo/__init__.py` + PEP 615. We
  embed a stripped IANA tzdata blob (only the named zones, no
  posix-only variants) so `ZoneInfo("America/Los_Angeles")` works
  without a system tzdata install.
- **`unittest.IsolatedAsyncioTestCase`** — `Lib/unittest/async_case.py`.
- **PEP 685** (`await` at module level) — accepted in 3.13. We
  follow the rule that a module containing a top-level `await`
  must be executed under an async runner; the CLI's `-c` /
  script paths wrap the body in an implicit coroutine when needed.

We deliberately do **not** track:

- **A full `pip` implementation.** The bundled pip in `ensurepip`
  is intentionally minimal: it installs pure-Python wheels from
  PyPI or a local path; it does not yet support `requirements.txt`
  resolution beyond a flat list, build-from-source via
  `pyproject.toml` PEP 517, or extras / markers in the deep way
  modern pip does. Real pip can be installed (recursively) once
  the minimal one bootstraps.
- **The `_curses` / `readline` / `tkinter` / `idle` modules.**
  Out of scope; the REPL uses `rustyline` directly. `_curses` is
  a separate ecosystem.
- **Loadable C extensions in venvs.** `pip install requests` works
  (pure Python); `pip install numpy` does not (C extension —
  needs the C-API arc).
- **`asyncio.IsolatedAsyncioTestCase`'s deep loop-lifecycle**
  (`async def asyncSetUp`/`asyncTearDown` running on a fresh
  loop per test). We ship the surface; the deep wiring with
  user-supplied loop factories is approximate.
- **PEP 657 column-precise tracebacks** — still line-only.
- **Source-hash `__pycache__` mode (PEP 552)** — we ship the
  mtime mode that CPython 3.7+ defaults to.

## Detailed design

### Crate-by-crate scope

#### `weavepy-cli` (extended)

| Surface                              | File                | LOC (approx.) |
|--------------------------------------|---------------------|--------------:|
| Argv + env-var parsing              | `main.rs`           | +900          |
| Real REPL (rustyline-backed)         | `repl.rs` (new)     | 600           |
| `regrtest`/`venv`/`pip` plumbing     | `main.rs`           | +200          |

#### `weavepy-vm` (Rust extensions)

| Surface                                  | File                       | LOC (approx.) |
|------------------------------------------|----------------------------|--------------:|
| `__pycache__` writer / reader            | `import.rs`                | +400          |
| `site` Rust shim (prefix discovery)      | `stdlib/site_mod.rs` (new) | 200           |
| `_importlib` shim                        | `stdlib/importlib_mod.rs`  | 150           |
| `_array` core                            | `stdlib/array_mod.rs` (new)| 350           |
| `_zoneinfo` core (tzdata blob)           | `stdlib/zoneinfo_mod.rs`   | 250           |
| `_pdb` step/breakpoint hooks             | `stdlib/pdb_mod.rs` (new)  | 200           |
| `_profile` shim (`sys.setprofile` real)  | `stdlib/profile_mod.rs`    | 150           |
| Top-level `await` wrapper                | `lib.rs`                   | +200          |
| Parser/compiler gap fixes                | `parser.rs`,`compiler/lib.rs` | +500       |

#### Frozen Python modules

| Module                                          | Source file                          | LOC (approx.) |
|-------------------------------------------------|--------------------------------------|--------------:|
| `site`                                          | `stdlib/python/site.py`              | 300           |
| `importlib.__init__`                            | `stdlib/python/importlib_init.py`    | 250           |
| `importlib.machinery`                           | `stdlib/python/importlib_machinery.py` | 200         |
| `importlib.util`                                | `stdlib/python/importlib_util.py`    | 200           |
| `importlib.abc`                                 | `stdlib/python/importlib_abc.py`     | 150           |
| `importlib.metadata`                            | `stdlib/python/importlib_metadata.py` | 600          |
| `importlib.resources`                           | `stdlib/python/importlib_resources.py` | 250         |
| `pkgutil`                                       | `stdlib/python/pkgutil.py`           | 250           |
| `venv`                                          | `stdlib/python/venv_mod.py`          | 500           |
| `ensurepip`                                     | `stdlib/python/ensurepip.py`         | 200           |
| `_minipip` (bundled minimal pip)                | `stdlib/python/_minipip.py`          | 900           |
| `pdb`                                           | `stdlib/python/pdb_mod.py`           | 1100          |
| `bdb`                                           | `stdlib/python/bdb_mod.py`           | 600           |
| `pprint`                                        | `stdlib/python/pprint_mod.py`        | 700           |
| `tomllib`                                       | `stdlib/python/tomllib_mod.py`       | 700           |
| `configparser`                                  | `stdlib/python/configparser_mod.py`  | 1400          |
| `getopt`                                        | `stdlib/python/getopt_mod.py`        | 200           |
| `optparse`                                      | `stdlib/python/optparse_mod.py`      | 1500          |
| `cProfile` / `profile`                          | `stdlib/python/profile_mod.py`       | 600           |
| `pstats`                                        | `stdlib/python/pstats_mod.py`        | 700           |
| `timeit`                                        | `stdlib/python/timeit_mod.py`        | 350           |
| `webbrowser`                                    | `stdlib/python/webbrowser_mod.py`    | 600           |
| `array`                                         | `stdlib/python/array_mod.py`         | 300           |
| `plistlib`                                      | `stdlib/python/plistlib_mod.py`      | 600           |
| `zoneinfo`                                      | `stdlib/python/zoneinfo_mod.py`      | 500           |
| `unittest.async_case`                           | `stdlib/python/unittest_async.py`    | 250           |

#### `weavepy-conformance` (rebuilt)

| Surface                          | File          | LOC (approx.) |
|----------------------------------|---------------|--------------:|
| Token comparator (ENCODING + Op normalisation) | `normalize.rs` | +120 |
| Dis comparator (CPython-shape)   | `normalize.rs` | +150          |
| AST comparator (field ordering)  | `normalize.rs` | +100          |
| `regrtest` runner (real)         | `regrtest.rs` (new) | 500       |
| Expectations file                 | `expectations.toml` (data) | 250  |

#### Fixtures

| Fixture | What it shows |
|---------|--------------|
| `77_repl.py`         | REPL invariants: `_` binding, history file write, multi-line continuation (driven via stdin script) |
| `78_pycache.py`      | `__pycache__` round-trip: write `.pyc`, mutate source, reload after touch |
| `79_site.py`         | `site.getsitepackages()`, `USER_SITE`, `.pth` walk |
| `80_importlib.py`    | `importlib.machinery.SourceFileLoader`, `importlib.util.spec_from_file_location` |
| `81_importlib_metadata.py` | `version()`, `distributions()`, `entry_points()` over a synthetic dist-info |
| `82_venv.py`         | `venv.create('.venv')` produces the expected files |
| `83_pdb_post_mortem.py` | `pdb.post_mortem` walks a traceback, prints the right frame info |
| `84_pprint.py`       | `pprint.pformat` on a nested structure |
| `85_tomllib.py`      | round-trip a CPython-shaped `pyproject.toml` |
| `86_configparser.py` | read/write an `.ini` file with `[section]` and `key = value` |
| `87_optparse_getopt.py` | parse `-x foo --long bar pos1 pos2` |
| `88_profile_timeit.py` | `cProfile.run`, `pstats.Stats.sort_stats`, `timeit.Timer.timeit` |
| `89_array_plistlib.py` | array type codes; XML and binary plist round-trip |
| `90_zoneinfo.py`     | `ZoneInfo("UTC")` / `ZoneInfo("America/Los_Angeles")` arithmetic |
| `91_isolated_async.py` | `unittest.IsolatedAsyncioTestCase` runs an `async def test_*` |

#### Totals

~5K LOC Rust, ~17K LOC frozen Python, ~1.5K LOC fixtures, ~500 LOC
conformance, plus minor lifts to `Cargo.toml`/`stdlib/mod.rs`/CI.
Net diff ≈ **22-28K LOC**.

### CLI flag handling

The `weavepy-cli` `Cli` struct grows from a handful of fields to the
full CPython 3.13 surface. Flag interactions follow CPython:

```
weavepy [-bBdEhiIOPqsSuvVWx?] [-c command | -m module-name | script | -]
        [-X option] [--check-hash-based-pycs default|always|never]
        [--help] [--help-env] [--help-xoptions] [--version] [args ...]
```

| Flag                                     | Behaviour |
|------------------------------------------|-----------|
| `-b`                                     | Warn on `bytes`/`str` comparisons. |
| `-bb`                                    | Error on `bytes`/`str` comparisons (sets `sys.flags.bytes_warning = 2`). |
| `-B`                                     | Suppress `.pyc` writes (`sys.dont_write_bytecode = True`). |
| `-c command`                             | Execute `command` as `__main__`; argv[0] = `-c`. |
| `-d`                                     | Parser debug (no-op stub today). |
| `-E`                                     | Ignore all `PYTHON*` env vars. |
| `-h`, `--help`                           | Print help and exit 0. |
| `-i`                                     | Drop into REPL after script. |
| `-I`                                     | Isolated mode: implies `-E`, `-s`, sets `sys.flags.isolated`. |
| `-m module`                              | Run library module as `__main__`. |
| `-O`                                     | Optimisation level 1 (`__debug__ = False`). |
| `-OO`                                    | Optimisation level 2 (also strips docstrings). |
| `-P`                                     | Don't prepend script dir / cwd to `sys.path`. |
| `-q`                                     | Suppress REPL banner. |
| `-s`                                     | Don't add user site-packages to `sys.path`. |
| `-S`                                     | Don't run `site` initialisation. |
| `-u`                                     | Force stdout/stderr unbuffered. |
| `-v`                                     | Verbose imports (one `import` line per module loaded to stderr). |
| `-V`, `--version`                        | Print version. |
| `-W filter`                              | Append a warning filter (e.g. `-W ignore`). |
| `-x`                                     | Skip the first source line (shebang trick). |
| `-X opt[=val]`                           | Implementation-specific option (mirrored on `sys._xoptions`). |
| `--check-hash-based-pycs MODE`           | Cache-mode override (accepted; mtime-mode used regardless). |

Environment variables:

| Variable                       | Behaviour |
|--------------------------------|-----------|
| `PYTHONPATH`                   | Colon-/semicolon-separated list prepended to `sys.path`. |
| `PYTHONSTARTUP`                | File executed before the REPL starts (interactive only). |
| `PYTHONDONTWRITEBYTECODE`      | Same effect as `-B`. |
| `PYTHONUNBUFFERED`             | Same effect as `-u`. |
| `PYTHONHASHSEED`               | Sets `sys.flags.hash_randomization` accordingly. |
| `PYTHONNOUSERSITE`             | Same as `-s`. |
| `PYTHONHOME`                   | Override `sys.prefix` / `sys.exec_prefix`. |
| `PYTHONIOENCODING`             | `encoding[:errors]` for stdin/stdout/stderr. |
| `PYTHONUTF8`                   | Force UTF-8 mode (always-on for us; flag is honoured anyway). |
| `PYTHONOPTIMIZE`               | Same as `-O` / `-OO`. |
| `PYTHONWARNINGS`               | Comma-separated `-W` filters. |
| `PYTHONBREAKPOINT`             | Override `breakpoint()`'s target (default `pdb.set_trace`). |
| `PYTHONNODEBUGRANGES`          | Disable PEP 657 ranges (no-op today). |
| `PYTHONINSPECT`                | Same as `-i`. |

`-I` (isolated mode) trumps everything: when set, every `PYTHON*`
variable is ignored, the user site is suppressed, and `sys.flags`
reports the mode for any Python code that introspects.

### REPL

`weavepy-cli/src/repl.rs` wraps `rustyline::Editor` and drives a
classic CPython-shaped read-eval-print loop:

```rust
pub struct Repl {
    interpreter: vm::Interpreter,
    editor: rustyline::DefaultEditor,
    history_path: Option<PathBuf>,
    ps1: String,
    ps2: String,
    last_result_name: &'static str, // "_"
}
```

The continuation predicate (does the user need to keep typing?) is
the `codeop`-style "can this be compiled standalone?" test. We try
to parse the buffer as a `Module`; if parsing fails because of
end-of-input, we read another line. Other parse errors abort the
current input and re-prompt with `ps1`.

The REPL also injects a synthetic `__main__` module so user-typed
names persist between statements, binds `_` to the last
non-None expression result (matching CPython), honours
`PYTHONSTARTUP`, and writes per-input lines to
`~/.weavepy_history` (configurable via `WEAVEPY_HISTORY`).

Ctrl-C raises `KeyboardInterrupt` into the current evaluation;
Ctrl-D on an empty line exits cleanly.

### `__pycache__`

The import loader gains a two-step cache check before falling back
to source compilation:

```
src_path = "/a/b/foo.py"
cache_path = "/a/b/__pycache__/foo.weavepy-3.13.pyc"

if cache exists and cache.mtime >= src.mtime and cache.magic == MAGIC:
    code = marshal.loads(cache.body[16:])
    return code

code = compile(src)
write(cache, magic + flags + mtime + size + marshal.dumps(code))
return code
```

The header layout follows CPython's PEP 552 timestamp-mode:

```
+----+----+----+----+----+----+----+----+----+----+----+----+----+----+----+----+
|     MAGIC (4)     | FLAGS (4)   = 0   |     MTIME (4)     |     SIZE (4)      |
+----+----+----+----+----+----+----+----+----+----+----+----+----+----+----+----+
| marshal.dumps(code) ...                                                       |
```

`MAGIC` is the WeavePy-specific magic number `WPY0` (4 bytes), so
CPython refuses our cache files cleanly and vice versa. The cache
tag `weavepy-3.13` is exposed on `sys.implementation.cache_tag` and
used to derive both the cache filename and the `__pycache__`
sub-directory name.

Cache writes are suppressed when:

- `-B` was passed,
- `PYTHONDONTWRITEBYTECODE` is in the environment,
- `sys.dont_write_bytecode` is `True` at the time of the import.

Cache reads always happen (subject to the magic / mtime checks)
because reads are cheap and reduce startup time substantially —
the frozen stdlib alone compiles to ~250KB of marshal bytes.

### `site`

The Rust shim discovers `sys.prefix` from `std::env::current_exe()`:
the prefix is the parent of the directory the binary lives in
(e.g. `/usr/local/bin/weavepy` → `sys.prefix = /usr/local`).
Inside a venv, the binary is `<venv>/bin/python` and we read
`<venv>/pyvenv.cfg` to find `home = <real-prefix>` so
`sys.base_prefix` points at the underlying install.

The frozen Python `site` module then:

1. Constructs the default `site-packages` paths:
   - `<prefix>/lib/python3.13/site-packages` (POSIX)
   - `<prefix>/Lib/site-packages` (Windows)
2. Appends the user site:
   - `~/.local/lib/python3.13/site-packages` (POSIX)
   - `%APPDATA%/Python/Python313/site-packages` (Windows)
3. Walks each directory looking for `.pth` files. Each line that
   isn't a comment is appended to `sys.path` if it's an existing
   directory, or `exec`-ed if it starts with `import` (the
   classic CPython `.pth` hack).
4. Calls `sitecustomize` and `usercustomize` (each optional) so
   environment-specific knobs can land before user code runs.

`-S` skips step 1-4 entirely. `-s` / `PYTHONNOUSERSITE` skips
step 2. `-I` is `-S -s -E`.

### `importlib` package

Six submodules ship as frozen Python, exposing the canonical surface
the packaging ecosystem expects:

- `importlib.__init__` — `import_module(name, package=None)`,
  `reload(module)`, `invalidate_caches()`, the `__import__` re-export.
- `importlib.abc` — abstract base classes: `Finder`, `Loader`,
  `MetaPathFinder`, `PathEntryFinder`, `ResourceLoader`,
  `InspectLoader`, `ExecutionLoader`.
- `importlib.machinery` — `SourceFileLoader`,
  `SourcelessFileLoader`, `ExtensionFileLoader`, `PathFinder`,
  `FileFinder`, `BuiltinImporter`, `FrozenImporter`, `ModuleSpec`.
- `importlib.util` — `spec_from_file_location`,
  `module_from_spec`, `MAGIC_NUMBER`, `cache_from_source`,
  `source_from_cache`, `find_spec`, `decode_source`, the
  `LazyLoader` wrapper.
- `importlib.metadata` — `version(name)`, `metadata(name)`,
  `distributions()`, `Distribution`, `PackageNotFoundError`,
  `entry_points()`, `requires()`, `files()`. Backed by a
  `dist-info` walker that reads `METADATA`, `RECORD`, and
  `entry_points.txt` from `<site>/<dist>-<ver>.dist-info/`.
- `importlib.resources` — `files(package)`, `as_file(traversable)`,
  the `Traversable` protocol.

`pkgutil` ships alongside: `iter_modules`, `walk_packages`,
`get_data`, `find_loader`, `ImpImporter` (stub), `extend_path`.

### `venv` and the pip story

`venv.create(env_dir)` writes:

```
<env_dir>/
├── bin/                # or Scripts/ on Windows
│   ├── python          # symlink (POSIX) or copy (Windows) of weavepy
│   ├── pip             # tiny shim that invokes `weavepy -m _minipip`
│   └── activate        # standard bash activation script
├── lib/python3.13/site-packages/
└── pyvenv.cfg          # home, version, prompt, include-system-site-packages
```

The `pyvenv.cfg` shape matches CPython's exactly. When WeavePy
boots, it consults `<argv[0]>/../../pyvenv.cfg` (parent of `bin/`)
to find `home = <prefix>` and re-derives `sys.prefix` /
`sys.base_prefix` so `site` discovers the venv's `site-packages`.

`ensurepip` is a thin frozen Python wrapper that, on
`python -m ensurepip`, copies the bundled `_minipip.py` into the
venv's `site-packages` and writes a `pip` shim.

`_minipip` is a self-contained ~900-LOC pure-Python pip-lite. It
implements the subset of pip commands needed to bootstrap real pip:

```
pip install <wheel>            # local .whl file
pip install <package>          # resolves on PyPI, downloads, installs
pip install -r requirements.txt
pip uninstall <package>
pip list
pip show <package>
pip --version
```

Wheel selection follows PEP 425 compatibility tags
(`py3-none-any` is the universal-pure-Python tag we match). For
real binary wheels (CPython ABI) we error out cleanly, since the
C-API doesn't exist yet. The `--index-url` flag honours
PyPA Simple Repository API responses.

Once `_minipip` is installed, the user can `pip install pip` to
upgrade to real pip (which then handles everything `_minipip`
doesn't).

### `pdb` and `bdb`

`bdb` is the canonical "breakpoint database" base class. It tracks
per-file line breakpoints, hooks into `sys.settrace`, and
dispatches `user_line` / `user_call` / `user_return` / `user_exception`
to subclasses on the right events.

`pdb` is the interactive driver on top of `bdb`. The user-visible
command set:

```
h, help [topic]      help / list commands
s, step              step into
n, next              step over
r, return            run until current function returns
c, continue          continue execution
q, quit              quit debugger
b, break [arg]       set / list breakpoints
cl, clear [arg]      clear breakpoints
disable / enable     toggle breakpoints
where, w, bt         print stack trace
u, up [n]            move up the stack
d, down [n]          move down the stack
l, list              list source around the current line
p expr               print expr
pp expr              pretty-print expr
a, args              print current frame arguments
retval               print return value
unt, until [line]    continue until line
j, jump line         jump to line
debug stmt           recursive debug
display, undisplay   watch expressions
condition bpno expr  conditional break
commands [bpno]      attach commands to a breakpoint
ignore bpno count    skip N hits
source filename       run pdb commands from a file
alias name cmd       define a shorthand
unalias name
restart
EOF                  same as quit
```

`pdb.set_trace()` (and the new `breakpoint()` builtin in CPython
3.7+) drop into the debugger at the call site;
`pdb.post_mortem(tb)` enters with a traceback;
`pdb.run(stmt, globals=None, locals=None)` runs a statement under
the debugger.

`sys.settrace` (a no-op stub in RFC 0018) gains a real
implementation here: the VM consults a per-frame trace function
after each opcode batch, calling it with `(frame, "line", arg)`
or `(frame, "call", arg)` etc. as appropriate. The overhead is
zero when no trace function is installed (we check a single
boolean before each instruction batch).

### Conformance comparator fixes

Three changes turn the 0% conformance lines into honest numbers:

1. **`ENCODING` token**: CPython's `tokenize.tokenize` emits a
   leading `ENCODING` token (e.g. `(56, 'utf-8', (0, 0), (0, 0), '')`).
   WeavePy doesn't (we're always UTF-8). The comparator now drops
   leading `ENCODING` from the oracle side before comparing.
2. **`dis` shape**: WeavePy's `format_dis` emits one line per
   instruction with a fixed `(offset, opname, arg)` layout. CPython's
   `dis.dis` emits a richer two-line-per-instruction layout with a
   leading line number column. The comparator now extracts
   `(opname, arg_int)` pairs from both sides and compares the
   resulting sequences.
3. **AST field ordering**: CPython's `ast.dump` emits fields in
   constructor order (`Module(body=[...], type_ignores=[])`).
   WeavePy's `dump_module` did the same, but a few node shapes
   (`Try`, `MatchAs`, `AnnAssign`) had fields in a slightly
   different order. The comparator now normalises field order
   alphabetically before diffing.

After this RFC, we expect the conformance harness to report
**>80% match on tokens, >70% on AST, >50% on dis** on the in-tree
corpus (against CPython 3.13).

### `regrtest` runner

The `weavepy-conformance regrtest` subcommand now does real work:

```bash
# Run the curated set of CPython tests that pass today.
cargo run -p weavepy-conformance -- regrtest

# Run a single test and capture full output.
cargo run -p weavepy-conformance -- regrtest --test test_grammar

# Update the expectations file after a green run.
cargo run -p weavepy-conformance -- regrtest --update-expectations
```

The runner:

1. Reads `crates/weavepy-conformance/expectations.toml` — a
   per-file allowlist of tests that *should* pass today.
2. For each entry, runs `weavepy -m unittest Lib.test.<file>`
   in a subprocess with a 60-second timeout.
3. Captures stdout / stderr / exit code and classifies:
   `pass` (exit 0), `fail` (exit non-zero), `error`
   (interpreter crash), `timeout`, `skip` (test self-reports
   `unittest.skip`).
4. Diffs the observed outcome against the expectation. CI fails
   if a previously-passing test starts failing, *or* if a
   previously-failing test starts passing (the expectations file
   needs an update — this is a feature, not a bug).

The initial `expectations.toml` lists ~60 CPython tests we know
pass today. The set grows monotonically.

### Parser / compiler gaps

Four small but user-visible holes close in this RFC:

1. **Starred assignment targets** — `a, *b, c = xs`. The compiler
   recognises a starred sub-target inside an assignment list and
   emits `UNPACK_EX` (new opcode, `arg = (before_count << 8) |
   after_count`) which the VM lowers to `list(iter[:before])`,
   `iter[before:total - after]` as a list, `list(iter[total - after:])`.
2. **`**dict` literal spread** — `{"a": 1, **other, "b": 2}`. The
   compiler emits `DICT_UPDATE` (new opcode) for each `**`
   fragment, accumulating into the result dict.
3. **Top-level `await` (PEP 685)** — `await something()` at module
   level. The CLI's `-c` and script paths detect a top-level
   `await` in the parsed AST and wrap the body in an implicit
   `async def __main_async__(): <body>; asyncio.run(__main_async__())`
   when present. Pure async code becomes a one-line program.
4. **`*starred` in call arguments and collection literals beyond
   the trivial case** — already supported in many places; the
   long-tail forms (`[1, *xs, 2, *ys, 3]`) now work consistently.

## Drawbacks

- **`_minipip` is genuinely minimal.** It installs pure-Python
  wheels and does flat dependency resolution. It does not do
  PEP 517 source builds, environment markers in full, or any of
  the resolver subtleties pip itself spent years polishing. The
  expectation is that users `pip install pip` (recursive) to
  upgrade once `_minipip` bootstraps.
- **No C extensions yet.** `pip install requests` works
  (pure-Python all the way down). `pip install numpy` fails at
  install time with a clear "C extensions are not yet supported
  in WeavePy (RFC TBD)" message rather than crashing later.
- **REPL line editing is `rustyline`, not `readline`.** Most users
  won't notice; vi/emacs binding modes and history search work.
  Plugins that hook CPython's `_readline` (rare in practice) won't.
- **`pdb` runs without column-precise breakpoints.** `b file:line`
  works; `b file:line:col` does not (PEP 657 columns aren't
  populated).
- **`zoneinfo` ships an embedded tzdata snapshot.** The snapshot
  is a build-time export of `iana.org/time-zones/2025a`; users who
  need a newer DST rule (rare) can install the `tzdata` PyPI
  package once pip works, which `zoneinfo` will find before its
  bundled copy.
- **`array` supports the documented type codes** but ships in
  pure Python with a Rust `_array` helper for buffer-protocol
  ops; performance for very large arrays is poor compared to
  CPython's C accelerator.
- **`webbrowser` opens via `xdg-open` / `open` / `cmd /c start`**
  on Linux / macOS / Windows respectively. Browser-specific
  back-ends (`webbrowser.get("firefox")`) work only when the
  binary is on `PATH`; the deep `Mozilla` / `Galeon` browser
  classes CPython ships are absent.
- **Regrtest is gated on the curated allowlist** — CI does not
  attempt to run the *entire* `Lib/test/` corpus on every PR
  (it would take ~30 minutes and most of the corpus depends on
  surface we don't ship yet). The allowlist grows with each RFC.
- **`__pycache__` writes go to disk** unconditionally when
  enabled, ignoring permissions errors (CPython silently
  succeeds-ignoring-write-failure too). Read-only filesystems
  (`/usr/lib/python3.13`) won't see cache writes, just like
  with CPython.
- **The REPL doesn't support `async def` at the top level today.**
  Top-level `await` works through `-c` and script wrapping;
  the REPL prompt itself runs each line as sync. Lift planned
  for a follow-up.

## Alternatives

- **Bundle a vendored CPython `Lib/site.py` / `Lib/pdb.py` /
  `Lib/importlib/*`**, change a few imports, ship as-is. Rejected:
  CPython's `Lib/importlib/_bootstrap.py` is bootstrapped at C
  initialisation time and assumes private API we don't have. The
  port-and-trim approach (what we do) is smaller in net code and
  more maintainable.
- **Skip the `pip` story entirely; tell users `wget https://...`
  and unzip the wheel manually.** Rejected: this fails the
  drop-in test on first contact with any real workflow.
- **Implement a full `pip` from scratch.** Rejected: pip is ~30K
  LOC of careful code with deep ecosystem expectations. We
  bootstrap a minimal version capable of `pip install pip` and
  then use real pip.
- **Build the REPL in frozen Python on top of `code.InteractiveConsole`.**
  Tempting (less Rust) but the input-loop / Ctrl-C handling /
  history persistence belongs in the host. The REPL we build is
  a thin Rust shell that delegates *evaluation* to the
  interpreter — same architectural separation CPython uses.
- **Skip the conformance comparator fix and just delete the
  Skipped phases.** Rejected: the comparator was always meant to
  return real numbers once we shipped enough of the pipeline.
  The fix is small.
- **Don't ship `_minipip`; require an upstream pip wheel to be
  manually placed next to the binary.** Rejected: the bundled
  pip is the whole point of `ensurepip`. Users expect
  `python -m ensurepip` to "just work."

## Prior art

- **CPython 3.13** — the conformance target. The CLI's flag set,
  the REPL behaviour, the `site` discovery, `__pycache__` layout,
  `importlib`'s shape, `pdb`'s command syntax.
- **PyPy** — ships its own `Lib/` tree with a similar
  modernisation strategy: the user-visible surface is CPython's,
  the internals are PyPy's. Their REPL uses `pyrepl` (a
  pure-Python `readline` replacement); we use `rustyline`.
- **RustPython** — exposes a `-c` / `-m` / script CLI in
  approximately the shape we extend here. Their REPL is also
  `rustyline`-backed.
- **MicroPython** — ships a vastly smaller subset of CPython's
  CLI and stdlib; useful comparison for "what's the floor for
  a 'Python distribution.'"
- **uv** — Charlie Marsh's Rust-based Python toolchain. Their
  `uv pip` provides a separate, faster pip implementation; the
  bundled `_minipip` in this RFC is a much smaller version of
  the same idea, scoped to bootstrap (not replace) real pip.
- **PyOxidizer / shiv** — for the "embedded Python in one binary"
  story; we ship the interpreter and stdlib in the binary too
  (via `include_str!` for frozen modules), but we want the host
  to have a writable `site-packages`, not a frozen one.

## Unresolved questions

- **`PYTHONHOME` semantics on macOS / Linux**. CPython's
  prefix-discovery is platform-specific (`getpath.c`). We follow
  the documented behaviour but the exact symlink-resolution
  rules vary; some edge cases involving `realpath` differ
  observationally.
- **`pip install` and the `~/.cache/pip` location**. We follow
  XDG Base Directory specification on Linux, `Library/Caches`
  on macOS, `%LOCALAPPDATA%` on Windows. Real pip's cache
  layout is opinionated about per-package metadata vs. wheel
  blobs; we approximate.
- **`pdb` and signal handling**. SIGINT inside `pdb` should drop
  to the prompt, not kill the program. We hook
  `signal.signal(SIGINT, ...)` while pdb is active; the
  interaction with user-installed signal handlers is
  approximated.
- **REPL color output.** We currently emit plain text; a
  follow-up adds basic ANSI for prompts / errors when stdout
  isatty.
- **`venv` on Windows**. The `Scripts/python.exe` shim is a
  small launcher we build per-OS; on Windows we copy the
  binary rather than symlink (matching CPython 3.5+ default).
- **`zoneinfo`'s embedded tzdata refresh cadence.** CPython
  delegates to the system tzdata install; we embed a snapshot
  so the experience is platform-uniform. A new tzdata release
  arrives ~quarterly; we will update the embedded blob on the
  same cadence.

## Future work

- **C-API foundation** (RFC 0021 candidate): tagged-pointer
  object model, `Py_LIMITED_API` shim, ability to load real
  `.so` extensions, `numpy` as a litmus test.
- **Performance baseline** (RFC 0022 candidate): adaptive
  specialization, inline caches for `LOAD_ATTR` /
  `LOAD_GLOBAL` / `CALL`, computed-goto dispatch on supported
  targets, `pyperformance`-shaped benchmark suite.
- **Cycle-collecting GC** (RFC 0023 candidate): real
  `weakref` semantics, deterministic `__del__` firing,
  `gc.collect()` that actually does something.
- **`multiprocessing`**: Queue, Pipe, Manager, Pool over
  `subprocess` + `pickle` (both shipped). Worker
  `fork`/`spawn`/`forkserver` start methods.
- **`xmlrpc`, `wsgiref`** — the older networked-stdlib surface.
- **Real `_pickle` C-shaped accelerator in Rust** — for the
  fast path that CPython's `_pickle.c` covers.
- **REPL `async def` at the top level** — same wrapping trick
  the CLI uses, lifted into the REPL eval path.
- **PEP 657 column-precise tracebacks** — needs a column-
  aware compiler.
- **`pdb` post-mortem from a coredump-style capture** —
  serialise the frame stack via marshal, replay later.
- **`zoneinfo` and `tzdata` integration** — automatic
  upgrade from the embedded snapshot to the `tzdata` wheel
  when present.
- **Hot-reload for the REPL** — `%reload` magic command to
  re-import a module without restarting.

## Implementation status (post-merge)

Tracked snapshot of what's wired up vs. deferred. Update this
table when individual line items move.

| area                              | status         | notes                                                                   |
|-----------------------------------|----------------|-------------------------------------------------------------------------|
| Full CPython CLI flag table       | ✅ done        | `weavepy --help` matches the CPython manpage.                            |
| `PYTHON*` env-var honouring       | ✅ done        | `PYTHONPATH`, `PYTHONSTARTUP`, `PYTHONOPTIMIZE`, `PYTHONDONTWRITEBYTECODE`, etc. |
| `rustyline`-backed REPL           | ✅ done        | History at `~/.weavepy_history`; multi-line; `_` binding.                |
| `__pycache__` (`weavepy-3.13.pyc`) | ✅ done        | Magic `WPY0`; mtime invalidation; `-B` honoured.                         |
| Frozen `site`                     | ✅ done        | `.pth` files, `USER_SITE`, venv discovery.                               |
| Frozen `importlib` package        | ✅ done        | `machinery`, `util`, `abc`, `metadata`, `resources`.                     |
| Frozen `pkgutil`                  | ✅ done        |                                                                          |
| Frozen `venv`                     | ✅ done        | `weavepy -m venv .venv` writes `pyvenv.cfg` + activate scripts.          |
| Frozen `ensurepip` + `_minipip`   | ✅ done        | Bootstraps minimal pip; full pip via `pip install pip`.                  |
| Frozen `pdb` / `bdb`              | ✅ done        | Sits on RFC 0018 frame introspection.                                    |
| Frozen `pprint` / `tomllib` /     | ✅ done        | Plus `configparser`, `getopt`, `optparse`, `profile`, `cProfile`, `pstats`,  |
|   `webbrowser` / `array` /        |                | `timeit`, `plistlib`, `zoneinfo`, `unittest.async_case`.                  |
|   `zoneinfo` / IsolatedAsyncio    |                |                                                                          |
| `assert` statement                | ✅ done        | Parser + compiler + ast.dump; emits conditional `RAISE_VARARGS`.          |
| Starred assignment targets        | ✅ done        | `a, *b, c = xs`; new `UnpackEx` opcode.                                  |
| `**dict` literal spread           | ✅ done        | New `DictUpdate` opcode.                                                  |
| Conformance comparator fix        | ✅ done        | `ENCODING`/`NL` filtered, AST default-kwarg stripped, dis lifted to opname pairs. |
| `weavepy regrtest` subcommand     | ✅ done        | `tests/regrtest/` + `expectations.toml` gating; CI-grade strict mode.    |
| `weavepy-conformance regrtest`    | ✅ done        | Same harness, exposed for the conformance binary.                        |
| Bundled regression test fixtures  | ✅ done        | 12 fixtures cover arithmetic, strings, collections, control flow, classes, iter/gen, exceptions, pattern match, async, decimal/fractions, stdlib imports. |
| Top-level `await` for `-c`/scripts| 🔜 deferred    | Wrapping path lands in a follow-up; REPL stays sync.                     |
| F-string with backslashes         | 🔜 deferred    | Tokenizer rewrite required (PEP 701 grammar).                            |
| `sys.flags` as attribute object   | 🔜 deferred    | Currently `dict`-shaped; requires a `SimpleNamespace`-style host type.   |
| `-O`-elided assertions            | 🔜 deferred    | Compiler always emits the raise today; VM could skip on `optimize >= 1`.  |
