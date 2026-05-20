"""Helper module: surface every public name via `from _shapes import *`."""

PI_APPROX = 3.14159
_INTERNAL = "should not be exported by import *"


def circle_area(r):
    return PI_APPROX * r * r


def square_area(s):
    return s * s


def _private_helper(x):
    return x
