class Positive:
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

    def __init__(self, w):
        self.width = w


b = Box(5)
print(b.width)
