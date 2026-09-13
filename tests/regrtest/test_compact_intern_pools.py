"""Exercise canonical-string identities across growth and serialization."""
import gc
import marshal
import pickle
import sys
import threading


def fresh(text):
    return text.encode("utf-8").decode("utf-8")


def check_canonical_strings():
    for suffix in ("ascii-name", "caf\u00e9-name", "\u6f22\u5b57-name", "\U0001f9f5-name", "nul\0name"):
        first = fresh("compact pool identity / " + suffix)
        second = fresh(first)
        assert first == second and first is not second
        assert sys.intern(first) is first
        assert sys.intern(second) is first
        # Equal text alone must not mark the noncanonical object as interned.
        for version in (3, 4, 5):
            pooled_bytes = marshal.dumps(first, version)
            plain_bytes = marshal.dumps(second, version)
            assert pooled_bytes[0] & 127 in (ord("t"), ord("A"), ord("Z"))
            assert plain_bytes[0] & 127 in (ord("u"), ord("a"), ord("z"))
            assert marshal.loads(pooled_bytes) is first
            plain_roundtrip = marshal.loads(plain_bytes)
            assert plain_roundtrip == first and plain_roundtrip is not first
        pair = [first, first, second, second]
        for version in (3, 4, 5):
            restored = marshal.loads(marshal.dumps(pair, version))
            assert restored == pair
            assert restored[0] is first and restored[1] is first
            assert restored[2] is restored[3] and restored[2] is not first


def check_growth_and_lifetime():
    keep = []
    for i in range(4096):
        value = fresh("compact pool growth / %d / %s" % (i, "x" * (i % 80)))
        assert sys.intern(value) is value
        keep.append(value)
    gc.collect()
    for value in reversed(keep):
        assert sys.intern(fresh(value)) is value
    for value in ("",) + tuple(chr(i) for i in range(256)):
        canonical = sys.intern(value)
        assert canonical == value
        assert sys.intern(fresh(value)) is canonical
    for i in range(32):
        cls = type("CompactPoolType%d" % i, (), {})
        name = cls.__name__
        gc.collect()
        assert cls.__name__ is name
        assert cls.__qualname__ == name


class Record:
    pass


def check_attribute_names():
    record = Record()
    for i in range(32):
        setattr(record, fresh("compact_attr_%d" % i), i)
    keys = sorted(record.__dict__)
    assert all(sys.intern(key) is key for key in keys)
    for protocol in range(pickle.HIGHEST_PROTOCOL + 1):
        restored = pickle.loads(pickle.dumps(record, protocol))
        assert restored.__dict__ == record.__dict__
        assert all(a is b for a, b in zip(keys, sorted(restored.__dict__)))


def check_thread_transfer():
    results = []

    def worker(i):
        value = fresh("compact worker / %d / unique" % i)
        canonical = sys.intern(value)
        results.append((canonical, sys.intern(fresh(value)) is canonical))

    for i in range(4):
        thread = threading.Thread(target=worker, args=(i,))
        thread.start()
        thread.join()
    assert len(results) == 4 and all(ok for _, ok in results)
    gc.collect()
    for value, _ in results:
        canonical = sys.intern(value)
        assert canonical == value and sys.intern(fresh(value)) is canonical


def check_rejections():
    class StrSubclass(str):
        pass

    for value in (None, 1, b"bytes", StrSubclass("subclass")):
        try:
            sys.intern(value)
        except TypeError:
            pass
        else:
            raise AssertionError(type(value))


check_canonical_strings()
check_growth_and_lifetime()
check_attribute_names()
check_thread_transfer()
check_rejections()
print("compact intern pools: all checks passed")
