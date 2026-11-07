"""`str % dict` keeps CPython's results in compiled code where one bare
`%r` or `%s` consumes the dict itself: keys and values of each plain kind,
an empty dict, an escaped `%%`, a dict holding a value that renders
through Python code, and the errors a second conversion or `%d` raise."""


class R:
    def __repr__(self):
        return "R!"


def fmt(t, d):
    return t % d


cases = [
    ("error: %r", {"i": 1}),
    ("%s", {"a": 1.5, 2: None, True: "x"}),
    ("%r%% done", {}),
    ("<%r>", {"q": "it's " + chr(34) + "x" + chr(34)}),
    ("%r", {"r": R()}),
]
for _ in range(3000):
    for t, d in cases:
        assert fmt(t, d) == t % d
for t in ("%r %r", "%d"):
    try:
        fmt(t, {"a": 1})
    except TypeError:
        pass
    else:
        raise AssertionError(t)
print("ok")
