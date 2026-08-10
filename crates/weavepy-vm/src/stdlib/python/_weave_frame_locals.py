"""PEP 667 ``FrameLocalsProxy`` (RFC 0060, extending RFC 0057 WS4).

CPython 3.13 makes ``frame.f_locals`` of an *optimized* (function)
frame a write-through view over the frame's fast locals: reads track
execution live, writes land in the actual variable slots (or cells),
and keys that don't name a fast local are kept in a per-frame
``f_extra_locals`` dict. WeavePy implements the mapping surface here,
over the native primitives in ``_weave_frame`` (which read/write the
live frame storage directly).

PEP 709 wrinkle: CPython inlines list/set/dict comprehensions into the
enclosing frame, making the iteration variables *hidden* fast locals —
``proxy["a"]`` finds them, but ``in`` / ``iter`` / ``len`` skip them.
WeavePy lowers those comprehensions to their own frames; a proxy built
for such a frame reproduces CPython's surface by reading its own
(hidden) locals first and delegating everything visible — including
writes, which CPython routes past hidden slots — to the enclosing
frame's mapping.
"""

import _weave_frame as _wf

_MISSING = object()

_repr_running = set()


class FrameLocalsProxy:
    __slots__ = ("_frame", "_hidden", "_visible")

    # Py_TPFLAGS_MAPPING for MATCH_MAPPING (`case {...}` patterns).
    _abc_collection_flags = 1 << 6

    def __new__(cls, *args, **kwargs):
        if kwargs:
            raise TypeError("FrameLocalsProxy() takes no keyword arguments")
        if len(args) != 1:
            raise TypeError(
                "FrameLocalsProxy() takes exactly 1 argument (%d given)" % len(args)
            )
        frame = args[0]
        _wf.check(frame)
        self = object.__new__(cls)
        object.__setattr__(self, "_frame", frame)
        if _wf.is_comp(frame):
            back = frame.f_back
            if back is not None:
                object.__setattr__(self, "_hidden", frame)
                object.__setattr__(self, "_visible", back.f_locals)
                return self
        object.__setattr__(self, "_hidden", None)
        object.__setattr__(self, "_visible", None)
        return self

    # -- key resolution ---------------------------------------------

    def _resolve(self, key, frame):
        """The fast-local name `key` designates in `frame`, or None.

        Exact-str keys match by name; other keys are hashed first
        (unhashable keys are a TypeError, like CPython) and matched by
        equality, so str subclasses and impostor objects that compare
        equal to a variable name resolve to that variable.
        """
        if type(key) is str:
            return key if _wf.is_fast(frame, key) else None
        hash(key)
        for name in _wf.fast_names(frame):
            if key == name:
                return name
        return None

    # -- mapping protocol -------------------------------------------

    def __getitem__(self, key):
        hidden = self._hidden
        if hidden is not None:
            # Reads see bound hidden (comprehension) variables first.
            name = self._resolve(key, hidden)
            if name is not None:
                try:
                    return _wf.getvar(hidden, name)
                except KeyError:
                    pass
            return self._visible[key]
        frame = self._frame
        name = self._resolve(key, frame)
        if name is not None:
            try:
                return _wf.getvar(frame, name)
            except KeyError:
                pass
        extra = _wf.extra(frame, False)
        if extra is not None and key in extra:
            return extra[key]
        raise KeyError(key)

    def __setitem__(self, key, value):
        if self._hidden is not None:
            # CPython routes writes past hidden slots: they land in the
            # enclosing frame's mapping.
            hash(key)
            self._visible[key] = value
            return
        frame = self._frame
        name = self._resolve(key, frame)
        if name is not None and _wf.setvar(frame, name, value):
            return
        _wf.extra(frame, True)[key] = value

    def __delitem__(self, key):
        if self._hidden is not None:
            hash(key)
            del self._visible[key]
            return
        frame = self._frame
        name = self._resolve(key, frame)
        if name is not None:
            raise ValueError(
                "cannot remove local variables from FrameLocalsProxy"
            )
        extra = _wf.extra(frame, False)
        if extra is not None and key in extra:
            del extra[key]
            return
        raise KeyError(key)

    def __contains__(self, key):
        # Hidden variables are invisible to `in` (PEP 667 asymmetry).
        if self._hidden is not None:
            return key in self._visible
        frame = self._frame
        name = self._resolve(key, frame)
        if name is not None:
            try:
                _wf.getvar(frame, name)
                return True
            except KeyError:
                pass
        extra = _wf.extra(frame, False)
        return extra is not None and key in extra

    def keys(self):
        if self._hidden is not None:
            visible = self._visible
            if isinstance(visible, FrameLocalsProxy):
                return visible.keys()
            return list(visible.keys())
        frame = self._frame
        out = _wf.bound_names(frame)
        extra = _wf.extra(frame, False)
        if extra is not None:
            out.extend(extra.keys())
        return out

    def values(self):
        return [self[k] for k in self.keys()]

    def items(self):
        return [(k, self[k]) for k in self.keys()]

    def __iter__(self):
        return iter(self.keys())

    def __reversed__(self):
        return list(reversed(self.keys()))

    def __len__(self):
        return len(self.keys())

    def get(self, key, default=None):
        try:
            return self[key]
        except KeyError:
            return default

    def setdefault(self, key, default=None):
        try:
            return self[key]
        except KeyError:
            self[key] = default
            return default

    def pop(self, key, default=_MISSING):
        if self._hidden is not None:
            visible = self._visible
            if default is _MISSING:
                return visible.pop(key)
            return visible.pop(key, default)
        frame = self._frame
        name = self._resolve(key, frame)
        if name is not None:
            raise ValueError(
                "cannot remove local variables from FrameLocalsProxy"
            )
        extra = _wf.extra(frame, False)
        if extra is not None and key in extra:
            value = extra[key]
            del extra[key]
            return value
        if default is _MISSING:
            raise KeyError(key)
        return default

    def update(self, other=_MISSING, /, **kwargs):
        if other is not _MISSING:
            if hasattr(other, "keys") and callable(other.keys):
                for key in other.keys():
                    self[key] = other[key]
            else:
                for key, value in other:
                    self[key] = value
        for key, value in kwargs.items():
            self[key] = value

    def copy(self):
        return dict(self.items())

    # -- number protocol (dict union) --------------------------------

    def __or__(self, other):
        if isinstance(other, FrameLocalsProxy):
            other = other.copy()
        if not isinstance(other, dict):
            return NotImplemented
        return self.copy() | other

    def __ror__(self, other):
        if not isinstance(other, dict):
            return NotImplemented
        return other | self.copy()

    def __ior__(self, other):
        if isinstance(other, FrameLocalsProxy):
            other = other.copy()
        if not isinstance(other, dict):
            return NotImplemented
        self.update(other)
        return self

    # -- comparisons / repr ------------------------------------------

    def __eq__(self, other):
        if isinstance(other, FrameLocalsProxy):
            other = other.copy()
        if isinstance(other, dict):
            return self.copy() == other
        try:
            keys = other.keys
        except AttributeError:
            return NotImplemented
        if not callable(keys):
            return NotImplemented
        return self.copy() == dict(other)

    def __repr__(self):
        # Py_ReprEnter-style cycle guard: a proxy reachable from its
        # own values (e.g. `d[1] = d`) reprs as `{...}` like a dict.
        key = id(self)
        if key in _repr_running:
            return "{...}"
        _repr_running.add(key)
        try:
            return repr(self.copy())
        finally:
            _repr_running.discard(key)

    # copy.copy / copy.deepcopy / pickle are unsupported, like
    # CPython's C proxy (test_frame.test_unsupport).
    def __reduce_ex__(self, protocol):
        raise TypeError("cannot pickle 'FrameLocalsProxy' object")

    def __reduce__(self):
        raise TypeError("cannot pickle 'FrameLocalsProxy' object")


try:
    from _collections_abc import Mapping as _Mapping

    _Mapping.register(FrameLocalsProxy)
except Exception:
    pass
