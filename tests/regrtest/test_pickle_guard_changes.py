"""The pickle accelerator notices in-place changes to the Python engine it
stands in for: a method's code, and a helper's defaults."""
import pickle
import copyreg


class Point:
    def __init__(self, x):
        self.x = x


def check_round_trips():
    for _ in range(50):
        assert pickle.loads(pickle.dumps("abc")) == "abc"
        assert pickle.loads(pickle.dumps([1, 2.5, None])) == [1, 2.5, None]
        assert pickle.loads(pickle.dumps(Point(3))).x == 3


check_round_trips()

# A patched method's code is what runs wherever the Python engine is the
# reference (the accelerator then stands in for it); a C pickler ignores it.
import types

save_str = pickle._Pickler.save_str
original = save_str.__code__
# The patched code runs with the pickle module's globals.
pickle._guard_test_save_str = types.FunctionType(
    original, save_str.__globals__, "save_str_copy")


def patched(self, obj):
    _guard_test_save_str(self, "patched" if obj == "abc" else obj)


expected = "patched" if issubclass(pickle.Pickler, pickle._Pickler) else "abc"
save_str.__code__ = patched.__code__
try:
    for _ in range(50):
        assert pickle.loads(pickle.dumps("abc")) == expected
        assert pickle.loads(pickle.dumps(["abc", "x"])) == [expected, "x"]
finally:
    save_str.__code__ = original
    del pickle._guard_test_save_str
check_round_trips()

# A helper whose defaults were replaced takes the checked path; the
# results don't change.
saved = copyreg.__newobj__.__defaults__
copyreg.__newobj__.__defaults__ = None
check_round_trips()
copyreg.__newobj__.__defaults__ = saved
check_round_trips()
print("ok")
