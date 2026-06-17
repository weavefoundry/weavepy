# weavepy-skip: windows
#
# asyncio is built on the Unix-only `select`/`selectors` backend right now
# (no IOCP / winsock `select(2)` adapter), so the event loop can't start on
# Windows (`ModuleNotFoundError: No module named '_overlapped'`). Unix-only
# until that backend lands.
#
# Async comprehensions — PEP 530. Exercises `await` inside list,
# set, dict, and generator comprehensions as well as the `async for`
# form. The await-in-elt variant is a regression guard for a closure
# bug where the outer scope failed to promote locals referenced only
# inside an `await` to cells.

import asyncio


async def double(x):
    return x * 2


async def gen():
    for i in range(4):
        yield i


async def main():
    factor = 10

    # `await` inside the element captures the outer `factor`.
    out = [await double(factor + i) for i in range(3)]
    print("list:", out)

    # Set-comp with await + outer closure.
    out = {await double(factor) for _ in range(3)}
    print("set:", sorted(out))

    # Dict-comp with await on both key and value.
    out = {i: await double(factor + i) for i in range(3)}
    print("dict:", out)

    # `async for` source.
    out = [v async for v in gen()]
    print("async-for:", out)

    # Combined async-for and await inside the elt.
    out = [await double(v + factor) async for v in gen()]
    print("combined:", out)


asyncio.run(main())
