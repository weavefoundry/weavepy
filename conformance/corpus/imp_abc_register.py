from abc import ABCMeta


class IContainer(metaclass=ABCMeta):
    pass


class MyList:
    pass


IContainer.register(MyList)
print(isinstance(MyList(), IContainer))
print(issubclass(MyList, IContainer))
