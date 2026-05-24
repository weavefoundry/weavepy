"""Tiny pancake-flip kernel — stresses list mutation, integer
arithmetic, and tight loops. Not the canonical fannkuch-redux
(which uses reverse slicing), but the same shape: count the
flips needed to reach a permutation in increasing order."""

import os


def _flips_to_sort(n):
    perm = list(range(n))
    flips = 0
    while perm[0] != 0:
        k = perm[0]
        # Reverse perm[:k+1] in place.
        i = 0
        j = k
        while i < j:
            perm[i], perm[j] = perm[j], perm[i]
            i += 1
            j -= 1
        flips += 1
        # Rotate the list left by one to give the kernel a
        # different starting permutation each iteration; a
        # random-looking sequence keeps the JIT-style cache
        # honest without depending on a real RNG.
        first = perm[0]
        for idx in range(len(perm) - 1):
            perm[idx] = perm[idx + 1]
        perm[-1] = first
    return flips


def bench(n):
    out = 0
    for _ in range(n):
        out = _flips_to_sort(7)
    return out


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "1"))
    bench(n)
