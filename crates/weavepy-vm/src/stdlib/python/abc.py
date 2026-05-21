"""Abstract Base Classes (PEP 3119), simplified.

This is a WeavePy-compatible re-implementation of the surface of
CPython's :mod:`abc`. It supports:

- ``ABCMeta``: a metaclass that records abstract methods and
  permits virtual-subclass registration via ``register()``.
- ``ABC``: a convenience base whose metaclass is :class:`ABCMeta`.
- ``abstractmethod``: marks a method as abstract.
- ``abstractproperty`` / ``abstractclassmethod`` / ``abstractstaticmethod``
  for backward compatibility — the documented modern form is
  ``@property @abstractmethod`` (etc.), but the old decorators are
  still in widespread use.

What is intentionally omitted:

- ``ABCMeta.__subclasshook__`` slow-path that walks the registered
  subclass cache invalidating it as classes get added — we use a
  simple set, since our object graph isn't watched for invalidation.
- The ``_py_abc`` C-accelerated fast path; everything here is pure
  Python.
"""


def abstractmethod(funcobj):
    """Mark *funcobj* as abstract. The decorated callable still works
    when invoked through ``super()`` from a concrete subclass; what
    changes is that ``ABCMeta`` refuses to instantiate any class
    that still carries an abstract method by the time its class
    statement finishes.
    """
    funcobj.__isabstractmethod__ = True
    return funcobj


class ABCMeta(type):
    """Metaclass for defining Abstract Base Classes (ABCs).

    Use this metaclass to create an ABC. An ABC can be subclassed
    directly, and then acts as a mix-in class. You can also register
    unrelated concrete classes (even built-in classes) and unrelated
    ABCs as 'virtual subclasses' — these and their descendants will
    be considered subclasses of the registering ABC by the built-in
    ``issubclass()`` function, but the registering ABC won't show up
    in their MRO, nor will method implementations defined by the
    registering ABC be callable (not even via ``super()``).
    """

    def __init__(cls, name, bases, namespace, **kwargs):
        super().__init__(name, bases, namespace, **kwargs)
        # Collect abstract methods declared on the class plus any
        # inherited ones that are not overridden by a concrete impl.
        abstracts = set()
        for attr_name, value in namespace.items():
            if getattr(value, "__isabstractmethod__", False):
                abstracts.add(attr_name)
        for base in bases:
            for attr_name in getattr(base, "__abstractmethods__", set()):
                value = namespace.get(attr_name, None)
                if value is None:
                    value = getattr(cls, attr_name, None)
                if getattr(value, "__isabstractmethod__", False):
                    abstracts.add(attr_name)
        cls.__abstractmethods__ = frozenset(abstracts)
        cls._abc_registry = set()

    def __call__(cls, *args, **kwargs):
        # Refuse to instantiate a class that still has unimplemented
        # abstract methods. Mirrors CPython's `object_new` check, but
        # implemented here so we don't need a special hook in the VM.
        abstracts = getattr(cls, "__abstractmethods__", None)
        if abstracts:
            names = ", ".join(sorted(abstracts))
            raise TypeError(
                f"Can't instantiate abstract class {cls.__name__} "
                f"with abstract methods {names}"
            )
        # Bypass ``super().__call__`` (which would round-trip through
        # ``type.__call__`` — not yet a real callable in this VM) and
        # invoke the standard ``__new__`` / ``__init__`` dance directly.
        new = cls.__new__
        instance = new(cls, *args, **kwargs)
        if isinstance(instance, cls):
            instance.__init__(*args, **kwargs)
        return instance

    def register(cls, subclass):
        """Register *subclass* as a virtual subclass of this ABC."""
        if not isinstance(subclass, type):
            raise TypeError("Can only register classes")
        if issubclass(subclass, cls):
            return subclass
        cls._abc_registry.add(subclass)
        return subclass

    def __instancecheck__(cls, instance):
        return cls.__subclasscheck__(type(instance))

    def __subclasscheck__(cls, subclass):
        # Fast path: ordinary subclass relationship.
        if cls is subclass:
            return True
        if type(subclass) is type or isinstance(subclass, type):
            if cls in getattr(subclass, "__mro__", ()):
                return True
        # Registered virtual subclasses (directly or transitively).
        registry = getattr(cls, "_abc_registry", ())
        for reg in registry:
            if reg is subclass:
                return True
            if isinstance(subclass, type) and issubclass(subclass, reg):
                return True
        return False


class ABC(metaclass=ABCMeta):
    """Helper class — direct inheritance avoids having to spell
    ``metaclass=ABCMeta`` every time."""

    __slots__ = ()


def abstractproperty(funcobj):
    funcobj = property(funcobj)
    funcobj.__isabstractmethod__ = True
    return funcobj


def abstractclassmethod(funcobj):
    cm = classmethod(funcobj)
    cm.__isabstractmethod__ = True
    return cm


def abstractstaticmethod(funcobj):
    sm = staticmethod(funcobj)
    sm.__isabstractmethod__ = True
    return sm


__all__ = [
    "ABCMeta",
    "ABC",
    "abstractmethod",
    "abstractproperty",
    "abstractclassmethod",
    "abstractstaticmethod",
]
