import sys


def outer():
    x = 1
    y = "hello"
    return inner()


def inner():
    frame = sys._getframe()
    print("self:", frame.f_code.co_name)
    print("parent:", frame.f_back.f_code.co_name)
    locs = frame.f_back.f_locals
    print("vars:", sorted(locs.keys()))
    print("values:", locs["x"], locs["y"])
    return None


outer()


def trace_self():
    frame = sys._getframe(0)
    parent = frame.f_back
    print("self name:", frame.f_code.co_name)
    print("parent name:", parent.f_code.co_name if parent is not None else None)


trace_self()


def show_lineno():
    line = sys._getframe().f_lineno
    print("lineno_positive:", line > 0)
    print("filename_set:", sys._getframe().f_code.co_filename != "")


show_lineno()
