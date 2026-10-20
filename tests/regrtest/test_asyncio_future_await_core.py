"""`await fut` of a native asyncio future in the core loop: `GET_AWAITABLE`
makes the `FutureIter` in place, and `SEND` steps it without a
`StopIteration` for a future that finished with a result (see
`asyncio_mod::future_iter_send` in the VM). Exceptions, cancellation and
anything else the iterator owes still go through the general path. Each
case runs warm and checks the results asyncio documents.
"""

import asyncio
import unittest

WARM = 500


class MyFuture(asyncio.Future):
    pass


class OwnAwait(asyncio.Future):
    def __await__(self):
        return (yield "custom")


class FutureAwaitCoreTests(unittest.TestCase):
    def run_main(self, coro):
        return asyncio.run(coro)

    def test_results_warm(self):
        async def main():
            loop = asyncio.get_running_loop()
            total = 0
            for i in range(WARM):
                fut = loop.create_future()
                loop.call_soon(fut.set_result, i)
                total += await fut
            done = loop.create_future()
            done.set_result(7)
            return total, await done, await done

        self.assertEqual(self.run_main(main()), (sum(range(WARM)), 7, 7))

    def test_subclass_warm(self):
        async def main():
            loop = asyncio.get_running_loop()
            out = []
            for i in range(WARM):
                fut = MyFuture(loop=loop)
                loop.call_soon(fut.set_result, (i, "x"))
                out.append(await fut)
            return out[-1], len(out)

        self.assertEqual(self.run_main(main()), ((WARM - 1, "x"), WARM))

    def test_exception_and_cancel(self):
        async def main():
            loop = asyncio.get_running_loop()
            seen = []
            for i in range(WARM // 10):
                fut = loop.create_future()
                loop.call_soon(fut.set_exception, KeyError(i))
                try:
                    await fut
                except KeyError as e:
                    seen.append(e.args[0])
                fut = loop.create_future()
                loop.call_soon(fut.cancel)
                try:
                    await fut
                except asyncio.CancelledError:
                    seen.append("cancelled")
            return seen

        seen = self.run_main(main())
        self.assertEqual(seen[:4], [0, "cancelled", 1, "cancelled"])
        self.assertEqual(len(seen), 2 * (WARM // 10))

    def test_own_await(self):
        def drive(coro):
            return coro.send(None)

        async def custom():
            return await OwnAwait(loop=asyncio.new_event_loop())

        self.assertEqual(drive(custom()), "custom")

    def test_gather_and_tasks(self):
        async def leaf(i):
            await asyncio.sleep(0)
            return i

        async def main():
            total = 0
            for _ in range(WARM // 10):
                res = await asyncio.gather(*[leaf(i) for i in range(5)])
                total += sum(res)
                t = asyncio.ensure_future(leaf(10))
                total += await t
            return total

        self.assertEqual(self.run_main(main()), (10 + 10) * (WARM // 10))


if __name__ == "__main__":
    unittest.main()
