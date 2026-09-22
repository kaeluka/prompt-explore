#!/usr/bin/env python3
"""Round 3: register the SAME fixture with a de-confounded tool contract.

Changes vs round 2 (the fixture bytes and ground truth are untouched):
  * `list_dir` is framed as the mandatory first step and says files live at the
    root, opened by the exact returned name (kills the phantom `app/` dir).
  * `grep` spells out that it is literal character-for-character with a concrete
    counterexample (`execute|query` does NOT match `execute`), because the host
    workspace grep has no regex mode.
  * the opening message tells the agent to call list_dir first.

Nothing about the taint task, the sources, the sinks, the decoys or the
expected answer changes.

Usage: python3 register_fixed.py [base_url]
"""
import importlib.util
import json
import os
import sys
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080"

spec = importlib.util.spec_from_file_location("reg", os.path.join(HERE, "register.py"))
reg = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reg)

LIST_DIR = (
    "STEP 1 \u2014 call this first. List the direct children of a directory in the "
    "repository. `path` is optional; omit it, or pass \"\" or \".\", for the "
    "repository root. The root is the only directory that exists: every source "
    "file lives directly in it, and read_file opens each by the exact `name` this "
    "returns with NO directory prefix (e.g. if entries include app.py, call "
    "read_file with path \"app.py\", not \"app/app.py\"). Returns "
    "{path, entries:[{name,kind:\"file\"|\"dir\"}], truncated}. An unknown "
    "directory returns {path, error:\"not found\"}."
)
READ_FILE = (
    "Read a UTF-8 text file from the repository. `path` is required and relative "
    "to the repository root; use the exact filename list_dir returned. Returns "
    "{path, content, truncated}; a missing file returns {path, error:\"not found\"}."
)
GREP = (
    "Search the repository for an EXACT, LITERAL, case-sensitive substring. This "
    "is NOT a regular expression and NOT a glob: every character is matched "
    "literally. `execute|query` searches for that exact 13-character text and "
    "does NOT match `execute` or `query`; `.*` searches for a literal dot and "
    "star. To search several tokens, make one call per token, e.g. grep(\"execute\") "
    "and grep(\"request\"). `pattern` is required; optional `path` restricts the "
    "search to one file or directory; optional `case_insensitive` boolean. Returns "
    "{pattern, matches:[{path,line,text}], truncated}."
)

USER_MESSAGE = (
    "Audit the repository in your workspace for security vulnerabilities. Start by "
    "calling list_dir with an empty path to see the exact filenames, then read those "
    "files by their exact names. Find every taint flow where untrusted input reaches "
    "a dangerous SQL sink and is actually exploitable. Note that some flows may look "
    "dangerous but are safely parameterized, cast, or whitelisted \u2014 those are NOT "
    "vulnerabilities and must not be reported. Report each real flow as: source "
    "file:line, sink file:line, and the propagation path."
)


def main():
    with open(os.path.join(HERE, "scenario.json")) as f:
        base = json.load(f)["scenario"]

    tools = []
    for t in base["tools"]:
        t = dict(t)
        t["description"] = {"list_dir": LIST_DIR, "read_file": READ_FILE, "grep": GREP}[t["name"]]
        tools.append(t)

    definition = {
        "world": base["world"],          # identical ground-truth narrative
        "input_domain": base["input_domain"],
        "user_message": USER_MESSAGE,
        "simulator_notes": base["simulator_notes"],
        "tools": tools,
        "simulation": base["simulation"],
    }
    request = {"label": "taint-hunt-hard-02-fixed-contract", "scenario": definition}
    with open(os.path.join(HERE, "scenario_fixed.json"), "w") as f:
        json.dump(request, f, indent=2)
        f.write("\n")

    body, ctype = reg.multipart({"request": json.dumps(request)}, {"workspace": ("w.zip", reg.make_zip())})
    req = urllib.request.Request(
        BASE + "/api/scenarios", data=body, headers={"Content-Type": ctype}, method="POST"
    )
    with urllib.request.urlopen(req) as r:
        result = json.load(r)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()