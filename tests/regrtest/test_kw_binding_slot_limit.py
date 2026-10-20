"""Keyword arguments bind to every parameter slot of wide signatures.

The keyword-call permutation packs each keyword's parameter slot into a
4-bit nibble whose value 15 means "into `**kwargs`"; a callee with 16
parameter slots once lost a keyword aimed at slot 15
(`dataclasses.make_dataclass(..., decorator=...)` raised
UnboundLocalError). Exercised at module level, in a loop, and in a hot
function, at signature widths around the limit.
"""

import dataclasses


def make(npos, nkw, varkw=False):
    pos = [f"a{i}" for i in range(npos)]
    kws = [f"k{i}=None" for i in range(nkw)]
    params = pos + (["*"] + kws if kws else [])
    if varkw:
        params.append("**rest")
    names = [f"a{i}" for i in range(npos)] + [f"k{i}" for i in range(nkw)]
    items = [f"{n!r}: {n}" for n in names] + (["'rest': rest"] if varkw else [])
    ns = {}
    exec(f"def f({', '.join(params)}):\n    return {{{', '.join(items)}}}\n", ns)
    return ns["f"]


def call_with(f, npos, name, value):
    """`f(0, 1, ..., name=value)` from a literal keyword call site."""
    ns = {"f": f}
    pos = "".join(f"{i}, " for i in range(npos))
    exec(f"def run():\n    return f({pos}{name}={value!r})\n", ns)
    return ns["run"]


for npos, nkw in [(0, 15), (0, 16), (0, 17), (1, 15), (2, 13), (2, 14), (2, 15), (3, 14)]:
    for varkw in (False, True):
        f = make(npos, nkw, varkw)
        for j in (0, nkw // 2, nkw - 1):
            name = f"k{j}"
            run = call_with(f, npos, name, "X")
            for _ in range(300):
                got = run()
                assert got[name] == "X", (npos, nkw, varkw, name, got)
                for i in range(nkw):
                    if i != j:
                        assert got[f"k{i}"] is None, (npos, nkw, varkw, name, got)
        if varkw:
            got = call_with(f, npos, "extra", 1)()
            assert got["rest"] == {"extra": 1}, got


def deco(cls, **kw):
    return dataclasses.dataclass(cls, **kw)


for _ in range(300):
    C = dataclasses.make_dataclass("C", [("x", int)], decorator=deco)
    assert repr(C(1)) == "C(x=1)"

print("kw binding slot limit: ok")
