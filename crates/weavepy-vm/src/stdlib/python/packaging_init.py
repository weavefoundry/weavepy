"""``packaging`` — third-party-shaped facade over :mod:`_packaging`.

The PyPA ``packaging`` project (BSD-licensed, ~7K LOC) is the de
facto standard implementation of PEP 440/503/508/425. We don't
vendor it; instead we re-export our in-tree :mod:`_packaging`
primitives under the same submodule layout so user code that does
``from packaging.version import Version`` works unchanged.
"""

from _packaging import (
    InvalidMarker,
    InvalidRequirement,
    InvalidSpecifier,
    InvalidVersion,
    Marker,
    Requirement,
    Specifier,
    SpecifierSet,
    Version,
    WheelTag,
    canonicalize_name,
    compatible_tags,
    default_environment,
    parse_version,
    parse_wheel_filename,
    wheel_is_compatible,
    wheel_score,
)


__version__ = '24.0+weavepy'

__all__ = [
    '__version__',
    'Version', 'InvalidVersion', 'parse_version',
    'SpecifierSet', 'Specifier', 'InvalidSpecifier',
    'Requirement', 'InvalidRequirement',
    'Marker', 'InvalidMarker',
    'WheelTag', 'parse_wheel_filename', 'compatible_tags',
    'wheel_is_compatible', 'wheel_score',
    'canonicalize_name', 'default_environment',
]
