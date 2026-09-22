#!/usr/bin/env python3
"""Submit the four round-2 investigations against the pinned scenario.

2 x single-agent baseline (one nano agent, one prompt, structured answer).
2 x two-stage decomposition (enumerate candidates -> verify each against code).

Prints the created investigation ids. Requires a running server and the
scenario id (defaults to the one registered by register.py; override with argv).

Usage: python3 run_investigations.py [scenario_id] [base_url]
"""
import json
import os
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
SCENARIO = sys.argv[1] if len(sys.argv) > 1 else "scn-2b11bf9abd9a"
BASE = sys.argv[2] if len(sys.argv) > 2 else "http://127.0.0.1:8080"
MODEL = "open_router::openai/gpt-4.1-nano"

BASELINE_SYSTEM = """You are a static-analysis security auditor. Use the tools list_dir, read_file and grep to inspect the repository. Untrusted input enters through request.args, request.headers, and request bodies. SQL sinks include cursor.execute, cursor.executescript, and SQL strings built with %, .format(), or f-strings. A path from a source to a sink is an exploitable flow ONLY if the value is not parameterized (a `?` placeholder with a bound parameter tuple), not cast to a safe type, and not whitelisted/constrained to a fixed set of values. Parameterized, cast or whitelisted paths are NOT vulnerabilities. Verify every claim by reading the actual code.

Report ONLY confirmed exploitable flows, one per line, exactly:
FLOW source=<file>:<line> sink=<file>:<line>

Do not report safe paths. If there are no exploitable flows, print NONE."""

ENUMERATE_SYSTEM = """You are a candidate enumerator for a taint analysis. In a first cheap pass, list every untrusted-input source and every SQL sink in the repository, then pair plausible source->sink paths. Be OVER-INCLUSIVE: include every pair that could conceivably carry data from a source to a sink, even if it might be parameterized, cast or whitelisted. Use list_dir, read_file and grep.

Output a JSON array and nothing else:
[{"source": "<file>:<line>", "sink": "<file>:<line>", "how": "<short propagation note>"}]

Use the exact file and line where the untrusted value is read, and the exact file and line of the SQL execution or SQL string construction."""

VERIFY_SYSTEM = """You are a strict verifier. You are given candidate source->sink flows. For EACH candidate, read the actual code with the tools and trace the value from the source line to the sink line. Confirm the flow ONLY if the untrusted value reaches the SQL sink without being parameterized (a `?` placeholder with a bound parameter tuple), without being cast to a safe type (e.g. int()), and without being whitelisted/constrained to a fixed set of values. If any of those defenses applies, REJECT the candidate. A flow is exploitable only if raw untrusted string data is interpolated into SQL.

Output ONLY confirmed exploitable flows, one per line, exactly:
FLOW source=<file>:<line> sink=<file>:<line>

Do not report rejected candidates. If none survive, print NONE."""


def read(name):
    with open(os.path.join(HERE, name)) as f:
        return f.read()


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
            "budget": {"max_steps_per_trace": 10, "max_tokens": 12000},
            "reason": "round-2 baseline: one nano agent, single prompt, hard 3-file fixture",
        },
        "workflow": {
            "lua_source": read("baseline.lua"),
            "params": {"model": MODEL, "system": BASELINE_SYSTEM},
        },
        "attributes": {"variant": "single-agent-nano-r2", "fixture": "taint-hunt-hard", "run": str(n)},
    }


def decompose(n):
    return {
        "scenario_id": SCENARIO,
        "investigation": {
            "budget": {"max_steps_per_trace": 18, "max_tokens": 24000},
            "reason": "round-2 decomposition: enumerate candidates then verify each against the code",
        },
        "workflow": {
            "lua_source": read("decompose.lua"),
            "params": {"model": MODEL, "enumerate": ENUMERATE_SYSTEM, "verify": VERIFY_SYSTEM},
        },
        "attributes": {"variant": "enumerate-verify-nano-r2", "fixture": "taint-hunt-hard", "run": str(n)},
    }


def main():
    ids = []
    for n in (1, 2):
        r = post(baseline(n))
        print("baseline", n, r["id"])
        ids.append(("baseline", n, r["id"]))
    for n in (1, 2):
        r = post(decompose(n))
        print("decompose", n, r["id"])
        ids.append(("decompose", n, r["id"]))
    with open(os.path.join(HERE, "runs.json"), "w") as f:
        json.dump(ids, f, indent=2)
        f.write("\n")


if __name__ == "__main__":
    main()