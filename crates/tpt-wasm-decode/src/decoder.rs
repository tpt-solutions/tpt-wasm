// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! WebAssembly binary format decoder.
//!
//! This module is intentionally structural: it checks the binary grammar and
//! framing, but it does not resolve indices or check instruction types.

use tpt_wasm_format::{
    ConstExpr, CustomSection, DataMode, DataSegment, Element, ElementInit, ElementMode, Export,
    ExportDesc, Function, Global, Import, ImportDesc, LocalDecl, Memory, Module, Table,
};
use tpt_wasm_types::{FunctionType, GlobalType, Limits, MemoryType, RefType, TableType, ValueType};

use super::DecodeError;

const MAGIC: &[u8; 4] = b"\0asm";
const VERSION: &[u8; 4] = &[1, 0, 0, 0];

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn finish(&self) -> Result<(), DecodeError> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            Err(DecodeError::UnexpectedData)
        }
    }

    fn byte(&mut self) -> Result<u8, DecodeError> {
        let byte = *self
            .bytes
            .get(self.position)
            .ok_or(DecodeError::UnexpectedEof)?;
        self.position += 1;
        Ok(byte)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(DecodeError::SectionTooLarge)?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(DecodeError::UnexpectedEof)?;
        self.position = end;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = self.byte()?;
            let payload = u32::from(byte & 0x7f);
            if shift == 28 && payload > 0x0f {
                return Err(DecodeError::InvalidLeb128);
            }
            value |= payload << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(DecodeError::InvalidLeb128)
    }

    fn i32(&mut self) -> Result<i32, DecodeError> {
        let mut result = 0i64;
        let mut shift = 0;
        for index in 0..5 {
            let byte = self.byte()?;
            result |= i64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if index == 4 {
                    let sign = (result >> 31) & 1;
                    let extension = if sign == 0 { 0 } else { -1 };
                    if ((result >> 32) & 0x7) != (extension & 0x7) {
                        return Err(DecodeError::InvalidLeb128);
                    }
                } else if shift < 32 && byte & 0x40 != 0 {
                    result |= -1i64 << shift;
                }
                return Ok(result as i32);
            }
        }
        Err(DecodeError::InvalidLeb128)
    }

    fn i64(&mut self) -> Result<i64, DecodeError> {
        let mut result = 0i128;
        let mut shift = 0;
        for index in 0..10 {
            let byte = self.byte()?;
            result |= i128::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if index == 9 {
                    let sign = i128::from((byte & 0x01 != 0) as u8);
                    let extension = if sign == 0 { 0 } else { -1 };
                    if ((result >> 64) & 0x3f) != (extension & 0x3f) {
                        return Err(DecodeError::InvalidLeb128);
                    }
                } else if shift < 64 && byte & 0x40 != 0 {
                    result |= -1i128 << shift;
                }
                return Ok(result as i64);
            }
        }
        Err(DecodeError::InvalidLeb128)
    }

    fn f32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| DecodeError::UnexpectedEof)?,
        ))
    }

    fn f64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| DecodeError::UnexpectedEof)?,
        ))
    }

    fn name(&mut self) -> Result<String, DecodeError> {
        let length = self.u32()? as usize;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| DecodeError::InvalidUtf8)
    }

    fn vector<T>(
        &mut self,
        mut read: impl FnMut(&mut Self) -> Result<T, DecodeError>,
    ) -> Result<Vec<T>, DecodeError> {
        let length = self.u32()? as usize;
        let mut values = Vec::new();
        for _ in 0..length {
            values.push(read(self)?);
        }
        Ok(values)
    }
}

fn value_type(reader: &mut Reader<'_>) -> Result<ValueType, DecodeError> {
    match reader.byte()? {
        0x7f => Ok(ValueType::I32),
        0x7e => Ok(ValueType::I64),
        0x7d => Ok(ValueType::F32),
        0x7c => Ok(ValueType::F64),
        byte => Err(DecodeError::UnsupportedValueType(byte)),
    }
}

fn reference_type(reader: &mut Reader<'_>) -> Result<RefType, DecodeError> {
    match reader.byte()? {
        0x70 => Ok(RefType::FuncRef),
        byte => Err(DecodeError::UnsupportedReferenceType(byte)),
    }
}

fn limits(reader: &mut Reader<'_>) -> Result<Limits, DecodeError> {
    let flags = reader.byte()?;
    let min = u64::from(reader.u32()?);
    let max = match flags {
        0x00 => None,
        0x01 => Some(u64::from(reader.u32()?)),
        _ => return Err(DecodeError::UnsupportedLimitFlags(flags)),
    };
    Ok(Limits { min, max })
}

fn table_type(reader: &mut Reader<'_>) -> Result<TableType, DecodeError> {
    Ok(TableType {
        element_type: reference_type(reader)?,
        limits: limits(reader)?,
    })
}

fn global_type(reader: &mut Reader<'_>) -> Result<GlobalType, DecodeError> {
    let value_type = value_type(reader)?;
    let mutable = match reader.byte()? {
        0x00 => false,
        0x01 => true,
        byte => return Err(DecodeError::UnsupportedMutability(byte)),
    };
    Ok(GlobalType {
        value_type,
        mutable,
    })
}

fn function_type(reader: &mut Reader<'_>) -> Result<FunctionType, DecodeError> {
    if reader.byte()? != 0x60 {
        return Err(DecodeError::InvalidTypeForm);
    }
    Ok(FunctionType {
        params: tpt_wasm_types::ResultType(reader.vector(value_type)?),
        results: tpt_wasm_types::ResultType(reader.vector(value_type)?),
    })
}

fn const_expr(reader: &mut Reader<'_>) -> Result<ConstExpr, DecodeError> {
    let start = reader.position;
    loop {
        match reader.byte()? {
            0x0b => return Ok(ConstExpr(reader.bytes[start..reader.position].to_vec())),
            0x41 => {
                let _ = reader.i32()?;
            }
            0x42 => {
                let _ = reader.i64()?;
            }
            0x43 => {
                let _ = reader.f32()?;
            }
            0x44 => {
                let _ = reader.f64()?;
            }
            0x23 => {
                let _ = reader.u32()?;
            }
            // The two reference-producing forms. An element segment whose entries
            // are expressions uses these, and a `ref.null` entry in particular is
            // not an index at all, which is why these segments have their own
            // representation rather than reusing a vector of function indices.
            0xd0 => {
                let _ = reference_type(reader)?;
            }
            0xd2 => {
                let _ = reader.u32()?;
            }
            opcode => return Err(DecodeError::InvalidOpcode(u32::from(opcode))),
        }
    }
}

fn decode_type_section(reader: &mut Reader<'_>) -> Result<Vec<FunctionType>, DecodeError> {
    reader.vector(function_type)
}

fn decode_import_section(reader: &mut Reader<'_>) -> Result<Vec<Import>, DecodeError> {
    reader.vector(|reader| {
        let module = reader.name()?;
        let name = reader.name()?;
        let desc = match reader.byte()? {
            0x00 => ImportDesc::Function(reader.u32()?),
            0x01 => ImportDesc::Table(table_type(reader)?),
            0x02 => ImportDesc::Memory(MemoryType {
                limits: limits(reader)?,
                memory64: false,
            }),
            0x03 => ImportDesc::Global(global_type(reader)?),
            byte => return Err(DecodeError::InvalidImportKind(byte)),
        };
        Ok(Import { module, name, desc })
    })
}

fn decode_function_section(reader: &mut Reader<'_>) -> Result<Vec<u32>, DecodeError> {
    reader.vector(Reader::u32)
}

fn decode_table_section(reader: &mut Reader<'_>) -> Result<Vec<Table>, DecodeError> {
    reader.vector(|reader| {
        Ok(Table {
            table_type: table_type(reader)?,
            init: None,
        })
    })
}

fn decode_memory_section(reader: &mut Reader<'_>) -> Result<Vec<Memory>, DecodeError> {
    reader.vector(|reader| {
        Ok(Memory {
            memory_type: MemoryType {
                limits: limits(reader)?,
                memory64: false,
            },
        })
    })
}

fn decode_global_section(reader: &mut Reader<'_>) -> Result<Vec<Global>, DecodeError> {
    reader.vector(|reader| {
        Ok(Global {
            global_type: global_type(reader)?,
            init: const_expr(reader)?,
        })
    })
}

fn decode_export_section(reader: &mut Reader<'_>) -> Result<Vec<Export>, DecodeError> {
    reader.vector(|reader| {
        let name = reader.name()?;
        let desc = match reader.byte()? {
            0x00 => ExportDesc::Function(reader.u32()?),
            0x01 => ExportDesc::Table(reader.u32()?),
            0x02 => ExportDesc::Memory(reader.u32()?),
            0x03 => ExportDesc::Global(reader.u32()?),
            byte => return Err(DecodeError::InvalidExportKind(byte)),
        };
        Ok(Export { name, desc })
    })
}

fn decode_start_section(reader: &mut Reader<'_>) -> Result<u32, DecodeError> {
    let index = reader.u32()?;
    reader.finish()?;
    Ok(index)
}

fn decode_element_section(reader: &mut Reader<'_>) -> Result<Vec<Element>, DecodeError> {
    reader.vector(|reader| {
        // An element segment begins with a kind byte selecting one of eight
        // forms. Forms 0-3 list plain function indices and carry an element kind
        // byte, which must be `0x00`. Forms 4-7 list constant expressions and
        // carry a reference type instead, so a segment can hold a null. The two
        // families are read differently and kept apart in `ElementInit`.
        let kind = reader.u32()?;
        let (mode, element_type, expressions) = match kind {
            // Active in table 0, no element kind byte.
            0 => (
                ElementMode::Active {
                    table_index: 0,
                    offset: const_expr(reader)?,
                },
                RefType::FuncRef,
                false,
            ),
            // Passive, declarative, and active-in-an-explicit-table all carry an
            // element kind byte, which is a single `0x00` for a function segment.
            1 | 3 => {
                element_kind(reader)?;
                (
                    if kind == 1 {
                        ElementMode::Passive
                    } else {
                        ElementMode::Declarative
                    },
                    RefType::FuncRef,
                    false,
                )
            }
            2 => {
                let table_index = reader.u32()?;
                let offset = const_expr(reader)?;
                element_kind(reader)?;
                (
                    ElementMode::Active {
                        table_index,
                        offset,
                    },
                    RefType::FuncRef,
                    false,
                )
            }
            // The expression forms, which name their reference type directly and
            // so may be `externref` as well as `funcref`.
            4 => (
                ElementMode::Active {
                    table_index: 0,
                    offset: const_expr(reader)?,
                },
                RefType::FuncRef,
                true,
            ),
            5 | 7 => {
                let element_type = reference_type(reader)?;
                (
                    if kind == 5 {
                        ElementMode::Passive
                    } else {
                        ElementMode::Declarative
                    },
                    element_type,
                    true,
                )
            }
            6 => {
                let table_index = reader.u32()?;
                let offset = const_expr(reader)?;
                let element_type = reference_type(reader)?;
                (
                    ElementMode::Active {
                        table_index,
                        offset,
                    },
                    element_type,
                    true,
                )
            }
            _ => return Err(DecodeError::UnsupportedElementSegmentKind),
        };
        let init = if expressions {
            ElementInit::Expressions(reader.vector(const_expr)?)
        } else {
            ElementInit::FuncIndices(reader.vector(Reader::u32)?)
        };
        Ok(Element {
            element_type,
            mode,
            init,
        })
    })
}

/// Read the element kind byte, which must be `0x00` for a function segment.
///
/// This is one byte in the encoding, not a LEB, so it is read as a byte. The one
/// value that is valid here is the zero byte; an `externref` segment would be a
/// different byte, and no such segment is representable above.
fn element_kind(reader: &mut Reader<'_>) -> Result<(), DecodeError> {
    if reader.byte()? != 0x00 {
        return Err(DecodeError::UnsupportedElementSegmentKind);
    }
    Ok(())
}

fn decode_data_section(reader: &mut Reader<'_>) -> Result<Vec<DataSegment>, DecodeError> {
    reader.vector(|reader| {
        // A data segment begins with a kind byte, and all three forms are part of
        // the core format: 0 is an active segment in memory 0, 1 is passive, and
        // 2 is active in an explicitly named memory. The kind is read as a u32
        // rather than a single byte so that a non-minimal encoding of a valid
        // kind is accepted, which the core format allows; the check below is on
        // the value, not on how it was spelled.
        let kind = reader.u32()?;
        let mode = match kind {
            0 => DataMode::Active {
                memory_index: 0,
                offset: const_expr(reader)?,
            },
            1 => DataMode::Passive,
            2 => DataMode::Active {
                memory_index: reader.u32()?,
                offset: const_expr(reader)?,
            },
            _ => return Err(DecodeError::UnsupportedDataSegmentKind),
        };
        let length = reader.u32()? as usize;
        let data = reader.take(length)?.to_vec();
        Ok(DataSegment { mode, data })
    })
}

fn decode_custom_section(payload: &[u8]) -> Result<CustomSection, DecodeError> {
    let mut reader = Reader::new(payload);
    let name = reader.name()?;
    let data = reader.take(reader.remaining())?.to_vec();
    Ok(CustomSection { name, data })
}

type CodeEntry = (Vec<LocalDecl>, Vec<u8>);

fn decode_code_section(reader: &mut Reader<'_>) -> Result<Vec<CodeEntry>, DecodeError> {
    reader.vector(|reader| {
        let body_length = reader.u32()? as usize;
        let payload = reader.take(body_length)?;
        let mut body_reader = Reader::new(payload);
        let locals = body_reader.vector(|reader| {
            Ok(LocalDecl {
                count: reader.u32()?,
                value_type: value_type(reader)?,
            })
        })?;
        // The declared local counts are a *sum*, and the spec caps that sum at
        // 2^32-1. Each count is a u32 on its own, so a module can declare groups
        // that individually fit and together overflow; allocating on the sum
        // would be the point of failure, so it is rejected here instead.
        let mut total: u64 = 0;
        for decl in &locals {
            total += u64::from(decl.count);
            if total > u64::from(u32::MAX) {
                return Err(DecodeError::TooManyLocals);
            }
        }
        let body = body_reader.take(body_reader.remaining())?.to_vec();
        body_reader.finish()?;
        // A function body is an expression, and an expression must end with the
        // `end` opcode. A body whose last byte is anything else would leave the
        // interpreter running past the end of the function, so the structure is
        // checked here rather than trusted to the executor. This does not parse
        // the body: an illegal opcode in the middle is still caught later, by
        // the instruction decoder.
        if body.last() != Some(&0x0b) {
            return Err(DecodeError::MissingEndOpcode);
        }
        Ok((locals, body))
    })
}

pub fn decode(bytes: &[u8]) -> Result<Module, DecodeError> {
    let mut reader = Reader::new(bytes);
    if reader.take(4)? != MAGIC {
        return Err(DecodeError::InvalidMagic);
    }
    if reader.take(4)? != VERSION {
        return Err(DecodeError::InvalidVersion);
    }

    let mut module = Module::default();
    let mut previous_section = 0u8;
    let mut function_type_indices = Vec::new();
    let mut code_entries = Vec::new();
    let mut saw_start = false;

    while reader.remaining() > 0 {
        let id = reader.byte()?;
        let length = reader.u32()? as usize;
        let payload = reader.take(length)?;
        if id != 0 {
            if id > 11 {
                return Err(DecodeError::InvalidSectionId(id));
            }
            if id <= previous_section {
                return Err(DecodeError::DuplicateSection(id));
            }
            previous_section = id;
        }
        let custom = if id == 0 {
            Some(decode_custom_section(payload)?)
        } else {
            None
        };
        let mut section = Reader::new(payload);
        match id {
            0 => module
                .custom_sections
                .push(custom.expect("custom section decoded")),
            1 => module.types = decode_type_section(&mut section)?,
            2 => module.imports = decode_import_section(&mut section)?,
            3 => function_type_indices = decode_function_section(&mut section)?,
            4 => module.tables = decode_table_section(&mut section)?,
            5 => module.memories = decode_memory_section(&mut section)?,
            6 => module.globals = decode_global_section(&mut section)?,
            7 => module.exports = decode_export_section(&mut section)?,
            8 => {
                if saw_start {
                    return Err(DecodeError::DuplicateSection(8));
                }
                saw_start = true;
                module.start = Some(decode_start_section(&mut section)?);
            }
            9 => module.elements = decode_element_section(&mut section)?,
            10 => code_entries = decode_code_section(&mut section)?,
            11 => module.data = decode_data_section(&mut section)?,
            _ => return Err(DecodeError::InvalidSectionId(id)),
        }
        if id != 0 {
            section.finish()?;
        }
    }

    if function_type_indices.len() != code_entries.len() {
        return Err(DecodeError::MismatchedCodeSection);
    }
    module.functions = function_type_indices
        .into_iter()
        .zip(code_entries)
        .map(|(type_index, (locals, body))| Function {
            type_index,
            locals,
            body,
        })
        .collect();
    Ok(module)
}
