"""tomllib.loads of a configuration-sized document (pure-Python parser)."""

import tomllib

WORK = 40

DOC = "\n".join(
    [
        'title = "Example config"',
        "[owner]",
        'name = "Tom Preston-Werner"',
        "dob = 1979-05-27T07:32:00-08:00",
        "[database]",
        "enabled = true",
        "ports = [ 8000, 8001, 8002 ]",
        "data = [ [\"delta\", \"phi\"], [3.14] ]",
        "temp_targets = { cpu = 79.5, case = 72.0 }",
    ]
    + [
        "[servers.s%d]\nip = \"10.0.0.%d\"\nrole = \"%s\"\nweight = %d\ntags = [\"a\", \"b\", \"c%d\"]\n"
        "limits = { cpu = %d.5, mem = \"%dGi\" }\nnotes = '''\nmulti\nline %d\n'''"
        % (i, i, "frontend" if i % 2 else "backend", i * 3, i, i, i, i)
        for i in range(40)
    ]
)


def bench(n):
    total = 0
    for _ in range(n):
        doc = tomllib.loads(DOC)
        total += len(doc["servers"]) + doc["database"]["ports"][2] + len(doc["servers"]["s7"]["notes"])
    return total
