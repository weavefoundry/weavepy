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


for x in Squares(4):
    print(x)
