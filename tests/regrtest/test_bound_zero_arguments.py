"""Self-only calls preserve binding, keyword arguments, and receiver lifetime."""
import functools
import gc
import weakref


class Methods:
    def method(self, *, value=17):
        return self, value

    @classmethod
    def class_method(cls, *, value=19):
        return cls, value

    @staticmethod
    def static_method(*, value=23):
        return value

    partial = functools.partialmethod(method, value=29)


instance = Methods()
saved = instance.method
assert saved() == (instance, 17)
assert saved(value=31) == (instance, 31)
assert instance.class_method() == (Methods, 19)
assert instance.class_method(value=37) == (Methods, 37)
assert instance.static_method() == 23
assert instance.partial() == (instance, 29)


class Descriptor:
    def __get__(self, instance, owner):
        return lambda *, value=41: (instance, owner, value)


class Callable:
    __call__ = Descriptor()


callable_instance = Callable()
assert callable_instance() == (callable_instance, Callable, 41)
assert callable_instance(value=43) == (callable_instance, Callable, 43)

values = [1, 2, 3]
assert values.copy() == values
assert list({'a': 1}.keys()) == ['a']
assert {1, 2}.copy() == {1, 2}
iterator = iter(values)
assert iterator.__next__() == 1
generator = (value + 1 for value in values)
assert generator.__next__() == 2
generator.close()

missing = []


class Mapping(dict):
    def __missing__(self, key):
        missing.append(key)
        return 47


mapping = Mapping()
try:
    mapping.__getitem__()
except TypeError:
    pass
else:
    raise AssertionError('missing key argument was accepted')
assert missing == []
assert mapping.__getitem__('absent') == 47
assert missing == ['absent']
try:
    values.clear(unexpected=1)
except TypeError:
    pass
else:
    raise AssertionError('unexpected keyword was accepted')
assert values == [1, 2, 3]


class Owned:
    def call(self, *, expected):
        global owner, retained
        del owner, retained
        gc.collect()
        assert witness() is self
        return expected


for expanded in (False, True):
    owner = Owned()
    witness = weakref.ref(owner)
    retained = owner.call
    if expanded:
        # Expanded calls exercise the generic bound-method fallback.
        assert retained(*(), **{'expected': 53}) == 53
    else:
        assert retained(expected=53) == 53
    gc.collect()
    assert witness() is None

# Warm one zero-argument call site, then change its target and result shape.
# Cached native entry must agree with generic dispatch after each change.
def call_zero(callback):
    return callback()


for callback, expected in [([1, 2].__len__, 2), ([3, 5].copy, [3, 5]),
                           ({'a': 7}.copy, {'a': 7}), ({11}.copy, {11}),
                           (instance.method, (instance, 17)), (lambda: 13, 13)]:
    for unused in range(100):
        assert call_zero(callback) == expected

# Errors must survive both cached native entry and arity deoptimization.
for callback in ([].pop, [].append):
    for unused in range(100):
        try:
            call_zero(callback)
        except (IndexError, TypeError):
            pass
        else:
            raise AssertionError('zero-argument native call lost its error')


def call_expanded(callback, arguments):
    return callback(*arguments)


for unused in range(100):
    items = [17, 19]
    assert call_expanded(items.pop, ()) == 19
    assert call_expanded(items.pop, (0,)) == 17

# Native mutation still promptly releases referents, including a destructor
# that reenters the same call site and discards the saved bound method.
deaths = []


class Released:
    def __del__(self):
        global clear_saved
        clear_saved = None
        deaths.append(call_zero([1, 2, 3].__len__))


for unused in range(100):
    items = [Released()]
    clear_saved = items.clear
    call_zero(clear_saved)
    assert clear_saved is None
    assert deaths[-1] == 3
assert deaths == [3] * 100
print('self-only bound calls preserve binding and lifetime')
