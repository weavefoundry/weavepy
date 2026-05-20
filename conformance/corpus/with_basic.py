class Ctx:
    def __init__(self, name):
        self.name = name

    def __enter__(self):
        print("enter", self.name)
        return self.name

    def __exit__(self, t, v, tb):
        print("exit", self.name)


with Ctx("a") as a:
    print("body", a)
