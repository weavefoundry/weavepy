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

# `_test_module_state_shared` (single-phase variant, bpo-44050): its
# PyInit_ adds `Error = PyExc_RuntimeError`. The exception type is the
# runtime singleton, so its identity is shared across interpreters.
Error = RuntimeError


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


class StateAccessType:
    """Type accessing per-module state (the `PyInit__testmultiphase_meth_state_access`
    variant's `StateAccessType`, graded by test_capi.test_misc
    Test_ModuleStateAccess). Each `_load_dynamic` of the
    `_testmultiphase_meth_state_access` name executes this body afresh,
    so the class-level `_count` is genuinely per-module state and
    `_defining_module` is the module PyType_GetModuleByDef would find.
    """

    _defining_module = None
    _count = 0

    def get_defining_module(self):
        return StateAccessType._defining_module

    def getmodulebydef_bad_def(self):
        # PyType_GetModuleByDef with a module def no superclass was
        # created from (bpo-46433).
        raise TypeError(
            "PyType_GetModuleByDef: No superclass of 'StateAccessType' "
            "has the given module"
        )

    def increment_count_clinic(self, n=1, /, *, twice=False):
        StateAccessType._count += n * (2 if twice else 1)

    def increment_count_noclinic(self, n=1, /, *, twice=False):
        StateAccessType._count += n * (2 if twice else 1)

    def get_count(self):
        return StateAccessType._count


def _weave_rebind_module(mod):
    # Called by ExtensionFileLoader.exec_module after it copies this
    # body's dict into the spec-allocated module object: rebind the
    # "defining module" so `get_defining_module()` preserves identity
    # with the caller's module (assertIs in Test_ModuleStateAccess).
    StateAccessType._defining_module = mod


try:
    import sys as _sys

    StateAccessType._defining_module = _sys.modules.get(__name__)
    del _sys
except Exception:
    pass
