import sqlite3

CONN = sqlite3.connect("app.db")


def by_like(q):
    sql = "SELECT * FROM docs WHERE body LIKE '%%%s%%'" % q
    cur = CONN.cursor()
    cur.execute(sql)
    return cur.fetchall()


def by_exact(term):
    cur = CONN.cursor()
    cur.execute("SELECT * FROM docs WHERE body = ?", (term,))
    return cur.fetchall()