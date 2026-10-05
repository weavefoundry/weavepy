# A constructor fast path that falls back to the generic call must not
# finalize the instance it allocated and then abandoned: Python never saw
# it, so its `__del__` would run on an object `__init__` never touched.

log = []
missing = []


class Finalized:
    def __init__(self, value):
        self.value = value

    def __del__(self):
        try:
            log.append(self.value)
        except AttributeError:
            missing.append(len(log))


class Holder:
    pass


def rebind(n):
    holder = Holder()
    holder.item = 0
    for i in range(n):
        holder.item = Finalized(i)


for unused in range(3):
    log.clear()
    rebind(3000)
    assert missing == [], f"{len(missing)} unborn instances finalized"
    # The last instance is still bound until `holder` dies with the frame.
    assert log == list(range(3000)), (len(log), log[:5])

print("constructor fallback finalization: ok")
