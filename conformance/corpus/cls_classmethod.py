class C:
    count = 0

    @classmethod
    def tick(cls):
        cls.count += 1
        return cls.count

    @staticmethod
    def square(n):
        return n * n


print(C.tick())
print(C.tick())
print(C.square(5))
