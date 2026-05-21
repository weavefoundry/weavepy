class Meta(type):
    def __init__(cls, name, bases, namespace, **kwargs):
        super().__init__(name, bases, namespace, **kwargs)
        cls.created = True


class A(metaclass=Meta):
    pass


print(A.created)
print(type(A).__name__)
