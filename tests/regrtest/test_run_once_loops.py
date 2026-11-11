"""A function called once whose main loop tier 2 doesn't take (it makes
generators, its other loops never run) runs the same once compiled code
takes it over: values, a generator's finalizer before the next statement,
an exception caught in the loop, and a branch first taken late."""

import gc


class Mortal:
    log = []

    def __init__(self, v):
        self.v = v

    def __del__(self):
        Mortal.log.append(self.v)


def gen(n, keep=None):
    for i in range(n):
        yield i


def run(n, case):
    t = 0
    seen = 0
    for k in range(n):
        if case == "create":
            g = gen(5)
            t += 1
        elif case == "drain":
            for x in gen(3):
                t += x
        elif case == "other":
            for x in [k, k]:
                t += x
        if k == n - 7:
            try:
                {}["missing"]
            except KeyError:
                seen += 1
        if k > n - 3:
            m = Mortal(k)
            del m
            seen += len(Mortal.log)
            Mortal.log.clear()
    return t, seen


gc.collect()
assert run(30000, "create") == (30000, 3)
assert run(30000, "drain") == (90000, 3)


def late(n):
    out = []
    for k in range(n):
        g = gen(2)
        if k == n - 1:
            out.extend(g)
            out.append(sum(x for x in gen(4)))
    return out


assert late(30000) == [0, 1, 6]
print("ok")
