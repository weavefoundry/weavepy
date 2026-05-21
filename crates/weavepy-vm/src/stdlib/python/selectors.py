"""WeavePy `selectors` — selector abstraction on top of `select`.

A `SelectorKey` is a named tuple-shaped object capturing a single
registration. A `BaseSelector` exposes `register`, `unregister`,
`modify`, `select(timeout)`, and `close`. We ship `DefaultSelector`
(which is a `SelectSelector` for portability — `mio` underneath
picks the most efficient backend per platform).

The asyncio event loop drives this surface for its I/O multiplexing.
"""

import select


EVENT_READ = 1
EVENT_WRITE = 2


class SelectorKey:
    """A registered file descriptor + its current interest mask + opaque user data."""

    __slots__ = ("fileobj", "fd", "events", "data")

    def __init__(self, fileobj, fd, events, data):
        self.fileobj = fileobj
        self.fd = fd
        self.events = events
        self.data = data

    def __repr__(self):
        return "SelectorKey(fileobj={!r}, fd={}, events={}, data={!r})".format(
            self.fileobj, self.fd, self.events, self.data
        )


def _fileobj_to_fd(fileobj):
    if isinstance(fileobj, int):
        return fileobj
    if hasattr(fileobj, "fileno"):
        return fileobj.fileno()
    raise ValueError("Invalid file object: {!r}".format(fileobj))


class BaseSelector:
    """Abstract base class for selector implementations."""

    def __init__(self):
        self._fd_to_key = {}

    def register(self, fileobj, events, data=None):
        fd = _fileobj_to_fd(fileobj)
        if fd in self._fd_to_key:
            raise KeyError("{!r} is already registered".format(fileobj))
        key = SelectorKey(fileobj, fd, events, data)
        self._fd_to_key[fd] = key
        return key

    def unregister(self, fileobj):
        fd = _fileobj_to_fd(fileobj)
        key = self._fd_to_key.pop(fd, None)
        if key is None:
            raise KeyError("{!r} is not registered".format(fileobj))
        return key

    def modify(self, fileobj, events, data=None):
        fd = _fileobj_to_fd(fileobj)
        key = self._fd_to_key.get(fd)
        if key is None:
            raise KeyError("{!r} is not registered".format(fileobj))
        if events != key.events:
            self.unregister(fileobj)
            key = self.register(fileobj, events, data)
        elif data != key.data:
            key.data = data
        return key

    def select(self, timeout=None):
        raise NotImplementedError

    def close(self):
        self._fd_to_key.clear()

    def get_key(self, fileobj):
        fd = _fileobj_to_fd(fileobj)
        key = self._fd_to_key.get(fd)
        if key is None:
            raise KeyError("{!r} is not registered".format(fileobj))
        return key

    def get_map(self):
        return dict(self._fd_to_key)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False


class SelectSelector(BaseSelector):
    """Default selector backed by `select.select`."""

    def select(self, timeout=None):
        rlist = [k.fd for k in self._fd_to_key.values() if k.events & EVENT_READ]
        wlist = [k.fd for k in self._fd_to_key.values() if k.events & EVENT_WRITE]
        if not rlist and not wlist:
            return []
        r_ready, w_ready, _ = select.select(rlist, wlist, [], timeout)
        ready = []
        seen = set()
        for fd in r_ready:
            key = self._fd_to_key.get(fd)
            if key is not None:
                ready.append((key, EVENT_READ))
                seen.add(fd)
        for fd in w_ready:
            key = self._fd_to_key.get(fd)
            if key is not None:
                events = EVENT_WRITE
                if fd in seen:
                    # already reported as read-ready — merge interest.
                    for i, (existing_key, existing_events) in enumerate(ready):
                        if existing_key.fd == fd:
                            ready[i] = (existing_key, existing_events | EVENT_WRITE)
                            break
                else:
                    ready.append((key, events))
        return ready


DefaultSelector = SelectSelector


__all__ = [
    "EVENT_READ", "EVENT_WRITE", "SelectorKey", "BaseSelector",
    "SelectSelector", "DefaultSelector",
]
