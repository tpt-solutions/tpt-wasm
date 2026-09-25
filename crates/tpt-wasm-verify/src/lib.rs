// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Verification infrastructure.
//!
//! Contains: invariants, contracts, model checking, property tests,
//! semantic correspondence checks, proof harnesses.
//!
//! Verification ladder:
//!   V0  Type invariants
//!   V1  Interpreter correspondence
//!   V2  Memory safety
//!   V3  Host capability safety
//!   V4  IR refinement
//!   V5  Compiler transformation correctness
//!   V6  Machine-code refinement
//!
//! Lean 4 proofs live in formal/ at repository root.
//! The V0 type/configuration checker and the initial V4 IR projection are
//! executable Rust; the proof assistant and full V1-V4 properties remain pending.

mod ir_projection;

pub use ir_projection::{project_function_to_model, IrProjectionError};

/// Run the executable V0 configuration/type invariant checker.
pub fn check_v0(
    configuration: &tpt_wasm_semantics::Configuration,
) -> Result<(), tpt_wasm_semantics::ModelError> {
    configuration.check_invariants()
}

/// Verification level identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VerificationLevel {
    V0TypeInvariants,
    V1InterpreterCorrespondence,
    V2MemorySafety,
    V3HostCapabilitySafety,
    V4IrRefinement,
    V5CompilerTransformations,
    V6MachineCodeRefinement,
}

#[cfg(test)]
mod tests {
    use super::{check_v0, project_function_to_model};
    use tpt_wasm_format::{Function, LocalDecl, Module};
    use tpt_wasm_ir::lower_and_verify;
    use tpt_wasm_semantics::{
        Configuration, FloatConversion, FloatTrunc, Frame, ModelError, Store, Transition,
    };
    use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};
    use tpt_wasm_validate::Validator;

    #[test]
    fn v0_checker_rejects_unknown_frame_function() {
        let mut configuration = Configuration::default();
        configuration.frames.push(Frame {
            function_index: 0,
            locals: Vec::new(),
            pc: 0,
            return_arity: 0,
            operand_base: 0,
            control_base: 0,
        });
        assert_eq!(
            check_v0(&configuration),
            Err(ModelError::InvariantViolation(
                "frame references unknown function"
            ))
        );
    }

    fn module(body: Vec<u8>, result: Option<ValueType>) -> Module {
        Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(result.into_iter().collect()),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body,
            }],
            ..Module::default()
        }
    }

    #[test]
    fn certified_ir_projects_and_executes_in_the_formal_model() {
        let module = module(vec![0x41, 20, 0x41, 22, 0x6a, 0x0b], Some(ValueType::I32));
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert_eq!(
            function.body,
            vec![
                tpt_wasm_semantics::Instruction::I32Const(20),
                tpt_wasm_semantics::Instruction::I32Const(22),
                tpt_wasm_semantics::Instruction::AddI32,
                tpt_wasm_semantics::Instruction::End,
            ]
        );

        let mut configuration = Configuration {
            store: Store {
                functions: vec![function],
                ..Store::default()
            },
            ..Configuration::default()
        };
        configuration
            .push_frame(Frame {
                function_index: 0,
                locals: Vec::new(),
                pc: 0,
                return_arity: 1,
                operand_base: 0,
                control_base: 0,
            })
            .unwrap();
        for _ in 0..3 {
            assert!(matches!(configuration.step().unwrap(), Transition::Step(_)));
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![Value::I32(42)])
        );
    }

    #[test]
    fn certified_local_projection_executes() {
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: vec![LocalDecl {
                    count: 1,
                    value_type: ValueType::I32,
                }],
                body: vec![0x41, 7, 0x21, 0, 0x41, 8, 0x22, 0, 0x1a, 0x20, 0, 0x0b],
            }],
            ..Module::default()
        };
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function.body.iter().any(|instruction| matches!(
            instruction,
            tpt_wasm_semantics::Instruction::LocalSet(0)
        )));
        assert!(function.body.iter().any(|instruction| matches!(
            instruction,
            tpt_wasm_semantics::Instruction::LocalTee(0)
        )));
        assert!(function.body.iter().any(|instruction| matches!(
            instruction,
            tpt_wasm_semantics::Instruction::LocalGet(0)
        )));

        let mut configuration = Configuration {
            store: Store {
                functions: vec![function],
                ..Store::default()
            },
            ..Configuration::default()
        };
        configuration
            .push_frame(Frame {
                function_index: 0,
                locals: vec![Value::I32(0)],
                pc: 0,
                return_arity: 1,
                operand_base: 0,
                control_base: 0,
            })
            .unwrap();
        for _ in 0..6 {
            assert!(matches!(configuration.step().unwrap(), Transition::Step(_)));
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![Value::I32(8)])
        );
    }

    #[test]
    fn certified_select_projection_executes() {
        let module = module(
            vec![0x41, 1, 0x41, 10, 0x41, 20, 0x1b, 0x0b],
            Some(ValueType::I32),
        );
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::SelectI32));
    }

    #[test]
    fn certified_i32_bitwise_projection_executes() {
        let module = module(vec![0x41, 10, 0x41, 6, 0x73, 0x0b], Some(ValueType::I32));
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::XorI32));
    }

    #[test]
    fn certified_i32_comparison_projection_executes() {
        let module = module(
            vec![0x41, 0, 0x45, 0x1a, 0x41, 1, 0x41, 0x7f, 0x49, 0x0b],
            Some(ValueType::I32),
        );
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::EqzI32));
        assert!(function.body.iter().any(|instruction| {
            matches!(instruction, tpt_wasm_semantics::Instruction::CompareI32(_))
        }));
    }

    #[test]
    fn certified_i32_unary_projection_executes() {
        let module = module(vec![0x41, 1, 0x67, 0x0b], Some(ValueType::I32));
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::UnaryI32(
                tpt_wasm_semantics::UnaryOperation::Clz,
            )));
    }

    #[test]
    fn certified_i64_unary_projection_executes() {
        let module = module(vec![0x42, 1, 0x79, 0x0b], Some(ValueType::I64));
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::UnaryI64(
                tpt_wasm_semantics::UnaryOperation::Clz,
            )));
    }

    #[test]
    fn certified_i64_comparison_projection_executes() {
        let module = module(vec![0x42, 1, 0x42, 2, 0x51, 0x0b], Some(ValueType::I32));
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function.body.iter().any(|instruction| matches!(
            instruction,
            tpt_wasm_semantics::Instruction::CompareI64(_)
        )));
    }

    #[test]
    fn certified_i64_arithmetic_projection_executes() {
        let module = module(vec![0x42, 6, 0x42, 7, 0x7e, 0x0b], Some(ValueType::I64));
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::MulI64));
    }

    #[test]
    fn certified_i64_shift_projection_executes() {
        let module = module(vec![0x42, 16, 0x42, 2, 0x86, 0x0b], Some(ValueType::I64));
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::ShlI64));
    }

    #[test]
    fn certified_integer_rotation_projection_executes() {
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![
                    ValueType::I32,
                    ValueType::I32,
                    ValueType::I64,
                    ValueType::I64,
                ]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![
                    0x41, 1, 0x41, 0x21, 0x77, 0x41, 1, 0x41, 0x22, 0x78, 0x42, 1, 0x42, 0xc1,
                    0x00, 0x89, 0x42, 1, 0x42, 0xc2, 0x00, 0x8a, 0x0b,
                ],
            }],
            ..Module::default()
        };
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        for instruction in [
            tpt_wasm_semantics::Instruction::RotlI32,
            tpt_wasm_semantics::Instruction::RotrI32,
            tpt_wasm_semantics::Instruction::RotlI64,
            tpt_wasm_semantics::Instruction::RotrI64,
        ] {
            assert!(function.body.contains(&instruction));
        }
    }

    #[test]
    fn certified_i64_bitwise_projection_executes() {
        let module = module(vec![0x42, 10, 0x42, 6, 0x85, 0x0b], Some(ValueType::I64));
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::XorI64));
    }

    #[test]
    fn certified_float_binary_projection_executes() {
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::F32, ValueType::F64]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![
                    0x43, 0, 0, 0x20, 0x40, 0x43, 0, 0, 0x60, 0xc0, 0x96, 0x44, 0, 0, 0, 0, 0, 0,
                    0x03, 0xc0, 0x44, 0, 0, 0, 0, 0, 0, 0x60, 0xc0, 0xa4, 0x0b,
                ],
            }],
            ..Module::default()
        };
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::MinF32));
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::MinF64));
    }

    #[test]
    fn certified_float_unary_projection_executes() {
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::F32, ValueType::F64]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![
                    0x43, 0, 0, 0x20, 0x40, 0x91, 0x44, 0, 0, 0, 0, 0, 0, 0x04, 0x40, 0x9f, 0x0b,
                ],
            }],
            ..Module::default()
        };
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::UnaryF32(
                tpt_wasm_semantics::FloatUnaryOperation::Sqrt,
            )));
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::UnaryF64(
                tpt_wasm_semantics::FloatUnaryOperation::Sqrt,
            )));
    }

    #[test]
    fn certified_integer_division_and_remainder_projection_executes() {
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32, ValueType::I64]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![0x41, 42, 0x41, 6, 0x6d, 0x42, 50, 0x42, 3, 0x82, 0x0b],
            }],
            ..Module::default()
        };
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::DivSI32));
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::RemUI64));
    }

    #[test]
    fn certified_reinterpret_projection_executes() {
        for (body, result, operation) in [
            (
                vec![0x43, 0, 0, 0, 0, 0xbc, 0x0b],
                ValueType::I32,
                tpt_wasm_semantics::ReinterpretOperation::I32FromF32,
            ),
            (
                vec![0x44, 0, 0, 0, 0, 0, 0, 0, 0, 0xbd, 0x0b],
                ValueType::I64,
                tpt_wasm_semantics::ReinterpretOperation::I64FromF64,
            ),
            (
                vec![0x41, 0x7f, 0xbe, 0x0b],
                ValueType::F32,
                tpt_wasm_semantics::ReinterpretOperation::F32FromI32,
            ),
            (
                vec![0x42, 0x7f, 0xbf, 0x0b],
                ValueType::F64,
                tpt_wasm_semantics::ReinterpretOperation::F64FromI64,
            ),
        ] {
            let module = module(body, Some(result));
            let validated = Validator::new().validate(module).unwrap();
            let verified = lower_and_verify(&validated).unwrap();
            let function = project_function_to_model(&verified, 0).unwrap();
            assert!(function
                .body
                .contains(&tpt_wasm_semantics::Instruction::Reinterpret(operation)));
        }
    }

    #[test]
    fn certified_integer_conversions_project_and_execute() {
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32, ValueType::I64, ValueType::I64]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![0x42, 0x2a, 0xa7, 0x41, 0x7e, 0xac, 0x41, 0x7f, 0xad, 0x0b],
            }],
            ..Module::default()
        };
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        for operation in [
            tpt_wasm_semantics::IntegerConversion::I32WrapI64,
            tpt_wasm_semantics::IntegerConversion::I64ExtendI32S,
            tpt_wasm_semantics::IntegerConversion::I64ExtendI32U,
        ] {
            assert!(function
                .body
                .contains(&tpt_wasm_semantics::Instruction::IntConvert(operation)));
        }
    }

    #[test]
    fn certified_float_conversions_project_and_execute() {
        let mut body = Vec::new();
        for (opcode, source) in [(0xb2, 0x41), (0xb3, 0x41), (0xb4, 0x42), (0xb5, 0x42)] {
            body.push(source);
            body.push(0x7f);
            body.push(opcode);
        }
        body.push(0x44);
        body.extend(f64::NAN.to_bits().to_le_bytes());
        body.push(0xb6);
        for (opcode, source) in [(0xb7, 0x41), (0xb8, 0x41), (0xb9, 0x42), (0xba, 0x42)] {
            body.push(source);
            body.push(0x7f);
            body.push(opcode);
        }
        body.push(0x43);
        body.extend(f32::NAN.to_bits().to_le_bytes());
        body.push(0xbb);
        body.push(0x0b);
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(
                    (0..5)
                        .map(|_| ValueType::F32)
                        .chain((0..5).map(|_| ValueType::F64))
                        .collect(),
                ),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body,
            }],
            ..Module::default()
        };
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        for operation in [
            FloatConversion::F32FromI32S,
            FloatConversion::F32FromI32U,
            FloatConversion::F32FromI64S,
            FloatConversion::F32FromI64U,
            FloatConversion::F32FromF64,
            FloatConversion::F64FromI32S,
            FloatConversion::F64FromI32U,
            FloatConversion::F64FromI64S,
            FloatConversion::F64FromI64U,
            FloatConversion::F64FromF32,
        ] {
            assert!(function
                .body
                .contains(&tpt_wasm_semantics::Instruction::FloatConvert(operation,)));
        }
    }

    #[test]
    fn certified_float_truncation_projects_and_executes() {
        let mut body = Vec::new();
        for (opcode, source) in [(0xa8, 0x43), (0xa9, 0x43), (0xaa, 0x44), (0xab, 0x44)] {
            body.push(source);
            if source == 0x43 {
                body.extend(3.9f32.to_bits().to_le_bytes());
            } else {
                body.extend(3.9f64.to_bits().to_le_bytes());
            }
            body.push(opcode);
        }
        for (opcode, source) in [(0xae, 0x43), (0xaf, 0x43), (0xb0, 0x44), (0xb1, 0x44)] {
            body.push(source);
            if source == 0x43 {
                body.extend(3.9f32.to_bits().to_le_bytes());
            } else {
                body.extend(3.9f64.to_bits().to_le_bytes());
            }
            body.push(opcode);
        }
        body.push(0x0b);
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(
                    (0..4)
                        .map(|_| ValueType::I32)
                        .chain((0..4).map(|_| ValueType::I64))
                        .collect(),
                ),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body,
            }],
            ..Module::default()
        };
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        for operation in [
            FloatTrunc::I32FromF32S,
            FloatTrunc::I32FromF32U,
            FloatTrunc::I32FromF64S,
            FloatTrunc::I32FromF64U,
            FloatTrunc::I64FromF32S,
            FloatTrunc::I64FromF32U,
            FloatTrunc::I64FromF64S,
            FloatTrunc::I64FromF64U,
        ] {
            assert!(function
                .body
                .contains(&tpt_wasm_semantics::Instruction::FloatTrunc(operation)));
        }

        let mut configuration = Configuration {
            store: Store {
                functions: vec![function],
                ..Store::default()
            },
            ..Configuration::default()
        };
        configuration
            .push_frame(Frame {
                function_index: 0,
                locals: Vec::new(),
                pc: 0,
                return_arity: 8,
                operand_base: 0,
                control_base: 0,
            })
            .unwrap();
        for _ in 0..16 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::I32(3),
                Value::I32(3),
                Value::I32(3),
                Value::I32(3),
                Value::I64(3),
                Value::I64(3),
                Value::I64(3),
                Value::I64(3),
            ])
        );
    }

    #[test]
    fn certified_float_comparisons_project_and_execute() {
        let module = module(
            vec![
                0x43, 0, 0, 0x80, 0x3f, 0x43, 0, 0, 0, 0x40, 0x5d, 0x1a, 0x44, 0, 0, 0, 0, 0, 0,
                0xf0, 0x3f, 0x44, 0, 0, 0, 0, 0, 0, 0, 0x40, 0x63, 0x0b,
            ],
            Some(ValueType::I32),
        );
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function.body.iter().any(|instruction| matches!(
            instruction,
            tpt_wasm_semantics::Instruction::CompareF32(_)
        )));
        assert!(function.body.iter().any(|instruction| matches!(
            instruction,
            tpt_wasm_semantics::Instruction::CompareF64(_)
        )));
    }

    #[test]
    fn certified_float_arithmetic_projects_and_executes() {
        let mut body = Vec::new();
        body.push(0x43);
        body.extend(1.0f32.to_le_bytes());
        body.push(0x43);
        body.extend(2.0f32.to_le_bytes());
        body.extend([0x92, 0x44]);
        body.extend(1.0f64.to_le_bytes());
        body.push(0x44);
        body.extend(2.0f64.to_le_bytes());
        body.extend([0xa0, 0x0b]);
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::F32, ValueType::F64]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body,
            }],
            ..Module::default()
        };
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let function = project_function_to_model(&verified, 0).unwrap();
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::AddF32));
        assert!(function
            .body
            .contains(&tpt_wasm_semantics::Instruction::AddF64));

        let mut configuration = Configuration {
            store: Store {
                functions: vec![function],
                ..Store::default()
            },
            ..Configuration::default()
        };
        configuration
            .push_frame(Frame {
                function_index: 0,
                locals: Vec::new(),
                pc: 0,
                return_arity: 2,
                operand_base: 0,
                control_base: 0,
            })
            .unwrap();
        for _ in 0..6 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![
                Value::F32(3.0f32.to_bits()),
                Value::F64(3.0f64.to_bits()),
            ])
        );
    }

    #[test]
    fn certified_direct_call_projects_and_executes() {
        let callee_type = FunctionType {
            params: ResultType(vec![ValueType::I32, ValueType::I32]),
            results: ResultType(vec![ValueType::I32]),
        };
        let module = Module {
            types: vec![
                callee_type.clone(),
                FunctionType {
                    params: ResultType(Vec::new()),
                    results: ResultType(vec![ValueType::I32]),
                },
            ],
            functions: vec![
                Function {
                    type_index: 0,
                    locals: Vec::new(),
                    body: vec![0x20, 0, 0x20, 1, 0x6b, 0x0b],
                },
                Function {
                    type_index: 1,
                    locals: Vec::new(),
                    body: vec![0x41, 20, 0x41, 21, 0x10, 0, 0x0b],
                },
            ],
            ..Module::default()
        };
        let validated = Validator::new().validate(module).unwrap();
        let verified = lower_and_verify(&validated).unwrap();
        let callee = project_function_to_model(&verified, 0).unwrap();
        let caller = project_function_to_model(&verified, 1).unwrap();
        assert!(caller
            .body
            .contains(&tpt_wasm_semantics::Instruction::Call(0)));

        let mut configuration = Configuration {
            store: Store {
                functions: vec![callee, caller],
                ..Store::default()
            },
            ..Configuration::default()
        };
        configuration
            .push_frame(Frame {
                function_index: 1,
                locals: Vec::new(),
                pc: 0,
                return_arity: 1,
                operand_base: 0,
                control_base: 0,
            })
            .unwrap();
        for _ in 0..7 {
            configuration = match configuration.step().unwrap() {
                Transition::Step(next) => next,
                other => panic!("unexpected transition: {other:?}"),
            };
        }
        assert_eq!(
            configuration.step().unwrap(),
            Transition::Return(vec![Value::I32(-1)])
        );
    }
}
