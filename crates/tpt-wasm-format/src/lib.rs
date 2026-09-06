// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! WebAssembly module structure.
//!
//! Semantic representation of a decoded WebAssembly module.
//! This is NOT an execution representation — it is the structural view
//! produced by decoding and consumed by validation.

use tpt_wasm_types::{FunctionType, GlobalType, MemoryType, RefType, TableType, TagType, ValueType};

/// A decoded WebAssembly module (structural representation).
#[derive(Debug, Clone, Default)]
pub struct Module {
    pub types: Vec<FunctionType>,
    pub imports: Vec<Import>,
    pub functions: Vec<Function>,
    pub tables: Vec<Table>,
    pub memories: Vec<Memory>,
    pub globals: Vec<Global>,
    pub exports: Vec<Export>,
    pub start: Option<u32>,
    pub elements: Vec<Element>,
    pub data: Vec<DataSegment>,
    pub tags: Vec<Tag>,
    pub custom_sections: Vec<CustomSection>,
}

#[derive(Debug, Clone)]
pub struct Import {
    pub module: String,
    pub name: String,
    pub desc: ImportDesc,
}

#[derive(Debug, Clone)]
pub enum ImportDesc {
    Function(u32),
    Table(TableType),
    Memory(MemoryType),
    Global(GlobalType),
    Tag(TagType),
}

#[derive(Debug, Clone)]
pub struct Function {
    pub type_index: u32,
    pub locals: Vec<LocalDecl>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct LocalDecl {
    pub count: u32,
    pub value_type: ValueType,
}

#[derive(Debug, Clone)]
pub struct Table {
    pub table_type: TableType,
    pub init: Option<ConstExpr>,
}

#[derive(Debug, Clone)]
pub struct Memory {
    pub memory_type: MemoryType,
}

#[derive(Debug, Clone)]
pub struct Global {
    pub global_type: GlobalType,
    pub init: ConstExpr,
}

#[derive(Debug, Clone)]
pub struct Export {
    pub name: String,
    pub desc: ExportDesc,
}

#[derive(Debug, Clone, Copy)]
pub enum ExportDesc {
    Function(u32),
    Table(u32),
    Memory(u32),
    Global(u32),
    Tag(u32),
}

#[derive(Debug, Clone)]
pub struct Element {
    pub element_type: RefType,
    pub mode: ElementMode,
    pub init: Vec<ConstExpr>,
}

#[derive(Debug, Clone)]
pub enum ElementMode {
    Passive,
    Active { table_index: u32, offset: ConstExpr },
    Declarative,
}

#[derive(Debug, Clone)]
pub struct DataSegment {
    pub mode: DataMode,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum DataMode {
    Passive,
    Active { memory_index: u32, offset: ConstExpr },
}

#[derive(Debug, Clone)]
pub struct Tag {
    pub tag_type: TagType,
}

/// A constant expression (used in initializers).
#[derive(Debug, Clone)]
pub struct ConstExpr(pub Vec<u8>);

#[derive(Debug, Clone)]
pub struct CustomSection {
    pub name: String,
    pub data: Vec<u8>,
}
