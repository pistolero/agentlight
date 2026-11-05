#!/usr/bin/env python3
"""Exercise TLS/auth/logs/revocation using an isolated synthetic allowlist.
Run after cargo build --release. No real display approval is created.
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
import time

with tempfile.TemporaryDirectory(prefix="agentlight-wifi-", dir="/tmp/opencode") as config:
    env = dict(os.environ, XDG_CONFIG_HOME=config)
    process = subprocess.Popen(
        ["target/release/agentlight", "--wifi", "--wifi-only", "--demo"],
        env=env, stderr=subprocess.PIPE, text=True,
    )
    try:
        assert select.select([process.stderr], [], [], 15)[0], "server startup timeout"
        assert "Encrypted Wi-Fi server" in process.stderr.readline(), "server startup failed"
        identity = json.loads((Path(config)/"agentlight/wifi-identity.json").read_text())
        expected = hashlib.sha256(bytes(identity["cert"])).digest()

        def connect(token):
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            ctx.check_hostname = False
            ctx.verify_mode = ssl.CERT_NONE
            sock = ctx.wrap_socket(socket.create_connection(("127.0.0.1", 47832), timeout=5))
            # Same trust model as the device: verify the USB-provisioned pin
            # before sending any authorization credential or application data.
            assert hashlib.sha256(sock.getpeercert(binary_form=True)).digest() == expected
            sock.sendall((json.dumps(dict(v=1, t="auth", device="test-display", token=token, logv=2))+"\n").encode())
            return sock

        with connect("not-approved") as sock:
            try:
                assert not sock.recv(4096), "unapproved connection received application data"
            except (ssl.SSLError, ConnectionResetError):
                pass
        permits = Path(config)/"agentlight/wifi-displays.json"
        permits.write_text(json.dumps([dict(device="test-display", token="a"*64)]))
        with connect("a"*64) as sock:
            lines = sock.makefile("rb")
            assert json.loads(lines.readline())["t"] == "authorized"
            state = json.loads(lines.readline())
            assert state["t"] == "state" and state["agents"]
            parent = next(a for a in state["agents"] if a.get("children"))
            sock.sendall((json.dumps(dict(v=1, t="tail", id=parent["id"]))+"\n").encode())
            reply = json.loads(lines.readline())
            assert reply["t"] == "tail" and reply["state"] == "ok" and "child" not in reply
            assert all(row[0] in ("u", "a", "t") and isinstance(row[1], str) for row in reply["entries"])
            # Subagent logs: wait for the next state cycle, then ask for the child.
            assert json.loads(lines.readline())["t"] == "state"
            child = parent["children"][0]["id"]
            sock.sendall((json.dumps(dict(v=1, t="tail", id=parent["id"], child=child))+"\n").encode())
            reply = json.loads(lines.readline())
            assert reply["t"] == "tail" and reply["child"] == child and reply["entries"], reply
            # Exercise the actual revocation CLI and ensure no further states.
            subprocess.run(["target/release/agentlight", "--wifi-revoke", "test-display"], env=env, check=True, stdout=subprocess.DEVNULL)
            try:
                assert not lines.readline(), "revoked connection received more data"
            except (ssl.SSLError, ConnectionResetError):
                pass
        print("PASS: TLS pin, unapproved rejection, approved state/agent+subagent log access, active revocation")
    finally:
        process.terminate()
        process.wait(timeout=5)
