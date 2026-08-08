"""Spectral norm (shootout) — float arithmetic + list subscripts."""

import os


def _eval_a(i, j):
    return 1.0 / ((i + j) * (i + j + 1) // 2 + i + 1)


def _eval_a_times_u(u):
    n = len(u)
    out = [0.0] * n
    for i in range(n):
        s = 0.0
        for j in range(n):
            s += _eval_a(i, j) * u[j]
        out[i] = s
    return out


def _eval_at_times_u(u):
    n = len(u)
    out = [0.0] * n
    for i in range(n):
        s = 0.0
        for j in range(n):
            s += _eval_a(j, i) * u[j]
        out[i] = s
    return out


def _eval_ata_times_u(u):
    return _eval_at_times_u(_eval_a_times_u(u))


def bench(n):
    u = [1.0] * n
    for _ in range(10):
        v = _eval_ata_times_u(u)
        u = _eval_ata_times_u(v)
    vbv = vv = 0.0
    for ue, ve in zip(u, v):
        vbv += ue * ve
        vv += ve * ve
    return (vbv / vv) ** 0.5


if __name__ == "__main__":
    import time

    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "60"))
    _t0 = time.perf_counter_ns()
    bench(n)
    _t1 = time.perf_counter_ns()
    print("WEAVEPY_BENCH_NS=%d" % (_t1 - _t0))
