// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Tests for the structural WebAssembly MVP binary codec.

use tpt_wasm_format::{
    ConstExpr, CustomSection, DataMode, DataSegment, Element, ElementMode, Export, ExportDesc,
    Function, Global, Import, ImportDesc, Memory, Module, Table,
};
use tpt_wasm_types::{
    FunctionType, GlobalType, Limits, MemoryType, RefType, ResultType, TableType, ValueType,
};

use super::{decode, encode, DecodeError};

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

fn sample_module() -> Module {
    Module {
        types: vec![function_type(Vec::new(), vec![ValueType::I32])],
        imports: vec![Import {
            module: "env".into(),
            name: "answer".into(),
            desc: ImportDesc::Function(0),
        }],
        functions: vec![Function {
            type_index: 0,
            locals: vec![],
            body: vec![0x41, 0x2a, 0x0b],
        }],
        tables: vec![Table {
            table_type: TableType {
                element_type: RefType::FuncRef,
                limits: Limits {
                    min: 1,
                    max: Some(2),
                },
            },
            init: None,
        }],
        memories: vec![Memory {
            memory_type: MemoryType {
                limits: Limits { min: 1, max: None },
                memory64: false,
            },
        }],
        globals: vec![Global {
            global_type: GlobalType {
                value_type: ValueType::I32,
                mutable: false,
            },
            init: const_i32(7),
        }],
        exports: vec![Export {
            name: "run".into(),
            desc: ExportDesc::Function(1),
        }],
        start: None,
        elements: vec![Element {
            element_type: RefType::FuncRef,
            mode: ElementMode::Active {
                table_index: 0,
                offset: const_i32(0),
            },
            init: vec![1],
        }],
        data: vec![DataSegment {
            mode: DataMode::Active {
                memory_index: 0,
                offset: const_i32(0),
            },
            data: vec![1, 2, 3],
        }],
        tags: vec![],
        custom_sections: vec![CustomSection {
            name: "producers".into(),
            data: vec![1, 2, 3],
        }],
    }
}

#[test]
fn negative_constants_round_trip() {
    let mut module = sample_module();
    module.globals[0].init = const_i32(-1);
    module.functions[0].body = vec![0x41, 0x7f, 0x0b];
    let bytes = encode(&module).unwrap();
    assert_eq!(decode(&bytes).unwrap(), module);
}

#[test]
fn arbitrary_and_mutated_inputs_do_not_panic() {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for length in 0..=256usize {
        for _ in 0..8 {
            let mut input = Vec::with_capacity(length);
            for _ in 0..length {
                state ^= state << 7;
                state ^= state >> 9;
                state ^= state << 8;
                input.push(state as u8);
            }
            let _ = decode(&input);
        }
    }

    let valid = encode(&sample_module()).unwrap();
    for index in 0..valid.len() {
        for replacement in [0x00, 0x7f, 0x80, 0xff] {
            let mut mutated = valid.clone();
            mutated[index] = replacement;
            let _ = decode(&mutated);
        }
    }
    for length in 0..valid.len() {
        let _ = decode(&valid[..length]);
    }
}

#[test]
fn empty_module_round_trips() {
    let bytes = encode(&Module::default()).unwrap();
    assert_eq!(bytes, b"\0asm\x01\0\0\0");
    assert_eq!(decode(&bytes).unwrap(), Module::default());
}

#[test]
fn representative_module_round_trips() {
    let module = sample_module();
    let bytes = encode(&module).unwrap();
    assert_eq!(decode(&bytes).unwrap(), module);
    assert_eq!(encode(&decode(&bytes).unwrap()).unwrap(), bytes);
}

#[test]
fn custom_sections_are_accepted_between_known_sections() {
    let mut bytes = Vec::from(*b"\0asm\x01\0\0\0");
    bytes.extend_from_slice(&[0, 1, 0]);
    bytes.extend_from_slice(&[1, 1, 0]);
    assert_eq!(decode(&bytes).unwrap().custom_sections.len(), 1);
}

#[test]
fn decoder_rejects_bad_header_and_section_order() {
    assert_eq!(decode(b"wasm"), Err(DecodeError::InvalidMagic));
    assert_eq!(decode(b"\0asm\x02\0\0\0"), Err(DecodeError::InvalidVersion));

    let mut duplicate = Vec::from(*b"\0asm\x01\0\0\0");
    duplicate.extend_from_slice(&[1, 1, 0, 1, 1, 0]);
    assert_eq!(decode(&duplicate), Err(DecodeError::DuplicateSection(1)));
}

#[test]
fn decoder_rejects_invalid_utf8_and_overlong_leb() {
    let mut invalid_utf8 = Vec::from(*b"\0asm\x01\0\0\0");
    invalid_utf8.extend_from_slice(&[0, 2, 1, 0xff]);
    assert_eq!(decode(&invalid_utf8), Err(DecodeError::InvalidUtf8));

    let mut overlong = Vec::from(*b"\0asm\x01\0\0\0");
    overlong.extend_from_slice(&[1, 7, 1, 0x60, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00]);
    assert_eq!(decode(&overlong), Err(DecodeError::InvalidLeb128));
}

#[test]
fn decoder_rejects_function_and_code_count_mismatch() {
    let mut bytes = Vec::from(*b"\0asm\x01\0\0\0");
    bytes.extend_from_slice(&[3, 2, 1, 0]);
    assert_eq!(decode(&bytes), Err(DecodeError::MismatchedCodeSection));
}
