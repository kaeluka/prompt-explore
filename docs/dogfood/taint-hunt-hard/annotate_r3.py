#!/usr/bin/env python3
"""Record caller grades + assessments on the round-3 runs.

Reads runs_r3_scored.json and PATCHes every run with:
  real_flows_found = recall*2 (0..2)
  false_positives  = distinct decoys claimed (0..4)
and a short summary drawn from the scored row.

Usage: python3 annotate_r3.py [base_url]
"""
import json
import os
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080"

RUBRIC = (
    "Exact match against ground-truth.md. real_flows_found: F1 requires source "
    "app.py:12 and sink db.py:7|9; F2 requires source app.py:19 and sink db.py:15. "
    "false_positives: distinct decoys claimed (D1 db.py:20 parameterized, "
    "D2 db.py:26 int cast, D3 db.py:32 whitelisted, D4 db.py:38 constant). "
    "Scored from the FINAL agent answer only, FLOW lines preferred but a "
    "tolerant parser accepts 'FLOW <file>:<line> sink=<file>:<line>'. "
    "Round-3 fixture is byte-identical to round 2; only the tool contract "
    "(literal grep, list_dir as step 1) changed."
)


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
    rows = json.load(open(os.path.join(HERE, "runs_r3_scored.json")))
    for r in rows:
        model = r["model"].split("::")[-1]
        summary = (
            f"Round-3 {r['kind']} on the fixed-contract fixture: model={model}, "
            f"thinking={r['thinking'] or 'provider_default'}. Final answer scored "
            f"{len(r['found'])}/2 real flows (found={r['found']}) and "
            f"{r['fp']} decoys (fpos={r['fpos']}). "
            f"steps={r['steps']}, put_tokens={r['tokens']}, cost=${r['cost']:.5f}, "
            f"stop={r['stop']}."
        )
        body = {
            "grades": {"real_flows_found": float(len(r["found"])), "false_positives": float(r["fp"])},
            "assessment": {"summary": summary, "rubric": RUBRIC, "evidence": []},
            "attributes": {
                "variant": r["kind"] + "-r3",
                "model": model,
                "thinking": str(r["thinking"] or "provider_default"),
                "tag": r["tag"],
                "adjudicated": "true",
            },
        }
        patch(r["id"], body)
    print(f"annotated {len(rows)} runs")


if __name__ == "__main__":
    main()