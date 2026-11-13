//! Globals in compiled code: the array the caller provides is read and written, a call without it is
//! refused, and an index past its end traps instead of reading memory it does not own.
#![cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        unix,
        any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        )
    )
))]

use tpt_wasm_jit::{compile_module, CompiledModule, JitError};
use tpt_wasm_types::Trap;

/// Append a section with `content`, which must be shorter than 128 bytes.
fn section(bytes: &mut Vec<u8>, id: u8, content: &[u8]) {
    bytes.push(id);
    bytes.push(content.len() as u8);
    bytes.extend(content);
}

/// A module with a mutable `i32` global, and one exported function `f` of type `(i32) -> i32` with
/// the given body.
fn module(body: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    section(&mut bytes, 1, &[0x01, 0x60, 0x01, 0x7f, 0x01, 0x7f]);
    section(&mut bytes, 3, &[0x01, 0x00]);
    section(&mut bytes, 6, &[0x01, 0x7f, 0x01, 0x41, 0x00, 0x0b]);
    section(&mut bytes, 7, &[0x01, 0x01, b'f', 0x00, 0x00]);
    let mut body_bytes = vec![0x00];
    body_bytes.extend(body);
    let mut code = vec![0x01, body_bytes.len() as u8];
    code.extend(body_bytes);
    section(&mut bytes, 10, &code);
    bytes
}

/// Compile the module, as the runtime does.
fn compiled(bytes: &[u8]) -> CompiledModule {
    let decoded = tpt_wasm_decode::decode(bytes).expect("the module decodes");
    let validated = tpt_wasm_validate::validate(decoded).expect("the module validates");
    let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("the module lowers");
    compile_module(verified.module(), 1_000, true)
}

/// `f(x)`: `global = global + x`, and the new global.
fn accumulate() -> Vec<u8> {
    module(&[
        0x23, 0x00, // global.get 0
        0x20, 0x00, // local.get 0
        0x6a, // i32.add
        0x24, 0x00, // global.set 0
        0x23, 0x00, // global.get 0
        0x0b,
    ])
}

/// `f(x)`: `global = x`, then `1 / x`.
fn store_then_divide() -> Vec<u8> {
    module(&[
        0x20, 0x00, // local.get 0
        0x24, 0x00, // global.set 0
        0x41, 0x01, // i32.const 1
        0x20, 0x00, // local.get 0
        0x6d, // i32.div_s
        0x0b,
    ])
}

#[test]
fn the_globals_the_caller_provides_are_read_and_written() {
    let module = compiled(&accumulate());
    assert!(module.is_compiled(0), "the function is compiled");
    let mut globals = [10u64];
    let result = module
        .call_with_globals(0, &[5], None, &mut globals)
        .expect("the call runs");
    assert_eq!(
        result as u32, 15,
        "the function reads the global and adds the argument"
    );
    assert_eq!(
        globals[0] as u32, 15,
        "the store is written back to the caller"
    );
}

#[test]
fn a_module_with_globals_is_refused_without_them() {
    let module = compiled(&accumulate());
    assert!(
        matches!(module.call(0, &[1], None), Err(JitError::Unsupported(_))),
        "a call that provides no globals is refused rather than run"
    );
}

#[test]
fn an_index_past_the_provided_globals_traps_without_reading_them() {
    let module = compiled(&accumulate());
    // The function's global is index 0, and the caller provides none: the code checks the length
    // before it reads, so the call reports the missing global instead of reading an empty array.
    let result = module.call_with_globals(0, &[1], None, &mut []);
    assert!(
        matches!(result, Err(JitError::Unsupported(_))),
        "a missing global is reported, got {result:?}"
    );
}

#[test]
fn a_store_before_a_trap_is_written_back() {
    let module = compiled(&store_then_divide());
    let mut globals = [9u64];
    let result = module.call_with_globals(0, &[0], None, &mut globals);
    assert_eq!(
        result,
        Err(JitError::Trap(Trap::IntegerDivisionByZero)),
        "a zero divisor traps"
    );
    assert_eq!(
        globals[0], 0,
        "the store before the trap stays, as on Micro"
    );
}
