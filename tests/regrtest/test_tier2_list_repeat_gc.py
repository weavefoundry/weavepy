"""Lists built by repetition, slicing and starred unpacking, and the collector.

`[x] * n`, `xs[a:b]` and the starred target of `a, *b = xs` are new
lists whose tracking may be deferred; a cycle that runs through one is
still collected (a slice's and a starred target's lists were never
registered at all), and `gc.is_tracked` answers as CPython's does. Run it
past the compile threshold.
"""

import gc
import weakref


class Node:
    pass


def build(n, xs):
    t = 0
    for i in range(n):
        rep = [0] * 8
        part = xs[1:4]
        t += len(rep) + len(part)
    return t


def cycles(n):
    dead = []
    for i in range(n):
        node = Node()
        node.rep = [node] * 2
        node.part = [node, node, 1][0:2]
        first, *node.rest = (1, node, node)
        weakref.finalize(node, dead.append, i)
    return dead


assert build(3000, list(range(10))) == 11 * 3000
dead = cycles(500)
gc.collect()
assert len(dead) == 500, len(dead)
assert gc.is_tracked([0] * 3) and gc.is_tracked(list(range(5))[1:3])
print("ok")
