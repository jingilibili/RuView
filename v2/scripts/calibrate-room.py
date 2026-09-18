#!/usr/bin/env python3
"""Empty-room runtime calibration for the identity-bearing calibration API.

Why this exists: the server binds a calibration to a room identity, so `start`
and `stop` both require a binding digest and the node set, and the persisted
image of ADR-364 means a restart no longer discards a completed hold. The older
shell script probed `start` with no body, which the identity API refuses, so it
could no longer take a hold at all.

Usage:
  python3 calibrate-room.py --check                # read-only: sources + status + verdict
  python3 calibrate-room.py --yes                  # take a hold, no prompt
  python3 calibrate-room.py --yes --p95-limit 13   # explicit quiet-hold gate
  python3 calibrate-room.py --base http://host:8080 --room-label bedroom

The room binding digest is derived locally from the room label, so it is stable
across holds, reproducible, and never a secret.
"""

import argparse
import hashlib
import json
import sys
import time
import urllib.error
import urllib.request

P95_LIMIT_DEFAULT = 13.0
ACTIVE_POLL_S = 15
FEED_CHECK_S = 25


def call(base, path, body=None, timeout=30):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(
        base + path, data=data, headers={"Content-Type": "application/json"}
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return json.loads(response.read().decode() or "{}")
    except urllib.error.HTTPError as error:
        return {"_http_error": error.code, "_body": error.read().decode()[:300]}
    except Exception as error:  # noqa: BLE001 - operational script
        return {"_error": str(error)}


def binding_digest(label):
    return hashlib.sha256(("ruview.room-binding.v1|" + label).encode()).hexdigest()


def describe_status(status):
    reference = status.get("runtime_reference") or {}
    print("  status: %s | active: %s | binding_mode: %s" % (
        status.get("status"), status.get("active"), status.get("binding_mode")))
    restored = status.get("restored_calibration") or {}
    if restored.get("stored"):
        print("  restored calibration: model_id %s, active %s, vitals authorized %s" % (
            restored.get("model_id"), restored.get("active"),
            restored.get("numeric_vitals_authorized")))
    for key in ("residual_reference_p50", "residual_reference_p95",
                "residual_energy_threshold_effective", "residual_reference_window_count",
                "residual_reference_contaminated"):
        if key in reference:
            print("  %-38s %s" % (key, reference.get(key)))
    return reference


def quiet_verdict(reference, limit):
    p95 = reference.get("residual_reference_p95")
    if p95 is None:
        print("  verdict: retry (no reference statistics)")
        return False
    accepted = p95 <= limit
    print("  verdict: %s (p95 %.2f against the %.1f gate)" % (
        "accept" if accepted else "retry", p95, limit))
    return accepted


def choose_source(base):
    eligibility = call(base, "/api/v1/calibration/eligibility")
    if eligibility.get("_http_error"):
        raise SystemExit(
            "the eligibility route is missing (older build, or not this branch): %s"
            % json.dumps(eligibility)[:200])
    sources = eligibility.get("eligible_sources") or []
    if not sources:
        raise SystemExit("no eligible source: %s" % json.dumps(eligibility)[:300])
    for source in sources:
        evidence = source["evidence"]
        print("  node %s  grid %s  %.2f Hz  gap %.2f s  rssi_spread %s dB" % (
            source["source_node_id"], source["grid"]["n_subcarriers"],
            evidence["rate_hz"], evidence["max_gap_s"], evidence.get("rssi_spread_db")))
    best = sorted(
        sources,
        key=lambda s: (-s["evidence"]["rate_hz"], s["evidence"].get("rssi_spread_db") or 99),
    )[0]
    return best["source_node_id"]


def take_hold(base, label, limit, assume_yes):
    digest = binding_digest(label)
    status = call(base, "/api/v1/calibration/status")
    if status.get("_error"):
        raise SystemExit("no sensing server at %s" % base)
    print("== preflight ==")
    reference = describe_status(status)
    if status.get("active") and reference.get("residual_reference_p95") is not None:
        print("  a calibration is already finalized; reset it before taking another hold")
        return 2

    if not assume_yes:
        print("\nThe room must stay empty for the whole hold, and the room binding is")
        print("derived from the label %r as %s" % (label, digest))
        reply = input("Is the room empty now and will stay empty? [y/N] ")
        if reply.strip().lower() not in ("y", "yes"):
            raise SystemExit("aborted by operator")

    print("\n== choosing a source ==")
    node = choose_source(base)

    print("\n== starting ==")
    start = call(base, "/api/v1/calibration/start?source_node_id=%d" % node,
                 {"binding_digest": digest, "source_node_ids": [node]})
    if not start.get("success"):
        raise SystemExit("start failed: %s" % json.dumps(start)[:300])
    identity = {
        "boot_epoch": start["boot_epoch"],
        "session_id": start["session_id"],
        "binding_digest": start["binding_digest"],
        "source_node_ids": start["source_node_ids"],
    }
    print("  node %s, grid %s, model_id %s" % (
        node, start.get("source_grid"), start.get("model_id")))

    print("\n== verifying the feed (%d s) ==" % FEED_CHECK_S)
    time.sleep(FEED_CHECK_S)
    status = call(base, "/api/v1/calibration/status")
    if status.get("stalled"):
        call(base, "/api/v1/calibration/reset", identity)
        raise SystemExit("the bound grid is not arriving; the hold was reset, retry")
    print("  %.2f Hz, frames %s" % (
        status.get("frames_per_second") or 0, status.get("frame_count")))

    print("\n== collecting: do not enter the room ==")
    deadline = time.time() + 1800
    while True:
        status = call(base, "/api/v1/calibration/status")
        elapsed = status.get("elapsed_s") or 0
        need = status.get("min_duration_s") or 0
        frames = status.get("frame_count") or 0
        min_frames = status.get("min_frames") or 0
        print("  %4.0f/%4.0f s  frames %5s/%s  %.1f Hz  stalled=%s" % (
            elapsed, need, frames, min_frames, status.get("frames_per_second") or 0,
            status.get("stalled")))
        if status.get("stalled"):
            call(base, "/api/v1/calibration/reset", identity)
            raise SystemExit("collection stalled; the hold was reset")
        if elapsed >= need and frames >= min_frames:
            break
        if time.time() > deadline:
            raise SystemExit("gave up after 30 minutes")
        time.sleep(ACTIVE_POLL_S)

    print("\n== finalizing ==")
    stop = call(base, "/api/v1/calibration/stop", identity, timeout=60)
    if not stop.get("success"):
        raise SystemExit("stop failed: %s" % json.dumps(stop)[:300])
    print("  success: model_id %s, frames %s" % (stop.get("model_id"), stop.get("frame_count")))

    print("\n== resulting state ==")
    reference = describe_status(call(base, "/api/v1/calibration/status"))
    accepted = quiet_verdict(reference, limit)
    print("\n  the completed hold is persisted (ADR-364), so a restart keeps occupancy")
    print("  authority; numeric vitals still need a hold in the running process.")
    return 0 if accepted else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="http://localhost:8080")
    parser.add_argument("--room-label", default="roomB",
                        help="stable room identity the binding digest is derived from")
    parser.add_argument("--p95-limit", type=float, default=P95_LIMIT_DEFAULT,
                        help="a hold is accepted only when the reference p95 is at or below this")
    parser.add_argument("--yes", "-y", action="store_true", help="no empty-room prompt")
    parser.add_argument("--check", action="store_true",
                        help="read-only: report eligibility, status and the verdict, take no hold")
    args = parser.parse_args()

    if args.check:
        print("== read-only check ==")
        status = call(args.base, "/api/v1/calibration/status")
        if status.get("_error"):
            raise SystemExit("no sensing server at %s" % args.base)
        reference = describe_status(status)
        print("\n== eligible sources ==")
        choose_source(args.base)
        print("\n== quiet-hold verdict for the current reference ==")
        quiet_verdict(reference, args.p95_limit)
        return 0

    return take_hold(args.base, args.room_label, args.p95_limit, args.yes)


if __name__ == "__main__":
    sys.exit(main())
