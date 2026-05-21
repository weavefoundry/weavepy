from abc import ABC, ABCMeta, abstractmethod


class Shape(ABC):
    @abstractmethod
    def area(self):
        pass

    @abstractmethod
    def name(self):
        pass


try:
    Shape()
except TypeError as e:
    print("abstract reject:", str(e)[:40])


class Square(Shape):
    def __init__(self, side):
        self.side = side

    def area(self):
        return self.side * self.side

    def name(self):
        return "square"


s = Square(4)
print(s.name(), s.area())


class PartialShape(Shape):
    def area(self):
        return 0


try:
    PartialShape()
except TypeError as e:
    print("partial reject:", str(e)[:40])


class IContainer(metaclass=ABCMeta):
    pass


class MyList:
    pass


IContainer.register(MyList)
print(isinstance(MyList(), IContainer))
print(issubclass(MyList, IContainer))


class StillNot:
    pass


print(isinstance(StillNot(), IContainer))
