#!/usr/bin/env python3
"""Record caller assessments + grades on the eight round-2 investigations.

Grades (see ground-truth.md and results.md for the rubric):
  real_flows_found       0-2   strict FLOW-line matches against ground truth
  false_positives        0-4   distinct decoys claimed via FLOW lines
  content_decoys_confirmed 0-4 decoys asserted vulnerable in prose (manual)

Run after the server holds the runs. Usage: python3 annotate.py [base_url]
"""
import json
import os
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080"

RUBRIC = (
    "Binary/exact against the pinned ground truth in ground-truth.md. "
    "real_flows_found counts how many of the two real flows "
    "(F1 app.py:12 -> db.py:7/9, F2 app.py:19 -> db.py:15) the run stated as a "
    "FLOW source=<file>:<line> sink=<file>:<line> line. false_positives counts "
    "distinct decoys (D1 db.py:20 parameterized, D2 db.py:26 int cast, "
    "D3 db.py:32 whitelist, D4 db.py:38 constant) claimed via FLOW lines. "
    "content_decoys_confirmed is a manual count of decoys the run called "
    "vulnerable in prose when it did not use the FLOW format."
)

# investigation id -> (variant, run, grade dict, summary, evidence refs)
ANNOTATIONS = {
    "2ea6b794-ead2-4e2c-a483-3574bb0f6160": (
        "baseline", 1,
        {"real_flows_found": 0, "false_positives": 0, "content_decoys_confirmed": 0},
        "Baseline 1. Nano invented an 'app/' subdirectory, read app/app.py, app/db.py, "
        "app/handlers.py (all not found), and concluded the repository had no flows. "
        "It never read app.py at the root despite list_dir returning it. Result: 0/2 real "
        "flows, 0 decoys. Failure is tool use / path handling, not taint reasoning. "
        "No FLOW lines emitted.",
        [{"turn": 0, "note": "list_dir('') returns app.py, db.py, handlers.py at root; "
                             "the same turn also calls list_dir('app') (not found)."},
         {"turn": 1, "note": "read_file('app/app.py') -> not found; model adopts the "
                             "phantom directory instead of retrying the real path."},
         {"turn": 5, "note": "Final answer: no confirmed flows. Wrong."}],
    ),
    "e98d777e-5cbc-4603-b80e-9d0367b2f3ef": (
        "baseline", 2,
        {"real_flows_found": 0, "false_positives": 0, "content_decoys_confirmed": 0},
        "Baseline 2. Same phantom 'app/' directory error; every read_file targets app/. "
        "Gave up after 6 steps with 'files could not be found'. 0/2 real flows, 0 decoys. "
        "No FLOW lines emitted.",
        [{"turn": 0, "note": "list_dir('') returns the three root files; list_dir('app') not found."},
         {"turn": 4, "note": "Final answer claims app.py/db.py/handlers.py could not be found."}],
    ),
    "55ab69f0-74dd-4137-a350-b2a4e24ffd44": (
        "decompose", 1,
        {"real_flows_found": 0, "false_positives": 0, "content_decoys_confirmed": 0},
        "Decompose run 2x. Enumerate stage used regex alternation in the literal grep "
        "(pattern '(request|params|body|query)\\s*\\.'), matched nothing, and emitted prose "
        "instead of the requested JSON candidate list. Verify stage got no candidates and "
        "printed NONE. 0/2 real flows, 0 decoys. The verifier never had a decoy to reject.",
        [{"turn": 0, "note": "grep with regex metacharacters against a literal-grep tool -> zero matches."},
         {"turn": 3, "note": "Stage 2 output: NONE; no candidate reached verification."}],
    ),
    "95ee3e50-8322-41b6-9ce1-da608c5c17f5": (
        "decompose", 2,
        {"real_flows_found": 0, "false_positives": 0, "content_decoys_confirmed": 0},
        "Decompose run 2x. Same regex-into-literal-grep collapse; enumerate emitted prose "
        "('no direct or obvious taint flows'), verify printed NONE. 0/2 real flows, 0 decoys. "
        "No code was read.",
        [{"turn": 0, "note": "grep('(execute|query|sql)\\s*\\(') against literal grep -> no matches."},
         {"turn": 5, "note": "Final answer: NONE."}],
    ),
    "769d0881-6631-4a26-8a1a-850c904bbae6": (
        "baseline-guided", 1,
        {"real_flows_found": 0, "false_positives": 0, "content_decoys_confirmed": 0},
        "Guided baseline 1. Correct literal grep for request.args / request.headers found "
        "the sources. But it grepped 'cursor.execute'/'cursor.executescript' while the code "
        "uses the variable name 'cur', so it saw no sinks, and it never called read_file. "
        "Concluded no flows. 0/2 real flows, 0 decoys.",
        [{"turn": 0, "note": "grep('request.args') and grep('request.headers') correctly locate "
                             "sources at app.py:12 and app.py:19."},
         {"turn": 1, "note": "grep('cursor.execute') -> no matches; the sink uses 'cur.execute'."},
         {"turn": 6, "note": "Final answer: no confirmed exploitable flows. Wrong."}],
    ),
    "af526a3d-b337-4006-9e37-4c3a80aa027b": (
        "baseline-guided", 2,
        {"real_flows_found": 0, "false_positives": 0, "content_decoys_confirmed": 0},
        "Guided baseline 2. list_dir succeeded, then grepped 'cursor.execute'/'cursor.executescript' "
        "(code uses 'cur'), found nothing, and stopped in 4 steps without reading a file. "
        "0/2 real flows, 0 decoys.",
        [{"turn": 0, "note": "list_dir returns the three files; grep('cursor.execute') -> no matches."},
         {"turn": 1, "note": "Final answer: no SQL execution usage found. Wrong."}],
    ),
    "9969b60c-0fd7-4938-bf71-136e1231ac3f": (
        "decompose-guided", 1,
        {"real_flows_found": 0, "false_positives": 0, "content_decoys_confirmed": 0},
        "Guided decompose 1. Enumerate still used regex grep ('execute|query') despite guidance, "
        "but did find sources via grep('request') and cursors via grep('cursor'). It then "
        "concluded no SQL sinks exist and produced no candidate list; verify printed prose 'no "
        "taint flows to report'. 0/2 real flows, 0 decoys.",
        [{"turn": 0, "note": "grep('execute|query') regex still used -> no matches."},
         {"turn": 3, "note": "Final answer: no SQL sinks, no taint flows to report. Wrong."}],
    ),
    "d9bed340-fea8-4e2f-9aca-d3870f476deb": (
        "decompose-guided", 2,
        {"real_flows_found": 0, "false_positives": 0, "content_decoys_confirmed": 2},
        "Guided decompose 2 — the only run that actually read the code (app.py and db.py, 16 "
        "steps, 12000 tokens, completed normally with no budget cutoff). It found the string "
        "interpolations but failed the "
        "task: it reported db.py:15 (write_audit, the F2 sink) with source AND sink both at "
        "db.py:15, never linking back to the untrusted app.py:19 header — so 0 real source->sink "
        "flows. Its verifier then CONFIRMED two decoys as vulnerable: db.py:26 (D2, int cast) and "
        "db.py:32 (D3, whitelisted in handlers.py:24-25). It ignored fetch_by_name's real F1 "
        "sink at db.py:9 entirely. content_decoys_confirmed=2; no FLOW-line format so strict "
        "false_positives=0.",
        [{"turn": 2, "note": "Reads db.py and app.py in full; enumerates sources at app.py:12/19/26/33."},
         {"turn": 4, "note": "Candidates assert db.py:15, db.py:26, db.py:32 — the last two are decoys D2/D3."},
         {"turn": 7, "note": "Verifier 'confirms' top_rows (D2 cast) and ordered_users (D3 whitelist) "
                             "as exploitable; misses the real fetch_by_name flow."}],
    ),
}


def patch(jid, body):
    req = urllib.request.Request(
        f"{BASE}/api/investigations/{jid}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="PATCH",
    )
    with urllib.request.urlopen(req) as r:
        return json.load(r)


def main():
    for jid, (variant, run, grades, summary, evidence) in ANNOTATIONS.items():
        body = {
            "grades": grades,
            "assessment": {"summary": summary, "rubric": RUBRIC, "evidence": evidence},
            "attributes": {"variant": variant, "run": str(run), "fixture": "taint-hunt-hard",
                           "adjudicated": "true"},
        }
        r = patch(jid, body)
        print(jid[:8], variant, run, "grades=", r.get("grades"))


if __name__ == "__main__":
    main()