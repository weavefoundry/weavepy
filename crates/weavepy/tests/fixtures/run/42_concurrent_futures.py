# `concurrent.futures` — synchronous Executor + Future API.

from concurrent.futures import (
    Future,
    ThreadPoolExecutor,
    ProcessPoolExecutor,
    as_completed,
    wait,
    ALL_COMPLETED,
)


def square(x):
    return x * x


def boom(x):
    raise ValueError(f"boom {x}")


with ThreadPoolExecutor(max_workers=4) as ex:
    futures = [ex.submit(square, i) for i in range(5)]
    print(sorted(f.result() for f in futures))
    print(list(ex.map(square, [1, 2, 3])))


# Future state machine
f = Future()
print("pending:", not f.done())
f.set_result(42)
print("done:", f.done(), "value:", f.result())


# Exception propagation through submit
with ThreadPoolExecutor() as ex:
    f = ex.submit(boom, 7)
    try:
        f.result()
    except ValueError as e:
        print("caught:", e)


# as_completed + wait
with ThreadPoolExecutor() as ex:
    fs = [ex.submit(square, i) for i in range(3)]
    done = list(as_completed(fs))
    print("as_completed:", [f.result() for f in done])

with ThreadPoolExecutor() as ex:
    fs = [ex.submit(square, i) for i in range(3)]
    result = wait(fs, return_when=ALL_COMPLETED)
    print("wait done:", len(result.done), "not done:", len(result.not_done))


# ProcessPoolExecutor behaves the same way in our single-process model.
with ProcessPoolExecutor() as ex:
    print(ex.submit(square, 9).result())
