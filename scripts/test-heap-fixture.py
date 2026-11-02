#!/usr/bin/env python3
"""Small, dependency-free checks for the shared synthetic heap fixture."""
import base64
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class FixtureTest(unittest.TestCase):
    def test_exact_size_columns_and_determinism(self):
        generator = Path(__file__).with_name("generate-heap-fixture.py")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for size in (1024, 1025, 1026, 4096):
                command = [sys.executable, str(generator), directory, "--bytes", str(size)]
                subprocess.run(command, check=True, capture_output=True)
                data = (root / "data.csv").read_bytes()
                wire = (root / "request.json").read_bytes()
                self.assertEqual(len(data), size)
                self.assertTrue(data.endswith(b"\n"))
                self.assertTrue(all(len(row.split(b";")) == 13 for row in data.splitlines()))
                encoded = json.loads(wire)["params"]["arguments"]["files"][0]["content_base64"]
                self.assertEqual(base64.b64decode(encoded, validate=True), data)
                self.assertEqual(len(encoded), 4 * ((size + 2) // 3))
                subprocess.run(command, check=True, capture_output=True)
                self.assertEqual((root / "request.json").read_bytes(), wire)


if __name__ == "__main__":
    unittest.main()
