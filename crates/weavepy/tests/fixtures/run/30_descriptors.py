# Descriptor protocol: property, classmethod, staticmethod, plus
# user-defined data descriptors with __get__ / __set__ / __delete__.


class Celsius:
    """Property-based temperature unit, kept clamped to a sane range."""

    def __init__(self, c):
        self._c = c

    @property
    def value(self):
        return self._c

    @value.setter
    def value(self, v):
        if v < -273.15:
            raise ValueError("below absolute zero")
        self._c = v

    @value.deleter
    def value(self):
        self._c = 0


c = Celsius(20)
print(c.value)
c.value = 100
print(c.value)
try:
    c.value = -400
except ValueError as e:
    print("rejected:", e)
del c.value
print(c.value)


class Counter:
    count = 0

    @classmethod
    def tick(cls):
        cls.count += 1
        return cls.count

    @staticmethod
    def square(n):
        return n * n


print(Counter.tick())
print(Counter.tick())
print(Counter.square(5))


class Positive:
    """Custom data descriptor enforcing strictly-positive ints."""

    def __set_name__(self, owner, name):
        self.name = "_" + name

    def __get__(self, instance, owner=None):
        if instance is None:
            return self
        return getattr(instance, self.name, 0)

    def __set__(self, instance, value):
        if value <= 0:
            raise ValueError("must be positive")
        setattr(instance, self.name, value)


class Box:
    width = Positive()
    height = Positive()

    def __init__(self, w, h):
        self.width = w
        self.height = h


b = Box(3, 4)
print(b.width, b.height)
try:
    b.width = 0
except ValueError as e:
    print("rejected:", e)
