import sqlite3

CONN = sqlite3.connect("app.db")


def search(q):
    sql = "SELECT * FROM docs WHERE body LIKE '%%%s%%'" % q
    cur = CONN.cursor()
    cur.execute(sql)
    return cur.fetchall()


def by_id(uid):
    cur = CONN.cursor()
    cur.execute("SELECT * FROM users WHERE id = ?", (int(uid),))
    return cur.fetchall()


def by_slug(slug):
    cur = CONN.cursor()
    cur.execute("SELECT * FROM pages WHERE slug = ?", (slug,))
    return cur.fetchall()


def sorted(col):
    allowed = ("name", "email")
    column = col if col in allowed else "name"
    cur = CONN.cursor()
    cur.execute("SELECT * FROM users ORDER BY " + column)
    return cur.fetchall()