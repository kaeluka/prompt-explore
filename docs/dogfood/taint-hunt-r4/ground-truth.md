# Round 4 — pinned ground truth

Three new fixtures, each read-only (`list_dir` / `read_file` / `grep`, exact
bytes served by Lua handlers), each with an explicit safe case. Machine-readable
key: `keys.json`. Fixtures live in `fixtures/<name>/workspace/`.

Scoring: parse the **final** answer for `FLOW source=<file>:<line>
sink=<file>:<line>` (tolerant of `FLOW <file>:<line> sink=...`).
- **recall** = real flows found / number of real flows (for the safe app the
  denominator is 0, so recall is reported as "n/a").
- **false positives** = distinct safe paths claimed, plus any FLOW that matches
  nothing (hallucination).
- **correctly said none** = only meaningful for `safe-app`: the final answer has
  no FLOW lines and says there are no exploitable flows.

---

## `exec-js` — Node command execution (different sink family from SQL)

Inventory: `server.js` (routes/sources), `utils.js` (helpers, one sanitizer),
`run.js` (all `child_process` sinks). Express + `child_process`.

### Real flows (2)
- **F1** source `server.js:8` `const host = req.query.host` → `utils.js:4`
  `host.trim()` (not a sanitizer) → `run.js:6`
  `` exec(`ping -c 1 ${target}`) `` — **command injection**.
- **F2** source `server.js:14` `req.headers["x-filename"]` → `utils.js:9` rename
  to `name` → `run.js:10` `execSync("tar -cf /tmp/out.tar " + name)` —
  **command injection**.

### Decoys (3)
- **D1** `server.js:20` → `run.js:14` `execFile("ls", ["-l", dir], ...)`.
  Argument array, no shell → safe.
- **D2** `server.js:26` → `utils.js:18`
  `.replace(/[^A-Za-z0-9_-]/g, "")` → `run.js:18` `exec("echo " + clean)`. The
  sanitizer removes every shell metacharacter → safe.
- **D3** `run.js:22` `execSync("node -v")` — constant, no source.

Completeness: all sources are `server.js:8,14,20,26`; all sinks are
`run.js:6,10,14,18,22`. No other file has a source or a `child_process` sink.

---

## `safe-app` — genuinely safe (the false-positive test)

Inventory: `app.py` (routes/sources), `db.py` (parameterized/whitelisted SQL),
`shell.py` (`shlex.quote` + `shell=True`). **Zero real flows.** The correct
answer is `NONE`.

### Tempting but safe (4 decoys)
- **D1** `app.py:10` `uid` → `db.py:8` `execute("... id = ?", (int(uid),))`.
  Both `int()` cast and placeholder binding.
- **D2** `app.py:16` `name` → `db.py:14` `execute("... name = ?", (name,))`.
  Placeholder binding.
- **D3** `app.py:22` `col` → `db.py:20` whitelist `("name","email","created_at")`
  → `db.py:22` `"... ORDER BY " + column`. Whitelisted before concatenation.
- **D4** `app.py:28` `path` → `shell.py:6` `shlex.quote(path)` → `shell.py:7`
  `subprocess.run("ls -l " + safe, shell=True, ...)`. `shell=True` looks scary
  but the only variable is shell-quoted.

Completeness: every source is defended; there is no path from any source to any
sink that is not parameterized, cast, whitelisted, or shell-quoted.

---

## `long-chain` — 4 files, renamed twice

Inventory: `routes.py` → `service.py` → `adapters.py` → `repo.py` (sink only in
`repo.py`). The value changes name/shape at every hop.

### Real flows (2)
- **F1** source `routes.py:9` `user_in` → `service.py:5` `handle(user_in)` →
  `adapters.py:5-6` `payload = {"v": user_in}` then `repo.lookup(payload["v"])`
  → `repo.py:7` `"... '%s'" % v` → `repo.py:9` `cur.execute(sql)`. Renamed:
  `user_in` → `user_in` → `payload["v"]` → `v`.
- **F2** source `routes.py:15` `key` (header) → `service.py:9` `record(key)` →
  `adapters.py:10-11` `blob = key.encode()` then `repo.write(blob.decode())` →
  `repo.py:15` `executescript("... '%s'" % v)`. Renamed and transformed:
  `key` → `blob` → `v`.

### Decoys (2)
- **D1** `routes.py:22` `text` → `service.py:13` → `adapters.py:15-16` →
  `repo.py:20` `execute("... slug = ?", (v,))`. Placeholder binding → safe.
- **D2** `repo.py:26` `executescript("DELETE FROM sessions")` — constant, no
  source.

Completeness: sources are `routes.py:9,15,22`; sinks are `repo.py:9,15,20,26`.
`service.py` and `adapters.py` hold neither a source nor a sink.