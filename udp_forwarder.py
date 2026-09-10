#!/usr/bin/env python3
"""udp_forwarder.py - relay ESP32 CSI datagrams from the LAN to the WSL aggregator.

Why this exists
---------------
The ESP32 CSI nodes send raw CSI over UDP to the aggregator host that was baked
into their NVS provisioning (`target_ip`). The RuView CLI runs inside WSL, whose
interface lives on a NAT network (172.22.x.x) that is *not* routable from the
LAN, so the nodes cannot address WSL directly.

This script runs on the Windows host - which the nodes *can* reach on the LAN -
binds UDP :5005, and re-sends every datagram to the WSL IP.

    ESP32 (192.168.1.x) --UDP--> Windows :5005 --UDP--> WSL :5005 (CLI)

Usage
-----
    py -3 udp_forwarder.py
    py -3 udp_forwarder.py --target-ip 172.22.100.172 --target-port 5005

When --target-ip is omitted the WSL IP is auto-detected with
`wsl hostname -I`, so the forwarder keeps working after a WSL restart
changes the NAT address. Per-packet logging is replaced by a periodic
summary so long calibration runs do not flood the log.
"""

from __future__ import annotations

import argparse
import socket
import subprocess
import sys
import time

DEFAULT_PORT = 5005
DEFAULT_DISTRO = "Ubuntu-26.04"


def detect_wsl_ip(distro: str | None) -> str | None:
    """Return the first IPv4 address of the WSL distro, or None."""
    cmd = ["wsl.exe"] + (["-d", distro] if distro else []) + ["hostname", "-I"]
    try:
        out = subprocess.run(
            cmd, capture_output=True, text=True, timeout=10, check=False
        )
    except (OSError, subprocess.SubprocessError):
        return None
    for token in out.stdout.split():
        if token.count(".") == 3 and not token.startswith("127."):
            return token
    return None


def main() -> int:
    ap = argparse.ArgumentParser(description="Relay ESP32 CSI UDP to WSL.")
    ap.add_argument("--bind", default="0.0.0.0", help="local bind address")
    ap.add_argument("--port", type=int, default=DEFAULT_PORT, help="listen port")
    ap.add_argument(
        "--target-ip",
        default=None,
        help="WSL IP to forward to (auto-detected when omitted)",
    )
    ap.add_argument(
        "--target-port",
        type=int,
        default=None,
        help="destination port (defaults to --port)",
    )
    ap.add_argument(
        "--distro",
        default=DEFAULT_DISTRO,
        help="WSL distro used for auto-detection",
    )
    ap.add_argument(
        "--summary-s",
        type=float,
        default=10.0,
        help="seconds between throughput summaries (0 disables)",
    )
    args = ap.parse_args()

    target_ip = args.target_ip or detect_wsl_ip(args.distro)
    if not target_ip:
        print(
            "ERROR: could not determine the WSL IP; pass --target-ip explicitly.",
            file=sys.stderr,
        )
        return 2
    target_port = args.target_port or args.port

    sock_in = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock_in.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        sock_in.bind((args.bind, args.port))
    except OSError as exc:
        print(f"ERROR: cannot bind {args.bind}:{args.port}: {exc}", file=sys.stderr)
        return 1

    sock_out = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    dest = (target_ip, target_port)

    print(
        f"UDP forwarder listening {args.bind}:{args.port} -> {target_ip}:{target_port}",
        flush=True,
    )
    print("waiting for ESP32 CSI datagrams...", flush=True)

    total = 0
    bytes_total = 0
    per_source: dict[str, int] = {}
    last_report = time.monotonic()

    while True:
        try:
            data, addr = sock_in.recvfrom(65535)
        except KeyboardInterrupt:
            break
        except OSError as exc:
            print(f"recv error: {exc}", file=sys.stderr, flush=True)
            continue

        total += 1
        bytes_total += len(data)
        per_source[addr[0]] = per_source.get(addr[0], 0) + 1

        try:
            sock_out.sendto(data, dest)
        except OSError as exc:
            print(f"forward error to {dest}: {exc}", file=sys.stderr, flush=True)

        now = time.monotonic()
        if args.summary_s > 0 and now - last_report >= args.summary_s:
            window = now - last_report
            srcs = ", ".join(
                f"{ip}:{n}" for ip, n in sorted(per_source.items(), key=lambda kv: -kv[1])
            )
            print(
                f"[{time.strftime('%H:%M:%S')}] {total} pkts "
                f"({bytes_total / 1024:.1f} KiB, {total / window:.0f} pkt/s) "
                f"from {srcs}",
                flush=True,
            )
            last_report = now

    sock_in.close()
    sock_out.close()
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(0)
