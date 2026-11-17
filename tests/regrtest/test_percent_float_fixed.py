"""`%f` and `%F` formatting of floats, as CPython rounds them.

Each case's exact decimal expansion rounds half to even at the
precision; zero precision, negative zero, subnormals, large magnitudes,
precisions past 17 and the sign, space, width and zero flags. Run past the
compile threshold.
"""

CASES = [
    (0, 2.5, '2'),
    (0, 3.5, '4'),
    (1, 0.05, '0.1'),
    (2, 2.675, '2.67'),
    (2, 1.005, '1.00'),
    (3, 3.14159, '3.142'),
    (6, 0.3333333333333333, '0.333333'),
    (0, -0.0, '-0'),
    (2, -0.001, '-0.00'),
    (17, 0.1, '0.10000000000000001'),
    (20, 0.1, '0.10000000000000000555'),
    (3, 1e+16, '10000000000000000.000'),
    (2, 123456789.98765433, '123456789.99'),
    (10, 5e-324, '0.0000000000'),
    (4, 9.99995, '10.0000'),
]
FLAGS = [
    ('%+.2f', 2.5, '+2.50'),
    ('% .1f', 0.25, ' 0.2'),
    ('%-10.3f|', 1.5, '1.500     |'),
    ('%010.4f', -3.14159, '-0003.1416'),
    ('%F', 1e+20, '100000000000000000000.000000'),
    ('%f', 0.1, '0.100000'),
]


def run(n):
    for _ in range(n):
        for p, v, want in CASES:
            assert "%.*f" % (p, v) == want, (p, v, "%.*f" % (p, v), want)
        for f, v, want in FLAGS:
            assert f % v == want, (f, v, f % v, want)


run(300)
print("ok")
