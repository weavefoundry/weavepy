class Vec:
    def __init__(self, x, y):
        self.x = x
        self.y = y

    def __add__(self, other):
        return Vec(self.x + other.x, self.y + other.y)

    def __eq__(self, other):
        return self.x == other.x and self.y == other.y

    def __repr__(self):
        return "Vec(" + str(self.x) + ", " + str(self.y) + ")"

    def __len__(self):
        return 2


a = Vec(1, 2)
b = Vec(3, 4)
print(a + b)
print(a == Vec(1, 2))
print(a == b)
print(len(a))
