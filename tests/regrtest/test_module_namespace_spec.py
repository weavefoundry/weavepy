"""A module loaded from a file has `__spec__` and `__loader__` in its
namespace however the namespace is reached (`vars`, `__dict__`,
`globals()` in its body or its functions, `dir`), in a program that has
not imported `importlib.machinery` itself (`-c`), as well as one that has
(which also loads the modules from their cached bytecode)."""

import os
import subprocess
import sys
import tempfile

MODULES = {
    "ns_body": "X = 1\nG = globals()\nSNAP = dict(globals())\nL = sorted(locals())\nD = dir()\n",
    "ns_vars": "X = 2\n",
    "ns_dict": "X = 3\n",
    "ns_dir": "X = 4\n",
    "ns_fn": "X = 5\ndef f():\n    return '__spec__' in globals(), '__loader__' in globals()\n",
    "ns_attr": "X = 6\n",
}

PROGRAM = r"""
import sys
sys.path.insert(0, {path!r})
{preamble}
import ns_body, ns_vars, ns_dict, ns_dir, ns_fn, ns_attr
assert "__spec__" in ns_body.G and "__loader__" in ns_body.G
assert ns_body.G is vars(ns_body)
assert "__spec__" in ns_body.SNAP and "__loader__" in ns_body.SNAP
assert "__spec__" in ns_body.L and "__spec__" in ns_body.D
assert "__spec__" in vars(ns_vars) and "__loader__" in vars(ns_vars)
assert ns_dict.__dict__["__spec__"].name == "ns_dict"
assert ns_dict.__dict__["__loader__"] is ns_dict.__spec__.loader
assert "__spec__" in dir(ns_dir) and "__loader__" in dir(ns_dir)
assert ns_fn.f() == (True, True)
assert ns_attr.__spec__.name == "ns_attr" and "__spec__" in vars(ns_attr)
assert ns_attr.__spec__.origin.endswith("ns_attr.py")
assert type(ns_attr.__loader__).__name__ == "SourceFileLoader"
print("ok")
"""


def main():
    with tempfile.TemporaryDirectory() as d:
        for name, source in MODULES.items():
            with open(os.path.join(d, name + ".py"), "w") as f:
                f.write(source)
        for preamble in ("", "import importlib.machinery"):
            program = PROGRAM.format(path=d, preamble=preamble)
            out = subprocess.run(
                [sys.executable, "-c", program], capture_output=True, text=True, timeout=120
            )
            assert out.returncode == 0 and out.stdout.strip() == "ok", (preamble, out.stderr[-2000:])


main()
print("ok")
