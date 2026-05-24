# RFC 0021: a polymorphic call site should still produce correct
# output. We deliberately mix int / float / str at the same
# instruction so the specialization layer must repeatedly deopt and
# re-warm — exercising the Cooldown -> Empty -> specialized cycle
# without observable behaviour change.

def add(a, b):
    return a + b


def cmp(a, b):
    return a < b


pairs = [
    (1, 2),
    (1.0, 2.0),
    ("hello, ", "world"),
    (3, 4),
    (3.5, 4.5),
    ("a", "b"),
    (10, 20),
    (1.5, 1.0),
    ("x", "x"),
]
for a, b in pairs:
    print(add(a, b))
    print(cmp(a, b))

# After polymorphic warmup, a long monomorphic run should still
# behave correctly — even if the cache is in Cooldown, the generic
# path is the source of truth.
total = 0
for i in range(200):
    total = add(total, i)
print(total)
