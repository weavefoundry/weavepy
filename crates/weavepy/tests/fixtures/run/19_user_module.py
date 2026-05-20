import _calc
from _calc import add, square

print(_calc.GREETING)
print(add(2, 3))
print(square(4))
print(_calc.Counter(10).bump().bump().value)
