"""Nested, failed, and sustained imports retain ordinary Python behavior."""
import os
import sys
import tempfile


def write_module(directory, name, source):
    with open(os.path.join(directory, name + ".py"), "w") as stream:
        stream.write(source)


with tempfile.TemporaryDirectory() as directory:
    sys.path.insert(0, directory)
    try:
        write_module(directory, "budget_child", """
import budget_parent
assert budget_parent.started

def leaf(value):
    return value + 1

values = [leaf(i) for i in range(40)]
""")
        write_module(directory, "budget_parent", """
started = True
import budget_child
assert budget_child.values == list(range(1, 41))
leaf = budget_child.leaf
""")
        import budget_parent
        assert sum(budget_parent.leaf(i) for i in range(2000)) == 2001000
        write_module(directory, "budget_failure", """
import budget_parent
assert budget_parent.leaf(41) == 42
raise RuntimeError('expected import failure')
""")
        for _ in range(2):
            try:
                import budget_failure
            except RuntimeError as error:
                assert str(error) == "expected import failure"
            else:
                raise AssertionError("import must fail")
            assert "budget_failure" not in sys.modules
        write_module(directory, "budget_sustained", """
def total(n):
    result = 0
    for i in range(n):
        result += i
    return result

value = total(100000)
assert value == 4999950000
""")
        import budget_sustained
        assert budget_sustained.total(100) == 4950
        assert budget_parent.leaf(41) == 42
    finally:
        sys.path.remove(directory)
        for name in ("budget_parent", "budget_child", "budget_failure", "budget_sustained"):
            sys.modules.pop(name, None)

print("import compilation scope: ok")
