#!/usr/bin/env python3
"""Replay generate-heap-fixture.py's synthetic request against an MCP server."""
import argparse
import json
from pathlib import Path
import urllib.request


def post(url, body, session=None):
    headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream"}
    if session:
        headers["Mcp-Session-Id"] = session
        headers["MCP-Protocol-Version"] = "2025-11-25"
    with urllib.request.urlopen(urllib.request.Request(url, data=body, headers=headers), timeout=180) as response:
        text = response.read().decode()
        if response.headers.get_content_type() == "text/event-stream":
            messages = [json.loads(line[6:]) for line in text.splitlines() if line.startswith("data: ") and line[6:].strip()]
            result = next((item for item in messages if "id" in item), None)
        else:
            result = json.loads(text) if text else None
        return result, response.headers.get("Mcp-Session-Id")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("request", type=Path)
    parser.add_argument("--url", default="http://127.0.0.1:8000/mcp")
    args = parser.parse_args()
    _, session = post(args.url, json.dumps({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "synthetic-heap-replay", "version": "1"}
    }}).encode())
    post(args.url, b'{"jsonrpc":"2.0","method":"notifications/initialized"}', session)
    result, _ = post(args.url, args.request.read_bytes(), session)
    print(json.dumps(result, indent=2))
    if not result or "error" in result or result.get("result", {}).get("isError"):
        raise SystemExit(1)
    output = result["result"].get("structuredContent")
    if output is not None and not output.get("success", False):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
