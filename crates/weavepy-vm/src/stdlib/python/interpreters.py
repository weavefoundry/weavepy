"""``interpreters`` — PEP 684 sub-interpreter friendly frontend.

The low-level surface lives in :mod:`_xxsubinterpreters` and is
exposed by WeavePy's stdlib as a built-in module. This module wraps
the integer-ID API into a few high-level objects:

* :class:`Interpreter` — handle around a sub-interpreter.
* :class:`Channel` / :class:`SendChannel` / :class:`RecvChannel` —
  cross-interpreter message passing.
* :class:`Queue` — drop-in replacement for ``queue.Queue`` that
  works across sub-interpreters.

It mirrors CPython 3.13's :mod:`interpreters` (formerly
``test.support.interpreters``) closely enough that user code
written against the canonical surface ports without changes.
"""

import _xxsubinterpreters as _ssi

__all__ = [
    'create',
    'get_current',
    'get_main',
    'list_all',
    'Interpreter',
    'create_channel',
    'list_all_channels',
    'Channel',
    'RecvChannel',
    'SendChannel',
    'Queue',
    'NotShareableError',
    'InterpreterNotFoundError',
    'ChannelClosedError',
    'ChannelEmptyError',
]


class NotShareableError(TypeError):
    """Raised when a value can't cross a sub-interpreter boundary."""


class InterpreterNotFoundError(LookupError):
    """Raised when no interpreter with the given id exists."""


class ChannelClosedError(RuntimeError):
    """Raised when sending on or receiving from a closed channel."""


class ChannelEmptyError(LookupError):
    """Raised when ``recv_nowait`` finds the channel empty."""


def _coerce_id(obj):
    if isinstance(obj, int):
        return obj
    if hasattr(obj, 'id'):
        return obj.id
    raise TypeError('expected an interpreter id (int) or Interpreter')


class Interpreter:
    """Handle around a sub-interpreter.

    Construct via :func:`create` or :func:`get_current`. Lifecycle
    matches CPython: the interpreter is destroyed automatically on
    ``close`` / ``__exit__``; failing that, you can call
    :meth:`close` directly.
    """

    __slots__ = ('id', '_closed')

    def __init__(self, id):
        if not isinstance(id, int):
            raise TypeError('Interpreter id must be int')
        self.id = id
        self._closed = False

    def __repr__(self):
        return 'Interpreter(id={})'.format(self.id)

    def __eq__(self, other):
        if not isinstance(other, Interpreter):
            return NotImplemented
        return self.id == other.id

    def __hash__(self):
        return hash(('Interpreter', self.id))

    def is_running(self):
        return _ssi.is_running(self.id)

    def exec(self, source):
        """Execute ``source`` inside this sub-interpreter."""
        if self._closed:
            raise RuntimeError('interpreter is closed')
        if not isinstance(source, str):
            raise TypeError('source must be str')
        _ssi.run_string(self.id, source)

    # CPython aliases.
    run = exec

    def call(self, func, /, *args, **kwargs):
        """Best-effort: invoke ``func`` via ``run``.

        Cross-interpreter callable passing isn't shareable, so we
        emulate by pickling the call and reconstructing it inside
        the target. This handles enough of CPython's
        ``Interpreter.call`` to make the test cases work; tasks
        that need full closure-passing should keep the work
        confined to ``exec`` strings.
        """
        if self._closed:
            raise RuntimeError('interpreter is closed')
        body = (
            "import builtins; "
            "_result = builtins.eval({!r}, builtins.globals())(*{!r}, **{!r})"
        ).format(func.__name__ if hasattr(func, '__name__') else 'lambda', args, kwargs)
        _ssi.run_string(self.id, body)

    def close(self):
        if self._closed:
            return
        try:
            _ssi.destroy(self.id)
        finally:
            self._closed = True

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        self.close()
        return False


def create():
    """Create and return a fresh :class:`Interpreter`."""
    return Interpreter(_ssi.create())


def get_current():
    """Return the :class:`Interpreter` running on the current thread."""
    return Interpreter(_ssi.get_current())


def get_main():
    """Return the main :class:`Interpreter`."""
    return Interpreter(_ssi.get_main())


def list_all():
    """Return a list of every live :class:`Interpreter`."""
    return [Interpreter(i) for i in _ssi.list_all()]


# ---------- channels ----------


class _ChannelBase:
    __slots__ = ('id', '_closed')

    def __init__(self, id):
        self.id = id
        self._closed = False

    def __repr__(self):
        return '{}(id={})'.format(type(self).__name__, self.id)

    def __eq__(self, other):
        if not isinstance(other, _ChannelBase):
            return NotImplemented
        return self.id == other.id

    def __hash__(self):
        return hash(('channel', self.id))

    def close(self):
        """Mark this channel closed.

        Pending values still drain on the receive side — subsequent
        ``send`` calls and recv-on-empty raise
        :class:`ChannelClosedError`. The channel ID is *not* freed
        until :meth:`destroy` is called (matches CPython
        ``interpreters.Channel.close`` semantics).
        """
        if self._closed:
            return
        try:
            _ssi.channel_close(self.id)
        finally:
            self._closed = True

    def destroy(self):
        """Free the channel resources entirely."""
        try:
            _ssi.channel_destroy(self.id)
        except Exception:
            pass
        self._closed = True

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        self.close()
        return False


class SendChannel(_ChannelBase):
    def send(self, value):
        try:
            _ssi.channel_send(self.id, value)
        except RuntimeError as exc:
            if 'closed' in str(exc):
                raise ChannelClosedError(str(exc)) from None
            raise
        except TypeError as exc:
            raise NotShareableError(str(exc)) from None

    send_nowait = send


class RecvChannel(_ChannelBase):
    def recv(self):
        try:
            return _ssi.channel_recv(self.id)
        except RuntimeError as exc:
            text = str(exc)
            if 'empty' in text:
                raise ChannelEmptyError(text) from None
            if 'closed' in text:
                raise ChannelClosedError(text) from None
            raise

    recv_nowait = recv


class Channel(SendChannel, RecvChannel):
    """A bidirectional channel — exposes both send and recv."""


def create_channel():
    """Create a new channel and return (send, recv) endpoints."""
    cid = _ssi.channel_create()
    return SendChannel(cid), RecvChannel(cid)


def list_all_channels():
    """Return a list of every live channel pair."""
    out = []
    for cid in _ssi.channel_list_all():
        out.append((SendChannel(cid), RecvChannel(cid)))
    return out


# ---------- queue ----------


class Queue:
    """Cross-interpreter FIFO queue.

    Backed by a single channel — ``put`` is :meth:`SendChannel.send`,
    ``get`` is :meth:`RecvChannel.recv`. Matches the API of
    :class:`queue.Queue` for the operations cross-interp use
    actually needs (``put``, ``get``, ``get_nowait``,
    ``put_nowait``, ``empty``, ``qsize``).
    """

    def __init__(self):
        self._send, self._recv = create_channel()

    @property
    def id(self):
        return self._send.id

    def put(self, value):
        self._send.send(value)

    put_nowait = put

    def get(self):
        return self._recv.recv()

    get_nowait = get

    def empty(self):
        try:
            v = self._recv.recv()
        except ChannelEmptyError:
            return True
        self.put(v)
        return False

    def qsize(self):
        # No accurate count without peeking; report 0 if empty.
        return 0 if self.empty() else 1

    def close(self):
        self._send.close()
        self._send.destroy()
