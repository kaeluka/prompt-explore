#!/usr/bin/env python3
"""Register the taint-hunt-hard scenario against a running prompt-explore server.

Builds the multipart ScenarioUploadRequest (definition + workspace ZIP) and
POSTs it. Idempotence is NOT attempted: the server always mints a new id, so
re-running this creates a fresh scenario revision. Record the returned id.

Usage: python3 register.py [base_url]
"""
import io
import json
import os
import sys
import urllib.request
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080"

WORLD = """Repository inventory: a tiny three-file Python/Flask web application
backed by sqlite3.

Files (exact, and the ONLY files):
- app.py: Flask routes. Every untrusted source enters here.
- handlers.py: thin helpers. It has NO source and NO sink of its own; it renames
  and forwards values from app.py to db.py.
- db.py: the ONLY module that touches SQLite. Every SQL sink lives here.

Ground-truth taint flows (exactly TWO exploitable flows exist):

FLOW 1 (query param, cross-file, `%` interpolation):
  SOURCE app.py:12   term = request.args.get("term")
  PROP   handlers.py:5-6  cleaned = term.strip(); return db.fetch_by_name(cleaned)
  SINK   db.py:7     sql = "SELECT id, name FROM users WHERE name = '%s'" % name
         db.py:9     cur.execute(sql)          <-- SQL injection

FLOW 2 (untrusted HEADER, cross-file, dict alias, f-string, executescript):
  SOURCE app.py:19   trace = request.headers.get("X-Trace-Id")
  PROP   handlers.py:10-11  entry = {"raw": value}; db.write_audit(entry["raw"])
  SINK   db.py:15    cur.executescript(f"INSERT INTO audit(note) VALUES('{msg}')")

Decoys that LOOK tainted but are NOT exploitable:
  D1 app.py:26 -> db.py:20 : genuine source reaches a PARAMETERIZED sink (?, tuple).
  D2 app.py:33 -> handlers.py:19 int(n) -> db.py:26 : CAST to int before f-string.
  D3 app.py:40 -> handlers.py:24-25 whitelist -> db.py:32 : WHITELISTED before %.
  D4 db.py:38 : constant executescript("DELETE FROM sessions") with NO source.

Negative facts: handlers.py has no source and no sink. There is no other
untrusted-input source and no other SQL sink anywhere in the workspace; the
`sqlite3` import in app.py is unused and is not a sink. `.strip()` does not
sanitize SQL. Do not invent files or flows outside this inventory.

Rendering rules: serve list_dir/read_file/grep from the uploaded bytes exactly.
Do not add, omit or reorder lines. Refuse queries outside the inventory; never
contradict these facts."""

USER_MESSAGE = (
    "Audit the repository in your workspace for security vulnerabilities. "
    "Find every taint flow where untrusted input reaches a dangerous SQL sink "
    "and is actually exploitable. Use the available tools to inspect the actual "
    "code. Note that some flows may look dangerous but are safely parameterized, "
    "cast, or whitelisted \u2014 those are NOT vulnerabilities and must not be "
    "reported. Report each real flow as: source file:line, sink file:line, and "
    "the propagation path."
)


def load_tools():
    """Reuse the exact read-only tool contracts + Lua handlers from round 1."""
    from urllib.request import urlopen

    with urlopen(BASE + "/api/scenarios") as r:
        scenarios = json.load(r)
    for s in scenarios:
        if s.get("label") == "taint-hunt-sql-flask-01":
            with urlopen(BASE + "/api/scenarios/" + s["id"]) as r2:
                return json.load(r2)["definition"]["tools"]
    raise SystemExit("round-1 scenario not found; cannot copy tool contracts")


def make_zip():
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        for name in ("app.py", "handlers.py", "db.py"):
            z.write(os.path.join(HERE, "workspace", name), name)
    return buf.getvalue()


def multipart(fields, files):
    boundary = "----taint-hunt-hard-boundary"
    out = io.BytesIO()
    for k, v in fields.items():
        out.write(("--" + boundary + "\r\n").encode())
        out.write(f'Content-Disposition: form-data; name="{k}"\r\n'.encode())
        out.write(b"Content-Type: application/json\r\n\r\n")
        out.write(v.encode())
        out.write(b"\r\n")
    for k, (fn, data) in files.items():
        out.write(("--" + boundary + "\r\n").encode())
        out.write(
            f'Content-Disposition: form-data; name="{k}"; filename="{fn}"\r\n'.encode()
        )
        out.write(b"Content-Type: application/zip\r\n\r\n")
        out.write(data)
        out.write(b"\r\n")
    out.write(("--" + boundary + "--\r\n").encode())
    return out.getvalue(), "multipart/form-data; boundary=" + boundary


def main():
    tools = load_tools()
    definition = {
        "world": WORLD,
        "input_domain": {},
        "user_message": USER_MESSAGE,
        "simulator_notes": "",
        "tools": tools,
        "simulation": {
            "sim_model": "open_router::openai/gpt-4.1-nano",
            "temperature": 0.0,
            "max_tokens": 512,
        },
    }
    request = {"label": "taint-hunt-hard-01", "scenario": definition}
    with open(os.path.join(HERE, "scenario.json"), "w") as f:
        json.dump(request, f, indent=2)
        f.write("\n")

    body, ctype = multipart({"request": json.dumps(request)}, {"workspace": ("w.zip", make_zip())})
    req = urllib.request.Request(
        BASE + "/api/scenarios", data=body, headers={"Content-Type": ctype}, method="POST"
    )
    with urllib.request.urlopen(req) as r:
        result = json.load(r)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()