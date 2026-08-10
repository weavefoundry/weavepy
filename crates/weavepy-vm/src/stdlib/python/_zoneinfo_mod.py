"""``_zoneinfo`` — the C-accelerator-shaped ``ZoneInfo``.

CPython implements this module in C (``Modules/_zoneinfo.c``). The
observable differences from the pure-Python class are all about caching,
and ``test_zoneinfo``'s ``CZoneInfoCacheTest`` / ``ExtensionBuiltTest``
assert them directly:

- the *base* class keeps its weak/strong caches in module state, so the
  type object has **no** ``_weak_cache`` / ``_strong_cache`` attributes;
- subclasses get a fresh ``_weak_cache`` from ``__init_subclass__`` (and
  only that — the strong cache exists solely for the base type), and the
  cache is re-read with ``getattr`` on every use so deleting or replacing
  it is honored (``test_deleted_weak_cache``);
- whatever a (possibly user-replaced) weak cache returns from ``get`` /
  ``setdefault`` is type-checked, and an imposter raises ``RuntimeError``
  with the C module's exact message (``test_inconsistent_weak_cache_*``).

The algorithmic core is shared with the pure class via
``zoneinfo._ZoneInfoBase``.
"""

import collections
import weakref

import zoneinfo as _zoneinfo_module

__all__ = ["ZoneInfo"]

_STRONG_CACHE_SIZE = 8

# Base-class caches: module state, exactly like the C accelerator's
# per-module `zoneinfo_state`.
_WEAK_CACHE = weakref.WeakValueDictionary()
_STRONG_CACHE = collections.OrderedDict()


def _check_cached(instance, cls, key):
    """Validate an object produced by the (replaceable) weak cache.

    Mirrors `_zoneinfo.c`'s guard: anything that is not a ZoneInfo is an
    "unexpected instance" and surfaces as RuntimeError, never a crash.
    """
    if not isinstance(instance, ZoneInfo):
        raise RuntimeError(
            f"Unexpected instance of {type(instance).__name__} in "
            f"{cls.__name__} weak cache for key {key!r}"
        )


class ZoneInfo(_zoneinfo_module._ZoneInfoBase):
    __module__ = "zoneinfo"

    def __init_subclass__(cls):
        # The C accelerator hands every subclass its own weak cache and
        # nothing else (the strong cache belongs to the base type only).
        cls._weak_cache = weakref.WeakValueDictionary()

    def __new__(cls, key):
        if cls is ZoneInfo:
            weak_cache = _WEAK_CACHE
            # Strong cache: base type only, checked first (C order).
            entry = _STRONG_CACHE.pop(key, None)
            if entry is not None:
                _STRONG_CACHE[key] = entry
                return entry
        else:
            # Re-resolved on every call: a deleted `_weak_cache` raises
            # AttributeError (test_deleted_weak_cache), a replaced one is
            # honored (test_inconsistent_weak_cache_*).
            weak_cache = cls._weak_cache

        instance = weak_cache.get(key, None)
        if instance is not None:
            _check_cached(instance, cls, key)
        else:
            instance = weak_cache.setdefault(key, cls._new_instance(key))
            _check_cached(instance, cls, key)
            instance._from_cache = True

        if cls is ZoneInfo:
            _STRONG_CACHE[key] = instance
            while len(_STRONG_CACHE) > _STRONG_CACHE_SIZE:
                try:
                    _STRONG_CACHE.popitem(last=False)
                except KeyError:
                    break

        return instance

    @classmethod
    def clear_cache(cls, *, only_keys=None):
        if cls is ZoneInfo:
            if only_keys is None:
                _WEAK_CACHE.clear()
                _STRONG_CACHE.clear()
            else:
                for key in only_keys:
                    _WEAK_CACHE.pop(key, None)
                    _STRONG_CACHE.pop(key, None)
        else:
            weak_cache = cls._weak_cache
            if only_keys is None:
                weak_cache.clear()
            else:
                for key in only_keys:
                    weak_cache.pop(key, None)


_zoneinfo_module._REPR_ROOTS.add(ZoneInfo)
