#!/usr/bin/env python3
"""Generate the wfb-ng link-layer test vectors in tests/fixtures/.

The Rust link layer in src/wfb/ is an independent implementation of the
wfb-ng protocol, so "it decodes its own output" would prove nothing. These
vectors come from the reference implementations instead:

  * the ciphers from libsodium, loaded through ctypes, which is what wfb-ng
    itself calls;
  * the erasure code from wfb-ng's own fec.c, compiled here into a temporary
    shared object.

Run it once when the protocol or the reference changes; the JSON it writes is
checked in, so the test suite needs neither libsodium nor a wfb-ng checkout.

    ./tools/gen_wfb_fixtures.py --wfb-ng ~/wfb-ng
"""

import argparse
import ctypes
import ctypes.util
import json
import os
import pathlib
import struct
import subprocess
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
FIXTURES = HERE.parent / "tests" / "fixtures"

# Sizes libsodium fixes for the primitives wfb-ng uses.
BOX_NONCE = 24
BOX_MAC = 16
KEY = 32
AEAD_TAG = 16

WFB_PACKET_DATA = 0x1
WFB_PACKET_SESSION = 0x2
WFB_FEC_VDM_RS = 0x1


def load_sodium():
    name = ctypes.util.find_library("sodium")
    if name is None:
        raise SystemExit("libsodium not found; install libsodium-dev")
    lib = ctypes.CDLL(name)
    if lib.sodium_init() < 0:
        raise SystemExit("sodium_init failed")
    return lib


def keypair(lib, seed):
    """A deterministic X25519 keypair, so the fixtures never change by luck."""
    pk = ctypes.create_string_buffer(KEY)
    sk = ctypes.create_string_buffer(KEY)
    lib.crypto_box_seed_keypair(pk, sk, ctypes.create_string_buffer(seed, KEY))
    return pk.raw, sk.raw


def box_seal(lib, plain, nonce, peer_pk, own_sk):
    out = ctypes.create_string_buffer(len(plain) + BOX_MAC)
    rc = lib.crypto_box_easy(
        out,
        ctypes.create_string_buffer(plain, len(plain)),
        ctypes.c_ulonglong(len(plain)),
        ctypes.create_string_buffer(nonce, BOX_NONCE),
        ctypes.create_string_buffer(peer_pk, KEY),
        ctypes.create_string_buffer(own_sk, KEY),
    )
    if rc != 0:
        raise SystemExit("crypto_box_easy failed")
    return out.raw


def aead_seal(lib, plain, aad, nonce, key):
    """The ORIGINAL ChaCha20-Poly1305, 8-byte nonce - not the IETF variant.

    crypto_aead_chacha20poly1305_encrypt is the one wfb-ng calls; the _ietf_
    suffixed function next to it in the same header is a different cipher and
    produces a different tag for the same inputs.
    """
    out = ctypes.create_string_buffer(len(plain) + AEAD_TAG)
    out_len = ctypes.c_ulonglong(0)
    rc = lib.crypto_aead_chacha20poly1305_encrypt(
        out,
        ctypes.byref(out_len),
        ctypes.create_string_buffer(plain, len(plain)),
        ctypes.c_ulonglong(len(plain)),
        ctypes.create_string_buffer(aad, len(aad)),
        ctypes.c_ulonglong(len(aad)),
        None,
        ctypes.create_string_buffer(nonce, 8),
        ctypes.create_string_buffer(key, KEY),
    )
    if rc != 0:
        raise SystemExit("crypto_aead_chacha20poly1305_encrypt failed")
    return out.raw[: out_len.value]


def build_fec(wfb_dir):
    """Compile wfb-ng's fec.c into a shared object and load it.

    Kept entirely inside a temporary directory: nothing from wfb-ng ends up in
    this repository, which matters because wfb-ng is GPL-3.0 and this project
    is GPL-2.0-only. The vectors it produces are data.
    """
    src = pathlib.Path(wfb_dir).expanduser() / "src" / "fec.c"
    if not src.exists():
        raise SystemExit(f"no fec.c at {src}; pass --wfb-ng <checkout>")

    tmp = tempfile.mkdtemp(prefix="wfb-fec-")
    so = os.path.join(tmp, "libfec.so")
    subprocess.run(
        ["cc", "-O2", "-fPIC", "-shared", "-o", so, str(src), f"-I{src.parent}"],
        check=True,
    )
    lib = ctypes.CDLL(so)
    lib.fec_new.restype = ctypes.c_void_p
    lib.fec_new.argtypes = [ctypes.c_uint16, ctypes.c_uint16]
    lib.fec_encode.argtypes = [
        ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_char_p),
        ctypes.POINTER(ctypes.c_char_p),
        ctypes.c_size_t,
    ]
    return lib


def fec_encode(lib, k, n, blocks, size):
    code = lib.fec_new(k, n)
    src = (ctypes.c_char_p * k)(*[bytes(b) for b in blocks])
    outs = [ctypes.create_string_buffer(size) for _ in range(n - k)]
    fecs = (ctypes.c_char_p * (n - k))(*[ctypes.cast(o, ctypes.c_char_p) for o in outs])
    lib.fec_encode(code, src, fecs, size)
    return [o.raw[:size] for o in outs]


def data_fragment(payload, fec_payload):
    """One FEC fragment: the 3-byte wpacket header then the payload, padded."""
    body = struct.pack(">BH", 0, len(payload)) + payload
    return body + b"\0" * (fec_payload - len(body))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--wfb-ng", default="~/wfb-ng", help="wfb-ng checkout, for fec.c")
    args = ap.parse_args()

    sodium = load_sodium()
    fec = build_fec(args.wfb_ng)

    drone_pk, drone_sk = keypair(sodium, b"drone-fixture-seed")
    gs_pk, gs_sk = keypair(sodium, b"ground-fixture-seed")

    # The two key files wfb_keygen writes: own secret then the peer's public.
    gs_key = gs_sk + drone_pk
    drone_key = drone_sk + gs_pk

    channel_id = (7669206 << 8) | 0
    fec_k, fec_n = 8, 12
    session_key = bytes(range(1, 33))
    epoch = 3

    # A session packet, exactly as wfb_tx emits it.
    session_plain = (
        struct.pack(">QIBBB", epoch, channel_id, WFB_FEC_VDM_RS, fec_k, fec_n)
        + session_key
    )
    session_nonce = bytes(range(100, 100 + BOX_NONCE))
    session_sealed = box_seal(sodium, session_plain, session_nonce, gs_pk, drone_sk)

    # AEAD vectors over payloads of assorted lengths, including the empty one
    # and one that lands exactly on a block boundary.
    aead = []
    for i, length in enumerate([0, 1, 15, 16, 17, 64, 1023, 1448]):
        block_idx, fragment_idx = 0x0102030405 + i, i % fec_n
        nonce = struct.pack(">Q", (block_idx << 8) | fragment_idx)
        aad = bytes([WFB_PACKET_DATA]) + nonce
        plain = bytes((j * 7 + i) % 256 for j in range(length))
        aead.append(
            {
                "nonce": nonce.hex(),
                "aad": aad.hex(),
                "plain": plain.hex(),
                "sealed": aead_seal(sodium, plain, aad, nonce, session_key).hex(),
            }
        )

    # A whole FEC block, encrypted fragment by fragment the way wfb_tx does,
    # so the aggregator can be driven with real packets.
    payloads = [bytes((i * 13 + j) % 256 for j in range(200 + 40 * i)) for i in range(fec_k)]
    fec_payload = max(len(p) for p in payloads) + 3
    fragments = [data_fragment(p, fec_payload) for p in payloads]
    parity = fec_encode(fec, fec_k, fec_n, fragments, fec_payload)

    block_idx = 42
    packets = []
    for idx, frag in enumerate(fragments + parity):
        nonce = struct.pack(">Q", (block_idx << 8) | idx)
        aad = bytes([WFB_PACKET_DATA]) + nonce
        packets.append(
            {
                "fragment": idx,
                "wire": (aad + aead_seal(sodium, frag, aad, nonce, session_key)).hex(),
            }
        )

    # Standalone FEC vectors, so fec.rs can be checked without the crypto.
    fec_cases = []
    for k, n, size in [(1, 3, 16), (4, 8, 64), (8, 12, 1449), (16, 24, 33)]:
        blocks = [bytes((i * 37 + j * 11 + 3) % 256 for j in range(size)) for i in range(k)]
        fec_cases.append(
            {
                "k": k,
                "n": n,
                "size": size,
                "data": [b.hex() for b in blocks],
                "parity": [p.hex() for p in fec_encode(fec, k, n, blocks, size)],
            }
        )

    out = {
        "note": "generated by tools/gen_wfb_fixtures.py from libsodium and wfb-ng's fec.c",
        "channel_id": channel_id,
        "gs_key": gs_key.hex(),
        "drone_key": drone_key.hex(),
        "session_key": session_key.hex(),
        "epoch": epoch,
        "fec_k": fec_k,
        "fec_n": fec_n,
        "session": {
            "nonce": session_nonce.hex(),
            "plain": session_plain.hex(),
            "sealed": session_sealed.hex(),
            "wire": (bytes([WFB_PACKET_SESSION]) + session_nonce + session_sealed).hex(),
        },
        "aead": aead,
        "block": {
            "block_idx": block_idx,
            "fec_payload": fec_payload,
            "payloads": [p.hex() for p in payloads],
            "packets": packets,
        },
        "fec": fec_cases,
    }

    FIXTURES.mkdir(parents=True, exist_ok=True)
    path = FIXTURES / "wfb_vectors.json"
    path.write_text(json.dumps(out, indent=1) + "\n")
    print(f"wrote {path} ({path.stat().st_size} bytes)")


if __name__ == "__main__":
    main()
