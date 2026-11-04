"""`ast.parse` builds nodes natively only while nothing could tell."""
import _ast
import ast
import sys

if not hasattr(_ast, "_build"):
    print("Native AST direct checks skipped")
    sys.exit(0)


def shape(value):
    """Everything about a tree a program could see, in order."""
    if isinstance(value, ast.AST):
        return (type(value), [(key, shape(item)) for key, item in vars(value).items()])
    if isinstance(value, list):
        return [shape(item) for item in value]
    return (type(value), value)


def through_python(run):
    native = _ast._build
    _ast._build = lambda spec, builder, state: builder(spec)
    try:
        return run()
    finally:
        _ast._build = native


sources = [
    "x = 1\n",
    "a, (b, *c) = d\ndel e[0], f.g\nfor h in i: pass\nwith j as (k, l): pass\n",
    "def f(a, /, b=2, *c, d, **e) -> int:\n    return a\n",
    "async def f():\n    async with a as b:\n        await c\n",
    "class C(B, metaclass=M):\n    x: int = 3\n",
    "y = [i for i in range(3) if i] + {k: v for k, v in z}\n",
    "s = f'{a!r:>{w}} and {b=}' 'tail' u'u'\n",
    "match p:\n    case [1, *r] | {'k': v} if v:\n        pass\n    case C(x=1):\n        pass\n",
    "try:\n    pass\nexcept* E as e:\n    pass\n",
    "type T[U: int, *V, **W] = list[U]\n",
    "lambda x, *, y=1: (x := y)\n",
    "print(...)\nglobal g\nnonlocal_ = 1\nassert x, 'm'\nraise E from F\n",
    "x = 1  # type: int\n",
]
for source in sources:
    for kwargs in ({}, {"type_comments": True}):
        direct = ast.parse(source, **kwargs)
        python = through_python(lambda: ast.parse(source, **kwargs))
        assert shape(direct) == shape(python), source
for mode, source in (("eval", "a + b"), ("single", "x = 1")):
    assert shape(ast.parse(source, mode=mode)) == shape(
        through_python(lambda: ast.parse(source, mode=mode)))
assert ast.parse("x").body[0].value.id is sys.intern("x")
tree = ast.parse("x = y = 1")
assert type(tree.body[0].targets[0].ctx) is ast.Store
assert compile(tree, "<direct>", "exec")


def observed(patch, unpatch):
    """The events of parsing with a node class patched."""
    events = []
    patch(events)
    try:
        tree = ast.parse("x = 1\n")
    finally:
        unpatch()
    return ast.dump(tree, include_attributes=True), events


expected_dump = ast.dump(ast.parse("x = 1\n"), include_attributes=True)


def new_patch(events):
    def new(cls, *args):
        events.append(("new", cls.__name__))
        return object.__new__(cls)
    ast.Constant.__new__ = staticmethod(new)


dump, events = observed(new_patch, lambda: delattr(ast.Constant, "__new__"))
assert dump == expected_dump and events == [("new", "Constant")], events


def setattr_patch(events):
    def setter(self, key, value):
        events.append(key)
        object.__setattr__(self, key, value)
    ast.Name.__setattr__ = setter


dump, events = observed(setattr_patch, lambda: delattr(ast.Name, "__setattr__"))
assert dump == expected_dump, dump
assert events == ["id", "ctx", "lineno", "col_offset", "end_lineno", "end_col_offset"], events


def property_patch(events):
    def get(self):
        return self.__dict__["_col"]

    def put(self, value):
        events.append(("col", value))
        self.__dict__["_col"] = value
    ast.Constant.col_offset = property(get, put)


dump, events = observed(property_patch, lambda: delattr(ast.Constant, "col_offset"))
assert events == [("col", 4)], events


def finalizer_patch(events):
    ast.Store.__del__ = lambda self: events.append("del")


dump, events = observed(finalizer_patch, lambda: delattr(ast.Store, "__del__"))
assert dump == expected_dump and events == [], events


def base_new_patch(events):
    original = ast.AST.__dict__["__new__"]

    def new(cls, *args, **kwargs):
        events.append(cls.__name__)
        return object.__new__(cls)
    ast.AST.__new__ = staticmethod(new)
    base_new_patch.original = original


dump, events = observed(
    base_new_patch, lambda: setattr(ast.AST, "__new__", base_new_patch.original))
assert dump == expected_dump and events == ["Module", "Assign", "Name", "Store", "Constant"], events


# The replacement code runs with `ast`'s globals.
replaced_news = sys._direct_ast_news = []


def replacement_new(cls, /, *args, **kwargs):
    __import__("sys")._direct_ast_news.append(cls.__name__)
    return object.__new__(cls)


def code_patch(events):
    code_patch.events = events
    replaced_news.clear()
    function = ast.AST.__dict__["__new__"].__func__
    code_patch.saved = function.__code__
    function.__code__ = replacement_new.__code__


dump, events = observed(
    code_patch,
    lambda: setattr(ast.AST.__dict__["__new__"].__func__, "__code__", code_patch.saved))
assert dump == expected_dump, dump
assert replaced_news == ["Module", "Assign", "Name", "Store", "Constant"], replaced_news


def registry_patch(events):
    class Counting(dict):
        def __getitem__(self, name):
            events.append(name)
            return dict.__getitem__(self, name)
    registry_patch.saved = ast._NODE_TYPES
    ast._NODE_TYPES = Counting(ast._NODE_TYPES)


dump, events = observed(registry_patch, lambda: setattr(ast, "_NODE_TYPES", registry_patch.saved))
assert dump == expected_dump and events == ["Module", "Assign", "Name", "Store", "Constant"], events


def subclass_patch(events):
    class Constant(ast.Constant):
        pass
    subclass_patch.saved = ast._NODE_TYPES["Constant"]
    ast._NODE_TYPES["Constant"] = Constant
    events.append(Constant)


def subclass_unpatch():
    ast._NODE_TYPES["Constant"] = subclass_patch.saved


events = []
subclass_patch(events)
try:
    tree = ast.parse("x = 1\n")
finally:
    subclass_unpatch()
assert type(tree.body[0].value) is events[0]


def trace_events():
    calls = []

    def trace(frame, event, arg):
        if event == "call" and frame.f_code.co_name in ("_from_spec", "_build", "__new__"):
            calls.append(frame.f_code.co_name)
        return trace
    sys.settrace(trace)
    try:
        ast.parse("x = 1\n")
    finally:
        sys.settrace(None)
    return calls


assert "_from_spec" in trace_events()
print("Native AST direct checks passed")
