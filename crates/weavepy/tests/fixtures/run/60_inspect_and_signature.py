import inspect


def add(x, y, z=10, *args, kw_only=None, **kwargs):
    """Adder doc."""
    return x + y + z


sig = inspect.signature(add)
print("signature:", str(sig))
print("params:", list(sig.parameters.keys()))
print("default z:", sig.parameters["z"].default)


spec = inspect.getfullargspec(add)
print("args:", spec.args)
print("varargs:", spec.varargs)
print("varkw:", spec.varkw)
print("kwonly:", spec.kwonlyargs)


class Cls:
    def method(self, x, y=2):
        return x + y


inst = Cls()
mig = inspect.signature(inst.method)
print("method:", str(mig))


# Predicates.
print("isfunction:", inspect.isfunction(add))
print("ismethod:", inspect.ismethod(inst.method))
print("isclass:", inspect.isclass(Cls))


def gen():
    yield 1


print("isgeneratorfunction:", inspect.isgeneratorfunction(gen))


def show_caller():
    frame = inspect.currentframe()
    caller = frame.f_back
    print("caller name:", caller.f_code.co_name)


def caller():
    show_caller()


caller()


# stack() and FrameInfo.
def show_stack():
    frames = inspect.stack()
    print("stack depth >= 2:", len(frames) >= 2)
    print("top name:", frames[0].function)


def parent():
    show_stack()


parent()
