"""Build a Python AST, compile it at three optimization levels, and execute it.

Source construction, AST construction, compilation, execution, and result checks
are timed. The AST, compiled code, and final namespace remain live afterward.
"""
import ast
import os
import time


def bench(n):
    source = "\n".join("def function_%d():\n    return %d + 1\n" % (i, i) for i in range(n))
    tree = ast.parse(source, filename="<ast-compilation>")
    codes = []
    namespace = {}
    for optimize in range(3):
        code = compile(tree, "<ast-compilation>", "exec", optimize=optimize)
        codes.append(code)
        namespace = {}
        exec(code, namespace)
        assert len(namespace) == n + 1
        if n:
            assert namespace["function_0"]() == 1
            assert namespace["function_%d" % (n - 1)]() == n
    return tree, codes, namespace


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "500"))
    start = time.perf_counter_ns()
    retained = bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
