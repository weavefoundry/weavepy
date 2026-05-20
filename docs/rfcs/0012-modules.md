# RFC 0012: Modules and the import system

- **Status**: Accepted
- **Authors**: WeavePy authors
- **Created**: 2026-05-20
- **Tracking issue**: TBD

## Summary

Wire the `import` statement end-to-end. After this RFC lands:

- `import name`, `import a.b`, `import a as x`, `from m import n`,
  `from m import n as x`, `from m import *`, and the relative forms
  `from . import x` / `from ..pkg import y` all execute.
- A new `Object::Module` runtime value represents a module; attribute
  access reads `module.__dict__`, the canonical Python data model.
- A tiny **stdlib bootstrap** ships with the interpreter: `sys`,
  `math`, `os`, and `os.path`. These are registered as built-in
  modules and are sufficient for most simple scripts.
- `sys.argv`, `sys.path`, `sys.modules`, `__name__`, and `__main__`
  are wired through the embedding API and the CLI so `weavepy
  script.py arg1 arg2` behaves like `python script.py arg1 arg2`.
- A path-based loader executes user `.py` files and package
  `__init__.py` files, caching the resulting modules in `sys.modules`
  exactly the way CPython does.

This RFC retires the `CompileError::NotImplemented("import", …)`
guard introduced in RFC 0001 and closes the largest single hole in
the slice: every nontrivial Python program imports something.

## Motivation

After RFC 0004 the interpreter could run single-file programs that
defined their own classes, raised their own exceptions, and managed
their own resources. That's enough for toy scripts, but it's
nowhere near "drop-in replacement for CPython": the first thing
almost every real Python file does is `import` something.

Concretely, without imports we cannot:

- Run any multi-file program. Even a two-file script that splits
  utilities into `helpers.py` and a driver into `main.py` fails on
  the first `from helpers import …`.
- Touch any standard-library functionality. Every `random.choice`,
  `os.environ.get`, `json.loads`, or `argparse.ArgumentParser`
  starts with an import.
- Run CPython's own test-runner stub or `regrtest`-style harness.
  Both depend on `unittest`, which imports `sys`, `os`, `traceback`,
  and more before doing anything.

A working module system also unblocks downstream work:

- The conformance harness (today single-file) can graduate to
  multi-file packages.
- Once `sys` exists, generators (RFC 0006) and bignum (RFC 0008) can
  expose runtime knobs (`sys.getrecursionlimit`, `sys.maxsize`)
  through it instead of inventing parallel APIs.
- An eventual stdlib bootstrap that vendors pure-Python CPython
  modules (`fractions.py`, `bisect.py`, `enum.py`) needs nothing
  else than a working loader to start running.

## CPython reference

This RFC tracks **CPython 3.13**:

- `Lib/importlib/_bootstrap.py` — the canonical pure-Python
  implementation of the import machinery, including finders,
  loaders, `sys.modules` caching, and relative-import resolution.
- `Python/import.c` — the C side of `__import__`, including
  `PyImport_ImportModule` and the package `__path__` walk.
- `Python/compile.c` — emission of `IMPORT_NAME` / `IMPORT_FROM` /
  `IMPORT_STAR`, including how `level` and `fromlist` are passed.
- `Objects/moduleobject.c` — the `module` type and its attribute
  layout (`__name__`, `__file__`, `__package__`, `__dict__`,
  `__loader__`, `__spec__`).
- Language reference, "The import system" chapter — the spec for
  relative imports, package `__init__.py` execution, and the
  semantics of `from X import *` with and without `__all__`.

We do **not** track:

- PEP 451 import specs and their full `Finder` / `Loader` Python
  surface. The loader is internal to the VM here; RFC 0013 will
  expose `importlib.abc.Loader` so Python code can plug in custom
  finders.
- `.pyc` bytecode caching. We always re-parse the source. CPython's
  cache layout is well documented and is a perf-oriented follow-up.
- Namespace packages (PEP 420). Every directory we treat as a
  package must contain `__init__.py`. RFC 0013 will lift this.
- Zip imports, frozen modules, and the `_bootstrap_external` path.
- `importlib.reload`. Modules are loaded once; subsequent imports
  return the cached object.

## Detailed design

### Bytecode

Three new opcodes, mirroring CPython 3.13's `Lib/opcode.py`:

- `IMPORT_NAME(arg)` — pops `fromlist` and `level` (in that order
  from TOS down), looks up `co_names[arg]` as the module name, and
  pushes the loaded module. Whether the *top-level* or the *leaf*
  module is pushed depends on `fromlist`:
  - empty `fromlist`: push the top-level module (so `import a.b`
    binds `a` in the current scope; `a.b` is then reached through
    `a.b` attribute resolution).
  - non-empty `fromlist`: push the *leaf* module (so
    `from a.b import x` binds `x` from the leaf, not from `a`).
- `IMPORT_FROM(arg)` — peeks the module on TOS and pushes
  `module.<co_names[arg]>`, raising `ImportError` if not present.
- `IMPORT_STAR` — pops the module and writes every public name from
  its `__dict__` into the current frame's locals (function scope)
  or globals (module scope). Honours `__all__` when present.

### Compile shapes

Mirroring CPython:

```python
import os
```

```
LOAD_CONST 0      # level
LOAD_CONST None   # fromlist
IMPORT_NAME os
STORE_NAME os
```

```python
import os.path as p
```

```
LOAD_CONST 0
LOAD_CONST None
IMPORT_NAME os.path
STORE_NAME p
```

```python
from os import path, sep
```

```
LOAD_CONST 0
LOAD_CONST ('path', 'sep')
IMPORT_NAME os
IMPORT_FROM path
STORE_NAME path
IMPORT_FROM sep
STORE_NAME sep
POP_TOP             # discard the module
```

```python
from os import *
```

```
LOAD_CONST 0
LOAD_CONST ('*',)
IMPORT_NAME os
IMPORT_STAR
```

Relative imports encode `level` as the count of leading dots:

```python
from . import sibling      #   level=1, module=None
from ..pkg import x         #   level=2, module='pkg'
```

### `Object::Module`

```rust
pub struct PyModule {
    pub name: String,
    pub filename: Option<String>,
    pub dict: Rc<RefCell<DictData>>,
}
```

Attribute access (`module.x`) reads `dict["x"]`, raising
`AttributeError` on miss with the CPython-shaped message
`module '<name>' has no attribute '<x>'`.

`Object::Module(Rc<PyModule>)` joins the object enum. Modules are
cheap to clone (`Rc` bump). `is` is `Rc::ptr_eq`. The `module` type
exposes `__name__`, `__file__`, `__dict__`, `__loader__` (a stub for
now), and `__package__` (best-effort).

### The import machinery

The `Interpreter` owns a `ModuleCache`:

```rust
pub struct ModuleCache {
    /// `sys.modules` — every loaded module, keyed by full dotted name.
    pub modules: Rc<RefCell<DictData>>,
    /// `sys.path` — list of directories searched for `.py` files.
    /// Shared by-Rc with the `sys` module's `.path` attribute, so
    /// `sys.path.append("…")` from Python code is visible to the loader.
    pub path: Rc<RefCell<Vec<Object>>>,
    /// Registered built-in module factories (Rust-defined).
    pub builtins: HashMap<&'static str, BuiltinModuleFactory>,
}
```

The factory signature lets a module close over the interpreter
state it needs:

```rust
type BuiltinModuleFactory = fn(&ModuleCache) -> Rc<PyModule>;
```

`import_module(name, fromlist, level, current_globals)` resolves
imports in three steps:

1. **Resolve relative.** If `level > 0`, walk up `level - 1` package
   levels starting from `current_globals.__package__` (or
   `__name__`, stripped of its last component) and prepend the
   resolved prefix to `name`. Raise `ImportError("attempted
   relative import beyond top-level package")` on overshoot.
2. **Load each part.** Walk the dotted name (`a.b.c`):
   - If `sys.modules[full_so_far]` exists, use it.
   - Else, if `full_so_far` is in the built-in registry, instantiate
     it via the factory and store in `sys.modules`.
   - Else, search `sys.path` for `full_so_far_leaf.py` or
     `full_so_far_leaf/__init__.py` (treating dots as path
     separators). Execute the file in a fresh module's `dict`.
     Cache.
   - For non-top-level parts, set the parent module's attribute
     `parent.<leaf>` to the loaded module (matches CPython).
3. **Return.** With `fromlist` empty: return the *top-level*
   module. Otherwise: return the *leaf* module.

### Stdlib bootstrap

Three built-in modules ship in this RFC, each as a Rust factory:

- **`sys`** (`stdlib/sys.rs`): `argv`, `path`, `modules`, `version`,
  `version_info`, `platform`, `executable`, `exit()`,
  `getrecursionlimit()`, `setrecursionlimit()`,
  `maxsize`, `byteorder`. Output streams (`stdout`/`stderr`) are
  not exposed as Python file objects yet; deferred to RFC 0014.
- **`math`** (`stdlib/math.rs`): `pi`, `e`, `tau`, `inf`, `nan`,
  `sqrt`, `pow`, `exp`, `log`, `log2`, `log10`, `sin`, `cos`, `tan`,
  `asin`, `acos`, `atan`, `atan2`, `floor`, `ceil`, `trunc`, `fabs`,
  `gcd`, `lcm`, `factorial`, `isnan`, `isinf`, `isfinite`,
  `copysign`, `fmod`, `radians`, `degrees`.
- **`os`** (`stdlib/os.rs`): `getcwd`, `environ` (a `dict`), `sep`,
  `linesep`, `name`, `getenv`, plus `os.path` as a sub-module.
- **`os.path`** (also `stdlib/os.rs`): `join`, `split`, `splitext`,
  `basename`, `dirname`, `exists`, `isfile`, `isdir`, `abspath`,
  `normpath`, `sep`.

Anything beyond this — `io`, `time`, `random`, `json`, `re`,
`itertools` — is deliberately out of scope. They will come in a
follow-up RFC (or a vendored pure-Python bootstrap) once we want to
trade scope for surface area.

### CLI integration

`weavepy script.py a b c` now:

1. Constructs the interpreter.
2. Prepends `dirname(script.py)` (or `cwd` for `-c` and stdin) to
   `sys.path`.
3. Binds `sys.argv = ["script.py", "a", "b", "c"]`.
4. Sets `__name__ = "__main__"` and `__file__ = "script.py"` in
   the module's globals.

`weavepy -m pkg.mod` (deferred to follow-up): would resolve
`pkg.mod` via the same import machinery and run it under
`__name__ = "__main__"`. Out of scope for this RFC.

### Error mapping

- Missing module: `ModuleNotFoundError` (a subclass of
  `ImportError`, added to the built-in exception hierarchy in this
  RFC).
- Missing name on import: `ImportError("cannot import name 'X'
  from 'Y'")`.
- Beyond-top-level relative import: `ImportError("attempted
  relative import beyond top-level package")`.
- Circular import where the inner attribute lookup happens before
  the outer module finished initialising: `ImportError("cannot
  import name 'X' (most likely due to a circular import)")` —
  matches CPython's hint phrasing.

`ImportError` carries `name` and `path` attributes per the data
model. `ModuleNotFoundError` inherits both.

## Drawbacks

- **No namespace packages.** Every directory in the import path
  must contain `__init__.py` to count as a package. CPython has
  supported PEP 420 namespace packages since 3.3. We accept the
  gap because every popular stdlib module is a regular package;
  the few namespace-package consumers (mostly large monorepo
  layouts) can wait for RFC 0013.
- **No `.pyc` cache.** Every import re-parses the source. Fine for
  the slice's perf level; revisit after the bytecode-compaction RFC.
- **Module attribute writes from Rust are non-atomic.** A built-in
  module could in principle be observed mid-construction. Today no
  Python code can run during a built-in module's factory, so this
  is theoretical. Worth fixing if we add reentrancy.
- **`stdout` / `stderr` are not Python file objects.** The CLI and
  fixture harness still own the sink directly. `sys.stdout.write`
  is therefore unsupported until RFC 0014 (`io` module + Python
  file objects).

## Alternatives

- **Skip the loader; vendor a pure-Python `_bootstrap`.** CPython
  itself bootstraps imports from a freeze of `importlib`. We chose
  not to because (a) freezing requires a working interpreter to
  produce, (b) we want full control over error messages and
  diagnostics, and (c) the surface needed by the slice is small.
- **Implement only built-in modules; no file loader.** Rejected.
  Multi-file user programs are exactly the customer here, and a
  built-in-only system can't run them.
- **Land f-strings (RFC 0005) first.** They're a smaller diff and
  more visible, but they're a papercut. Imports are *blocking* —
  this is the right next thing.

## Prior art

- CPython 3.13's `importlib._bootstrap` (the design we follow).
- RustPython's `vm/src/import.rs` and its `Frozen` / `LibCabi`
  module loaders — close in spirit but trades a different set of
  invariants on `sys.modules`.
- MicroPython's `py/builtinimport.c` — a much smaller import
  implementation for a similar slice scope; useful comparison point
  for "what's the minimum to ship and feel like Python."

## Unresolved questions

- **`__main__` reuse.** CPython gives every interpreter a single
  module named `__main__` that survives between successive
  `eval()`s. Our `run_source` creates a fresh module dict each
  call. For embedding REPL-like consumers this should match CPython
  eventually; left for the REPL RFC.
- **Build-in `os.environ` mutation.** CPython mirrors environ
  writes back to the host process via `setenv`. We bind once at
  startup and let writes only affect the Python-visible dict.
  Tracked.
- **Module `__path__`.** Packages get a `__path__` list whose first
  entry is the package directory. We populate it as a list of
  `str`; CPython actually uses a magic `_NamespacePath` for
  namespace packages, irrelevant here.

## Future work

- **RFC 0013**: importlib finders/loaders Python surface, namespace
  packages (PEP 420), `__path__` hooks, `importlib.reload`.
- **RFC 0014**: `io` module + Python file objects for
  `sys.stdout`/`sys.stderr`.
- **RFC 0015**: stdlib bootstrap — vendor and run a curated set of
  pure-Python CPython stdlib modules under WeavePy.
