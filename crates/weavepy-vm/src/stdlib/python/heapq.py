"""Min-heap algorithms (`heapq`).

A pure-Python heap. Implements the core CPython API: `heappush`,
`heappop`, `heappushpop`, `heapreplace`, `heapify`, `nlargest`,
`nsmallest`, `merge`.
"""


def heappush(heap, item):
    """Push `item` onto `heap`, maintaining the heap invariant."""
    heap.append(item)
    _siftdown(heap, 0, len(heap) - 1)


def heappop(heap):
    """Pop the smallest item from `heap`, maintaining the invariant."""
    last = heap.pop()
    if heap:
        ret = heap[0]
        heap[0] = last
        _siftup(heap, 0)
        return ret
    return last


def heappushpop(heap, item):
    """Push then pop in one operation — faster than separate calls."""
    if heap and heap[0] < item:
        item, heap[0] = heap[0], item
        _siftup(heap, 0)
    return item


def heapreplace(heap, item):
    """Pop and return the smallest, then push `item`."""
    ret = heap[0]
    heap[0] = item
    _siftup(heap, 0)
    return ret


def heapify(x):
    """In-place transform `x` into a heap."""
    n = len(x)
    for i in reversed(range(n // 2)):
        _siftup(x, i)


def nlargest(n, iterable, key=None):
    """Return the `n` largest items from `iterable`."""
    items = list(iterable)
    if key is not None:
        items.sort(key=key, reverse=True)
    else:
        items.sort(reverse=True)
    return items[:n]


def nsmallest(n, iterable, key=None):
    """Return the `n` smallest items from `iterable`."""
    items = list(iterable)
    if key is not None:
        items.sort(key=key)
    else:
        items.sort()
    return items[:n]


def merge(*iterables, key=None, reverse=False):
    """Merge sorted iterables into a single sorted iterator."""
    its = [iter(it) for it in iterables]
    heap = []
    for idx, it in enumerate(its):
        try:
            v = next(it)
        except StopIteration:
            continue
        k = key(v) if key is not None else v
        heap.append((k, idx, v))
    if reverse:
        heap.sort(reverse=True)
    else:
        heap.sort()
    while heap:
        k, idx, v = heap.pop(0)
        yield v
        try:
            v = next(its[idx])
        except StopIteration:
            continue
        k = key(v) if key is not None else v
        new = (k, idx, v)
        # Insert in sorted order.
        lo, hi = 0, len(heap)
        while lo < hi:
            mid = (lo + hi) // 2
            if reverse:
                if heap[mid] < new:
                    hi = mid
                else:
                    lo = mid + 1
            else:
                if heap[mid] < new:
                    lo = mid + 1
                else:
                    hi = mid
        heap.insert(lo, new)


# ---- internal -----------------------------------------------------


def _siftdown(heap, start, pos):
    item = heap[pos]
    while pos > start:
        parent_pos = (pos - 1) >> 1
        parent = heap[parent_pos]
        if item < parent:
            heap[pos] = parent
            pos = parent_pos
        else:
            break
    heap[pos] = item


def _siftup(heap, pos):
    end = len(heap)
    start = pos
    item = heap[pos]
    child = 2 * pos + 1
    while child < end:
        right = child + 1
        if right < end and not heap[child] < heap[right]:
            child = right
        heap[pos] = heap[child]
        pos = child
        child = 2 * pos + 1
    heap[pos] = item
    _siftdown(heap, start, pos)


__all__ = [
    "heappush",
    "heappop",
    "heappushpop",
    "heapreplace",
    "heapify",
    "nlargest",
    "nsmallest",
    "merge",
]
