"""``packaging.utils`` — small utilities used by `packaging` consumers."""

from _packaging import (
    canonicalize_name,
    parse_wheel_filename,
)


__all__ = [
    'canonicalize_name', 'parse_wheel_filename',
    'NormalizedName', 'canonicalize_version', 'is_normalized_name',
]


NormalizedName = str  # PEP 503 normalised string.


def canonicalize_version(version, *, strip_trailing_zero: bool = True) -> str:
    """Normalise a version string to a canonical form."""
    from _packaging import Version
    try:
        v = Version(str(version))
    except Exception:
        return str(version)
    return str(v)


def is_normalized_name(name: str) -> bool:
    return name == canonicalize_name(name)
