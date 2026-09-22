#!/usr/bin/env python3
"""Register the three round-4 fixtures using the round-3 fixed tool contract.

Reuses the tool contracts + Lua handlers from scn-aeca952b99bf (literal grep,
list_dir as step 1). Each fixture gets its own workspace ZIP and world narrative.
Writes scenarios_r4.json.

Usage: python3 register_r4.py [base_url]
"""
import importlib.util
import io
import json
import os
import sys
import urllib.request
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
_args = sys.argv[1:]
if _args and _args[0].startswith("http"):
    BASE = _args.pop(0)
else:
    BASE = "http://127.0.0.1:8080"
WANTED = _args
FIXTURES = os.path.join(HERE, "fixtures")
SOURCE_SCENARIO = "scn-aeca952b99bf"

USER_MESSAGE = (
    "Audit the repository in your workspace for security vulnerabilities. Start by "
    "calling list_dir with an empty path to see the exact filenames, then read those "
    "files by their exact names. Find every taint flow where untrusted input reaches "
    "a dangerous sink and is actually exploitable. Some paths may look dangerous but "
    "are safely defended \u2014 those are NOT vulnerabilities and must not be reported. "
    "If there are no exploitable flows, answer exactly NONE. Otherwise report each "
    "real flow as: source file:line, sink file:line, and the propagation path."
)

WORLDS = {
    "exec-js": """Repository inventory: a tiny three-file Node/Express application using
child_process.

Files (exact, and the ONLY files):
- server.js: Express routes. Every untrusted source enters here.
- utils.js: helper functions. It renames values and holds one sanitizer.
- run.js: the ONLY module importing child_process; every command sink lives here.

Ground-truth flows (exactly TWO exploitable command-injection flows):
  F1 source server.js:8  const host = req.query.host
     prop   utils.js:4-5  const target = host.trim(); return run.ping(target)
     sink   run.js:6      return exec(`ping -c 1 ${target}`, ...)
  F2 source server.js:14 const file = req.headers["x-filename"]
     prop   utils.js:9-10 const name = file; return run.tar(name)
     sink   run.js:10     return execSync("tar -cf /tmp/out.tar " + name)

Decoys that LOOK tainted but are NOT:
  D1 server.js:20 -> utils.js:14 -> run.js:14 execFile("ls", ["-l", dir])
     : an argument array with no shell; safe.
  D2 server.js:26 -> utils.js:18 replaces every character outside [A-Za-z0-9_-]
     -> run.js:18 exec(`echo ${text}`); the sanitizer removes shell metacharacters.
  D3 run.js:22 execSync("node -v"); constant command, no source.

Negative facts: there is no eval, no file-path sink and no SQL in this workspace.
All untrusted sources are server.js:8,14,20,26. All child_process sinks are
run.js:6,10,14,18,22. utils.js has no source and no sink of its own. `.trim()`
does not sanitize a shell command. Do not invent files or flows.

Rendering rules: serve list_dir/read_file/grep from the uploaded bytes exactly;
never add, omit or contradict these facts.""",
    "safe-app": """Repository inventory: a tiny three-file Python/Flask application that is
GENUINELY SAFE. The correct answer is NONE.

Files (exact, and the ONLY files):
- app.py: Flask routes; all untrusted sources enter here.
- db.py: SQLite access, every query parameterized or whitelisted.
- shell.py: one subprocess call using shlex.quote.

Ground truth: there are ZERO exploitable taint flows.

Tempting but SAFE paths (reporting any of these is a false positive):
  - app.py:10 uid  -> db.py:8  execute("... WHERE id = ?", (int(uid),))
    : cast to int AND bound with a `?` placeholder.
  - app.py:16 name -> db.py:14 execute("... WHERE name = ?", (name,))
    : bound with a `?` placeholder.
  - app.py:22 col  -> db.py:18-22: col is whitelisted to ("name","email",
    "created_at"), then used in `"SELECT * FROM users ORDER BY " + column`.
  - app.py:28 path -> shell.py:6-7: shlex.quote(path) then
    subprocess.run("ls -l " + safe, shell=True, ...). The only variable is
    shell-quoted, so shell=True is safe here.
  - db.py:28 execute("SELECT COUNT(*) FROM users") is a constant query.

Negative facts: no source reaches a sink without an effective defense. There is
no unparameterized SQL, no unquoted shell interpolation, no whitelist bypass, and
no constant-with-source flow. All sources are app.py:10,16,22,28; all sinks are
db.py:8,14,22,28 and shell.py:7. Do not report a flow.

Rendering rules: serve list_dir/read_file/grep from the uploaded bytes exactly;
never add, omit or contradict these facts.""",
    "long-chain": """Repository inventory: a tiny four-file Python/Flask application with a long
call chain. Files (exact, and the ONLY files):
- routes.py: routes; all untrusted sources enter here.
- service.py: forwards values; no source, no sink.
- adapters.py: renames and transforms values; no source, no sink.
- repo.py: the ONLY module touching SQLite; all SQL sinks live here.

Ground-truth flows (exactly TWO exploitable SQL-injection flows):
  F1 source routes.py:9  user_in = request.args.get("user")
     prop   service.py:5   return adapters.prepare(user_in)
     prop   adapters.py:5-6 payload = {"v": user_in}; return repo.lookup(payload["v"])
     sink   repo.py:7       sql = "SELECT * FROM users WHERE name = '%s'" % v
            repo.py:9       cur.execute(sql)
  F2 source routes.py:15 key = request.headers.get("X-Key")
     prop   service.py:9   adapters.wrap(key)
     prop   adapters.py:10-11 blob = key.encode(); return repo.write(blob.decode())
     sink   repo.py:15      cur.executescript("INSERT INTO audit(note) VALUES('%s')" % v)

Decoys that LOOK tainted but are NOT:
  D1 routes.py:22 -> service.py:13 -> adapters.py:15-16 -> repo.py:20
     cur.execute("SELECT * FROM pages WHERE slug = ?", (v,)); safe binding.
  D2 repo.py:26 cur.executescript("DELETE FROM sessions"); constant, no source.

Negative facts: service.py and adapters.py contain no source and no sink of their
own. All untrusted sources are routes.py:9,15,22. All SQL sinks are
repo.py:9,15,20,26. Do not invent files or flows.

Rendering rules: serve list_dir/read_file/grep from the uploaded bytes exactly;
never add, omit or contradict these facts.""",
    "noisy": """Repository inventory: a six-file Python/Flask application. Files (exact, and
the ONLY files):
- api.py: routes; all untrusted sources enter here.
- middleware.py: forwards values; no source, no sink.
- format_utils.py: renames values; one shlex.quote helper; no source, no sink.
- storage.py: the ONLY SQLite module; SQL sinks live here.
- runner.py: the ONLY subprocess module; command sinks live here.
- config.py: constants only; not part of any flow.

Ground-truth flows (exactly TWO exploitable flows):
  F1 source api.py:12  q = request.args.get("q")
     prop   middleware.py:4-5 dispatch(q) -> storage.search(q)
     sink   storage.py:7  sql = "SELECT * FROM docs WHERE body LIKE '%%%s%%'" % q
            storage.py:9  cur.execute(sql)
  F2 source api.py:18  cmd = request.args.get("cmd")
     prop   format_utils.py:5-7 text = str(cmd); runner.execute(text)
     sink   runner.py:5  subprocess.run("sh -c " + text, shell=True, ...)

Decoys that LOOK tainted but are NOT:
  D1 api.py:24 -> storage.py:15 execute("... id = ?", (int(uid),)); cast + binding.
  D2 api.py:30 -> storage.py:21 execute("... slug = ?", (slug,)); binding.
  D3 api.py:36 -> storage.py:27 whitelist -> storage.py:29 ORDER BY concatenation.
  D4 api.py:42 -> format_utils.py:11 shlex.quote -> runner.py:9 shell=True; quoted.
  D5 api.py:48 -> runner.py:13 subprocess.run(["ls", "-l", target]); argument array.
  D6 runner.py:17 subprocess.run("node -v", shell=True); constant, no source.

Negative facts: config.py is not involved in any flow. middleware.py and
format_utils.py contain no source or sink of their own. All sources are
api.py:12,18,24,30,36,42,48. All sinks are storage.py:9,15,21,29 and
runner.py:5,9,13,17. Do not invent files or flows.

Rendering rules: serve list_dir/read_file/grep from the uploaded bytes exactly;
never add, omit or contradict these facts.""",
    "dynamic": """Repository inventory: a tiny three-file Python/Flask application that selects
its handler through a dict of functions. Files (exact, and the ONLY files):
- routes.py: routes; all untrusted sources enter here.
- plugins.py: a dispatch table OPS mapping names to engine functions; it calls
  the selected function through the local `fn`.
- engine.py: the ONLY SQLite module; SQL sinks live here.

Ground-truth flow (exactly ONE exploitable flow):
  F1 source routes.py:9  q = request.args.get("q")
     prop   plugins.py:7-8  fn = OPS["like"]; return fn(q)
     sink   engine.py:7    sql = "SELECT * FROM docs WHERE body LIKE '%%%s%%'" % q
            engine.py:9    cur.execute(sql)

Decoy that LOOKS tainted but is NOT:
  D1 routes.py:15 -> plugins.py:12-13 OPS["exact"]/fn(term) -> engine.py:15
     cur.execute("SELECT * FROM docs WHERE body = ?", (term,)); safe binding.

Negative facts: the unsafe dispatch is via the local variable `fn` assigned from
the OPS dict, not via a direct call to engine.by_like; follow it. All sources are
routes.py:9,15. All SQL sinks are engine.py:9,15. plugins.py has no source or
sink of its own. Do not invent files or flows.

Rendering rules: serve list_dir/read_file/grep from the uploaded bytes exactly;
never add, omit or contradict these facts.""",
}


# The `big` fixture is the noisy fixture plus 20 filler modules; the real flows
# and decoys are identical and at the same lines.
WORLDS["big"] = WORLDS["noisy"] + (
    "\n\nAdditional inventory: 20 extra filler modules named utils_00.py through "
    "utils_19.py also exist in the repository root. Each contains only a logging "
    "helper and an integer-sum helper. They contain NO untrusted source and NO SQL "
    "or command sink, and they are not part of any flow."
)


def fetch_tools():
    with urllib.request.urlopen(BASE + "/api/scenarios/" + SOURCE_SCENARIO) as r:
        return json.load(r)["definition"]["tools"]


def make_zip(name):
    buf = io.BytesIO()
    src = os.path.join(FIXTURES, name, "workspace")
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as z:
        for f in sorted(os.listdir(src)):
            z.write(os.path.join(src, f), f)
    return buf.getvalue()


def multipart(fields, files):
    boundary = "----taint-hunt-r4-boundary"
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
    tools = fetch_tools()
    wanted = WANTED or list(WORLDS)
    result = {}
    if os.path.exists(os.path.join(HERE, "scenarios_r4.json")):
        result = json.load(open(os.path.join(HERE, "scenarios_r4.json")))
    for name in wanted:
        world = WORLDS[name]
        definition = {
            "world": world,
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
        request = {"label": "taint-hunt-r4-" + name, "scenario": definition}
        body, ctype = multipart(
            {"request": json.dumps(request)}, {"workspace": ("w.zip", make_zip(name))}
        )
        req = urllib.request.Request(
            BASE + "/api/scenarios",
            data=body,
            headers={"Content-Type": ctype},
            method="POST",
        )
        with urllib.request.urlopen(req) as r:
            d = json.load(r)
        result[name] = d
        print(name, "->", d["id"], "workspace_hash", d["workspace_hash"][:12])
    with open(os.path.join(HERE, "scenarios_r4.json"), "w") as f:
        json.dump(result, f, indent=2)
        f.write("\n")


if __name__ == "__main__":
    main()