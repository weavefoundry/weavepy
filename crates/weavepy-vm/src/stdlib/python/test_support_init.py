"""``test.support`` — the helper layer CPython's regression tests import.

This is a faithful subset of CPython 3.13's
``Lib/test/support/__init__.py``: the names ``Lib/test/`` modules import
unconditionally (``verbose``, the ``requires*`` resource gates, captured
IO, ``swap_attr``/``swap_item``, ``gc_collect``, the impl-detail guards,
``run_unittest``/``run_doctest``, …) plus re-exports of the most-used
helper-submodule names. Each is backed by an engine primitive WeavePy
already ships (``os``, ``gc``, ``io``, ``contextlib``, ``warnings``).

The six 3.13 split-out helpers live alongside as submodules:
``os_helper``, ``import_helper``, ``warnings_helper``,
``threading_helper``, ``script_helper``, ``socket_helper``.
"""

import contextlib
import functools
import gc
import os
import sys
import time
import types
import unittest

# ---------------------------------------------------------------------------
# Platform / build flags (faithful port of CPython's test.support surface).
# Many Lib/test modules import these at top level to gate platform-specific
# behaviour; absent them the whole module fails to import.
# ---------------------------------------------------------------------------
MS_WINDOWS = (sys.platform == 'win32')
is_jython = sys.platform.startswith('java')
is_android = sys.platform == "android"
is_emscripten = sys.platform == "emscripten"
is_wasi = sys.platform == "wasi"
is_apple_mobile = sys.platform in {"ios", "tvos", "watchos"}
is_apple = is_apple_mobile or sys.platform == "darwin"
is_s390x = hasattr(os, 'uname') and os.uname().machine == 's390x'

# WeavePy targets the standard GIL-enabled build.
Py_GIL_DISABLED = False

has_fork_support = hasattr(os, "fork") and not (
    is_emscripten or is_wasi or is_apple_mobile or is_android
)


def requires_fork():
    return unittest.skipUnless(has_fork_support, "requires working os.fork()")


def requires_gil_enabled(msg="needs the GIL enabled"):
    """Decorator for skipping tests on the free-threaded build."""
    return unittest.skipIf(Py_GIL_DISABLED, msg)


def requires_specialization(test):
    # WeavePy does not expose the adaptive-specialization opcodes, so these
    # tests are not applicable; skip them the way a non-specializing build does.
    return unittest.skip("requires specialization")(test)


def requires_specialization_ft(test):
    return unittest.skip("requires specialization")(test)


# Some CPython tests are skipped on the s390x buildbots; mirror the decorator
# so suites that reference it import cleanly (a no-op off s390x).
skip_on_s390x = unittest.skipIf(is_s390x, 'skipped on s390x')


def _requires_unix_version(sysname, min_version):
    """SkipTest if running on `sysname` with a kernel older than min_version."""
    import platform

    def decorator(func):
        @functools.wraps(func)
        def wrapper(*args, **kw):
            if platform.system() == sysname:
                version_txt = platform.release().split('-', 1)[0]
                try:
                    version = tuple(map(int, version_txt.split('.')))
                except ValueError:
                    pass
                else:
                    if version < min_version:
                        raise unittest.SkipTest(
                            "%s version %s or higher required, not %s"
                            % (sysname, '.'.join(map(str, min_version)), version_txt))
            return func(*args, **kw)
        return wrapper
    return decorator


def requires_linux_version(*min_version):
    """Decorator raising SkipTest if the Linux kernel is older than min_version."""
    return _requires_unix_version('Linux', min_version)


def control_characters_c0():
    """Return the C0 control characters (0x00-0x1F plus 0x7F) as strings."""
    return [chr(c) for c in range(0x00, 0x20)] + ["\x7F"]


# The POSIX shell used by tests that shell out. ``None`` on platforms
# without one (mirrors CPython's gate on win32/vxworks/apple-mobile).
if sys.platform not in {"win32", "vxworks", "ios", "tvos", "watchos"}:
    unix_shell = '/system/bin/sh' if is_android else '/bin/sh'
else:
    unix_shell = None


def check_sanitizer(*, address=False, memory=False, ub=False, thread=False):
    """Returns True if Python is compiled with sanitizer support.

    Verbatim port of CPython 3.13's helper. WeavePy is a plain release
    build with no ``-fsanitize`` flags, so this reads the (empty) sysconfig
    CFLAGS and returns ``False`` — the sanitizer-gated tests run normally.
    """
    if not (address or memory or ub or thread):
        raise ValueError('At least one of address, memory, ub or thread must be True')

    import sysconfig
    cflags = sysconfig.get_config_var('CFLAGS') or ''
    config_args = sysconfig.get_config_var('CONFIG_ARGS') or ''
    memory_sanitizer = (
        '-fsanitize=memory' in cflags or
        '--with-memory-sanitizer' in config_args
    )
    address_sanitizer = (
        '-fsanitize=address' in cflags or
        '--with-address-sanitizer' in config_args
    )
    ub_sanitizer = (
        '-fsanitize=undefined' in cflags or
        '--with-undefined-behavior-sanitizer' in config_args
    )
    thread_sanitizer = (
        '-fsanitize=thread' in cflags or
        '--with-thread-sanitizer' in config_args
    )
    return (
        (memory and memory_sanitizer) or
        (address and address_sanitizer) or
        (ub and ub_sanitizer) or
        (thread and thread_sanitizer)
    )


def skip_if_sanitizer(reason=None, *, address=False, memory=False, ub=False, thread=False):
    """Decorator raising SkipTest if running with a sanitizer active."""
    if not reason:
        reason = 'not working with sanitizers active'
    skip = check_sanitizer(address=address, memory=memory, ub=ub, thread=thread)
    return unittest.skipIf(skip, reason)


# gh-89363: True if fork() can hang if Python is built with Address Sanitizer
# (ASAN): libasan race condition, dead lock in pthread_create().
HAVE_ASAN_FORK_BUG = check_sanitizer(address=True)


# Pull the helper submodules in so ``from test.support import os_helper``
# and bare ``support.os_helper`` both work.
from test.support import os_helper
from test.support import import_helper
from test.support import warnings_helper

__all__ = [
    # verbosity / resources
    "verbose", "is_resource_enabled", "requires", "requires_resource",
    "ResourceDenied", "use_resources", "max_memuse",
    # runners
    "run_unittest", "run_doctest",
    # captured IO
    "captured_stdout", "captured_stderr", "captured_stdin", "captured_output",
    # attribute / item swapping
    "swap_attr", "swap_item",
    # gc
    "gc_collect", "disable_gc", "gc_threshold",
    # impl detail
    "check_impl_detail", "impl_detail", "cpython_only",
    "requires_docstrings", "MISSING_C_DOCSTRINGS",
    # misc
    "findfile", "sortdict", "Error", "TestFailed",
    "EnvironmentVarGuard", "catch_unraisable_exception", "infinite_recursion",
    "SHORT_TIMEOUT", "LOOPBACK_TIMEOUT", "requires_IEEE_754", "no_tracing",
    "refcount_test", "check_disallow_instantiation", "force_not_colorized",
    "check_syntax_error", "run_with_locale", "get_attribute",
    "ALWAYS_EQ", "NEVER_EQ", "LARGEST", "SMALLEST",
    "requires_zlib", "requires_bz2", "requires_lzma", "requires_gzip",
    # re-exports
    "TESTFN", "TESTFN_ASCII", "TESTFN_UNICODE", "TESTFN_UNDECODABLE",
    "unlink", "rmtree", "rmdir", "create_empty_file", "skip_unless_symlink",
    "import_module", "import_fresh_module", "check_warnings",
    "os_helper", "import_helper", "warnings_helper",
]


# ---------------------------------------------------------------------------
# Errors
# ---------------------------------------------------------------------------

class Error(Exception):
    """Base class for regression-test exceptions."""


class TestFailed(Error):
    """A test failed (raised by the older ``run_unittest`` path)."""


class TestDidNotRun(Error):
    """A test ran no cases."""


# ---------------------------------------------------------------------------
# Resource gating
# ---------------------------------------------------------------------------

# Filled by libregrtest; ``None`` means "no optional resources enabled".
use_resources = None

# Verbosity. Tools (libregrtest, ``run_doctest``) flip this.
verbose = 1

# Maximum memory a "bigmem" test may use, in bytes. 0 disables them.
max_memuse = 0
real_max_memuse = 0

# Common timeouts (seconds) tests pull from support.
LOOPBACK_TIMEOUT = 5.0
INTERNET_TIMEOUT = 60.0
SHORT_TIMEOUT = 30.0
LONG_TIMEOUT = 5 * 60.0


class ResourceDenied(unittest.SkipTest):
    """A resource the test needs is not enabled, so the test skips."""


# Resources that are always treated as available (cheap / always-safe).
ALWAYS_ENABLED_RESOURCES = frozenset()


def is_resource_enabled(resource):
    """``True`` if *resource* is currently enabled."""
    return use_resources is not None and resource in use_resources


def requires(resource, msg=None):
    """Raise :class:`ResourceDenied` if *resource* is not enabled."""
    if resource in ALWAYS_ENABLED_RESOURCES:
        return
    if not is_resource_enabled(resource):
        if msg is None:
            msg = "Use of the %r resource not enabled" % resource
        raise ResourceDenied(msg)


def requires_resource(resource):
    """Decorator: skip the test when *resource* is not enabled."""
    if resource == 'gui' and not _is_gui_available():
        return unittest.skip(_is_gui_available.reason)
    if is_resource_enabled(resource):
        return _id
    return unittest.skip("resource %r is not enabled" % resource)


def _is_gui_available():
    return False


_is_gui_available.reason = "no GUI available"


def _id(obj):
    return obj


# ---------------------------------------------------------------------------
# Implementation-detail guards
# ---------------------------------------------------------------------------

def _parse_guards(guards):
    if not guards:
        return ({'cpython': True}, False)
    if list(guards.values()) == [True]:
        return (guards, False)
    if list(guards.values()) == [False]:
        return (guards, True)
    raise ValueError("guards must be all True or all False")


def check_impl_detail(**guards):
    """``True`` when the running implementation matches *guards*.

    WeavePy reports ``sys.implementation.name == 'weavepy'``, so
    ``check_impl_detail(cpython=True)`` is ``False`` and CPython-internal
    tests honestly *skip* rather than fail.
    """
    guards, default = _parse_guards(guards)
    return guards.get(sys.implementation.name, default)


def impl_detail(msg=None, **guards):
    if check_impl_detail(**guards):
        return _id
    if msg is None:
        guardnames, default = _parse_guards(guards)
        guardnames = sorted(guardnames.keys())
        if default:
            msg = "implementation detail not available on {0}"
        else:
            msg = "implementation detail specific to {0}"
        msg = msg.format(' or '.join(guardnames))
    return unittest.skip(msg)


def cpython_only(test):
    """Decorator: skip *test* on non-CPython implementations."""
    return impl_detail(cpython=True)(test)


# True when C functions lack docstrings (e.g. built with -OO). We ship
# docstrings, so this is False.
MISSING_C_DOCSTRINGS = False


def _check_docstrings():
    """Just used to check if docstrings are enabled"""


# Probe-derived, exactly as CPython's test.support computes them.
HAVE_PY_DOCSTRINGS = _check_docstrings.__doc__ is not None
HAVE_DOCSTRINGS = (HAVE_PY_DOCSTRINGS and not MISSING_C_DOCSTRINGS)


def requires_docstrings(func):
    """Decorator skipping when docstrings were stripped."""
    return unittest.skipUnless(HAVE_DOCSTRINGS,
                               "test requires docstrings")(func)


def requires_IEEE_754(func):
    import math
    return unittest.skipUnless(
        getattr(getattr(__import__('sys'), 'float_info', None),
                'radix', 2) == 2 or math, "test requires IEEE 754 doubles")(func)


# ---------------------------------------------------------------------------
# Captured IO
# ---------------------------------------------------------------------------

@contextlib.contextmanager
def captured_output(stream_name):
    """Swap *stream_name* on ``sys`` for a ``StringIO`` for the block."""
    import io
    orig_stdout = getattr(sys, stream_name)
    setattr(sys, stream_name, io.StringIO())
    try:
        yield getattr(sys, stream_name)
    finally:
        setattr(sys, stream_name, orig_stdout)


def captured_stdout():
    return captured_output("stdout")


def captured_stderr():
    return captured_output("stderr")


def captured_stdin():
    return captured_output("stdin")


# ---------------------------------------------------------------------------
# Attribute / item swapping
# ---------------------------------------------------------------------------

@contextlib.contextmanager
def swap_attr(obj, attr, new_val):
    """Temporarily set ``obj.attr = new_val`` (restoring/removing after)."""
    if hasattr(obj, attr):
        real_val = getattr(obj, attr)
        setattr(obj, attr, new_val)
        try:
            yield real_val
        finally:
            setattr(obj, attr, real_val)
    else:
        setattr(obj, attr, new_val)
        try:
            yield
        finally:
            if hasattr(obj, attr):
                delattr(obj, attr)


@contextlib.contextmanager
def swap_item(obj, item, new_val):
    """Temporarily set ``obj[item] = new_val`` (restoring/removing after)."""
    if item in obj:
        real_val = obj[item]
        obj[item] = new_val
        try:
            yield real_val
        finally:
            obj[item] = real_val
    else:
        obj[item] = new_val
        try:
            yield
        finally:
            if item in obj:
                del obj[item]


@contextlib.contextmanager
def adjust_int_max_str_digits(max_digits):
    """Temporarily change the integer string conversion length limit."""
    current = sys.get_int_max_str_digits()
    try:
        sys.set_int_max_str_digits(max_digits)
        yield
    finally:
        sys.set_int_max_str_digits(current)


class _ClockInfo:
    def __init__(self, implementation, resolution):
        self.implementation = implementation
        self.monotonic = True
        self.adjustable = False
        self.resolution = resolution


class CPUStopwatch:
    """Context manager to roughly time a CPU-bound operation.

    WeavePy lacks ``time.process_time``/``time.get_clock_info`` so this is
    backed by ``time.perf_counter``; the public surface (``seconds`` and
    ``clock_info.resolution``) matches CPython's helper.
    """

    def __enter__(self):
        self.clock_info = _ClockInfo("perf_counter", 1e-9)
        self.get_time = time.perf_counter
        self.context = disable_gc()
        self.context.__enter__()
        self.start_time = self.get_time()
        self.seconds = 0.0
        return self

    def __exit__(self, *exc):
        try:
            end_time = self.get_time()
        finally:
            result = self.context.__exit__(*exc)
        self.seconds = end_time - self.start_time
        return result


def run_in_subinterp(code):
    """Run ``code`` in a subinterpreter.

    WeavePy does not implement subinterpreters, so tests that depend on
    per-interpreter isolation are skipped rather than silently mis-run.
    """
    import unittest
    raise unittest.SkipTest("subinterpreters are not supported")


def patch(test_instance, object_to_patch, attr_name, new_value):
    """Override 'object_to_patch'.'attr_name' with 'new_value'.

    Also, add a cleanup procedure to 'test_instance' to restore
    'object_to_patch' value for 'attr_name'.
    The 'attr_name' should be a valid attribute for 'object_to_patch'.
    """
    # check that 'attr_name' is a real attribute for 'object_to_patch'
    # will raise AttributeError if it does not exist
    getattr(object_to_patch, attr_name)

    # keep a copy of the old value
    attr_is_local = False
    try:
        old_value = object_to_patch.__dict__[attr_name]
    except (AttributeError, TypeError, KeyError):
        old_value = getattr(object_to_patch, attr_name, None)
    else:
        attr_is_local = True

    # restore the value when the test is done
    def cleanup():
        if attr_is_local:
            setattr(object_to_patch, attr_name, old_value)
        else:
            try:
                delattr(object_to_patch, attr_name)
            except (AttributeError, TypeError, KeyError):
                pass

    test_instance.addCleanup(cleanup)

    # actually override the attribute
    setattr(object_to_patch, attr_name, new_value)


def check__all__(test_case, module, name_of_module=None, extra=(),
                 not_exported=()):
    """Assert that the __all__ variable of 'module' contains all public names.

    The module's public names (its API) are detected automatically based on
    whether they are documented in the module's docstring by being prefixed by
    a '>>>' followed by a space, or are imported from another module
    (when ``name_of_module`` is provided).

    Args:
        test_case: an instance of unittest.TestCase to use the assert* methods.
        module: the module to check.
        name_of_module: the name(s) of 'module' (in case the module imports
            objects from other modules e.g. ``collections.abc`` imports from
            ``_collections_abc``). This argument can be a sequence of names or
            a string.
        extra: names that are imported into the module but aren't part of
            ``__all__``, which are still expected to be in ``__all__``.
        not_exported: names that are in the module but expected to not be in
            ``__all__``.
    """

    if name_of_module is None:
        name_of_module = (module.__name__, )
    elif isinstance(name_of_module, str):
        name_of_module = (name_of_module, )

    expected = set(extra)

    for name in dir(module):
        if name.startswith('_') or name in not_exported:
            continue
        obj = getattr(module, name)

        if (getattr(obj, '__module__', None) in name_of_module or
                (not hasattr(obj, '__module__') and
                 not isinstance(obj, types.ModuleType))):
            expected.add(name)

    test_case.assertCountEqual(module.__all__, expected)


def setswitchinterval(interval):
    """Set the bytecode switch interval, clamped to a sane minimum
    (CPython clamps harder on Android; we keep the plain floor)."""
    minimum_interval = 1e-9
    interval = max(interval, minimum_interval)
    sys.setswitchinterval(interval)


# ---------------------------------------------------------------------------
# GC helpers
# ---------------------------------------------------------------------------

def gc_collect():
    """Force a few GC passes so finalizers/weakrefs settle."""
    gc.collect()
    gc.collect()
    gc.collect()


@contextlib.contextmanager
def disable_gc():
    have_gc = gc.isenabled()
    gc.disable()
    try:
        yield
    finally:
        if have_gc:
            gc.enable()


@contextlib.contextmanager
def gc_threshold(*args):
    old_threshold = gc.get_threshold()
    gc.set_threshold(*args)
    try:
        yield
    finally:
        gc.set_threshold(*old_threshold)


# ---------------------------------------------------------------------------
# Test runners (legacy ``test_main`` protocol)
# ---------------------------------------------------------------------------

def _run_suite(suite):
    """Run *suite* with a quiet runner; raise ``TestFailed`` on failure."""
    runner = unittest.TextTestRunner(sys.stdout, verbosity=verbose)
    result = runner.run(suite)
    if not result.wasSuccessful():
        if len(result.errors) == 1 and not result.failures:
            err = result.errors[0][1]
        elif len(result.failures) == 1 and not result.errors:
            err = result.failures[0][1]
        else:
            err = "errors=%d failures=%d" % (len(result.errors),
                                             len(result.failures))
        raise TestFailed(err)
    return result


def run_unittest(*classes):
    """Run the given ``TestCase`` classes / modules / suites."""
    valid_types = (unittest.TestSuite, unittest.TestCase)
    loader = unittest.TestLoader()
    suite = unittest.TestSuite()
    for cls in classes:
        if isinstance(cls, str):
            if cls in sys.modules:
                suite.addTest(loader.loadTestsFromModule(sys.modules[cls]))
            else:
                raise ValueError("str arguments must be keys in sys.modules")
        elif isinstance(cls, valid_types):
            suite.addTest(cls)
        else:
            suite.addTest(loader.loadTestsFromTestCase(cls))
    return _run_suite(suite)


def run_doctest(module, verbosity=None, optionflags=0):
    """Run *module*'s doctests; raise ``TestFailed`` if any fail."""
    import doctest
    if verbosity is None:
        verbosity = verbose
    else:
        verbosity = None
    f, t = doctest.testmod(module, verbose=verbosity, optionflags=optionflags)
    if f:
        raise TestFailed("%d of %d doctests failed" % (f, t))
    if verbose:
        print('doctest (%s) ... %d tests with zero failures' %
              (module.__name__, t))
    return f, t


# ---------------------------------------------------------------------------
# Misc helpers
# ---------------------------------------------------------------------------

def _test_home_dir():
    """Best-effort absolute path of the on-disk ``Lib/test`` directory.

    CPython derives this from ``support/__init__.py``'s ``__file__``, but
    WeavePy ships ``test``/``test.support`` *frozen* — their ``__file__``
    is a synthetic ``<frozen ...>`` token that ``abspath`` mangles into a
    cwd-relative path. The actual ``test_*`` modules still load from disk,
    so locate the package root via an already-imported on-disk ``test``
    submodule, then by scanning ``sys.path`` for a ``test`` directory.
    """
    for name, mod in list(sys.modules.items()):
        if name == 'test' or name.startswith('test.'):
            f = getattr(mod, '__file__', None)
            if f and os.path.exists(f):
                d = os.path.dirname(os.path.abspath(f))
                while d and os.path.basename(d) != 'test':
                    parent = os.path.dirname(d)
                    if parent == d:
                        break
                    d = parent
                if os.path.basename(d) == 'test':
                    return d
    for p in sys.path:
        cand = os.path.join(p, 'test')
        if os.path.isdir(cand):
            return cand
    f = globals().get('__file__')
    if f and os.path.exists(f):
        return os.path.dirname(os.path.abspath(f))
    return os.getcwd()


def findfile(filename, subdir=None):
    """Locate a test data file; return *filename* unchanged if not found.

    Mirrors CPython's search order: the ``test`` home directory first
    (where top-level data files like ``mime.types`` live), then every
    ``sys.path`` entry.
    """
    if os.path.isabs(filename):
        return filename
    if subdir is not None:
        filename = os.path.join(subdir, filename)
    home = _test_home_dir()
    for path in [home, os.path.join(home, "data")] + list(sys.path):
        fn = os.path.join(path, filename)
        if os.path.exists(fn):
            return fn
    return filename


def load_package_tests(pkg_dir, loader, standard_tests, pattern):
    """Generic ``load_tests`` body for simple test *packages*.

    A package whose ``__init__.py`` only delegates to this (e.g.
    ``test.test_inspect``) discovers and runs every ``test*`` module under
    its directory. ``top_level_dir`` is the stdlib root so the discovered
    modules import under their dotted ``test.<pkg>.<mod>`` names.
    """
    if pattern is None:
        pattern = "test*"
    package_tests = loader.discover(start_dir=pkg_dir,
                                    top_level_dir=STDLIB_DIR,
                                    pattern=pattern)
    standard_tests.addTests(package_tests)
    return standard_tests


def sortdict(dict):
    """Return a repr of *dict* with keys in sorted order."""
    keys = sorted(dict.keys())
    lines = ["%r: %r" % (k, dict[k]) for k in keys]
    return "{%s}" % ", ".join(lines)


def get_attribute(obj, name):
    """``getattr(obj, name)`` but turn a miss into ``SkipTest``."""
    try:
        attribute = getattr(obj, name)
    except AttributeError:
        raise unittest.SkipTest("object %r has no attribute %r" %
                                (obj, name))
    return attribute


def check_syntax_error(testcase, statement, errtext='', *, lineno=None,
                       offset=None):
    """Assert *statement* raises a matching ``SyntaxError`` at compile."""
    with testcase.assertRaisesRegex(SyntaxError, errtext) as cm:
        compile(statement, '<test string>', 'exec')
    err = cm.exception
    testcase.assertIsNotNone(err.lineno)
    if lineno is not None:
        testcase.assertEqual(err.lineno, lineno)
    if offset is not None:
        testcase.assertEqual(err.offset, offset)


@contextlib.contextmanager
def catch_unraisable_exception():
    """Capture ``sys.unraisablehook`` output for the block (best-effort)."""
    class _Catcher:
        unraisable = None

        def _hook(self, unraisable):
            self.unraisable = unraisable

    catcher = _Catcher()
    old_hook = getattr(sys, 'unraisablehook', None)
    if old_hook is not None:
        sys.unraisablehook = catcher._hook
    try:
        yield catcher
    finally:
        if old_hook is not None:
            sys.unraisablehook = old_hook
        catcher.unraisable = None


@contextlib.contextmanager
def infinite_recursion(max_depth=None):
    """Raise the recursion limit so tests can recurse (nearly) without
    bound — CPython's helper sets 20 000 by default and relies on the
    C-stack guard underneath; callers pass a small ``max_depth`` when
    they *want* a quick ``RecursionError``."""
    if max_depth is None:
        max_depth = 20_000
    elif max_depth < 3:
        raise ValueError("max_depth must be at least 3")
    get_limit = getattr(sys, 'getrecursionlimit', None)
    set_limit = getattr(sys, 'setrecursionlimit', None)
    if get_limit is None or set_limit is None:
        yield
        return
    original_depth = get_limit()
    try:
        set_limit(max_depth)
        yield
    finally:
        set_limit(original_depth)


def no_tracing(func):
    """Decorator: disable ``sys.settrace`` for the duration of *func*."""
    if not hasattr(sys, 'gettrace'):
        return func

    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        original_trace = sys.gettrace()
        try:
            sys.settrace(None)
            return func(*args, **kwargs)
        finally:
            sys.settrace(original_trace)
    return wrapper


def refcount_test(test):
    """Decorator: skip a refcount-sensitive test off CPython."""
    return unittest.skipUnless(
        hasattr(sys, 'gettotalrefcount') or
        check_impl_detail(cpython=True),
        "needs CPython reference counting")(test)


def check_disallow_instantiation(testcase, tp, *args, **kwargs):
    """Assert that ``tp(*args, **kwargs)`` raises ``TypeError``."""
    msg = "cannot create '%s' instances" % getattr(tp, '__name__', tp)
    with testcase.assertRaisesRegex(TypeError, ""):
        tp(*args, **kwargs)


def force_not_colorized(func):
    """Decorator forcing un-colorized output around *func*."""
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        guard = os_helper.EnvironmentVarGuard()
        with guard:
            guard.set("NO_COLOR", "1")
            guard.unset("FORCE_COLOR") if "FORCE_COLOR" in os.environ else None
            return func(*args, **kwargs)
    return wrapper


@contextlib.contextmanager
def run_with_locale(catstr, *locales):
    """Run the block under the first *locale* that ``setlocale`` accepts."""
    try:
        import locale
        category = getattr(locale, catstr)
        orig_locale = locale.setlocale(category)
    except (ImportError, AttributeError):
        locale = None
        orig_locale = None
        category = None
    else:
        for loc in locales:
            try:
                locale.setlocale(category, loc)
                break
            except Exception:
                pass
    try:
        yield
    finally:
        if locale is not None and orig_locale is not None:
            try:
                locale.setlocale(category, orig_locale)
            except Exception:
                pass


# ---------------------------------------------------------------------------
# Comparison sentinels
# ---------------------------------------------------------------------------

class _ALWAYS_EQ:
    """Object equal to everything (for testing == semantics)."""

    def __eq__(self, other):
        return True

    def __ne__(self, other):
        return False


ALWAYS_EQ = _ALWAYS_EQ()


class _NEVER_EQ:
    """Object unequal to everything."""

    def __eq__(self, other):
        return False

    def __ne__(self, other):
        return True

    def __hash__(self):
        return 1


NEVER_EQ = _NEVER_EQ()


class _LARGEST:
    """Object larger than every other object."""

    def __eq__(self, other):
        return isinstance(other, _LARGEST)

    def __lt__(self, other):
        return False

    def __le__(self, other):
        return isinstance(other, _LARGEST)

    def __gt__(self, other):
        return not isinstance(other, _LARGEST)

    def __ge__(self, other):
        return True

    def __hash__(self):
        return id(_LARGEST)


LARGEST = _LARGEST()


class _SMALLEST:
    """Object smaller than every other object."""

    def __eq__(self, other):
        return isinstance(other, _SMALLEST)

    def __gt__(self, other):
        return False

    def __ge__(self, other):
        return isinstance(other, _SMALLEST)

    def __lt__(self, other):
        return not isinstance(other, _SMALLEST)

    def __le__(self, other):
        return True

    def __hash__(self):
        return id(_SMALLEST)


SMALLEST = _SMALLEST()


# ---------------------------------------------------------------------------
# Compression-module resource gates
# ---------------------------------------------------------------------------

def _requires_module(name):
    try:
        __import__(name)
    except ImportError:
        return unittest.skip("requires %s" % name)
    return _id


requires_zlib = _requires_module('zlib')
requires_gzip = _requires_module('gzip')
requires_bz2 = _requires_module('bz2')
requires_lzma = _requires_module('lzma')


# ---------------------------------------------------------------------------
# Size constants commonly imported by tests
# ---------------------------------------------------------------------------

MAX_Py_ssize_t = sys.maxsize
_1M = 1024 * 1024
_1G = 1024 * _1M
_2G = 2 * _1G
_4G = 4 * _1G

Py_DEBUG = hasattr(sys, 'gettotalrefcount')

# Directory holding the test package. Resolved against the on-disk
# `Lib/test` (the frozen package's `__file__` is synthetic — see
# `_test_home_dir`), so `STDLIB_DIR` correctly points at `Lib` for
# `load_package_tests`' discovery `top_level_dir`.
TEST_HOME_DIR = _test_home_dir()
TEST_SUPPORT_DIR = os.path.join(TEST_HOME_DIR, "support")
STDLIB_DIR = os.path.dirname(TEST_HOME_DIR)
REPO_ROOT = os.path.dirname(STDLIB_DIR)


# ---------------------------------------------------------------------------
# Re-exports from helper submodules (legacy import locations)
# ---------------------------------------------------------------------------

# os_helper
TESTFN = os_helper.TESTFN
TESTFN_ASCII = os_helper.TESTFN_ASCII
TESTFN_UNICODE = os_helper.TESTFN_UNICODE
TESTFN_UNDECODABLE = os_helper.TESTFN_UNDECODABLE
TESTFN_NONASCII = os_helper.TESTFN_NONASCII
SAVEDCWD = os_helper.SAVEDCWD
EnvironmentVarGuard = os_helper.EnvironmentVarGuard
FakePath = os_helper.FakePath
unlink = os_helper.unlink
rmtree = os_helper.rmtree
rmdir = os_helper.rmdir
create_empty_file = os_helper.create_empty_file
make_bad_fd = os_helper.make_bad_fd
can_symlink = os_helper.can_symlink
skip_unless_symlink = os_helper.skip_unless_symlink
temp_dir = os_helper.temp_dir
temp_cwd = os_helper.temp_cwd
change_cwd = os_helper.change_cwd

# import_helper (legacy: these used to live directly on support)
import_module = import_helper.import_module
import_fresh_module = import_helper.import_fresh_module
unload = import_helper.unload
forget = import_helper.forget
CleanImport = import_helper.CleanImport
DirsOnSysPath = import_helper.DirsOnSysPath

# warnings_helper
check_warnings = warnings_helper.check_warnings
check_no_resource_warning = warnings_helper.check_no_resource_warning
ignore_warnings = warnings_helper.ignore_warnings


# ---------------------------------------------------------------------------
# bigmem decorators (mostly no-ops here: max_memuse defaults to 0)
# ---------------------------------------------------------------------------

def bigmemtest(size, memuse, dry_run=True):
    def decorator(f):
        @functools.wraps(f)
        def wrapper(self, *args, **kwargs):
            size_val = wrapper.size
            if not real_max_memuse:
                maxsize = 5147
            else:
                maxsize = size_val
            if real_max_memuse and real_max_memuse < maxsize * memuse:
                if dry_run:
                    maxsize = 5147
                else:
                    raise unittest.SkipTest(
                        "not enough memory: %.1fG minimum needed" %
                        (size_val * memuse / (1024 ** 3)))
            return f(self, maxsize)
        wrapper.size = size
        wrapper.memuse = memuse
        return wrapper
    return decorator


def precisionbigmemtest(size, memuse, dry_run=True):
    return bigmemtest(size, memuse, dry_run)


def reap_children():
    """Best-effort reap of any leaked child processes (no-op on success)."""
    if not hasattr(os, 'waitpid') or not hasattr(os, 'WNOHANG'):
        return
    while True:
        try:
            pid, status = os.waitpid(-1, os.WNOHANG)
        except Exception:
            break
        if pid == 0:
            break


def get_pagesize():
    try:
        return os.sysconf("SC_PAGESIZE")
    except (ValueError, AttributeError, OSError):
        return 4096


def python_is_optimized():
    """``True`` if the interpreter was built with optimizations.

    WeavePy is always an optimized native build, so report ``True``.
    """
    return True


def check_sizeof(test, o, size):
    """Skip ``sys.getsizeof`` assertions when the API is unavailable."""
    if not hasattr(sys, 'getsizeof'):
        raise unittest.SkipTest("sys.getsizeof not available")
    result = sys.getsizeof(o)
    test.assertEqual(result, size)


# Backwards-compatible aliases some tests use.
run_doctest = run_doctest


# ---------------------------------------------------------------------------
# RFC 0036 — helpers reached for by `Lib/test/` files in the conformance
# sweep: a faithful `open_urlresource` (skips unless the `urlfetch`
# resource is enabled, exactly as CPython does), a no-op
# `SuppressCrashReport`, and the `bigaddrspacetest` decorator.
# ---------------------------------------------------------------------------

TEST_DATA_DIR = os.path.join(TEST_HOME_DIR, "data")


def open_urlresource(url, *args, **kw):
    import urllib.parse

    check = kw.pop('check', None)
    filename = urllib.parse.urlparse(url)[2].split('/')[-1]  # '/': it's URL!
    fn = os.path.join(TEST_DATA_DIR, filename)

    def check_valid_file(fn):
        f = open(fn, *args, **kw)
        if check is None:
            return f
        elif check(f):
            f.seek(0)
            return f
        f.close()

    if os.path.exists(fn):
        f = check_valid_file(fn)
        if f is not None:
            return f

    # Verify the requirement before downloading the file. In the
    # conformance sandbox the `urlfetch` resource is never enabled, so
    # this raises `ResourceDenied` and the calling test is skipped —
    # matching CPython's `OK (skipped=…)` outcome for network fixtures.
    requires('urlfetch')

    import urllib.request

    opener = urllib.request.urlopen(url, timeout=15)
    try:
        with open(fn, "wb") as out:
            out.write(opener.read())
    finally:
        opener.close()
    f = check_valid_file(fn)
    if f is not None:
        return f
    raise TestFailed('invalid resource %r' % fn)


class SuppressCrashReport:
    """Best-effort suppression of OS crash dialogs / coredumps.

    WeavePy does not surface a Windows Error Reporting dialog and the
    conformance harness already isolates each test in its own process,
    so this is a no-op context manager that matches CPython's interface.
    """

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        return False


def bigaddrspacetest(f):
    """Decorator for tests that fill the address space."""

    def wrapper(self):
        if max_memuse < MAX_Py_ssize_t:
            if MAX_Py_ssize_t >= 2**63 - 1 and max_memuse >= 2**31:
                raise unittest.SkipTest(
                    "not enough memory: try a 32-bit build instead")
            else:
                raise unittest.SkipTest(
                    "not enough memory: %.1fG minimum needed"
                    % (MAX_Py_ssize_t / (1024 ** 3)))
        else:
            return f(self)

    return wrapper


_is_pgo = False

# True only for a `--with-trace-refs` debug build of CPython. WeavePy is
# always a release-shaped build, so the all-objects tracker is absent.
Py_TRACE_REFS = hasattr(sys, "getobjects")


@contextlib.contextmanager
def no_color():
    """Context manager forcing un-colorized output.

    WeavePy never emits ANSI colour escapes from the interpreter or its
    tracebacks, so there is nothing to suppress; the helper exists purely
    so ``Lib/test/`` files that wrap assertions in it import and run.
    """
    yield


def force_not_colorized(func):
    """Force the terminal not to be colorized."""
    @functools.wraps(func)
    def wrapper(*args, **kwargs):
        with no_color():
            return func(*args, **kwargs)
    return wrapper


def force_not_colorized_test_class(cls):
    """Force the terminal not to be colorized for the entire test class.

    The CPython original swaps ``_colorize.can_colorize`` for the class via
    ``enterClassContext``; WeavePy output is never colorized, so the class
    is returned unchanged.
    """
    return cls


def linked_to_musl():
    """
    Test if the Python executable is linked to the musl C library.
    """
    if sys.platform != 'linux':
        return False

    import subprocess
    exe = getattr(sys, '_base_executable', sys.executable)
    cmd = ['ldd', exe]
    try:
        stdout = subprocess.check_output(cmd,
                                         text=True,
                                         stderr=subprocess.STDOUT)
    except (OSError, subprocess.CalledProcessError):
        return False
    return ('musl' in stdout)


def requires_mac_ver(*min_version):
    """Decorator raising SkipTest if the OS is Mac OS X and the OS X
    version if less than min_version.

    For example, @requires_mac_ver(10, 5) raises SkipTest if the OS X version
    is lesser than 10.5.
    """
    def decorator(func):
        @functools.wraps(func)
        def wrapper(*args, **kw):
            if sys.platform == 'darwin':
                import platform
                version_txt = platform.mac_ver()[0]
                try:
                    version = tuple(map(int, version_txt.split('.')))
                except ValueError:
                    pass
                else:
                    if version < min_version:
                        min_version_txt = '.'.join(map(str, min_version))
                        raise unittest.SkipTest(
                            "Mac OS X %s or higher required, not %s"
                            % (min_version_txt, version_txt))
            return func(*args, **kw)
        wrapper.min_version = min_version
        return wrapper
    return decorator


def skip_if_pgo_task(test):
    """Skip decorator for tests not run in (non-extended) PGO task.

    WeavePy is never built under a profile-guided-optimisation task, so
    `_is_pgo` is always false and the test runs unchanged.
    """
    msg = "Not run for (non-extended) PGO task"
    return test if not _is_pgo else unittest.skip(msg)(test)


# --- RFC 0037 (WS9): helpers many CPython test modules import from
# `test.support`. CPython sources several of these from the `_testcapi` /
# `_testinternalcapi` C extensions, which WeavePy does not ship; we provide
# behaviour-equivalent fallbacks so the importing test bodies actually run.

def get_c_recursion_limit():
    """Depth at which the interpreter raises ``RecursionError``.

    CPython reads ``_testcapi.Py_C_RECURSION_LIMIT`` (its separate C-stack
    ceiling). WeavePy enforces a single Python-level recursion limit in the
    dispatch loop (RFC 0037 WS1), so the meaningful value here is exactly
    ``sys.getrecursionlimit()`` — what we actually raise at.
    """
    return sys.getrecursionlimit()


def exceeds_recursion_limit():
    """For recursion tests, easily exceeds default recursion limit."""
    return get_c_recursion_limit() * 3


def check_free_after_iterating(test, iter, cls, args=()):
    done = False
    def wrapper():
        class A(cls):
            def __del__(self):
                nonlocal done
                done = True
                try:
                    next(it)
                except StopIteration:
                    pass

        it = iter(A(*args))
        # Issue 26494: Shouldn't crash
        test.assertRaises(StopIteration, next, it)

    wrapper()
    # The sequence should be deallocated just after the end of iterating
    gc_collect()
    test.assertTrue(done)


def subTests(arg_names, arg_values, /, *, _do_cleanups=False):
    """Run multiple subtests with different parameters."""
    single_param = False
    if isinstance(arg_names, str):
        arg_names = arg_names.replace(',', ' ').split()
        if len(arg_names) == 1:
            single_param = True
    arg_values = tuple(arg_values)
    def decorator(func):
        if isinstance(func, type):
            raise TypeError('subTests() can only decorate methods, not classes')
        @functools.wraps(func)
        def wrapper(self, /, *args, **kwargs):
            for values in arg_values:
                if single_param:
                    values = (values,)
                subtest_kwargs = dict(zip(arg_names, values))
                with self.subTest(**subtest_kwargs):
                    func(self, *args, **kwargs, **subtest_kwargs)
                if _do_cleanups:
                    self.doCleanups()
        return wrapper
    return decorator


def can_use_suppress_immortalization(suppress=True):
    # WeavePy has no deferred-object immortalization, so the suppression
    # context is always usable (it's a no-op).
    return True


@contextlib.contextmanager
def suppress_immortalization(suppress=True):
    """No-op on WeavePy.

    CPython toggles a refcount-immortalization optimization via
    ``_testinternalcapi``; WeavePy has no such optimization, so there is
    nothing to suppress and the body simply runs.
    """
    yield


def skip_if_suppress_immortalization():
    # Nothing to skip: WeavePy never immortalizes deferred objects.
    return None


def has_no_debug_ranges():
    # WeavePy emits per-instruction source positions (co_positions / debug
    # ranges, RFC 0033), so tests guarded on their presence may run.
    return False


def requires_debug_ranges(reason='requires co_positions / debug_ranges'):
    try:
        skip = has_no_debug_ranges()
    except unittest.SkipTest as e:
        skip = True
        reason = e.args[0] if e.args else reason
    return unittest.skipIf(skip, reason)


# WeavePy ships a `socket` module and its cooperative event loop does not
# need privileged sockets, so socket-gated test modules are allowed to run.
has_socket_support = True


def requires_working_socket(*, module=False):
    """Skip tests or modules that require working sockets.

    Can be used as a function/class decorator or to skip an entire module.
    """
    msg = "requires socket support"
    if module:
        if not has_socket_support:
            raise unittest.SkipTest(msg)
    else:
        return unittest.skipUnless(has_socket_support, msg)


# WeavePy can spawn subprocesses through its `subprocess` module.
has_subprocess_support = True


def requires_subprocess():
    """Used for subprocess, os.spawn calls, fd inheritance."""
    return unittest.skipUnless(has_subprocess_support, "requires subprocess support")


@contextlib.contextmanager
def patch_list(orig):
    """Like unittest.mock.patch.dict, but for lists."""
    try:
        saved = orig[:]
        yield
    finally:
        orig[:] = saved


class BrokenIter:
    def __init__(self, init_raises=False, next_raises=False, iter_raises=False):
        if init_raises:
            1/0
        self.next_raises = next_raises
        self.iter_raises = iter_raises

    def __next__(self):
        if self.next_raises:
            1/0

    def __iter__(self):
        if self.iter_raises:
            1/0
        return self


__all__ += [
    "get_c_recursion_limit", "exceeds_recursion_limit",
    "check_free_after_iterating", "subTests",
    "can_use_suppress_immortalization", "suppress_immortalization",
    "skip_if_suppress_immortalization",
    "has_no_debug_ranges", "requires_debug_ranges",
    "has_socket_support", "requires_working_socket",
    "has_subprocess_support", "requires_subprocess",
    "patch_list", "BrokenIter",
]


__all__ += ["open_urlresource", "SuppressCrashReport", "bigaddrspacetest",
            "TEST_DATA_DIR", "TEST_HOME_DIR", "skip_if_pgo_task", "Py_TRACE_REFS",
            "requires_mac_ver", "no_color", "force_not_colorized",
            "force_not_colorized_test_class", "linked_to_musl"]

__all__ += ["check_sanitizer", "skip_if_sanitizer", "HAVE_ASAN_FORK_BUG",
            "unix_shell"]
