"""Support for template string literals (PEP 750).

WeavePy: pure-Python implementation of CPython 3.14's
``string.templatelib``, available under the ``-X lang=next`` language
preview (RFC 0076 WS15). The ``BUILD_TEMPLATE`` / ``BUILD_INTERPOLATION``
opcodes emitted for ``t"..."`` literals construct these same types, so
literal-created and manually-created templates are indistinguishable.
"""

__all__ = ["Template", "Interpolation", "convert"]


def convert(obj, /, conversion):
    """Apply formatted-string-literal conversion semantics to *obj*.

    ``'s'`` calls :func:`str`, ``'r'`` calls :func:`repr`, ``'a'`` calls
    :func:`ascii`; ``None`` returns *obj* unchanged.
    """
    if conversion is None:
        return obj
    if conversion == 's':
        return str(obj)
    if conversion == 'r':
        return repr(obj)
    if conversion == 'a':
        return ascii(obj)
    raise ValueError(f'invalid conversion specifier: {conversion!r}')


class Interpolation:
    """One ``{...}`` replacement field of a template string.

    Immutable. ``value`` is the evaluated result; ``expression`` is the
    source text between the braces (before any ``!``/``:``/``=``);
    ``conversion`` is ``None`` or one of ``'s'``/``'r'``/``'a'``;
    ``format_spec`` is the (eagerly evaluated) format-spec string.
    """

    __slots__ = ('_value', '_expression', '_conversion', '_format_spec')
    __match_args__ = ('value', 'expression', 'conversion', 'format_spec')

    def __new__(cls, value, expression='', conversion=None, format_spec=''):
        if not isinstance(expression, str):
            raise TypeError(
                f'Interpolation() argument 2 must be str, not '
                f'{type(expression).__name__}')
        if conversion not in (None, 's', 'r', 'a'):
            raise ValueError(
                "Interpolation() argument 'conversion' must be one of "
                "'s', 'a' or 'r'")
        if not isinstance(format_spec, str):
            raise TypeError(
                f'Interpolation() argument 4 must be str, not '
                f'{type(format_spec).__name__}')
        self = super().__new__(cls)
        object.__setattr__(self, '_value', value)
        object.__setattr__(self, '_expression', expression)
        object.__setattr__(self, '_conversion', conversion)
        object.__setattr__(self, '_format_spec', format_spec)
        return self

    @property
    def value(self):
        return self._value

    @property
    def expression(self):
        return self._expression

    @property
    def conversion(self):
        return self._conversion

    @property
    def format_spec(self):
        return self._format_spec

    def __setattr__(self, name, value):
        raise AttributeError(
            f'cannot set attribute {name!r} on immutable '
            f'{type(self).__name__} instance')

    def __delattr__(self, name):
        raise AttributeError(
            f'cannot delete attribute {name!r} on immutable '
            f'{type(self).__name__} instance')

    def __repr__(self):
        return (f'{type(self).__name__}({self._value!r}, '
                f'{self._expression!r}, {self._conversion!r}, '
                f'{self._format_spec!r})')


class Template:
    """The contents of a template string literal (``t"..."``).

    Immutable. Stored as ``strings`` (a tuple with exactly one more
    element than ``interpolations`` — the static text around each
    field, including empty strings) plus ``interpolations``.
    """

    __slots__ = ('_strings', '_interpolations')

    def __new__(cls, *args):
        strings = []
        interpolations = []
        current = ''
        for arg in args:
            if isinstance(arg, str):
                # Consecutive strings concatenate into one entry.
                current += arg
            elif isinstance(arg, Interpolation):
                strings.append(current)
                current = ''
                interpolations.append(arg)
            else:
                raise TypeError(
                    f'Template.__new__ *args need to be of type '
                    f"'str' or 'Interpolation', got "
                    f'{type(arg).__name__}')
        strings.append(current)
        self = super().__new__(cls)
        object.__setattr__(self, '_strings', tuple(strings))
        object.__setattr__(self, '_interpolations', tuple(interpolations))
        return self

    @property
    def strings(self):
        return self._strings

    @property
    def interpolations(self):
        return self._interpolations

    @property
    def values(self):
        return tuple(i.value for i in self._interpolations)

    def __setattr__(self, name, value):
        raise AttributeError(
            f'cannot set attribute {name!r} on immutable '
            f'{type(self).__name__} instance')

    def __delattr__(self, name):
        raise AttributeError(
            f'cannot delete attribute {name!r} on immutable '
            f'{type(self).__name__} instance')

    def __iter__(self):
        # Non-empty strings and interpolations, in order; empty strings
        # are skipped.
        interpolations = self._interpolations
        for i, s in enumerate(self._strings):
            if s:
                yield s
            if i < len(interpolations):
                yield interpolations[i]

    def __add__(self, other):
        if isinstance(other, Template):
            # Renormalizing through the constructor merges the boundary
            # strings and re-inserts any needed empty separators.
            return Template(*tuple(self), *tuple(other))
        # Template + str is deliberately unsupported (ambiguous: static
        # text or interpolation?) — see PEP 750.
        return NotImplemented

    def __repr__(self):
        return (f'{type(self).__name__}(strings={self._strings!r}, '
                f'interpolations={self._interpolations!r})')
