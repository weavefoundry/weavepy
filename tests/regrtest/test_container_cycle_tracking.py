"""Cycles through lists and dicts made by builtins are collected.

Each maker's container is given an object that refers back to it; the
collector must find every such cycle. `list.copy()`, `dir()`,
`json.loads`, `re.findall`, `dict.fromkeys`, `dict | dict`, a list's `+`
and `*`, and a function's `**kwargs` built their containers without
registering them, so these cycles were never collected.
"""

import copy
import gc
import json
import re
import weakref


class Node:
    pass


def kwargs(**k):
    return k


LISTS = {
    "split": lambda: "a,b".split(","),
    "copy": lambda: [1, 2].copy(),
    "sorted": lambda: sorted([2, 1]),
    "slice": lambda: [1, 2, 3][0:2],
    "slice_step": lambda: [1, 2, 3][::2],
    "concat": lambda: [1] + [2],
    "repeat": lambda: [1] * 2,
    "dir": lambda: dir(),
    "json": lambda: json.loads("[1, 2]"),
    "findall": lambda: re.findall("a", "aa"),
    "deepcopy": lambda: copy.deepcopy([1, 2]),
    "enumerate": lambda: list(enumerate((1, 2))),
}
DICTS = {
    "literal": lambda: {"a": 1},
    "copy": lambda: {"a": 1}.copy(),
    "fromkeys": lambda: dict.fromkeys("ab"),
    "union": lambda: {"a": 1} | {"b": 2},
    "json": lambda: json.loads('{"a": 1}'),
    "kwargs": lambda: kwargs(a=1),
    "kwargs_empty": lambda: kwargs(),
}


def check(makers, put):
    bad = []
    for name, make in makers.items():
        dead = []
        for i in range(60):
            c = make()
            node = Node()
            put(c, node)
            node.ref = c
            weakref.finalize(node, dead.append, i)
        del c, node
        gc.collect()
        if len(dead) != 60:
            bad.append((name, len(dead)))
    return bad


assert check(LISTS, lambda c, n: c.append(n)) == []
assert check(DICTS, lambda c, n: c.__setitem__("n", n)) == []
print("ok")
