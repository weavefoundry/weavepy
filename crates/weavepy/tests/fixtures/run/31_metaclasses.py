# Metaclasses: custom metaclass, __init_subclass__, __set_name__,
# __class_getitem__.


class Registry(type):
    """Collect every concrete subclass that uses us as its metaclass."""

    instances = []

    def __init__(cls, name, bases, namespace, **kwargs):
        super().__init__(name, bases, namespace, **kwargs)
        Registry.instances.append(cls.__name__)


class A(metaclass=Registry):
    pass


class B(A):
    pass


class C(A):
    pass


print(sorted(Registry.instances))


class Base:
    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        cls.subclass_count = getattr(Base, "_count", 0) + 1
        Base._count = cls.subclass_count


class Child1(Base):
    pass


class Child2(Base):
    pass


print(Child1.subclass_count, Child2.subclass_count)


class Tagged:
    def __set_name__(self, owner, name):
        self.owner_name = owner.__name__
        self.attr_name = name


class Holder:
    tag = Tagged()


print(Holder.tag.owner_name, Holder.tag.attr_name)


class GenericLike:
    """Mimic `MyClass[int]` syntax via __class_getitem__."""

    @classmethod
    def __class_getitem__(cls, item):
        return (cls.__name__, item.__name__)


print(GenericLike[int])
print(GenericLike[str])
