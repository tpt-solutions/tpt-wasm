// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! A table entry that names a host function. The element segment seeds a table slot with an imported
//! function, and a `call_indirect` through that slot calls the host, on the interpreter and on the baseline.

use tpt_wasm_runtime::{Config, Engine, EngineMode, HostFunction};
use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};

/// Returns 7 to whoever calls it.
struct Seven;

impl HostFunction for Seven {
    fn function_type(&self) -> FunctionType {
        FunctionType {
            params: ResultType(Vec::new()),
            results: ResultType(vec![ValueType::I32]),
        }
    }

    fn call(&self, _args: &[Value]) -> Result<Vec<Value>, String> {
        Ok(vec![Value::I32(7)])
    }
}

/// Import `env.h` is function 0, `() -> i32`. Slot 0 of the table is seeded with function 0, so the host.
/// Function 1, exported as `c`, calls through slot 0.
fn module() -> Vec<u8> {
    let mut module = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    module.extend([1, 5, 1, 0x60, 0, 1, 0x7f]); // type 0: () -> i32
    module.extend([2, 9, 1, 3, b'e', b'n', b'v', 1, b'h', 0x00, 0]); // env.h is type 0
    module.extend([3, 2, 1, 0]); // one defined function, of type 0
    module.extend([4, 4, 1, 0x70, 0, 1]); // one funcref table, minimum 1
    module.extend([7, 5, 1, 1, b'c', 0, 1]); // export c = function 1
    module.extend([9, 7, 1, 0, 0x41, 0, 0x0b, 1, 0]); // slot 0 is function 0, the import
    module.extend([10, 9, 1, 7, 0x00, 0x41, 0, 0x11, 0, 0, 0x0b]); // c: call_indirect type 0, slot 0
    module
}

#[test]
fn a_table_entry_naming_a_host_function_calls_the_host_on_both_engines() {
    for mode in [EngineMode::Micro, EngineMode::Baseline] {
        let mut engine = Engine::new(Config {
            engine_mode: mode,
            ..Config::default()
        })
        .expect("engine");
        engine.linker_mut().define_function("env", "h", Seven);
        let mut instance = engine
            .instantiate_bytes(&module())
            .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
        assert_eq!(
            instance
                .call("c", Vec::new())
                .expect("calls through the table"),
            vec![Value::I32(7)],
            "{mode:?}: the slot's host function answers"
        );
    }
}
