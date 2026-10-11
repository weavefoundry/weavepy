"""The embedded standard library's code cache fills under -B.

`-B`/`PYTHONDONTWRITEBYTECODE` governs `.pyc` files beside source files;
CPython ships its standard library already compiled. WeavePy's frozen
cache is the counterpart of that installed bytecode, so a process under
`-B` still writes it (otherwise every such process compiled each stdlib
module it imported), while a user module's `.pyc` stays unwritten.
"""

import os
import subprocess
import sys
import tempfile


def cached_names(cache):
    names = set()
    for root, _dirs, files in os.walk(cache):
        names.update(files)
    return names


def run(cache, cwd, code, dont_write):
    env = dict(os.environ)
    env['WEAVEPY_FROZEN_CACHE'] = cache
    env.pop('PYTHONDONTWRITEBYTECODE', None)
    if dont_write:
        env['PYTHONDONTWRITEBYTECODE'] = '1'
    result = subprocess.run([sys.executable, '-c', code], cwd=cwd, env=env,
                            capture_output=True, text=True, timeout=120)
    assert result.returncode == 0, (result.stdout, result.stderr)
    return result.stdout


if sys.implementation.name == 'weavepy':
    with tempfile.TemporaryDirectory() as directory:
        directory = os.path.realpath(directory)
        with open(os.path.join(directory, 'frozen_cache_user_mod.py'), 'w') as f:
            f.write('VALUE = 7\n')
        cache = os.path.join(directory, 'cache')
        code = ('import sys, json, frozen_cache_user_mod\n'
                'assert sys.dont_write_bytecode\n'
                'print(json.dumps({"v": frozen_cache_user_mod.VALUE}))\n')
        assert run(cache, directory, code, True) == '{"v": 7}\n'
        names = cached_names(cache)
        assert 'json' in names or 'json.decoder' in names, sorted(names)[:20]
        # The user module's bytecode stays unwritten under -B.
        assert not os.path.exists(os.path.join(directory, '__pycache__'))
        # A second process reads what the first wrote.
        assert run(cache, directory, code, True) == '{"v": 7}\n'

print('frozen cache under -B: ok')
