try:
    raise ValueError("boom")
except ValueError as e:
    print(e.args[0])

try:
    raise KeyError("k")
except (ValueError, KeyError) as e:
    print("caught", e.args[0])
