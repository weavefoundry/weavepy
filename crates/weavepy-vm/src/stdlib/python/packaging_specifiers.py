"""``packaging.specifiers`` — PEP 440 specifier sets."""

from _packaging import (
    InvalidSpecifier,
    Specifier,
    SpecifierSet,
)


__all__ = ['InvalidSpecifier', 'Specifier', 'SpecifierSet']


# packaging.specifiers also exports a LegacySpecifier (deprecated/removed).
LegacySpecifier = Specifier
