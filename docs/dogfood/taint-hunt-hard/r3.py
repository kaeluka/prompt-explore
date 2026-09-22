#!/usr/bin/env python3
"""Round-3 runner: single-agent baseline (and optional decompose) across models.

Subcommands:
  run <tag> <kind> <model> [run] [thinking]
        submit one investigation; append {tag,kind,model,run,thinking,id} to
        runs_r3.json. kind = baseline | decompose.
  wait                poll every id in runs_r3.json until terminal
  score [runs.json]   fetch evidence and score recall / false positives / cost

The baseline system prompt is the round-2 one, unchanged, so the only variable
is the model (or thinking level).

Usage examples:
  python3 r3.py run fix baseline open_router::openai/gpt-4.1-nano 1
  python3 r3.py wait
  python3 r3.py score
"""
import importlib.util
import json
import os
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
SCENARIO = "scn-aeca952b99bf"
BASE = "http://127.0.0.1:8080"
RUNS = os.path.join(HERE, "runs_r3.json")

# Same taint-audit system prompt as round 2's single-agent baseline.
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


def submit(kind, model, tag, run, thinking=None, max_tokens=3000):
    controls = {"temperature": 0, "max_tokens": max_tokens}
    if thinking:
        controls["thinking"] = thinking
    if kind == "baseline":
        program = read("baseline_r3.lua")
        params = {"model": model, "system": BASELINE_SYSTEM, "controls": controls}
    elif kind == "decompose":
        program = read("decompose_r3.lua")
        params = {
            "model": model,
            "enumerate": ENUMERATE_SYSTEM,
            "verify": VERIFY_SYSTEM,
            "controls": controls,
        }
    else:
        raise SystemExit("kind must be baseline or decompose")
    payload = {
        "scenario_id": SCENARIO,
        "investigation": {
            "budget": {"max_steps_per_trace": 20, "max_tokens": 40000},
            "reason": f"round-3 {kind} sweep: model={model} thinking={thinking or 'default'}",
        },
        "workflow": {"lua_source": program, "params": params},
        "attributes": {
            "variant": f"{kind}-r3",
            "model": model.split("::")[-1],
            "tag": tag,
            "run": str(run),
        },
    }
    r = post(payload)
    entry = {"tag": tag, "kind": kind, "model": model, "run": run,
             "thinking": thinking, "id": r["id"]}
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
    ids = [r["id"] for r in runs]
    for _ in range(120):
        states = []
        for i in ids:
            d = get(f"{BASE}/api/investigations/{i}")
            states.append((i, d["status"]))
        running = [i for i, s in states if s not in ("done", "failed")]
        if not running:
            print("all terminal")
            for i, s in states:
                print(i[:8], s)
            return
        print(f"{len(running)} running")
        time.sleep(4)
    raise SystemExit("timeout")


def score(runs_file=None):
    runs_file = runs_file or RUNS
    spec = importlib.util.spec_from_file_location("sc", os.path.join(HERE, "score.py"))
    sc = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(sc)
    runs = json.load(open(runs_file))
    out = []
    for r in runs:
        d = get(f"{BASE}/api/investigations/{r['id']}/evidence")
        text = sc.collect_text(d)
        flows = sc.extract_lines(text)
        found, fpos, unknown = set(), set(), []
        for src, snk in flows:
            matched = False
            for name, gt in sc.REAL.items():
                if src == gt["source"] and snk in gt["sinks"]:
                    found.add(name); matched = True
            for name, gt in sc.DECOYS.items():
                if snk in gt["sinks"] or (gt["source"] and src == gt["source"]):
                    fpos.add(name); matched = True
            if not matched:
                unknown.append((src, snk))
        ex = d.get("execution", {})
        u = d.get("usage", {}).get("put", {})
        out.append({**r, "found": sorted(found), "recall": len(found) / 2.0,
                    "fpos": sorted(fpos), "fp": len(fpos), "unknown": unknown,
                    "flows": flows, "steps": ex.get("steps_used"),
                    "tokens": ex.get("put_tokens_used"), "cost": u.get("cost_usd"),
                    "stop": ex.get("stop_reason"),
                    "answer": text[:1500]})
    json.dump(out, open(runs_file.replace(".json", "_scored.json"), "w"), indent=2)
    hdr = f"{'kind':10} {'tag':16} {'model':34} {'thk':5} {'run':>3} {'rec':>4} {'fp':>3} {'steps':>5} {'tok':>6} {'cost$':>9}"
    print(hdr)
    for r in sorted(out, key=lambda x: (x["kind"], x["recall"], x["cost"] or 0)):
        print(f"{r['kind']:10} {r['tag']:16} {r['model'].split('::')[-1]:34} "
              f"{str(r['thinking'] or '-'):5} {r['run']:>3} {r['recall']:>4.2f} {r['fp']:>3} "
              f"{str(r['steps']):>5} {str(r['tokens']):>6} {r['cost']:>9.5f}")
    return out


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else ""
    if cmd == "run":
        tag, kind, model = sys.argv[2], sys.argv[3], sys.argv[4]
        run = sys.argv[5] if len(sys.argv) > 5 else "1"
        thinking = sys.argv[6] if len(sys.argv) > 6 and sys.argv[6] != "-" else None
        max_tokens = int(sys.argv[7]) if len(sys.argv) > 7 else 3000
        submit(kind, model, tag, run, thinking, max_tokens)
    elif cmd == "wait":
        wait()
    elif cmd == "score":
        score(sys.argv[2] if len(sys.argv) > 2 else None)
    else:
        print(__doc__)
        raise SystemExit(2)


if __name__ == "__main__":
    main()