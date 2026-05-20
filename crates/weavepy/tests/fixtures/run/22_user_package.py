import _pkg
from _pkg import VERSION
from _pkg.core import greet, GREETING

print(GREETING)
print(_pkg.VERSION)
print(VERSION)
print(greet("WeavePy"))
print(_pkg.core.GREETING == GREETING)
