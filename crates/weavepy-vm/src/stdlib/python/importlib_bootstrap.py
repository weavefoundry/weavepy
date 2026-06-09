"""``importlib._bootstrap`` — WeavePy façade.

In CPython this is the frozen core of the import system; ``importlib``
itself aliases it (``importlib._bootstrap = _bootstrap``). WeavePy's
import core lives in Rust, so this module exposes the handful of
bootstrap entry points stdlib code reaches for directly (notably
``pydoc.importfile`` calling ``_bootstrap._load(spec)``), implemented
over the same spec/loader machinery as ``importlib.util``.
"""

import sys

from importlib.util import module_from_spec as _module_from_spec

__all__ = ['_load', 'spec_from_loader', 'ModuleSpec']

from importlib.util import spec_from_loader
from importlib.machinery import ModuleSpec


def _load(spec):
    """Create, register, and execute the module described by *spec*.

    Mirrors CPython's `_bootstrap._load`: the module is inserted into
    ``sys.modules`` *before* execution (so circular imports during exec
    see the partial module) and removed again if execution fails.
    """
    module = _module_from_spec(spec)
    sys.modules[spec.name] = module
    try:
        if spec.loader is not None:
            spec.loader.exec_module(module)
    except BaseException:
        try:
            del sys.modules[spec.name]
        except KeyError:
            pass
        raise
    # An import hook may have replaced the entry; honour what's there,
    # like CPython does.
    return sys.modules.get(spec.name, module)
