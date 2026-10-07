"""Check split-list lifetimes after parked generators are materialized."""
import gc
import sys
import weakref


def count_parts():
    return sum(1 for obj in gc.get_objects()
               if type(obj) is list and len(obj) == 2
               and type(obj[0]) is str and obj[0] == 'parked-pin-probe')


def generated(n):
    # The inner loop does not yield. A generator whose every loop a
    # resume reaches yields is handed to the interpreted resume on purpose
    # (the native resume protocol costs more than the one iteration it
    # would run), so a yield-dense body would never reach the parking path
    # this fixture probes. What it yields is unchanged.
    for i in range(n):
        j = 0
        while j < 1:
            j = j + 1
        parts = 'parked-pin-probe alpha'.split()
        yield len(parts) + i


gc.collect()
was_enabled = gc.isenabled()
gc.disable()
try:
    for mode in ['observe', 'close', 'throw', 'drop', 'exhaust']:
        it = generated(100)
        # (`send` resumes through the activation that parks natively;
        # `next` may step a simple body in place instead.)
        assert next(it) == 2
        for i in range(1, 40):
            assert it.send(None) == i + 2
        if mode == 'observe':
            frame = it.gi_frame
            assert frame.f_locals['parts'] == ['parked-pin-probe', 'alpha']
            assert next(it) == 42
            it.close()
            assert frame.f_locals['parts'] == ['parked-pin-probe', 'alpha']
            assert count_parts() == 1
            del frame
        elif mode == 'close':
            it.close()
        elif mode == 'throw':
            try:
                it.throw(ValueError('stop'))
            except ValueError as exc:
                assert str(exc) == 'stop'
            else:
                raise AssertionError('throw did not raise')
        elif mode == 'exhaust':
            assert list(it) == list(range(42, 102))
        del it
        assert count_parts() == 0, mode

    # An escaped frame keeps its locals alive until its last alias disappears.
    events = []


    class Marker:
        def __del__(self):
            events.append("released")


    def capture():
        marker = Marker()
        return sys._getframe(), weakref.ref(marker)


    frame, ref = capture()
    alias = frame
    del frame
    assert ref() is not None
    assert events == []
    del alias
    assert ref() is None
    assert events == ["released"]
finally:
    if was_enabled:
        gc.enable()
print("ok")
