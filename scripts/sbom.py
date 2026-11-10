#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 TPT Solutions
"""Write a CycloneDX 1.5 software bill of materials for the workspace.

The components are every package in the resolved dependency graph, read from
`cargo metadata --locked`, so the list is exactly what `Cargo.lock` pins. Components
are sorted by name and version, and the document carries no timestamp or serial
number, so the same lock file always produces the same bytes.

Usage:
    python3 scripts/sbom.py [OUTPUT]     default OUTPUT: sbom.cdx.json
"""

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def metadata():
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def component(package, workspace_ids):
    name = package["name"]
    version = package["version"]
    purl = f"pkg:cargo/{name}@{version}"
    entry = {
        "type": "library",
        "bom-ref": purl,
        "name": name,
        "version": version,
        "purl": purl,
        "properties": [
            {
                "name": "tpt-wasm:workspace-member",
                "value": "true" if package["id"] in workspace_ids else "false",
            }
        ],
    }
    if package.get("license"):
        entry["licenses"] = [{"expression": package["license"]}]
    if package.get("repository"):
        entry["externalReferences"] = [
            {"type": "vcs", "url": package["repository"]}
        ]
    return entry


def build(meta):
    workspace_ids = set(meta["workspace_members"])
    packages = sorted(meta["packages"], key=lambda p: (p["name"], p["version"]))
    root = next(p for p in meta["packages"] if p["id"] in workspace_ids and p["name"] == "tpt-wasm")
    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "name": root["name"],
                "version": root["version"],
                "bom-ref": f"pkg:cargo/{root['name']}@{root['version']}",
                "licenses": [{"expression": root["license"]}],
            }
        },
        "components": [component(p, workspace_ids) for p in packages],
    }


def main(argv):
    output = Path(argv[0]) if argv else ROOT / "sbom.cdx.json"
    document = build(metadata())
    output.write_text(
        json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(f"wrote {output} with {len(document['components'])} components")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
