"""Value ownership across containers, buffers, serialization, and threads."""

import gc
import json
import marshal
import pickle
import sys
import threading
import weakref


def test_unicode_keys_and_interned_identity():
    for value in ["", "a", "x\0y", "mañana", "日本語", "🧶🙂", "e\u0301", "\ud800x\udfff"]:
        rebuilt = (value + "!")[:-1]
        assert rebuilt == value
        assert list(rebuilt) == list(value)
        assert len(rebuilt) == len(value)
        assert {value: 17}[rebuilt] == 17
        assert hash(value) == hash(rebuilt)
        assert marshal.loads(marshal.dumps(value)) == value
        assert pickle.loads(pickle.dumps(value)) == value
        assert json.loads(json.dumps(value)) == value
    first = sys.intern("thin_value_" + str(71231))
    second = sys.intern("thin_" + "value_71231")
    assert first is second


def test_immutable_buffers_keep_their_owner():
    for size in [0, 1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 257]:
        original = bytes(i % 256 for i in range(size))
        view = memoryview(original)
        sliced = view[::2]
        expected = original[::2]
        del original
        gc.collect()
        assert view.readonly
        assert sliced.tobytes() == expected
        assert bytes(sliced) == expected
        assert hash(sliced) == hash(expected)
        view.release()
        assert sliced.tobytes() == expected
        sliced.release()


def test_tuple_recycling_does_not_reuse_stale_hashes():
    for size in range(18):
        for start in range(80):
            current = tuple(range(start, start + size))
            expected = tuple([x for x in range(start, start + size)])
            assert current == expected
            assert hash(current) == hash(expected)
            assert {current: start}[expected] == start


def test_tuple_hash_retries_failure_then_caches_success():
    class ChangingHash:
        def __init__(self):
            self.calls = 0
            self.fail = True

        def __hash__(self):
            self.calls += 1
            if self.fail:
                raise ValueError("element hash")
            return self.calls

    item = ChangingHash()
    value = (item,)
    for _ in range(2):
        try:
            hash(value)
        except ValueError as error:
            assert str(error) == "element hash"
        else:
            raise AssertionError("tuple swallowed element hash failure")
    assert item.calls == 2
    item.fail = False
    first = hash(value)
    item.fail = True
    assert hash(value) == first
    assert item.calls == 3


def test_container_aliases_and_element_lifetime():
    class Item:
        pass

    item = Item()
    ref = weakref.ref(item)
    text = "retained " + "🧶" * 7
    data = bytes(range(128))
    value = (item, text, data)
    owners = [value] * 50
    del item, value
    gc.collect()
    assert ref() is owners[0][0]
    assert all(owner is owners[0] for owner in owners)
    assert owners[-1][1] is text
    assert owners[-1][2] is data
    del owners
    gc.collect()
    assert ref() is None


def test_serialized_aliases_and_dictionary_keys():
    text = "payload-" + "x" * 127
    data = bytes(range(256))
    key = (text, data, 23)
    values = [key, key, {key: (text, data)}, "\ud800"]
    for protocol in [3, 4, 5]:
        restored = pickle.loads(pickle.dumps(values, protocol=protocol))
        assert restored == values
        assert restored[0] is restored[1]
        assert restored[2][restored[0]] == (text, data)
    assert marshal.loads(marshal.dumps(values)) == values


def test_thread_handoffs_preserve_value_identity():
    text = "thread-shared-" + "🧶" * 17
    data = bytes(range(256))
    value = (text, data, tuple(range(23)))
    outputs = []

    def read(shared):
        for _ in range(100):
            assert shared[0] is text
            assert shared[1] is data
            assert hash(shared) == hash(value)
        outputs.append(shared)

    workers = [threading.Thread(target=read, args=(value,)) for _ in range(2)]
    for worker in workers:
        worker.start()
    for worker in workers:
        worker.join()
    assert len(outputs) == 2
    assert outputs[0] is value and outputs[1] is value


if __name__ == "__main__":
    tests = [value for name, value in list(globals().items()) if name.startswith("test_")]
    for test in tests:
        test()
    print("shared value storage:", len(tests), "passed")
