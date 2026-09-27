"""AST spec traversal preserves live mutations, callbacks, and fallbacks."""
import _ast
import ast
import marshal
import sys


native = getattr(_ast, "_build", None)


def compare(run):
    actual = run()
    if native is not None:
        _ast._build = lambda spec, builder, state: builder(spec)
        try:
            expected = run()
        finally:
            _ast._build = native
        assert actual == expected, (actual, expected)
    return actual


sources = [
    "value = 1 + 2\n",
    "def outer(x: int = 4) -> int:\n    def inner():\n        return x\n    return inner\n",
    "from __future__ import annotations\ndef f(x: Missing):\n    return x\n",
    "values = [x * 2 for x in range(5) if x]\n",
    "try:\n    1 / 0\nexcept ZeroDivisionError as error:\n    pass\nfinally:\n    pass\n",
    "class C:\n    @property\n    def value(self):\n        return 3\n",
    "async def f(xs):\n    async for x in xs:\n        await x\n",
    "match value:\n    case {'x': x, **rest}:\n        result = x\n",
    "type Pair[T] = tuple[T, T]\n",
    "text = f'{value!r:>8}'\n",
    "x = ...\ny = {1, 2}\nz = b'bytes'\n",
    "x = 1  # type: int\n# type: ignore\n",
]


def compilation(source, mode, optimize):
    tree = ast.parse(source, mode=mode, type_comments=True)
    return ast.dump(tree, include_attributes=True), marshal.dumps(
        compile(tree, "native_ast_build.py", mode, optimize=optimize))


for source in sources:
    for optimize in range(3):
        compare(lambda: compilation(source, "exec", optimize))
for mode in ("eval", "single"):
    compare(lambda: compilation("1 + 2", mode, 0))
compare(lambda: ast.dump(ast.parse("(int, str) -> bool", mode="func_type")))


def private_checks():
    original = ast._build
    original_types = ast._NODE_TYPES

    def normalize(value):
        if isinstance(value, ast.AST):
            return ast.dump(value, include_attributes=True)
        if isinstance(value, list):
            return [normalize(item) for item in value]
        return value

    def outcome(spec):
        try:
            return normalize(ast._from_spec(spec))
        except Exception as error:
            return type(error).__name__, str(error)

    for spec in (None, 3, "text", b"bytes", (1, 2), frozenset({1}),
                 {"_type": "Constant", "value": 4}, {}, {"_type": "Missing"},
                 {"_type": "Constant", 42: 3}):
        compare(lambda: outcome(spec))
    compare(lambda: outcome(list(range(1100))))  # Evaluator checkpoint budget.
    shared = {"_type": "Constant", "value": 3}
    compare(lambda: outcome([shared, shared]))
    nodes = ast._from_spec([shared, shared])
    assert nodes[0] is not nodes[1]

    class ListSubclass(list):
        def __iter__(self):
            yield {"_type": "Constant", "value": 19}

    class DictSubclass(dict):
        def items(self):
            yield "value", 23

    class PretendList:
        @property
        def __class__(self):
            return list

        def __iter__(self):
            yield 29

    for spec in (ListSubclass(), DictSubclass(_type="Constant"), PretendList()):
        compare(lambda: outcome(spec))

    def string_subclass_key():
        events = []
        marker = "_type"
        class Key(str):
            __hash__ = str.__hash__
            def __eq__(self, other):
                events.append((str(other), other is marker))
                return str.__eq__(self, other)
        spec = {"_type": "Constant", Key("value"): 37}
        events.clear()
        return outcome(spec), events
    assert compare(string_subclass_key)[1], "key comparison callback was skipped"

    def callbacks(mutation):
        events = []
        child = {"_type": "Constant", "value": 7}
        children = [child]
        if mutation == "registry":
            children.append({"_type": "Constant", "value": 13})
        spec = {"_type": "Module", "body": children, "type_ignores": []}
        saved_new = ast.Constant.__dict__.get("__new__")
        saved_set = ast.Constant.__dict__.get("__setattr__")

        def new(cls):
            events.append(("new", cls.__name__))
            if mutation == "append" and len(children) == 1:
                children.append({"_type": "Constant", "value": 11})
            if mutation == "dict-size":
                spec["extra"] = 1
            if mutation == "registry":
                ast._NODE_TYPES = dict(original_types, Constant=ast.Pass)
            if mutation == "builder":
                ast._build = lambda value: "changed"
            if mutation == "setter":
                ast.setattr = lambda node, key, value: events.append(("replacement", key))
            if mutation == "error":
                raise ValueError("constructor failed")
            return object.__new__(cls)

        def setter(node, key, value):
            events.append(("set", key, value))
            object.__setattr__(node, key, value)

        ast.Constant.__new__ = staticmethod(new)
        ast.Constant.__setattr__ = setter
        try:
            return outcome(spec), events
        finally:
            ast._build = original
            ast._NODE_TYPES = original_types
            if "setattr" in ast.__dict__:
                del ast.setattr
            for name, saved in (("__new__", saved_new), ("__setattr__", saved_set)):
                if saved is None:
                    delattr(ast.Constant, name)
                else:
                    setattr(ast.Constant, name, saved)

    for mutation in ("ordinary", "append", "dict-size", "registry", "builder", "setter", "error"):
        compare(lambda: callbacks(mutation))

    def descriptors():
        events = []

        class Meta(type):
            def __getattribute__(cls, name):
                if name == "__new__":
                    events.append("metaclass")
                return super().__getattribute__(name)

        class New:
            def __get__(self, instance, owner):
                events.append("new descriptor")
                return lambda cls: object.__new__(cls)

        class Node(ast.AST, metaclass=Meta):
            _fields = ("value",)
            __new__ = New()

            @property
            def value(self):
                return self.saved

            @value.setter
            def value(self, value):
                events.append(("property", value))
                self.saved = value

        class Registry:
            def __getitem__(self, name):
                events.append(("registry", name))
                return Node

        ast._NODE_TYPES = Registry()
        try:
            tree = ast._from_spec({"_type": "Node", "value": 31})
            return tree.value, events
        finally:
            ast._NODE_TYPES = original_types
    assert compare(descriptors)[0] == 31

    def changed_builtin(captured=False):
        calls = []
        saved_state = ast._BUILD_STATE
        def check(value, cls):
            calls.append(cls.__name__)
            return isinstance(value, cls)
        ast.isinstance = check
        if captured:
            ast._BUILD_STATE = (*saved_state[:3], check)
        try:
            return outcome(shared), calls
        finally:
            del ast.isinstance
            ast._BUILD_STATE = saved_state
    compare(changed_builtin)
    compare(lambda: changed_builtin(captured=True))

    def replacement_code(value):
        return "replacement code"
    code = original.__code__
    original.__code__ = replacement_code.__code__
    try:
        assert ast._from_spec(shared) == "replacement code"
    finally:
        original.__code__ = code

    def observed():
        calls = []
        def trace(frame, event, arg):
            if event == "call" and frame.f_code.co_name == "_build":
                calls.append(frame.f_code.co_name)
            return trace
        sys.settrace(trace)
        try:
            result = outcome(shared)
        finally:
            sys.settrace(None)
        return result, calls
    assert compare(observed)[1]

    cyclic = []
    cyclic.append(cyclic)
    # The recursion limit must raise cleanly, including from native traversal.
    assert outcome(cyclic)[0] == "RecursionError"
    deep = 1
    for _ in range(100):
        deep = [deep]
    compare(lambda: outcome(deep))


if native is not None:
    private_checks()
print("Native AST build checks passed")
