#!/usr/bin/env python3
"""Round-4 runner + scorer: does the round-3 winner generalize to new fixtures?

Subcommands:
  run <fixture> <tag> <model> [run] [thinking] [max_tokens]
  wait
  score

Fixtures: exec-js | safe-app | long-chain (from scenarios_r4.json).
Ground truth: keys.json. Winner config: gpt-5-nano, thinking unset, temp 0,
max_tokens 12000.
"""
import importlib.util
import json
import os
import re
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = "http://127.0.0.1:8080"
RUNS = os.path.join(HERE, "runs_r4.json")

# Language-general single-agent taint-audit prompt (round-3 prompt extended
# beyond SQL to command execution and file paths).
SYSTEM = """You are a static-analysis security auditor. Use the tools list_dir, read_file and grep to inspect the repository. Untrusted input enters through request query parameters, headers, and bodies. Dangerous sinks include SQL execution (execute/executescript, string-built SQL), command execution (child_process exec/execSync/execFile/spawn, subprocess with shell=True), eval, and file paths.

A path from a source to a sink is an exploitable flow ONLY if the untrusted value reaches the sink without an effective defense. Effective defenses include: parameter binding (a `?` placeholder with a bound parameter tuple), a type cast (e.g. int()), a whitelist/allowlist restricting the value to a fixed set, shell quoting (e.g. shlex.quote), and argument arrays passed to a no-shell API. Parameterized, cast, whitelisted, quoted and argument-array paths are NOT vulnerabilities. Verify every claim by reading the actual code.

Report ONLY confirmed exploitable flows, one per line, exactly:
FLOW source=<file>:<line> sink=<file>:<line>

Do not report safe paths. If there are no exploitable flows, print NONE."""


def read(name):
    with open(os.path.join(HERE, name)) as f:
        return f.read()


def load(path):
    with open(os.path.join(HERE, path)) as f:
        return json.load(f)


def post(payload):
    req = urllib.request.Request(
        BASE + "/api/investigations",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req) as r:
        return json.load(r)


def submit(fixture, tag, model, run, thinking=None, max_tokens=12000):
    scenarios = load("scenarios_r4.json")
    scenario = scenarios[fixture]["id"]
    controls = {"temperature": 0, "max_tokens": max_tokens}
    if thinking:
        controls["thinking"] = thinking
    budget_steps = int(os.environ.get("R4_MAX_STEPS", "20"))
    budget_tokens = int(os.environ.get("R4_MAX_TOKENS", "40000"))
    payload = {
        "scenario_id": scenario,
        "investigation": {
            "budget": {"max_steps_per_trace": budget_steps, "max_tokens": budget_tokens},
            "reason": f"round-4 {fixture}: {tag} model={model} thinking={thinking or 'default'} max_tokens={max_tokens}",
        },
        "workflow": {
            "lua_source": read("baseline_r4.lua"),
            "params": {"model": model, "system": SYSTEM, "controls": controls},
        },
        "attributes": {
            "variant": "baseline-r4",
            "fixture": fixture,
            "model": model.split("::")[-1],
            "tag": tag,
            "run": str(run),
            "thinking": str(thinking or "provider_default"),
        },
    }
    r = post(payload)
    entry = {"fixture": fixture, "tag": tag, "model": model, "run": run,
             "thinking": thinking, "max_tokens": max_tokens,
             "budget_steps": budget_steps, "budget_tokens": budget_tokens, "id": r["id"]}
    runs = json.load(open(RUNS)) if os.path.exists(RUNS) else []
    runs.append(entry)
    json.dump(runs, open(RUNS, "w"), indent=2)
    print(json.dumps(entry))
    return entry


def get(url):
    with urllib.request.urlopen(url) as r:
        return json.load(r)


def wait():
    runs = json.load(open(RUNS))
    for _ in range(150):
        states = [(r["id"], get(f"{BASE}/api/investigations/{r['id']}")["status"]) for r in runs]
        running = [i for i, s in states if s not in ("done", "failed")]
        if not running:
            print("all terminal")
            return
        print(f"{len(running)} running")
        time.sleep(4)
    raise SystemExit("timeout")


def score():
    spec = importlib.util.spec_from_file_location(
        "sc", os.path.join(os.path.dirname(HERE), "taint-hunt-hard", "score.py")
    )  # ../taint-hunt-hard/score.py
    sc = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(sc)
    keys = load("keys.json")
    runs = json.load(open(RUNS))
    out = []
    for r in runs:
        d = get(f"{BASE}/api/investigations/{r['id']}/evidence")
        text = sc.collect_text(d)
        flows = sc.extract_lines(text)
        key = keys[r["fixture"]]
        real, decoys = key["real"], key["decoys"]
        found, fpos, unknown = set(), set(), []
        for src, snk in flows:
            matched = False
            for name, gt in real.items():
                if src == gt["source"] and snk in gt["sinks"]:
                    found.add(name); matched = True
            for name, gt in decoys.items():
                if snk in gt["sinks"] or (gt["source"] and src == gt["source"]):
                    fpos.add(name); matched = True
            if not matched:
                unknown.append((src, snk))
        said_none = (not flows) and bool(
            re.search(r"\bnone\b|no\s+(confirmed\s+)?(exploitable|taint)", text, re.I)
        )
        ex = d.get("execution", {})
        u = d.get("usage", {}).get("put", {})
        out.append({
            **r, "found": sorted(found), "fpos": sorted(fpos),
            "unknown": unknown, "flows": flows,
            "recall": (len(found) / len(real)) if real else None,
            "fp": len(fpos) + len(unknown),
            "said_none": said_none,
            "steps": ex.get("steps_used"), "tokens": ex.get("put_tokens_used"),
            "cost": u.get("cost_usd"), "stop": ex.get("stop_reason"),
            "answer": text[:1600],
        })
    json.dump(out, open(os.path.join(HERE, "runs_r4_scored.json"), "w"), indent=2)
    hdr = f"{'fixture':11} {'tag':14} {'model':22} {'thk':8} {'mtok':>5} {'run':>3} {'rec':>4} {'fp':>3} {'none':>4} {'steps':>5} {'tok':>6} {'cost':>9}"
    print(hdr)
    for x in sorted(out, key=lambda z: (z["fixture"], z["tag"], str(z["run"]))):
        rec = "n/a" if x["recall"] is None else f"{x['recall']:.2f}"
        print(f"{x['fixture']:11} {x['tag']:14} {x['model'].split('::')[-1]:22} "
              f"{str(x['thinking'] or '-'):8} {x['max_tokens']:>5} {x['run']:>3} {rec:>4} "
              f"{x['fp']:>3} {str(x['said_none']):>4} {str(x['steps']):>5} {str(x['tokens']):>6} {x['cost']:>9.5f}")
    return out


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "run":
        fixture, tag, model = sys.argv[2], sys.argv[3], sys.argv[4]
        run = sys.argv[5] if len(sys.argv) > 5 else "1"
        thinking = sys.argv[6] if len(sys.argv) > 6 and sys.argv[6] != "-" else None
        max_tokens = int(sys.argv[7]) if len(sys.argv) > 7 else 12000
        submit(fixture, tag, model, run, thinking, max_tokens)
    elif cmd == "wait":
        wait()
    elif cmd == "score":
        score()
    else:
        print(__doc__)
        raise SystemExit(2)


if __name__ == "__main__":
    main()