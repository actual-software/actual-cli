#!/usr/bin/env python3
"""
Anthropic API Auth Proxy for container-e2e.sh

This proxy:
1. Captures a fresh OAuth token + required headers from the local `claude` binary
2. Listens on a local port (default: 7477)
3. Forwards container requests to api.anthropic.com with the fresh token

The container sets ANTHROPIC_BASE_URL=http://host.docker.internal:7477 so all
Claude Code API calls route through here.

Usage:
    python3 scripts/auth-proxy.py [--port 7477] [--verbose]

Token is refreshed every --token-ttl seconds (default: 300).
"""

import argparse
import http.server
import json
import os
import signal
import socket
import ssl
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

UPSTREAM = "https://api.anthropic.com"
DEFAULT_PORT = 7477

# Token + header state (captured from local claude binary)
_state = {
    "token": None,
    "extra_headers": {},  # headers to inject: anthropic-beta, x-app, etc.
    "fetched_at": 0,
    "ttl": 300,
}
_state_lock = threading.Lock()

# Required extra headers to pass OAuth through api.anthropic.com
_REQUIRED_EXTRA_HEADERS = {
    "anthropic-beta": "oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,prompt-caching-scope-2026-01-05,claude-code-20250219",
    "anthropic-dangerous-direct-browser-access": "true",
    "x-app": "cli",
    "User-Agent": "claude-cli/2.1.50 (external, sdk-cli)",
}


def log(msg):
    ts = time.strftime("%H:%M:%S")
    print(f"[auth-proxy {ts}] {msg}", flush=True)


def _capture_token_from_claude():
    """
    Start a local HTTP capture server, run `claude --print` with ANTHROPIC_BASE_URL
    pointing at it, capture the Authorization header + extra headers, then shut down.
    Returns (token_str, extra_headers_dict) or (None, {}).
    """
    captured = {}
    done = threading.Event()

    class CaptureHandler(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            length = int(self.headers.get("Content-Length", 0))
            _ = self.rfile.read(length)
            auth = self.headers.get("Authorization", "")
            if auth.startswith("Bearer "):
                captured["token"] = auth[len("Bearer ") :]
            # Capture all extra Anthropic headers
            extra = {}
            for k, v in self.headers.items():
                kl = k.lower()
                if kl in (
                    "anthropic-beta",
                    "anthropic-dangerous-direct-browser-access",
                    "x-app",
                    "user-agent",
                    "anthropic-version",
                ):
                    extra[k] = v
            captured["extra_headers"] = extra
            # Return a minimal valid messages response
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            resp = json.dumps(
                {
                    "id": "msg_capture",
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "text", "text": "ok"}],
                    "model": "claude-haiku-4-5",
                    "stop_reason": "end_turn",
                    "stop_sequence": None,
                    "usage": {"input_tokens": 1, "output_tokens": 1},
                }
            ).encode()
            self.wfile.write(resp)
            done.set()

        def log_message(self, *args):
            pass

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        capture_port = s.getsockname()[1]

    server = http.server.HTTPServer(("127.0.0.1", capture_port), CaptureHandler)
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()

    try:
        env = {**os.environ, "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{capture_port}"}
        subprocess.run(
            [
                "claude",
                "--print",
                "--model",
                "claude-haiku-4-5",
                "--no-session-persistence",
            ],
            input="hi",
            capture_output=True,
            text=True,
            timeout=25,
            env=env,
        )
        done.wait(timeout=8)
    except Exception as e:
        log(f"Token capture subprocess error: {e}")
    finally:
        server.shutdown()

    return captured.get("token"), captured.get("extra_headers", {})


def refresh_state():
    """Refresh the cached token + headers from the local claude binary."""
    log("Capturing fresh token from local claude binary...")
    token, extra_headers = _capture_token_from_claude()

    with _state_lock:
        if token:
            _state["token"] = token
            # Merge captured extra headers with our required set
            merged = dict(_REQUIRED_EXTRA_HEADERS)
            merged.update(extra_headers)
            _state["extra_headers"] = merged
            _state["fetched_at"] = time.time()
            log(f"Token captured: {token[:35]}... (expires in {_state['ttl']}s)")
        else:
            log("WARNING: Token capture failed — proxy requests will likely 401")


def get_state():
    """Return (token, extra_headers), refreshing if stale."""
    with _state_lock:
        age = time.time() - _state["fetched_at"]
        needs_refresh = not _state["token"] or age >= _state["ttl"]

    if needs_refresh:
        refresh_state()

    with _state_lock:
        return _state["token"], dict(_state["extra_headers"])


class ProxyHandler(http.server.BaseHTTPRequestHandler):
    """Forward requests to api.anthropic.com with fresh OAuth token."""

    def _forward(self):
        method = self.command
        path = self.path
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length) if length > 0 else None

        token, extra_headers = get_state()

        # Build upstream headers: start with extras, then overlay inbound
        # headers (except host/auth/connection), then set our own auth.
        upstream_headers = dict(extra_headers)
        for key, val in self.headers.items():
            kl = key.lower()
            if kl in (
                "host",
                "connection",
                "transfer-encoding",
                "authorization",
                "content-length",
            ):
                continue
            upstream_headers[key] = val
        if token:
            upstream_headers["Authorization"] = f"Bearer {token}"
        if body:
            upstream_headers["Content-Length"] = str(len(body))

        upstream_url = f"{UPSTREAM}{path}"

        if args.verbose:
            log(f"{method} {path} → {upstream_url}")

        try:
            ctx = ssl.create_default_context()
            req = urllib.request.Request(
                upstream_url, data=body, headers=upstream_headers, method=method
            )
            with urllib.request.urlopen(req, context=ctx, timeout=120) as resp:
                self.send_response(resp.status)
                is_streaming = False
                for hk, hv in resp.headers.items():
                    if hk.lower() in ("transfer-encoding",):
                        continue
                    self.send_header(hk, hv)
                    if hk.lower() == "content-type" and "event-stream" in hv.lower():
                        is_streaming = True
                self.end_headers()
                while True:
                    chunk = resp.read(4096)
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    if is_streaming:
                        self.wfile.flush()

        except urllib.error.HTTPError as e:
            body_bytes = e.read()
            if args.verbose or e.code >= 400:
                log(f"Upstream HTTP {e.code}: {body_bytes[:300]}")
            self.send_response(e.code)
            for hk, hv in e.headers.items():
                if hk.lower() in ("transfer-encoding",):
                    continue
                self.send_header(hk, hv)
            self.end_headers()
            self.wfile.write(body_bytes)
        except Exception as e:
            log(f"Proxy error for {method} {path}: {e}")
            self.send_response(502)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"error": str(e)}).encode())

    def do_GET(self):
        self._forward()

    def do_POST(self):
        self._forward()

    def do_PUT(self):
        self._forward()

    def do_DELETE(self):
        self._forward()

    def do_PATCH(self):
        self._forward()

    def log_message(self, fmt, *fmtargs):
        if args.verbose:
            log(fmt % fmtargs)


class ThreadingHTTPServer(http.server.HTTPServer):
    def process_request(self, request, client_address):
        threading.Thread(
            target=self.process_request_thread,
            args=(request, client_address),
            daemon=True,
        ).start()

    def process_request_thread(self, request, client_address):
        try:
            self.finish_request(request, client_address)
        except Exception:
            self.handle_error(request, client_address)
        finally:
            self.shutdown_request(request)


def main():
    global args
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument("--verbose", "-v", action="store_true")
    parser.add_argument(
        "--token-ttl",
        type=int,
        default=300,
        help="Seconds before re-capturing token (default: 300)",
    )
    args = parser.parse_args()

    _state["ttl"] = args.token_ttl

    log(f"Starting Anthropic API auth proxy on port {args.port}")
    log(f"Upstream: {UPSTREAM}")

    # Pre-warm the token cache
    refresh_state()
    with _state_lock:
        if not _state["token"]:
            log("ERROR: Could not capture token. Is `claude` installed and logged in?")
            sys.exit(1)

    server = ThreadingHTTPServer(("0.0.0.0", args.port), ProxyHandler)

    def shutdown(sig, frame):
        log("Shutting down...")
        threading.Thread(target=server.shutdown, daemon=True).start()
        sys.exit(0)

    signal.signal(signal.SIGINT, shutdown)
    signal.signal(signal.SIGTERM, shutdown)

    log(f"Proxy ready on port {args.port}")
    log(f"  Container env: ANTHROPIC_BASE_URL=http://host.docker.internal:{args.port}")
    server.serve_forever()


if __name__ == "__main__":
    main()
