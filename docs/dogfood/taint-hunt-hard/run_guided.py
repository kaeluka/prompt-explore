#!/usr/bin/env python3
"""Supplementary controlled arm: same fixture, same programs, only the
tool-use confusion from the primary runs corrected.

The primary runs failed because the agents (a) invented an `app/` subdirectory
and (b) passed regex to a literal grep. This runner appends explicit tool-contract
guidance to the SAME system prompts, so the taint-reasoning task is actually
reached. Keep every other variable identical.

Usage: python3 run_guided.py [scenario_id] [base_url]
"""
import importlib.util
import json
import os
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
SCENARIO = sys.argv[1] if len(sys.argv) > 1 else "scn-2b11bf9abd9a"
BASE = sys.argv[2] if len(sys.argv) > 2 else "http://127.0.0.1:8080"

spec = importlib.util.spec_from_file_location("ri", os.path.join(HERE, "run_investigations.py"))
ri = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ri)

GUIDANCE = """

TOOL CONTRACT — read carefully:
- `grep` searches for an EXACT, LITERAL substring. It is NOT a regular expression.
  Passing `execute|query` searches for those exact characters and matches nothing.
  Call grep once per plain token, e.g. grep("execute"), grep("cursor"), grep("request").
- `list_dir` with an empty path lists the repository root. All files live at the
  root. Read them with the exact names it returns, e.g. read_file("app.py"), with
  NO subdirectory prefix. Do not invent directories that list_dir did not return."""


def post(payload):
    req = urllib.request.Request(
        BASE + "/api/investigations",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req) as r:
        return json.load(r)


def baseline(n):
    return {
        "scenario_id": SCENARIO,
        "investigation": {
            "budget": {"max_steps_per_trace": 12, "max_tokens": 16000},
            "reason": "round-2 guided baseline: tool contract corrected, taint task reached",
        },
        "workflow": {
            "lua_source": ri.read("baseline.lua"),
            "params": {"model": ri.MODEL, "system": ri.BASELINE_SYSTEM + GUIDANCE},
        },
        "attributes": {"variant": "baseline-guided", "fixture": "taint-hunt-hard", "run": str(n)},
    }


def decompose(n):
    return {
        "scenario_id": SCENARIO,
        "investigation": {
            "budget": {"max_steps_per_trace": 20, "max_tokens": 28000},
            "reason": "round-2 guided decomposition: tool contract corrected, enumerate -> verify",
        },
        "workflow": {
            "lua_source": ri.read("decompose.lua"),
            "params": {
                "model": ri.MODEL,
                "enumerate": ri.ENUMERATE_SYSTEM + GUIDANCE,
                "verify": ri.VERIFY_SYSTEM + GUIDANCE,
            },
        },
        "attributes": {"variant": "decompose-guided", "fixture": "taint-hunt-hard", "run": str(n)},
    }


def main():
    ids = []
    for n in (1, 2):
        r = post(baseline(n))
        print("baseline-guided", n, r["id"])
        ids.append(("baseline-guided", n, r["id"]))
    for n in (1, 2):
        r = post(decompose(n))
        print("decompose-guided", n, r["id"])
        ids.append(("decompose-guided", n, r["id"]))
    with open(os.path.join(HERE, "runs_guided.json"), "w") as f:
        json.dump(ids, f, indent=2)
        f.write("\n")


if __name__ == "__main__":
    main()