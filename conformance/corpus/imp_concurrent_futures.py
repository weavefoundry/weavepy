from concurrent.futures import ThreadPoolExecutor, as_completed


def square(x):
    return x * x


def main():
    with ThreadPoolExecutor(max_workers=2) as ex:
        futures = [ex.submit(square, i) for i in range(4)]
        results = []
        for f in as_completed(futures):
            results.append(f.result())
        print(sorted(results))
        print(list(ex.map(square, [1, 2, 3])))


main()
