"""``unittest.async_case`` — ``IsolatedAsyncioTestCase``.

Lets ``async def test_*`` methods run from a regular
``unittest`` runner. We provide a real (per-test) event loop and
honour ``asyncSetUp`` / ``asyncTearDown``.
"""

import asyncio
import inspect
import unittest


__all__ = ['IsolatedAsyncioTestCase']


class IsolatedAsyncioTestCase(unittest.TestCase):
    """``TestCase`` that runs async coroutines per test method."""

    def asyncSetUp(self):
        pass

    def asyncTearDown(self):
        pass

    async def _setUp_async(self):
        result = self.asyncSetUp()
        if inspect.iscoroutine(result):
            await result

    async def _tearDown_async(self):
        result = self.asyncTearDown()
        if inspect.iscoroutine(result):
            await result

    def _callTestMethod(self, method):
        if inspect.iscoroutinefunction(method):
            self._asyncioRunner = _Runner()
            self._asyncioRunner.run(self._async_test(method))
        else:
            method()

    async def _async_test(self, method):
        await self._setUp_async()
        try:
            result = method()
            if inspect.iscoroutine(result):
                await result
        finally:
            await self._tearDown_async()


class _Runner:
    """Thin wrapper around ``asyncio.run`` that doesn't blow up if
    a loop is already running (we just nest a fresh one)."""

    def run(self, coro):
        try:
            return asyncio.run(coro)
        except RuntimeError:
            loop = asyncio.new_event_loop()
            try:
                return loop.run_until_complete(coro)
            finally:
                loop.close()
