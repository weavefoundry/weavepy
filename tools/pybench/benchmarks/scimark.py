"""SciMark-style kernels: successive over-relaxation, LU factorization,
sparse matrix multiply, and Monte Carlo integration, over lists of lists."""

import random

WORK = 60


def sor(n, cycles, omega=1.25):
    g = [[(i * j) % 7 / 7.0 for j in range(n)] for i in range(n)]
    for _ in range(cycles):
        for i in range(1, n - 1):
            gi, gim, gip = g[i], g[i - 1], g[i + 1]
            for j in range(1, n - 1):
                gi[j] = omega * 0.25 * (gim[j] + gip[j] + gi[j - 1] + gi[j + 1]) + (1 - omega) * gi[j]
    return g[n // 2][n // 2]


def lu(n, rng):
    a = [[rng.random() for _ in range(n)] for _ in range(n)]
    pivot = [0] * n
    for j in range(n):
        jp = max(range(j, n), key=lambda i: abs(a[i][j]))
        pivot[j] = jp
        if jp != j:
            a[j], a[jp] = a[jp], a[j]
        if j < n - 1:
            recp = 1.0 / a[j][j]
            for k in range(j + 1, n):
                a[k][j] *= recp
        for ii in range(j + 1, n):
            aii, aj = a[ii], a[j]
            aiij = aii[j]
            for jj in range(j + 1, n):
                aii[jj] -= aiij * aj[jj]
    return a[n - 1][n - 1]


def sparse_matmult(n, nz, cycles, rng):
    rows = [[(rng.randrange(n), rng.random()) for _ in range(nz)] for _ in range(n)]
    x = [rng.random() for _ in range(n)]
    y = [0.0] * n
    for _ in range(cycles):
        for r, row in enumerate(rows):
            s = 0.0
            for col, v in row:
                s += x[col] * v
            y[r] = s
    return sum(y)


def monte_carlo(samples, rng):
    under = 0
    for _ in range(samples):
        x = rng.random()
        y = rng.random()
        if x * x + y * y <= 1.0:
            under += 1
    return under / samples * 4


def bench(n):
    rng = random.Random(7)
    r = sor(n, 10)
    r += lu(n + 20, rng)
    r += sparse_matmult(n * 25, 5, 10, rng)
    r += monte_carlo(n * 2000, rng)
    return round(r, 6)
