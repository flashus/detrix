#!/usr/bin/env python3
"""One-time RSA-2048 key generation script for mock JWKS server.

Outputs test_private_key.pem and jwks.json in the same directory.
Run from this directory:
    python generate_keys.py

The generated keys are checked in to the repo as test-only material.
They are NOT used in production.
"""

import base64
import json
import os
import subprocess


def run(cmd, **kwargs):
    result = subprocess.run(cmd, capture_output=True, **kwargs)
    if result.returncode != 0:
        raise RuntimeError(f"Command failed: {' '.join(cmd)}\n{result.stderr.decode()}")
    return result.stdout


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    key_path = os.path.join(here, "test_private_key.pem")
    jwks_path = os.path.join(here, "jwks.json")

    # Generate RSA-2048 private key
    pem = run(["openssl", "genrsa", "2048"])
    with open(key_path, "wb") as f:
        f.write(pem)
    print(f"Written: {key_path}")

    # Extract modulus (n)
    n_hex_line = run(["openssl", "rsa", "-in", key_path, "-noout", "-modulus"]).decode().strip()
    n_hex = n_hex_line.split("=", 1)[1]
    n_bytes = bytes.fromhex(n_hex)
    if n_bytes[0] == 0:
        n_bytes = n_bytes[1:]
    n_b64 = base64.urlsafe_b64encode(n_bytes).rstrip(b"=").decode()

    # Exponent is always 65537 for openssl genrsa
    e_bytes = (65537).to_bytes(3, "big")
    e_b64 = base64.urlsafe_b64encode(e_bytes).rstrip(b"=").decode()

    jwk = {
        "kty": "RSA",
        "alg": "RS256",
        "use": "sig",
        "kid": "test-key-001",
        "n": n_b64,
        "e": e_b64,
    }
    jwks = {"keys": [jwk]}

    with open(jwks_path, "w") as f:
        json.dump(jwks, f, indent=2)
        f.write("\n")
    print(f"Written: {jwks_path}")


if __name__ == "__main__":
    main()
