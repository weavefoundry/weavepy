"""`hash`, `str` and three-argument `pow` of big integers.

`hash(n)` folds the magnitude's words modulo `2**61 - 1`, `str(n)`
converts in base `10**9` (larger values keep the general conversion),
and `pow(b, e, m)` takes machine words for a word-size modulus and
`modpow` for a larger one. Each is checked against its definition in
Python arithmetic, over signs and word boundaries.
"""

import random

P = 2**61 - 1


def ref_hash(n):
    h = abs(n) % P
    if n < 0:
        h = -h
    return -2 if h == -1 else h


def ref_str(n):
    if n == 0:
        return "0"
    s, neg, n = [], n < 0, abs(n)
    while n:
        n, d = divmod(n, 10**18)
        s.append("%018d" % d if n else str(d))
    return ("-" if neg else "") + "".join(reversed(s))


def ref_pow(b, e, m):
    r, b2 = 1 % abs(m), b % abs(m)
    while e:
        if e & 1:
            r = r * b2 % abs(m)
        e >>= 1
        b2 = b2 * b2 % abs(m)
    return r + m if m < 0 and r else r


rng = random.Random(3)
edge = [0, 1, -1, P - 1, P, P + 1, 2 * P, -P, 2**61, 2**63, -(2**63) - 1, 2**64, 2**64 - 1, 10**9, 10**18 - 1, 3**400 + 5]
for n in edge + [rng.getrandbits(rng.randint(60, 9000)) * rng.choice([1, -1]) for _ in range(300)]:
    assert hash(n) == ref_hash(n), n
    assert str(n) == ref_str(n), n
    assert int(str(n)) == n
for _ in range(1500):
    b = rng.getrandbits(rng.choice([1, 63, 64, 65, 300])) * rng.choice([1, -1])
    e = rng.getrandbits(rng.choice([0, 1, 7, 64, 70]))
    m = (rng.getrandbits(rng.choice([1, 32, 63, 64, 65, 200])) or 1) * rng.choice([1, -1])
    assert pow(b, e, m) == ref_pow(b, e, m), (b, e, m)
assert pow(3, -1, 7) == 5
try:
    pow(2, -1, 4)
except ValueError:
    pass
else:
    raise AssertionError("no raise")
