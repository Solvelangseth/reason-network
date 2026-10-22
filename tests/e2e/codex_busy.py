"""Codex delivery routing against an isolated daemon and a queue-accepting CLI.

    cargo build && python3 tests/e2e/codex_busy.py
"""
import json
import os
import queue
import sqlite3
import subprocess
import tempfile
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.path.join(ROOT, "target", "debug", "reason-network")

with tempfile.TemporaryDirectory(prefix="reason-network-codex-") as d:
    queue_log = os.path.join(d, "queue.log")
    fake = os.path.join(d, "codex")
    with open(fake, "w") as f:
        f.write('#!/bin/sh\nprintf "%s\\n" "$*" >> "$CODEX_QUEUE_LOG"\n')
    os.chmod(fake, 0o755)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("CLAUDE", "CODEX", "REASON_NETWORK", "TINCAN"))}
    env.update(REASON_NETWORK_HOME=os.path.join(d, "home"), REASON_NETWORK_NO_MDNS="1",
               REASON_NETWORK_NO_SERVICE="1", REASON_NETWORK_MACHINE="box",
               REASON_NETWORK_CODEX_BIN=fake, CODEX_QUEUE_LOG=queue_log)
    proj = os.path.join(d, "proj")
    os.makedirs(proj)

    def run(*args, payload=None):
        r = subprocess.run([BIN, *args], env=env, cwd=proj, input=json.dumps(payload) if payload else None,
                           capture_output=True, text=True, timeout=30)
        assert r.returncode == 0, r.stdout + r.stderr
        return (r.stdout + r.stderr).strip()

    def hook(event):
        return run("hook", "codex", event, payload={"session_id": "busy-thread", "cwd": proj})

    def queued():
        if not os.path.exists(queue_log):
            return ""
        with open(queue_log) as f:
            return f.read()

    def rpc(p, request):
        p.stdin.write(json.dumps(request) + "\n")
        p.stdin.flush()
        if "id" not in request:
            return None
        responses = queue.Queue()

        def receive():
            while line := p.stdout.readline():
                response = json.loads(line)
                if response.get("id") == request["id"]:
                    responses.put(response)
                    return

        threading.Thread(target=receive, daemon=True).start()
        return responses.get(timeout=10)

    cx = None
    try:
        run("init", "codex-test", "--as", "alex", "--no-setup")
        hook("session-start")
        hook("user-prompt-submit")
        out = run("send", "proj-codex", "busy message")
        # A later command round trip lets a successful queue command finish before the hook reads.
        run("who")
        out = hook("post-tool-use")
        assert "busy message" in out and '"PostToolUse"' in out, f"busy hook lost the message: {out!r}; queue={queued()!r}"
        assert "busy message" not in queued(), "busy mail must not also be queued as a follow-up"
        assert hook("post-tool-use") == "", "busy mail must arrive once"
        print("PASS: busy Codex gets mail through PostToolUse once, without a queued follow-up")

        hook("stop")
        out = run("send", "proj-codex", "idle message")
        assert "queued into the Codex session" in out, out
        deadline = time.monotonic() + 5
        while "idle message" not in queued() and time.monotonic() < deadline:
            time.sleep(0.02)
        assert "idle message" in queued(), "idle session must still wake via codex queue"
        with sqlite3.connect(os.path.join(env["REASON_NETWORK_HOME"], "network.db")) as db:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                read_at = db.execute("SELECT read_at FROM messages WHERE body = ?", ("idle message",)).fetchone()[0]
                if read_at is not None:
                    break
                time.sleep(0.02)
            assert read_at is not None, "queue success must be acknowledged, not merely in flight"
        assert hook("post-tool-use") == "", "queued mail must not also arrive through the hook"
        print("PASS: idle Codex still wakes through codex queue, without duplicate hook delivery")

        cx = subprocess.Popen([BIN, "mcp"], env=env, cwd=proj, stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        rpc(cx, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "codex-mcp-client", "version": "1"}}})
        rpc(cx, {"jsonrpc": "2.0", "method": "notifications/initialized"})
        tools = rpc(cx, {"jsonrpc": "2.0", "id": 3, "method": "tools/list"})
        assert any(t["name"] == "check_inbox" for t in tools["result"]["tools"]), tools
        out = run("send", "proj-codex", "--fyi", "polled FYI")
        assert "queued as FYI" in out, out
        assert hook("post-tool-use") == "", "FYI must not interrupt at PostToolUse"
        response = rpc(cx, {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "check_inbox", "arguments": {}, "_meta": {"sessionId": "busy-thread"}}})
        assert "polled FYI" in json.dumps(response), response
        assert "polled FYI" not in queued(), "FYI must not wake the session"
        print("PASS: --fyi plus explicit check_inbox returns mail without a queue wake")
    finally:
        if cx:
            cx.kill()
            cx.wait()
        run("stop")
