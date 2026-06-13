"""Keywords (from "Grammar/python.gram")

This file is intentionally a verbatim port of CPython 3.13's
auto-generated ``Lib/keyword.py``: the literal keyword and
soft-keyword lists plus the two membership predicates. Modules such
as ``dataclasses`` import ``keyword.iskeyword`` to validate field
names.
"""

__all__ = ["iskeyword", "issoftkeyword", "kwlist", "softkwlist"]

kwlist = [
    'False',
    'None',
    'True',
    'and',
    'as',
    'assert',
    'async',
    'await',
    'break',
    'class',
    'continue',
    'def',
    'del',
    'elif',
    'else',
    'except',
    'finally',
    'for',
    'from',
    'global',
    'if',
    'import',
    'in',
    'is',
    'lambda',
    'nonlocal',
    'not',
    'or',
    'pass',
    'raise',
    'return',
    'try',
    'while',
    'with',
    'yield'
]

softkwlist = [
    '_',
    'case',
    'match',
    'type'
]

iskeyword = frozenset(kwlist).__contains__
issoftkeyword = frozenset(softkwlist).__contains__
