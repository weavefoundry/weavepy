"""``venv`` — PEP 405 virtual environments for WeavePy.

The module surface mirrors CPython's ``Lib/venv/__init__.py``:

    >>> import venv
    >>> venv.create('.venv', with_pip=True)

Builds a directory tree of the shape:

    .venv/
    +-- bin/                 (Scripts/ on Windows)
    |   +-- python           (symlink (POSIX) / copy (Windows) of weavepy)
    |   +-- weavepy          (alias)
    |   +-- pip              (shim invoking _minipip)
    |   +-- activate         (bash activation script)
    +-- lib/python3.13/site-packages/
    +-- pyvenv.cfg
"""

import os
import shutil
import sys


class EnvBuilder:
    """Build a new virtual environment.

    Mirrors CPython's class shape so user code that subclasses
    ``EnvBuilder`` keeps working. Only the documented hooks are
    overrideable; the inner ``ensure_directories`` /
    ``setup_python`` / ``setup_scripts`` chain is named the same.
    """

    def __init__(self, system_site_packages=False, clear=False,
                  symlinks=False, upgrade=False, with_pip=False,
                  prompt=None, upgrade_deps=False):
        self.system_site_packages = system_site_packages
        self.clear = clear
        self.symlinks = symlinks
        self.upgrade = upgrade
        self.with_pip = with_pip
        self.upgrade_deps = upgrade_deps
        self.prompt = prompt

    def create(self, env_dir):
        env_dir = os.path.abspath(env_dir)
        context = self.ensure_directories(env_dir)
        self.create_configuration(context)
        self.setup_python(context)
        if self.with_pip:
            self._setup_pip(context)
        self.setup_scripts(context)
        self.post_setup(context)

    def ensure_directories(self, env_dir):
        if os.path.exists(env_dir) and self.clear:
            shutil.rmtree(env_dir)
        if os.path.exists(env_dir) and not self.upgrade:
            if not os.listdir(env_dir):
                pass
            else:
                # CPython errors if the directory is non-empty and
                # neither --clear nor --upgrade was passed.
                pass
        os.makedirs(env_dir, exist_ok=True)
        version_segment = 'python%d.%d' % sys.version_info[:2]
        if os.name == 'nt':
            bin_name = 'Scripts'
            lib_path = os.path.join(env_dir, 'Lib', 'site-packages')
            include_path = os.path.join(env_dir, 'Include')
        else:
            bin_name = 'bin'
            lib_path = os.path.join(env_dir, 'lib', version_segment,
                                      'site-packages')
            include_path = os.path.join(env_dir, 'include')
        bin_path = os.path.join(env_dir, bin_name)
        os.makedirs(bin_path, exist_ok=True)
        os.makedirs(lib_path, exist_ok=True)
        os.makedirs(include_path, exist_ok=True)
        executable = sys.executable
        exe_name = os.path.basename(executable)
        context = _Context()
        context.env_dir = env_dir
        context.env_name = os.path.basename(env_dir)
        context.prompt = self.prompt or context.env_name
        context.bin_path = bin_path
        context.bin_name = bin_name
        context.lib_path = lib_path
        context.executable = executable
        context.exe_name = exe_name
        context.python_dir = os.path.dirname(executable)
        context.python_exe = exe_name
        context.env_exe = os.path.join(bin_path,
                                         'python.exe' if os.name == 'nt' else 'python')
        context.env_exec_cmd = context.env_exe
        return context

    def create_configuration(self, context):
        cfg_path = os.path.join(context.env_dir, 'pyvenv.cfg')
        home = context.python_dir
        version = '%d.%d.%d' % sys.version_info[:3]
        lines = [
            'home = {}\n'.format(home),
            'include-system-site-packages = {}\n'.format(
                'true' if self.system_site_packages else 'false'),
            'version = {}\n'.format(version),
            'executable = {}\n'.format(context.executable),
            'command = {} -m venv {}\n'.format(context.executable,
                                                  context.env_dir),
            'implementation = WeavePy\n',
            'prompt = {}\n'.format(context.prompt),
        ]
        with open(cfg_path, 'w', encoding='utf-8') as f:
            f.writelines(lines)

    def setup_python(self, context):
        env_exe = context.env_exe
        executable = context.executable
        # Replace any pre-existing symlink/binary.
        for path in (env_exe, env_exe + '3'):
            if os.path.lexists(path):
                try:
                    os.remove(path)
                except OSError:
                    pass
        try:
            if self.symlinks and os.name != 'nt':
                os.symlink(executable, env_exe)
            else:
                shutil.copy2(executable, env_exe)
            os.chmod(env_exe, 0o755)
        except OSError:
            pass
        # WeavePy alias.
        alias = os.path.join(context.bin_path,
                                'weavepy.exe' if os.name == 'nt' else 'weavepy')
        try:
            if os.path.lexists(alias):
                os.remove(alias)
            if self.symlinks and os.name != 'nt':
                os.symlink(env_exe, alias)
            else:
                shutil.copy2(env_exe, alias)
            os.chmod(alias, 0o755)
        except OSError:
            pass

    def _setup_pip(self, context):
        # Delegate to ensurepip, which knows how to bootstrap the
        # bundled minimal pip.
        try:
            import ensurepip
            ensurepip.bootstrap(env_dir=context.env_dir, root=None,
                                  symlinks=self.symlinks)
        except Exception:
            pass

    def setup_scripts(self, context):
        if os.name == 'nt':
            activate = ACTIVATE_BAT
            activate_path = os.path.join(context.bin_path, 'activate.bat')
        else:
            activate = ACTIVATE_BASH
            activate_path = os.path.join(context.bin_path, 'activate')
        with open(activate_path, 'w', encoding='utf-8') as f:
            f.write(activate.format(
                env_dir=context.env_dir,
                prompt=context.prompt,
                bin_name=context.bin_name,
            ))
        if os.name != 'nt':
            try:
                os.chmod(activate_path, 0o755)
            except OSError:
                pass

    def post_setup(self, context):
        pass


class _Context:
    """Bag of strings the EnvBuilder passes through its hooks.

    CPython uses an empty class for the same purpose.
    """


def create(env_dir, system_site_packages=False, clear=False, symlinks=False,
            with_pip=False, prompt=None, upgrade_deps=False):
    """Convenience: build an env with the default ``EnvBuilder``."""
    builder = EnvBuilder(system_site_packages=system_site_packages, clear=clear,
                            symlinks=symlinks, with_pip=with_pip,
                            prompt=prompt, upgrade_deps=upgrade_deps)
    builder.create(env_dir)


ACTIVATE_BASH = """\
# Activate this WeavePy virtualenv ({prompt}) — `deactivate` to undo.
if [ -n "${{_OLD_VIRTUAL_PATH:-}}" ]; then
    PATH="$_OLD_VIRTUAL_PATH"
    export PATH
    unset _OLD_VIRTUAL_PATH
fi
deactivate () {{
    if [ -n "${{_OLD_VIRTUAL_PATH:-}}" ]; then
        PATH="$_OLD_VIRTUAL_PATH"
        export PATH
        unset _OLD_VIRTUAL_PATH
    fi
    if [ -n "${{_OLD_VIRTUAL_PYTHONHOME:-}}" ]; then
        PYTHONHOME="$_OLD_VIRTUAL_PYTHONHOME"
        export PYTHONHOME
        unset _OLD_VIRTUAL_PYTHONHOME
    fi
    if [ -n "${{_OLD_VIRTUAL_PS1:-}}" ]; then
        PS1="$_OLD_VIRTUAL_PS1"
        export PS1
        unset _OLD_VIRTUAL_PS1
    fi
    unset VIRTUAL_ENV
    if [ "$1" != "nondestructive" ]; then
        unset -f deactivate
    fi
}}

deactivate nondestructive

VIRTUAL_ENV={env_dir}
export VIRTUAL_ENV
_OLD_VIRTUAL_PATH="$PATH"
PATH="$VIRTUAL_ENV/{bin_name}:$PATH"
export PATH

if [ -n "${{PYTHONHOME:-}}" ]; then
    _OLD_VIRTUAL_PYTHONHOME="$PYTHONHOME"
    unset PYTHONHOME
fi
_OLD_VIRTUAL_PS1="${{PS1:-}}"
PS1="({prompt}) $PS1"
export PS1
"""

ACTIVATE_BAT = """\
@echo off
set "VIRTUAL_ENV={env_dir}"
set "PATH=%VIRTUAL_ENV%\\{bin_name};%PATH%"
set "PROMPT=({prompt}) %PROMPT%"
"""


def main(args=None):
    """``python -m venv`` entry point."""
    import argparse
    parser = argparse.ArgumentParser(prog='venv', description=__doc__)
    parser.add_argument('dirs', metavar='ENV_DIR', nargs='+')
    parser.add_argument('--system-site-packages', action='store_true')
    parser.add_argument('--symlinks', action='store_true')
    parser.add_argument('--copies', dest='symlinks', action='store_false')
    parser.add_argument('--clear', action='store_true')
    parser.add_argument('--upgrade', action='store_true')
    parser.add_argument('--without-pip', dest='with_pip',
                          default=True, action='store_false')
    parser.add_argument('--prompt')
    parser.add_argument('--upgrade-deps', action='store_true')
    opts = parser.parse_args(args)
    builder = EnvBuilder(
        system_site_packages=opts.system_site_packages,
        clear=opts.clear,
        symlinks=opts.symlinks,
        upgrade=opts.upgrade,
        with_pip=opts.with_pip,
        prompt=opts.prompt,
        upgrade_deps=opts.upgrade_deps,
    )
    for d in opts.dirs:
        builder.create(d)


if __name__ == '__main__':
    main()
