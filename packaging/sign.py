#!/usr/bin/env python3
"""Signs release archives with Portal's release key, in minisign format.

    python3 packaging/sign.py keygen SECRET_FILE # new key pair; secret to file
    python3 packaging/sign.py sign FILE...       # writes FILE.minisig
    python3 packaging/sign.py verify KEY FILE... # checks FILE.minisig

`sign` reads the secret key from the PORTAL_SIGNING_KEY environment variable
(a GitHub secret in the release workflow). The public key is built into
`portal update`, which refuses archives without a valid signature, and
anyone can check a download with the standard tool:

    minisign -Vm portal-v1.2.3-x86_64-unknown-linux-gnu.tar.gz -P <public key>

The trusted comment names the file, so a validly signed archive cannot be
passed off as another release or platform. Needs the `cryptography` package.
"""

import base64
import hashlib
import os
import pathlib
import secrets
import sys

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import (
    Ed25519PrivateKey,
    Ed25519PublicKey,
)
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

# minisign: "Ed" for keys, "ED" for signatures over the BLAKE2b-512 of the file.
KEY_ALG = b"Ed"
SIG_ALG = b"ED"


def public_key_line(private: Ed25519PrivateKey, key_id: bytes) -> str:
    public = private.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
    return base64.b64encode(KEY_ALG + key_id + public).decode()


def keygen(secret_file: str) -> None:
    # The secret goes only into the file, never to the terminal or its
    # scrollback; refuse to overwrite a key that may be the only copy.
    key_id = secrets.token_bytes(8)
    seed = secrets.token_bytes(32)
    private = Ed25519PrivateKey.from_private_bytes(seed)
    with open(secret_file, "x", encoding="ascii") as out:
        out.write(base64.b64encode(KEY_ALG + key_id + seed).decode() + "
")
    print(f"Secret key written to {secret_file}; keep it private and backed up.")
    print("Public key (for RELEASE_KEYS in crates/portal-cli/src/update.rs):")
    print(public_key_line(private, key_id))


def load_secret() -> tuple[Ed25519PrivateKey, bytes]:
    encoded = os.environ.get("PORTAL_SIGNING_KEY", "").strip()
    if not encoded:
        sys.exit("PORTAL_SIGNING_KEY is not set; releases must be signed (see docs/RELEASING.md)")
    raw = base64.b64decode(encoded)
    if len(raw) != 42 or raw[:2] != KEY_ALG:
        sys.exit("PORTAL_SIGNING_KEY is not a key made by `sign.py keygen`")
    return Ed25519PrivateKey.from_private_bytes(raw[10:]), raw[2:10]


def sign(paths: list[str]) -> None:
    private, key_id = load_secret()
    print(f"Signing with public key {public_key_line(private, key_id)}")
    for name in paths:
        path = pathlib.Path(name)
        digest = hashlib.blake2b(path.read_bytes(), digest_size=64).digest()
        signature = private.sign(digest)
        trusted = f"file:{path.name}"
        global_signature = private.sign(signature + trusted.encode())
        path.with_name(path.name + ".minisig").write_text(
            "untrusted comment: signature from the Portal release key\n"
            + base64.b64encode(SIG_ALG + key_id + signature).decode() + "\n"
            + f"trusted comment: {trusted}\n"
            + base64.b64encode(global_signature).decode() + "\n",
            encoding="ascii",
        )
        print(f"Signed {path.name}")


def verify(public_line: str, paths: list[str]) -> None:
    raw = base64.b64decode(public_line)
    if len(raw) != 42 or raw[:2] != KEY_ALG:
        sys.exit("not a minisign public key")
    key_id, public = raw[2:10], Ed25519PublicKey.from_public_bytes(raw[10:])
    for name in paths:
        path = pathlib.Path(name)
        lines = path.with_name(path.name + ".minisig").read_text(encoding="ascii").splitlines()
        sig = base64.b64decode(lines[1])
        trusted = lines[2].removeprefix("trusted comment: ")
        try:
            if sig[:2] != SIG_ALG or sig[2:10] != key_id:
                raise InvalidSignature
            digest = hashlib.blake2b(path.read_bytes(), digest_size=64).digest()
            public.verify(sig[10:], digest)
            public.verify(base64.b64decode(lines[3]), sig[10:] + trusted.encode())
            if trusted != f"file:{path.name}":
                raise InvalidSignature
        except InvalidSignature:
            sys.exit(f"BAD signature: {path.name}")
        print(f"Good signature: {path.name}")


def main() -> None:
    match sys.argv[1:]:
        case ["keygen", secret_file]:
            keygen(secret_file)
        case ["sign", *paths] if paths:
            sign(paths)
        case ["verify", key, *paths] if paths:
            verify(key, paths)
        case _:
            sys.exit(__doc__)


if __name__ == "__main__":
    main()
