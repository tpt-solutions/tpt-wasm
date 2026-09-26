// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Module-level WebAssembly validation.

use std::collections::HashSet;

use tpt_wasm_format::{ExportDesc, ImportDesc, Module};
use tpt_wasm_types::{FunctionType, GlobalType, Limits, RefType, TableType, ValueType};

use super::{body, ValidationError};

#[derive(Debug)]
pub(crate) struct ModuleContext {
    pub types: Vec<FunctionType>,
    pub function_types: Vec<FunctionType>,
    pub table_types: Vec<TableType>,
    pub memory_count: usize,
    pub global_types: Vec<GlobalType>,
    pub imported_globals: usize,
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
    let mut imported_globals = 0usize;
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
                imported_globals += 1;
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
    if table_types.len() > 1 {
        return Err(ValidationError::InvalidLimits(
            "MVP modules may contain at most one table".into(),
        ));
    }
    Ok(ModuleContext {
        types: module.types.clone(),
        function_types,
        table_types,
        memory_count,
        global_types,
        imported_globals,
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
    if table_type.element_type != RefType::FuncRef {
        return Err(ValidationError::UnsupportedFeature("non-funcref table"));
    }
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
    for global in &module.globals {
        validate_const_expr(&global.init, global.global_type.value_type, context, true)?;
    }
    for element in &module.elements {
        if element.element_type != RefType::FuncRef {
            return Err(ValidationError::UnsupportedFeature("non-funcref element"));
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
            tpt_wasm_format::ElementMode::Passive | tpt_wasm_format::ElementMode::Declarative => {
                return Err(ValidationError::UnsupportedFeature(
                    "non-active element segments",
                ))
            }
        };
        if let Some(table_index) = table_index {
            if table_index != 0 {
                return Err(ValidationError::UnsupportedFeature("multiple tables"));
            }
            if table_index as usize >= context.table_types.len() {
                return Err(ValidationError::UnknownTable(table_index));
            }
        }
        if let Some(offset) = offset {
            validate_const_expr(offset, ValueType::I32, context, false)?;
        }
        for function in &element.init {
            if *function as usize >= context.function_types.len() {
                return Err(ValidationError::UnknownFunction(*function));
            }
        }
    }
    for segment in &module.data {
        let tpt_wasm_format::DataMode::Active {
            memory_index,
            offset,
        } = &segment.mode
        else {
            return Err(ValidationError::UnsupportedFeature("passive data segments"));
        };
        if *memory_index != 0 || context.memory_count == 0 {
            return Err(ValidationError::UnknownMemory(*memory_index));
        }
        validate_const_expr(offset, ValueType::I32, context, false)?;
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
    let actual = match reader.byte()? {
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
            if index as usize >= context.imported_globals || global.mutable {
                return Err(ValidationError::InvalidConstantExpression);
            }
            global.value_type
        }
        _ => return Err(ValidationError::InvalidConstantExpression),
    };
    if reader.byte()? != 0x0b || reader.remaining() != 0 {
        return Err(ValidationError::InvalidConstantExpression);
    }
    if actual != expected {
        return Err(ValidationError::TypeMismatch {
            expected: format!("{expected:?}"),
            actual: format!("{actual:?}"),
        });
    }
    Ok(())
}
