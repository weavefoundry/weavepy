"""A module that reads `__spec__` or `__loader__` as a global name, in its
body or in a function, sees its own (not the `builtins` module's, where a
name its dict lacks is looked up next), whether or not `importlib` has been
imported yet and whether it loads from source or from its bytecode cache
(pydantic's `__init__` computes its lazy imports from `__spec__.parent`)."""

import os
import shutil
import subprocess
import sys
import tempfile

PACKAGE = {
    "pkg/__init__.py": """
from importlib import import_module
PARENT = __spec__.parent
NAME = __spec__.name
_lazy = {"thing": (__spec__.parent, ".main")}
def __getattr__(attr):
    package, module_name = _lazy[attr]
    return getattr(import_module(module_name, package=package), attr)
""",
    "pkg/main.py": """
thing = 42
NAME = __spec__.name
def describe():
    return __spec__.parent, __package__, __loader__ is not None
""",
    "pkg/sub/__init__.py": """
PARENT = __spec__.parent
LOCATIONS = __spec__.submodule_search_locations is not None
""",
    "plain.py": """
def spec_name():
    return __spec__.name
LOADER = __loader__
""",
}

SCRIPT = """
import sys
early = "importlib.machinery" in sys.modules
import pkg, plain
assert (pkg.PARENT, pkg.NAME) == ("pkg", "pkg"), (pkg.PARENT, pkg.NAME)
assert pkg.thing == 42
import pkg.main, pkg.sub
assert pkg.main.NAME == "pkg.main"
assert pkg.main.describe() == ("pkg", "pkg", True), pkg.main.describe()
assert (pkg.sub.PARENT, pkg.sub.LOCATIONS) == ("pkg.sub", True)
assert plain.spec_name() == "plain"
assert plain.LOADER is plain.__loader__ is not None
print("ok")
"""

root = tempfile.mkdtemp()
try:
    for name, source in PACKAGE.items():
        path = os.path.join(root, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(source)
    # Twice: from source, then from the bytecode cache the first run wrote.
    for _ in range(2):
        for prelude in ("", "import importlib.machinery\n"):
            out = subprocess.run(
                [sys.executable, "-c", prelude + SCRIPT],
                cwd=root,
                capture_output=True,
                text=True,
            )
            assert out.returncode == 0 and out.stdout.strip() == "ok", out.stderr
finally:
    shutil.rmtree(root, ignore_errors=True)
