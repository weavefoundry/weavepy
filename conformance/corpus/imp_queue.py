import queue


def main():
    q = queue.Queue()
    for i in range(3):
        q.put(i)
    print("qsize", q.qsize())
    while not q.empty():
        print(q.get())
    pq = queue.PriorityQueue()
    pq.put(3)
    pq.put(1)
    pq.put(2)
    print(pq.get(), pq.get(), pq.get())
    lq = queue.LifoQueue()
    lq.put("a")
    lq.put("b")
    lq.put("c")
    print(lq.get(), lq.get(), lq.get())


main()
