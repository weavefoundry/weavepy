"""The opaque `PyCapsule` type (`types.CapsuleType`).

CPython reaches it through the C `_types` module; WeavePy has no C
capsules, so this frozen helper hosts the single stand-in type. It is
imported by both `types` and the `_datetime` accelerator alias (for
`datetime_CAPI`), so `import types` does not depend on `_datetime` (a
harness that blocks `_datetime` -- test_zoneinfo's `test_pydatetime` --
must still be able to import `enum`/`types`).
"""


class PyCapsule:
    """Stand-in for CPython's opaque `PyCapsule`. Not instantiable, like
    the real one."""

    __module__ = 'builtins'

    _capsule_name = "capsule"

    def __new__(cls, *args, **kwargs):
        raise TypeError("cannot create 'PyCapsule' instances")

    def __repr__(self):
        return '<capsule object "%s" at %#x>' % (self._capsule_name, id(self))


def new_capsule(name):
    obj = object.__new__(PyCapsule)
    object.__setattr__(obj, "_capsule_name", name)
    return obj
