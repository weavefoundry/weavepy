"""Minimal `test.pickletester` shim for WeavePy's bundled conformance run.

CPython's real `Lib/test/pickletester.py` is ~4900 lines and exercises the
full pickle protocol matrix. The only symbol the bundled `test_copyreg`
imports from it is `ExtensionSaver`, the copyreg extension-registry
save/restore helper, so we carry that verbatim rather than the whole file.
"""

import copyreg


class ExtensionSaver:
    # Remember current registration for code (if any), and remove it (if
    # there is one).
    def __init__(self, code):
        self.code = code
        if code in copyreg._inverted_registry:
            self.pair = copyreg._inverted_registry[code]
            copyreg.remove_extension(self.pair[0], self.pair[1], code)
        else:
            self.pair = None

    # Restore previous registration for code.
    def restore(self):
        code = self.code
        curpair = copyreg._inverted_registry.get(code)
        if curpair is not None:
            copyreg.remove_extension(curpair[0], curpair[1], code)
        pair = self.pair
        if pair is not None:
            copyreg.add_extension(pair[0], pair[1], code)
