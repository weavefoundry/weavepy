"""PEP 667/709 shim for lowered-comprehension frames (RFC 0057 WS4).

CPython 3.12+ inlines list/set/dict comprehensions into the enclosing
frame (PEP 709); the comprehension's working variables become *hidden*
fast locals (``CO_FAST_HIDDEN``). PEP 667's ``FrameLocalsProxy`` then
gives them a deliberately asymmetric surface: ``proxy["a"]`` finds a
hidden variable, but ``"a" in proxy`` / ``iter(proxy)`` / ``len(proxy)``
skip it (``test_listcomps.test_frame_locals``).

WeavePy lowers those comprehensions to their own frame instead. This
proxy reproduces the CPython-visible surface for such a frame's
``f_locals``: lookups hit the comprehension's own (hidden) locals first
and fall back to the enclosing frame's mapping; every *enumerating*
operation delegates to the enclosing frame only.
"""


class CompFrameLocalsProxy:
    __slots__ = ("_hidden", "_visible")

    def __init__(self, hidden, visible):
        self._hidden = hidden
        self._visible = visible

    def __getitem__(self, key):
        hidden = self._hidden
        if type(key) is str and not key.startswith(".") and key in hidden:
            return hidden[key]
        return self._visible[key]

    def __setitem__(self, key, value):
        hidden = self._hidden
        if type(key) is str and not key.startswith(".") and key in hidden:
            hidden[key] = value
        else:
            self._visible[key] = value

    def __delitem__(self, key):
        hidden = self._hidden
        if type(key) is str and not key.startswith(".") and key in hidden:
            del hidden[key]
        else:
            del self._visible[key]

    def __contains__(self, key):
        return key in self._visible

    def __iter__(self):
        return iter(self._visible)

    def __len__(self):
        return len(self._visible)

    def keys(self):
        return self._visible.keys()

    def values(self):
        return self._visible.values()

    def items(self):
        return self._visible.items()

    def get(self, key, default=None):
        try:
            return self[key]
        except KeyError:
            return default

    def copy(self):
        return dict(self._visible)

    def __eq__(self, other):
        if isinstance(other, CompFrameLocalsProxy):
            other = dict(other._visible)
        return dict(self._visible) == other

    def __repr__(self):
        return repr(dict(self._visible))
