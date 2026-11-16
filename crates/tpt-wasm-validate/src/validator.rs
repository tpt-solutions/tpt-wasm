// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Module-level WebAssembly validation.

use std::collections::HashSet;

use tpt_wasm_format::{ElementInit, ExportDesc, ImportDesc, Module};
use tpt_wasm_types::{FunctionType, GlobalType, Limits, RefType, TableType, ValueType};

use super::{body, ValidationError};

#[derive(Debug)]
pub(crate) struct ModuleContext {
    pub types: Vec<FunctionType>,
    pub function_types: Vec<FunctionType>,
    pub table_types: Vec<TableType>,
    pub memory_count: usize,
    pub global_types: Vec<GlobalType>,
    /// The data count section's value, if the module declares one.
    pub data_count: Option<u32>,
}

pub(crate) fn validate_module(module: &Module) -> Result<(), ValidationError> {
    if !module.tags.is_empty() {
        return Err(ValidationError::UnsupportedFeature("tags"));
    }
    validate_types(&module.types)?;
    let context = build_context(module)?;
    validate_exports(module, &context)?;
    validate_start(module, &context)?;
    validate_segments(module, &context)?;
    for function in &module.functions {
        let function_type = context
            .types
            .get(function.type_index as usize)
            .ok_or(ValidationError::UnknownType(function.type_index))?;
        body::validate_function(function, function_type, &context)?;
    }
    Ok(())
}

fn validate_types(types: &[FunctionType]) -> Result<(), ValidationError> {
    for function_type in types {
        for value in function_type
            .params
            .0
            .iter()
            .chain(function_type.results.0.iter())
        {
            if !value.is_supported() {
                return Err(ValidationError::UnsupportedFeature(
                    "unsupported value type",
                ));
            }
        }
    }
    Ok(())
}

fn build_context(module: &Module) -> Result<ModuleContext, ValidationError> {
    let mut function_types = Vec::new();
    let mut table_types = Vec::new();
    let mut memory_count = 0usize;
    let mut global_types = Vec::new();
    for import in &module.imports {
        match &import.desc {
            ImportDesc::Function(index) => {
                let function_type = module
                    .types
                    .get(*index as usize)
                    .ok_or(ValidationError::UnknownType(*index))?;
                function_types.push(function_type.clone());
            }
            ImportDesc::Table(table_type) => {
                validate_table_type(table_type)?;
                table_types.push(*table_type);
            }
            ImportDesc::Memory(memory_type) => {
                if memory_type.memory64 {
                    return Err(ValidationError::UnsupportedFeature("memory64"));
                }
                validate_limits(&memory_type.limits, 65536, "memory")?;
                memory_count += 1;
            }
            ImportDesc::Global(global_type) => {
                validate_global_type(*global_type)?;
                global_types.push(*global_type);
            }
            ImportDesc::Tag(_) => return Err(ValidationError::UnsupportedFeature("tags")),
        }
    }
    for function in &module.functions {
        let function_type = module
            .types
            .get(function.type_index as usize)
            .ok_or(ValidationError::UnknownType(function.type_index))?;
        function_types.push(function_type.clone());
    }
    for table in &module.tables {
        validate_table_type(&table.table_type)?;
        if table.init.is_some() {
            return Err(ValidationError::UnsupportedFeature("table initializers"));
        }
        table_types.push(table.table_type);
    }
    for memory in &module.memories {
        if memory.memory_type.memory64 {
            return Err(ValidationError::UnsupportedFeature("memory64"));
        }
        validate_limits(&memory.memory_type.limits, 65536, "memory")?;
        memory_count += 1;
    }
    for global in &module.globals {
        validate_global_type(global.global_type)?;
        global_types.push(global.global_type);
    }
    if memory_count > 1 {
        return Err(ValidationError::InvalidLimits(
            "MVP modules may contain at most one memory".into(),
        ));
    }
    Ok(ModuleContext {
        data_count: module.data_count,
        types: module.types.clone(),
        function_types,
        table_types,
        memory_count,
        global_types,
    })
}

fn validate_limits(limits: &Limits, maximum: u64, kind: &str) -> Result<(), ValidationError> {
    if limits.min > maximum {
        return Err(ValidationError::InvalidLimits(format!(
            "{kind} minimum exceeds the MVP limit"
        )));
    }
    if let Some(max) = limits.max {
        if max > maximum || max < limits.min {
            return Err(ValidationError::InvalidLimits(format!(
                "invalid {kind} maximum"
            )));
        }
    }
    Ok(())
}

fn validate_table_type(table_type: &TableType) -> Result<(), ValidationError> {
    validate_limits(&table_type.limits, u64::from(u32::MAX), "table")
}

fn validate_global_type(global_type: GlobalType) -> Result<(), ValidationError> {
    if !global_type.value_type.is_supported() {
        return Err(ValidationError::UnsupportedFeature(
            "unsupported global type",
        ));
    }
    Ok(())
}

fn validate_exports(module: &Module, context: &ModuleContext) -> Result<(), ValidationError> {
    let mut names = HashSet::new();
    for export in &module.exports {
        if !names.insert(export.name.as_str()) {
            return Err(ValidationError::DuplicateExport(export.name.clone()));
        }
        match export.desc {
            ExportDesc::Function(index) => {
                if index as usize >= context.function_types.len() {
                    return Err(ValidationError::UnknownFunction(index));
                }
            }
            ExportDesc::Table(index) => {
                if index as usize >= context.table_types.len() {
                    return Err(ValidationError::UnknownTable(index));
                }
            }
            ExportDesc::Memory(index) => {
                if index != 0 || context.memory_count == 0 {
                    return Err(ValidationError::UnknownMemory(index));
                }
            }
            ExportDesc::Global(index) => {
                if index as usize >= context.global_types.len() {
                    return Err(ValidationError::UnknownGlobal(index));
                }
            }
            ExportDesc::Tag(_) => return Err(ValidationError::UnsupportedFeature("tags")),
        }
    }
    Ok(())
}

fn validate_start(module: &Module, context: &ModuleContext) -> Result<(), ValidationError> {
    let Some(index) = module.start else {
        return Ok(());
    };
    let function_type = context
        .function_types
        .get(index as usize)
        .ok_or(ValidationError::UnknownFunction(index))?;
    if !function_type.params.0.is_empty() || !function_type.results.0.is_empty() {
        return Err(ValidationError::InvalidStartFunction);
    }
    Ok(())
}

fn validate_segments(module: &Module, context: &ModuleContext) -> Result<(), ValidationError> {
    // A declared data count must be the number of data segments, since the count is what an instruction
    // that names a segment is checked against.
    if let Some(declared) = module.data_count {
        if declared as usize != module.data.len() {
            return Err(ValidationError::DataCountMismatch {
                declared,
                found: module.data.len(),
            });
        }
    }
    for global in &module.globals {
        validate_const_expr(&global.init, global.global_type.value_type, context, true)?;
    }
    for element in &module.elements {
        // A plain index list spells only function references, so it cannot initialize an
        // `externref` segment; that segment's entries are constant expressions.
        if element.element_type == RefType::ExternRef
            && matches!(element.init, ElementInit::FuncIndices(_))
        {
            return Err(ValidationError::TypeMismatch {
                expected: "externref entries".into(),
                actual: "function indices".into(),
            });
        }
        let ElementModeDetails {
            table_index,
            offset,
        } = match &element.mode {
            tpt_wasm_format::ElementMode::Active {
                table_index,
                offset,
            } => ElementModeDetails {
                table_index: Some(*table_index),
                offset: Some(offset),
            },
            // A passive or declarative segment writes no table at instantiation
            // and so has no table or offset to check. It is still validated: its
            // entries must be well-formed references. Executing one is a
            // separate matter, and the runtime reports that it is not yet
            // supported rather than the module being treated as invalid.
            tpt_wasm_format::ElementMode::Passive | tpt_wasm_format::ElementMode::Declarative => {
                ElementModeDetails {
                    table_index: None,
                    offset: None,
                }
            }
        };
        if let Some(table_index) = table_index {
            let table_type = context
                .table_types
                .get(table_index as usize)
                .ok_or(ValidationError::UnknownTable(table_index))?;
            if table_type.element_type != element.element_type {
                return Err(ValidationError::TypeMismatch {
                    expected: format!("{:?} table", table_type.element_type),
                    actual: format!("{:?} segment", element.element_type),
                });
            }
        }
        if let Some(offset) = offset {
            validate_const_expr(offset, ValueType::I32, context, true)?;
        }
        // The two families are validated differently. A plain index is checked
        // against the function index space; an expression is a constant
        // expression that must produce a reference of the segment's element type.
        match &element.init {
            ElementInit::FuncIndices(indices) => {
                for function in indices {
                    if *function as usize >= context.function_types.len() {
                        return Err(ValidationError::UnknownFunction(*function));
                    }
                }
            }
            ElementInit::Expressions(expressions) => {
                for expression in expressions {
                    validate_const_expr(
                        expression,
                        ValueType::Ref(element.element_type),
                        context,
                        true,
                    )?;
                }
            }
        }
    }
    for segment in &module.data {
        // A passive segment has no offset or memory to check, and writes nothing at instantiation.
        let tpt_wasm_format::DataMode::Active {
            memory_index,
            offset,
        } = &segment.mode
        else {
            continue;
        };
        if *memory_index != 0 || context.memory_count == 0 {
            return Err(ValidationError::UnknownMemory(*memory_index));
        }
        validate_const_expr(offset, ValueType::I32, context, true)?;
    }
    Ok(())
}

struct ElementModeDetails<'a> {
    table_index: Option<u32>,
    offset: Option<&'a tpt_wasm_format::ConstExpr>,
}

fn validate_const_expr(
    expr: &tpt_wasm_format::ConstExpr,
    expected: ValueType,
    context: &ModuleContext,
    allow_global_get: bool,
) -> Result<(), ValidationError> {
    let mut reader = body::Reader::new(&expr.0);
    let mut stack: Vec<ValueType> = Vec::new();
    loop {
        match reader.byte()? {
            0x0b => break,
            0x6a..=0x6c => integer_operation(&mut stack, ValueType::I32)?,
            0x7c..=0x7e => integer_operation(&mut stack, ValueType::I64)?,
            opcode => stack.push(const_leaf(opcode, &mut reader, context, allow_global_get)?),
        }
    }
    if reader.remaining() != 0 {
        return Err(ValidationError::InvalidConstantExpression);
    }
    match stack.as_slice() {
        [actual] if *actual == expected => Ok(()),
        [actual] => Err(ValidationError::TypeMismatch {
            expected: format!("{expected:?}"),
            actual: format!("{actual:?}"),
        }),
        _ => Err(ValidationError::InvalidConstantExpression),
    }
}

/// An `add`, `sub` or `mul` of two values of `ty`, which leaves one value of `ty`.
fn integer_operation(stack: &mut Vec<ValueType>, ty: ValueType) -> Result<(), ValidationError> {
    for _ in 0..2 {
        if stack.pop() != Some(ty) {
            return Err(ValidationError::InvalidConstantExpression);
        }
    }
    stack.push(ty);
    Ok(())
}

/// One constant or global read, and the type it produces.
fn const_leaf(
    opcode: u8,
    reader: &mut body::Reader<'_>,
    context: &ModuleContext,
    allow_global_get: bool,
) -> Result<ValueType, ValidationError> {
    Ok(match opcode {
        0x41 => {
            let _ = reader.i32()?;
            ValueType::I32
        }
        0x42 => {
            let _ = reader.i64()?;
            ValueType::I64
        }
        0x43 => {
            let _ = reader.f32()?;
            ValueType::F32
        }
        0x44 => {
            let _ = reader.f64()?;
            ValueType::F64
        }
        0x23 if allow_global_get => {
            let index = reader.u32()?;
            let global = context
                .global_types
                .get(index as usize)
                .ok_or(ValidationError::UnknownGlobal(index))?;
            // A constant expression may read any immutable global, imported or defined, as the
            // pinned spec revision has it. A mutable one is not a constant.
            if global.mutable {
                return Err(ValidationError::InvalidConstantExpression);
            }
            global.value_type
        }
        // The two reference-producing forms. `ref.null` carries the reference type it produces, and
        // `ref.func` names a function, which must exist in the index space.
        0xd0 => {
            let actual = match reader.byte()? {
                0x70 => RefType::FuncRef,
                0x6f => RefType::ExternRef,
                _ => return Err(ValidationError::InvalidConstantExpression),
            };
            ValueType::Ref(actual)
        }
        0xd2 => {
            let index = reader.u32()?;
            if index as usize >= context.function_types.len() {
                return Err(ValidationError::UnknownFunction(index));
            }
            ValueType::Ref(RefType::FuncRef)
        }
        _ => return Err(ValidationError::InvalidConstantExpression),
    })
}
