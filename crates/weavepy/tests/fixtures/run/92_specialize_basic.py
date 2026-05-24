# RFC 0021: tight monomorphic loop should produce identical output
# to a generic loop. Tests BINARY_OP_ADD_INT, COMPARE_OP_INT, and
# FOR_ITER_RANGE specialization paths together.

def hot_loop_int(n):
    total = 0
    for i in range(n):
        total = total + i
    return total


def hot_loop_float(n):
    total = 0.0
    for i in range(n):
        total = total + float(i)
    return total


def hot_loop_str(n):
    out = ""
    parts = ["a", "b", "c"]
    for i in range(n):
        out = out + parts[i % 3]
    return out


# Run each loop ~1000 times so the cache fully warms.
print(hot_loop_int(1000))
print(hot_loop_float(100))
print(hot_loop_str(15))

# Repeat with different sizes to confirm the cache survives.
print(hot_loop_int(100))
print(hot_loop_int(50))
