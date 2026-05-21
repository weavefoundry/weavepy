class Ctx:
    def __init__(self, name):
        self.name = name

    async def __aenter__(self):
        print(f"enter {self.name}")
        return self

    async def __aexit__(self, et, ev, tb):
        print(f"exit {self.name}")
        return False


async def run():
    async with Ctx("a"):
        async with Ctx("b"):
            print("body")
    return "done"


c = run()
try:
    while True:
        c.send(None)
except StopIteration as e:
    print(e.value)
