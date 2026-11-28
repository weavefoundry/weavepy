"""A colliding instance-dict key's `__eq__` sees its real caller.

An object with `__getattr__` and an instance dict holding a key whose hash
collides with an attribute name: the generic lookup compares that key
with the name, calling its Python `__eq__`. Compiled code's check for a
certain miss must not make that call itself (frameless, the `__eq__` saw
the caller's caller); it leaves it to the full lookup. Run under each
tier setting.
"""

import os
import subprocess
import sys

PROGRAM = r'''
import sys

class Missing:
    def __init__(self):
        self._value = 7
    def __getattr__(self, name):
        if name == "value":
            return self._value
        if name == "other":
            return self._value + 4
        raise AttributeError(name)

def key_read(root, n):
    total = 0
    for _ in range(n):
        total += root.value
        total += root.other
    return total

missing = Missing()
for _ in range(2):
    assert key_read(missing, 1200) == 21600

events = []

class Key:
    def __hash__(self):
        return hash("value")
    def __eq__(self, name):
        events.append(sys._getframe(1).f_code.co_name)
        return name == "value"

missing.__dict__[Key()] = 23
assert key_read(missing, 3) == 102
print(events)
'''

EXPECTED = "['key_read', 'key_read', 'key_read']\n"

SETTINGS = [
    {},
    {"WEAVEPY_TIER2": "0"},
    {"WEAVEPY_TIER2": "0", "WEAVEPY_FRAME_JIT_SYNC": "1"},
    {"WEAVEPY_JIT": "0", "WEAVEPY_FRAME_JIT": "0"},
    {"WEAVEPY_JIT_THRESHOLD": "2"},
]

for extra in SETTINGS:
    env = dict(os.environ, **extra)
    result = subprocess.run([sys.executable, "-c", PROGRAM], env=env,
                            capture_output=True, text=True, timeout=120)
    assert result.returncode == 0, (extra, result.stderr)
    assert result.stdout == EXPECTED, (extra, result.stdout)

print("colliding key frames: ok")
