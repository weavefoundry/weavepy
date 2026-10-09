"""Dijkstra with heapq, BFS with deque, and topological sort over a grid
graph stored in dicts of lists."""

import heapq
from collections import deque

WORK = 120


def make_graph(size):
    g = {}
    for y in range(size):
        for x in range(size):
            node = (x, y)
            edges = []
            for dx, dy in ((1, 0), (0, 1), (-1, 0), (0, -1)):
                nx, ny = x + dx, y + dy
                if 0 <= nx < size and 0 <= ny < size:
                    edges.append(((nx, ny), 1 + (x * 7 + y * 13 + dx * 3) % 9))
            g[node] = edges
    return g


def dijkstra(g, src):
    dist = {src: 0}
    heap = [(0, src)]
    while heap:
        d, u = heapq.heappop(heap)
        if d > dist.get(u, 1 << 60):
            continue
        for v, w in g[u]:
            nd = d + w
            if nd < dist.get(v, 1 << 60):
                dist[v] = nd
                heapq.heappush(heap, (nd, v))
    return dist


def bfs(g, src):
    seen = {src}
    q = deque([src])
    order = 0
    while q:
        u = q.popleft()
        order += 1
        for v, _ in g[u]:
            if v not in seen:
                seen.add(v)
                q.append(v)
    return order


def bench(n):
    g = make_graph(n)
    dist = dijkstra(g, (0, 0))
    total = dist[(n - 1, n - 1)] + bfs(g, (n // 2, n // 2))
    total += sum(dijkstra(g, (n - 1, 0)).values()) % 100003
    return total
