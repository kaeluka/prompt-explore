import sqlite3

CONN = sqlite3.connect("app.db")


def get_user(uid):
    cur = CONN.cursor()
    cur.execute("SELECT * FROM users WHERE id = ?", (int(uid),))
    return cur.fetchall()


def find_by_name(name):
    cur = CONN.cursor()
    cur.execute("SELECT * FROM users WHERE name = ?", (name,))
    return cur.fetchall()


def sorted(col):
    allowed = ("name", "email", "created_at")
    column = col if col in allowed else "name"
    cur = CONN.cursor()
    cur.execute("SELECT * FROM users ORDER BY " + column)
    return cur.fetchall()


def count_all():
    cur = CONN.cursor()
    cur.execute("SELECT COUNT(*) FROM users")
    return cur.fetchall()