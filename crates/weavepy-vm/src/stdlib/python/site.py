"""Append module search paths for third-party packages.

This module is automatically imported during the interpreter
bootstrap (unless ``-S`` was passed). It looks at the environment
the binary is running under and extends ``sys.path`` with the
appropriate site-packages directories, then walks each one for
``.pth`` files (PEP 405-shaped, see ``python -c 'import site;
help(site)'``).

The implementation mirrors CPython's ``Lib/site.py`` closely
enough that user code that introspects ``site.PREFIXES``,
``site.USER_SITE``, or ``site.getsitepackages()`` keeps working.
"""

import os
import sys

ENABLE_USER_SITE = None
USER_SITE = None
USER_BASE = None
PREFIXES = []


def _is_64bit():
    return sys.maxsize > 2 ** 32


def _abs_paths():
    """Force every ``sys.path`` entry to an absolute path so that
    ``import`` doesn't trip when cwd changes mid-run.
    """
    for i, p in enumerate(sys.path):
        try:
            sys.path[i] = os.path.abspath(p)
        except Exception:
            pass


def _remove_duplicate_paths():
    seen = set()
    out = []
    for p in sys.path:
        if p in seen:
            continue
        seen.add(p)
        out.append(p)
    sys.path[:] = out


def _init_pathinfo():
    """Snapshot the directories currently in ``sys.path``."""
    d = set()
    for item in sys.path:
        try:
            if item and os.path.isdir(item):
                d.add(os.path.realpath(item))
        except (TypeError, AttributeError):
            continue
    return d


def addsitedir(sitedir, known_paths=None):
    """Append ``sitedir`` to ``sys.path`` and process its ``.pth`` files."""
    if known_paths is None:
        known_paths = _init_pathinfo()
        reset = True
    else:
        reset = False
    try:
        sitedir_real = os.path.realpath(sitedir)
    except Exception:
        return known_paths
    if sitedir_real not in known_paths:
        sys.path.append(sitedir)
        known_paths.add(sitedir_real)
    try:
        names = sorted(os.listdir(sitedir))
    except OSError:
        return known_paths
    for name in names:
        if name.endswith('.pth') and not name.startswith('.'):
            addpackage(sitedir, name, known_paths)
    if reset:
        known_paths = None
    return known_paths


def addpackage(sitedir, name, known_paths):
    """Process one ``.pth`` file inside ``sitedir``."""
    if known_paths is None:
        known_paths = _init_pathinfo()
    fullname = os.path.join(sitedir, name)
    try:
        with open(fullname, 'r', encoding='utf-8') as f:
            lines = f.readlines()
    except OSError:
        return known_paths
    for n, line in enumerate(lines):
        if line.startswith('#'):
            continue
        line = line.rstrip()
        if not line:
            continue
        if line.startswith(('import ', 'import\t')):
            try:
                exec(line)
            except Exception:
                sys.stderr.write(
                    'Error processing line %d of %s:\n' % (n + 1, fullname))
            continue
        # Plain directory entry.
        dirpath = os.path.abspath(os.path.join(sitedir, line))
        try:
            real = os.path.realpath(dirpath)
        except Exception:
            real = dirpath
        if real not in known_paths and os.path.exists(dirpath):
            sys.path.append(dirpath)
            known_paths.add(real)
    return known_paths


def _get_path(prefix, is_user=False):
    """Compose the canonical site-packages path for a prefix."""
    py = 'python%d.%d' % sys.version_info[:2]
    if os.name == 'nt':
        if is_user:
            return os.path.join(prefix, 'Python%d%d' % sys.version_info[:2], 'site-packages')
        return os.path.join(prefix, 'Lib', 'site-packages')
    if is_user:
        return os.path.join(prefix, 'lib', py, 'site-packages')
    return os.path.join(prefix, 'lib', py, 'site-packages')


def getuserbase():
    """Return the user base directory (``~/.local`` style)."""
    global USER_BASE
    if USER_BASE is not None:
        return USER_BASE
    base = os.environ.get('PYTHONUSERBASE')
    if base:
        USER_BASE = base
        return USER_BASE
    home = os.path.expanduser('~')
    if os.name == 'nt':
        appdata = os.environ.get('APPDATA') or home
        USER_BASE = os.path.join(appdata, 'Python')
    elif sys.platform == 'darwin':
        framework = sys.platform == 'darwin'
        if framework:
            USER_BASE = os.path.join(home, 'Library', 'Python',
                                      '%d.%d' % sys.version_info[:2])
        else:
            USER_BASE = os.path.join(home, '.local')
    else:
        xdg = os.environ.get('XDG_DATA_HOME')
        if xdg:
            USER_BASE = os.path.join(xdg, 'python')
        else:
            USER_BASE = os.path.join(home, '.local')
    return USER_BASE


def getusersitepackages():
    """Return the user site-packages directory."""
    global USER_SITE
    if USER_SITE is not None:
        return USER_SITE
    base = getuserbase()
    USER_SITE = _get_path(base, is_user=True)
    return USER_SITE


def getsitepackages(prefixes=None):
    """Return the full list of site-packages directories."""
    out = []
    if prefixes is None:
        prefixes = PREFIXES
    seen = set()
    for prefix in prefixes:
        if not prefix or prefix in seen:
            continue
        seen.add(prefix)
        out.append(_get_path(prefix))
    return out


def _get_prefixes():
    prefix = getattr(sys, 'prefix', '')
    exec_prefix = getattr(sys, 'exec_prefix', prefix)
    out = []
    for p in (prefix, exec_prefix):
        if p and p not in out:
            out.append(p)
    return out


def addusersitepackages(known_paths):
    """Append the user site-packages, unless disabled."""
    global ENABLE_USER_SITE
    user_site = getusersitepackages()
    if ENABLE_USER_SITE and os.path.isdir(user_site):
        addsitedir(user_site, known_paths)
    return known_paths


def check_enableusersite():
    """Decide whether to honour the user site directory."""
    if hasattr(sys.flags, 'no_user_site') and sys.flags.no_user_site:
        return False
    if hasattr(sys.flags, 'isolated') and sys.flags.isolated:
        return False
    return True


def venv(known_paths):
    """If we're inside a venv (``pyvenv.cfg`` present next to the
    executable's parent), adjust ``sys.prefix`` / ``sys.base_prefix``
    accordingly. Mirrors CPython's :pep:`405` handling.
    """
    env_dir = os.path.dirname(os.path.dirname(sys.executable))
    cfg = os.path.join(env_dir, 'pyvenv.cfg')
    if not os.path.isfile(cfg):
        return known_paths
    try:
        with open(cfg, 'r', encoding='utf-8') as f:
            lines = f.readlines()
    except OSError:
        return known_paths
    settings = {}
    for line in lines:
        if '=' not in line:
            continue
        k, _, v = line.partition('=')
        settings[k.strip()] = v.strip()
    home = settings.get('home')
    if home:
        sys.base_prefix = os.path.dirname(home)
        sys.base_exec_prefix = sys.base_prefix
    sys.prefix = env_dir
    sys.exec_prefix = env_dir
    include_system = settings.get('include-system-site-packages',
                                   'false').lower() == 'true'
    if not include_system:
        # Replace PREFIXES so getsitepackages() returns the venv
        # site only.
        global PREFIXES
        PREFIXES = [env_dir]
    return known_paths


def main():
    """Entry point invoked from the interpreter bootstrap."""
    global ENABLE_USER_SITE, PREFIXES
    _abs_paths()
    # Establish base prefixes.
    if not getattr(sys, 'prefix', None):
        # Best-effort guess from the executable.
        exe = getattr(sys, 'executable', '')
        prefix = os.path.dirname(os.path.dirname(exe)) if exe else ''
        sys.prefix = prefix or os.getcwd()
    if not getattr(sys, 'exec_prefix', None):
        sys.exec_prefix = sys.prefix
    if not getattr(sys, 'base_prefix', None):
        sys.base_prefix = sys.prefix
    if not getattr(sys, 'base_exec_prefix', None):
        sys.base_exec_prefix = sys.exec_prefix
    PREFIXES = _get_prefixes()
    known_paths = _init_pathinfo()
    known_paths = venv(known_paths)
    ENABLE_USER_SITE = check_enableusersite()
    for path in getsitepackages():
        if os.path.isdir(path):
            addsitedir(path, known_paths)
    addusersitepackages(known_paths)
    _remove_duplicate_paths()
    # `sitecustomize`/`usercustomize` are optional hooks for sysadmins
    # and individual users; failures are swallowed.
    try:
        import sitecustomize  # noqa: F401
    except Exception:
        pass
    try:
        import usercustomize  # noqa: F401
    except Exception:
        pass


def gethistoryfile():
    """Hook used by ``code.interact``; returns the REPL history path."""
    if not sys.flags.ignore_environment:
        history = os.environ.get('PYTHON_HISTORY')
        if history:
            return history
    return os.path.join(os.path.expanduser('~'), '.python_history')
