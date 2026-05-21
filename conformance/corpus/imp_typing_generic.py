from typing import TypeVar, Generic


T = TypeVar("T")


class Container(Generic[T]):
    def __init__(self, item):
        self.item = item

    def get(self):
        return self.item


c = Container[int](42)
print(c.get())
