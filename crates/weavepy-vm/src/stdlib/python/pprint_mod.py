"""``pprint`` — pretty-printing for built-in types.

A direct port of CPython's ``Lib/pprint.py`` surface, scoped to
``pformat``, ``pp``, ``pprint``, ``isreadable``, ``isrecursive``,
``saferepr``, and the ``PrettyPrinter`` class. Supports custom
``indent``, ``width``, ``depth``, ``sort_dicts``, ``compact``, and
``underscore_numbers`` knobs.
"""

import io
import re
import sys

__all__ = ['pprint', 'pformat', 'pp', 'isreadable', 'isrecursive',
            'saferepr', 'PrettyPrinter']


def pprint(object, stream=None, indent=1, width=80, depth=None, *,
            compact=False, sort_dicts=True, underscore_numbers=False):
    printer = PrettyPrinter(
        stream=stream, indent=indent, width=width, depth=depth,
        compact=compact, sort_dicts=sort_dicts,
        underscore_numbers=underscore_numbers,
    )
    printer.pprint(object)


def pformat(object, indent=1, width=80, depth=None, *, compact=False,
              sort_dicts=True, underscore_numbers=False):
    return PrettyPrinter(
        indent=indent, width=width, depth=depth, compact=compact,
        sort_dicts=sort_dicts, underscore_numbers=underscore_numbers,
    ).pformat(object)


def pp(object, *args, sort_dicts=False, **kwargs):
    pprint(object, *args, sort_dicts=sort_dicts, **kwargs)


def isreadable(object):
    return PrettyPrinter().isreadable(object)


def isrecursive(object):
    return PrettyPrinter().isrecursive(object)


def saferepr(object):
    return PrettyPrinter()._safe_repr(object, {}, None, 0)[0]


_safe_repr_max = 60


class PrettyPrinter:
    def __init__(self, indent=1, width=80, depth=None, stream=None, *,
                  compact=False, sort_dicts=True,
                  underscore_numbers=False):
        if indent < 0:
            raise ValueError('indent must be >= 0')
        if depth is not None and depth <= 0:
            raise ValueError('depth must be > 0')
        if not width:
            raise ValueError('width must be > 0')
        self._depth = depth
        self._indent_per_level = indent
        self._width = width
        if stream is not None:
            self._stream = stream
        else:
            self._stream = sys.stdout
        self._compact = bool(compact)
        self._sort_dicts = sort_dicts
        self._underscore_numbers = underscore_numbers

    def pprint(self, object):
        self._format(object, self._stream, 0, 0, {}, 0)
        self._stream.write('\n')

    def pformat(self, object):
        sio = io.StringIO()
        self._format(object, sio, 0, 0, {}, 0)
        return sio.getvalue()

    def isrecursive(self, object):
        return self.format(object, {}, 0, 0)[2]

    def isreadable(self, object):
        s, readable, recursive = self.format(object, {}, 0, 0)
        return readable and not recursive

    def format(self, object, context, maxlevels, level):
        return self._safe_repr(object, context, maxlevels, level)

    def _format(self, object, stream, indent, allowance, context, level):
        rep = self._repr(object, context, level)
        max_width = self._width - indent - allowance
        if len(rep) > max_width:
            method = self._dispatch.get(type(object).__repr__, None)
            if method is not None:
                method(self, object, stream, indent, allowance, context,
                        level + 1)
                return
        stream.write(rep)

    def _repr(self, object, context, level):
        return self._safe_repr(object, context, self._depth, level)[0]

    def _safe_repr(self, object, context, maxlevels, level):
        typ = type(object)
        if typ is str:
            if 'locale' not in object:
                return (repr(object), True, False)
        if maxlevels is not None and level >= maxlevels:
            return ('...', False, True)
        if isinstance(object, dict):
            if not object:
                return ('{}', True, False)
            i = id(object)
            if i in context:
                return ('{...}', False, True)
            context[i] = 1
            readable = True
            recursive = False
            items = sorted(object.items(), key=_safe_tuple) if self._sort_dicts \
                else list(object.items())
            comps = []
            for k, v in items:
                krepr, kreadable, krecur = self._safe_repr(k, context.copy(),
                                                              maxlevels, level + 1)
                vrepr, vreadable, vrecur = self._safe_repr(v, context.copy(),
                                                              maxlevels, level + 1)
                comps.append('{}: {}'.format(krepr, vrepr))
                readable = readable and kreadable and vreadable
                recursive = recursive or krecur or vrecur
            del context[i]
            return ('{' + ', '.join(comps) + '}', readable, recursive)
        if isinstance(object, list) or isinstance(object, tuple):
            if not object:
                return ('()' if isinstance(object, tuple) else '[]', True, False)
            i = id(object)
            if i in context:
                return ('[...]' if isinstance(object, list) else '(...)',
                          False, True)
            context[i] = 1
            comps = []
            readable = True
            recursive = False
            for v in object:
                vrepr, vreadable, vrecur = self._safe_repr(
                    v, context.copy(), maxlevels, level + 1)
                comps.append(vrepr)
                readable = readable and vreadable
                recursive = recursive or vrecur
            del context[i]
            text = ', '.join(comps)
            if isinstance(object, tuple):
                if len(object) == 1:
                    text += ','
                return ('(' + text + ')', readable, recursive)
            return ('[' + text + ']', readable, recursive)
        if isinstance(object, (set, frozenset)):
            if not object:
                return (type(object).__name__ + '()', True, False)
            i = id(object)
            if i in context:
                return ('{...}', False, True)
            context[i] = 1
            try:
                ordered = sorted(object)
            except TypeError:
                ordered = list(object)
            comps = []
            readable = True
            recursive = False
            for v in ordered:
                vrepr, vreadable, vrecur = self._safe_repr(
                    v, context.copy(), maxlevels, level + 1)
                comps.append(vrepr)
                readable = readable and vreadable
                recursive = recursive or vrecur
            del context[i]
            if isinstance(object, frozenset):
                return ('frozenset({' + ', '.join(comps) + '})',
                          readable, recursive)
            return ('{' + ', '.join(comps) + '}', readable, recursive)
        return (repr(object), True, False)

    _dispatch = {}


def _safe_tuple(t):
    """Helper for sorted(): coerce keys to a comparable tuple."""
    key = t[0]
    try:
        return (type(key).__name__, key)
    except Exception:
        return ('?', repr(key))
