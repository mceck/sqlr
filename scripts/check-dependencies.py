#!/usr/bin/env python3
"""Enforce the workspace's TLS and egui dependency constraints."""
import json
import subprocess

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1"], text=True))
resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
packages = [p for p in metadata["packages"] if p["id"] in resolved]
for name in ("openssl-sys", "aws-lc-sys"):
    assert not any(p["name"] == name for p in packages), f"forbidden dependency: {name}"
for name in ("egui", "eframe", "egui_extras", "egui_kittest"):
    versions = {p["version"] for p in packages if p["name"] == name}
    assert versions == {"0.36.2"}, f"{name}: {versions}"
print("TLS and egui dependency constraints passed")
