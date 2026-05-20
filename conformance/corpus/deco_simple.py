def wrap(fn):
    def inner(name):
        return "[" + fn(name) + "]"

    return inner


@wrap
def greet(name):
    return "hi " + name


print(greet("Owen"))
