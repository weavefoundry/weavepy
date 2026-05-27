"""Drop-in test — numpy 2.x facade.

Exercises the surface that real numpy 2.x code reaches for:
construction, arithmetic, reductions, broadcasting, dtype, the
linear-algebra primitives, and the ``linalg`` / ``random`` /
``fft`` submodules. Backed by the pure-Python fallback when
``_numpylike`` isn't compiled in (CI matrix runs both).
"""

import numpy as np


def assert_eq(a, b, label=''):
    if a != b:
        raise AssertionError('{}: {!r} != {!r}'.format(label or 'eq', a, b))


def assert_close(a, b, rel=1e-6, abs_=1e-9, label=''):
    if abs(float(a) - float(b)) > abs_ + rel * abs(float(b)):
        raise AssertionError('{}: {!r} vs {!r}'.format(label or 'close', a, b))


def assert_true(cond, label=''):
    if not cond:
        raise AssertionError('{}: expected True'.format(label))


def test_constructors():
    a = np.zeros((2, 3))
    assert_eq(a.shape, (2, 3), 'zeros shape')
    b = np.ones((3,))
    assert_eq(b.shape, (3,), 'ones shape')
    c = np.arange(10)
    assert_eq(c.shape, (10,), 'arange shape')
    assert_eq(c.tolist(), list(range(10)), 'arange data')


def test_arithmetic():
    a = np.array([1.0, 2.0, 3.0])
    b = np.array([4.0, 5.0, 6.0])
    assert_eq((a + b).tolist(), [5.0, 7.0, 9.0])
    assert_eq((a * 2).tolist(), [2.0, 4.0, 6.0])
    assert_eq((b - a).tolist(), [3.0, 3.0, 3.0])
    assert_close((a / b).tolist()[0], 0.25, label='div')


def test_reductions():
    a = np.arange(10)
    assert_eq(a.sum(), 45.0 if isinstance(a.sum(), float) else 45)
    assert_close(a.mean(), 4.5)
    assert_eq(a.min(), 0)
    assert_eq(a.max(), 9)


def test_matmul():
    m1 = np.array([[1.0, 2.0], [3.0, 4.0]])
    m2 = np.array([[5.0, 6.0], [7.0, 8.0]])
    out = m1 @ m2
    assert_eq(out.tolist(), [[19.0, 22.0], [43.0, 50.0]])


def test_reshape():
    a = np.arange(12).reshape(3, 4)
    assert_eq(a.shape, (3, 4))
    assert_eq(a.reshape(2, 6).shape, (2, 6))
    flat = a.ravel()
    assert_eq(flat.shape, (12,))


def test_dtype():
    assert_eq(str(np.dtype('f8')), 'float64')
    assert_eq(np.dtype(int).name, 'int64')
    assert_eq(np.float32.itemsize, 4)
    assert_eq(np.complex128.kind, 'c')


def test_linalg():
    m = np.array([[1.0, 2.0], [3.0, 4.0]])
    det = np.linalg.det(m)
    assert_close(det, -2.0, label='det 2x2')
    inv = np.linalg.inv(m)
    # Sanity: m @ inv should be ~ identity.
    p = m @ inv
    assert_close(p.tolist()[0][0], 1.0, label='inv[0,0]')


def test_random():
    np.random.seed(42)
    v = np.random.rand()
    assert_true(0.0 <= v <= 1.0)
    arr = np.random.rand(5)
    assert_eq(arr.shape, (5,))


def test_fft_smoke():
    # Just exercise the API so it doesn't crash; correctness of the
    # naive DFT in the facade is verified against numpy's at:
    # tests/dropin/numpy_fft_parity.py (not bundled here).
    out = np.fft.fft([1.0, 0.0, 0.0, 0.0])
    assert_eq(len(out), 4, 'fft length')


def test_aliases_and_constants():
    assert_close(np.pi, 3.141592653589793)
    assert_close(np.e, 2.718281828459045)
    assert_true(np.array_equal([1, 2, 3], np.array([1, 2, 3]).tolist()))


def main():
    tests = [v for k, v in globals().items() if k.startswith('test_')]
    failures = 0
    for fn in tests:
        try:
            fn()
            print('OK   {}'.format(fn.__name__))
        except Exception as exc:
            failures += 1
            print('FAIL {}: {}'.format(fn.__name__, exc))
    if failures:
        raise SystemExit(1)
    print('{} numpy drop-in tests passed (backend={})'.format(
        len(tests), np._CORE_KIND))


if __name__ == '__main__':
    main()
