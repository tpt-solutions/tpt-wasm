#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 TPT Solutions
#
# Reproducible build check: build the CLI release binary twice, each time in a
# fresh target directory, and fail unless the two binaries are byte-identical.
#
# What is controlled, and why:
# * `--locked` builds exactly the versions in Cargo.lock.
# * Each build has its own CARGO_TARGET_DIR, so no artifact is reused between runs.
# * CARGO_INCREMENTAL=0, because incremental state is part of what a build
#   depends on and it is not reproducible across fresh directories.
# * `--remap-path-prefix` replaces the checkout path with a fixed one, so the
#   binary does not record where it was built. Without it a build in another
#   directory would differ in its embedded paths.
#
# This does not prove a build is trustworthy. It proves the build is a function of
# the source and the lock file, which is what a release reviewer needs to rebuild
# and compare.

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

build() {
  local name="$1"
  # CARGO_ENCODED_RUSTFLAGS separates flags with 0x1f, not spaces, so a checkout
  # path containing a space survives intact. RUSTFLAGS would split it in two.
  local flags
  flags="$(printf -- '--remap-path-prefix=%s=/tpt-wasm' "$root")"
  if [ "${OS:-}" = "Windows_NT" ]; then
    # The MSVC linker records a timestamp and a PDB path, and the PDB path contains
    # the target directory, which differs between the two builds. /Brepro replaces
    # the timestamp with a content hash, and /DEBUG:NONE drops the PDB, so the
    # binary depends only on its contents. The cost is that a Windows release
    # binary ships without a separate PDB, so crash dumps lose their symbols.
    flags="$flags"$'\x1f'"-C"$'\x1f'"link-arg=/Brepro"$'\x1f'"-C"$'\x1f'"link-arg=/DEBUG:NONE"
  fi
  CARGO_TARGET_DIR="$work/$name" \
  CARGO_INCREMENTAL=0 \
  CARGO_ENCODED_RUSTFLAGS="$flags" \
    cargo build --release --locked --manifest-path "$root/Cargo.toml" -p tpt-wasm-cli >/dev/null
  cp "$work/$name/release/tpt-wasm$( [ "${OS:-}" = "Windows_NT" ] && echo .exe )" "$work/$name.bin"
}

build first
build second

first_hash="$(sha256sum "$work/first.bin" | cut -d' ' -f1)"
second_hash="$(sha256sum "$work/second.bin" | cut -d' ' -f1)"
echo "first  $first_hash"
echo "second $second_hash"

if [ "$first_hash" != "$second_hash" ]; then
  echo "::error::two builds of the same source and lock file differ"
  exit 1
fi
echo "reproducible: both builds are byte-identical"
