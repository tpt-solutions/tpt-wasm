//! `memory.grow` run natively through `EngineMode::Jit`, checked against Micro: the sizes it reports, the
//! failures past the maximum, the memory it adds (which loads and stores reach), and the traps past the end.
#![cfg(feature = "jit")]

use tpt_wasm_runtime::{Config, Engine, EngineMode};
use tpt_wasm_types::Value;

/// An unsigned LEB128 integer.
fn leb(mut value: usize) -> Vec<u8> {
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

fn section(bytes: &mut Vec<u8>, id: u8, content: &[u8]) {
    bytes.push(id);
    bytes.extend(leb(content.len()));
    bytes.extend(content);
}

/// A module with one memory of one page and the given maximum, and the exports:
/// `grow(delta) -> old size or -1`, `size() -> pages`, `store(addr, value) -> value`, `load(addr) -> value`.
fn module(max_pages: usize) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    // types: (i32) -> i32; (i32, i32) -> i32; () -> i32
    section(
        &mut bytes,
        1,
        &[
            0x03, 0x60, 0x01, 0x7f, 0x01, 0x7f, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f, 0x60, 0x00,
            0x01, 0x7f,
        ],
    );
    // four functions: grow (type 0), store (type 1), load (type 0), size (type 2)
    section(&mut bytes, 3, &[0x04, 0x00, 0x01, 0x00, 0x02]);
    // one memory: limits with a maximum
    let mut memory = vec![0x01, 0x01];
    memory.extend(leb(max_pages));
    let mut memory_section = vec![0x01];
    memory_section.extend(memory);
    let mut memory_section_bytes = vec![0x01];
    memory_section_bytes.extend(&memory_section[1..]);
    section(&mut bytes, 5, &memory_section_bytes);
    // exports: grow, store, load, size, and the memory as "memory"
    let mut exports = vec![0x05];
    for (name, kind, index) in [
        ("grow", 0x00u8, 0u8),
        ("store", 0x00, 1),
        ("load", 0x00, 2),
        ("size", 0x00, 3),
    ] {
        exports.push(name.len() as u8);
        exports.extend(name.as_bytes());
        exports.extend([kind, index]);
    }
    exports.extend([0x06]);
    exports.extend(b"memory");
    exports.extend([0x02, 0x00]);
    section(&mut bytes, 7, &exports);
    // bodies: grow, store, load, size
    let bodies: [Vec<u8>; 4] = [
        vec![0x20, 0x00, 0x40, 0x00, 0x0b], // local.get 0; memory.grow; end
        vec![
            0x20, 0x00, 0x20, 0x01, 0x36, 0x02, 0x00, // i32.store at local 0 of local 1
            0x20, 0x01, 0x0b, // local.get 1; end
        ],
        vec![0x20, 0x00, 0x28, 0x02, 0x00, 0x0b], // local.get 0; i32.load; end
        vec![0x3f, 0x00, 0x0b],                   // memory.size; end
    ];
    let mut code = vec![0x04];
    for body in bodies {
        let mut function = vec![0x00];
        function.extend(body);
        code.extend(leb(function.len()));
        code.extend(function);
    }
    section(&mut bytes, 10, &code);
    bytes
}

/// The outcome of each call in order, the size in pages afterwards, and whether `grow` ran natively.
fn run(
    mode: EngineMode,
    max_pages: usize,
    calls: &[(&str, Vec<Value>)],
) -> (Vec<String>, Option<u64>, bool) {
    let engine = Engine::new(Config {
        engine_mode: mode,
        ..Config::default()
    })
    .expect("engine");
    let mut instance = engine
        .instantiate_bytes(&module(max_pages))
        .expect("module instantiates");
    let natively = instance.runs_natively("grow");
    let outcomes = calls
        .iter()
        .map(|(name, arguments)| format!("{:?}", instance.call(name, arguments.clone())))
        .collect();
    (outcomes, instance.memory_pages(), natively)
}

fn sequence() -> Vec<(&'static str, Vec<Value>)> {
    vec![
        ("size", vec![]),
        ("grow", vec![Value::I32(1)]),
        ("size", vec![]),
        ("grow", vec![Value::I32(0)]),
        ("grow", vec![Value::I32(3)]), // 2 + 3 is past the maximum of 4: -1, and no change
        ("grow", vec![Value::I32(2)]), // reaches the maximum
        ("size", vec![]),
        ("grow", vec![Value::I32(1)]), // -1
        (
            "store",
            vec![Value::I32(4 * 65_536 - 4), Value::I32(0x1234)],
        ),
        ("load", vec![Value::I32(4 * 65_536 - 4)]),
        ("store", vec![Value::I32(65_536 + 8), Value::I32(-7)]),
        ("load", vec![Value::I32(65_536 + 8)]),
        ("load", vec![Value::I32(4 * 65_536 - 3)]), // a load that runs past the end traps
    ]
}

#[test]
fn grow_within_the_maximum_runs_natively_and_agrees_with_micro() {
    let calls = sequence();
    let (golden, golden_pages, _) = run(EngineMode::Micro, 4, &calls);
    let (native, native_pages, natively) = run(EngineMode::Jit, 4, &calls);
    assert!(natively, "grow runs as machine code, in place");
    assert_eq!(
        native, golden,
        "the results, the failures and the traps agree with Micro"
    );
    assert_eq!(
        native_pages, golden_pages,
        "the final size agrees with Micro"
    );
    assert_eq!(native_pages, Some(4));
    assert_eq!(native[0], "Ok([I32(1)])");
    assert_eq!(native[1], "Ok([I32(1)])", "grow returns the old size");
    assert_eq!(native[4], "Ok([I32(-1)])", "a grow past the maximum fails");
    assert_eq!(
        native[8], "Ok([I32(4660)])",
        "the memory the grow added is stored to"
    );
    assert!(
        native.last().unwrap().starts_with("Err("),
        "the load past the end traps"
    );
}

#[test]
fn a_maximum_above_the_reservation_limit_stays_on_the_baseline() {
    // 1024 pages is above the cap, so the memory is not reserved and grow is left to the baseline.
    let calls = sequence();
    let (golden, golden_pages, _) = run(EngineMode::Micro, 1024, &calls);
    let (native, native_pages, natively) = run(EngineMode::Jit, 1024, &calls);
    assert!(!natively, "grow is not compiled for a memory this large");
    assert_eq!(native, golden);
    assert_eq!(native_pages, golden_pages);
}

#[test]
fn the_engine_memory_limit_bounds_growth_on_every_engine() {
    // The module allows four pages, the engine two. Growing by three from one page is past the limit, so it
    // fails and leaves the memory alone; growing by one then succeeds, on the interpreter and on both compiled tiers.
    let outcomes: Vec<Vec<String>> = [EngineMode::Micro, EngineMode::Baseline, EngineMode::Jit]
        .into_iter()
        .map(|mode| {
            let engine = Engine::new(Config {
                engine_mode: mode,
                limits: tpt_wasm_types::ResourceLimits {
                    max_memory_pages: 2,
                    ..Default::default()
                },
                ..Config::default()
            })
            .expect("engine");
            let mut instance = engine
                .instantiate_bytes(&module(4))
                .expect("module instantiates");
            vec![
                format!("{:?}", instance.call("grow", vec![Value::I32(3)])),
                format!("{:?}", instance.call("grow", vec![Value::I32(1)])),
            ]
        })
        .collect();
    assert_eq!(outcomes[0], ["Ok([I32(-1)])", "Ok([I32(1)])"]);
    assert_eq!(outcomes[1], outcomes[0], "the baseline agrees with Micro");
    assert_eq!(
        outcomes[2], outcomes[0],
        "the native tier agrees with Micro"
    );
}
