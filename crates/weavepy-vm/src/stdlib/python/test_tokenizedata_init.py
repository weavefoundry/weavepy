# Vendored CPython test fixture package (`test.tokenizedata`).
#
# CPython's `Lib/test/tokenizedata/` holds intentionally-malformed source
# files used by the lexer/tokenizer regression tests. `test_unicode_identifiers`
# imports `badsyntax_3131` to assert the exact `SyntaxError` raised for an
# invalid PEP 3131 identifier. The package `__init__` is empty upstream.
