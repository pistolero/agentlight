#!/usr/bin/env python3
"""Exercise the Wi-Fi OTA push with a synthetic image and fake displays.
Run after cargo build --release, from the repository root. No real display
approval is created. Port 47832 must be free (stop a running --wifi helper).
"""
import hashlib
import json
import os
from pathlib import Path
import select
import socket
import ssl
import subprocess
import tempfile

BUILD = "9.9.9;Jan  1 2030 00:00:00"

with tempfile.TemporaryDirectory(prefix="agentlight-ota-") as config:
    image = bytearray(200_000)
    image[0] = 0xE9
    image[12:14] = (9).to_bytes(2, "little")
    marker = b"AGENTLIGHT-BUILD:" + BUILD.encode() + b"\0"
    image[1000:1000 + len(marker)] = marker
    for i in range(2000, len(image)):
        image[i] = i * 7 % 251
    firmware = Path(config) / "firmware.bin"
    firmware.write_bytes(image)
    env = dict(os.environ, XDG_CONFIG_HOME=config)
    process = subprocess.Popen(
        ["target/release/agentlight", "--wifi", "--wifi-only", "--demo", "--ota", str(firmware)],
        env=env, stderr=subprocess.PIPE, text=True,
    )
    try:
        seen = []
        while not any("Encrypted Wi-Fi server" in line for line in seen):
            assert select.select([process.stderr], [], [], 15)[0], "server startup timeout"
            seen.append(process.stderr.readline())
            assert seen[-1], "server exited: " + "".join(seen)
        identity = json.loads((Path(config)/"agentlight/wifi-identity.json").read_text())
        expected = hashlib.sha256(bytes(identity["cert"])).digest()
        (Path(config)/"agentlight/wifi-displays.json").write_text(json.dumps([
            dict(device="ota-display", token="a"*64), dict(device="legacy-display", token="b"*64),
        ]))

        def connect(device, token, **extra):
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            ctx.check_hostname = False
            ctx.verify_mode = ssl.CERT_NONE
            sock = ctx.wrap_socket(socket.create_connection(("127.0.0.1", 47832), timeout=10))
            assert hashlib.sha256(sock.getpeercert(binary_form=True)).digest() == expected
            sock.sendall((json.dumps(dict(v=1, t="auth", device=device, token=token, **extra))+"\n").encode())
            stream = sock.makefile("rb")
            assert json.loads(stream.readline())["t"] == "authorized"
            return sock, stream

        # Old build with OTA support: receives the full image, verified by SHA-256.
        sock, stream = connect("ota-display", "a"*64, ota=1, fw="0.2.0", build="0.2.0;old")
        with sock:
            begin = json.loads(stream.readline())
            assert begin["t"] == "ota_begin" and begin["size"] == len(image), begin
            assert begin["version"] == "9.9.9" and begin["sha256"] == hashlib.sha256(image).hexdigest()
            sock.sendall(b'{"v":1,"t":"ota_ready"}\n')
            received = stream.read(begin["size"])
            assert hashlib.sha256(received).hexdigest() == begin["sha256"], "image corrupted in transit"
            sock.sendall(b'{"v":1,"t":"ota_result","ok":true}\n')
            assert not stream.read(1), "helper kept streaming after a successful update"

        # After the reboot the build matches: normal state streaming resumes.
        sock, stream = connect("ota-display", "a"*64, ota=1, fw="9.9.9", build=BUILD)
        with sock:
            assert json.loads(stream.readline())["t"] == "state"

        # Firmware without OTA support is never sent an update.
        sock, stream = connect("legacy-display", "b"*64, fw="0.2.0")
        with sock:
            assert json.loads(stream.readline())["t"] == "state"
        print("PASS: OTA image streamed and verified, matching build streams state, legacy firmware skipped")
    finally:
        process.terminate()
        process.wait(timeout=5)
