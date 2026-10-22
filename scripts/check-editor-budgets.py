#!/usr/bin/env python3
"""Fail CI when an editor performance budget is exceeded."""
import json
import sys
from pathlib import Path

data = json.loads(Path(sys.argv[1]).read_text())
assert {"big", "long", "multi", "lex", "palette"} <= data["phases"].keys(), "missing phases"
failures = []
checks = []

def visit(value, path):
    if isinstance(value, dict):
        if "pass" in value:
            checks.append(path)
            if value["pass"] is not True:
                failures.append(path)
        for key, item in value.items():
            visit(item, f"{path}.{key}")
    elif isinstance(value, list):
        for index, item in enumerate(value):
            visit(item, f"{path}[{index}]")

visit(data, "editor")
assert checks, "no editor budget verdicts"
assert not failures, f"editor budget regressions: {', '.join(failures)}"
print(f"{len(checks)} editor budget checks passed")
