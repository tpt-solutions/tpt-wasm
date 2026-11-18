// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! A data segment that traps during instantiation keeps the writes before it, on the baseline as on Micro.

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

fn leb(mut value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn section(id: u8, contents: &[u8]) -> Vec<u8> {
    let mut out = vec![id];
    out.extend(leb(contents.len() as u32));
    out.extend_from_slice(contents);
    out
}

fn name(text: &str) -> Vec<u8> {
    let mut out = leb(text.len() as u32);
    out.extend_from_slice(text.as_bytes());
    out
}

/// The exporter: one memory of one page, exported as `mem`.
fn exporter() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    bytes.extend(section(0x05, &[0x01, 0x00, 0x01]));
    let mut exports = vec![0x01];
    exports.extend(name("mem"));
    exports.extend([0x02, 0x00]);
    bytes.extend(section(0x07, &exports));
    bytes
}

/// The importer: imports `m.mem` (one page), then writes `abc` at 0 and a segment past the page.
fn importer() -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    let mut imports = vec![0x01];
    imports.extend(name("m"));
    imports.extend(name("mem"));
    imports.extend([0x02, 0x00, 0x01]);
    bytes.extend(section(0x02, &imports));
    let mut data = vec![0x02];
    data.extend([0x00, 0x41, 0x00, 0x0b]);
    data.extend(leb(3));
    data.extend(b"abc");
    data.extend([0x00, 0x41]);
    data.extend(leb(65536 - 2));
    data.push(0x0b);
    data.extend(leb(3));
    data.extend(b"zzz");
    bytes.extend(section(0x0b, &data));
    bytes
}

#[test]
fn an_imported_memory_keeps_the_writes_before_a_trapping_segment_on_every_engine() {
    for mode in [EngineMode::Micro, EngineMode::Baseline] {
        let mut engine = Engine::new(Config {
            engine_mode: mode,
            ..Config::default()
        })
        .expect("engine");
        let mut exporting = engine.instantiate_bytes(&exporter()).expect("exporter");
        exporting.write_memory("mem", 0, &[0u8; 4]).expect("zeroed");
        let _ = &mut exporting;
        engine
            .linker_mut()
            .define_instance("m", &exporting)
            .expect("registers");
        let result = engine.instantiate_bytes(&importer());
        assert!(result.is_err(), "{mode:?}: the importer traps");
        let bytes = exporting.read_memory("mem", 0, 3).expect("reads");
        assert_eq!(bytes, b"abc", "{mode:?}: the write before the trap stays");
        let _ = Value::I32(0);
    }
}
