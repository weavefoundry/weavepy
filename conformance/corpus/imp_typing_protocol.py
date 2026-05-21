from typing import Protocol, runtime_checkable


@runtime_checkable
class HasName(Protocol):
    def name(self): ...


class Foo:
    def name(self):
        return "foo"


print(isinstance(Foo(), HasName))
