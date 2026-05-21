class Temp:
    def __init__(self, c):
        self._c = c

    @property
    def value(self):
        return self._c

    @value.setter
    def value(self, v):
        self._c = v


t = Temp(20)
print(t.value)
t.value = 50
print(t.value)
