"""A compiled method site over native streams.

`out.write(x)` at one site answers from the method it loaded for the
stream's kind; a site that sees text and binary streams in turn calls each
one's own method, and a stream whose own attribute shadows the method
calls that. Run it past the compile threshold.
"""

import io


def emit(out, x, n):
    for _ in range(n):
        out.write(x)
    return out


N = 3000
assert emit(io.StringIO(), "ab", N).getvalue() == "ab" * N
assert emit(io.BytesIO(), b"cd", N).getvalue() == b"cd" * N
mixed = [io.StringIO(), io.BytesIO()]
for i in range(N):
    emit(mixed[i % 2], "e" if i % 2 == 0 else b"f", 1)
assert mixed[0].getvalue() == "e" * (N // 2) and mixed[1].getvalue() == b"f" * (N // 2)

seen = []
s = io.StringIO()
s.write = seen.append
emit(s, "g", 5)
assert seen == ["g"] * 5 and s.getvalue() == ""
try:
    emit(io.StringIO(), b"x", 1)
except TypeError:
    pass
else:
    raise AssertionError("bytes into a text stream")
closed = io.StringIO()
closed.close()
try:
    emit(closed, "x", 1)
except ValueError:
    pass
else:
    raise AssertionError("write to a closed stream")
print("ok")
