import db


def run_search(term):
    cleaned = term.strip()
    return db.fetch_by_name(cleaned)


def log_request(value):
    entry = {"raw": value}
    db.write_audit(entry["raw"])


def safe_lookup(name):
    return db.lookup_param(name)


def top_n(n):
    count = int(n)
    return db.top_rows(count)


def list_users(col):
    if col not in ("name", "email"):
        col = "name"
    return db.ordered_users(col)