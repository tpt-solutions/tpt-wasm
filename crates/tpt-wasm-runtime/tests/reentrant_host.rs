// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! A host function on the interpreter calls back into the instance that called it. The nested call runs
//! on the same store, and its effects are visible to the outer call. Unbounded re-entry is refused.

use tpt_wasm_runtime::{Caller, Config, Engine, EngineMode, HostFunction, RuntimeError};
use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};

/// Type 0 is `(i32) -> i32`, and type 1 is `() -> i32`. Import `env.h` is function 0.
fn type_section() -> Vec<u8> {
    vec![0x01, 10, 2, 0x60, 1, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f]
}

fn import_h() -> Vec<u8> {
    vec![0x02, 9, 1, 3, b'e', b'n', b'v', 1, b'h', 0x00, 0]
}

/// `run() -> i32` calls `h(3)`. `double(x) -> i32` stores `x * 2` at address 0 and returns it.
fn doubling_module() -> Vec<u8> {
    let mut module = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    module.extend(type_section());
    module.extend(import_h());
    module.extend([0x03, 3, 2, 1, 0]); // run is type 1, double is type 0
    module.extend([0x05, 3, 1, 0x00, 1]); // one memory, one page
    module.extend([
        0x07, 0x19, 3, // three exports
        3, b'r', b'u', b'n', 0x00, 1, //
        6, b'd', b'o', b'u', b'b', b'l', b'e', 0x00, 2, //
        6, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0,
    ]);
    module.extend([
        0x0a, 0x1a, 2, // two bodies
        6, 0x00, 0x41, 0x03, 0x10, 0x00, 0x0b, // run: h(3)
        17, 0x00, // double: no locals
        0x41, 0x00, // address 0
        0x20, 0x00, 0x41, 0x02, 0x6c, // x * 2
        0x36, 0x02, 0x00, // i32.store
        0x20, 0x00, 0x41, 0x02, 0x6c, // x * 2, the result
        0x0b,
    ]);
    module
}

/// `h(x)` calls the caller's `double` with `x`, and returns the result plus one.
struct AddOneToDouble;

impl HostFunction for AddOneToDouble {
    fn function_type(&self) -> FunctionType {
        FunctionType {
            params: ResultType(vec![ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        }
    }

    fn call(&self, _args: &[Value]) -> Result<Vec<Value>, String> {
        Err("h needs its caller".into())
    }

    fn call_with_caller(
        &self,
        caller: &mut dyn Caller,
        args: &[Value],
    ) -> Result<Vec<Value>, String> {
        let Value::I32(x) = args[0] else {
            return Err("h takes an i32".into());
        };
        let double = caller
            .export_function("double")
            .ok_or("the caller exports no double")?;
        let doubled = caller.call(double, vec![Value::I32(x)])?;
        let [Value::I32(d)] = doubled.as_slice() else {
            return Err("double did not return one i32".into());
        };
        Ok(vec![Value::I32(d + 1)])
    }
}

/// `run() -> i32` calls `h(0)`, and `h` calls the caller's `run` again, without end.
fn recursive_module() -> Vec<u8> {
    let mut module = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    module.extend(type_section());
    module.extend(import_h());
    module.extend([0x03, 2, 1, 1]); // one function, of type 1
    module.extend([
        0x07, 7, 1, 3, b'r', b'u', b'n', 0x00, 1, //
    ]);
    module.extend([0x0a, 8, 1, 6, 0x00, 0x41, 0x00, 0x10, 0x00, 0x0b]);
    module
}

/// `h` calls the caller's `run`, and returns what it returns.
struct CallRunAgain;

impl HostFunction for CallRunAgain {
    fn function_type(&self) -> FunctionType {
        FunctionType {
            params: ResultType(vec![ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        }
    }

    fn call(&self, _args: &[Value]) -> Result<Vec<Value>, String> {
        Err("h needs its caller".into())
    }

    fn call_with_caller(
        &self,
        caller: &mut dyn Caller,
        _args: &[Value],
    ) -> Result<Vec<Value>, String> {
        let run = caller
            .export_function("run")
            .ok_or("the caller exports no run")?;
        caller.call(run, Vec::new())
    }
}

#[test]
fn a_host_function_calls_back_into_its_caller_and_both_see_the_same_store() {
    let mut engine = Engine::new(Config {
        engine_mode: EngineMode::Micro,
        ..Config::default()
    })
    .expect("engine");
    engine
        .linker_mut()
        .define_function("env", "h", AddOneToDouble);
    let mut instance = engine
        .instantiate_bytes(&doubling_module())
        .expect("instantiates");
    assert_eq!(
        instance.call("run", Vec::new()).expect("runs"),
        vec![Value::I32(7)],
        "h(3) = double(3) + 1 = 6 + 1"
    );
    assert_eq!(
        instance.read_memory("memory", 0, 4).expect("reads"),
        6i32.to_le_bytes().to_vec(),
        "the nested call's store write is visible after the outer call returns"
    );
}

#[test]
fn unbounded_re_entry_is_refused_at_the_depth_limit_rather_than_overflowing_the_host_stack() {
    let mut engine = Engine::new(Config {
        engine_mode: EngineMode::Micro,
        ..Config::default()
    })
    .expect("engine");
    engine
        .linker_mut()
        .define_function("env", "h", CallRunAgain);
    let mut instance = engine
        .instantiate_bytes(&recursive_module())
        .expect("instantiates");
    let error: RuntimeError = instance
        .call("run", Vec::new())
        .expect_err("the calls never end, so they must be refused");
    assert!(
        error.to_string().contains("nest deeper"),
        "the refusal names the depth: {error}"
    );
}
