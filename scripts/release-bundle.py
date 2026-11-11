#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 TPT Solutions
"""Assemble the local release bundle: everything the release checklist names, in one directory.

Contents written to OUTPUT (default: target/release-bundle):

  source archive        tpt-wasm-<version>-source.tar.gz, from `git archive HEAD`
  binary                the release `tpt-wasm` CLI, built with `--locked`
  SBOM                  sbom.cdx.json, from scripts/sbom.py
  license report        licenses.json, every resolved package and its license expression
  notices               LICENSE-MIT, LICENSE-APACHE, NOTICE, COPYRIGHT
  feature manifest      feature-registry.toml and, if present, docs/standards/compat.md
  build metadata        build-metadata.json: version, git commit, toolchain, target, binary digest
  checksums             SHA256SUMS over every other file in the bundle

The bundle is built from a committed tree. A dirty working tree is refused, because the
source archive is HEAD and a bundle that disagreed with the working tree would be a lie.
`--allow-dirty` lifts that refusal for local inspection, and the metadata then records
`"dirty": true`.

  license texts         licenses/<name>-<version>/, every license and notice file each
                        third-party package ships in its source directory

A third-party package whose source directory ships no license or notice file is refused,
because the license of that package could not be shipped with the binary. The fix is to
add the text to the package's entry in the vendored notices, not to drop the check.

Nothing is published or uploaded.

Usage:
    python scripts/release-bundle.py [--allow-dirty] [OUTPUT]
"""

import hashlib
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NOTICE_FILES = ["LICENSE-MIT", "LICENSE-APACHE", "NOTICE", "COPYRIGHT"]
# File names that carry a package's license or attribution text, matched case-insensitively
# against the top level of the package's source directory.
LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|notice|unlicense)([-._].*)?$", re.I)
CLI_BINARY = "tpt-wasm"


def run(args, cwd=ROOT, capture=True):
    result = subprocess.run(args, cwd=cwd, check=True, capture_output=capture, text=True)
    return result.stdout.strip() if capture else ""


def workspace_version():
    manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    section = re.search(r"\[workspace\.package\](.*?)(\n\[|\Z)", manifest, re.S)
    if not section:
        sys.exit("error: Cargo.toml has no [workspace.package] table")
    version = re.search(r'^version\s*=\s*"([^"]+)"', section.group(1), re.M)
    if not version:
        sys.exit("error: [workspace.package] has no version")
    return version.group(1)


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def license_report():
    metadata = json.loads(run(["cargo", "metadata", "--format-version", "1", "--locked"]))
    packages = []
    for package in metadata["packages"]:
        packages.append(
            {
                "name": package["name"],
                "version": package["version"],
                "license": package.get("license"),
                "workspace": package["id"] in metadata["workspace_members"],
                "source_dir": str(Path(package["manifest_path"]).parent),
            }
        )
    packages.sort(key=lambda p: (p["name"], p["version"]))
    missing = [p for p in packages if not p["license"] and not p["workspace"]]
    return packages, missing


def license_texts(packages, out):
    """Copy each third-party package's license and notice files into the bundle.

    Returns the packages that ship none. Path-only dependencies such as the workspace
    members are skipped, because their notices are the bundle's own top-level files.
    """
    bare = []
    for package in packages:
        if package["workspace"]:
            continue
        source = Path(package["source_dir"])
        found = sorted(
            entry for entry in source.iterdir() if entry.is_file() and LICENSE_FILE.match(entry.name)
        )
        if not found:
            bare.append(package)
            continue
        target = out / "licenses" / f"{package['name']}-{package['version']}"
        target.mkdir(parents=True)
        for entry in found:
            shutil.copy2(entry, target / entry.name)
        package["texts"] = [entry.name for entry in found]
    return bare


def toolchain():
    rustc = run(["rustc", "-vV"])
    host = re.search(r"^host: (\S+)", rustc, re.M).group(1)
    return {
        "rustc": run(["rustc", "--version"]),
        "cargo": run(["cargo", "--version"]),
        "host": host,
    }


def main(argv):
    allow_dirty = "--allow-dirty" in argv
    positional = [a for a in argv if not a.startswith("--")]
    out = Path(positional[0]) if positional else ROOT / "target" / "release-bundle"
    out = out.resolve()

    dirty = bool(run(["git", "status", "--porcelain"]))
    if dirty and not allow_dirty:
        sys.exit("error: the working tree has uncommitted changes; commit them or pass --allow-dirty")

    version = workspace_version()
    commit = run(["git", "rev-parse", "HEAD"])
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)

    # 1. Source archive: exactly the committed tree at HEAD.
    source = out / f"tpt-wasm-{version}-source.tar.gz"
    run(
        ["git", "archive", "--format=tar.gz", f"--prefix=tpt-wasm-{version}/", "-o", str(source), "HEAD"],
        capture=False,
    )

    # 2. Release binary, locked to Cargo.lock.
    run(["cargo", "build", "--release", "--locked", "-p", "tpt-wasm-cli"], capture=False)
    exe = CLI_BINARY + (".exe" if sys.platform == "win32" else "")
    binary = out / exe
    shutil.copy2(ROOT / "target" / "release" / exe, binary)

    # 3. SBOM, from the same script CI uses.
    run([sys.executable, str(ROOT / "scripts" / "sbom.py"), str(out / "sbom.cdx.json")], capture=False)

    # 4. License report.
    packages, missing = license_report()
    (out / "licenses.json").write_text(
        json.dumps({"packages": packages}, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    if missing:
        names = ", ".join(f"{p['name']} {p['version']}" for p in missing)
        sys.exit(f"error: dependencies with no license expression: {names}")
    bare = license_texts(packages, out)
    (out / "licenses.json").write_text(
        json.dumps({"packages": packages}, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    if bare:
        names = ", ".join(f"{p['name']} {p['version']}" for p in bare)
        sys.exit(f"error: dependencies that ship no license or notice file: {names}")

    # 5. Notices and feature manifest.
    for name in NOTICE_FILES:
        shutil.copy2(ROOT / name, out / name)
    shutil.copy2(ROOT / "feature-registry.toml", out / "feature-registry.toml")
    compat = ROOT / "docs" / "standards" / "compat.md"
    if compat.exists():
        shutil.copy2(compat, out / "compat.md")

    # 6. Build metadata. No timestamp, so the same commit and toolchain give the same bytes.
    metadata = {
        "name": "tpt-wasm",
        "version": version,
        "commit": commit,
        "dirty": dirty,
        "toolchain": toolchain(),
        "binary": {"file": exe, "sha256": sha256(binary)},
    }
    (out / "build-metadata.json").write_text(
        json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )

    # 7. Checksums over everything else.
    lines = []
    for path in sorted(p for p in out.iterdir() if p.is_file() and p.name != "SHA256SUMS"):
        lines.append(f"{sha256(path)}  {path.name}")
    (out / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")

    print(f"release bundle for {version} at {commit[:12]}{' (dirty)' if dirty else ''}: {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
