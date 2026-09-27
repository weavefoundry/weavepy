"""A failed AST construction releases partial results in the original order."""
import _ast
import ast
import gc


def check_partial_list():
    native = _ast._build
    saved_new = ast.Constant.__dict__.get("__new__")
    saved_del = ast.Constant.__dict__.get("__del__")

    def run(force_python):
        gc.collect()
        events = []
        count = [0]

        def new(cls):
            count[0] += 1
            if count[0] == 3:
                raise ValueError("third node")
            return object.__new__(cls)

        def finalizer(node):
            events.append(getattr(node, "value", -1))

        ast.Constant.__new__ = staticmethod(new)
        ast.Constant.__del__ = finalizer
        _ast._build = (lambda spec, builder, state: builder(spec)) if force_python else native
        try:
            try:
                ast._from_spec([{"_type": "Constant", "value": i} for i in (1, 2, 3)])
            except ValueError as error:
                assert str(error) == "third node"
            else:
                raise AssertionError("constructor should fail")
            before = events[:]
            gc.collect()
            return before, events[:]
        finally:
            _ast._build = native
            for name, saved in (("__new__", saved_new), ("__del__", saved_del)):
                if saved is None:
                    delattr(ast.Constant, name)
                else:
                    setattr(ast.Constant, name, saved)

    actual = run(False)
    expected = run(True)
    assert actual == expected, (actual, expected)
    assert sorted(actual[1]) == [1, 2], actual


def check_setter_cleanup():
    native = _ast._build
    saved_del = ast.Constant.__dict__.get("__del__")

    def run(force_python, fail):
        gc.collect()
        events = []
        class ReturnValue:
            def __del__(self):
                events.append("return")
        def finalizer(node):
            events.append(("node", getattr(node, "value", -1)))
        def setter(node, key, value):
            events.append(("set", key))
            if key == "child":
                if fail:
                    raise ValueError("setter failed")
                return ReturnValue()
            object.__setattr__(node, key, value)
        ast.Constant.__del__ = finalizer
        ast.setattr = setter
        _ast._build = (lambda spec, builder, state: builder(spec)) if force_python else native
        try:
            spec = {"_type": "Constant", "value": 1,
                    "child": {"_type": "Constant", "value": 2}, "last": 3}
            try:
                result = ast._from_spec(spec)
            except ValueError as error:
                assert fail and str(error) == "setter failed"
            else:
                assert not fail
                del result
            before = events[:]
            gc.collect()
            return before, events[:]
        finally:
            _ast._build = native
            del ast.setattr
            if saved_del is None:
                del ast.Constant.__del__
            else:
                ast.Constant.__del__ = saved_del

    for fail in (False, True):
        actual = run(False, fail)
        expected = run(True, fail)
        assert actual == expected, (fail, actual, expected)


def check_constructor_temporary():
    native = _ast._build
    saved_new = ast.Constant.__dict__.get("__new__")
    saved_set = ast.Constant.__dict__.get("__setattr__")

    def run(force_python):
        gc.collect()
        events = []
        class Constructor:
            def __call__(self, cls):
                events.append("new")
                return object.__new__(cls)
            def __del__(self):
                events.append("constructor released")
        class Descriptor:
            def __get__(self, instance, owner):
                return Constructor()
        def setter(node, key, value):
            events.append(("set", key))
            object.__setattr__(node, key, value)
        ast.Constant.__new__ = Descriptor()
        ast.Constant.__setattr__ = setter
        _ast._build = (lambda spec, builder, state: builder(spec)) if force_python else native
        try:
            result = ast._from_spec({"_type": "Constant", "value": 7})
            assert result.value == 7
            before = events[:]
            gc.collect()
            return before, events[:]
        finally:
            _ast._build = native
            for name, saved in (("__new__", saved_new), ("__setattr__", saved_set)):
                if saved is None:
                    delattr(ast.Constant, name)
                else:
                    setattr(ast.Constant, name, saved)

    actual = run(False)
    expected = run(True)
    assert actual == expected, (actual, expected)


def check_rebound_builder():
    native = _ast._build
    original = ast._build
    saved_new = ast.Constant.__dict__.get("__new__")
    saved_set = ast.Constant.__dict__.get("__setattr__")

    def run(force_python):
        gc.collect()
        events = []
        class Builder:
            def __call__(self, spec):
                ast._build = original
                events.append("child")
                return spec
            def __del__(self):
                events.append("builder released")
        def new(cls):
            ast._build = Builder()
            return object.__new__(cls)
        def setter(node, key, value):
            events.append(("set", key))
            object.__setattr__(node, key, value)
        ast.Constant.__new__ = staticmethod(new)
        ast.Constant.__setattr__ = setter
        _ast._build = (lambda spec, builder, state: builder(spec)) if force_python else native
        try:
            result = ast._from_spec({"_type": "Constant", "value": 7})
            assert result.value == 7
            before = events[:]
            gc.collect()
            return before, events[:]
        finally:
            _ast._build = native
            ast._build = original
            for name, saved in (("__new__", saved_new), ("__setattr__", saved_set)):
                if saved is None:
                    delattr(ast.Constant, name)
                else:
                    setattr(ast.Constant, name, saved)

    actual = run(False)
    expected = run(True)
    assert actual == expected, (actual, expected)


if hasattr(_ast, "_build"):
    check_partial_list()
    check_setter_cleanup()
    check_constructor_temporary()
    check_rebound_builder()
print("Native AST construction cleanup checks passed")
