"""Ecosystem probe: typing_extensions — Protocol/TypedDict runtime checks."""

import typing_extensions as tx


@tx.runtime_checkable
class Quacks(tx.Protocol):
    def quack(self) -> str: ...


class Duck:
    def quack(self) -> str:
        return "quack"


class Rock:
    pass


assert isinstance(Duck(), Quacks)
assert not isinstance(Rock(), Quacks)


class Movie(tx.TypedDict):
    title: str
    year: tx.NotRequired[int]


m: Movie = {"title": "Metropolis"}
assert set(Movie.__required_keys__) == {"title"}
assert set(Movie.__optional_keys__) == {"year"}

# Literal / get_args
lit = tx.Literal["a", "b"]
assert tx.get_args(lit) == ("a", "b")

# Self and override exist and are usable in annotations
class Builder:
    @tx.override
    def __str__(self) -> str:
        return "builder"

    def chain(self) -> tx.Self:
        return self


b = Builder()
assert b.chain() is b
assert str(b) == "builder"

# deprecated decorator wraps callables
@tx.deprecated("use new_fn")
def old_fn() -> int:
    return 7


import warnings

with warnings.catch_warnings(record=True) as caught:
    warnings.simplefilter("always")
    assert old_fn() == 7
assert any(issubclass(w.category, DeprecationWarning) for w in caught)

print("typing_extensions ok")
