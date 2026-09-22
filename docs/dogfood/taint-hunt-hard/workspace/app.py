import sqlite3

from flask import Flask, request

import handlers

app = Flask(__name__)


@app.route("/search")
def search():
    term = request.args.get("term")
    rows = handlers.run_search(term)
    return str(rows)


@app.route("/audit/ping")
def audit_ping():
    trace = request.headers.get("X-Trace-Id")
    handlers.log_request(trace)
    return "ok"


@app.route("/profile")
def profile():
    name = request.args.get("name")
    row = handlers.safe_lookup(name)
    return str(row)


@app.route("/limit")
def limit():
    n = request.args.get("n")
    rows = handlers.top_n(n)
    return str(rows)


@app.route("/sort")
def sort_users():
    col = request.args.get("col")
    rows = handlers.list_users(col)
    return str(rows)