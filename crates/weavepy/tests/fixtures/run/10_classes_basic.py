class Greeter:
    def __init__(self, name):
        self.name = name

    def hello(self):
        return "hello, " + self.name


g = Greeter("Owen")
print(g.hello())
print(g.name)
