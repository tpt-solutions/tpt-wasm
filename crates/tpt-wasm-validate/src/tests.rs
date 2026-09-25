// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Tests for module validation and the decode/validate boundary.

use tpt_wasm_format::{ConstExpr, Export, ExportDesc, Function, Global, Module};
use tpt_wasm_types::{FunctionType, GlobalType, ResultType, ValueType};

use super::{validate, ValidationError};

fn function_type(params: Vec<ValueType>, results: Vec<ValueType>) -> FunctionType {
    FunctionType {
        params: ResultType(params),
        results: ResultType(results),
    }
}

fn const_i32(value: i32) -> ConstExpr {
    let mut bytes = vec![0x41];
    let mut value = value;
    loop {
        let byte = (value as u8) & 0x7f;
        value >>= 7;
        let sign_bit = byte & 0x40 != 0;
        if (value == 0 && !sign_bit) || (value == -1 && sign_bit) {
            bytes.push(byte);
            break;
        }
        bytes.push(byte | 0x80);
    }
    bytes.push(0x0b);
    ConstExpr(bytes)
}

fn function(body: Vec<u8>) -> Function {
    Function {
        type_index: 0,
        locals: vec![],
        body,
    }
}

fn module_with_function(function_type: FunctionType, body: Vec<u8>) -> Module {
    Module {
        types: vec![function_type.clone()],
        functions: vec![function(body)],
        ..Module::default()
    }
}

#[test]
fn empty_module_is_valid() {
    assert!(validate(Module::default()).is_ok());
}

#[test]
fn br_if_consumes_the_label_values_it_branches_with() {
    // `block (result i32) { 7; 0; br_if 0; 99 } end`
    //
    // `br_if` pops the condition and the label's value on *both* paths, so the
    // fall-through starts from an empty stack and may push a fresh `99` for the
    // block's result. A validator that restores the popped value would see two
    // values at the block's `end` and wrongly reject this module.
    let body = vec![
        0x02, 0x7f, // block (result i32)
        0x41, 0x07, // i32.const 7
        0x41, 0x00, // i32.const 0
        0x0d, 0x00, // br_if 0
        0x41, 0xe3, 0x00, // i32.const 99 (signed LEB128, two bytes)
        0x0b, // end (block)
        0x0b, // end (function)
    ];
    let module = module_with_function(function_type(vec![], vec![ValueType::I32]), body);
    assert!(validate(module).is_ok());
}

#[test]
fn br_if_still_requires_the_label_value_to_be_present() {
    // The same shape with no value for the block's result must be rejected, so
    // the test above cannot pass by simply skipping the arity check.
    let body = vec![
        0x02, 0x7f, // block (result i32)
        0x41, 0x00, // i32.const 0
        0x0d, 0x00, // br_if 0
        0x41, 0xe3, 0x00, // i32.const 99
        0x0b, // end (block)
        0x0b, // end (function)
    ];
    let module = module_with_function(function_type(vec![], vec![ValueType::I32]), body);
    assert!(validate(module).is_err());
}

#[test]
fn valid_constant_result_is_accepted() {
    let module = module_with_function(
        function_type(vec![], vec![ValueType::I32]),
        vec![0x41, 0x2a, 0x0b],
    );
    assert!(validate(module).is_ok());
}

#[test]
fn negative_i32_constant_is_accepted() {
    let module = module_with_function(
        function_type(vec![], vec![ValueType::I32]),
        vec![0x41, 0x7f, 0x0b],
    );
    assert!(validate(module).is_ok());
}

#[test]
fn result_type_mismatch_is_rejected() {
    let module = module_with_function(
        function_type(vec![], vec![ValueType::I32]),
        vec![0x42, 0x00, 0x0b],
    );
    assert!(matches!(
        validate(module),
        Err(ValidationError::TypeMismatch { .. })
    ));
}

#[test]
fn unknown_type_index_is_rejected_by_validation() {
    let mut module = module_with_function(function_type(vec![], vec![]), vec![0x0b]);
    module.functions[0].type_index = 7;
    assert_eq!(validate(module), Err(ValidationError::UnknownType(7)));
}

#[test]
fn unknown_call_index_is_rejected() {
    let module = module_with_function(function_type(vec![], vec![]), vec![0x10, 0x01, 0x0b]);
    assert_eq!(validate(module), Err(ValidationError::UnknownFunction(1)));
}

#[test]
fn immutable_global_write_is_rejected() {
    let module = Module {
        types: vec![function_type(vec![], vec![])],
        functions: vec![function(vec![0x41, 0x00, 0x24, 0x00, 0x0b])],
        globals: vec![Global {
            global_type: GlobalType {
                value_type: ValueType::I32,
                mutable: false,
            },
            init: const_i32(0),
        }],
        ..Module::default()
    };
    assert_eq!(validate(module), Err(ValidationError::ImmutableGlobal(0)));
}

#[test]
fn duplicate_exports_are_rejected() {
    let module = Module {
        types: vec![function_type(vec![], vec![])],
        functions: vec![function(vec![0x0b])],
        exports: vec![
            Export {
                name: "same".into(),
                desc: ExportDesc::Function(0),
            },
            Export {
                name: "same".into(),
                desc: ExportDesc::Function(0),
            },
        ],
        ..Module::default()
    };
    assert_eq!(
        validate(module),
        Err(ValidationError::DuplicateExport("same".into()))
    );
}

#[test]
fn missing_function_end_is_rejected() {
    let module = module_with_function(function_type(vec![], vec![]), vec![]);
    assert!(matches!(
        validate(module),
        Err(ValidationError::InvalidBody(_))
    ));
}

#[test]
fn global_initializer_type_is_checked() {
    let module = Module {
        globals: vec![Global {
            global_type: GlobalType {
                value_type: ValueType::I64,
                mutable: false,
            },
            init: const_i32(0),
        }],
        ..Module::default()
    };
    assert!(matches!(
        validate(module),
        Err(ValidationError::TypeMismatch { .. })
    ));
}

#[test]
fn validator_accepts_a_structurally_valid_empty_body() {
    let module = module_with_function(function_type(vec![], vec![]), vec![0x0b]);
    assert!(validate(module).is_ok());
}

#[test]
fn nested_block_results_are_propagated_to_the_function_frame() {
    let module = module_with_function(
        function_type(Vec::new(), vec![ValueType::I32]),
        vec![0x02, 0x7f, 0x41, 0x2a, 0x0b, 0x0b],
    );
    assert!(validate(module).is_ok());
}
