"""``packaging.markers`` — PEP 508 marker evaluation."""

from _packaging import (
    InvalidMarker,
    Marker,
    default_environment,
)


__all__ = ['InvalidMarker', 'Marker', 'default_environment']


class UndefinedComparison(Exception):
    """Raised when a marker compares incompatible types."""


class UndefinedEnvironmentName(Exception):
    """Raised on unknown environment names."""
