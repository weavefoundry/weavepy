"""Ecosystem probe: scipy (PyPI wheel) — linalg, sparse, optimize,
integrate, stats, fft, and a Cython typed-memoryview path (ndimage)."""

import numpy as np

# linalg: solve + LU factor round-trip against the numpy reference
from scipy import linalg

rng = np.random.default_rng(42)
a = rng.standard_normal((5, 5)) + 5.0 * np.eye(5)
b = rng.standard_normal(5)
x = linalg.solve(a, b)
assert np.allclose(a @ x, b), "linalg.solve residual"
assert np.allclose(x, np.linalg.solve(a, b)), "solve disagrees with numpy"

lu, piv = linalg.lu_factor(a)
x2 = linalg.lu_solve((lu, piv), b)
assert np.allclose(x2, x), "lu_factor/lu_solve round-trip"

# sparse: CSR matvec + format conversions
from scipy import sparse

dense = np.array([[1.0, 0.0, 2.0], [0.0, 0.0, 3.0], [4.0, 5.0, 0.0]])
csr = sparse.csr_matrix(dense)
assert csr.nnz == 5
v = np.array([1.0, 2.0, 3.0])
assert np.allclose(csr @ v, dense @ v), "csr matvec"
assert np.allclose(csr.tocsc().toarray(), dense), "csr -> csc"
assert np.allclose(csr.tocoo().toarray(), dense), "csr -> coo"
assert np.allclose(csr.T.toarray(), dense.T), "csr transpose"

# optimize: BFGS converges on the Rosenbrock function
from scipy import optimize

res = optimize.minimize(optimize.rosen, np.array([-1.2, 1.0]), method="BFGS")
assert res.success, res.message
assert np.allclose(res.x, [1.0, 1.0], atol=1e-4), res.x

# integrate: quad value + tolerance
from scipy import integrate

val, err = integrate.quad(np.exp, 0.0, 1.0)
assert abs(val - (np.e - 1.0)) < 1e-10, val
assert err < 1e-8, err

# stats: norm pdf/cdf/rvs shapes
from scipy import stats

assert abs(stats.norm.pdf(0.0) - 1.0 / np.sqrt(2.0 * np.pi)) < 1e-12
assert abs(stats.norm.cdf(0.0) - 0.5) < 1e-12
draws = stats.norm.rvs(size=(3, 4), random_state=7)
assert draws.shape == (3, 4), draws.shape

# fft: forward/inverse round-trip
from scipy import fft

sig = rng.standard_normal(64)
assert np.allclose(fft.ifft(fft.fft(sig)).real, sig), "fft round-trip"

# ndimage: Cython typed-memoryview path over a strided (non-contiguous) array
from scipy import ndimage

grid = np.arange(36, dtype=np.float64).reshape(6, 6)
strided = grid[::2, ::2]  # non-contiguous view
smoothed = ndimage.uniform_filter(strided, size=3, mode="nearest")
assert smoothed.shape == strided.shape
assert np.isfinite(smoothed).all()
# The center element of a 3x3 uniform filter is the mean of the window.
window = strided[0:3, 0:3]
assert abs(smoothed[1, 1] - window.mean()) < 1e-12, smoothed[1, 1]

import scipy

print("scipy ok", scipy.__version__)
