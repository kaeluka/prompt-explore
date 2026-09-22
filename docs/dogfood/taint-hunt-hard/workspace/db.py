import sqlite3

CONN = sqlite3.connect("app.db")


def fetch_by_name(name):
    sql = "SELECT id, name FROM users WHERE name = '%s'" % name
    cur = CONN.cursor()
    cur.execute(sql)
    return cur.fetchall()


def write_audit(msg):
    cur = CONN.cursor()
    cur.executescript(f"INSERT INTO audit(note) VALUES('{msg}')")


def lookup_param(name):
    cur = CONN.cursor()
    cur.execute("SELECT id, name FROM users WHERE name = ?", (name,))
    return cur.fetchall()


def top_rows(count):
    cur = CONN.cursor()
    cur.execute(f"SELECT id, name FROM users LIMIT {count}")
    return cur.fetchall()


def ordered_users(col):
    cur = CONN.cursor()
    cur.execute("SELECT id, name FROM users ORDER BY %s" % col)
    return cur.fetchall()


def purge_sessions():
    cur = CONN.cursor()
    cur.executescript("DELETE FROM sessions")