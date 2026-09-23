"""Startup runs site once; -S defers it, and explicit main calls still work."""

import os
import subprocess
import sys
import tempfile
from pathlib import Path

executable = [sys.executable]
if getattr(sys.flags, 'gil', None) == 0:
    executable += ['-X', 'gil=0']


with tempfile.TemporaryDirectory() as directory:
    Path(directory, 'sitecustomize.py').write_text('''
import site
site._startup_main_calls = 0
original_main = site.main
def counted_main():
    site._startup_main_calls += 1
    return original_main()
site.main = counted_main
''')
    env = os.environ.copy()
    env['PYTHONPATH'] = directory
    env['PYTHONUSERBASE'] = str(Path(directory, 'userbase'))
    env.pop('PYTHONNOUSERSITE', None)
    location = subprocess.run(
        [*executable, '-S', '-c', 'import site; print(site.getusersitepackages())'],
        env=env, capture_output=True, text=True, timeout=30)
    assert location.returncode == 0, location.stderr
    user_site = Path(location.stdout.strip())
    user_site.mkdir(parents=True)
    Path(user_site, 'once.pth').write_text(
        'import builtins; builtins._startup_pth_calls = '
        'getattr(builtins, "_startup_pth_calls", 0) + 1\n')
    for flags, source, expected in [
        ([], 'import site; print(site._startup_main_calls)', '0\n'),
        (['-s'], 'import site; print(site._startup_main_calls)', '0\n'),
        ([], 'import site; site.main(); print(site._startup_main_calls)', '1\n'),
        (['-S'], '''
import sys
assert 'sitecustomize' not in sys.modules
import site
assert 'sitecustomize' not in sys.modules
site.main()
assert 'sitecustomize' in sys.modules
print(site._startup_main_calls)
''', '0\n'),
    ]:
        child = subprocess.run([*executable, *flags, '-c', source], env=env,
                               capture_output=True, text=True, timeout=30)
        assert child.returncode == 0, (flags, child.stdout, child.stderr)
        assert child.stdout == expected, (flags, child.stdout, expected)

    for flags, expected in [([], '1\n'), (['-s'], '0\n'), (['-S'], '0\n')]:
        child = subprocess.run(
            [*executable, *flags, '-c',
             'import builtins; print(getattr(builtins, "_startup_pth_calls", 0))'],
            env=env, capture_output=True, text=True, timeout=30)
        assert child.returncode == 0, (flags, child.stdout, child.stderr)
        assert child.stdout == expected, (flags, child.stdout, expected)

print('site startup: ok')
