// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Canonical WebAssembly MVP binary encoder.

use tpt_wasm_format::{
    ConstExpr, CustomSection, DataMode, DataSegment, Element, ElementMode, Export, ExportDesc,
    Function, Global, Import, ImportDesc, Memory, Module, Table,
};
use tpt_wasm_types::{FunctionType, GlobalType, Limits, MemoryType, RefType, TableType, ValueType};

use super::EncodeError;

fn write_u32(value: u32, output: &mut Vec<u8>) {
    let mut value = value;
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn write_name(name: &str, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let bytes = name.as_bytes();
    write_u32(
        u32::try_from(bytes.len()).map_err(|_| EncodeError::ValueTooLarge)?,
        output,
    );
    output.extend_from_slice(bytes);
    Ok(())
}

fn write_vector<T>(
    values: &[T],
    output: &mut Vec<u8>,
    mut write: impl FnMut(&T, &mut Vec<u8>) -> Result<(), EncodeError>,
) -> Result<(), EncodeError> {
    write_u32(
        u32::try_from(values.len()).map_err(|_| EncodeError::ValueTooLarge)?,
        output,
    );
    for value in values {
        write(value, output)?;
    }
    Ok(())
}

fn write_value_type(value: ValueType, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let byte = match value {
        ValueType::I32 => 0x7f,
        ValueType::I64 => 0x7e,
        ValueType::F32 => 0x7d,
        ValueType::F64 => 0x7c,
        ValueType::V128 | ValueType::Ref(_) => return Err(EncodeError::UnsupportedValueType),
    };
    output.push(byte);
    Ok(())
}

fn write_limits(limits: Limits, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let min = u32::try_from(limits.min).map_err(|_| EncodeError::ValueTooLarge)?;
    let max = limits
        .max
        .map(u32::try_from)
        .transpose()
        .map_err(|_| EncodeError::ValueTooLarge)?;
    match max {
        Some(max) => {
            output.push(1);
            write_u32(min, output);
            write_u32(max, output);
        }
        None => {
            output.push(0);
            write_u32(min, output);
        }
    }
    Ok(())
}

fn write_table_type(table_type: TableType, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    match table_type.element_type {
        RefType::FuncRef => output.push(0x70),
        RefType::ExternRef => return Err(EncodeError::UnsupportedReferenceType),
    }
    write_limits(table_type.limits, output)
}

fn write_global_type(global_type: GlobalType, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    write_value_type(global_type.value_type, output)?;
    output.push(u8::from(global_type.mutable));
    Ok(())
}

fn write_function_type(
    function_type: &FunctionType,
    output: &mut Vec<u8>,
) -> Result<(), EncodeError> {
    output.push(0x60);
    write_vector(&function_type.params.0, output, |value, out| {
        write_value_type(*value, out)
    })?;
    write_vector(&function_type.results.0, output, |value, out| {
        write_value_type(*value, out)
    })
}

fn write_const_expr(expr: &ConstExpr, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    if expr.0.is_empty() || *expr.0.last().unwrap() != 0x0b {
        return Err(EncodeError::InvalidConstExpr);
    }
    output.extend_from_slice(&expr.0);
    Ok(())
}

fn write_import(import: &Import, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    write_name(&import.module, output)?;
    write_name(&import.name, output)?;
    match &import.desc {
        ImportDesc::Function(index) => {
            output.push(0x00);
            write_u32(*index, output);
        }
        ImportDesc::Table(table_type) => {
            output.push(0x01);
            write_table_type(*table_type, output)?;
        }
        ImportDesc::Memory(memory_type) => {
            output.push(0x02);
            write_memory_type(*memory_type, output)?;
        }
        ImportDesc::Global(global_type) => {
            output.push(0x03);
            write_global_type(*global_type, output)?;
        }
        ImportDesc::Tag(_) => return Err(EncodeError::UnsupportedFeature("tag imports")),
    }
    Ok(())
}

fn write_memory_type(memory_type: MemoryType, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    if memory_type.memory64 {
        return Err(EncodeError::UnsupportedFeature("memory64"));
    }
    write_limits(memory_type.limits, output)
}

fn write_export(export: &Export, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    write_name(&export.name, output)?;
    match export.desc {
        ExportDesc::Function(index) => {
            output.push(0x00);
            write_u32(index, output);
        }
        ExportDesc::Table(index) => {
            output.push(0x01);
            write_u32(index, output);
        }
        ExportDesc::Memory(index) => {
            output.push(0x02);
            write_u32(index, output);
        }
        ExportDesc::Global(index) => {
            output.push(0x03);
            write_u32(index, output);
        }
        ExportDesc::Tag(_) => return Err(EncodeError::UnsupportedFeature("tag exports")),
    }
    Ok(())
}

fn write_table(table: &Table, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    if table.init.is_some() {
        return Err(EncodeError::UnsupportedFeature("table initializers"));
    }
    write_table_type(table.table_type, output)
}

fn write_memory(memory: &Memory, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    write_memory_type(memory.memory_type, output)
}

fn write_global(global: &Global, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    write_global_type(global.global_type, output)?;
    write_const_expr(&global.init, output)
}

fn write_function(function: &Function, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut body = Vec::new();
    write_vector(&function.locals, &mut body, |local, out| {
        write_u32(local.count, out);
        write_value_type(local.value_type, out)
    })?;
    body.extend_from_slice(&function.body);
    write_u32(
        u32::try_from(body.len()).map_err(|_| EncodeError::ValueTooLarge)?,
        output,
    );
    output.extend_from_slice(&body);
    Ok(())
}

fn write_element(element: &Element, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    if element.element_type != RefType::FuncRef {
        return Err(EncodeError::UnsupportedReferenceType);
    }
    let (table_index, offset) = match &element.mode {
        ElementMode::Active {
            table_index,
            offset,
        } => (*table_index, offset),
        ElementMode::Passive | ElementMode::Declarative => {
            return Err(EncodeError::UnsupportedFeature("passive element segments"))
        }
    };
    if table_index != 0 {
        return Err(EncodeError::UnsupportedFeature("multiple tables"));
    }
    output.push(0x00);
    write_const_expr(offset, output)?;
    write_vector(&element.init, output, |index, out| {
        write_u32(*index, out);
        Ok(())
    })
}

fn write_data(data: &DataSegment, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let (memory_index, offset) = match &data.mode {
        DataMode::Active {
            memory_index,
            offset,
        } => (*memory_index, offset),
        DataMode::Passive => return Err(EncodeError::UnsupportedFeature("passive data segments")),
    };
    if memory_index != 0 {
        return Err(EncodeError::UnsupportedFeature("multiple memories"));
    }
    output.push(0x00);
    write_const_expr(offset, output)?;
    write_vector(&data.data, output, |bytes, out| {
        out.push(*bytes);
        Ok(())
    })
}

fn write_section(id: u8, payload: Vec<u8>, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    output.push(id);
    write_u32(
        u32::try_from(payload.len()).map_err(|_| EncodeError::ValueTooLarge)?,
        output,
    );
    output.extend_from_slice(&payload);
    Ok(())
}

fn write_custom(custom: &CustomSection, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut payload = Vec::new();
    write_name(&custom.name, &mut payload)?;
    payload.extend_from_slice(&custom.data);
    write_section(0, payload, output)
}

fn write_types(types: &[FunctionType], output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut payload = Vec::new();
    write_vector(types, &mut payload, |function_type, out| {
        write_function_type(function_type, out)
    })?;
    write_section(1, payload, output)
}

fn write_imports(imports: &[Import], output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut payload = Vec::new();
    write_vector(imports, &mut payload, write_import)?;
    write_section(2, payload, output)
}

fn write_functions(functions: &[Function], output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut payload = Vec::new();
    write_vector(functions, &mut payload, |function, out| {
        write_u32(function.type_index, out);
        Ok(())
    })?;
    write_section(3, payload, output)
}

fn write_tables(tables: &[Table], output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut payload = Vec::new();
    write_vector(tables, &mut payload, write_table)?;
    write_section(4, payload, output)
}

fn write_memories(memories: &[Memory], output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut payload = Vec::new();
    write_vector(memories, &mut payload, write_memory)?;
    write_section(5, payload, output)
}

fn write_globals(globals: &[Global], output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut payload = Vec::new();
    write_vector(globals, &mut payload, write_global)?;
    write_section(6, payload, output)
}

fn write_exports(exports: &[Export], output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut payload = Vec::new();
    write_vector(exports, &mut payload, write_export)?;
    write_section(7, payload, output)
}

fn write_start(index: u32, output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut payload = Vec::new();
    write_u32(index, &mut payload);
    write_section(8, payload, output)
}

fn write_elements(elements: &[Element], output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut payload = Vec::new();
    write_vector(elements, &mut payload, write_element)?;
    write_section(9, payload, output)
}

fn write_code(functions: &[Function], output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut payload = Vec::new();
    write_vector(functions, &mut payload, write_function)?;
    write_section(10, payload, output)
}

fn write_data_segments(data: &[DataSegment], output: &mut Vec<u8>) -> Result<(), EncodeError> {
    let mut payload = Vec::new();
    write_vector(data, &mut payload, write_data)?;
    write_section(11, payload, output)
}

/// Encode a structural module as a canonical WebAssembly MVP binary.
///
/// The encoder preserves custom-section payloads but emits known sections in
/// the standard order. It does not validate indices or instruction types.
pub fn encode(module: &Module) -> Result<Vec<u8>, EncodeError> {
    if !module.tags.is_empty() {
        return Err(EncodeError::UnsupportedFeature("tags"));
    }
    let mut output = Vec::from(*b"\0asm");
    output.extend_from_slice(&[1, 0, 0, 0]);

    if !module.types.is_empty() {
        write_types(&module.types, &mut output)?;
    }
    if !module.imports.is_empty() {
        write_imports(&module.imports, &mut output)?;
    }
    if !module.functions.is_empty() {
        write_functions(&module.functions, &mut output)?;
    }
    if !module.tables.is_empty() {
        write_tables(&module.tables, &mut output)?;
    }
    if !module.memories.is_empty() {
        write_memories(&module.memories, &mut output)?;
    }
    if !module.globals.is_empty() {
        write_globals(&module.globals, &mut output)?;
    }
    if !module.exports.is_empty() {
        write_exports(&module.exports, &mut output)?;
    }
    if let Some(start) = module.start {
        write_start(start, &mut output)?;
    }
    if !module.elements.is_empty() {
        write_elements(&module.elements, &mut output)?;
    }
    if !module.functions.is_empty() {
        write_code(&module.functions, &mut output)?;
    }
    if !module.data.is_empty() {
        write_data_segments(&module.data, &mut output)?;
    }
    for custom in &module.custom_sections {
        write_custom(custom, &mut output)?;
    }
    Ok(output)
}
