class Squares:
    def __init__(self, n):
        self.n = n
        self.i = 0

    def __iter__(self):
        return self

    def __next__(self):
        if self.i >= self.n:
            raise StopIteration
        v = self.i
        self.i = v + 1
        return v * v


for x in Squares(5):
    print(x)


# A separate iterator each time.
class CountUp:
    def __init__(self, n):
        self.n = n

    def __iter__(self):
        return CountUpIter(self.n)


class CountUpIter:
    def __init__(self, n):
        self.n = n
        self.i = 0

    def __iter__(self):
        return self

    def __next__(self):
        if self.i >= self.n:
            raise StopIteration
        v = self.i
        self.i = v + 1
        return v


seq = CountUp(3)
for x in seq:
    print(x)
for x in seq:
    print("again", x)
