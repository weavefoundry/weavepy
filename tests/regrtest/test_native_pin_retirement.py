"""Native side exits release obsolete pins before the continuation runs."""

import gc
import sys
import weakref


def count_temporaries():
    return sum(
        1
        for value in gc.get_objects()
        if type(value) is list and len(value) == 7 and value[0] == 123456789
    )


def churn(n):
    total = 0
    for i in range(n):
        values = [123456789, 2, 3, 4, 5, 6, 7]
        total += len(values)
    # The ASCII-only native slice helper exits here. The continuation
    # must see only the last list, which is still owned by this frame.
    text = "é"[:n:None]
    return total, len(text), count_temporaries()


was_enabled = gc.isenabled()
gc.disable()
try:
    for _ in range(10):
        assert churn(5) == (35, 1, 1)
    assert churn(400) == (2800, 1, 1)
finally:
    if was_enabled:
        gc.enable()
# Keep the entry argument rooted explicitly so only detached runtime
# temporaries are expected to die during the continuation.
events = []
references = []
frame_ids = []
frame_names = []
offset = 0


class Node:
    def __init__(self, tag, next_node):
        self.tag = tag
        self.next = next_node

    def __del__(self):
        global offset
        offset = 100
        events.append(self.tag)
        frame = sys._getframe(1)
        frame_ids.append(id(frame))
        frame_names.append(frame.f_code.co_name)


def make_chain(n):
    head = Node(n, None)
    references.append(weakref.ref(head))
    for i in range(n):
        head = Node(n - i - 1, head)
        references.append(weakref.ref(head))
    return head


def count_events():
    return len(events)


def consume_chain(head, n):
    total = 0
    for i in range(n):
        prev = head
        head = head.next
        prev.next = None
        total += head.tag
    text = "é"[:n:None]
    observed = count_events()
    for j in range(n):
        total += offset
    return total, len(text), observed, head, id(sys._getframe())


gc.disable()
try:
    for _ in range(10):
        events.clear()
        references.clear()
        frame_ids.clear()
        frame_names.clear()
        offset = 0
        root = make_chain(5)
        result = consume_chain(root, 5)
        assert result[:3] == (515, 1, 3), result[:3]
        assert events[:3] == [1, 2, 3], events
        assert frame_ids[:3] == [result[4]] * 3, frame_ids
        assert frame_names[:3] == ["consume_chain"] * 3, frame_names
        assert all(ref() is None for ref in references[2:5])
        assert references[0]() is result[3]
        assert references[-1]() is root
        del result, root
finally:
    if was_enabled:
        gc.enable()
print("ok")
