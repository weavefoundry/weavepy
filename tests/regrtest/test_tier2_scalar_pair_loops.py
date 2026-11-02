"""Tier 2 keeps tuple-target loops over scalar pairs and arithmetic-heavy
pair loops: `for i, c in enumerate(data)` and an n-body pair loop compile
and give the same results as the interpreter."""

import os
import subprocess
import sys

PROGRAM = r'''
def scramble(plain, key):
    out = []
    klen = len(key)
    for i, c in enumerate(plain):
        out.append((c ^ key[i % klen]) & 0xFF)
    return bytes(out)

def advance(bodies, pairs, dt):
    for i, j in pairs:
        bi = bodies[i]
        bj = bodies[j]
        dx = bi[0] - bj[0]
        dy = bi[1] - bj[1]
        d2 = dx * dx + dy * dy + 0.01
        mag = dt / (d2 * (d2 ** 0.5))
        bi[2] -= dx * bodies[j][4] * mag
        bi[3] -= dy * bodies[j][4] * mag
        bj[2] += dx * bodies[i][4] * mag
        bj[3] += dy * bodies[i][4] * mag
    for b in bodies:
        b[0] += dt * b[2]
        b[1] += dt * b[3]

plain = bytes(range(256)) * 4
key = bytes(range(16))
total = 0
for _ in range(300):
    total += sum(scramble(plain, key))
bodies = [[float(k), float(k * k % 7), 0.0, 0.0, 1.0 + k] for k in range(5)]
pairs = [(i, j) for i in range(5) for j in range(i + 1, 5)]
for _ in range(2000):
    advance(bodies, pairs, 0.001)
print(total, [round(v, 6) for b in bodies for v in b[:2]])
'''


def run(env_extra):
    env = dict(os.environ, **env_extra)
    out = subprocess.run([sys.executable, "-c", PROGRAM], capture_output=True, text=True, env=env, timeout=600)
    assert out.returncode == 0, out.stderr
    return out.stdout


def main():
    expected = run({"WEAVEPY_JIT": "0"})
    for env in ({}, {"WEAVEPY_JIT_THRESHOLD": "2"}, {"WEAVEPY_TIER2": "0"}):
        got = run(env)
        assert got == expected, (env, got, expected)
    print("tier2-scalar-pair-loops: ok")


if __name__ == "__main__":
    main()
