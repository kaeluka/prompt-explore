#!/usr/bin/env bash
# Start the prompt-explore server detached so it SURVIVES across agent
# tool calls. A plain `&` / `nohup &` / `disown` keeps the process in the
# harness shell's process group, so the process is reaped when the tool
# call returns (and macOS has no `setsid`). Spawning via Python with
# start_new_session=True (setsid) detaches it into its own session.
set -euo pipefail
cd "$(dirname "$0")/.."
pkill -f prompt-explore-server 2>/dev/null || true
sleep 1
ZAI_API_KEY="${ZAI_API_KEY:-}" OPEN_ROUTER_API_KEY="${OPEN_ROUTER_API_KEY:-}" python3 - <<'PY'
import os, subprocess
subprocess.Popen(
    ["target/debug/prompt-explore-server"],
    stdout=open("/tmp/pe-server.log", "wb"),
    stderr=subprocess.STDOUT,
    stdin=subprocess.DEVNULL,
    start_new_session=True,
    env=os.environ,
    cwd=os.getcwd(),
)
print("spawned")
PY
sleep 3
curl -s -o /dev/null -w "server up: %{http_code}\n" http://127.0.0.1:8080/openapi.json
