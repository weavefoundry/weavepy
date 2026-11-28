"""`round()` of a bool and the arity of int's and float's no-argument methods.

`round(True)` is `int.__round__(True)`, a plain `int`, with or without a
non-negative `ndigits`. `float.hex`, `int.bit_length` and the other
methods that take no arguments reject an extra one with a `TypeError`.
"""

for args in [(True,), (False,), (True, 2), (True, None), (False, -1)]:
    r = round(*args)
    assert type(r) is int and r == int(args[0]), (args, r)

calls = [
    (1.5).hex, (1.5).is_integer, (1.5).as_integer_ratio, (1.5).conjugate,
    (3).bit_length, (3).bit_count, (3).is_integer, (3).as_integer_ratio,
    (3).conjugate, True.bit_length,
]
for call in calls:
    call()
    try:
        call(1)
    except TypeError as e:
        assert "takes no arguments (1 given)" in str(e), e
    else:
        raise AssertionError(f"{call} accepted an argument")
print("ok")
