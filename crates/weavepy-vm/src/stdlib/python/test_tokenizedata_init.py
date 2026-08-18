# Vendored CPython test fixture package (`test.tokenizedata`).
#
# CPython's `Lib/test/tokenizedata/` holds intentionally-malformed source
# files used by the lexer/tokenizer regression tests. `test_unicode_identifiers`
# imports `badsyntax_3131` to assert the exact `SyntaxError` raised for an
# invalid PEP 3131 identifier. The package `__init__` is empty upstream.
#
# WeavePy carries only `badsyntax_3131` in the materialized stdlib tree;
# the rest of the fixtures (`bad_coding`, `coding20731`, the tokenize_tests
# data files) live in the vendored CPython Lib the suite runs from. Graft
# every on-disk `tokenizedata` under the `test` package's __path__ onto
# ours so `import test.tokenizedata.bad_coding` (test_source_encoding's
# verify_bad_module) and `findfile`-style lookups resolve there too.
import os as _os
import sys as _sys

try:
    _test_pkg = _sys.modules["test"]
    for _entry in getattr(_test_pkg, "__path__", []):
        _cand = _os.path.join(_os.path.abspath(_entry), "tokenizedata")
        if _cand not in __path__ and _os.path.isfile(
            _os.path.join(_cand, "bad_coding.py")
        ):
            __path__.append(_cand)
    del _test_pkg, _entry, _cand
except (KeyError, NameError):
    pass
del _os, _sys
