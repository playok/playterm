"""Loopback-only terminal output fixture. Synthetic data; Python standard library only."""
import argparse
import json
import select
import socketserver
import threading
import time
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument("--directory", type=Path, required=True)
parser.add_argument("--seconds", type=int, default=300)
parser.add_argument("--bytes-per-second", type=int, default=65536)
parser.add_argument("--disconnect-after", type=int, default=0)
args = parser.parse_args()
if args.seconds <= 0 or args.bytes_per_second < 256:
    parser.error("positive duration and at least 256 bytes/second required")
args.directory.mkdir(parents=True, exist_ok=True)
stop = threading.Event()
guard = threading.Lock()
stats = {"accepted": 0, "active": 0, "bytes": 0, "inputs": 0}


class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        with guard:
            stats["accepted"] += 1
            stats["active"] += 1
            number = stats["accepted"]
        started = time.monotonic()
        line = (f"test stream {number}: " + "x" * 100 + "\r\n").encode()
        block = (line * max(1, args.bytes_per_second // 10 // len(line)))
        self.request.settimeout(2)
        try:
            while not stop.is_set():
                if args.disconnect_after and time.monotonic() - started >= args.disconnect_after:
                    break
                self.request.sendall(block)
                with guard:
                    stats["bytes"] += len(block)
                if select.select([self.request], [], [], 0.1)[0]:
                    data = self.request.recv(4096)
                    if not data:
                        break
                    self.request.sendall(b"\r\ninput received\r\n")
                    with guard:
                        stats["inputs"] += 1
        except (ConnectionError, TimeoutError, OSError):
            pass
        finally:
            with guard:
                stats["active"] -= 1


class Server(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


with Server(("127.0.0.1", 0), Handler) as server:
    (args.directory / "url.txt").write_text(f"telnet://127.0.0.1:{server.server_address[1]}", encoding="utf-8")
    threading.Thread(target=server.serve_forever, daemon=True).start()
    deadline = time.monotonic() + args.seconds
    while time.monotonic() < deadline and not (args.directory / "stop").exists():
        with guard:
            snapshot = dict(stats)
        temporary = args.directory / "server-stats.tmp"
        temporary.write_text(json.dumps(snapshot), encoding="utf-8")
        temporary.replace(args.directory / "server-stats.json")
        time.sleep(0.25)
    stop.set()
    server.shutdown()
