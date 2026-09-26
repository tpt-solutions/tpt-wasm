// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Tests for module validation and the decode/validate boundary.

use tpt_wasm_format::{ConstExpr, Export, ExportDesc, Function, Global, Module};
use tpt_wasm_types::{FunctionType, GlobalType, ReferenceType, ResultType, ValueType};

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
fn br_if_leaves_the_label_values_on_the_fall_through_path() {
    // `block (result i32) { 7; 0; br_if 0; 99 } end`
    //
    // `br_if l` has type `[t* i32] -> [t*]`: the taken edge hands `t*` to the
    // label, and the fall-through keeps the same values. So after the `br_if` the
    // block's `7` is still on the stack, and pushing another value for the
    // block's result leaves two operands at the `end`. This module is invalid,
    // and a validator that dropped the value instead of restoring it would
    // wrongly accept it.
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
    assert!(validate(module).is_err());
}

#[test]
fn br_if_fall_through_may_reuse_the_value_it_carries() {
    // The valid form of the shape above: the carried value is the block's result
    // either way, so the fall-through drops it before producing a replacement.
    let body = vec![
        0x02, 0x7f, // block (result i32)
        0x41, 0x07, // i32.const 7
        0x41, 0x00, // i32.const 0
        0x0d, 0x00, // br_if 0
        0x1a, // drop
        0x41, 0xe3, 0x00, // i32.const 99
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
fn reference_instructions_are_validated() {
    // `ref.null funcref; ref.is_null` leaves an `i32` for the result.
    let body = vec![0xd0, 0x70, 0xd1, 0x0b];
    assert!(validate(module_with_function(
        function_type(vec![], vec![ValueType::I32]),
        body
    ))
    .is_ok());

    // `ref.func 0` names a real function, so it type-checks as a `funcref`.
    let body = vec![0xd2, 0x00, 0xd1, 0x0b];
    assert!(validate(module_with_function(
        function_type(vec![], vec![ValueType::I32]),
        body
    ))
    .is_ok());
}

#[test]
fn a_ref_func_naming_no_function_is_rejected() {
    let body = vec![0xd2, 0x07, 0xd1, 0x0b];
    assert_eq!(
        validate(module_with_function(
            function_type(vec![], vec![ValueType::I32]),
            body
        )),
        Err(ValidationError::UnknownFunction(7))
    );
}

#[test]
fn ref_is_null_requires_a_reference_operand() {
    // `ref.is_null` accepts either reference kind but nothing else, so an
    // `i32` operand is a type error rather than a silently coerced 0.
    let body = vec![0x41, 0x00, 0xd1, 0x0b];
    assert_eq!(
        validate(module_with_function(
            function_type(vec![], vec![ValueType::I32]),
            body
        )),
        Err(ValidationError::TypeMismatch {
            expected: "reference".into(),
            actual: "i32".into(),
        })
    );
}

#[test]
fn an_unknown_reference_type_immediate_is_rejected() {
    // 0x71 is not a reference type, so `ref.null` cannot read one.
    let body = vec![0xd0, 0x71, 0xd1, 0x0b];
    assert!(matches!(
        validate(module_with_function(
            function_type(vec![], vec![ValueType::I32]),
            body
        )),
        Err(ValidationError::InvalidBody(_))
    ));
}

#[test]
fn a_reference_may_appear_in_a_signature_and_a_local() {
    // The reference instructions are only usable if a reference can be stored,
    // so the type surface accepts `funcref` in a local and a result.
    let signature = function_type(vec![], vec![ValueType::Ref(ReferenceType::FuncRef)]);
    let module = Module {
        types: vec![signature],
        functions: vec![Function {
            type_index: 0,
            locals: vec![tpt_wasm_format::LocalDecl {
                count: 1,
                value_type: ValueType::Ref(ReferenceType::FuncRef),
            }],
            body: vec![0xd0, 0x70, 0x21, 0x00, 0x20, 0x00, 0x0b],
        }],
        ..Module::default()
    };
    assert!(validate(module).is_ok());
}

#[test]
fn a_vector_type_is_still_rejected() {
    // The reference instructions do not open the vector proposal: `v128` still
    // has no instructions to move it, so the type stays out of the surface.
    let vector = ValueType::V128;
    assert_eq!(
        validate(module_with_function(
            function_type(vec![], vec![vector]),
            vec![0x0b]
        )),
        Err(ValidationError::UnsupportedFeature(
            "unsupported value type"
        ))
    );
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
