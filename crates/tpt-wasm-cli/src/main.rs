// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! tpt-wasm CLI — command-line WebAssembly runner and tool.

fn main() {
    println!("tpt-wasm {}", env!("CARGO_PKG_VERSION"));
    // TODO(M3): implement CLI subcommands (run, validate, inspect, diff)
}
