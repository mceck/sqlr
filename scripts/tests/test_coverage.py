"""Exercise coverage orchestration without compiling Rust or starting databases."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class CoverageTests(unittest.TestCase):
    def test_removes_stale_instrumented_binaries_before_running_tests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            shutil.copy(Path(__file__).parents[1] / "coverage.sh", root / "scripts")
            tools = root / "bin"
            tools.mkdir()
            stale = root / "target/llvm-cov-target/debug/deps/old-test-binary"
            stale.parent.mkdir(parents=True)
            stale.touch()
            cargo = tools / "cargo"
            cargo.write_text('''#!/usr/bin/env python3
import json
from pathlib import Path
import shutil
import sys
args = sys.argv[1:]
if args[:3] == ["llvm-cov", "clean", "--workspace"]:
    shutil.rmtree("target/llvm-cov-target")
elif args[:2] == ["llvm-cov", "nextest"]:
    if Path("target/llvm-cov-target/debug/deps/old-test-binary").exists():
        sys.exit(42)
elif args[:2] == ["llvm-cov", "report"] and "--json" in args:
    output = Path(args[args.index("--output-path") + 1])
    output.write_text(json.dumps({"data": [{"files": [{
        "filename": str(Path.cwd() / "crates/example/src/lib.rs"),
        "summary": {"lines": {"count": 10, "covered": 8}}
    }]}]}))
''')
            cargo.chmod(0o755)
            for name in ("cargo-nextest", "cargo-llvm-cov"):
                (tools / name).symlink_to(cargo)
            result = subprocess.run(
                ["sh", "scripts/coverage.sh", "--fast", "--check"],
                cwd=root,
                env={**os.environ, "PATH": f"{tools}:{os.environ['PATH']}"},
                text=True,
                capture_output=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse(stale.exists())
            self.assertIn("80.00%", (root / "target/coverage/per-crate.txt").read_text())


if __name__ == "__main__":
    unittest.main()
