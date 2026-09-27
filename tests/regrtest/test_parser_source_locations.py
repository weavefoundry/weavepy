"""Source locations survive indexed parser lookups and nested completion."""
import ast

cases = [
    ("x = 1\ny = 2\nbroken =\n", (0, 0), 3),
    ("def outer():\n    if True:\n        class Inner:\n            pass\nbroken =\n", (3, 8), 5),
    ("@decorator\ndef outer():\n    if True:\n        pass\nbroken =\n", (3, 4), 5),
]
for source, expected, line in cases:
    try:
        compile(source, "<parser-locations>", "exec")
    except SyntaxError as error:
        assert error.lineno == line, (error.lineno, line)
        assert error._metadata[:2] == expected, (error._metadata, expected)
    else:
        raise AssertionError("invalid source compiled")

source = "label = 'é'\n\nif True:\n"
try:
    compile(source, "<missing-block>", "exec")
except IndentationError as error:
    assert "'if' statement on line 3" in error.msg, error.msg
else:
    raise AssertionError("missing block compiled")

source = "# é\ndef outer():\n    def café():\n        return 17\n    return café()\n"
module = ast.parse(source)
outer = module.body[0]
inner = outer.body[0]
assert (outer.lineno, outer.col_offset, inner.lineno, inner.col_offset) == (2, 0, 3, 4)
namespace = {}
exec(compile(source, "<unicode-functions>", "exec"), namespace)
assert namespace["outer"]() == 17
assert namespace["outer"].__code__.co_firstlineno == 2
print("parser source locations: ok")
