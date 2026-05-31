"""``test.libregrtest.save_env`` — the environment-mutation guard.

A faithful subset of CPython 3.13's
``Lib/test/libregrtest/save_env.py``. Snapshots a handful of process
globals around each test and restores them; the names that a test left
mutated (and that we had to put back) are reported so the runner can
flag ``ENV_CHANGED``.
"""

import os
import sys


class saved_test_environment:
    """Capture and restore selected process state around a test.

    Only the resources that real tests routinely (and accidentally)
    perturb are tracked: ``os.environ``, ``sys.path``, ``sys.argv``, the
    current working directory and ``warnings.filters``.
    """

    # Names of the resources we guard. Each has a ``get_<name>`` /
    # ``restore_<name>`` pair below.
    resources = ('cwd', 'environ', 'sys_path', 'sys_argv', 'warnings_filters')

    def __init__(self, test_name, verbose=0, quiet=False):
        self.test_name = test_name
        self.verbose = verbose
        self.quiet = quiet
        self.changed = False

    # -- cwd --
    def get_cwd(self):
        return os.getcwd()

    def restore_cwd(self, saved):
        try:
            os.chdir(saved)
        except OSError:
            pass

    # -- os.environ --
    def get_environ(self):
        return dict(os.environ)

    def restore_environ(self, saved):
        current = dict(os.environ)
        if current != saved:
            for key in list(current):
                if key not in saved:
                    try:
                        del os.environ[key]
                    except KeyError:
                        pass
            for key, value in saved.items():
                os.environ[key] = value

    # -- sys.path --
    def get_sys_path(self):
        return (id(sys.path), sys.path, sys.path[:])

    def restore_sys_path(self, saved):
        sys.path = saved[1]
        sys.path[:] = saved[2]

    # -- sys.argv --
    def get_sys_argv(self):
        return (id(sys.argv), sys.argv, sys.argv[:])

    def restore_sys_argv(self, saved):
        sys.argv = saved[1]
        sys.argv[:] = saved[2]

    # -- warnings.filters --
    def get_warnings_filters(self):
        try:
            import warnings
        except ImportError:
            return None
        return (id(warnings.filters), warnings.filters, warnings.filters[:])

    def restore_warnings_filters(self, saved):
        if saved is None:
            return
        import warnings
        warnings.filters = saved[1]
        warnings.filters[:] = saved[2]

    def __enter__(self):
        self.saved_values = {}
        for name in self.resources:
            get = getattr(self, 'get_' + name)
            try:
                self.saved_values[name] = get()
            except Exception:
                self.saved_values[name] = None
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        for name in self.resources:
            saved = self.saved_values.get(name)
            get = getattr(self, 'get_' + name)
            restore = getattr(self, 'restore_' + name)
            try:
                current = get()
            except Exception:
                current = saved
            # For the (id, container, copy) snapshots, compare the copy.
            changed = self._differs(name, saved, current)
            if changed:
                self.changed = True
                restore(saved)
                if not self.quiet and self.verbose:
                    print("Warning -- %s was modified by %s" %
                          (name, self.test_name), file=sys.stderr)
        return False

    def _differs(self, name, saved, current):
        if saved is None or current is None:
            return False
        if isinstance(saved, tuple) and len(saved) == 3:
            # (id, container, copy): compare the saved copy to the live one.
            return current[2] != saved[2]
        return saved != current
