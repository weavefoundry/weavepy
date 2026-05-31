"""``test.support.script_helper`` — spawn-the-interpreter helpers.

Faithful subset of CPython 3.13's
``Lib/test/support/script_helper.py``. These shell out to the running
``weavepy`` binary (``sys.executable``), so they exercise the real CLI:
``assert_python_ok`` / ``assert_python_failure``, ``spawn_python`` /
``kill_python`` / ``run_python_until_end``, and the ``make_script`` /
``make_pkg`` / ``make_zip_script`` builders.
"""

import collections
import os
import os.path
import subprocess
import sys


# Cache: does the interpreter need a clean environment to start?
__cached_interp_requires_environment = None


def interpreter_requires_environment():
    """``True`` if the interpreter cannot start with a scrubbed env.

    WeavePy starts fine from an empty environment, so this is almost
    always ``False``; we still probe once and cache, like CPython.
    """
    global __cached_interp_requires_environment
    if __cached_interp_requires_environment is None:
        env = dict(os.environ)
        env.pop('PYTHONHOME', None)
        try:
            proc = subprocess.run([sys.executable, '-E', '-c', 'pass'],
                                  env=env,
                                  stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE)
            __cached_interp_requires_environment = proc.returncode != 0
        except Exception:
            __cached_interp_requires_environment = False
    return __cached_interp_requires_environment


class _PythonRunResult(collections.namedtuple(
        '_PythonRunResult', ('rc', 'out', 'err'))):
    """Holds the result of running the interpreter in a subprocess."""

    def fail(self, cmd_line):
        if self.rc and self.rc != -2:
            try:
                exc_msg = self.err.decode('ascii', 'replace')
            except Exception:
                exc_msg = repr(self.err)
            err = exc_msg.rstrip()
        else:
            err = ''
        out = self.out.decode('ascii', 'replace').rstrip()
        raise AssertionError(
            "Process return code is %d\n"
            "command line: %r\n"
            "\n"
            "stdout:\n---\n%s\n---\n"
            "\n"
            "stderr:\n---\n%s\n---"
            % (self.rc, cmd_line, out, err))


def _assert_python(expected_success, /, *args, **env_vars):
    quiet = env_vars.pop('__quiet__', False)
    isolated = env_vars.pop('__isolated__', True)
    cleanenv = env_vars.pop('__cleanenv__', None)
    cwd = env_vars.pop('__cwd__', None)

    env = None
    if cleanenv or env_vars or isolated:
        env = dict(os.environ)
        for k in list(env_vars):
            env[k] = env_vars[k]
    cmd_line = [sys.executable]
    if isolated:
        # weavepy accepts (and ignores) -I/-E flags; keep CPython shape.
        cmd_line.append('-I')
    elif '__isolated__' not in env_vars:
        cmd_line.append('-E')
    cmd_line.extend(args)
    try:
        proc = subprocess.run(cmd_line,
                              stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE,
                              env=env,
                              cwd=cwd)
        out, err, rc = proc.stdout, proc.stderr, proc.returncode
    except Exception as exc:
        # Treat a spawn failure as an environment skip rather than a hang.
        import unittest
        raise unittest.SkipTest(
            f"cannot spawn the interpreter ({sys.executable!r}): {exc}")
    res = _PythonRunResult(rc, out, err)
    if (rc and expected_success) or (not rc and not expected_success):
        if not quiet:
            res.fail(cmd_line)
    return res, cmd_line


def assert_python_ok(*args, **env_vars):
    """Run ``weavepy *args`` and assert it exits 0."""
    res, _cmd = _assert_python(True, *args, **env_vars)
    return res


def assert_python_failure(*args, **env_vars):
    """Run ``weavepy *args`` and assert it exits non-zero."""
    res, _cmd = _assert_python(False, *args, **env_vars)
    return res


def spawn_python(*args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                 **kw):
    """``Popen`` the interpreter for streaming/interactive tests."""
    cmd_line = [sys.executable, '-E']
    cmd_line.extend(args)
    return subprocess.Popen(cmd_line, stdin=subprocess.PIPE,
                            stdout=stdout, stderr=stderr, **kw)


def kill_python(p):
    """Close stdin, drain stdout, wait. Returns captured stdout."""
    p.stdin.close()
    data = p.stdout.read()
    p.stdout.close()
    p.wait()
    return data


def run_python_until_end(*args, **env_vars):
    return _assert_python(None, *args, __quiet__=True, **env_vars)


# ---------------------------------------------------------------------------
# Script / package / zip builders
# ---------------------------------------------------------------------------

def make_script(script_dir, script_basename, source, omit_suffix=False):
    script_filename = script_basename
    if not omit_suffix:
        script_filename += os.extsep + 'py'
    script_name = os.path.join(script_dir, script_filename)
    with open(script_name, 'w', encoding='utf-8') as script_file:
        script_file.write(source)
    return script_name


def make_pkg(pkg_dir, init_source=''):
    os.mkdir(pkg_dir)
    make_script(pkg_dir, '__init__', init_source)


def make_zip_script(zip_dir, zip_basename, script_name, name_in_zip=None):
    import zipfile
    zip_filename = zip_basename + os.extsep + 'zip'
    zip_name = os.path.join(zip_dir, zip_filename)
    with zipfile.ZipFile(zip_name, 'w') as zip_file:
        if name_in_zip is None:
            parts = script_name.split(os.sep)
            if len(parts) >= 2 and parts[-2] == '__pycache__':
                legacy_pyc = os.path.basename(script_name)
                name_in_zip = legacy_pyc
            else:
                name_in_zip = os.path.basename(script_name)
        zip_file.write(script_name, name_in_zip)
    return zip_name, os.path.join(zip_name, name_in_zip)


def make_zip_pkg(zip_dir, zip_basename, pkg_name, script_basename,
                 source, depth=1, compiled=False):
    import zipfile
    unlink = []
    init_name = make_script(zip_dir, '__init__', '')
    unlink.append(init_name)
    init_basename = os.path.basename(init_name)
    script_name = make_script(zip_dir, script_basename, source)
    unlink.append(script_name)
    pkg_names = [os.sep.join([pkg_name] * i) for i in range(1, depth + 1)]
    script_name_in_zip = os.path.join(pkg_names[-1],
                                      os.path.basename(script_name))
    zip_filename = zip_basename + os.extsep + 'zip'
    zip_name = os.path.join(zip_dir, zip_filename)
    with zipfile.ZipFile(zip_name, 'w') as zip_file:
        for name in pkg_names:
            init_name_in_zip = os.path.join(name, init_basename)
            zip_file.write(init_name, init_name_in_zip)
        zip_file.write(script_name, script_name_in_zip)
    for name in unlink:
        os.unlink(name)
    return zip_name, os.path.join(zip_name, script_name_in_zip)
