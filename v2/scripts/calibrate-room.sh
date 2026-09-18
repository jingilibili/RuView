#!/usr/bin/env bash
# Empty-room runtime calibration, one command.
#
# The work lives in `calibrate-room.py` beside this file. It talks to the
# identity-bearing API the server now exposes: `POST /api/v1/calibration/start`
# and `stop` both require a room binding digest and the bound node set, and the
# source is chosen from the read-only `/api/v1/calibration/eligibility` route
# instead of a bodyless `start` probe (which that API refuses).
#
# Usage:
#   ./calibrate-room.sh --check                 # read-only: sources + status + verdict
#   ./calibrate-room.sh --yes                   # take a hold, no prompt
#   ./calibrate-room.sh --yes --p95-limit 13    # explicit quiet-hold gate
#   ./calibrate-room.sh --base http://host:8080 --room-label bedroom
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 1; }

exec python3 "$here/calibrate-room.py" "$@"
