"""Parse assignment-heavy input; include source creation and result checks."""

import ast
import os
import time


mode = os.environ.get("WEAVEPY_AST_PARSE_MODE", "exec")
if mode not in ("exec", "eval", "single", "func_type"):
    raise ValueError("unsupported parse mode")


def bench(n):
    if mode == "exec":
        source = "a, (b, *c) = data\nobj.field = data[index]\ndel a, obj.field\n" * n
    elif mode == "eval":
        source = "[" + "(value := obj.field[index])," * n + "]"
    elif mode == "single":
        source = "a, b = pair;" * n + "\n"
    else:
        source = "(" + ", ".join(["list[int]"] * n) + ") -> tuple"
    tree = ast.parse(source, mode=mode)
    if mode == "exec":
        assert len(tree.body) == 3 * n
        assert isinstance(tree.body[-3].targets[0].elts[1].elts[1].value.ctx, ast.Store)
        assert isinstance(tree.body[-2].targets[0].value.ctx, ast.Load)
        assert isinstance(tree.body[-1].targets[1].ctx, ast.Del)
    elif mode == "eval":
        assert len(tree.body.elts) == n
        assert isinstance(tree.body.elts[-1].target.ctx, ast.Store)
        assert isinstance(tree.body.elts[-1].value.ctx, ast.Load)
    elif mode == "single":
        assert len(tree.body) == n
        assert isinstance(tree.body[-1].targets[0].elts[-1].ctx, ast.Store)
    else:
        assert len(tree.argtypes) == n
        assert isinstance(tree.argtypes[-1].ctx, ast.Load)
        assert tree.returns.id == "tuple"
    return n


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "500"))
    start = time.perf_counter_ns()
    bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
