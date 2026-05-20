try:
    try:
        raise ValueError("inner")
    except ValueError as inner:
        raise RuntimeError("outer") from inner
except RuntimeError as e:
    print(e.args[0])
