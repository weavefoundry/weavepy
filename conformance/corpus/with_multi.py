class Ctx:
    def __init__(self, n):
        self.n = n

    def __enter__(self):
        return self.n

    def __exit__(self, t, v, tb):
        return None


with Ctx("a") as a, Ctx("b") as b:
    print(a, b)
