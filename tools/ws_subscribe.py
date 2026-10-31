#!/usr/bin/env python3
"""Subscribe to one or more Nano WebSocket topics on the bare-metal node.

Usage:
    ./tools/ws_subscribe.py [OPTIONS]
    python3 tools/ws_subscribe.py [OPTIONS]

Parameters:
    --host HOST           WebSocket server IP or hostname (default: 192.168.1.200)
    --port PORT           WebSocket server port (default: 7078)
    -t, --topic TOPIC     Topic to subscribe to (can be passed multiple times; default: confirmation).
                          Valid topics:
                            - confirmation
                            - started_election
                            - stopped_election
                            - vote
                            - active_difficulty
                            - work
                            - telemetry
                            - new_unconfirmed_block
                            - bootstrap
                            - in_pocket
                            - out_pocket
    -n, --count COUNT     Stop and exit after receiving COUNT topic messages (default: 0 = unlimited)
    --timeout SECONDS     Socket read timeout in seconds (default: 30.0)
    --options JSON        JSON string of topic filter options:
                            * confirmation options:
                              {
                                "include_block": true,
                                "include_election_info": true,
                                "include_election_info_with_votes": false,
                                "include_sideband_info": false,
                                "confirmation_type": "all" | "active" | "active_quorum"
                                                     | "active_confirmation_height" | "inactive",
                                "all_local_accounts": false,
                                "accounts": ["nano_..."]
                              }
                            * vote options:
                              {
                                "representatives": ["nano_..."],
                                "include_replays": false,
                                "include_indeterminate": false
                              }
    --no-ack              Disable subscription acknowledgement (ack: false)

Examples:
    # Stream confirmations continuously (Ctrl+C to stop)
    ./tools/ws_subscribe.py

    # Subscribe to confirmation and vote topics and exit after 5 messages
    ./tools/ws_subscribe.py -t confirmation -t vote -n 5

    # Subscribe to started_election on a custom host/port
    ./tools/ws_subscribe.py --host 192.168.1.200 --port 7078 -t started_election

    # Subscribe to active_quorum confirmations with election info included
    ./tools/ws_subscribe.py -t confirmation --options '{"confirmation_type":"active_quorum","include_election_info":true}'
"""

import argparse
import base64
import json
import os
import socket
import struct
import sys
import time
from typing import Any


VALID_TOPICS = (
    "confirmation",
    "started_election",
    "stopped_election",
    "vote",
    "active_difficulty",
    "work",
    "telemetry",
    "new_unconfirmed_block",
    "bootstrap",
    "in_pocket",
    "out_pocket",
)


def send_frame(sock: socket.socket, opcode: int, payload: bytes) -> None:
    mask_key = os.urandom(4)
    header = bytearray([0x80 | (opcode & 0x0F)])
    length = len(payload)
    if length < 126:
        header.append(0x80 | length)
    elif length < 65536:
        header.append(0x80 | 126)
        header.extend(struct.pack("!H", length))
    else:
        header.append(0x80 | 127)
        header.extend(struct.pack("!Q", length))
    header.extend(mask_key)
    masked = bytearray(b ^ mask_key[i % 4] for i, b in enumerate(payload))
    sock.sendall(header + masked)


def send_text_frame(sock: socket.socket, text: str) -> None:
    send_frame(sock, 0x1, text.encode("utf-8"))


class FrameReader:
    def __init__(self, sock: socket.socket, initial: bytes = b"") -> None:
        self.sock = sock
        self.buf = bytearray(initial)

    def recv_exact(self, n: int) -> bytes:
        while len(self.buf) < n:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise EOFError("Socket closed by peer")
            self.buf.extend(chunk)
        out = bytes(self.buf[:n])
        del self.buf[:n]
        return out

    def recv_frame(self) -> tuple[int, bytes]:
        first2 = self.recv_exact(2)
        opcode = first2[0] & 0x0F
        masked = (first2[1] & 0x80) != 0
        length = first2[1] & 0x7F
        if length == 126:
            length = struct.unpack("!H", self.recv_exact(2))[0]
        elif length == 127:
            length = struct.unpack("!Q", self.recv_exact(8))[0]
        mask_key = self.recv_exact(4) if masked else b""
        payload = self.recv_exact(length)
        if masked:
            payload = bytes(b ^ mask_key[i % 4] for i, b in enumerate(payload))
        return opcode, payload


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Subscribe to Nano WebSocket topics on the bare-metal node."
    )
    parser.add_argument(
        "--host",
        default="192.168.1.200",
        help="WebSocket server host/IP (default: 192.168.1.200)",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=7078,
        help="WebSocket server port (default: 7078)",
    )
    parser.add_argument(
        "-t",
        "--topic",
        action="append",
        dest="topics",
        choices=VALID_TOPICS,
        help="Topic(s) to subscribe to (can be specified multiple times; default: confirmation)",
    )
    parser.add_argument(
        "-n",
        "--count",
        type=int,
        default=0,
        help="Number of topic messages to receive before exiting (0 = unlimited)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=30.0,
        help="Socket read timeout in seconds (default: 30.0)",
    )
    parser.add_argument(
        "--options",
        type=str,
        default=None,
        help="Optional JSON object string passed as subscription options",
    )
    parser.add_argument(
        "--no-ack",
        action="store_true",
        help="Do not request subscription acknowledgement from the server",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    topics = args.topics or ["confirmation"]
    options_obj: dict[str, Any] | None = None
    if args.options is not None:
        options_obj = json.loads(args.options)

    t0 = time.time()
    print(f"[{time.time() - t0:.2f}s] Connecting to ws://{args.host}:{args.port} ...")

    with socket.create_connection((args.host, args.port), timeout=args.timeout) as sock:
        sock.settimeout(args.timeout)
        key = base64.b64encode(os.urandom(16)).decode("ascii")
        handshake = (
            f"GET / HTTP/1.1\r\n"
            f"Host: {args.host}:{args.port}\r\n"
            f"Upgrade: websocket\r\n"
            f"Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            f"Sec-WebSocket-Version: 13\r\n\r\n"
        )
        sock.sendall(handshake.encode("ascii"))

        resp = b""
        while b"\r\n\r\n" not in resp:
            chunk = sock.recv(1024)
            if not chunk:
                raise RuntimeError("Connection closed during HTTP upgrade")
            resp += chunk

        header_part, leftover = resp.split(b"\r\n\r\n", 1)
        status_line = header_part.splitlines()[0].decode("ascii", errors="replace")
        if "101" not in status_line:
            raise RuntimeError(f"WebSocket upgrade failed: {status_line}")
        print(f"[{time.time() - t0:.2f}s] Connected ({status_line})")

        reader = FrameReader(sock, leftover)

        for idx, topic in enumerate(topics, start=1):
            req: dict[str, Any] = {
                "action": "subscribe",
                "topic": topic,
                "ack": not args.no_ack,
                "id": f"sub-{topic}-{idx}",
            }
            if options_obj is not None:
                req["options"] = options_obj
            req_text = json.dumps(req)
            print(f"[{time.time() - t0:.2f}s] -> {req_text}")
            send_text_frame(sock, req_text)

        received_events = 0
        try:
            while True:
                opcode, payload = reader.recv_frame()
                if opcode == 0x1:  # Text frame
                    text = payload.decode("utf-8", errors="replace")
                    print(f"\n[{time.time() - t0:.2f}s] <- {text}")
                    try:
                        parsed = json.loads(text)
                        if "topic" in parsed:
                            received_events += 1
                    except json.JSONDecodeError:
                        received_events += 1

                    if args.count > 0 and received_events >= args.count:
                        break
                elif opcode == 0x9:  # Ping -> reply Pong
                    send_frame(sock, 0xA, payload)
                elif opcode == 0x8:  # Close
                    print(f"[{time.time() - t0:.2f}s] Server closed WebSocket connection")
                    break
        except KeyboardInterrupt:
            print(f"\n[{time.time() - t0:.2f}s] Interrupted by user, closing...")

        try:
            send_frame(sock, 0x8, b"")
        except OSError:
            pass

    return 0


if __name__ == "__main__":
    sys.exit(main())
