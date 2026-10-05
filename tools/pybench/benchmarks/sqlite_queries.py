"""sqlite3 in memory: inserts with executemany, indexed queries, and
aggregates (Python-side row handling dominates)."""

import sqlite3

WORK = 30


def bench(n):
    total = 0
    for _ in range(n):
        con = sqlite3.connect(":memory:")
        cur = con.cursor()
        cur.execute("create table t (id integer primary key, name text, grp integer, val real)")
        cur.execute("create index t_grp on t(grp)")
        cur.executemany("insert into t (name, grp, val) values (?, ?, ?)",
                        (("n%d" % i, i % 17, i * 0.25) for i in range(1000)))
        con.commit()
        for g in range(17):
            for row in cur.execute("select id, name, val from t where grp = ? order by val", (g,)):
                total += row[0] + len(row[1])
        total += int(cur.execute("select sum(val) from t").fetchone()[0])
        total += len(cur.execute("select grp, count(*) from t group by grp").fetchall())
        con.close()
    return total
