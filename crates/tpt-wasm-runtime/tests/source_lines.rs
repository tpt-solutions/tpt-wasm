// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Debug sessions report the source line of each frame, from a module's `.debug_line` section.

use tpt_wasm_decode::function_body_offsets;
use tpt_wasm_runtime::{Config, Engine, EngineMode};

/// `f() -> i32`, whose body is `i32.const 1; i32.const 2; i32.add; end`. The instructions start
/// at body offsets 0, 2, 4 and 5, and the body starts at the offset the code section gives it.
fn base_module() -> Vec<u8> {
    let mut module = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    module.extend_from_slice(&[0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f]); // type: () -> i32
    module.extend_from_slice(&[0x03, 0x02, 0x01, 0x00]); // one function of type 0
    module.extend_from_slice(&[0x07, 0x05, 0x01, 0x01, b'f', 0x00, 0x00]); // export "f"
    let body = [0x41, 0x01, 0x41, 0x02, 0x6a, 0x0b];
    let mut code = vec![0x01, (body.len() + 1) as u8, 0x00];
    code.extend_from_slice(&body);
    module.push(0x0a);
    module.push((code.len()) as u8);
    module.extend_from_slice(&code);
    module
}

/// A version 4 unit whose sequence starts at `start` and maps the four instructions of the body
/// to lines 3, 4, 5 and 5, with the sequence ending one past the `end` opcode.
fn line_unit(start: u64) -> Vec<u8> {
    let mut program = vec![0x00, 0x09, 0x02];
    program.extend_from_slice(&start.to_le_bytes());
    program.extend_from_slice(&[0x03, 0x02, 0x01]); // line 3, row at start
    program.extend_from_slice(&[0x02, 0x02, 0x03, 0x01, 0x01]); // +2 bytes, line 4, row
    program.extend_from_slice(&[0x02, 0x02, 0x03, 0x01, 0x01]); // +2 bytes, line 5, row at start + 4
    program.extend_from_slice(&[0x02, 0x02, 0x00, 0x01, 0x01]); // +2 bytes, end at start + 6
    let header = [
        1, 1, 1, 0xfb, 14,
        13, // min_inst, max_ops, default_is_stmt, line_base, line_range, opcode_base
        0, 1, 1, 1, 1, 0, 0, 0, 1, 0, 0, 1, // standard operand counts
        0, // no include directories
        b'm', b'.', b'c', 0, 0, 0, 0, 0, // file m.c, directory 0, mtime 0, length 0
        0, // end of the file list
    ];
    let mut after_length = 4u16.to_le_bytes().to_vec();
    after_length.extend_from_slice(&(header.len() as u32).to_le_bytes());
    after_length.extend_from_slice(&header);
    after_length.extend_from_slice(&program);
    let mut unit = (after_length.len() as u32).to_le_bytes().to_vec();
    unit.extend_from_slice(&after_length);
    unit
}

/// Append a custom section named `name` with `data`.
fn with_custom_section(mut module: Vec<u8>, name: &str, data: &[u8]) -> Vec<u8> {
    let mut payload = vec![name.len() as u8];
    payload.extend_from_slice(name.as_bytes());
    payload.extend_from_slice(data);
    module.push(0x00);
    module.push(payload.len() as u8);
    module.extend_from_slice(&payload);
    module
}

/// The module with line information for its one function.
fn module_with_lines() -> Vec<u8> {
    let base = base_module();
    let body_start = function_body_offsets(&base).expect("the fixture's code section")[0] as u64;
    with_custom_section(base, ".debug_line", &line_unit(body_start))
}

fn micro() -> Engine {
    Engine::new(Config {
        engine_mode: EngineMode::Micro,
        ..Config::default()
    })
    .expect("micro is accepted")
}

#[test]
fn each_step_reports_the_line_of_the_instruction_it_stops_before() {
    let module = module_with_lines();
    let engine = micro();
    let mut instance = engine.instantiate_bytes(&module).expect("instantiates");
    let mut session = instance.debug("f", Vec::new()).expect("a session");

    let line = |session: &tpt_wasm_runtime::DebugSession<'_>| {
        session
            .source_location(0)
            .expect("line information reads")
            .map(|location| (location.file, location.line))
    };
    assert_eq!(
        line(&session),
        Some(("m.c".to_owned(), 3)),
        "the first const"
    );
    session.step().expect("steps");
    assert_eq!(
        line(&session),
        Some(("m.c".to_owned(), 4)),
        "the second const"
    );
    session.step().expect("steps");
    assert_eq!(line(&session), Some(("m.c".to_owned(), 5)), "the add");
    session.step().expect("steps");
    assert_eq!(
        line(&session),
        Some(("m.c".to_owned(), 5)),
        "the end, still in line 5"
    );
}

#[test]
fn a_module_without_line_information_has_no_locations() {
    let engine = micro();
    let mut instance = engine
        .instantiate_bytes(&base_module())
        .expect("instantiates");
    let session = instance.debug("f", Vec::new()).expect("a session");
    assert_eq!(session.source_location(0), Ok(None));
    assert_eq!(session.source_location(7), Ok(None), "no such frame");
}

#[test]
fn unreadable_line_information_is_reported_and_does_not_refuse_the_module() {
    let base = base_module();
    let module = with_custom_section(base, ".debug_line", &[0x02, 0x00, 0x00, 0x00, 0x00, 0x00]);
    let engine = micro();
    let mut instance = engine
        .instantiate_bytes(&module)
        .expect("the module still instantiates");
    let session = instance.debug("f", Vec::new()).expect("a session");
    assert!(
        session.source_location(0).is_err(),
        "a version 2 unit is reported, not ignored"
    );
}

#[test]
fn the_module_still_runs_to_the_same_result() {
    use tpt_wasm_types::Value;
    let engine = micro();
    let mut with = engine
        .instantiate_bytes(&module_with_lines())
        .expect("instantiates");
    let mut without = engine
        .instantiate_bytes(&base_module())
        .expect("instantiates");
    assert_eq!(
        with.call("f", Vec::new()).expect("runs"),
        without.call("f", Vec::new()).expect("runs")
    );
    assert_eq!(
        with.call("f", Vec::new()).expect("runs"),
        vec![Value::I32(3)]
    );
}
