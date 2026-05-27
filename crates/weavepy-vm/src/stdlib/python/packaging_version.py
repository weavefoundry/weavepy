"""``packaging.version`` — PEP 440 versioning."""

from _packaging import (
    InvalidVersion,
    Version,
    parse_version as parse,
)


VERSION_PATTERN = r"""
    v?
    (?:
        (?:(?P<epoch>[0-9]+)!)?
        (?P<release>[0-9]+(?:\.[0-9]+)*)
    )
"""


__all__ = ['Version', 'InvalidVersion', 'parse', 'VERSION_PATTERN']


# Maintain compatibility with packaging's `LegacyVersion` (removed in
# 22.0) by aliasing to Version — the surface remained but the parser
# now treats non-PEP-440 input as InvalidVersion.
LegacyVersion = Version
