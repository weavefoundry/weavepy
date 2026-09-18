"""Pickle ordinary class instances exactly like CPython's C pickler.

`pickle.dumps` and `pickle.loads` handle instances that use the default
object reduction without entering `pickle.py`. Every stream here must equal
the bytes CPython 3.14 produces, and every unusual class must keep the
behavior of the full pickler.

The expected streams come from CPython. Regenerate the table with
`python3.14 tests/regrtest/test_pickle_native_instances.py --generate` and
paste its output over the `EXPECTED` assignment.
"""

import abc
import copyreg
import dataclasses
import hashlib
import io
import pickle
import struct
import sys
import types


class Point:
    __slots__ = ("x", "y", "tag")

    def __init__(self, x, y, tag):
        self.x = x
        self.y = y
        self.tag = tag


class Record:
    def __init__(self, i):
        self.ident = i
        self.name = "record-%d" % i
        self.points = [Point(i + k, i - k, "p%d" % k) for k in range(3)]
        self.meta = {"kind": "rec", "seq": i, "flags": (True, False, None)}


class Empty:
    pass


class Mixed(Point):
    pass


class DictBase:
    def __init__(self):
        self.base = "base-%d" % 1


class SlotChild(DictBase):
    __slots__ = ("extra",)


class Solo:
    __slots__ = "solo"


class ListSlots:
    __slots__ = ["first", "second"]


class Private:
    __slots__ = ("__secret", "plain")

    def __init__(self):
        self.__secret = 41
        self.plain = 42


class Level1:
    __slots__ = ("one",)


class Level2(Level1):
    __slots__ = ("two",)


class Level3(Level2):
    __slots__ = ()


class WithDictSlot:
    __slots__ = ("__dict__", "held")


class WithWeakrefSlot:
    __slots__ = ("__weakref__", "held")


class Outer:
    class Inner:
        def __init__(self, value):
            self.value = value

    class SlotInner:
        __slots__ = ("value",)


class Abstract(abc.ABC):
    def method(self):
        return self.value


class Concrete(Abstract):
    def __init__(self, value):
        self.value = value


@dataclasses.dataclass
class Data:
    number: int
    label: str


class WithEq:
    def __init__(self, value):
        self.value = value

    def __eq__(self, other):
        return type(other) is WithEq and other.value == self.value

    __hash__ = None


# Classes that must keep using the full pickler or unpickler.

CALLS = []


class WithReduce:
    def __init__(self, value):
        self.value = value

    def __reduce__(self):
        CALLS.append("reduce")
        return WithReduce, (self.value,)


class WithReduceEx:
    def __init__(self, value):
        self.value = value

    def __reduce_ex__(self, protocol):
        CALLS.append("reduce_ex")
        return WithReduceEx, (self.value,)


class WithGetstate:
    def __init__(self):
        self.kept = 1
        self.dropped = 2

    def __getstate__(self):
        CALLS.append("getstate")
        return {"kept": self.kept}


class WithSetstate:
    def __init__(self):
        self.value = 5

    def __setstate__(self, state):
        CALLS.append("setstate")
        self.__dict__.update(state)
        self.restored = True


class WithGetnewargs:
    def __new__(cls, value):
        self = object.__new__(cls)
        self.value = value
        return self

    def __getnewargs__(self):
        CALLS.append("getnewargs")
        return (self.value,)


class WithGetnewargsEx:
    def __new__(cls, *, value):
        self = object.__new__(cls)
        self.value = value
        return self

    def __getnewargs_ex__(self):
        CALLS.append("getnewargs_ex")
        return (), {"value": self.value}


class WithNew:
    def __new__(cls):
        CALLS.append("new")
        return object.__new__(cls)


class WithGetattr:
    def __init__(self):
        self.real = 1

    def __getattr__(self, name):
        CALLS.append("getattr:" + name)
        raise AttributeError(name)


class SlotsWithGetattr:
    __slots__ = ("set_slot", "unset_slot")

    def __getattr__(self, name):
        CALLS.append("getattr:" + name)
        raise AttributeError(name)


class WithGetattribute:
    def __init__(self):
        self.real = 1

    def __getattribute__(self, name):
        if not name.startswith("__") or name == "__reduce_ex__":
            CALLS.append("getattribute:" + name)
        return object.__getattribute__(self, name)


class SlotsWithSetattr:
    __slots__ = ("slot",)

    def __setattr__(self, name, value):
        CALLS.append("setattr:" + name)
        object.__setattr__(self, name, value)


class WithDel:
    def __init__(self):
        self.value = 1

    def __del__(self):
        CALLS.append("del")


class SlotShadow(Point):
    # A property hides the inherited slot from getattr and setattr.
    @property
    def tag(self):
        return "shadow"

    @tag.setter
    def tag(self, value):
        CALLS.append("tag-setter")


class DictProperty:
    @property
    def __dict__(self):
        CALLS.append("dict-property")
        return {"fake": 1}


class Registered:
    def __init__(self, value):
        self.value = value


def rebuild_registered(value):
    return Registered(value * 2)


def reduce_registered(obj):
    CALLS.append("dispatch_table")
    return rebuild_registered, (obj.value,)


def instance_reduce_ex(protocol):
    CALLS.append("instance reduce_ex")
    return Empty, ()


class MyList(list):
    pass


class MyDict(dict):
    pass


class MyTuple(tuple):
    pass


class MySet(set):
    pass


class MyInt(int):
    pass


class MyStr(str):
    pass


class MyError(Exception):
    pass


class Renamed:
    pass


NotRenamed = Renamed
Renamed = "no longer the class"


class WrongModule:
    pass


WrongModule.__module__ = "module_that_does_not_exist_for_pickle"


class Meta(type):
    pass


class WithMeta(metaclass=Meta):
    def __init__(self):
        self.value = "meta-%d" % 1


class Hidden:
    pass


HiddenAlias = Hidden
del Hidden


def make_local():
    class Local:
        pass

    return Local()


def record_like(i):
    return Record(i)


def case_dict_only():
    return Record(3)


def case_slots_only():
    return Point(1, -2, "tag-%d" % 1)


def case_mixed():
    value = Mixed(1, 2.5, None)
    value.extra = [1, 2]
    value.label = "mixed-%d" % 0
    return value


def case_slot_child():
    value = SlotChild()
    value.extra = (1, 2)
    return value


def case_empty():
    return Empty()


def case_empty_slots():
    return Point.__new__(Point)


def case_partial_slots():
    value = Point.__new__(Point)
    value.y = 7
    return value


def case_empty_mixed():
    return Mixed.__new__(Mixed)


def case_mixed_dict_only():
    value = Mixed.__new__(Mixed)
    value.only = 1
    return value


def case_mixed_slots_only():
    value = Mixed.__new__(Mixed)
    value.x = 1
    return value


def case_shared():
    point = Point(1, 2, "sp-%d" % 0)
    record = Record(1)
    return [point, point, {"k": point, "r": record}, (record, record), record]


def case_nested_containers():
    return {
        "list": [Record(0), [Point(0, 0, None)]],
        "tuple": (Empty(), (Empty(),)),
        "dict": {1: Point(1, 1, b"bytes"), "two": {"deep": Record(2)}},
    }


def case_shared_strings():
    text = "shared-%d" % 7
    first = Empty()
    second = Empty()
    first.v = text
    second.v = text
    second.w = [text, text]
    return [first, second, text]


def case_shared_state_values():
    shared = ["shared list"]
    first = Empty()
    second = Point(shared, shared, None)
    first.a = shared
    first.b = second
    return (first, second, shared)


def case_nested_class():
    slotted = Outer.SlotInner()
    slotted.value = 3
    return [Outer.Inner(1), Outer(), Outer.Inner(2), slotted]


def case_private_slots():
    return Private()


def case_slot_levels():
    value = Level3()
    value.one = 1
    value.two = [value.one]
    middle = Level2()
    middle.two = 2
    return [value, middle, Level1()]


def case_solo_slots():
    value = Solo()
    value.solo = "solo-%d" % 1
    return value


def case_list_slots():
    value = ListSlots()
    value.second = 2
    value.first = 1
    return value


def case_dict_slot():
    value = WithDictSlot()
    value.held = 1
    value.loose = 2
    return value


def case_weakref_slot():
    value = WithWeakrefSlot()
    value.held = 1
    return value


def case_scalars():
    value = Empty()
    value.values = [
        None, True, False, 0, 255, 256, 65535, 65536, -1, 1 << 31,
        -(1 << 31), 1 << 63, -(1 << 100), 0.0, -0.0, 1.5, float("inf"),
        "", "é€", b"", b"\0\xff", (), (1,), (1, 2, 3, 4),
    ]
    return value


def case_abc():
    return Concrete(5)


def case_dataclass():
    return Data(1, "data-%d" % 1)


def case_with_eq():
    return [WithEq(1), WithEq(1)]


def case_attr_order():
    value = Empty()
    value.z = 1
    value.a = 2
    del value.z
    value.z = 3
    return value


def case_instance_as_dict_value():
    return {"a": Point(1, 2, None), "b": Record(4), 3: Empty()}


def case_many():
    return [Record(i) for i in range(500)]


def case_many_slots():
    return [Point(i, float(i), "many-%d" % i) for i in range(2000)]


def case_big_string_attr():
    value = Empty()
    value.small = "s" * 300
    value.big = "b" * 70000
    value.after = Point(b"x" * 66000, None, None)
    return value


# Shapes that the native encoder or decoder must hand to the full engine.

def case_reduce():
    return [WithReduce(1), Empty()]


def case_reduce_ex():
    return [WithReduceEx(2), Empty()]


def case_getstate():
    return [WithGetstate(), Empty()]


def case_setstate():
    return [WithSetstate(), Empty()]


def case_getnewargs():
    return [WithGetnewargs(3), Empty()]


def case_getnewargs_ex():
    return [WithGetnewargsEx(value=4), Empty()]


def case_new():
    return [WithNew(), Empty()]


def case_getattr():
    return [WithGetattr(), Empty()]


def case_slots_getattr():
    value = SlotsWithGetattr()
    value.set_slot = 1
    return [value, Empty()]


def case_getattribute():
    return [WithGetattribute(), Empty()]


def case_slots_setattr():
    value = SlotsWithSetattr()
    value.slot = 1
    return [value, Empty()]


def case_del():
    return [WithDel(), Empty()]


def case_slot_shadow():
    value = SlotShadow(1, 2, "hidden")
    return [value, Empty()]


def case_registered():
    return [Registered(21), Empty()]


def case_instance_reduce_ex():
    value = Empty()
    value.__reduce_ex__ = instance_reduce_ex
    return [value, Empty()]


def case_non_str_key():
    value = Empty()
    value.__dict__[5] = "five"
    value.name = "name-%d" % 5
    return [value, Empty()]


def case_self_cycle():
    value = Empty()
    value.me = value
    return value


def case_pair_cycle():
    first = Empty()
    second = Point(first, None, None)
    first.other = second
    return [first, second]


def case_container_cycle():
    value = Empty()
    value.items = [value]
    return value


def case_builtin_subclasses():
    items = MyList([1, 2])
    items.note = "list-%d" % 1
    mapping = MyDict(a=1)
    mapping.note = "dict-%d" % 1
    return [items, mapping, MyTuple((1, 2)), MySet([1]), MyInt(7),
            MyStr("text"), Empty()]


def case_exception():
    return [MyError("message", 2), Empty()]


def case_metaclass():
    return [WithMeta(), Empty()]


def case_function_attr():
    value = Empty()
    value.function = record_like
    value.cls = Record
    return value


def case_local():
    return [Empty(), make_local()]


def case_renamed():
    return [Empty(), NotRenamed()]


def case_wrong_module():
    return [Empty(), WrongModule()]


def case_hidden():
    return [Empty(), HiddenAlias()]


def case_dict_property():
    return [DictProperty(), Empty()]


# Streams that both the native encoder and the native decoder handle.
NATIVE_CASES = [
    "dict_only", "slots_only", "mixed", "slot_child", "empty", "empty_slots",
    "partial_slots", "empty_mixed", "mixed_dict_only", "mixed_slots_only",
    "shared", "nested_containers", "shared_strings", "shared_state_values",
    "nested_class", "private_slots", "slot_levels", "solo_slots",
    "list_slots", "scalars", "abc", "dataclass", "with_eq", "attr_order",
    "instance_as_dict_value", "metaclass", "many", "many_slots",
    "big_string_attr",
]
# Shapes where the encoder, the decoder, or both hand the work to the
# full engine, which must still produce CPython's bytes and objects.
FALLBACK_CASES = [
    "dict_slot", "weakref_slot", "reduce", "reduce_ex", "getstate",
    "setstate", "getnewargs", "getnewargs_ex", "new", "getattr",
    "slots_getattr", "getattribute", "slots_setattr", "del", "slot_shadow",
    "registered", "instance_reduce_ex", "non_str_key", "self_cycle",
    "pair_cycle", "container_cycle", "builtin_subclasses", "exception",
    "function_attr",
]
ERROR_CASES = ["local", "renamed", "wrong_module", "hidden"]
# CPython's own `pickle._Pickler` output differs from its C pickler here,
# and the large streams take too long in it.
PYTHON_ENGINE_DIFFERS = ("exception", "many", "many_slots", "big_string_attr")
# These reductions deliberately rebuild something other than a copy.
LOSSY_CASES = ("registered", "getstate", "setstate", "instance_reduce_ex")

copyreg.pickle(Registered, reduce_registered)


def build(name):
    return globals()["case_" + name]()


def describe(blob):
    if len(blob) > 4096:
        return ("sha256", len(blob), hashlib.sha256(blob).hexdigest())
    return blob


def generate():
    print("EXPECTED = {")
    for name in NATIVE_CASES + FALLBACK_CASES:
        for protocol in (4, 5):
            blob = pickle.dumps(build(name), protocol=protocol)
            print("    (%r, %d): %r," % (name, protocol, describe(blob)))
    print("}")


# Generated by CPython 3.14; see the module docstring.
EXPECTED = {
    ('dict_only', 4): b'\x80\x04\x95\xdb\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x06Record\x94\x93\x94)\x81\x94}\x94(\x8c\x05ident\x94K\x03\x8c\x04name\x94\x8c\x08record-3\x94\x8c\x06points\x94]\x94(h\x00\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94K\x03\x8c\x01y\x94K\x03\x8c\x03tag\x94\x8c\x02p0\x94u\x86\x94bh\x0b)\x81\x94N}\x94(h\x0eK\x04h\x0fK\x02h\x10\x8c\x02p1\x94u\x86\x94bh\x0b)\x81\x94N}\x94(h\x0eK\x05h\x0fK\x01h\x10\x8c\x02p2\x94u\x86\x94be\x8c\x04meta\x94}\x94(\x8c\x04kind\x94\x8c\x03rec\x94\x8c\x03seq\x94K\x03\x8c\x05flags\x94\x88\x89N\x87\x94uub.',
    ('dict_only', 5): b'\x80\x05\x95\xdb\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x06Record\x94\x93\x94)\x81\x94}\x94(\x8c\x05ident\x94K\x03\x8c\x04name\x94\x8c\x08record-3\x94\x8c\x06points\x94]\x94(h\x00\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94K\x03\x8c\x01y\x94K\x03\x8c\x03tag\x94\x8c\x02p0\x94u\x86\x94bh\x0b)\x81\x94N}\x94(h\x0eK\x04h\x0fK\x02h\x10\x8c\x02p1\x94u\x86\x94bh\x0b)\x81\x94N}\x94(h\x0eK\x05h\x0fK\x01h\x10\x8c\x02p2\x94u\x86\x94be\x8c\x04meta\x94}\x94(\x8c\x04kind\x94\x8c\x03rec\x94\x8c\x03seq\x94K\x03\x8c\x05flags\x94\x88\x89N\x87\x94uub.',
    ('slots_only', 4): b'\x80\x04\x95>\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94K\x01\x8c\x01y\x94J\xfe\xff\xff\xff\x8c\x03tag\x94\x8c\x05tag-1\x94u\x86\x94b.',
    ('slots_only', 5): b'\x80\x05\x95>\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94K\x01\x8c\x01y\x94J\xfe\xff\xff\xff\x8c\x03tag\x94\x8c\x05tag-1\x94u\x86\x94b.',
    ('mixed', 4): b'\x80\x04\x95`\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Mixed\x94\x93\x94)\x81\x94}\x94(\x8c\x05extra\x94]\x94(K\x01K\x02e\x8c\x05label\x94\x8c\x07mixed-0\x94u}\x94(\x8c\x01x\x94K\x01\x8c\x01y\x94G@\x04\x00\x00\x00\x00\x00\x00\x8c\x03tag\x94Nu\x86\x94b.',
    ('mixed', 5): b'\x80\x05\x95`\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Mixed\x94\x93\x94)\x81\x94}\x94(\x8c\x05extra\x94]\x94(K\x01K\x02e\x8c\x05label\x94\x8c\x07mixed-0\x94u}\x94(\x8c\x01x\x94K\x01\x8c\x01y\x94G@\x04\x00\x00\x00\x00\x00\x00\x8c\x03tag\x94Nu\x86\x94b.',
    ('slot_child', 4): b'\x80\x04\x95D\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\tSlotChild\x94\x93\x94)\x81\x94}\x94\x8c\x04base\x94\x8c\x06base-1\x94s}\x94\x8c\x05extra\x94K\x01K\x02\x86\x94s\x86\x94b.',
    ('slot_child', 5): b'\x80\x05\x95D\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\tSlotChild\x94\x93\x94)\x81\x94}\x94\x8c\x04base\x94\x8c\x06base-1\x94s}\x94\x8c\x05extra\x94K\x01K\x02\x86\x94s\x86\x94b.',
    ('empty', 4): b'\x80\x04\x95\x19\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94.',
    ('empty', 5): b'\x80\x05\x95\x19\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94.',
    ('empty_slots', 4): b'\x80\x04\x95\x19\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Point\x94\x93\x94)\x81\x94.',
    ('empty_slots', 5): b'\x80\x05\x95\x19\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Point\x94\x93\x94)\x81\x94.',
    ('partial_slots', 4): b'\x80\x04\x95&\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94\x8c\x01y\x94K\x07s\x86\x94b.',
    ('partial_slots', 5): b'\x80\x05\x95&\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94\x8c\x01y\x94K\x07s\x86\x94b.',
    ('empty_mixed', 4): b'\x80\x04\x95\x19\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Mixed\x94\x93\x94)\x81\x94.',
    ('empty_mixed', 5): b'\x80\x05\x95\x19\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Mixed\x94\x93\x94)\x81\x94.',
    ('mixed_dict_only', 4): b'\x80\x04\x95&\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Mixed\x94\x93\x94)\x81\x94}\x94\x8c\x04only\x94K\x01sb.',
    ('mixed_dict_only', 5): b'\x80\x05\x95&\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Mixed\x94\x93\x94)\x81\x94}\x94\x8c\x04only\x94K\x01sb.',
    ('mixed_slots_only', 4): b'\x80\x04\x95&\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Mixed\x94\x93\x94)\x81\x94N}\x94\x8c\x01x\x94K\x01s\x86\x94b.',
    ('mixed_slots_only', 5): b'\x80\x05\x95&\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Mixed\x94\x93\x94)\x81\x94N}\x94\x8c\x01x\x94K\x01s\x86\x94b.',
    ('shared', 4): b'\x80\x04\x95\x18\x01\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94K\x01\x8c\x01y\x94K\x02\x8c\x03tag\x94\x8c\x04sp-0\x94u\x86\x94bh\x04}\x94(\x8c\x01k\x94h\x04\x8c\x01r\x94h\x01\x8c\x06Record\x94\x93\x94)\x81\x94}\x94(\x8c\x05ident\x94K\x01\x8c\x04name\x94\x8c\x08record-1\x94\x8c\x06points\x94]\x94(h\x03)\x81\x94N}\x94(h\x06K\x01h\x07K\x01h\x08\x8c\x02p0\x94u\x86\x94bh\x03)\x81\x94N}\x94(h\x06K\x02h\x07K\x00h\x08\x8c\x02p1\x94u\x86\x94bh\x03)\x81\x94N}\x94(h\x06K\x03h\x07J\xff\xff\xff\xffh\x08\x8c\x02p2\x94u\x86\x94be\x8c\x04meta\x94}\x94(\x8c\x04kind\x94\x8c\x03rec\x94\x8c\x03seq\x94K\x01\x8c\x05flags\x94\x88\x89N\x87\x94uubuh\x10h\x10\x86\x94h\x10e.',
    ('shared', 5): b'\x80\x05\x95\x18\x01\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94K\x01\x8c\x01y\x94K\x02\x8c\x03tag\x94\x8c\x04sp-0\x94u\x86\x94bh\x04}\x94(\x8c\x01k\x94h\x04\x8c\x01r\x94h\x01\x8c\x06Record\x94\x93\x94)\x81\x94}\x94(\x8c\x05ident\x94K\x01\x8c\x04name\x94\x8c\x08record-1\x94\x8c\x06points\x94]\x94(h\x03)\x81\x94N}\x94(h\x06K\x01h\x07K\x01h\x08\x8c\x02p0\x94u\x86\x94bh\x03)\x81\x94N}\x94(h\x06K\x02h\x07K\x00h\x08\x8c\x02p1\x94u\x86\x94bh\x03)\x81\x94N}\x94(h\x06K\x03h\x07J\xff\xff\xff\xffh\x08\x8c\x02p2\x94u\x86\x94be\x8c\x04meta\x94}\x94(\x8c\x04kind\x94\x8c\x03rec\x94\x8c\x03seq\x94K\x01\x8c\x05flags\x94\x88\x89N\x87\x94uubuh\x10h\x10\x86\x94h\x10e.',
    ('nested_containers', 4): b'\x80\x04\x95\xee\x01\x00\x00\x00\x00\x00\x00}\x94(\x8c\x04list\x94]\x94(\x8c\x08__main__\x94\x8c\x06Record\x94\x93\x94)\x81\x94}\x94(\x8c\x05ident\x94K\x00\x8c\x04name\x94\x8c\x08record-0\x94\x8c\x06points\x94]\x94(h\x03\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94K\x00\x8c\x01y\x94K\x00\x8c\x03tag\x94\x8c\x02p0\x94u\x86\x94bh\x0e)\x81\x94N}\x94(h\x11K\x01h\x12J\xff\xff\xff\xffh\x13\x8c\x02p1\x94u\x86\x94bh\x0e)\x81\x94N}\x94(h\x11K\x02h\x12J\xfe\xff\xff\xffh\x13\x8c\x02p2\x94u\x86\x94be\x8c\x04meta\x94}\x94(\x8c\x04kind\x94\x8c\x03rec\x94\x8c\x03seq\x94K\x00\x8c\x05flags\x94\x88\x89N\x87\x94uub]\x94h\x0e)\x81\x94N}\x94(h\x11K\x00h\x12K\x00h\x13Nu\x86\x94bae\x8c\x05tuple\x94h\x03\x8c\x05Empty\x94\x93\x94)\x81\x94h+)\x81\x94\x85\x94\x86\x94\x8c\x04dict\x94}\x94(K\x01h\x0e)\x81\x94N}\x94(h\x11K\x01h\x12K\x01h\x13C\x05bytes\x94u\x86\x94b\x8c\x03two\x94}\x94\x8c\x04deep\x94h\x05)\x81\x94}\x94(h\x08K\x02h\t\x8c\x08record-2\x94h\x0b]\x94(h\x0e)\x81\x94N}\x94(h\x11K\x02h\x12K\x02h\x13\x8c\x02p0\x94u\x86\x94bh\x0e)\x81\x94N}\x94(h\x11K\x03h\x12K\x01h\x13\x8c\x02p1\x94u\x86\x94bh\x0e)\x81\x94N}\x94(h\x11K\x04h\x12K\x00h\x13\x8c\x02p2\x94u\x86\x94beh\x1e}\x94(h h!h"K\x02h#h$uubsuu.',
    ('nested_containers', 5): b'\x80\x05\x95\xee\x01\x00\x00\x00\x00\x00\x00}\x94(\x8c\x04list\x94]\x94(\x8c\x08__main__\x94\x8c\x06Record\x94\x93\x94)\x81\x94}\x94(\x8c\x05ident\x94K\x00\x8c\x04name\x94\x8c\x08record-0\x94\x8c\x06points\x94]\x94(h\x03\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94K\x00\x8c\x01y\x94K\x00\x8c\x03tag\x94\x8c\x02p0\x94u\x86\x94bh\x0e)\x81\x94N}\x94(h\x11K\x01h\x12J\xff\xff\xff\xffh\x13\x8c\x02p1\x94u\x86\x94bh\x0e)\x81\x94N}\x94(h\x11K\x02h\x12J\xfe\xff\xff\xffh\x13\x8c\x02p2\x94u\x86\x94be\x8c\x04meta\x94}\x94(\x8c\x04kind\x94\x8c\x03rec\x94\x8c\x03seq\x94K\x00\x8c\x05flags\x94\x88\x89N\x87\x94uub]\x94h\x0e)\x81\x94N}\x94(h\x11K\x00h\x12K\x00h\x13Nu\x86\x94bae\x8c\x05tuple\x94h\x03\x8c\x05Empty\x94\x93\x94)\x81\x94h+)\x81\x94\x85\x94\x86\x94\x8c\x04dict\x94}\x94(K\x01h\x0e)\x81\x94N}\x94(h\x11K\x01h\x12K\x01h\x13C\x05bytes\x94u\x86\x94b\x8c\x03two\x94}\x94\x8c\x04deep\x94h\x05)\x81\x94}\x94(h\x08K\x02h\t\x8c\x08record-2\x94h\x0b]\x94(h\x0e)\x81\x94N}\x94(h\x11K\x02h\x12K\x02h\x13\x8c\x02p0\x94u\x86\x94bh\x0e)\x81\x94N}\x94(h\x11K\x03h\x12K\x01h\x13\x8c\x02p1\x94u\x86\x94bh\x0e)\x81\x94N}\x94(h\x11K\x04h\x12K\x00h\x13\x8c\x02p2\x94u\x86\x94beh\x1e}\x94(h h!h"K\x02h#h$uubsuu.',
    ('shared_strings', 4): b'\x80\x04\x95L\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94\x8c\x01v\x94\x8c\x08shared-7\x94sbh\x03)\x81\x94}\x94(h\x06h\x07\x8c\x01w\x94]\x94(h\x07h\x07eubh\x07e.',
    ('shared_strings', 5): b'\x80\x05\x95L\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94\x8c\x01v\x94\x8c\x08shared-7\x94sbh\x03)\x81\x94}\x94(h\x06h\x07\x8c\x01w\x94]\x94(h\x07h\x07eubh\x07e.',
    ('shared_state_values', 4): b'\x80\x04\x95g\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94(\x8c\x01a\x94]\x94\x8c\x0bshared list\x94a\x8c\x01b\x94h\x00\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94h\x06\x8c\x01y\x94h\x06\x8c\x03tag\x94Nu\x86\x94bubh\x0bh\x06\x87\x94.',
    ('shared_state_values', 5): b'\x80\x05\x95g\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94(\x8c\x01a\x94]\x94\x8c\x0bshared list\x94a\x8c\x01b\x94h\x00\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94h\x06\x8c\x01y\x94h\x06\x8c\x03tag\x94Nu\x86\x94bubh\x0bh\x06\x87\x94.',
    ('nested_class', 4): b'\x80\x04\x95q\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x0bOuter.Inner\x94\x93\x94)\x81\x94}\x94\x8c\x05value\x94K\x01sbh\x01\x8c\x05Outer\x94\x93\x94)\x81\x94h\x03)\x81\x94}\x94h\x06K\x02sbh\x01\x8c\x0fOuter.SlotInner\x94\x93\x94)\x81\x94N}\x94h\x06K\x03s\x86\x94be.',
    ('nested_class', 5): b'\x80\x05\x95q\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x0bOuter.Inner\x94\x93\x94)\x81\x94}\x94\x8c\x05value\x94K\x01sbh\x01\x8c\x05Outer\x94\x93\x94)\x81\x94h\x03)\x81\x94}\x94h\x06K\x02sbh\x01\x8c\x0fOuter.SlotInner\x94\x93\x94)\x81\x94N}\x94h\x06K\x03s\x86\x94be.',
    ('private_slots', 4): b'\x80\x04\x95B\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x07Private\x94\x93\x94)\x81\x94N}\x94(\x8c\x10_Private__secret\x94K)\x8c\x05plain\x94K*u\x86\x94b.',
    ('private_slots', 5): b'\x80\x05\x95B\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x07Private\x94\x93\x94)\x81\x94N}\x94(\x8c\x10_Private__secret\x94K)\x8c\x05plain\x94K*u\x86\x94b.',
    ('slot_levels', 4): b'\x80\x04\x95d\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x06Level3\x94\x93\x94)\x81\x94N}\x94(\x8c\x03two\x94]\x94K\x01a\x8c\x03one\x94K\x01u\x86\x94bh\x01\x8c\x06Level2\x94\x93\x94)\x81\x94N}\x94h\x06K\x02s\x86\x94bh\x01\x8c\x06Level1\x94\x93\x94)\x81\x94e.',
    ('slot_levels', 5): b'\x80\x05\x95d\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x06Level3\x94\x93\x94)\x81\x94N}\x94(\x8c\x03two\x94]\x94K\x01a\x8c\x03one\x94K\x01u\x86\x94bh\x01\x8c\x06Level2\x94\x93\x94)\x81\x94N}\x94h\x06K\x02s\x86\x94bh\x01\x8c\x06Level1\x94\x93\x94)\x81\x94e.',
    ('solo_slots', 4): b'\x80\x04\x95/\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x04Solo\x94\x93\x94)\x81\x94N}\x94\x8c\x04solo\x94\x8c\x06solo-1\x94s\x86\x94b.',
    ('solo_slots', 5): b'\x80\x05\x95/\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x04Solo\x94\x93\x94)\x81\x94N}\x94\x8c\x04solo\x94\x8c\x06solo-1\x94s\x86\x94b.',
    ('list_slots', 4): b'\x80\x04\x95:\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\tListSlots\x94\x93\x94)\x81\x94N}\x94(\x8c\x05first\x94K\x01\x8c\x06second\x94K\x02u\x86\x94b.',
    ('list_slots', 5): b'\x80\x05\x95:\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\tListSlots\x94\x93\x94)\x81\x94N}\x94(\x8c\x05first\x94K\x01\x8c\x06second\x94K\x02u\x86\x94b.',
    ('scalars', 4): b'\x80\x04\x95\xae\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94\x8c\x06values\x94]\x94(N\x88\x89K\x00K\xffM\x00\x01M\xff\xffJ\x00\x00\x01\x00J\xff\xff\xff\xff\x8a\x05\x00\x00\x00\x80\x00J\x00\x00\x00\x80\x8a\t\x00\x00\x00\x00\x00\x00\x00\x80\x00\x8a\r\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xf0G\x00\x00\x00\x00\x00\x00\x00\x00G\x80\x00\x00\x00\x00\x00\x00\x00G?\xf8\x00\x00\x00\x00\x00\x00G\x7f\xf0\x00\x00\x00\x00\x00\x00\x8c\x00\x94\x8c\x05\xc3\xa9\xe2\x82\xac\x94C\x00\x94C\x02\x00\xff\x94)K\x01\x85\x94(K\x01K\x02K\x03K\x04t\x94esb.',
    ('scalars', 5): b'\x80\x05\x95\xae\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94\x8c\x06values\x94]\x94(N\x88\x89K\x00K\xffM\x00\x01M\xff\xffJ\x00\x00\x01\x00J\xff\xff\xff\xff\x8a\x05\x00\x00\x00\x80\x00J\x00\x00\x00\x80\x8a\t\x00\x00\x00\x00\x00\x00\x00\x80\x00\x8a\r\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xf0G\x00\x00\x00\x00\x00\x00\x00\x00G\x80\x00\x00\x00\x00\x00\x00\x00G?\xf8\x00\x00\x00\x00\x00\x00G\x7f\xf0\x00\x00\x00\x00\x00\x00\x8c\x00\x94\x8c\x05\xc3\xa9\xe2\x82\xac\x94C\x00\x94C\x02\x00\xff\x94)K\x01\x85\x94(K\x01K\x02K\x03K\x04t\x94esb.',
    ('abc', 4): b'\x80\x04\x95*\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x08Concrete\x94\x93\x94)\x81\x94}\x94\x8c\x05value\x94K\x05sb.',
    ('abc', 5): b'\x80\x05\x95*\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x08Concrete\x94\x93\x94)\x81\x94}\x94\x8c\x05value\x94K\x05sb.',
    ('dataclass', 4): b'\x80\x04\x959\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x04Data\x94\x93\x94)\x81\x94}\x94(\x8c\x06number\x94K\x01\x8c\x05label\x94\x8c\x06data-1\x94ub.',
    ('dataclass', 5): b'\x80\x05\x959\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x04Data\x94\x93\x94)\x81\x94}\x94(\x8c\x06number\x94K\x01\x8c\x05label\x94\x8c\x06data-1\x94ub.',
    ('with_eq', 4): b'\x80\x04\x959\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x06WithEq\x94\x93\x94)\x81\x94}\x94\x8c\x05value\x94K\x01sbh\x03)\x81\x94}\x94h\x06K\x01sbe.',
    ('with_eq', 5): b'\x80\x05\x959\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x06WithEq\x94\x93\x94)\x81\x94}\x94\x8c\x05value\x94K\x01sbh\x03)\x81\x94}\x94h\x06K\x01sbe.',
    ('attr_order', 4): b'\x80\x04\x95*\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94(\x8c\x01a\x94K\x02\x8c\x01z\x94K\x03ub.',
    ('attr_order', 5): b'\x80\x05\x95*\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94(\x8c\x01a\x94K\x02\x8c\x01z\x94K\x03ub.',
    ('instance_as_dict_value', 4): b'\x80\x04\x95\x10\x01\x00\x00\x00\x00\x00\x00}\x94(\x8c\x01a\x94\x8c\x08__main__\x94\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94K\x01\x8c\x01y\x94K\x02\x8c\x03tag\x94Nu\x86\x94b\x8c\x01b\x94h\x02\x8c\x06Record\x94\x93\x94)\x81\x94}\x94(\x8c\x05ident\x94K\x04\x8c\x04name\x94\x8c\x08record-4\x94\x8c\x06points\x94]\x94(h\x04)\x81\x94N}\x94(h\x07K\x04h\x08K\x04h\t\x8c\x02p0\x94u\x86\x94bh\x04)\x81\x94N}\x94(h\x07K\x05h\x08K\x03h\t\x8c\x02p1\x94u\x86\x94bh\x04)\x81\x94N}\x94(h\x07K\x06h\x08K\x02h\t\x8c\x02p2\x94u\x86\x94be\x8c\x04meta\x94}\x94(\x8c\x04kind\x94\x8c\x03rec\x94\x8c\x03seq\x94K\x04\x8c\x05flags\x94\x88\x89N\x87\x94uubK\x03h\x02\x8c\x05Empty\x94\x93\x94)\x81\x94u.',
    ('instance_as_dict_value', 5): b'\x80\x05\x95\x10\x01\x00\x00\x00\x00\x00\x00}\x94(\x8c\x01a\x94\x8c\x08__main__\x94\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94K\x01\x8c\x01y\x94K\x02\x8c\x03tag\x94Nu\x86\x94b\x8c\x01b\x94h\x02\x8c\x06Record\x94\x93\x94)\x81\x94}\x94(\x8c\x05ident\x94K\x04\x8c\x04name\x94\x8c\x08record-4\x94\x8c\x06points\x94]\x94(h\x04)\x81\x94N}\x94(h\x07K\x04h\x08K\x04h\t\x8c\x02p0\x94u\x86\x94bh\x04)\x81\x94N}\x94(h\x07K\x05h\x08K\x03h\t\x8c\x02p1\x94u\x86\x94bh\x04)\x81\x94N}\x94(h\x07K\x06h\x08K\x02h\t\x8c\x02p2\x94u\x86\x94be\x8c\x04meta\x94}\x94(\x8c\x04kind\x94\x8c\x03rec\x94\x8c\x03seq\x94K\x04\x8c\x05flags\x94\x88\x89N\x87\x94uubK\x03h\x02\x8c\x05Empty\x94\x93\x94)\x81\x94u.',
    ('many', 4): ('sha256', 70459, '1ec1876c531234796b98224395ec4b08f386725e227a40a5484b973d938b940a'),
    ('many', 5): ('sha256', 70459, '305085d0eecff20471dca2a4be3e2377e9cd4c60b64713c2be7b261f70a24041'),
    ('many_slots', 4): ('sha256', 84688, '962b8bb45816f85513291ab2fc9890924b4ec4779464613d3740ecff748e70f8'),
    ('many_slots', 5): ('sha256', 84688, 'f5b2a9825b7f41feb5c9d5495a510d35e7d7ae383a10c8a2b81d5af7948e9ed3'),
    ('big_string_attr', 4): ('sha256', 136438, 'c1a9a9d63936882b746c63a404be79f0e75994441d57d03616e0282833ddf3d9'),
    ('big_string_attr', 5): ('sha256', 136438, '323a3c868910a4f7b1099074b7f77f697b5771e0b3415261074be86801d1276d'),
    ('dict_slot', 4): b'\x80\x04\x95<\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x0cWithDictSlot\x94\x93\x94)\x81\x94}\x94\x8c\x05loose\x94K\x02s}\x94\x8c\x04held\x94K\x01s\x86\x94b.',
    ('dict_slot', 5): b'\x80\x05\x95<\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x0cWithDictSlot\x94\x93\x94)\x81\x94}\x94\x8c\x05loose\x94K\x02s}\x94\x8c\x04held\x94K\x01s\x86\x94b.',
    ('weakref_slot', 4): b'\x80\x04\x953\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x0fWithWeakrefSlot\x94\x93\x94)\x81\x94N}\x94\x8c\x04held\x94K\x01s\x86\x94b.',
    ('weakref_slot', 5): b'\x80\x05\x953\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x0fWithWeakrefSlot\x94\x93\x94)\x81\x94N}\x94\x8c\x04held\x94K\x01s\x86\x94b.',
    ('reduce', 4): b'\x80\x04\x954\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\nWithReduce\x94\x93\x94K\x01\x85\x94R\x94h\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('reduce', 5): b'\x80\x05\x954\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\nWithReduce\x94\x93\x94K\x01\x85\x94R\x94h\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('reduce_ex', 4): b'\x80\x04\x956\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x0cWithReduceEx\x94\x93\x94K\x02\x85\x94R\x94h\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('reduce_ex', 5): b'\x80\x05\x956\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x0cWithReduceEx\x94\x93\x94K\x02\x85\x94R\x94h\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('getstate', 4): b'\x80\x04\x95@\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x0cWithGetstate\x94\x93\x94)\x81\x94}\x94\x8c\x04kept\x94K\x01sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('getstate', 5): b'\x80\x05\x95@\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x0cWithGetstate\x94\x93\x94)\x81\x94}\x94\x8c\x04kept\x94K\x01sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('setstate', 4): b'\x80\x04\x95A\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x0cWithSetstate\x94\x93\x94)\x81\x94}\x94\x8c\x05value\x94K\x05sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('setstate', 5): b'\x80\x05\x95A\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x0cWithSetstate\x94\x93\x94)\x81\x94}\x94\x8c\x05value\x94K\x05sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('getnewargs', 4): b'\x80\x04\x95F\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x0eWithGetnewargs\x94\x93\x94K\x03\x85\x94\x81\x94}\x94\x8c\x05value\x94K\x03sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('getnewargs', 5): b'\x80\x05\x95F\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x0eWithGetnewargs\x94\x93\x94K\x03\x85\x94\x81\x94}\x94\x8c\x05value\x94K\x03sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('getnewargs_ex', 4): b'\x80\x04\x95L\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x10WithGetnewargsEx\x94\x93\x94)}\x94\x8c\x05value\x94K\x04s\x92\x94}\x94h\x05K\x04sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('getnewargs_ex', 5): b'\x80\x05\x95L\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x10WithGetnewargsEx\x94\x93\x94)}\x94\x8c\x05value\x94K\x04s\x92\x94}\x94h\x05K\x04sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('new', 4): b'\x80\x04\x95.\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x07WithNew\x94\x93\x94)\x81\x94h\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('new', 5): b'\x80\x05\x95.\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x07WithNew\x94\x93\x94)\x81\x94h\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('getattr', 4): b'\x80\x04\x95?\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x0bWithGetattr\x94\x93\x94)\x81\x94}\x94\x8c\x04real\x94K\x01sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('getattr', 5): b'\x80\x05\x95?\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x0bWithGetattr\x94\x93\x94)\x81\x94}\x94\x8c\x04real\x94K\x01sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('slots_getattr', 4): b'\x80\x04\x95K\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x10SlotsWithGetattr\x94\x93\x94)\x81\x94N}\x94\x8c\x08set_slot\x94K\x01s\x86\x94bh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('slots_getattr', 5): b'\x80\x05\x95K\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x10SlotsWithGetattr\x94\x93\x94)\x81\x94N}\x94\x8c\x08set_slot\x94K\x01s\x86\x94bh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('getattribute', 4): b'\x80\x04\x95D\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x10WithGetattribute\x94\x93\x94)\x81\x94}\x94\x8c\x04real\x94K\x01sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('getattribute', 5): b'\x80\x05\x95D\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x10WithGetattribute\x94\x93\x94)\x81\x94}\x94\x8c\x04real\x94K\x01sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('slots_setattr', 4): b'\x80\x04\x95G\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x10SlotsWithSetattr\x94\x93\x94)\x81\x94N}\x94\x8c\x04slot\x94K\x01s\x86\x94bh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('slots_setattr', 5): b'\x80\x05\x95G\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x10SlotsWithSetattr\x94\x93\x94)\x81\x94N}\x94\x8c\x04slot\x94K\x01s\x86\x94bh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('del', 4): b'\x80\x04\x95<\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x07WithDel\x94\x93\x94)\x81\x94}\x94\x8c\x05value\x94K\x01sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('del', 5): b'\x80\x05\x95<\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x07WithDel\x94\x93\x94)\x81\x94}\x94\x8c\x05value\x94K\x01sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('slot_shadow', 4): b'\x80\x04\x95T\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\nSlotShadow\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94K\x01\x8c\x01y\x94K\x02\x8c\x03tag\x94\x8c\x06shadow\x94u\x86\x94bh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('slot_shadow', 5): b'\x80\x05\x95T\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\nSlotShadow\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94K\x01\x8c\x01y\x94K\x02\x8c\x03tag\x94\x8c\x06shadow\x94u\x86\x94bh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('registered', 4): b'\x80\x04\x95<\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x12rebuild_registered\x94\x93\x94K\x15\x85\x94R\x94h\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('registered', 5): b'\x80\x05\x95<\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x12rebuild_registered\x94\x93\x94K\x15\x85\x94R\x94h\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('instance_reduce_ex', 4): b'\x80\x04\x95"\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)R\x94h\x03)\x81\x94e.',
    ('instance_reduce_ex', 5): b'\x80\x05\x95"\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)R\x94h\x03)\x81\x94e.',
    ('non_str_key', 4): b'\x80\x04\x95@\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94(K\x05\x8c\x04five\x94\x8c\x04name\x94\x8c\x06name-5\x94ubh\x03)\x81\x94e.',
    ('non_str_key', 5): b'\x80\x05\x95@\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94(K\x05\x8c\x04five\x94\x8c\x04name\x94\x8c\x06name-5\x94ubh\x03)\x81\x94e.',
    ('self_cycle', 4): b'\x80\x04\x95$\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94\x8c\x02me\x94h\x03sb.',
    ('self_cycle', 5): b'\x80\x05\x95$\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94\x8c\x02me\x94h\x03sb.',
    ('pair_cycle', 4): b'\x80\x04\x95T\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94\x8c\x05other\x94h\x01\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94h\x04\x8c\x01y\x94N\x8c\x03tag\x94Nu\x86\x94bsbh\te.',
    ('pair_cycle', 5): b'\x80\x05\x95T\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94\x8c\x05other\x94h\x01\x8c\x05Point\x94\x93\x94)\x81\x94N}\x94(\x8c\x01x\x94h\x04\x8c\x01y\x94N\x8c\x03tag\x94Nu\x86\x94bsbh\te.',
    ('container_cycle', 4): b'\x80\x04\x95*\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94\x8c\x05items\x94]\x94h\x03asb.',
    ('container_cycle', 5): b'\x80\x05\x95*\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94\x8c\x05items\x94]\x94h\x03asb.',
    ('builtin_subclasses', 4): b'\x80\x04\x95\xc3\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x06MyList\x94\x93\x94)\x81\x94(K\x01K\x02e}\x94\x8c\x04note\x94\x8c\x06list-1\x94sbh\x01\x8c\x06MyDict\x94\x93\x94)\x81\x94\x8c\x01a\x94K\x01s}\x94h\x06\x8c\x06dict-1\x94sbh\x01\x8c\x07MyTuple\x94\x93\x94K\x01K\x02\x86\x94\x85\x94\x81\x94h\x01\x8c\x05MySet\x94\x93\x94]\x94K\x01a\x85\x94R\x94h\x01\x8c\x05MyInt\x94\x93\x94K\x07\x85\x94\x81\x94h\x01\x8c\x05MyStr\x94\x93\x94\x8c\x04text\x94\x85\x94\x81\x94h\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('builtin_subclasses', 5): b'\x80\x05\x95\xc3\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x06MyList\x94\x93\x94)\x81\x94(K\x01K\x02e}\x94\x8c\x04note\x94\x8c\x06list-1\x94sbh\x01\x8c\x06MyDict\x94\x93\x94)\x81\x94\x8c\x01a\x94K\x01s}\x94h\x06\x8c\x06dict-1\x94sbh\x01\x8c\x07MyTuple\x94\x93\x94K\x01K\x02\x86\x94\x85\x94\x81\x94h\x01\x8c\x05MySet\x94\x93\x94]\x94K\x01a\x85\x94R\x94h\x01\x8c\x05MyInt\x94\x93\x94K\x07\x85\x94\x81\x94h\x01\x8c\x05MyStr\x94\x93\x94\x8c\x04text\x94\x85\x94\x81\x94h\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('exception', 4): b'\x80\x04\x95;\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x07MyError\x94\x93\x94\x8c\x07message\x94K\x02\x86\x94R\x94h\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('exception', 5): b'\x80\x05\x95;\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x07MyError\x94\x93\x94\x8c\x07message\x94K\x02\x86\x94R\x94h\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('metaclass', 4): b'\x80\x04\x95D\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x08WithMeta\x94\x93\x94)\x81\x94}\x94\x8c\x05value\x94\x8c\x06meta-1\x94sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('metaclass', 5): b'\x80\x05\x95D\x00\x00\x00\x00\x00\x00\x00]\x94(\x8c\x08__main__\x94\x8c\x08WithMeta\x94\x93\x94)\x81\x94}\x94\x8c\x05value\x94\x8c\x06meta-1\x94sbh\x01\x8c\x05Empty\x94\x93\x94)\x81\x94e.',
    ('function_attr', 4): b'\x80\x04\x95N\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94(\x8c\x08function\x94h\x00\x8c\x0brecord_like\x94\x93\x94\x8c\x03cls\x94h\x00\x8c\x06Record\x94\x93\x94ub.',
    ('function_attr', 5): b'\x80\x05\x95N\x00\x00\x00\x00\x00\x00\x00\x8c\x08__main__\x94\x8c\x05Empty\x94\x93\x94)\x81\x94}\x94(\x8c\x08function\x94h\x00\x8c\x0brecord_like\x94\x93\x94\x8c\x03cls\x94h\x00\x8c\x06Record\x94\x93\x94ub.',
}


_MISSING = object()


def same(actual, expected, seen=None):
    if seen is None:
        seen = {}
    assert type(actual) is type(expected), (type(actual), type(expected))
    if id(expected) in seen:
        assert seen[id(expected)] is actual
        return
    if isinstance(expected, float):
        assert struct.pack(">d", actual) == struct.pack(">d", expected)
        return
    if isinstance(expected, (type, types.FunctionType)):
        assert actual is expected
        return
    if isinstance(expected, (str, bytes, int, type(None))) and \
            type(expected).__module__ == "builtins":
        assert actual == expected
        return
    seen[id(expected)] = actual
    if isinstance(expected, BaseException):
        same(actual.args, expected.args, seen)
    elif isinstance(expected, (list, tuple)):
        assert len(actual) == len(expected)
        for left, right in zip(actual, expected):
            same(left, right, seen)
    elif isinstance(expected, dict):
        assert list(actual) == list(expected)
        for key in expected:
            same(actual[key], expected[key], seen)
    elif isinstance(expected, (set, int, str)):
        assert actual == expected
    if type(expected).__module__ == "builtins":
        return
    left = getattr(actual, "__dict__", None)
    right = getattr(expected, "__dict__", None)
    same(left, right, seen)
    for name in copyreg._slotnames(type(expected)):
        left = object.__getattribute__(actual, name) \
            if _has_slot(actual, name) else _MISSING
        right = object.__getattribute__(expected, name) \
            if _has_slot(expected, name) else _MISSING
        if right is _MISSING:
            assert left is _MISSING, name
        else:
            same(left, right, seen)


def _has_slot(value, name):
    try:
        object.__getattribute__(value, name)
    except AttributeError:
        return False
    return True


def pickle_module_calls(function):
    """Run `function` and name the `pickle.py` functions it entered."""
    names = []

    def profiler(frame, event, arg):
        if event == "call":
            filename = frame.f_code.co_filename.replace("\\", "/")
            if filename.endswith("/pickle.py") or filename == "pickle.py":
                names.append(frame.f_code.co_name)

    sys.setprofile(profiler)
    try:
        result = function()
    finally:
        sys.setprofile(None)
    return result, names


def check_streams():
    for name in NATIVE_CASES + FALLBACK_CASES:
        for protocol in (4, 5):
            try:
                check_stream(name, protocol)
            except AssertionError as exc:
                raise AssertionError((name, protocol)) from exc


def check_stream(name, protocol):
    value = build(name)
    expected = EXPECTED[name, protocol]
    if name in NATIVE_CASES:
        blob, calls = pickle_module_calls(
            lambda: pickle.dumps(value, protocol=protocol))
        assert calls == [], calls[:5]
        # The second call finds `__slotnames__` already cached.
        assert pickle.dumps(value, protocol=protocol) == blob
    else:
        blob = pickle.dumps(value, protocol=protocol)
    assert describe(blob) == expected, blob[:200]
    if name in NATIVE_CASES:
        back, calls = pickle_module_calls(lambda: pickle.loads(blob))
        assert calls == [], calls[:5]
    else:
        back = pickle.loads(blob)
    if name == "registered":
        assert back[0].value == 42
    elif name == "getstate":
        assert vars(back[0]) == {"kept": 1}
    assert type(pickle.loads(bytearray(blob) + b"trailing")) is type(value)
    if name in LOSSY_CASES:
        return
    same(back, value)
    # The pure-Python engine must agree with the native streams.
    if name not in PYTHON_ENGINE_DIFFERS:
        stream = io.BytesIO()
        pickle._Pickler(stream, protocol).dump(value)
        assert describe(stream.getvalue()) == expected
        same(pickle._loads(blob), value)


def check_dict_property():
    # The engines disagree on a `__dict__` property (CPython's C pickler
    # reads the real instance dictionary); only the fallback matters here.
    value = DictProperty()
    del CALLS[:]
    back = pickle.loads(pickle.dumps([value, Empty()]))
    assert type(back[0]) is DictProperty and type(back[1]) is Empty


def check_slotnames_cache():
    class_names = [Point, Record, Empty, Mixed, Level3, Private]
    for cls in class_names:
        assert isinstance(cls.__dict__["__slotnames__"], list), cls
    assert Point.__slotnames__ == ["x", "y", "tag"]
    assert Record.__slotnames__ == []
    assert Mixed.__slotnames__ == ["x", "y", "tag"]
    assert Level3.__slotnames__ == ["two", "one"]
    assert Private.__slotnames__ == ["_Private__secret", "plain"]
    # The slot names are the strings in `__slots__`, as in copyreg.
    assert Point.__slotnames__[0] is Point.__slots__[0]

    # A dropped cache is rebuilt by the next call.
    value = Point(1, 2, 3)
    del Point.__slotnames__
    back = pickle.loads(pickle.dumps(value))
    assert (back.x, back.y, back.tag) == (1, 2, 3)
    assert Point.__dict__["__slotnames__"] == ["x", "y", "tag"]


def check_hooks_run():
    expected_calls = {
        "reduce": ["reduce"],
        "reduce_ex": ["reduce_ex"],
        "getstate": ["getstate"],
        "setstate": ["setstate"],
        "getnewargs": ["getnewargs"],
        "getnewargs_ex": ["getnewargs_ex"],
        "new": ["new"],
        "registered": ["dispatch_table"],
        "instance_reduce_ex": ["instance reduce_ex"],
        "slot_shadow": ["tag-setter"],
        "slots_setattr": ["setattr:slot"],
    }
    for name, calls in expected_calls.items():
        value = build(name)
        del CALLS[:]
        back = pickle.loads(pickle.dumps(value))
        assert CALLS == calls, (name, CALLS)
        assert type(back[1]) is Empty
    back = pickle.loads(pickle.dumps(build("setstate")))
    assert back[0].restored is True and back[0].value == 5

    # `__getattr__` sees the unpickler's `__setstate__` probe, and the
    # default `__getstate__` probes an unset slot through getattr.
    value = build("getattr")
    blob = pickle.dumps(value)
    del CALLS[:]
    pickle.loads(blob)
    assert "getattr:__setstate__" in CALLS, CALLS
    value = build("slots_getattr")
    del CALLS[:]
    blob = pickle.dumps(value)
    assert "getattr:unset_slot" in CALLS, CALLS
    del CALLS[:]
    back = pickle.loads(blob)
    assert "getattr:__setstate__" in CALLS, CALLS
    assert back[0].set_slot == 1

    value = build("getattribute")
    del CALLS[:]
    blob = pickle.dumps(value)
    assert "getattribute:__reduce_ex__" in CALLS, CALLS

    # Finalizers run once for each unpickled copy.
    blob = pickle.dumps(build("del"))
    del CALLS[:]
    back = pickle.loads(blob)
    assert CALLS == []
    del back
    import gc
    gc.collect()
    assert CALLS == ["del"], CALLS


def check_errors():
    for name in ERROR_CASES:
        for protocol in (4, 5):
            try:
                pickle.dumps(build(name), protocol=protocol)
            except (pickle.PicklingError, AttributeError):
                pass
            else:
                raise AssertionError(name)
    # A later rebinding of the module attribute is noticed on every call.
    global Empty
    value = [Empty()]
    assert pickle.dumps(value) == pickle.dumps(value)
    original = Empty
    Empty = None
    try:
        try:
            pickle.dumps(value)
        except pickle.PicklingError:
            pass
        else:
            raise AssertionError("rebound class must not pickle")
    finally:
        Empty = original
    assert type(pickle.loads(pickle.dumps(value))[0]) is Empty


def check_customized_picklers():
    value = [Record(1), Point(1, 2, None)]

    class Persistent(pickle.Pickler):
        def persistent_id(self, obj):
            return "point" if type(obj) is Point else None

    class PersistentLoad(pickle.Unpickler):
        def persistent_load(self, pid):
            return pid

    stream = io.BytesIO()
    Persistent(stream, 5).dump(value)
    back = PersistentLoad(io.BytesIO(stream.getvalue())).load()
    assert back[1] == "point" and back[0].points == ["point"] * 3

    class Override(pickle.Pickler):
        def reducer_override(self, obj):
            if type(obj) is Point:
                return Empty, ()
            return NotImplemented

    stream = io.BytesIO()
    Override(stream, 5).dump(value)
    back = pickle.loads(stream.getvalue())
    assert type(back[1]) is Empty and type(back[0]) is Record

    stream = io.BytesIO()
    pickler = pickle.Pickler(stream, 5)
    pickler.dispatch_table = {Point: lambda obj: (Empty, ())}
    pickler.dump(value)
    back = pickle.loads(stream.getvalue())
    assert type(back[1]) is Empty and type(back[0]) is Record

    class Finder(pickle.Unpickler):
        def find_class(self, module, name):
            if name == "Point":
                return Empty
            return super().find_class(module, name)

    back = Finder(io.BytesIO(pickle.dumps(Record(1)))).load()
    assert type(back) is Record

    # A dispatch-table registration made after earlier native calls.
    assert type(pickle.loads(pickle.dumps(Empty()))) is Empty
    copyreg.pickle(Empty, lambda obj: (Record, (9,)))
    try:
        back = pickle.loads(pickle.dumps([Empty()]))
        assert type(back[0]) is Record and back[0].ident == 9
    finally:
        del copyreg.dispatch_table[Empty]
    assert type(pickle.loads(pickle.dumps(Empty()))) is Empty


def check_loads_edge_cases():
    # Attribute names are interned, as in `load_build`.
    back = pickle.loads(pickle.dumps(Record(1)))
    for key in vars(back):
        assert key is sys.intern(key)
    assert sorted(vars(back))[0] is sorted(vars(Record(2)))[0]

    # State forms that CPython's pickler doesn't emit for these classes.
    proto = b"\x80\x04"
    point = b"\x8c\x08__main__\x8c\x05Point\x93)\x81"
    empty = b"\x8c\x08__main__\x8c\x05Empty\x93)\x81"
    back = pickle.loads(proto + empty + b"}b.")
    assert type(back) is Empty and vars(back) == {}
    back = pickle.loads(proto + empty + b"}\x8c\x01aK\x01sb}\x8c\x01bK\x02sb.")
    assert vars(back) == {"a": 1, "b": 2}
    back = pickle.loads(proto + empty + b"N}\x86b.")
    assert vars(back) == {}
    back = pickle.loads(proto + empty + b"Nb.")
    assert vars(back) == {}
    back = pickle.loads(proto + point + b"N}\x8c\x01xK\x01s\x86b.")
    assert back.x == 1 and not _has_slot(back, "y")
    # A slots-only class has no `__dict__` for dictionary state.
    for state in (b"}\x8c\x01xK\x01sb.", b"}\x8c\x01qK\x01sN\x86b."):
        try:
            pickle.loads(proto + point + state)
        except AttributeError:
            pass
        else:
            raise AssertionError("Point has no __dict__")
    # An unknown slot name is an AttributeError from setattr.
    try:
        pickle.loads(proto + point + b"N}\x8c\x01qK\x01s\x86b.")
    except AttributeError:
        pass
    else:
        raise AssertionError("Point has no attribute q")
    # Slot state on a dict class lands in `__dict__` through setattr.
    back = pickle.loads(proto + empty + b"N}\x8c\x01qK\x01s\x86b.")
    assert vars(back) == {"q": 1}
    # Non-string and unhashable state keys keep the full unpickler.
    back = pickle.loads(proto + empty + b"}K\x05K\x01sb.")
    assert vars(back) == {5: 1}
    # NEWOBJ with arguments, and globals that aren't plain classes.
    try:
        pickle.loads(proto + empty[:-2] + b"K\x01\x85\x81.")
    except TypeError:
        pass
    else:
        raise AssertionError("object.__new__ takes no arguments")
    assert pickle.loads(proto + b"\x8c\x08__main__\x8c\x05Empty\x93.") is Empty
    assert pickle.loads(
        proto + b"\x8c\x08__main__\x8c\x0brecord_like\x93K\x02\x85R.").ident == 2
    assert pickle.loads(
        proto + b"\x8c\x08builtins\x8c\x04list\x93)\x81.") == []
    back = pickle.loads(proto + b"\x8c\x08__main__\x8c\x0bOuter.Inner\x93)\x81.")
    assert type(back) is Outer.Inner
    for bad in (b"\x8c\x08__main__\x8c\x07Missing\x93)\x81.",
                b"\x8c\x08__main__\x8c\x0dOuter.Missing\x93)\x81."):
        try:
            pickle.loads(proto + bad)
        except AttributeError:
            pass
        else:
            raise AssertionError(bad)
    try:
        pickle.loads(proto + b"\x8c\x08__main__K\x01\x93.")
    except pickle.UnpicklingError:
        pass
    else:
        raise AssertionError("STACK_GLOBAL requires str")
    # An instance used as a dictionary key or a set-like member.
    back = pickle.loads(proto + b"}" + empty + b"K\x01s.")
    assert [type(key) for key in back] == [Empty]


def check_imports():
    # An unimported module is imported by the full unpickler.
    blob = (b"\x80\x04\x95+\x00\x00\x00\x00\x00\x00\x00\x8c\x08argparse"
            b"\x94\x8c\tNamespace\x94\x93\x94)\x81\x94}\x94\x8c\x05value"
            b"\x94K\x01sb.")
    was_imported = "argparse" in sys.modules
    back = pickle.loads(blob)
    assert "argparse" in sys.modules
    assert type(back).__name__ == "Namespace" and back.value == 1
    if not was_imported:
        assert type(pickle.loads(blob)) is type(back)
    assert pickle.dumps(back, 4) == blob

    module = types.ModuleType("pickle_native_fake_module")

    class Portable:
        pass

    Portable.__module__ = module.__name__
    Portable.__qualname__ = "Portable"
    module.Portable = Portable
    sys.modules[module.__name__] = module
    try:
        value = Portable()
        value.payload = [1, 2]
        blob = pickle.dumps(value)
        back = pickle.loads(blob)
        assert type(back) is Portable and back.payload == [1, 2]
    finally:
        del sys.modules[module.__name__]
    try:
        pickle.loads(blob)
    except ImportError:
        pass
    else:
        raise AssertionError("the module is no longer importable")
    try:
        pickle.dumps(value)
    except pickle.PicklingError:
        pass
    else:
        raise AssertionError("the module is no longer importable")


def check_audit_hook():
    events = []

    def hook(event, args):
        if event == "pickle.find_class":
            events.append(args)

    blob = pickle.dumps([Empty(), Point(1, 2, 3)])
    sys.addaudithook(hook)
    back = pickle.loads(blob)
    assert type(back[0]) is Empty
    assert events == [("__main__", "Empty"), ("__main__", "Point")], events


def main():
    check_streams()
    check_dict_property()
    check_slotnames_cache()
    check_hooks_run()
    check_errors()
    check_customized_picklers()
    check_loads_edge_cases()
    check_imports()
    check_audit_hook()
    print("pickle native instances: ok")


if __name__ == "__main__":
    if "--generate" in sys.argv:
        generate()
    else:
        main()
