class Base:
    def __init_subclass__(cls, **kwargs):
        super().__init_subclass__(**kwargs)
        cls.registered = True


class Child(Base):
    pass


print(Child.registered)
