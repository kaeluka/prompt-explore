import sqlite3

CONN = sqlite3.connect("app.db")


def lookup(v):
    sql = "SELECT * FROM users WHERE name = '%s'" % v
    cur = CONN.cursor()
    cur.execute(sql)
    return cur.fetchall()


def write(v):
    cur = CONN.cursor()
    cur.executescript("INSERT INTO audit(note) VALUES('%s')" % v)


def render(v):
    cur = CONN.cursor()
    cur.execute("SELECT * FROM pages WHERE slug = ?", (v,))
    return cur.fetchall()


def purge():
    cur = CONN.cursor()
    cur.executescript("DELETE FROM sessions")