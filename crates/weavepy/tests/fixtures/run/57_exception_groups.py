eg = ExceptionGroup("boom", [ValueError("v"), TypeError("t"), KeyError("k")])
print(type(eg).__name__)
print(eg.message)
print([type(e).__name__ for e in eg.exceptions])


def split_value():
    try:
        raise ExceptionGroup("inner", [ValueError("a"), TypeError("b")])
    except* ValueError as group:
        print("caught V:", [str(e) for e in group.exceptions])
    except* TypeError as group:
        print("caught T:", [str(e) for e in group.exceptions])


split_value()


def partial_split():
    try:
        raise ExceptionGroup("multi", [ValueError("v1"), ValueError("v2"), KeyError("k")])
    except* ValueError as group:
        print("V count:", len(group.exceptions))


try:
    partial_split()
except ExceptionGroup as exc:
    print("remaining:", [type(e).__name__ for e in exc.exceptions])


class MyError(Exception):
    pass


try:
    raise BaseExceptionGroup("base", [SystemExit("a"), MyError("b")])
except* MyError as g:
    print("got MyError:", [str(e) for e in g.exceptions])
except* SystemExit as g:
    print("got SystemExit:", [str(e) for e in g.exceptions])
