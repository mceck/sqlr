#!/usr/bin/env python3
"""Enforce the workspace's TLS and egui dependency constraints."""
import json
import subprocess

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--format-version", "1"], text=True))
resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
packages = [p for p in metadata["packages"] if p["id"] in resolved]
# AWS-LC is a rustls crypto provider, enabled transitively by Oracle and SQL Server.
# The workspace forbids OpenSSL, not rustls's native crypto providers.
for name in ("openssl-sys",):
    assert not any(p["name"] == name for p in packages), f"forbidden dependency: {name}"
for name in ("egui", "eframe", "egui_extras", "egui_kittest"):
    versions = {p["version"] for p in packages if p["name"] == name}
    assert versions == {"0.36.2"}, f"{name}: {versions}"
print("TLS and egui dependency constraints passed")
