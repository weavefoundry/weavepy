"""``test.support.os_helper`` — filesystem / environment helpers.

A faithful subset of CPython 3.13's ``Lib/test/support/os_helper.py``.
Only the names real ``Lib/test/`` modules touch are implemented; the
goal is that a test importing this module gets working ``TESTFN``,
``unlink``/``rmtree``, ``temp_dir``/``temp_cwd``/``change_cwd``,
``EnvironmentVarGuard`` and the symlink probes — backed by the engine
primitives WeavePy already ships (``os``, ``shutil``, ``tempfile``,
``contextlib``).
"""

import contextlib
import os
import sys

try:
    import shutil
except ImportError:  # pragma: no cover - shutil ships frozen
    shutil = None


# ---------------------------------------------------------------------------
# TESTFN family
# ---------------------------------------------------------------------------

# A writable scratch name unique to this process. CPython derives it from
# the running script; a pid-tagged name is good enough for our purposes
# and keeps parallel workers from clobbering each other.
TESTFN_ASCII = '@test'

# Make it unique per process so parallel test workers do not collide.
TESTFN = "{}_{}_tmp".format(TESTFN_ASCII, os.getpid())

# A non-ASCII variant. We keep it ASCII-safe-but-distinct if the platform
# cannot represent the usual unicode payload, mirroring CPython's fallback.
try:
    TESTFN_UNICODE = TESTFN + "-\xe0\xf2\u0258\u0141\u011f"
    TESTFN_UNICODE.encode(sys.getfilesystemencoding())
except (ValueError, LookupError, AttributeError):
    TESTFN_UNICODE = TESTFN + "-unicode"

# Filenames the platform cannot decode/encode. WeavePy does not model
# undecodable bytes paths, so these are ``None`` and tests gate on them.
TESTFN_UNDECODABLE = None
TESTFN_UNENCODABLE = None
TESTFN_NONASCII = TESTFN_UNICODE

# Marker used by some tests; harmless default.
TESTFN_UNICODE_UNENCODEABLE = None

SAVEDCWD = os.getcwd()


# ---------------------------------------------------------------------------
# Removal helpers (best-effort, never raise on "already gone")
# ---------------------------------------------------------------------------

def unlink(filename):
    """Remove *filename*, ignoring "does not exist"."""
    try:
        os.unlink(filename)
    except (FileNotFoundError, NotADirectoryError):
        pass


def rmdir(dirname):
    try:
        os.rmdir(dirname)
    except FileNotFoundError:
        pass


def rmtree(path):
    """Recursively remove *path*; ignore a missing tree."""
    try:
        if shutil is not None:
            shutil.rmtree(path, ignore_errors=True)
            return
    except Exception:
        pass
    # Fallback walk if shutil is unavailable.
    try:
        for root, dirs, files in os.walk(path, topdown=False):
            for name in files:
                unlink(os.path.join(root, name))
            for name in dirs:
                rmdir(os.path.join(root, name))
        rmdir(path)
    except FileNotFoundError:
        pass


def make_bad_fd():
    """Return a file descriptor that is closed (and therefore invalid)."""
    file = open(TESTFN, "wb")
    try:
        return file.fileno()
    finally:
        file.close()
        unlink(TESTFN)


def fd_count():
    """Best-effort count of open file descriptors.

    WeavePy does not expose ``/proc/self/fd`` reliably across platforms,
    so we probe a bounded range. Returns ``-1`` when we cannot tell, which
    callers treat as "skip the fd-leak assertion".
    """
    try:
        import resource
        soft, _hard = resource.getrlimit(resource.RLIMIT_NOFILE)
        limit = min(soft, 256) if soft > 0 else 256
    except Exception:
        limit = 256
    count = 0
    for fd in range(limit):
        try:
            os.fstat(fd)
        except OSError:
            continue
        except Exception:
            return -1
        count += 1
    return count


def create_empty_file(filename):
    """Create an empty file (truncating any existing one)."""
    with open(filename, "wb"):
        pass


# ---------------------------------------------------------------------------
# Path-like fake (FakePath) used by os/path tests
# ---------------------------------------------------------------------------

class FakePath:
    """Simple ``__fspath__`` wrapper, possibly raising for error paths."""

    def __init__(self, path):
        self.path = path

    def __repr__(self):
        return f'<FakePath {self.path!r}>'

    def __fspath__(self):
        if (isinstance(self.path, BaseException) or
                isinstance(self.path, type) and
                issubclass(self.path, BaseException)):
            raise self.path
        return self.path


# ---------------------------------------------------------------------------
# Directory / cwd context managers
# ---------------------------------------------------------------------------

@contextlib.contextmanager
def temp_dir(path=None, quiet=False):
    """Yield a temporary directory, removing it on exit."""
    import tempfile
    dir_created = False
    if path is None:
        path = tempfile.mkdtemp()
        dir_created = True
    else:
        try:
            os.mkdir(path)
            dir_created = True
        except OSError as exc:
            if not quiet:
                raise
            import warnings
            warnings.warn(f'tests may fail, unable to create '
                          f'temp dir: {path!r}: {exc}',
                          RuntimeWarning, stacklevel=3)
    try:
        yield path
    finally:
        if dir_created:
            rmtree(path)


@contextlib.contextmanager
def change_cwd(path, quiet=False):
    """``chdir`` into *path* for the duration of the block."""
    saved_dir = os.getcwd()
    try:
        os.chdir(path)
    except OSError as exc:
        if not quiet:
            raise
        import warnings
        warnings.warn(f'tests may fail, unable to change the CWD to '
                      f'{path!r}: {exc}', RuntimeWarning, stacklevel=3)
    try:
        yield os.getcwd()
    finally:
        os.chdir(saved_dir)


@contextlib.contextmanager
def temp_cwd(name='tempcwd', quiet=False):
    """Create a temp dir and ``chdir`` into it for the block."""
    with temp_dir(quiet=quiet) as temp_path:
        with change_cwd(temp_path, quiet=quiet) as cwd_dir:
            yield cwd_dir


@contextlib.contextmanager
def temp_umask(umask):
    """Temporarily set the process umask (no-op where unsupported)."""
    old = None
    try:
        old = os.umask(umask)
    except (AttributeError, OSError):
        yield
        return
    try:
        yield
    finally:
        os.umask(old)


# ---------------------------------------------------------------------------
# Symlink support probing
# ---------------------------------------------------------------------------

def _can_symlink():
    try:
        os.symlink  # noqa: B018 - attribute probe
    except AttributeError:
        return False
    symlink_path = TESTFN + "can_symlink"
    try:
        os.symlink(TESTFN, symlink_path)
        can = True
    except (OSError, NotImplementedError, AttributeError):
        can = False
    else:
        unlink(symlink_path)
    return can


_can_symlink_value = None


def can_symlink():
    global _can_symlink_value
    if _can_symlink_value is None:
        _can_symlink_value = _can_symlink()
    return _can_symlink_value


def skip_unless_symlink(test):
    """Decorator skipping *test* when symlinks are unavailable."""
    import unittest
    ok = can_symlink()
    msg = "Requires functional symlink implementation"
    return test if ok else unittest.skip(msg)(test)


# ---------------------------------------------------------------------------
# EnvironmentVarGuard
# ---------------------------------------------------------------------------

class EnvironmentVarGuard:
    """Mutate ``os.environ`` and restore it verbatim on exit.

    A small mutable-mapping shim (CPython subclasses
    ``collections.abc.MutableMapping`` but that ABC isn't part of
    WeavePy's frozen ``collections`` yet, so the handful of methods tests
    touch are spelled out directly).
    """

    def __init__(self):
        self._environ = os.environ
        self._changed = {}

    def __getitem__(self, envvar):
        return self._environ[envvar]

    def __contains__(self, envvar):
        return envvar in self._environ

    def get(self, envvar, default=None):
        return self._environ.get(envvar, default)

    def items(self):
        return self._environ.items()

    def values(self):
        return self._environ.values()

    def __setitem__(self, envvar, value):
        if envvar not in self._changed:
            self._changed[envvar] = self._environ.get(envvar)
        self._environ[envvar] = value

    def __delitem__(self, envvar):
        if envvar not in self._changed:
            self._changed[envvar] = self._environ.get(envvar)
        if envvar in self._environ:
            del self._environ[envvar]

    def keys(self):
        return self._environ.keys()

    def __iter__(self):
        return iter(self._environ)

    def __len__(self):
        return len(self._environ)

    def set(self, envvar, value):
        self[envvar] = value

    def unset(self, envvar, *envvars):
        del self[envvar]
        for ev in envvars:
            del self[ev]

    def copy(self):
        return dict(self._environ)

    def __enter__(self):
        return self

    def __exit__(self, *ignore_exc):
        for k, v in self._changed.items():
            if v is None:
                if k in self._environ:
                    del self._environ[k]
            else:
                self._environ[k] = v
        self._changed.clear()


# ---------------------------------------------------------------------------
# Misc small helpers
# ---------------------------------------------------------------------------

def fd_status_supported():
    return hasattr(os, "fstat")


@contextlib.contextmanager
def save_mode(path, quiet=False):
    """Save and restore the permission bits of *path*."""
    try:
        mode = os.stat(path).st_mode
    except OSError:
        if quiet:
            yield
            return
        raise
    try:
        yield
    finally:
        try:
            os.chmod(path, mode)
        except OSError:
            if not quiet:
                raise


def unlink_or_skip(filename):
    unlink(filename)
