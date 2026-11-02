#!/usr/bin/env python3
"""Shared synthetic fixture for ERMCP-43, ERMCP-44 and ERDSCAR-81; no customer data."""
import argparse
import base64
import hashlib
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("directory", type=Path)
parser.add_argument("--bytes", type=int, default=31_957_070)
args = parser.parse_args()
if args.bytes < 1024:
    parser.error("--bytes must be at least 1024")
args.directory.mkdir(parents=True, exist_ok=True)

# Fixed-width ASCII records, 13 columns. Extend the last identifier to hit
# the exact decoded length without introducing a partial CSV row.
rows = []
size = 0
index = 0
while True:
    fields = [f"SYN-{index:08d}", f"Invented copper widget {index:08d}",
              "DEMO", "EA", "12.50", "19", "0.25", "100", "200",
              "300", "400", "500", "600"]
    row = (";".join(fields) + "\n").encode()
    if size + len(row) > args.bytes:
        break
    rows.append(row)
    size += len(row)
    index += 1
rows[-1] = rows[-1].replace(b";", b"x" * (args.bytes - size) + b";", 1)
data = b"".join(rows)
assert len(data) == args.bytes
assert all(row.count(b";") == 12 for row in data.splitlines())
encoded = base64.b64encode(data).decode()
code = """with open('/workspace/data.csv', encoding='utf-8') as source:
    lines = [source.readline() for _ in range(15)]
print(''.join(lines[:5]))
import pandas as pd
print(pd.read_csv('/workspace/data.csv', sep=';', header=None, nrows=10, encoding='utf-8', dtype=str))
"""
request = {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
    "name": "run_python_code", "arguments": {
        "code": code, "files": [{"path": "data.csv", "content_base64": encoded}],
        "timeout_ms": 120000}}}
wire = json.dumps(request, separators=(",", ":")).encode()
(args.directory / "data.csv").write_bytes(data)
(args.directory / "request.json").write_bytes(wire)
print(json.dumps({"decoded_bytes": len(data), "base64_bytes": len(encoded),
                  "request_bytes": len(wire), "rows": len(rows),
                  "sha256": hashlib.sha256(data).hexdigest()}, indent=2))
