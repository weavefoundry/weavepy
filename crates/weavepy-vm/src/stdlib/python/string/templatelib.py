"""Support for template string literals (PEP 750).

WeavePy: pure-Python implementation of CPython 3.14's
``string.templatelib``, available under the ``-X lang=next`` language
preview (RFC 0076 WS15). The ``BUILD_TEMPLATE`` / ``BUILD_INTERPOLATION``
opcodes emitted for ``t"..."`` literals construct these same types, so
literal-created and manually-created templates are indistinguishable.
"""

__all__ = ["Template", "Interpolation", "convert"]

# 3.14: `Template[int]` / `Interpolation[int]` are `types.GenericAlias`
# (the C types carry `__class_getitem__ = Py_GenericAlias`).
from types import GenericAlias as _GenericAlias


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
    raise ValueError(f'invalid conversion specifier: {conversion}')


def _template_unpickle(*args):
    import itertools

    if len(args) != 2:
        raise ValueError('Template expects tuple of length 2 to unpickle')

    strings, interpolations = args
    parts = []
    for string, interpolation in itertools.zip_longest(strings, interpolations):
        if string is not None:
            parts.append(string)
        if interpolation is not None:
            parts.append(interpolation)
    return Template(*parts)


def _reject_subclass(cls, base):
    # CPython's Template/Interpolation/TemplateIter are not
    # `Py_TPFLAGS_BASETYPE` (test_templatelib.test_final_types).
    if cls is not base:
        raise TypeError(
            f"type '{base.__name__}' is not an acceptable base type")


class Interpolation:
    """One ``{...}`` replacement field of a template string.

    Immutable. ``value`` is the evaluated result; ``expression`` is the
    source text between the braces (before any ``!``/``:``/``=``);
    ``conversion`` is ``None`` or one of ``'s'``/``'r'``/``'a'``;
    ``format_spec`` is the (eagerly evaluated) format-spec string.
    """

    __slots__ = ('_value', '_expression', '_conversion', '_format_spec')
    __match_args__ = ('value', 'expression', 'conversion', 'format_spec')

    __class_getitem__ = classmethod(_GenericAlias)

    def __init_subclass__(cls, **kwargs):
        _reject_subclass(cls, Interpolation)

    def __reduce__(self):
        return (type(self), (self._value, self._expression,
                             self._conversion, self._format_spec))

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

    __class_getitem__ = classmethod(_GenericAlias)

    def __init_subclass__(cls, **kwargs):
        _reject_subclass(cls, Template)

    def __reduce__(self):
        return (_template_unpickle, (self._strings, self._interpolations))

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
        # text or interpolation?) — see PEP 750. CPython's `template_concat`
        # is an `sq_concat` slot: a right operand with its own `__radd__`
        # (an `nb_add` slot) is tried first, anything else gets the
        # sequence-concat wording.
        if hasattr(type(other), '__radd__'):
            return NotImplemented
        raise TypeError(
            'can only concatenate string.templatelib.Template (not '
            f'"{_tp_name(type(other))}") to string.templatelib.Template')

    def __repr__(self):
        return (f'{type(self).__name__}(strings={self._strings!r}, '
                f'interpolations={self._interpolations!r})')


def _tp_name(cls):
    """`Py_TYPE(o)->tp_name`: dotted for the module's static types."""
    if cls is Template or cls is Interpolation:
        return f'string.templatelib.{cls.__name__}'
    return cls.__name__


# CPython's `Template`/`Interpolation` are static C types whose `tp_name`
# carries the module prefix, and `tp_name`-based error text prints it
# ('can only concatenate str (not "string.templatelib.Template") to str').
__weavepy_set_tp_name__(Template, "string.templatelib.Template")
__weavepy_set_tp_name__(Interpolation, "string.templatelib.Interpolation")
