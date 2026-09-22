# Ground truth — `taint-hunt-hard` fixture

Pinned scoring key for the round-2 taint experiment. The fixture is a tiny
three-file Flask + sqlite3 app. It is deliberately **not** one-function: every
untrusted source enters in `app.py`, is handed to a helper in `handlers.py`
under a **different name**, and reaches a sink in `db.py` under another name
again. This file is the oracle; it is never uploaded to the workspace.

Fixture lives in `docs/dogfood/taint-hunt-hard/workspace/`.

## Real, exploitable taint flows (recall denominator = 2)

### F1 — query param through a strip, two files, `%` interpolation

| role | location | code |
|------|----------|------|
| source | `app.py:12` | `term = request.args.get("term")` |
| propagate | `handlers.py:5` | `cleaned = term.strip()` — **not** a sanitizer for SQL |
| propagate | `handlers.py:6` | `return db.fetch_by_name(cleaned)` |
| sink build | `db.py:7` | `sql = "SELECT id, name FROM users WHERE name = '%s'" % name` |
| **sink** | `db.py:9` | `cur.execute(sql)` |

Accepted sink lines for F1 (construction or execution): `db.py:7` **or**
`db.py:9`. Source must be `app.py:12`.

### F2 — untrusted header through a dict alias, f-string, `executescript`

| role | location | code |
|------|----------|------|
| source | `app.py:19` | `trace = request.headers.get("X-Trace-Id")` |
| propagate | `handlers.py:10` | `entry = {"raw": value}` — renamed through a dict |
| propagate | `handlers.py:11` | `db.write_audit(entry["raw"])` |
| **sink** | `db.py:15` | `cur.executescript(f"INSERT INTO audit(note) VALUES('{msg}')")` |

Accepted sink line for F2: `db.py:15`. Source must be `app.py:19`.
This is the subtle one: header (not query param), dict alias, `f`-string (not
`%`), and `executescript` (not `execute`).

## Decoys — tainted-looking but NOT exploitable (false-positive denominator = 4)

### D1 — genuine source, parameterized sink
- source `app.py:26` `name = request.args.get("name")`
- sink `db.py:20` `cur.execute("... WHERE name = ?", (name,))` — placeholder binding.
Claiming this as a flow is a false positive.

### D2 — genuine source, cast to int
- source `app.py:33` `n = request.args.get("n")`
- `handlers.py:19` `count = int(n)` — a real cast to int.
- sink `db.py:26` `cur.execute(f"... LIMIT {count}")` — safe because `count` is an int.
Claiming this as a flow is a false positive.

### D3 — genuine source, whitelisted upstream
- source `app.py:40` `col = request.args.get("col")`
- `handlers.py:24-25` whitelist: `col` is forced to `"name"` unless it is exactly
  `"name"` or `"email"`.
- sink `db.py:32` `cur.execute("... ORDER BY %s" % col)` — same `%` shape as F1,
  but safe because `col` is whitelisted before it arrives.
Claiming this as a flow is a false positive.

### D4 — dangerous sink with no source at all
- `db.py:38` `cur.executescript("DELETE FROM sessions")` — constant SQL, no
  untrusted input. `handlers`/`app` never route anything into it.
Flagging it as a taint flow is a false positive.

## Completeness

- These are **ALL** the untrusted-input sources: `app.py:12,19,26,33,40`.
- These are **ALL** the SQL sinks: `db.py:9,15,20,26,32,38`.
- There are exactly **2** exploitable flows (F1, F2) and **4** decoys (D1–D4).
- No other file exists; `handlers.py` and `db.py` contain no sources of their own.
- `.strip()` in `handlers.py:5` and the dict alias in `handlers.py:10` do not
  sanitize or parameterize.
- The `sqlite3` import in `app.py` is unused; it is not a sink.

## Scoring rubric

Parse each run's final answer for `FLOW source=<file>:<line> sink=<file>:<line>`
lines.

- **real_found(F1)** iff a FLOW has source `app.py:12` and sink `db.py:7|9`.
- **real_found(F2)** iff a FLOW has source `app.py:19` and sink `db.py:15`.
- **recall** = (# of {F1,F2} found) / 2.
- **false_positive(decoy)** — count a decoy once if any FLOW names its sink
  (`db.py:20`, `db.py:26`, `db.py:32`, `db.py:38`) or, for D1–D3, its source
  (`app.py:26/33/40`) paired with a SQL sink. Headers/sources alone without a
  sink do not count.
- **false_positives** = number of distinct decoys claimed (0–4).
- **precision** = found_reals / max(1, total FLOW lines) using the same matching
  (a FLOW matching neither a real nor a decoy is a hallucination and still a FP).