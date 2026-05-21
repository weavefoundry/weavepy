"""Python enumerations — small WeavePy-compatible subset.

Models the most-used surface of CPython's :mod:`enum`:

- :class:`Enum`, :class:`IntEnum`, :class:`Flag`, :class:`IntFlag`
- the :data:`auto` helper
- the :func:`unique` decorator
- ``Color.RED``, ``Color['RED']`` look-up
- iteration over members in declaration order
- ``Color(1)`` value-based lookup
- ``__str__`` / ``__repr__`` matching CPython's defaults
- Bitwise operations on :class:`Flag` / :class:`IntFlag`

Not implemented:

- ``_missing_`` hook for custom value coercion
- ``__init_subclass__`` integration with ``StrEnum`` (Python 3.11+)
- The ``_generate_next_value_`` customisation point
- Bare-class introspection helpers like ``Enum.__members__`` ordered
  dict — we expose a plain dict in declaration order, which is
  effectively the same on modern CPython.
"""


class auto:
    """Sentinel used inside an Enum class body to request
    auto-numbered values. The replacement happens in
    :class:`EnumMeta.__init__`."""

    __slots__ = ("value",)

    def __init__(self):
        self.value = None


def _next_power_of_two(n):
    """Smallest power of two strictly greater than ``n - 1``.

    Used by :class:`Flag` to keep auto-generated values pure bit-flags
    even when the user mixes explicit numeric values with ``auto()``.
    """

    if n <= 1:
        return 1
    p = 1
    while p < n:
        p *= 2
    return p


class EnumMeta(type):
    """The metaclass for all :class:`Enum` subclasses."""

    def __new__(mcs, name, bases, namespace, **kwargs):
        # The bare Enum/Flag/IntEnum/IntFlag base classes are
        # constructed before they themselves exist, so skip member
        # collection when there is no concrete Enum base in `bases`.
        # We use the presence of `_member_map_` *as a class attribute*
        # (set during a previous EnumMeta.__new__) to detect that a
        # base is an Enum-shaped class; whether the map is empty or
        # populated doesn't matter — what matters is that the
        # attribute exists.
        enum_base = None
        for b in bases:
            if isinstance(b, EnumMeta) and "_member_map_" in b.__dict__:
                enum_base = b
                break

        members = {}
        if enum_base is not None:
            # Flag/IntFlag use power-of-2 auto values; plain Enum uses
            # sequential integers. Detect FlagMeta lazily — the very
            # first time we run, FlagMeta itself hasn't been defined
            # yet, but no Flag bases exist either.
            flag_meta = globals().get("FlagMeta")
            is_flag_like = flag_meta is not None and issubclass(mcs, flag_meta)
            next_value = 1
            for key, value in list(namespace.items()):
                if key.startswith("_") and key.endswith("_"):
                    continue
                if callable(value):
                    continue
                if isinstance(value, (property, staticmethod, classmethod)):
                    continue
                if isinstance(value, auto):
                    value = next_value
                    next_value = next_value * 2 if is_flag_like else next_value + 1
                else:
                    if isinstance(value, int):
                        next_value = (
                            _next_power_of_two(value + 1) if is_flag_like else value + 1
                        )
                members[key] = value
            for key in members:
                namespace.pop(key, None)

        cls = super().__new__(mcs, name, bases, namespace, **kwargs)

        if enum_base is None:
            cls._member_map_ = None
            cls._value2member_map_ = None
            return cls

        cls._member_map_ = {}
        cls._value2member_map_ = {}
        for member_name, member_value in members.items():
            # Allow value-aliasing: re-using an existing value picks
            # up the original member (CPython semantics).
            if member_value in cls._value2member_map_:
                cls._member_map_[member_name] = cls._value2member_map_[member_value]
                # Aliases still bind to the same member at the class
                # level so ``MyEnum.ALIAS is MyEnum.ORIGINAL``.
                setattr(cls, member_name, cls._value2member_map_[member_value])
                continue
            member = cls._create_member_(member_name, member_value)
            cls._member_map_[member_name] = member
            cls._value2member_map_[member_value] = member
            # Bind the member as a class attribute so ``Color.RED``
            # works through ordinary attribute lookup.
            setattr(cls, member_name, member)
        return cls

    def __call__(cls, value=None, *args, **kwargs):
        # Member lookup form: `Color(1)`.
        if cls._member_map_ is not None and not args and not kwargs:
            if value in cls._value2member_map_:
                return cls._value2member_map_[value]
            if isinstance(cls, FlagMeta):
                return cls._decompose_flag(value)
            raise ValueError(f"{value!r} is not a valid {cls.__name__}")
        # Functional API: `Color = Enum('Color', 'RED GREEN BLUE')`
        if args or kwargs:
            return cls._create_(value, *args, **kwargs)
        return super().__call__(value)

    def __getitem__(cls, name):
        if cls._member_map_ is None:
            raise KeyError(name)
        return cls._member_map_[name]

    def __iter__(cls):
        if cls._member_map_ is None:
            return iter(())
        return iter(cls._member_map_.values())

    def __len__(cls):
        return 0 if cls._member_map_ is None else len(cls._member_map_)

    def __contains__(cls, member):
        return cls._member_map_ is not None and member in cls._member_map_.values()

    @property
    def __members__(cls):
        return dict(cls._member_map_) if cls._member_map_ is not None else {}

    def _create_member_(cls, name, value):
        member = object.__new__(cls)
        member._name_ = name
        member._value_ = value
        return member

    def _create_(cls, name, names, module=None, qualname=None, type_=None, start=1):
        # Minimal Functional API: accepts a string of space/comma-
        # separated names, or an iterable.
        if isinstance(names, str):
            names = names.replace(",", " ").split()
        bases = (cls,) if type_ is None else (type_, cls)
        body = {}
        next_val = start
        for entry in names:
            if isinstance(entry, tuple):
                key, val = entry
            else:
                key, val = entry, next_val
                next_val += 1
            body[key] = val
        new_cls = EnumMeta(name, bases, body)
        return new_cls


class Enum(metaclass=EnumMeta):
    @property
    def name(self):
        return self._name_

    @property
    def value(self):
        return self._value_

    def __repr__(self):
        return f"<{type(self).__name__}.{self._name_}: {self._value_!r}>"

    def __str__(self):
        return f"{type(self).__name__}.{self._name_}"

    def __eq__(self, other):
        if isinstance(other, Enum):
            return self is other
        return NotImplemented

    def __ne__(self, other):
        return not self.__eq__(other)

    def __hash__(self):
        return hash(self._name_)


class IntEnum(Enum):
    """Mirror of :class:`Enum` whose members compare equal to their
    integer value. (CPython inherits from ``int`` directly; WeavePy
    keeps a separate base and overloads ``__eq__`` / ``__int__`` to
    cover the common patterns.)
    """

    def __int__(self):
        return self._value_

    def __eq__(self, other):
        if isinstance(other, IntEnum):
            return self._value_ == other._value_
        if isinstance(other, int):
            return self._value_ == other
        return NotImplemented

    def __ne__(self, other):
        eq = self.__eq__(other)
        if eq is NotImplemented:
            return NotImplemented
        return not eq

    def __hash__(self):
        return hash(self._value_)

    def __add__(self, other):
        return self._value_ + (other._value_ if isinstance(other, IntEnum) else other)

    def __radd__(self, other):
        return other + self._value_

    def __sub__(self, other):
        return self._value_ - (other._value_ if isinstance(other, IntEnum) else other)

    def __rsub__(self, other):
        return other - self._value_

    def __mul__(self, other):
        return self._value_ * (other._value_ if isinstance(other, IntEnum) else other)

    def __rmul__(self, other):
        return other * self._value_

    def __index__(self):
        return self._value_

    def __lt__(self, other):
        if isinstance(other, IntEnum):
            return self._value_ < other._value_
        if isinstance(other, int):
            return self._value_ < other
        return NotImplemented

    def __le__(self, other):
        if isinstance(other, IntEnum):
            return self._value_ <= other._value_
        if isinstance(other, int):
            return self._value_ <= other
        return NotImplemented

    def __gt__(self, other):
        if isinstance(other, IntEnum):
            return self._value_ > other._value_
        if isinstance(other, int):
            return self._value_ > other
        return NotImplemented

    def __ge__(self, other):
        if isinstance(other, IntEnum):
            return self._value_ >= other._value_
        if isinstance(other, int):
            return self._value_ >= other
        return NotImplemented


class FlagMeta(EnumMeta):
    """Metaclass for :class:`Flag` — adds bitwise-decomposed lookups."""

    def _decompose_flag(cls, value):
        # Combine individual single-bit members covered by `value`.
        if cls._member_map_ is None:
            raise ValueError(f"{value!r} is not a valid {cls.__name__}")
        combined_name = []
        combined_value = 0
        for name, member in cls._member_map_.items():
            if member._value_ & value == member._value_ and member._value_:
                combined_name.append(name)
                combined_value |= member._value_
        if combined_value != value:
            raise ValueError(f"{value!r} is not a valid {cls.__name__}")
        new_member = object.__new__(cls)
        new_member._name_ = "|".join(combined_name)
        new_member._value_ = value
        return new_member


class Flag(Enum, metaclass=FlagMeta):
    def __or__(self, other):
        if isinstance(other, type(self)):
            return type(self)._decompose_flag(self._value_ | other._value_)
        return NotImplemented

    def __and__(self, other):
        if isinstance(other, type(self)):
            return type(self)._decompose_flag(self._value_ & other._value_)
        return NotImplemented

    def __xor__(self, other):
        if isinstance(other, type(self)):
            return type(self)._decompose_flag(self._value_ ^ other._value_)
        return NotImplemented

    def __invert__(self):
        all_bits = 0
        for member in type(self)._member_map_.values():
            all_bits |= member._value_
        return type(self)._decompose_flag(all_bits & ~self._value_)

    def __contains__(self, other):
        if isinstance(other, type(self)):
            return (self._value_ & other._value_) == other._value_
        return False

    def __bool__(self):
        return bool(self._value_)


class IntFlag(Flag):
    """Like :class:`IntEnum` but for bitfield-style values."""

    def __int__(self):
        return self._value_

    def __eq__(self, other):
        if isinstance(other, IntFlag):
            return self._value_ == other._value_
        if isinstance(other, int):
            return self._value_ == other
        return NotImplemented

    def __hash__(self):
        return hash(self._value_)


def unique(enumeration):
    """Class decorator that ensures only one name maps to any value."""
    seen = {}
    duplicates = []
    for name, member in (enumeration._member_map_ or {}).items():
        if member._value_ in seen:
            duplicates.append((name, seen[member._value_]))
        else:
            seen[member._value_] = name
    if duplicates:
        joined = ", ".join(f"{n} -> {a}" for n, a in duplicates)
        raise ValueError(f"duplicate values found in {enumeration!r}: {joined}")
    return enumeration


__all__ = [
    "auto",
    "EnumMeta",
    "Enum",
    "IntEnum",
    "FlagMeta",
    "Flag",
    "IntFlag",
    "unique",
]
