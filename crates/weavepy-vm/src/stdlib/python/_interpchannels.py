"""CPython 3.13's `_interpchannels` — cross-interpreter channels.

CPython implements this as the C extension
`Modules/_interpchannelsmodule.c`. WeavePy's channel registry lives in
the native `_xxsubinterpreters` module (`interpreters_mod.rs`,
RFC 0031) — process-global, addressable from any interpreter — with
the full 3.13 semantics: per-end interpreter association, release,
closing-vs-closed states, blocking sends, and unbound items whose
sending interpreter was destroyed. This frozen shim adapts the calling
conventions (ChannelID objects, typed exceptions, keyword surfaces)
onto those primitives.

Graded consumers: test__interpchannels, and
`test.support.interpreters.channels` (test_interpreters.test_channels,
test_types SubinterpreterTests).
"""

import _xxsubinterpreters as _channels_backend

__all__ = [
    'ChannelError', 'ChannelNotFoundError', 'ChannelClosedError',
    'ChannelEmptyError', 'ChannelNotEmptyError', 'ChannelID',
    'create', 'destroy', 'list_all', 'list_interpreters',
    'send', 'send_buffer', 'recv', 'close', 'release',
    'get_info', 'get_count', 'get_channel_defaults', '_channel_id',
    '_register_end_types',
]


# Like CPython (`ADD_EXCTYPE(ChannelError, PyExc_RuntimeError, …)`),
# the hierarchy roots in RuntimeError — test__interpchannels asserts
# `assertRaisesRegex(RuntimeError, 'channel .* is closed')`.
class ChannelError(RuntimeError):
    pass


class ChannelNotFoundError(ChannelError):
    pass


class ChannelClosedError(ChannelError):
    pass


class ChannelEmptyError(ChannelError):
    pass


class ChannelNotEmptyError(ChannelError):
    pass


def _map_error(exc):
    """Retype a backend error into the `_interpchannels` hierarchy."""
    text = str(exc)
    if 'does not exist' in text:
        return ChannelNotFoundError(text)
    if 'may not be closed' in text:
        return ChannelNotEmptyError(text)
    if 'is closed' in text or 'channel closed' in text:
        return ChannelClosedError(text)
    if 'is empty' in text:
        return ChannelEmptyError(text)
    return ChannelError(text)


class ChannelID:
    """A channel identifier, optionally bound to one end.

    Mirrors CPython's `channelid` type: int-like (`__index__`,
    equality with ints and integral floats), hashable, shareable
    across interpreters, with `.end` reporting which end it is bound
    to ('send' / 'recv' / 'both').
    """

    __slots__ = ('_cid', '_end', '_owned')

    # The native XID registry marker: instances cross interpreters
    # (via channels or `set___main___attrs`) like CPython's channelid.
    _weave_xid_shareable = True

    def __new__(cls, cid, end='both'):
        self = object.__new__(cls)
        self._cid = cid
        self._end = end
        # Each live ChannelID object holds a reference on its channel;
        # the last one deallocated destroys it (CPython's
        # `_channels_drop_id_object` — test_interpreters test_channels
        # TestChannels.test_list_all relies on unreferenced channels
        # vanishing).
        try:
            _channels_backend.channel_incref(cid)
            self._owned = True
        except Exception:
            self._owned = False
        return self

    def __del__(self):
        if getattr(self, '_owned', False):
            self._owned = False
            try:
                _channels_backend.channel_decref(self._cid)
            except Exception:
                pass

    @property
    def end(self):
        return self._end

    @property
    def send(self):
        return ChannelID(self._cid, 'send')

    @property
    def recv(self):
        return ChannelID(self._cid, 'recv')

    def __repr__(self):
        end = f', {self._end}=True' if self._end in ('send', 'recv') else ''
        return f'ChannelID({self._cid}{end})'

    def __str__(self):
        return str(self._cid)

    def __int__(self):
        return self._cid

    def __index__(self):
        return self._cid

    def __hash__(self):
        return hash(self._cid)

    def __eq__(self, other):
        if isinstance(other, ChannelID):
            return self._cid == other._cid
        if isinstance(other, float):
            return other.is_integer() and self._cid == int(other)
        if isinstance(other, int):
            return self._cid == other
        return NotImplemented


def _coerce_cid(cid):
    import operator

    try:
        cid = operator.index(cid)
    except TypeError:
        raise TypeError(
            f'channel ID must be an int, got {type(cid).__name__}'
        ) from None
    if cid < 0:
        raise ValueError(f'channel ID must be a non-negative int, got {cid}')
    if cid >= 2 ** 64:
        raise OverflowError(f'channel ID too large: {cid}')
    return cid


def _channel_id(cid, *, send=None, recv=None, force=False, _resolve=False):
    cid = _coerce_cid(cid)
    if send is None and recv is None:
        end = 'both'
    elif send and recv:
        end = 'both'
    elif send:
        end = 'send'
    elif recv:
        end = 'recv'
    else:
        raise ValueError("'send' and 'recv' cannot both be False")
    if not force:
        # Unforced IDs must name an existing channel
        # (test__interpchannels ChannelIDTests.test_does_not_exist).
        get_channel_defaults(cid)
    return ChannelID(cid, end)


class ChannelInfo:
    """The subset of CPython's `ChannelInfo` struct sequence the
    stdlib wrapper reads (`closed`, `closing`), plus the queued-item
    count backing `ChannelNotEmptyError` checks."""

    __slots__ = ('closed', 'closing', 'count')

    def __init__(self, closed, closing, count):
        self.closed = closed
        self.closing = closing
        self.count = count

    def __repr__(self):
        return (f'ChannelInfo(closed={self.closed}, '
                f'closing={self.closing}, count={self.count})')


def create(unboundop):
    try:
        cid = _channels_backend.channel_create(unboundop)
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None
    return ChannelID(cid)


def destroy(cid):
    try:
        _channels_backend.channel_destroy(int(cid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def list_all():
    try:
        cids = _channels_backend.channel_list_all()
        return [(ChannelID(cid),
                 (_channels_backend.channel_get_defaults(cid),))
                for cid in cids]
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def list_interpreters(cid, *, send):
    try:
        return _channels_backend.channel_list_interpreters(
            int(cid), bool(send))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


_NO_DEFAULT = object()


def send(cid, obj, unboundop=None, *, timeout=None, blocking=True):
    if timeout is not None:
        if timeout < 0:
            raise ValueError('timeout value must be non-negative')
        if not blocking:
            raise ValueError('timeout is not supported for non-blocking sends')
    try:
        _channels_backend.channel_send(
            int(cid), obj, unboundop, blocking, timeout)
    except (RuntimeError, ValueError) as exc:
        if 'not shareable' in str(exc):
            raise
        raise _map_error(exc) from None
    return None if blocking else False


def send_buffer(cid, obj, unboundop=None, *, timeout=None, blocking=True):
    # CPython ships the *buffer* (mutations are visible on both
    # sides). Our interpreters share one object heap, so a memoryview
    # over the sender's object gives exactly that (gh-110246;
    # test__interpchannels test_send_buffer mutates both ways).
    return send(cid, memoryview(obj), unboundop,
                timeout=timeout, blocking=blocking)


def recv(cid, default=_NO_DEFAULT):
    try:
        if default is _NO_DEFAULT:
            return _channels_backend.channel_recv(int(cid))
        else:
            return _channels_backend.channel_recv(int(cid), default)
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def close(cid, *, send=False, recv=False, force=False):
    try:
        _channels_backend.channel_close(
            int(cid), bool(send), bool(recv), bool(force))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def release(cid, *, send=False, recv=False, force=False):
    try:
        _channels_backend.channel_release(int(cid), bool(send), bool(recv))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def get_info(cid):
    try:
        closed, closing, count = _channels_backend.channel_get_info(int(cid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None
    return ChannelInfo(closed, closing, count)


def get_channel_defaults(cid):
    try:
        return _channels_backend.channel_get_defaults(int(cid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


def get_count(cid):
    try:
        return _channels_backend.channel_get_count(int(cid))
    except (RuntimeError, ValueError) as exc:
        raise _map_error(exc) from None


_end_types = None


def _register_end_types(send_cls, recv_cls):
    # CPython registers the wrapper classes in the XID registry so
    # channel ends are shareable (they reconstruct on the far side from
    # their cid; in our shared-heap model the instance itself crosses).
    # The native shareability check looks for this class marker
    # (test_interpreters test_channels.TestChannels.test_shareable).
    global _end_types
    _end_types = (send_cls, recv_cls)
    send_cls._weave_xid_shareable = True
    recv_cls._weave_xid_shareable = True
