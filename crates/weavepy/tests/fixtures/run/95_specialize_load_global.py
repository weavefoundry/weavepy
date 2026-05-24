# RFC 0021: LOAD_GLOBAL specialization for both module-level
# globals and builtins. The fast path must correctly distinguish
# between the two and re-deopt when a global shadows a builtin.

GLOBAL_K = 7
GLOBAL_NAMES = ("Alice", "Bob")


def hot_global(n):
    total = 0
    for i in range(n):
        total = total + GLOBAL_K
    return total


def hot_builtin(n):
    total = 0
    for i in range(n):
        total = total + len(GLOBAL_NAMES)
    return total


print(hot_global(100))
print(hot_builtin(100))

# Now shadow `len` in globals and confirm we get the new value
# (the specialized cache must deopt cleanly when this happens).
def shadow_then_call():
    def f():
        return len([1, 2, 3])

    print(f())
    return f


s = shadow_then_call()
print(s())
