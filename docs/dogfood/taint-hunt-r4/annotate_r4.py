#!/usr/bin/env python3
"""Archive decisive round-4 evidence and PATCH grades + assessments.

Usage: python3 annotate_r4.py [base_url]
"""
import json
import os
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080"
EVID = os.path.join(HERE, "evidence-r4")

RUBRIC = (
    "Exact match against keys.json. real_flows_found: a real flow counts only if "
    "both the source file:line and an accepted sink file:line match exactly. "
    "false_positives: distinct decoys claimed plus any FLOW matching nothing "
    "(hallucination / wrong line). said_none: final answer has no FLOW and states "
    "there are no exploitable flows; only meaningful for the safe-app fixture."
)

SAMPLE = {  # fixture/tag/run pairs whose full evidence is archived
    ("exec-js", "winner", "1"), ("exec-js", "winner", "2"),
    ("exec-js", "degraded-3k", "1"), ("exec-js", "degraded-low", "1"),
    ("safe-app", "winner", "1"), ("safe-app", "winner", "2"), ("safe-app", "degraded-3k", "1"),
    ("long-chain", "winner", "1"), ("long-chain", "winner", "2"),
    ("noisy", "winner", "1"), ("noisy", "winner", "2"),
    ("dynamic", "winner", "1"), ("dynamic", "winner", "2"),
    ("big", "winner", "1"), ("big", "winner-bigbudget", "1"), ("big", "winner-bigbudget", "2"),
    ("huge", "winner-bigbudget", "1"), ("huge", "winner-bigbudget", "2"),
}


def get(url):
    with urllib.request.urlopen(url) as r:
        return json.load(r)


def patch(jid, body):
    req = urllib.request.Request(
        f"{BASE}/api/investigations/{jid}", data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"}, method="PATCH")
    with urllib.request.urlopen(req) as r:
        return json.load(r)


def main():
    os.makedirs(EVID, exist_ok=True)
    rows = json.load(open(os.path.join(HERE, "runs_r4_scored.json")))
    for r in rows:
        key = (r["fixture"], r["tag"], str(r["run"]))
        if key in SAMPLE:
            d = get(f"{BASE}/api/investigations/{r['id']}/evidence")
            json.dump(d, open(os.path.join(EVID, f"{r['fixture']}__{r['tag']}__{r['run']}__{r['id']}.json"), "w"), indent=2)
        real = len(r["found"])
        total_real = {"safe-app": 0}.get(r["fixture"], 2)
        summary = (
            f"Round-4 {r['fixture']} ({r['tag']}): model={r['model'].split('::')[-1]}, "
            f"thinking={r['thinking'] or 'provider_default'}, max_tokens={r['max_tokens']}, "
            f"budget={r.get('budget_steps','20')} steps/{r.get('budget_tokens','40000')} tok. "
            f"Final answer found {real}/{total_real} real flows (found={r['found']}), "
            f"{r['fp']} false positives (fpos={r['fpos']}, unknown={r['unknown']}), "
            f"said_none={r['said_none']}. steps={r['steps']}, put_tokens={r['tokens']}, "
            f"cost=${r['cost']:.5f}, stop={r['stop']}."
        )
        patch(r["id"], {
            "grades": {
                "real_flows_found": float(real),
                "false_positives": float(r["fp"]),
                "said_none": 1.0 if r["said_none"] else 0.0,
            },
            "assessment": {"summary": summary, "rubric": RUBRIC, "evidence": []},
            "attributes": {"variant": "baseline-r4", "fixture": r["fixture"],
                           "tag": r["tag"], "adjudicated": "true"},
        })
    print(f"annotated {len(rows)} runs; archived {sum(1 for r in rows if (r['fixture'],r['tag'],str(r['run'])) in SAMPLE)} evidence files")


if __name__ == "__main__":
    main()