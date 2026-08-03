"""Pure-Python stand-in for CPython's `_testmultiphase` C test extension.

CPython builds `_testmultiphase` as a multi-phase-init (PEP 489) test
module. Its mere *importability* gates unrelated suites: `test.test_importlib.util`
does `import_helper.import_module("_testmultiphase")` at module scope, so on
builds without the extension every module that does
`from test.test_importlib.util import uncache` — notably
`test.test_unittest.testmock.testpatch` — is skipped wholesale.

WeavePy cannot host the real C extension, but the patch suite is far too
valuable to lose to an import guard. This stub mirrors the module's public
constants/types closely enough for the guard (and casual attribute pokes);
tests that exercise genuine multi-phase-init machinery (`test_importlib`
extension loaders, `test_import` subinterpreter legs) gate on
`ExtensionFileLoader`-discovered `.so` files or fail loudly, so they are not
silently faked into passing.
"""

int_const = 1969
str_const = 'something different'


class error(Exception):
    pass


class Example:
    """Example heap type from the real extension."""

    def demo(self, arg=None):
        return arg

    def __getattr__(self, name):
        raise AttributeError(name)


class Str(str):
    pass


def foo(a, b):
    """Return the sum of two integers."""
    return a + b


def call_state_registration_func(n):
    raise error("WeavePy _testmultiphase stub has no per-module state")
