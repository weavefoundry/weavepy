class Ctx:
    def __init__(self, name):
        self.name = name

    def __enter__(self):
        print("enter " + self.name)
        return self.name

    def __exit__(self, exc_type, exc, tb):
        print("exit " + self.name)


with Ctx("outer") as a:
    print("body " + a)

# Nested with statements run __exit__ in reverse order.
with Ctx("A") as a:
    with Ctx("B") as b:
        print("nested " + a + " " + b)
