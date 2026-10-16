# SPDX-License-Identifier: GPL-3.0-or-later
"""Reference implementation of AV Relay's pairing and session crypto, used
to generate the shared test vectors in this folder.

It is independent of both apps: CPace via libsodium (ctypes), Noise via the
`noiseprotocol` package. Before writing anything it checks itself against
the published vectors (CPace draft appendix B.3, cacophony for Noise).

    python3 -m venv .venv-vectors
    .venv-vectors/bin/pip install -r tests/vectors/requirements.txt
    .venv-vectors/bin/python tests/vectors/generate.py          # write
    .venv-vectors/bin/python tests/vectors/generate.py --check  # compare

Needs libsodium (macOS: brew install libsodium; Ubuntu: libsodium23).
See docs/protocol.md for what each value means.
"""

import argparse
import ctypes
import ctypes.util
import hashlib
import hmac
import json
import sys
import warnings
from pathlib import Path

from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat
from noise.connection import Keypair, NoiseConnection

HERE = Path(__file__).resolve().parent
# noiseprotocol warns when ephemeral keys are fixed; that's the point of test vectors.
warnings.filterwarnings("ignore", message="One of ephemeral keypairs is already set")

# --- libsodium -------------------------------------------------------------

_path = ctypes.util.find_library("sodium")
for _candidate in (_path, "/opt/homebrew/lib/libsodium.dylib", "/usr/local/lib/libsodium.dylib", "libsodium.so.23"):
    if not _candidate:
        continue
    try:
        sodium = ctypes.CDLL(_candidate)
        break
    except OSError:
        continue
else:
    sys.exit("libsodium not found (macOS: brew install libsodium; Ubuntu: apt install libsodium23)")
if sodium.sodium_init() < 0:
    sys.exit("sodium_init failed")


def ristretto_from_hash(h64: bytes) -> bytes:
    assert len(h64) == 64
    out = ctypes.create_string_buffer(32)
    sodium.crypto_core_ristretto255_from_hash(out, h64)
    return out.raw


def ristretto_scalarmult(scalar: bytes, point: bytes) -> bytes | None:
    """encode(scalar * decode(point)); None if decoding fails or the result is the identity."""
    out = ctypes.create_string_buffer(32)
    if sodium.crypto_scalarmult_ristretto255(out, scalar, point) != 0:
        return None
    return out.raw


# --- CPace (draft-irtf-cfrg-cpace-21), CPACE-RISTR255-SHA512 ------------------

DSI = b"CPaceRistretto255"
IDENTITY = bytes(32)
S_IN_BYTES = 128  # SHA-512 input block size


def prepend_len(data: bytes) -> bytes:
    length, out = len(data), b""
    while True:
        out += bytes([length]) if length < 128 else bytes([(length & 0x7F) | 0x80])
        length >>= 7
        if length == 0:
            return out + data


def lv_cat(*args: bytes) -> bytes:
    return b"".join(prepend_len(a) for a in args)


def generator_string(prs: bytes, ci: bytes, sid: bytes) -> bytes:
    zpad = max(0, S_IN_BYTES - 1 - len(prepend_len(prs)) - len(prepend_len(DSI)))
    return lv_cat(DSI, prs, bytes(zpad), ci, sid)


def calculate_generator(prs: bytes, ci: bytes, sid: bytes) -> bytes:
    return ristretto_from_hash(hashlib.sha512(generator_string(prs, ci, sid)).digest())


def scalar_mult_vfy(y: bytes, x: bytes) -> bytes:
    return ristretto_scalarmult(y, x) or IDENTITY


def isk_ir(sid: bytes, k: bytes, ya: bytes, ada: bytes, yb: bytes, adb: bytes) -> bytes:
    transcript = lv_cat(ya, ada) + lv_cat(yb, adb)
    return hashlib.sha512(lv_cat(DSI + b"_ISK", sid, k) + transcript).digest()


def sample_scalar(seed: bytes) -> bytes:
    """Deterministic stand-in for G.sample_scalar(): 32 bytes, bits >= 252 cleared."""
    s = bytearray(seed[:32])
    s[31] &= 0x0F
    return bytes(s)


# --- AV Relay definitions (docs/protocol.md) ---------------------------------

PAIR_CI = lv_cat(b"av-relay/1 pairing", b"client", b"server")
PAIR_PROLOGUE_LABEL = b"av-relay/1 pair"
SESSION_PROLOGUE = b"av-relay/1 session"
PSK_INFO = b"av-relay/1 pairing psk"
PAIR_NOISE = b"Noise_XXpsk3_25519_ChaChaPoly_SHA256"
SESSION_NOISE = b"Noise_IK_25519_ChaChaPoly_SHA256"


def pairing_psk(isk: bytes) -> bytes:
    return HKDF(algorithm=hashes.SHA256(), length=32, salt=None, info=PSK_INFO).derive(isk)


def device_id(static_public: bytes) -> str:
    return hashlib.sha256(static_public).digest()[:8].hex()


def x25519_public(private: bytes) -> bytes:
    return X25519PrivateKey.from_private_bytes(private).public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)


def frame(payload: bytes) -> bytes:
    assert len(payload) <= 0xFFFF
    return len(payload).to_bytes(2, "big") + payload


def det(label: str, n: int) -> bytes:
    """Fixed 'random' bytes so the vectors are reproducible."""
    return hashlib.sha512(b"av-relay test vector " + label.encode()).digest()[:n]


def compact(obj) -> bytes:
    return json.dumps(obj, separators=(",", ":"), ensure_ascii=False).encode()


def noise(name: bytes, initiator: bool, static: bytes, ephemeral: bytes, prologue: bytes,
          remote_static: bytes | None = None, psk: bytes | None = None) -> NoiseConnection:
    n = NoiseConnection.from_name(name)
    n.set_as_initiator() if initiator else n.set_as_responder()
    n.set_keypair_from_private_bytes(Keypair.STATIC, static)
    n.set_keypair_from_private_bytes(Keypair.EPHEMERAL, ephemeral)
    if remote_static is not None:
        n.set_keypair_from_public_bytes(Keypair.REMOTE_STATIC, remote_static)
    if psk is not None:
        n.set_psks(psk=psk)
    n.set_prologue(prologue)
    n.start_handshake()
    return n


# --- self-checks against published vectors ------------------------------------

def check_cpace_draft() -> dict:
    """CPace draft-21 appendix B.3 (ristretto255, SHA-512), verbatim."""
    h = bytes.fromhex
    v = {
        "PRS": b"Password",
        "CI": h("0b415f696e69746961746f720b425f726573706f6e646572"),
        "sid": h("7e4b4791d6a8ef019b936c79fb7f2c57"),
        "g": h("222b6b195fe84b1652badb6f6a3ae3d24341e7306967f0b8115b40d5698c7e56"),
        "ya": h("da3d23700a9e5699258aef94dc060dfda5ebb61f02a5ea77fad53f4ff0976d08"),
        "ADa": b"ADa",
        "Ya": h("d6bac480f2c386c394efc7c47adb9925dcd2630b64f240c50f8d0eec482b9157"),
        "yb": h("d2316b454718c35362d83d69df6320f38578ed5984651435e2949762d900b80d"),
        "ADb": b"ADb",
        "Yb": h("3ea7e0b19560d7c0b0f5734f63b955286dfa8232b5ebe63324e2d9e7433f7258"),
        "K": h("80b69a8a76457ab6a4d7f887a4bf6b55a2f80ac19c333f917a05fc9887c8b40f"),
        "ISK_IR": h("b69effbf61b51d56401c0f65601abe428de8206feaaf0e32198896dc"
                    "ae7b35cd2b38950a39dfd5d4a79164614c2984f7daa460b588c1e80c3fa2068af7900447"),
        "valid_s": h("7cd0e075fa7955ba52c02759a6c90dbbfc10e6d40aea8d283e407d88cf538a05"),
        "valid_X": h("2c3c6b8c4f3800e7aef6864025b4ed79bd599117e427c41bd47d93d654b4a51c"),
        "valid_sX": h("7c13645fe790a468f62c39beb7388e541d8405d1ade69d1778c5fe3e7f6b600e"),
        "invalid_Y1": h("2b3c6b8c4f3800e7aef6864025b4ed79bd599117e427c41bd47d93d654b4a51c"),
        "invalid_Y2": IDENTITY,
    }
    assert prepend_len(b"") == h("00") and prepend_len(bytes(range(128)))[:2] == h("8001")
    assert lv_cat(b"1234", b"5", b"", b"678") == h("043132333401350003363738")
    g = calculate_generator(v["PRS"], v["CI"], v["sid"])
    assert g == v["g"], "generator"
    assert ristretto_scalarmult(v["ya"], g) == v["Ya"], "Ya"
    assert ristretto_scalarmult(v["yb"], g) == v["Yb"], "Yb"
    assert scalar_mult_vfy(v["ya"], v["Yb"]) == v["K"] == scalar_mult_vfy(v["yb"], v["Ya"]), "K"
    assert isk_ir(v["sid"], v["K"], v["Ya"], v["ADa"], v["Yb"], v["ADb"]) == v["ISK_IR"], "ISK"
    assert scalar_mult_vfy(v["valid_s"], v["valid_X"]) == v["valid_sX"], "valid scalar_mult"
    assert scalar_mult_vfy(v["valid_s"], v["invalid_Y1"]) == IDENTITY, "invalid Y1"
    assert scalar_mult_vfy(v["valid_s"], v["invalid_Y2"]) == IDENTITY, "invalid Y2"
    return {k: (val.hex() if isinstance(val, bytes) else val) for k, val in v.items()} | {
        "PRS": v["PRS"].hex(), "ADa": v["ADa"].hex(), "ADb": v["ADb"].hex()}


def check_noise_cacophony(vectors: list[dict]) -> None:
    """Replay the cacophony vectors through noiseprotocol (one-way: initiator writes, responder reads)."""
    for v in vectors:
        name = v["protocol_name"].encode()
        psks = [bytes.fromhex(p) for p in v.get("init_psks", [])]
        i = noise(name, True, bytes.fromhex(v["init_static"]), bytes.fromhex(v["init_ephemeral"]),
                  bytes.fromhex(v["init_prologue"]),
                  bytes.fromhex(v["init_remote_static"]) if "init_remote_static" in v else None,
                  psks[0] if psks else None)
        r = noise(name, False, bytes.fromhex(v["resp_static"]), bytes.fromhex(v["resp_ephemeral"]),
                  bytes.fromhex(v["resp_prologue"]), None, psks[0] if psks else None)
        writer, reader = i, r
        for m in v["messages"]:
            payload, ciphertext = bytes.fromhex(m["payload"]), bytes.fromhex(m["ciphertext"])
            if writer.handshake_finished:
                out = writer.encrypt(payload)
                assert reader.decrypt(out) == payload
            else:
                out = writer.write_message(payload)
                assert reader.read_message(out) == payload
            assert out == ciphertext, f"{v['protocol_name']}: message mismatch"
            writer, reader = reader, writer
        if "handshake_hash" in v:
            assert i.get_handshake_hash() == bytes.fromhex(v["handshake_hash"])


# --- AV Relay vectors -----------------------------------------------------------

def pairing_vector() -> dict:
    code = "042137"
    client_static, server_static = det("client static", 32), det("server static", 32)
    client_pub, server_pub = x25519_public(client_static), x25519_public(server_static)
    sid_c, sid_s = det("sid_c", 16), det("sid_s", 16)
    sid = sid_c + sid_s
    ya, yb = sample_scalar(det("ya", 32)), sample_scalar(det("yb", 32))

    g = calculate_generator(code.encode(), PAIR_CI, sid)
    Ya, Yb = ristretto_scalarmult(ya, g), ristretto_scalarmult(yb, g)
    k = scalar_mult_vfy(ya, Yb)
    assert k != IDENTITY and k == scalar_mult_vfy(yb, Ya)
    isk = isk_ir(sid, k, Ya, b"", Yb, b"")
    psk = pairing_psk(isk)
    prologue = PAIR_PROLOGUE_LABEL + sid + Ya + Yb

    wrong_isk = isk_ir(sid, scalar_mult_vfy(ya, ristretto_scalarmult(yb, calculate_generator(b"042138", PAIR_CI, sid))),
                       Ya, b"", ristretto_scalarmult(yb, calculate_generator(b"042138", PAIR_CI, sid)), b"")

    c = noise(PAIR_NOISE, True, client_static, det("client ephemeral (pair)", 32), prologue, psk=psk)
    s = noise(PAIR_NOISE, False, server_static, det("server ephemeral (pair)", 32), prologue, psk=psk)
    client_info = {"name": "ubuntu-desktop", "app": "av-relay 0.1.0 linux"}
    paired = {"type": "paired", "name": "MacBook Air", "device_id": device_id(server_pub), "app": "AV Relay 0.1.0 macOS"}
    m1 = c.write_message(b"")
    assert s.read_message(m1) == b""
    m2 = s.write_message(b"")
    assert c.read_message(m2) == b""
    m3 = c.write_message(compact(client_info))
    assert s.read_message(m3) == compact(client_info)
    assert c.handshake_finished and s.handshake_finished
    t1 = s.encrypt(compact(paired))
    assert c.decrypt(t1) == compact(paired)

    preamble = compact({"proto": "av-relay", "version": 1, "mode": "pair"})
    start = compact({"type": "pair-start", "sid": b64(sid_s)})
    cpace_c = compact({"type": "cpace", "sid": b64(sid_c), "Y": b64(Ya)})
    cpace_s = compact({"type": "cpace", "Y": b64(Yb)})

    return {
        "description": "One complete pairing (docs/protocol.md, 'Pairing'). Fixed randomness; "
                       "JSON texts are examples (any valid JSON encoding of the same object is fine), "
                       "but every binary value must match.",
        "code": code,
        "cpace": {
            "suite": "CPACE-RISTR255-SHA512 (draft-irtf-cfrg-cpace-21)",
            "PRS": code.encode().hex(),
            "CI": PAIR_CI.hex(),
            "sid_c": sid_c.hex(), "sid_s": sid_s.hex(), "sid": sid.hex(),
            "generator_string": generator_string(code.encode(), PAIR_CI, sid).hex(),
            "g": g.hex(),
            "ya": ya.hex(), "Ya": Ya.hex(), "ADa": "",
            "yb": yb.hex(), "Yb": Yb.hex(), "ADb": "",
            "K": k.hex(),
            "ISK": isk.hex(),
            "ISK_if_server_code_were_042138": wrong_isk.hex(),
        },
        "psk": psk.hex(),
        "noise": {
            "protocol": PAIR_NOISE.decode(),
            "prologue": prologue.hex(),
            "client_static_private": client_static.hex(), "client_static_public": client_pub.hex(),
            "server_static_private": server_static.hex(), "server_static_public": server_pub.hex(),
            "client_ephemeral_private": det("client ephemeral (pair)", 32).hex(),
            "server_ephemeral_private": det("server ephemeral (pair)", 32).hex(),
            "msg1": m1.hex(), "msg2": m2.hex(),
            "msg3_payload": compact(client_info).decode(), "msg3": m3.hex(),
            "handshake_hash": c.get_handshake_hash().hex(),
            "transport_s2c_0_plaintext": compact(paired).decode(),
            "transport_s2c_0": t1.hex(),
        },
        "device_id": {"client": device_id(client_pub), "server": device_id(server_pub)},
        "wire": [
            {"dir": "c2s", "what": "preamble", "frame": frame(preamble).hex()},
            {"dir": "s2c", "what": "pair-start", "frame": frame(start).hex()},
            {"dir": "c2s", "what": "cpace", "frame": frame(cpace_c).hex()},
            {"dir": "s2c", "what": "cpace", "frame": frame(cpace_s).hex()},
            {"dir": "c2s", "what": "noise msg1", "frame": frame(m1).hex()},
            {"dir": "s2c", "what": "noise msg2", "frame": frame(m2).hex()},
            {"dir": "c2s", "what": "noise msg3", "frame": frame(m3).hex()},
            {"dir": "s2c", "what": "paired", "frame": frame(t1).hex()},
        ],
    }


def session_vector() -> dict:
    client_static, server_static = det("client static", 32), det("server static", 32)
    server_pub = x25519_public(server_static)
    c = noise(SESSION_NOISE, True, client_static, det("client ephemeral (session)", 32), SESSION_PROLOGUE,
              remote_static=server_pub)
    s = noise(SESSION_NOISE, False, server_static, det("server ephemeral (session)", 32), SESSION_PROLOGUE)
    m1 = c.write_message(b"")
    assert s.read_message(m1) == b""
    m2 = s.write_message(b"")
    assert c.read_message(m2) == b""
    hello = {"type": "hello", "name": "ubuntu-desktop", "app": "av-relay 0.1.0 linux",
             "video": {"width": 1280, "height": 720, "fps": 30}}
    welcome = {"type": "welcome", "name": "MacBook Air", "app": "AV Relay 0.1.0 macOS",
               "camera": "FaceTime HD Camera", "mic": "MacBook Air Microphones"}
    t_c0 = c.encrypt(compact(hello))
    assert s.decrypt(t_c0) == compact(hello)
    t_s0 = s.encrypt(compact(welcome))
    assert c.decrypt(t_s0) == compact(welcome)
    preamble = compact({"proto": "av-relay", "version": 1, "mode": "session"})
    return {
        "description": "One session handshake and the first message each way (docs/protocol.md, 'Session').",
        "noise": {
            "protocol": SESSION_NOISE.decode(),
            "prologue": SESSION_PROLOGUE.hex(),
            "client_static_private": client_static.hex(), "client_static_public": x25519_public(client_static).hex(),
            "server_static_private": server_static.hex(), "server_static_public": server_pub.hex(),
            "client_ephemeral_private": det("client ephemeral (session)", 32).hex(),
            "server_ephemeral_private": det("server ephemeral (session)", 32).hex(),
            "msg1": m1.hex(), "msg2": m2.hex(),
            "handshake_hash": c.get_handshake_hash().hex(),
            "transport_c2s_0_plaintext": compact(hello).decode(), "transport_c2s_0": t_c0.hex(),
            "transport_s2c_0_plaintext": compact(welcome).decode(), "transport_s2c_0": t_s0.hex(),
        },
        "wire": [
            {"dir": "c2s", "what": "preamble", "frame": frame(preamble).hex()},
            {"dir": "c2s", "what": "noise msg1", "frame": frame(m1).hex()},
            {"dir": "s2c", "what": "noise msg2", "frame": frame(m2).hex()},
            {"dir": "c2s", "what": "hello", "frame": frame(t_c0).hex()},
            {"dir": "s2c", "what": "welcome", "frame": frame(t_s0).hex()},
        ],
    }


def b64(data: bytes) -> str:
    import base64
    return base64.b64encode(data).decode()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="compare with the files instead of writing them")
    ap.add_argument("--cacophony", type=Path, help="cacophony.txt, to refresh noise-cacophony.json from it")
    args = ap.parse_args()

    cacophony_file = HERE / "noise-cacophony.json"
    if args.cacophony:
        wanted = {PAIR_NOISE.decode(), SESSION_NOISE.decode()}
        picked = [v for v in json.loads(args.cacophony.read_text())["vectors"] if v["protocol_name"] in wanted]
        assert {v["protocol_name"] for v in picked} == wanted
        cacophony = {"source": "https://github.com/haskell-cryptography/cacophony/blob/master/vectors/cacophony.txt",
                     "vectors": picked}
    else:
        cacophony = json.loads(cacophony_file.read_text())
    check_noise_cacophony(cacophony["vectors"])

    outputs = {
        "cpace-draft.json": {"description": "draft-irtf-cfrg-cpace-21 appendix B.3 (CPACE-RISTR255-SHA512), "
                                            "initiator-responder; invalid_Y1/Y2 must make scalar_mult_vfy return the identity.",
                             "vectors": check_cpace_draft()},
        "noise-cacophony.json": cacophony,
        "pairing.json": pairing_vector(),
        "session.json": session_vector(),
    }
    failed = False
    for name, obj in outputs.items():
        text = json.dumps(obj, indent=2, ensure_ascii=False) + "\n"
        path = HERE / name
        if args.check:
            if not path.exists() or path.read_text() != text:
                print(f"DIFFERS: {name}")
                failed = True
        else:
            path.write_text(text)
            print(f"wrote {path.relative_to(HERE.parent.parent)}")
    if args.check:
        print("vectors differ" if failed else "vectors match")
        sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
