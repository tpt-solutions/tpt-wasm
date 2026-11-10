#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 TPT Solutions
#
# Counts `unsafe` code per crate and enforces the zero-unsafe policy.
#
# The policy (todo.md, "Track unsafe counts per crate"): the crates listed in
# ZERO_UNSAFE_CRATES must contain no `unsafe` at all, and the rest may only
# contain it at the executable-memory boundary once JIT/AOT land. The count is
# taken over code only: a `//` comment, including a doc comment, is removed
# before matching, so the word in prose does not count. A line that is a string
# literal containing `//` is cut short, which can only under-count; the
# `#![forbid(unsafe_code)]` attribute on the zero-policy crates is the compiler's
# own check and does not depend on this script.
#
# Usage: scripts/unsafe-count.sh            report every crate, fail on policy
#        scripts/unsafe-count.sh --quiet    print only failures

set -euo pipefail

ZERO_UNSAFE_CRATES="tpt-wasm-micro tpt-wasm-host tpt-wasm-capability tpt-wasm-resource"

root="$(cd "$(dirname "$0")/.." && pwd)"
quiet=0
[ "${1:-}" = "--quiet" ] && quiet=1

count_unsafe() {
  # Strip line comments, then count lines with `unsafe` as a whole word.
  find "$1" -name '*.rs' -print0 |
    xargs -0 awk '{ line = $0; sub(/\/\/.*$/, "", line); if (line ~ /(^|[^A-Za-z0-9_])unsafe([^A-Za-z0-9_]|$)/) n++ } END { print n + 0 }' |
    awk '{ total += $1 } END { print total + 0 }'
}

failures=0
for dir in "$root"/crates/*/; do
  name="$(basename "$dir")"
  src="$dir/src"
  [ -d "$src" ] || continue
  n="$(count_unsafe "$src")"
  zero=0
  for z in $ZERO_UNSAFE_CRATES; do
    [ "$z" = "$name" ] && zero=1
  done
  if [ "$zero" = 1 ] && [ "$n" -ne 0 ]; then
    echo "::error::$name must contain no unsafe code, found $n occurrence(s)"
    failures=$((failures + 1))
  elif [ "$quiet" = 0 ]; then
    policy="policy: none"
    [ "$zero" = 1 ] && policy="policy: zero"
    printf '%-24s %4s  (%s)\n' "$name" "$n" "$policy"
  fi
done

if [ "$failures" -ne 0 ]; then
  echo "unsafe audit failed: $failures crate(s) violate the zero-unsafe policy"
  exit 1
fi
[ "$quiet" = 0 ] && echo "unsafe audit passed"
exit 0
