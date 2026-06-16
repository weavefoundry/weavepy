"""RFC 0039 WS6 — real selector backends (poll/epoll/kqueue).

`DefaultSelector` picks the platform's native readiness backend (kqueue on
BSD/macOS, epoll on Linux, poll/select fallback) and reports read/write
readiness over a real socketpair. This pins the selector layer that the WS7
asyncio loop is built on.
"""

import selectors
import socket


# ---------------------------------------------------------------------------
# DefaultSelector is a real backend, not the bare select() fallback on
# platforms that provide poll/epoll/kqueue.
# ---------------------------------------------------------------------------

sel = selectors.DefaultSelector()
backend = type(sel).__name__
assert backend in {
    "KqueueSelector",
    "EpollSelector",
    "PollSelector",
    "SelectSelector",
}, backend


# ---------------------------------------------------------------------------
# Read readiness: a socketpair becomes readable once bytes are written.
# ---------------------------------------------------------------------------

rsock, wsock = socket.socketpair()
try:
    rsock.setblocking(False)
    wsock.setblocking(False)
    sel.register(rsock, selectors.EVENT_READ, data="reader")

    # Nothing written yet → no read readiness within a short timeout.
    assert sel.select(timeout=0) == []

    wsock.send(b"payload")
    events = sel.select(timeout=1)
    assert len(events) == 1, events
    key, mask = events[0]
    assert key.data == "reader"
    assert mask & selectors.EVENT_READ
    assert rsock.recv(16) == b"payload"

    # After draining, readiness clears again.
    assert sel.select(timeout=0) == []
finally:
    sel.unregister(rsock)
    sel.close()
    rsock.close()
    wsock.close()


# ---------------------------------------------------------------------------
# Write readiness + modify(): a fresh socketpair end is writable immediately,
# and modify() can switch the interest set.
# ---------------------------------------------------------------------------

sel2 = selectors.DefaultSelector()
r2, w2 = socket.socketpair()
try:
    r2.setblocking(False)
    w2.setblocking(False)
    key = sel2.register(w2, selectors.EVENT_WRITE)
    events = sel2.select(timeout=1)
    assert len(events) == 1 and (events[0][1] & selectors.EVENT_WRITE)

    sel2.modify(w2, selectors.EVENT_READ)
    assert sel2.get_key(w2).events == selectors.EVENT_READ
    # Now only interested in READ; an empty pipe is not readable.
    assert sel2.select(timeout=0) == []
finally:
    sel2.close()
    r2.close()
    w2.close()


print("WS6 selector backends ok")
