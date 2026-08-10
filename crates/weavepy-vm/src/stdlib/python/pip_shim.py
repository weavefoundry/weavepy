"""The frozen ``pip`` module (RFC 0055 WS2).

WeavePy ships a pip-compatible installer in the binary (``_minipip``,
RFC 0030) so ``weavepy -m pip install requests`` works out of the box.
But a *frozen* pip must not repeal CPython's environment semantics:

* ``python -m venv --without-pip`` creates an environment where
  ``import pip`` raises ``ModuleNotFoundError`` — tools probe for
  exactly that (``test_venv.EnsurePipTest.assert_pip_not_installed``).
* A pip installed into site-packages (``ensurepip``'s bundled wheel,
  or a future ``pip install --upgrade pip``) must win over the frozen
  copy, and ``pip.__file__``/``pip --version`` must report the
  installed location.

So the frozen module is a dispatcher, resolved at import time:

1. An installed distribution on ``sys.path`` (``pip/__init__.py`` or
   ``pip.py``) is executed in this module's namespace, with
   ``__file__`` (and ``__path__`` for the package form) pointing at
   the installed copy.
2. Otherwise, inside a virtual environment (``sys.prefix !=
   sys.base_prefix``) the import *fails*, exactly like CPython.
3. Otherwise (the base environment), the embedded ``_minipip``
   implementation runs — the batteries-included default.
"""

import os as _os
import sys as _sys


def _find_installed():
    """First `pip` distribution on sys.path.

    Returns ``(filename, source, is_package)``; ``(None, None, False)``
    when nothing is installed. Zip entries (``ensurepip`` prepends the
    bundled wheel itself to ``sys.path``) are searched too.

    The materialized stdlib tree is on ``sys.path`` (it is CPython's
    ``{prefix}/lib/pythonX.Y``) and contains this very facade as
    ``pip.py`` — skip it, or the dispatcher would exec itself forever.
    """
    _self_dir = None
    try:
        _self_dir = _os.path.dirname(_os.path.abspath(__file__))
    except NameError:
        pass
    for _entry in _sys.path:
        if not _entry:
            continue
        if _self_dir is not None and _os.path.abspath(_entry) == _self_dir:
            continue
        if _os.path.isdir(_entry):
            _pkg = _os.path.join(_entry, 'pip', '__init__.py')
            if _os.path.isfile(_pkg):
                with open(_pkg, 'r', encoding='utf-8') as _f:
                    return _pkg, _f.read(), True
            _mod = _os.path.join(_entry, 'pip.py')
            if _os.path.isfile(_mod):
                with open(_mod, 'r', encoding='utf-8') as _f:
                    return _mod, _f.read(), False
        elif _os.path.isfile(_entry):
            import zipfile
            try:
                with zipfile.ZipFile(_entry) as _zf:
                    _names = set(_zf.namelist())
                    for _name, _is_pkg in (('pip/__init__.py', True),
                                           ('pip.py', False)):
                        if _name in _names:
                            _data = _zf.read(_name).decode('utf-8')
                            return (_os.path.join(_entry, _name),
                                    _data, _is_pkg)
            except (OSError, zipfile.BadZipFile):
                pass
    return None, None, False


_installed, _source, _is_package = _find_installed()

if _installed is not None:
    __file__ = _installed
    if _is_package:
        __path__ = [_os.path.dirname(_installed)]
    _code = compile(_source, _installed, 'exec')
    del _source, _installed, _is_package, _find_installed
    exec(_code)
elif _sys.prefix != getattr(_sys, 'base_prefix', _sys.prefix):
    # A virtual environment without pip installed: honour venv
    # semantics rather than leaking the embedded copy in.
    raise ModuleNotFoundError("No module named 'pip'", name='pip')
else:
    from _minipip import *  # noqa: F401,F403
    from _minipip import main, __version__, __doc__  # noqa: F401
    import _minipip as _impl

    # Keep the materialized file identity (`pip.__file__` exists on
    # disk courtesy of the stdlib tree); expose the same module dict
    # surface `_minipip` has for introspection-happy callers.
    if __name__ == '__main__':
        _sys.exit(main())
