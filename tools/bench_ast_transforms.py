"""Parse, rewrite, compile, and execute functions containing varied AST nodes.

All source construction and tree work is timed. The rewrite renames a parameter
and its uses; checked outputs ensure the transformed program still executes.
"""
import ast
import os
import time


class RenameScale(ast.NodeTransformer):
    def visit_Name(self, node):
        if node.id == "scale":
            node.id = "factor"
        return node

    def visit_arg(self, node):
        if node.arg == "scale":
            node.arg = "factor"
        return node


def bench(n):
    body = """def function_%d(values, scale=2):
    total = 0
    for value in values:
        if value > 0:
            total += value * scale
    try:
        squares = [value * value for value in values]
        mapping = {value: str(value) for value in values}
        return total, squares, mapping
    except TypeError:
        return None
"""
    source = "\n".join(body % i for i in range(n))
    tree = RenameScale().visit(ast.parse(source, filename="<ast-transforms>"))
    code = compile(tree, "<ast-transforms>", "exec")
    namespace = {}
    exec(code, namespace)
    assert len(namespace) == n + 1
    for index in sorted({0, n - 1}) if n else []:
        assert namespace["function_%d" % index]([1, -2, 3], factor=2) == (
            8, [1, 4, 9], {1: "1", -2: "-2", 3: "3"})
    return tree, code, namespace


if __name__ == "__main__":
    n = int(os.environ.get("WEAVEPY_BENCH_WORK", "100"))
    start = time.perf_counter_ns()
    retained = bench(n)
    print("WEAVEPY_BENCH_NS=%d" % (time.perf_counter_ns() - start))
