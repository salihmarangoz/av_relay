# SPDX-License-Identifier: GPL-3.0-or-later
"""A reference AV Relay client (docs/protocol.md, sections 4 to 7), for
interop tests against a server. Uses the independent crypto of
tests/vectors/generate.py (libsodium and noiseprotocol); no media.

    python tests/interop/reference_client.py pair HOST PORT CODE
    python tests/interop/reference_client.py session HOST PORT

`pair` saves the client key and the server's key to --state (default
./.interop-client.json); `session` uses them, says hello, asks for the
camera and microphone, pings, and prints everything it receives.
"""

import argparse
import base64
import json
import os
import socket
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "vectors"))
import generate as ref  # noqa: E402
from noise.connection import Keypair, NoiseConnection  # noqa: E402


class Framed:
    def __init__(self, host: str, port: int):
        self.sock = socket.create_connection((host, port), timeout=10)

    def send(self, payload: bytes) -> None:
        self.sock.sendall(struct.pack(">H", len(payload)) + payload)

    def recv(self) -> bytes:
        (length,) = struct.unpack(">H", self._exactly(2))
        return self._exactly(length)

    def _exactly(self, n: int) -> bytes:
        out = b""
        while len(out) < n:
            chunk = self.sock.recv(n - len(out))
            if not chunk:
                raise ConnectionError("closed by server")
            out += chunk
        return out


def preamble(mode: str) -> bytes:
    return ref.compact({"proto": "av-relay", "version": 1, "mode": mode})


def expect_type(data: bytes, wanted: str) -> dict:
    msg = json.loads(data)
    if msg.get("type") == "error":
        sys.exit(f"server error: {msg}")
    if msg.get("type") != wanted:
        sys.exit(f"expected {wanted}, got {msg}")
    return msg


def pair(args) -> None:
    static = os.urandom(32)
    conn = Framed(args.host, args.port)
    conn.send(preamble("pair"))
    sid_s = base64.b64decode(expect_type(conn.recv(), "pair-start")["sid"])
    sid_c = os.urandom(16)
    sid = sid_c + sid_s
    ya = ref.sample_scalar(os.urandom(32))
    g = ref.calculate_generator(args.code.encode(), ref.PAIR_CI, sid)
    Ya = ref.ristretto_scalarmult(ya, g)
    conn.send(ref.compact({"type": "cpace", "sid": ref.b64(sid_c), "Y": ref.b64(Ya)}))
    Yb = base64.b64decode(expect_type(conn.recv(), "cpace")["Y"])
    k = ref.scalar_mult_vfy(ya, Yb)
    if k == ref.IDENTITY:
        sys.exit("invalid point from server")
    psk = ref.pairing_psk(ref.isk_ir(sid, k, Ya, b"", Yb, b""))

    n = NoiseConnection.from_name(ref.PAIR_NOISE)
    n.set_as_initiator()
    n.set_keypair_from_private_bytes(Keypair.STATIC, static)
    n.set_psks(psk=psk)
    n.set_prologue(ref.PAIR_PROLOGUE_LABEL + sid + Ya + Yb)
    n.start_handshake()
    conn.send(n.write_message(b""))
    n.read_message(conn.recv())
    # noiseprotocol drops the handshake state when it completes: take the server's key now.
    server_key = n.noise_protocol.handshake_state.rs.public_bytes
    conn.send(n.write_message(ref.compact({"name": "reference-client", "app": "reference_client.py"})))
    try:
        paired = json.loads(n.decrypt(conn.recv()))
    except Exception as e:  # noqa: BLE001
        sys.exit(f"pairing rejected (wrong code?): {e!r}")
    print("paired:", paired)
    assert paired["device_id"] == ref.device_id(server_key), "device_id doesn't match the server key"
    Path(args.state).write_text(json.dumps({"static": static.hex(), "server": server_key.hex(),
                                            "server_device_id": paired["device_id"]}))
    print("saved", args.state)


def session(args) -> None:
    state = json.loads(Path(args.state).read_text())
    conn = Framed(args.host, args.port)
    conn.send(preamble("session"))
    n = NoiseConnection.from_name(ref.SESSION_NOISE)
    n.set_as_initiator()
    n.set_keypair_from_private_bytes(Keypair.STATIC, bytes.fromhex(state["static"]))
    n.set_keypair_from_public_bytes(Keypair.REMOTE_STATIC, bytes.fromhex(state["server"]))
    n.set_prologue(ref.SESSION_PROLOGUE)
    n.start_handshake()
    conn.send(n.write_message(b""))
    n.read_message(conn.recv())

    def send(msg: dict) -> None:
        conn.send(n.encrypt(ref.compact(msg)))

    def recv() -> dict:
        msg = json.loads(n.decrypt(conn.recv()))
        print("<-", msg)
        return msg

    send({"type": "hello", "name": "reference-client", "app": "reference_client.py",
          "video": {"width": 1280, "height": 720, "fps": 30}})
    welcome = recv()
    assert welcome["type"] == "welcome", welcome
    assert welcome["device_id"] == state["server_device_id"]
    send({"type": "media", "video": True, "audio": True})
    send({"type": "ping", "id": 1})
    conn.sock.settimeout(2)
    try:
        while True:
            if recv().get("type") == "pong":
                break
    except socket.timeout:
        sys.exit("no pong")
    send({"type": "bye", "reason": "quit"})
    print("session OK")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--state", default=".interop-client.json")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("pair")
    p.add_argument("host")
    p.add_argument("port", type=int)
    p.add_argument("code")
    s = sub.add_parser("session")
    s.add_argument("host")
    s.add_argument("port", type=int)
    args = ap.parse_args()
    {"pair": pair, "session": session}[args.cmd](args)


if __name__ == "__main__":
    main()
