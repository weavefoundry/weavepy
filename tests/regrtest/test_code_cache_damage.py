"""A damaged code cache fails at most one run.

A cached function's body decodes when it first runs, too late to fall back to
compiling, so the run that finds the damage may fail; the cache must then be
gone so that the next run compiles again.
"""

import os
import subprocess
import sys
import tempfile
import unittest

PROGRAM = "import json; print(json.dumps({'a': [1, 2]}), json.loads('[3]'))\n"
EXPECTED = '{"a": [1, 2]} [3]\n'


def run(cache):
    env = dict(os.environ, WEAVEPY_FROZEN_CACHE=cache)
    env.pop("PYTHONDONTWRITEBYTECODE", None)
    return subprocess.run(
        [sys.executable, "-c", PROGRAM],
        capture_output=True,
        text=True,
        env=env,
        timeout=120,
    )


class DamagedCache(unittest.TestCase):
    def test_damage_fails_at_most_one_run(self):
        with tempfile.TemporaryDirectory() as cache:
            self.assertEqual(run(cache).stdout, EXPECTED)
            artifacts = [
                os.path.join(root, name)
                for root, _, names in os.walk(cache)
                for name in names
                if "json" in name
            ]
            if not artifacts:
                self.skipTest("this interpreter keeps no code cache here")
            for path in artifacts:
                with open(path, "r+b") as f:
                    data = bytearray(f.read())
                    # Past the header and module table: function bodies.
                    start = len(data) // 3
                    data[start:] = b"\xff" * (len(data) - start)
                    f.seek(0)
                    f.write(data)
            run(cache)
            # Whatever the damaged run did, the next one is healthy.
            again = run(cache)
            self.assertEqual(again.returncode, 0, again.stderr[-2000:])
            self.assertEqual(again.stdout, EXPECTED)


if __name__ == "__main__":
    unittest.main()
