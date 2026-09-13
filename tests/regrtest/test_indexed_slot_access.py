"""Cached slot positions preserve names, descriptors, and mutation semantics."""

import threading


def read(item):
    return item.value


def write(item, value):
    item.value = value


def assert_missing(item):
    try:
        read(item)
    except AttributeError:
        pass
    else:
        raise AssertionError("an unset slot returned another field's value")


def exercise(cls, names):
    def total_value(item, n):
        total = 0
        for _ in range(n):
            total += item.value
        return total

    forward = cls()
    reverse = cls()
    sparse = cls()
    assert_missing(sparse)
    for i, name in enumerate(names):
        setattr(forward, name, i)
    for i, name in enumerate(reversed(names)):
        setattr(reverse, name, -i)
    write(sparse, 1000)
    # Warm a stable position, then use the same sites on a different order
    # and on an instance that has populated only the target slot.
    for i in range(100):
        write(forward, i)
        assert read(forward) == i
        assert total_value(forward, 20) == 20 * i
    for i in range(100):
        for item in (forward, reverse, sparse):
            write(item, i + 300)
            assert read(item) == i + 300
            assert total_value(item, 20) == 20 * (i + 300)
        for name in names[:-1]:
            assert getattr(forward, name) == names.index(name)

    # Ordered removal shifts the target even though its class is unchanged.
    for name in names[:-1]:
        delattr(forward, name)
        assert read(forward) == 399
        assert total_value(forward, 20) == 7980
        write(forward, 400)
        assert read(forward) == 400
        write(forward, 399)
    del forward.value
    assert_missing(forward)
    for name in reversed(names):
        setattr(forward, name, name)
    for i in range(100):
        write(forward, i)
        assert read(forward) == i
        for name in names[:-1]:
            assert getattr(forward, name) == name
    return forward, total_value


for count in (1, 2, 8, 9, 16):
    names = tuple("field%d" % i for i in range(count - 1)) + ("value",)
    cls = type("Slots%d" % count, (), {"__slots__": names})
    item, total_value = exercise(cls, names)
    descriptor = cls.value
    events = []

    def get_value(self):
        events.append("get")
        return 700

    def set_value(self, value):
        events.append(("set", value))

    cls.value = property(get_value, set_value)
    assert read(item) == 700
    write(item, 701)
    assert events == ["get", ("set", 701)]
    events.clear()
    assert total_value(item, 20) == 14000
    assert events == ["get"] * 20
    cls.value = descriptor
    assert read(item) == 99
    assert total_value(item, 20) == 1980
    write(item, 99.5)
    assert total_value(item, 20) == 1990.0
    write(item, None)
    try:
        total_value(item, 20)
    except TypeError:
        pass
    else:
        raise AssertionError("a changed slot type bypassed arithmetic dispatch")
    write(item, 702)
    assert read(item) == 702

    # An inherited member changes resolution when its base is mutated.
    child = type("Child%d" % count, (cls,), {"__slots__": ()})
    inherited = child()
    for i in range(100):
        write(inherited, i)
        assert read(inherited) == i
    cls.value = property(get_value, set_value)
    events.clear()
    assert read(inherited) == 700
    write(inherited, 703)
    assert events == ["get", ("set", 703)]
    cls.value = descriptor
    assert read(inherited) == 99


class SharedCode:
    __slots__ = ("a", "b", "c", "d", "e", "f", "g", "value", "extra")


errors = []


def worker(offset):
    try:
        item = SharedCode()
        names = SharedCode.__slots__
        for name in names[offset:] + names[:offset]:
            setattr(item, name, name)
        for i in range(500):
            write(item, i)
            assert read(item) == i
        for name in names:
            if name != "value":
                assert getattr(item, name) == name
    except BaseException as error:
        errors.append(repr(error))


threads = [threading.Thread(target=worker, args=(offset,)) for offset in range(4)]
for thread in threads:
    thread.start()
for thread in threads:
    thread.join()
assert errors == [], errors
print("ok")
