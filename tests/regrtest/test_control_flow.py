"""Smoke test: if/for/while/try/break/continue/with."""

x = 5
if x > 0:
    sign = "positive"
elif x < 0:
    sign = "negative"
else:
    sign = "zero"
assert sign == "positive"

acc = 0
for i in range(10):
    if i == 5:
        break
    acc += i
assert acc == 0 + 1 + 2 + 3 + 4

acc = 0
for i in range(5):
    if i == 2:
        continue
    acc += i
assert acc == 0 + 1 + 3 + 4

n = 10
fib = []
a, b = 0, 1
while a < n:
    fib.append(a)
    a, b = b, a + b
assert fib == [0, 1, 1, 2, 3, 5, 8]

# try/except/else/finally
seen = []
try:
    seen.append("try")
    1 / 0
except ZeroDivisionError as e:
    seen.append("except:" + type(e).__name__)
else:
    seen.append("else")
finally:
    seen.append("finally")
assert seen == ["try", "except:ZeroDivisionError", "finally"]

# raise + chain
try:
    try:
        raise ValueError("inner")
    except ValueError as e:
        raise RuntimeError("outer") from e
except RuntimeError as e:
    assert isinstance(e.__cause__, ValueError)
    assert str(e.__cause__) == "inner"

# with-statement
import io
buf = io.StringIO()
with buf as fp:
    fp.write("hello")
assert buf.getvalue() == "hello"

# nested loops + else
found = None
for i in range(5):
    for j in range(5):
        if i * j == 12:
            found = (i, j)
            break
    else:
        continue
    break
assert found == (3, 4)

# nested function-local control flow
def looped(n):
    out = []
    for i in range(n):
        for j in range(n):
            if j > i:
                break
            out.append((i, j))
    return out


assert looped(3) == [(0, 0), (1, 0), (1, 1), (2, 0), (2, 1), (2, 2)]
