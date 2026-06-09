"""Access to WeavePy's configuration information.

A faithful-in-shape, minimal implementation of CPython's ``sysconfig``
public API. ``sysconfig`` is *definitionally* implementation-provided —
its values describe the running interpreter's build — so the honest
WeavePy answer reports WeavePy's own layout: a self-contained binary
whose stdlib is frozen into the executable (no on-disk ``lib/pythonX.Y``
tree). Consumers in the conformance surface (`pydoc.getdocloc`,
`platform`, `site`) only need the call shapes and sane strings.
"""

import os
import sys

__all__ = [
    'get_config_h_filename',
    'get_config_var',
    'get_config_vars',
    'get_default_scheme',
    'get_makefile_filename',
    'get_path',
    'get_path_names',
    'get_paths',
    'get_platform',
    'get_python_version',
    'get_scheme_names',
    'is_python_build',
    'parse_config_h',
]

_PY_VERSION = sys.version.split()[0]
_PY_VERSION_SHORT = '.'.join(_PY_VERSION.split('.')[:2])
_PY_VERSION_SHORT_NO_DOT = _PY_VERSION_SHORT.replace('.', '')

_PREFIX = getattr(sys, 'prefix', '') or os.path.dirname(
    getattr(sys, 'executable', '') or '/usr/local/bin')
_EXEC_PREFIX = getattr(sys, 'exec_prefix', _PREFIX) or _PREFIX

# One scheme; WeavePy is a single self-contained binary, so every
# location resolves under the executable's prefix.
_SCHEME = {
    'stdlib': '{installed_base}/lib/python{py_version_short}',
    'platstdlib': '{platbase}/lib/python{py_version_short}',
    'purelib': '{base}/lib/python{py_version_short}/site-packages',
    'platlib': '{platbase}/lib/python{py_version_short}/site-packages',
    'include': '{installed_base}/include/python{py_version_short}',
    'platinclude': '{installed_platbase}/include/python{py_version_short}',
    'scripts': '{base}/bin',
    'data': '{base}',
}

_CONFIG_VARS = None


def _expand(template, vars):
    out = template
    for key, value in vars.items():
        out = out.replace('{%s}' % key, str(value))
    return out


def _init_config_vars():
    global _CONFIG_VARS
    if _CONFIG_VARS is None:
        _CONFIG_VARS = {
            'prefix': _PREFIX,
            'exec_prefix': _EXEC_PREFIX,
            'base': _PREFIX,
            'platbase': _EXEC_PREFIX,
            'installed_base': _PREFIX,
            'installed_platbase': _EXEC_PREFIX,
            'py_version': _PY_VERSION,
            'py_version_short': _PY_VERSION_SHORT,
            'py_version_nodot': _PY_VERSION_SHORT_NO_DOT,
            'abiflags': '',
            'EXT_SUFFIX': '.so',
            'SOABI': 'weavepy',
            'Py_DEBUG': 0,
            'Py_ENABLE_SHARED': 0,
            'Py_GIL_DISABLED': 0,
            'LIBDIR': os.path.join(_PREFIX, 'lib'),
            'INCLUDEPY': os.path.join(_PREFIX, 'include',
                                      'python' + _PY_VERSION_SHORT),
            'projectbase': os.path.dirname(
                getattr(sys, 'executable', '') or _PREFIX),
            'platlibdir': 'lib',
            'userbase': os.path.expanduser('~/.local'),
        }
    return _CONFIG_VARS


def get_config_vars(*args):
    vars = _init_config_vars()
    if args:
        return [vars.get(name) for name in args]
    return vars


def get_config_var(name):
    return get_config_vars().get(name)


def get_scheme_names():
    return ('weavepy',)


def get_default_scheme():
    return 'weavepy'


def get_path_names():
    return tuple(_SCHEME)


def get_paths(scheme=get_default_scheme(), vars=None, expand=True):
    all_vars = dict(_init_config_vars())
    if vars is not None:
        all_vars.update(vars)
    if expand:
        return {name: _expand(template, all_vars)
                for name, template in _SCHEME.items()}
    return dict(_SCHEME)


def get_path(name, scheme=get_default_scheme(), vars=None, expand=True):
    paths = get_paths(scheme, vars, expand)
    try:
        return paths[name]
    except KeyError:
        raise KeyError('unknown path name %r' % (name,)) from None


def get_python_version():
    return _PY_VERSION_SHORT


def get_platform():
    if sys.platform == 'darwin':
        import platform as _platform
        machine = _platform.machine() or 'arm64'
        return 'macosx-11.0-%s' % machine
    if sys.platform.startswith('linux'):
        import platform as _platform
        machine = _platform.machine() or 'x86_64'
        return 'linux-%s' % machine
    return sys.platform


def is_python_build(check_home=None):
    return False


def get_makefile_filename():
    return os.path.join(get_path('stdlib'), 'config', 'Makefile')


def get_config_h_filename():
    return os.path.join(get_path('platinclude'), 'pyconfig.h')


def parse_config_h(fp, vars=None):
    """Parse a config.h-style file (name/value pairs)."""
    import re
    if vars is None:
        vars = {}
    define_rx = re.compile('#define ([A-Z][A-Za-z0-9_]+) (.*)\n')
    undef_rx = re.compile('/[*] #undef ([A-Z][A-Za-z0-9_]+) [*]/\n')
    while True:
        line = fp.readline()
        if not line:
            break
        m = define_rx.match(line)
        if m:
            n, v = m.group(1, 2)
            try:
                v = int(v)
            except ValueError:
                pass
            vars[n] = v
        else:
            m = undef_rx.match(line)
            if m:
                vars[m.group(1)] = 0
    return vars
