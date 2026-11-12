"""`%(key)s` conversions keep CPython's results: a plain dict is probed by
the key's text, a missing key raises KeyError naming it, a `str` subclass
key still matches, a mapping instance goes through `__getitem__`, and
bytes formatting keys the mapping by bytes."""
import collections


class Record:
    def __init__(self, i):
        self.asctime = "t%d" % i
        self.levelname = "WARNING"
        self.name = "bench"
        self.message = "m%d" % i


fmt = "%(asctime)s %(levelname)s %(name)s: %(message)s"
for i in range(3000):
    assert fmt % Record(i).__dict__ == "t%d WARNING bench: m%d" % (i, i)

assert "%(a)s-%(bb)r" % {"a": 1, "bb": "x"} == "1-'x'"
assert "%(a)5d|%(a)-5d|" % {"a": 42} == "   42|42   |"
try:
    "%(missing)s" % {"a": 1}
except KeyError as e:
    assert e.args == ("missing",), e.args
else:
    raise AssertionError("no KeyError")


class S(str):
    pass


assert "%(a)s" % {S("a"): 5} == "5"
assert "%(a)s" % collections.OrderedDict(a=3) == "3"
assert "%(k)s" % collections.defaultdict(lambda: "dflt") == "dflt"


class Upper(dict):
    def __getitem__(self, key):
        return key.upper()


assert "%(abc)s" % Upper() == "ABC"
assert b"%(k)s" % {b"k": b"v"} == b"v"
assert "%(é)s" % {"é": "accent"} == "accent"
print("ok")
