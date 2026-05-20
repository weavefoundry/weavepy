class V:
    def __init__(self, n):
        self.n = n

    def __add__(self, other):
        return V(self.n + other.n)

    def __repr__(self):
        return "V(" + str(self.n) + ")"


print(V(2) + V(3))
