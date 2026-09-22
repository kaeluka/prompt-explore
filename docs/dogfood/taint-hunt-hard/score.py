#!/usr/bin/env python3
"""Score runs against the pinned ground truth in ground-truth.md.

Reads a runs file (default runs.json), fetches each investigation's evidence,
extracts FLOW source=<file>:<line> sink=<file>:<line> lines, and reports
recall over the 2 real flows and false positives over the 4 decoys.

Usage: python3 score.py [runs.json] [base_url]
"""
import json
import os
import re
import sys
import urllib.request
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))

REAL = {
    "F1": {"source": "app.py:12", "sinks": {"db.py:7", "db.py:9"}},
    "F2": {"source": "app.py:19", "sinks": {"db.py:15"}},
}
DECOYS = {
    "D1": {"source": "app.py:26", "sinks": {"db.py:20"}},
    "D2": {"source": "app.py:33", "sinks": {"db.py:26"}},
    "D3": {"source": "app.py:40", "sinks": {"db.py:32"}},
    "D4": {"source": None, "sinks": {"db.py:38"}},
}

FLOW_RE = re.compile(r"source\s*[=:]?\s*`?\s*([\w./-]+:\d+)", re.I)
SINK_RE = re.compile(r"sink\s*[=:]?\s*`?\s*([\w./-]+:\d+)", re.I)
TOKEN_RE = re.compile(r"([\w./-]+:\d+)")


def norm(x):
    return x.strip().strip("`").strip(".,;)")


def extract_lines(text):
    """Parse FLOW lines tolerantly.

    Accepts `FLOW source=<f>:<l> sink=<f>:<l>` and the common variant
    `FLOW <f>:<l> sink=<f>:<l>` (first file:line is the source).
    """
    out = []
    for line in (text or "").splitlines():
        if "FLOW" not in line.upper():
            continue
        toks = TOKEN_RE.findall(line)
        m_src = FLOW_RE.search(line)
        m_snk = SINK_RE.search(line)
        src = norm(m_src.group(1)) if m_src else (toks[0] if toks else None)
        snk = norm(m_snk.group(1)) if m_snk else (toks[-1] if len(toks) >= 2 else None)
        if src and snk:
            out.append((src, snk))
    return out


def get(url):
    with urllib.request.urlopen(url) as r:
        return json.load(r)


def collect_text(d):
    """Return the FINAL answer only — never intermediate turns' reasoning.

    Preference: last agent invocation output, then the workflow's returned
    `answer`, then the last model turn. Intermediate turns are excluded so a
    model thinking out loud does not have its scratch FLOW lines scored as
    final claims.
    """
    invs = d.get("workflow", {}).get("invocations", [])
    last = invs[-1].get("output") if invs else None
    if isinstance(last, str):
        return last
    out = d.get("workflow", {}).get("output")
    if isinstance(out, dict) and isinstance(out.get("answer"), str):
        return out["answer"]
    if isinstance(out, str):
        return out
    for t in reversed(d.get("turns", [])):
        if isinstance(t.get("model_output"), str) and t["model_output"].strip():
            return t["model_output"]
    return ""


def score_run(base, v, n, i):
    d = get(f"{base}/api/investigations/{i}/evidence")
    text = collect_text(d)
    flows = extract_lines(text)
    found = set()
    fpos = set()
    unknown = []
    for src, snk in flows:
        matched = False
        for name, gt in REAL.items():
            if src == gt["source"] and snk in gt["sinks"]:
                found.add(name)
                matched = True
        for name, gt in DECOYS.items():
            sink_hit = snk in gt["sinks"]
            source_hit = gt["source"] is not None and src == gt["source"]
            if sink_hit or source_hit:
                fpos.add(name)
                matched = True
        if not matched:
            unknown.append((src, snk))
    exec_ = d.get("execution", {})
    usage = d.get("usage", {})
    put = usage.get("put", {})
    return {
        "variant": v,
        "run": n,
        "id": i,
        "found": sorted(found),
        "recall": len(found) / 2.0,
        "fpos": sorted(fpos),
        "fp": len(fpos),
        "unknown": unknown,
        "flows": flows,
        "steps": exec_.get("steps_used"),
        "put_tokens": exec_.get("put_tokens_used"),
        "cost_usd": put.get("cost_usd"),
        "llm_calls": put.get("llm_calls"),
        "tool_calls": put.get("tool_calls"),
        "stop_reason": exec_.get("stop_reason"),
        "answer": (text or "")[:1200],
    }


def main():
    runs_file = sys.argv[1] if len(sys.argv) > 1 else HERE + "/runs.json"
    base = sys.argv[2] if len(sys.argv) > 2 else "http://127.0.0.1:8080"
    rows = json.load(open(runs_file))
    results = [score_run(base, v, n, i) for v, n, i in rows]
    json.dump(results, open(runs_file.replace(".json", "_scored.json"), "w"), indent=2)
    print(f"{'variant':28} {'run':>3} {'recall':>6} {'FP':>2} {'steps':>5} {'tokens':>6} {'cost$':>8}")
    for r in results:
        print(
            f"{r['variant']:28} {r['run']:>3} {r['recall']:>6.2f} {r['fp']:>2} "
            f"{str(r['steps']):>5} {str(r['put_tokens']):>6} {r['cost_usd']:>8.5f}"
        )
    print()
    agg = defaultdict(lambda: {"recall": [], "fp": [], "tokens": [], "cost": []})
    for r in results:
        a = agg[r["variant"]]
        a["recall"].append(r["recall"])
        a["fp"].append(r["fp"])
        a["tokens"].append(r["put_tokens"] or 0)
        a["cost"].append(r["cost_usd"] or 0)
    for v, a in agg.items():
        print(
            f"{v:28} mean_recall={sum(a['recall'])/len(a['recall']):.2f} "
            f"mean_FP={sum(a['fp'])/len(a['fp']):.2f} "
            f"mean_tokens={sum(a['tokens'])/len(a['tokens']):.0f} "
            f"mean_cost=${sum(a['cost'])/len(a['cost']):.5f}"
        )


if __name__ == "__main__":
    main()