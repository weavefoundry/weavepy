"""The frozen code cache keys each artifact by its module's source.

Artifacts are named `<module>.<source hash>`, so builds whose embedded
sources differ keep one each instead of replacing each other's at every
start. Writing one removes the same module's artifacts for other sources
(and an earlier layout's unhashed one) once they are a day old, and
keeps fresher ones.
"""

import os
import subprocess
import sys
import tempfile
import time


def run(cache):
    env = dict(os.environ)
    env['WEAVEPY_FROZEN_CACHE'] = cache
    env.pop('PYTHONDONTWRITEBYTECODE', None)
    result = subprocess.run([sys.executable, '-c', 'import json; print(json.dumps(1))'],
                            env=env, capture_output=True, text=True, timeout=120)
    assert result.returncode == 0, (result.stdout, result.stderr)
    return result.stdout


def artifact_dir(cache):
    for root, _dirs, files in os.walk(cache):
        if any(f.split('.')[0] == 'json' for f in files):
            return root
    return None


if sys.implementation.name == 'weavepy':
    with tempfile.TemporaryDirectory() as cache:
        assert run(cache) == '1\n'
        directory = artifact_dir(cache)
        assert directory is not None
        mine = [f for f in os.listdir(directory)
                if f.startswith('json.') and f.count('.') == 1]
        assert len(mine) == 1, mine
        # Another build's artifacts: an old and a fresh one, and an old
        # unhashed one.
        old = time.time() - 3 * 24 * 3600
        stale = os.path.join(directory, 'json.' + '0' * 16)
        fresh = os.path.join(directory, 'json.' + 'f' * 16)
        legacy = os.path.join(directory, 'json')
        for path in (stale, fresh, legacy):
            with open(path, 'wb') as f:
                f.write(b'not an artifact')
        os.utime(stale, (old, old))
        os.utime(legacy, (old, old))
        # A start that writes `json` again (its own artifact gone).
        os.remove(os.path.join(directory, mine[0]))
        assert run(cache) == '1\n'
        assert os.path.exists(os.path.join(directory, mine[0]))
        assert not os.path.exists(stale)
        assert not os.path.exists(legacy)
        assert os.path.exists(fresh)
        # And a start that finds its own reads it, writing nothing.
        before = os.stat(os.path.join(directory, mine[0])).st_mtime_ns
        assert run(cache) == '1\n'
        assert os.stat(os.path.join(directory, mine[0])).st_mtime_ns == before

print('frozen cache source keys: ok')
