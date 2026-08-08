#!/usr/bin/env bash
# Regenerate openapi.json from the server's annotated handlers/types.
# The spec is compiled from the code (utoipa), so it cannot drift:
# change a handler or type, re-run this script, get a fresh spec.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo run -q -p prompt-explore-server -- --dump-openapi > openapi.json
python3 scripts/openapi_to_md.py openapi.json API.md
