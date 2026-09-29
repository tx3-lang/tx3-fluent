#!/usr/bin/env python3
"""A load run against `fluent serve --http`, with the Python standard library.

    load_run.py stub --port 9999 --delay 0.2
        Serves a scripted TRP endpoint that answers every `trp.resolve` with
        the preprod transfer fixture after `--delay` seconds.

    load_run.py drive --url http://127.0.0.1:8080 --token $FLUENT_API_TOKEN \
        --clients 50 --calls 10
        Opens one MCP session per client, then has every client call the
        transfer tool `--calls` times at once, and prints the latency
        histogram, the outcomes and the server's `fluent_*` metrics.

Each client has its own session and numbers its requests, because concurrent
requests sharing a JSON-RPC id in one session are not answered reliably. MCP
failures are tool results inside HTTP 200 responses, so outcomes are read
from the results, not from HTTP statuses.
"""

import argparse
import collections
import json
import pathlib
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = pathlib.Path(__file__).resolve().parent.parent
FIXTURE = ROOT / "crates/fluent-core/tests/fixtures/tx/transfer-preprod.hex"
TRANSFER_HASH = "b2698db18245555a24e2e38a1a1c1a9623465aacf28b19ac93f9287093e58f73"
SENDER = "addr_test1qrxchm0g4la6hqfd9wq6vuuldx7l20az52t7lvgpgujr8pvwmpzru5kuf4mpmvtaf0hlsjtz7t4r2h7tj9v3c02dhljq0wqkef"
RECEIVER = "addr_test1qpwms9gqr76nar77cja9yq6dl44zdn3wflmd96h4wp3ae9yzg29hdhjxjuf3jpgqq2df60v0aq63dn96ey9mh6njcatsdynmak"
PROTOCOL_VERSION = "2025-06-18"
BUCKETS = [0.05, 0.1, 0.25, 0.5, 1, 2.5, 5, 10, 20, 45, float("inf")]


def stub(port, delay):
    body = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": "1",
            "result": {"hash": TRANSFER_HASH, "tx": FIXTURE.read_text().strip()},
        }
    ).encode()

    class Resolver(BaseHTTPRequestHandler):
        def do_POST(self):
            self.rfile.read(int(self.headers.get("content-length", 0)))
            time.sleep(delay)
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    ThreadingHTTPServer.daemon_threads = True
    ThreadingHTTPServer(("127.0.0.1", port), Resolver).serve_forever()


class Client:
    def __init__(self, url, token):
        self.url = url.rstrip("/") + "/mcp"
        self.token = token
        self.session = None
        self.next_id = 0

    def post(self, message):
        headers = {
            "content-type": "application/json",
            "accept": "application/json, text/event-stream",
            "authorization": f"Bearer {self.token}",
        }
        if self.session:
            headers["mcp-session-id"] = self.session
            headers["mcp-protocol-version"] = PROTOCOL_VERSION
        request = urllib.request.Request(
            self.url, json.dumps(message).encode(), headers, method="POST"
        )
        with urllib.request.urlopen(request, timeout=120) as response:
            session = response.headers.get("mcp-session-id")
            text = response.read().decode()
        if session:
            self.session = session
        return reply(text)

    def request(self, method, params):
        self.next_id += 1
        return self.post(
            {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}
        )

    def initialize(self):
        self.request(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "fluent-load-run", "version": "0"},
            },
        )
        self.post({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def call(self, name, arguments):
        result = self.request("tools/call", {"name": name, "arguments": arguments})[
            "result"
        ]
        if result.get("isError"):
            return json.loads(result["content"][0]["text"])["error"]["code"]
        return "ok"


def reply(text):
    """The JSON-RPC reply in a JSON or SSE body; None for an acknowledgement."""
    if not text.strip():
        return None
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        for line in text.splitlines():
            if line.startswith("data:"):
                try:
                    message = json.loads(line[5:])
                except json.JSONDecodeError:
                    continue  # A priming event without a message.
                if isinstance(message, dict) and "id" in message:
                    return message
    raise ValueError(f"no JSON-RPC reply in {text!r}")


def wait_until_healthy(url, attempts=100):
    for _ in range(attempts):
        try:
            urllib.request.urlopen(url.rstrip("/") + "/healthz", timeout=2).read()
            return
        except OSError:
            time.sleep(0.2)
    raise SystemExit(f"{url} did not become healthy")


def drive(url, token, clients, calls, tool):
    arguments = {"quantity": 3_000_000, "sender": SENDER, "receiver": RECEIVER, "middleman": SENDER}
    wait_until_healthy(url)
    sessions = [Client(url, token) for _ in range(clients)]
    for client in sessions:
        client.initialize()

    results = []
    lock = threading.Lock()
    start = threading.Barrier(clients)

    def run(client):
        start.wait()
        for _ in range(calls):
            began = time.monotonic()
            try:
                outcome = client.call(tool, arguments)
            except Exception as err:  # A transport failure, not a tool result.
                outcome = f"transport: {type(err).__name__}"
            elapsed = time.monotonic() - began
            with lock:
                results.append((outcome, elapsed))

    threads = [threading.Thread(target=run, args=(c,)) for c in sessions]
    began = time.monotonic()
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    wall = time.monotonic() - began

    latencies = sorted(elapsed for _, elapsed in results)

    def quantile(q):
        return latencies[min(len(latencies) - 1, int(q * len(latencies)))]

    print(f"{len(results)} calls from {clients} clients in {wall:.2f}s ({len(results) / wall:.1f}/s)")
    print(
        "latency s: min {:.3f}  p50 {:.3f}  p90 {:.3f}  p95 {:.3f}  p99 {:.3f}  max {:.3f}".format(
            latencies[0], quantile(0.5), quantile(0.9), quantile(0.95), quantile(0.99), latencies[-1]
        )
    )
    print("latency histogram (calls with latency <= le):")
    for le in BUCKETS:
        count = sum(1 for elapsed in latencies if elapsed <= le)
        print(f"  le {le:>5}: {count}")
    print("outcomes:")
    for outcome, count in collections.Counter(o for o, _ in results).most_common():
        print(f"  {outcome}: {count}")

    metrics = urllib.request.urlopen(url.rstrip("/") + "/metrics", timeout=10).read().decode()
    print("server metrics:")
    for line in metrics.splitlines():
        if line.startswith("fluent_") and "_bucket" not in line:
            print(f"  {line}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    stub_args = commands.add_parser("stub")
    stub_args.add_argument("--port", type=int, default=9999)
    stub_args.add_argument("--delay", type=float, default=0.2)
    drive_args = commands.add_parser("drive")
    drive_args.add_argument("--url", default="http://127.0.0.1:8080")
    drive_args.add_argument("--token", required=True)
    drive_args.add_argument("--clients", type=int, default=50)
    drive_args.add_argument("--calls", type=int, default=10)
    drive_args.add_argument("--tool", default="transfer_preprod_transfer")
    args = parser.parse_args()
    if args.command == "stub":
        stub(args.port, args.delay)
    else:
        drive(args.url, args.token, args.clients, args.calls, args.tool)


if __name__ == "__main__":
    main()
