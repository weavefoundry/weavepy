import threading


def worker(out):
    out.append("worked")


def main():
    out = []
    t = threading.Thread(target=worker, args=(out,))
    t.start()
    t.join()
    print(out)
    lock = threading.Lock()
    with lock:
        print("locked")
    print("locked", lock.locked())
    ev = threading.Event()
    print("event", ev.is_set())
    ev.set()
    print("event", ev.is_set())


main()
