"""Helper module imported by 19_user_module.py."""

GREETING = "hello from _calc"


def add(a, b):
    return a + b


def square(x):
    return x * x


class Counter:
    def __init__(self, start):
        self.value = start

    def bump(self):
        self.value += 1
        return self
