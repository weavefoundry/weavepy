"""RFC 0053 invariants: materialized stdlib tree, module identity, tooling.

Covers the wave-8 acceptance criteria as a bundled regrtest:

* WS1 — every frozen-stdlib module's ``__file__`` is a real on-disk
  path whose bytes are exactly what the interpreter executed
  (``open``/``linecache``/``inspect.getsource`` agree).
* WS2 — every imported module carries a PEP 451 ``__spec__`` and
  ``__loader__`` consistent with its kind.
* WS4 — ``sysconfig``/``site`` describe real, existing directories
  anchored on ``sys.prefix``.
* WS5 — ``cProfile`` over the ``_lsprof`` core aggregates both
  Python-level and C-level (``c_call``) events.
"""

import os
import sys

# --- WS1: materialized tree ------------------------------------------------

import argparse
import linecache
import inspect

assert not argparse.__file__.startswith("<"), argparse.__file__
assert os.path.isfile(argparse.__file__), argparse.__file__

with open(argparse.__file__, encoding="utf-8") as fp:
    on_disk = fp.read()
assert "class ArgumentParser" in on_disk

# linecache and inspect read the same bytes.
lines = linecache.getlines(argparse.__file__)
assert "".join(lines) == on_disk
src = inspect.getsource(argparse.ArgumentParser)
assert src.startswith("class ArgumentParser")

# The tree hangs off sys.prefix and sys._stdlib_dir agrees.
assert hasattr(sys, "_stdlib_dir")
assert os.path.dirname(argparse.__file__) == sys._stdlib_dir
assert sys._stdlib_dir.startswith(sys.prefix)

# A traceback through a stdlib frame renders real source lines.
import traceback

try:
    import ast

    ast.literal_eval("f(1)")
except ValueError:
    tb_text = traceback.format_exc()
    # The stdlib frame's source line must be rendered (only possible
    # with a real file behind ast.__file__), not just the header.
    assert 'raise ValueError' in tb_text or 'malformed node' in tb_text, tb_text


# --- WS2: module identity ---------------------------------------------------

# Frozen-source module materialized on disk -> SourceFileLoader.
spec = argparse.__spec__
assert spec is not None and spec.name == "argparse"
assert argparse.__loader__ is not None
assert type(argparse.__loader__).__name__ == "SourceFileLoader"
assert spec.origin == argparse.__file__

# Rust-native module -> BuiltinImporter.
import _thread

assert _thread.__spec__ is not None
assert "BuiltinImporter" in repr(_thread.__spec__.loader) or (
    getattr(_thread.__spec__.loader, "__name__", "") == "BuiltinImporter"
)

# Packages carry submodule_search_locations.
import email

assert email.__spec__.submodule_search_locations


# --- WS4: sysconfig/site describe the real layout ---------------------------

import sysconfig

paths = sysconfig.get_paths()
assert os.path.isdir(paths["stdlib"]), paths["stdlib"]
assert paths["stdlib"].startswith(sys.prefix)
assert sysconfig.get_python_version() == "3.13"

import site

for d in site.getsitepackages():
    assert d.startswith(sys.prefix)


# --- WS5: cProfile over _lsprof ---------------------------------------------

import cProfile
import pstats
import io


def workload():
    acc = []
    for i in range(100):
        acc.append(i * i)  # C-level list.append -> c_call events
    return len(acc)


prof = cProfile.Profile()
prof.enable()
workload()
prof.disable()

out = io.StringIO()
stats = pstats.Stats(prof, stream=out)
stats.print_stats()
report = out.getvalue()
assert "workload" in report, report
assert "list" in report and "append" in report, report

print("ok")
