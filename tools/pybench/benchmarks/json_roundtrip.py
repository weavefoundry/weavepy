"""json.dumps and json.loads of a realistic API-shaped document, with
indent and sort_keys variants."""

import json

WORK = 150


def make():
    users = []
    for i in range(60):
        users.append({
            "id": i,
            "name": "User %d" % i,
            "email": "user%d@example.com" % i,
            "active": i % 3 != 0,
            "score": i * 1.25,
            "roles": ["admin", "dev"] if i % 10 == 0 else ["dev"],
            "address": {"city": "Springfield", "zip": "%05d" % (i * 37), "geo": [12.5 + i, -71.25]},
            "bio": "Line one\nLine \"two\" é中",
            "manager": None,
        })
    return {"users": users, "count": len(users), "next": None, "version": "1.0"}


def bench(n):
    doc = make()
    total = 0
    for i in range(n):
        s = json.dumps(doc)
        total += len(s)
        back = json.loads(s)
        total += back["count"]
        if i % 10 == 0:
            total += len(json.dumps(doc, indent=2, sort_keys=True))
    return total
