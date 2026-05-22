import sqlite3

conn = sqlite3.connect(":memory:")
cur = conn.cursor()
cur.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
cur.executemany(
    "INSERT INTO t(name, age) VALUES (?, ?)",
    [("Alice", 30), ("Bob", 25), ("Charlie", 35)],
)
cur.execute("SELECT name, age FROM t ORDER BY age")
print("rows:", cur.fetchall())

cur.execute("SELECT COUNT(*) FROM t")
print("count:", cur.fetchone())

cur.execute("INSERT INTO t(name, age) VALUES (?, ?)", ("Dora", 28))
print("lastrowid:", cur.lastrowid)
print("rowcount:", cur.rowcount)

# Use Connection.execute shortcut.
cur2 = conn.execute("SELECT name FROM t WHERE age > ? ORDER BY name", (27,))
print("> 27:", [row[0] for row in cur2.fetchall()])

conn.close()
