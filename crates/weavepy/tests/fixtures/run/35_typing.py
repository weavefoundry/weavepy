from typing import (
    Any,
    Optional,
    Union,
    List,
    Dict,
    Tuple,
    TypeVar,
    Generic,
    Protocol,
    runtime_checkable,
    get_origin,
    get_args,
    cast,
)


print(Optional[int])
print(Union[int, str])
print(List[int])
print(Dict[str, int])
print(Tuple[int, str, float])

print(get_origin(List[int]))
print(get_args(Dict[str, int]))


T = TypeVar("T")


class Container(Generic[T]):
    def __init__(self, item):
        self.item = item

    def get(self):
        return self.item


c = Container[int](42)
print(c.get())


@runtime_checkable
class HasName(Protocol):
    def name(self):
        ...


class Foo:
    def name(self):
        return "foo"


class Bar:
    pass


print(isinstance(Foo(), HasName))
print(isinstance(Bar(), HasName))


print(cast(int, 5))
print(Any)
