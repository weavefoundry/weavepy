from unittest.mock import Mock, MagicMock, patch, ANY, sentinel


m = Mock(return_value=42)
print("mock call result:", m(1, 2, key="v"))
print("call count:", m.call_count)
print("called once:", m.called)
m.assert_called_once()
m.assert_called_with(1, 2, key="v")

# attribute auto-creation
m.do_thing(3)
print("do_thing called:", m.do_thing.called)
print("do_thing args:", m.do_thing.call_args[0])

# spec'd return values
mm = MagicMock()
mm.thing.return_value = "x"
print("mm.thing() type:", type(mm.thing()).__name__ in ("MagicMock", "Mock", "str"))

# side effect
seq = Mock(side_effect=[10, 20, 30])
print("seq:", seq(), seq(), seq())

# ANY / sentinel
m2 = Mock()
m2(1, "hello", [1, 2, 3])
m2.assert_called_with(ANY, ANY, ANY)
print("sentinel works:", sentinel.foo is sentinel.foo)
print("sentinels distinct:", sentinel.foo is not sentinel.bar)


# patch as context manager
class Obj:
    def method(self):
        return "real"


o = Obj()
with patch.object(o, "method", return_value="patched"):
    print("patched:", o.method())
print("restored:", o.method())
