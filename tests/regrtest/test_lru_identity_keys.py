"""`functools.lru_cache` keys of objects that hash by identity.

`typing`'s `_tp_cache` calls its cache as `f(*args, **kwds)` with keys
such as `(alias, (str, Any))`. Compiled code probes such keys natively:
objects that hash and compare by identity, classes, and tuples of them.
An object whose class defines `__eq__` or `__hash__` must still go through
Python, and a miss must still run the function. Run it past the compile
threshold.
"""

import functools
import typing


class Plain:
    pass


class Eq:
    def __init__(self, v):
        self.v = v

    def __eq__(self, other):
        return isinstance(other, Eq) and self.v == other.v

    def __hash__(self):
        return hash(self.v)


calls = []


@functools.lru_cache(maxsize=None)
def cached(*args):
    calls.append(args)
    return len(calls)


def forward(*args, **kwds):
    return cached(*args, **kwds)


a, b = Plain(), Plain()
for _ in range(3000):
    first = forward(a, (str, int))
    assert forward(a, (str, int)) == first
    assert forward(b, (str, int)) != first
    assert forward(a, (int, str)) != first
    assert forward(Eq(1)) == forward(Eq(1))
    assert typing.List[int] is typing.List[int]
    assert typing.Dict[str, typing.Any] == typing.Dict[str, typing.Any]
    assert typing.Optional[str] == typing.Union[str, None]

assert len(calls) == 4, calls
