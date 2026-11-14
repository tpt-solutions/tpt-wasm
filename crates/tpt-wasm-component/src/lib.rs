// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

#![forbid(unsafe_code)]
//! The Component Model envelope: types, records, imports, canonical functions, aliases, core
//! instances, and exports.
//!
//! A component is a binary with the Core Wasm magic, the version `0x0d` and the layer
//! `0x01`, so it cannot be mistaken for a core module, whose layer is zero. After the
//! preamble comes a sequence of sections: each is an id byte, a `u32` size, and the
//! contents. This crate reads that framing, collects the core modules a component nests,
//! decodes the type (7), import (10), canon (8), alias (6), core-instance (2) and export (11)
//! sections, and runs the instances on a runtime `Engine`.
//!
//! The values that cross the boundary are scalars (`bool`, `s8`, `u8`, `s16`, `u16`, `s32`,
//! `u32`, `s64`, `u64`, `f32`, `f64`, `char`), UTF-8 strings, and records of scalars. A scalar
//! is one core value reinterpreted at the boundary. A string is a pointer and a length into the
//! instance's linear memory, copied in through the instance's `realloc` and copied out from its
//! memory. A record is passed as its fields, flattened, and a record result that does not fit one
//! core value is read from memory with the canonical layout. A component function is either an
//! import the embedder supplies, or a core function lifted to a component type. An import can be
//! lowered to a core function, which a core module calls back into the embedder through, and each
//! import is granted through the capability model. Every other section, and every form the crate
//! does not implement, is refused by name and offset rather than skipped.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use tpt_wasm_capability::{CapabilityId, CapabilitySet, Operation};
use tpt_wasm_format::{ExportDesc, ImportDesc};
use tpt_wasm_runtime::{Engine, HostFunction, Instance, Linker};
use tpt_wasm_types::{FunctionType, ResultType, Value, ValueType};

/// The component preamble: the magic, then version `0x0d` and layer `0x01`.
pub const PREAMBLE: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];

/// The most flat core parameters a lifted function takes directly. More are passed through
/// one pointer, which this crate does not implement, so such a function is refused.
const MAX_FLAT_PARAMS: usize = 16;

/// The most flat core results a function returns directly. More are returned through memory.
const MAX_FLAT_RESULTS: usize = 1;

/// The section id of a custom section, which is skipped.
const SECTION_CUSTOM: u8 = 0;
/// The section id of a nested core module.
const SECTION_CORE_MODULE: u8 = 1;
/// The section id of core instances.
const SECTION_CORE_INSTANCE: u8 = 2;
/// The section id of aliases.
const SECTION_ALIAS: u8 = 6;
/// The section id of component types.
const SECTION_TYPE: u8 = 7;
/// The section id of canonical functions.
const SECTION_CANON: u8 = 8;
/// The section id of imports.
const SECTION_IMPORT: u8 = 10;
/// The section id of exports.
const SECTION_EXPORT: u8 = 11;

/// The core-instance expression tag that instantiates a module.
const INSTANTIATE: u8 = 0x00;
/// The core-instance expression tag that builds an instance from inline exports.
const INLINE_EXPORTS: u8 = 0x01;
/// The sort wrapper that marks a core sort in an alias.
const SORT_CORE_WRAPPER: u8 = 0x00;
/// The core sort of a function, and the sort of a function in a sort index.
const SORT_FUNC: u8 = 0x00;
/// The core sort of a memory.
const SORT_CORE_MEMORY: u8 = 0x02;
/// The sort byte of a core instance, which an instance argument names.
const SORT_CORE_INSTANCE: u8 = 0x12;
/// The alias tag that takes an export of a core instance.
const ALIAS_CORE_EXPORT: u8 = 0x01;
/// The component sort of a function in an export.
const COMPONENT_SORT_FUNC: u8 = 0x01;
/// The extern type tag of a function, in an import.
const EXTERN_FUNC: u8 = 0x01;

/// The component type tag of a function type, and of a record.
const TYPE_FUNC: u8 = 0x40;
const TYPE_RECORD: u8 = 0x72;
const TYPE_LIST: u8 = 0x70;
const TYPE_OPTION: u8 = 0x6b;
const TYPE_ENUM: u8 = 0x6d;
const TYPE_FLAGS: u8 = 0x6e;
const TYPE_TUPLE: u8 = 0x6f;
const TYPE_RESULT: u8 = 0x6a;
const TYPE_VARIANT: u8 = 0x71;
/// The canon tag of a lift, and of a lower.
const CANON_LIFT: u8 = 0x00;
const CANON_LOWER: u8 = 0x01;
/// The canonical option tags: the string encoding (UTF-8 only), a memory, and a `realloc`.
const OPTION_UTF8: u8 = 0x00;
/// The string encoding options: UTF-16, and Latin-1 with UTF-16 as the fallback.
const OPTION_UTF16: u8 = 0x01;
const OPTION_LATIN1_UTF16: u8 = 0x02;
/// The option naming a core function the runtime calls after a lift has read its result.
const OPTION_POST_RETURN: u8 = 0x05;
/// The longest string the canonical ABI admits, in bytes.
const MAX_STRING_BYTE_LENGTH: usize = (1 << 28) - 1;
/// The bit a `latin1+utf16` string's length carries when it is stored as UTF-16.
const UTF16_TAG: u32 = 0x8000_0000;
const OPTION_MEMORY: u8 = 0x03;
const OPTION_REALLOC: u8 = 0x04;
/// The value type bytes this crate supports.
const VALTYPE_S32: u8 = 0x7a;
const VALTYPE_U32: u8 = 0x79;
const VALTYPE_S64: u8 = 0x78;
const VALTYPE_U64: u8 = 0x77;
const VALTYPE_F32: u8 = 0x76;
const VALTYPE_F64: u8 = 0x75;
const VALTYPE_CHAR: u8 = 0x74;
const VALTYPE_STRING: u8 = 0x73;
const VALTYPE_U16: u8 = 0x7b;
const VALTYPE_S16: u8 = 0x7c;
const VALTYPE_U8: u8 = 0x7d;
const VALTYPE_S8: u8 = 0x7e;
const VALTYPE_BOOL: u8 = 0x7f;

/// The operation an import's capability supports: calling the embedder's function.
const CALL_OPERATION: &str = "call";

/// Why a component was refused or could not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentError {
    /// The bytes are not a component: the magic, version or layer is wrong. A core module
    /// is refused this way, because its layer is zero.
    NotAComponent,
    /// The bytes end inside a section's framing or contents.
    Truncated { offset: usize },
    /// A `u32` in `LEB128` form is not canonical or does not fit.
    InvalidInteger { offset: usize },
    /// A section of a kind this crate does not implement yet.
    UnsupportedSection { id: u8, offset: usize },
    /// An instance form this crate does not implement yet.
    UnsupportedInstance { what: &'static str, offset: usize },
    /// An alias form this crate does not implement yet.
    UnsupportedAlias { what: &'static str, offset: usize },
    /// A type this crate does not implement yet.
    UnsupportedType { what: &'static str, offset: usize },
    /// An import form this crate does not implement yet.
    UnsupportedImport { what: &'static str, offset: usize },
    /// A canonical function form this crate does not implement yet.
    UnsupportedCanon { what: &'static str, offset: usize },
    /// An export form this crate does not implement yet.
    UnsupportedExport { what: &'static str, offset: usize },
    /// A tag or index that the grammar does not allow here, or a name that is not UTF-8.
    Malformed { offset: usize },
    /// A core instance names a module the component does not nest.
    NoSuchModule { module: u32 },
    /// A reference names a core instance that is not declared before it.
    NoSuchInstance { instance: usize },
    /// A reference names a core function that is not defined before it.
    NoSuchFunction { function: u32 },
    /// A reference names a core memory that is not defined before it.
    NoSuchMemory { memory: u32 },
    /// A reference names a component type that is not declared before it.
    NoSuchType { index: u32 },
    /// A reference names a component function that is not defined before it.
    NoSuchComponentFunction { function: u32 },
    /// No export has this name.
    NoSuchExport { name: String },
    /// The component imports a function the embedder did not supply.
    MissingImport { name: String },
    /// The embedder supplied a function whose type differs from the component's import.
    ImportMismatch { name: String, message: String },
    /// An import is not granted the `call` operation on its capability.
    CapabilityDenied { name: String, message: String },
    /// A core instance could not be created, with the runtime's reason.
    Instantiate { instance: usize, message: String },
    /// A lifted function's core type does not match its component type, or its memory or
    /// `realloc` does not exist.
    LiftMismatch { export: String, message: String },
    /// A call's arguments do not match the export's type.
    ArgumentMismatch { export: String, message: String },
    /// A call into a core instance, into its memory, or into an embedder function failed.
    Call { instance: usize, message: String },
}

impl fmt::Display for ComponentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ComponentError::NotAComponent => write!(f, "not a component"),
            ComponentError::Truncated { offset } => {
                write!(f, "the component ends inside a section at byte {offset}")
            }
            ComponentError::InvalidInteger { offset } => {
                write!(f, "invalid integer at byte {offset}")
            }
            ComponentError::UnsupportedSection { id, offset } => {
                write!(f, "section id {id} at byte {offset} is not supported yet")
            }
            ComponentError::UnsupportedInstance { what, offset }
            | ComponentError::UnsupportedAlias { what, offset }
            | ComponentError::UnsupportedType { what, offset }
            | ComponentError::UnsupportedImport { what, offset }
            | ComponentError::UnsupportedCanon { what, offset }
            | ComponentError::UnsupportedExport { what, offset } => {
                write!(f, "{what} at byte {offset} is not supported yet")
            }
            ComponentError::Malformed { offset } => write!(f, "malformed entry at byte {offset}"),
            ComponentError::NoSuchModule { module } => {
                write!(
                    f,
                    "core instance names module {module}, which the component does not nest"
                )
            }
            ComponentError::NoSuchInstance { instance } => {
                write!(
                    f,
                    "there is no core instance {instance} declared before this use"
                )
            }
            ComponentError::NoSuchFunction { function } => {
                write!(
                    f,
                    "there is no core function {function} defined before this use"
                )
            }
            ComponentError::NoSuchMemory { memory } => {
                write!(
                    f,
                    "there is no core memory {memory} defined before this use"
                )
            }
            ComponentError::NoSuchType { index } => {
                write!(
                    f,
                    "there is no component type {index} declared before this use"
                )
            }
            ComponentError::NoSuchComponentFunction { function } => {
                write!(
                    f,
                    "there is no component function {function} defined before this use"
                )
            }
            ComponentError::NoSuchExport { name } => write!(f, "there is no export named {name}"),
            ComponentError::MissingImport { name } => {
                write!(f, "the component imports {name}, which was not supplied")
            }
            ComponentError::ImportMismatch { name, message } => {
                write!(
                    f,
                    "the supplied {name} does not match the component's import: {message}"
                )
            }
            ComponentError::CapabilityDenied { name, message } => {
                write!(f, "the import {name} is not granted: {message}")
            }
            ComponentError::Instantiate { instance, message } => {
                write!(f, "core instance {instance} did not instantiate: {message}")
            }
            ComponentError::LiftMismatch { export, message } => {
                write!(
                    f,
                    "export {export} does not match its component type: {message}"
                )
            }
            ComponentError::ArgumentMismatch { export, message } => {
                write!(f, "a call to {export} does not match its type: {message}")
            }
            ComponentError::Call { instance, message } => {
                write!(f, "a call into core instance {instance} failed: {message}")
            }
        }
    }
}

impl std::error::Error for ComponentError {}

/// A component's scalar value type: one core value, reinterpreted at the boundary, or a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    S32,
    U32,
    S64,
    U64,
    F32,
    F64,
    /// A boolean, one `i32`: zero is false, and any other value is true.
    Bool,
    S8,
    U8,
    S16,
    U16,
    /// A Unicode scalar value, one `i32`, which must not be a surrogate or above `0x10FFFF`.
    Char,
    /// A UTF-8 string, passed as a pointer and a length into linear memory.
    String,
}

impl Scalar {
    /// The core value type one value of this type takes in one position. A string's pointer
    /// takes one, and its length a second.
    fn single(self) -> ValueType {
        match self {
            Scalar::S32
            | Scalar::U32
            | Scalar::String
            | Scalar::Bool
            | Scalar::S8
            | Scalar::U8
            | Scalar::S16
            | Scalar::U16
            | Scalar::Char => ValueType::I32,
            Scalar::S64 | Scalar::U64 => ValueType::I64,
            Scalar::F32 => ValueType::F32,
            Scalar::F64 => ValueType::F64,
        }
    }

    /// The core parameters this type flattens to: a string is its pointer and its length.
    fn flat_params(self) -> Vec<ValueType> {
        match self {
            Scalar::String => vec![ValueType::I32, ValueType::I32],
            other => vec![other.single()],
        }
    }

    /// The size and alignment of this type in linear memory, by the canonical ABI.
    fn size_and_align(self) -> (usize, usize) {
        match self {
            Scalar::Bool | Scalar::S8 | Scalar::U8 => (1, 1),
            Scalar::S16 | Scalar::U16 => (2, 2),
            Scalar::S32 | Scalar::U32 | Scalar::F32 | Scalar::Char => (4, 4),
            Scalar::S64 | Scalar::U64 | Scalar::F64 => (8, 8),
            // A string in memory is its pointer and its length.
            Scalar::String => (8, 4),
        }
    }
}

/// A component value type: a scalar, or a record, named by its index in the type table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentType {
    Scalar(Scalar),
    /// A record, by the index of its definition in the component's type table.
    Record(u32),
    /// A list of scalars, by the index of its definition in the component's type table.
    List(u32),
    /// An option of a scalar, by the index of its definition in the component's type table.
    Option(u32),
    /// An enum, by the index of its definition in the component's type table.
    Enum(u32),
    /// A set of flags, by the index of its definition in the component's type table.
    Flags(u32),
    /// A tuple of scalars, by the index of its definition in the type table.
    Tuple(u32),
    /// A result, by the index of its definition in the component's type table.
    Result(u32),
    /// A variant, by the index of its definition in the component's type table.
    Variant(u32),
}

/// One field of a record type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordField {
    /// The field's label.
    pub name: String,
    /// The field's type. A record field is a scalar, not a string or another record.
    pub ty: Scalar,
}

/// One definition of the component's type index space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentTypeDef {
    /// A function type.
    Func(ComponentFuncType),
    /// A record type, with its fields in order.
    Record(Vec<RecordField>),
    /// A list type, whose elements are of one scalar type.
    List(Scalar),
    /// An option type, whose payload is of one scalar type.
    Option(Scalar),
    /// An enum type: its case labels, in order. A value is the index of one of them.
    Enum(Vec<String>),
    /// A tuple of scalars, in order.
    Tuple(Vec<Scalar>),
    /// A result: the payload types of its ok and error cases, each optional.
    Result {
        ok: Option<Scalar>,
        err: Option<Scalar>,
    },
    /// A variant: each case's label and the scalar its payload holds, if it has one. Every
    /// payload shares one flat type.
    Variant {
        cases: Vec<(String, Option<Scalar>)>,
    },
    /// A set of flags: its labels, at most 32, each a bit.
    Flags(Vec<String>),
}

/// A component function type: parameter types and at most one result type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentFuncType {
    /// The parameter types, in order.
    pub params: Vec<ComponentType>,
    /// The result type, if the function returns one.
    pub result: Option<ComponentType>,
}

/// A value of a component type, as the caller sees it.
#[derive(Debug, Clone, PartialEq)]
pub enum ComponentValue {
    S32(i32),
    U32(u32),
    S64(i64),
    U64(u64),
    F32(f32),
    F64(f64),
    String(String),
    Bool(bool),
    S8(i8),
    U8(u8),
    S16(i16),
    U16(u16),
    Char(char),
    /// A record: its fields, in the order of its definition.
    Record(Vec<(String, ComponentValue)>),
    /// A list of scalars, whose elements all have the list's element type.
    List(Vec<ComponentValue>),
    /// An option: `None`, or the payload.
    Option(Option<Box<ComponentValue>>),
    /// An enum value: the label of one of its cases.
    Enum(String),
    /// A tuple: its values, in order.
    Tuple(Vec<ComponentValue>),
    /// A result: its ok or error case, and the case's payload if it has one.
    Result(Result<Option<Box<ComponentValue>>, Option<Box<ComponentValue>>>),
    /// A variant: the label of its case, and the case's payload if it has one.
    Variant(String, Option<Box<ComponentValue>>),
    /// A set of flags: the labels that are set.
    Flags(Vec<String>),
}

impl ComponentValue {
    /// The scalar type of this value, or `None` for a compound value.
    pub fn scalar(&self) -> Option<Scalar> {
        match self {
            ComponentValue::S32(_) => Some(Scalar::S32),
            ComponentValue::U32(_) => Some(Scalar::U32),
            ComponentValue::S64(_) => Some(Scalar::S64),
            ComponentValue::U64(_) => Some(Scalar::U64),
            ComponentValue::F32(_) => Some(Scalar::F32),
            ComponentValue::F64(_) => Some(Scalar::F64),
            ComponentValue::String(_) => Some(Scalar::String),
            ComponentValue::Bool(_) => Some(Scalar::Bool),
            ComponentValue::S8(_) => Some(Scalar::S8),
            ComponentValue::U8(_) => Some(Scalar::U8),
            ComponentValue::S16(_) => Some(Scalar::S16),
            ComponentValue::U16(_) => Some(Scalar::U16),
            ComponentValue::Char(_) => Some(Scalar::Char),
            ComponentValue::Record(_)
            | ComponentValue::List(_)
            | ComponentValue::Option(_)
            | ComponentValue::Enum(_)
            | ComponentValue::Flags(_)
            | ComponentValue::Tuple(_)
            | ComponentValue::Result(_)
            | ComponentValue::Variant(..) => None,
        }
    }
}

/// The encoding a lifted function's strings use, named by its canon lift options. The default is
/// UTF-8, as the canonical ABI has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringEncoding {
    /// UTF-8, one byte a code unit.
    Utf8,
    /// UTF-16, two bytes a code unit.
    Utf16,
    /// Latin-1 where the string fits it, and UTF-16 where it does not, marked by the length's tag.
    Latin1Utf16,
}

/// How a string read from memory is decoded, which the encoding and the length tag select.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringDecoding {
    Utf8,
    Utf16,
    Latin1,
}

/// The little-endian UTF-16 bytes of `text`, two bytes a code unit.
fn utf16_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A component function: one entry of the component function index space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentFunction {
    /// A function the embedder supplies, under this name, of the type with index `ty`.
    Import { name: String, ty: u32 },
    /// A core function lifted to the type with index `ty`. Its `memory` and `realloc` are the
    /// core memory and function that a string or an indirect result crosses through.
    Lift {
        core_function: u32,
        ty: u32,
        memory: Option<u32>,
        realloc: Option<u32>,
        encoding: StringEncoding,
        /// The core function run after the result is read, if the lift names one.
        post_return: Option<u32>,
    },
}

/// One entry of the core function index space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreFunction {
    /// An export of a core instance, named by an alias.
    Alias { instance: u32, export: String },
    /// A component import, lowered so that core code can call it.
    Lowered { function: u32 },
}

/// One entry of the core memory index space: an export of a core instance, named by an alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreMemory {
    /// The index of the core instance that exports the memory.
    pub instance: u32,
    /// The name of the export.
    pub export: String,
}

/// One export of the component: a name for a component function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentExport {
    /// The export name.
    pub name: String,
    /// The index of the component function, in the component function index space.
    pub function: u32,
}

/// One instance argument: every import of the instantiated module under `name` resolves to
/// the exports of the core instance with index `instance`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceArgument {
    /// The import module name.
    pub name: String,
    /// The index of the core instance whose exports satisfy it.
    pub instance: u32,
}

/// One export of an inline-exports instance: a name for a core function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineExport {
    /// The export name.
    pub name: String,
    /// The index of the core function, in the core function index space.
    pub function: u32,
}

/// One core instance a component declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreInstance {
    /// Instantiate the nested core module with index `module`. Each argument binds one of
    /// the module's import modules to an earlier core instance.
    Instantiate {
        module: u32,
        arguments: Vec<InstanceArgument>,
    },
    /// A core instance that exists only to group functions: each export is one function of
    /// the core function index space. Nothing runs for it; calling one of its exports runs
    /// the function it names.
    FromExports { exports: Vec<InlineExport> },
}

/// The embedder's implementation of a component import: scalar values in, scalar values out.
pub type ComponentFn =
    Arc<dyn Fn(&[ComponentValue]) -> Result<Vec<ComponentValue>, String> + Send + Sync>;

/// An embedder function with the type it was supplied at.
#[derive(Clone)]
struct SuppliedFunction {
    ty: ComponentFuncType,
    closure: ComponentFn,
}

/// The functions an embedder supplies for a component's imports, by name, each with its type.
/// The type must be the one the component declares for the import, or the instantiation is
/// refused.
///
/// Each import is a capability whose id is its name, and whose only operation is `call`. An
/// import runs only if the capability set grants that operation, and the grant is checked again
/// on every call, so revoking a grant stops the later calls. Without a capability set, no import
/// runs.
#[derive(Default)]
pub struct Imports {
    functions: BTreeMap<String, SuppliedFunction>,
    capabilities: Option<Arc<Mutex<CapabilitySet>>>,
}

impl Imports {
    /// No imports, and no capability set.
    pub fn new() -> Self {
        Self::default()
    }

    /// No functions yet, drawing on `capabilities` for the grants each import needs.
    pub fn with_capabilities(capabilities: Arc<Mutex<CapabilitySet>>) -> Self {
        Self {
            functions: BTreeMap::new(),
            capabilities: Some(capabilities),
        }
    }

    /// Supply the import `name`, of type `ty`. The function receives the arguments and returns
    /// the results, or an error that stops the call that reached it.
    pub fn define_function<F>(
        &mut self,
        name: impl Into<String>,
        ty: ComponentFuncType,
        function: F,
    ) -> &mut Self
    where
        F: Fn(&[ComponentValue]) -> Result<Vec<ComponentValue>, String> + Send + Sync + 'static,
    {
        self.functions.insert(
            name.into(),
            SuppliedFunction {
                ty,
                closure: Arc::new(function),
            },
        );
        self
    }
}

/// A decoded component envelope.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Component {
    core_modules: Vec<Vec<u8>>,
    core_functions: Vec<CoreFunction>,
    core_memories: Vec<CoreMemory>,
    core_instances: Vec<CoreInstance>,
    types: Vec<ComponentTypeDef>,
    functions: Vec<ComponentFunction>,
    exports: Vec<ComponentExport>,
}

impl Component {
    /// Decode a component envelope.
    pub fn decode(bytes: &[u8]) -> Result<Self, ComponentError> {
        if bytes.len() < PREAMBLE.len() || bytes[..PREAMBLE.len()] != PREAMBLE {
            return Err(ComponentError::NotAComponent);
        }
        let mut component = Component::default();
        let mut reader = Reader::new(bytes, PREAMBLE.len(), bytes.len());
        while !reader.at_end() {
            let id = reader.byte()?;
            let size_at = reader.position();
            let size = reader.u32()?;
            let start = reader.position();
            let end = start
                .checked_add(size as usize)
                .filter(|end| *end <= bytes.len())
                .ok_or(ComponentError::Truncated { offset: start })?;
            let mut section = Reader::new(bytes, start, end);
            match id {
                SECTION_CUSTOM => {}
                SECTION_CORE_MODULE => component.core_modules.push(bytes[start..end].to_vec()),
                SECTION_ALIAS => decode_aliases(
                    &mut section,
                    &component.core_instances,
                    &mut component.core_functions,
                    &mut component.core_memories,
                )?,
                SECTION_CORE_INSTANCE => decode_instances(
                    &mut section,
                    &component.core_functions,
                    &mut component.core_instances,
                )?,
                SECTION_TYPE => decode_types(&mut section, &mut component.types)?,
                SECTION_IMPORT => {
                    decode_imports(&mut section, &component.types, &mut component.functions)?
                }
                SECTION_CANON => decode_canon(
                    &mut section,
                    &mut component.core_functions,
                    &component.core_memories,
                    &component.types,
                    &mut component.functions,
                )?,
                SECTION_EXPORT => {
                    decode_exports(&mut section, &component.functions, &mut component.exports)?
                }
                other => {
                    return Err(ComponentError::UnsupportedSection {
                        id: other,
                        offset: size_at - 1,
                    })
                }
            }
            // Every section with contents must fill its size exactly.
            if id != SECTION_CUSTOM && id != SECTION_CORE_MODULE && !section.at_end() {
                return Err(ComponentError::Malformed {
                    offset: section.position(),
                });
            }
            reader = Reader::new(bytes, end, bytes.len());
        }
        for instance in &component.core_instances {
            if let CoreInstance::Instantiate { module, .. } = instance {
                if *module as usize >= component.core_modules.len() {
                    return Err(ComponentError::NoSuchModule { module: *module });
                }
            }
        }
        Ok(component)
    }

    /// The core modules the component nests, as their raw bytes.
    pub fn core_modules(&self) -> &[Vec<u8>] {
        &self.core_modules
    }

    /// The core functions, in index order.
    pub fn core_functions(&self) -> &[CoreFunction] {
        &self.core_functions
    }

    /// The core memories, in index order.
    pub fn core_memories(&self) -> &[CoreMemory] {
        &self.core_memories
    }

    /// The core instances the component declares, in order.
    pub fn core_instances(&self) -> &[CoreInstance] {
        &self.core_instances
    }

    /// The component's type definitions, in index order.
    pub fn component_types(&self) -> &[ComponentTypeDef] {
        &self.types
    }

    /// The component functions, in index order.
    pub fn component_functions(&self) -> &[ComponentFunction] {
        &self.functions
    }

    /// The component's exports, in the order they appear.
    pub fn component_exports(&self) -> &[ComponentExport] {
        &self.exports
    }

    /// Instantiate a component that has no imports. See [`Component::instantiate_with`].
    pub fn instantiate(&self, engine: &Engine) -> Result<ComponentInstance, ComponentError> {
        self.instantiate_with(engine, &Imports::new())
    }

    /// Instantiate each core instance, in order, on `engine`, with the embedder's `imports`.
    /// Every import must be supplied at its declared type and granted its capability. Each lifted
    /// function's core type is then checked against its component type, its memory and `realloc`
    /// are checked to exist, and the exports are recorded.
    pub fn instantiate_with(
        &self,
        engine: &Engine,
        imports: &Imports,
    ) -> Result<ComponentInstance, ComponentError> {
        // Every import must be supplied, at the type the component declares for it.
        for function in &self.functions {
            if let ComponentFunction::Import { name, ty } = function {
                let supplied = imports
                    .functions
                    .get(name)
                    .ok_or_else(|| ComponentError::MissingImport { name: name.clone() })?;
                let declared = self
                    .func_type(*ty)
                    .ok_or(ComponentError::NoSuchType { index: *ty })?;
                if supplied.ty != *declared {
                    return Err(ComponentError::ImportMismatch {
                        name: name.clone(),
                        message: format!(
                            "the component declares {declared:?}, and {:?} was supplied",
                            supplied.ty
                        ),
                    });
                }
            }
        }
        // Every import must also be granted the `call` operation on its capability.
        for function in &self.functions {
            if let ComponentFunction::Import { name, .. } = function {
                authorize_call(imports.capabilities.as_ref(), name).map_err(|message| {
                    ComponentError::CapabilityDenied {
                        name: name.clone(),
                        message,
                    }
                })?;
            }
        }

        let mut slots: Vec<Slot> = Vec::with_capacity(self.core_instances.len());
        for (index, instance) in self.core_instances.iter().enumerate() {
            let slot = match instance {
                CoreInstance::Instantiate { module, arguments } => {
                    let bytes = &self.core_modules[*module as usize];
                    // Each core instance gets a linker of its own, so one instance's imports
                    // cannot leak into another.
                    let mut linker = Linker::new();
                    for argument in arguments {
                        bind(
                            &mut linker,
                            &argument.name,
                            &slots,
                            argument.instance as usize,
                        )
                        .map_err(|message| {
                            ComponentError::Instantiate {
                                instance: index,
                                message,
                            }
                        })?;
                    }
                    let runtime = engine
                        .instantiate_bytes_with_linker(bytes, &linker)
                        .map_err(|error| ComponentError::Instantiate {
                            instance: index,
                            message: error.to_string(),
                        })?;
                    Slot::Runtime(Box::new(runtime))
                }
                CoreInstance::FromExports { exports } => {
                    let mut bound = Vec::with_capacity(exports.len());
                    for export in exports {
                        let target = match &self.core_functions[export.function as usize] {
                            CoreFunction::Alias {
                                instance: source,
                                export: source_export,
                            } => {
                                let Slot::Runtime(runtime) = &slots[*source as usize] else {
                                    return Err(ComponentError::Instantiate {
                                        instance: index,
                                        message: "an alias names an inline-exports instance"
                                            .to_owned(),
                                    });
                                };
                                // Check now that the export exists, so a missing one is an
                                // error here and not at the first call.
                                runtime.get_function(source_export).map_err(|error| {
                                    ComponentError::Instantiate {
                                        instance: index,
                                        message: error.to_string(),
                                    }
                                })?;
                                Target::Alias {
                                    source: *source as usize,
                                    export: source_export.clone(),
                                }
                            }
                            CoreFunction::Lowered { function } => {
                                Target::Host(self.lowered(*function, imports).map_err(
                                    |message| ComponentError::Instantiate {
                                        instance: index,
                                        message,
                                    },
                                )?)
                            }
                        };
                        bound.push(BoundExport {
                            name: export.name.clone(),
                            target,
                        });
                    }
                    Slot::Exports(bound)
                }
            };
            slots.push(slot);
        }

        // Each lifted function's core type must be the flattening of its component type, and
        // the memory and `realloc` it names must exist in the instance that defines it.
        for function in &self.functions {
            let ComponentFunction::Lift {
                core_function,
                ty,
                memory,
                realloc,
                post_return,
                ..
            } = function
            else {
                continue;
            };
            let CoreFunction::Alias { instance, export } =
                &self.core_functions[*core_function as usize]
            else {
                return Err(ComponentError::LiftMismatch {
                    export: String::new(),
                    message: "a lift names a lowered function".to_owned(),
                });
            };
            let func_type = self
                .func_type(*ty)
                .ok_or(ComponentError::NoSuchType { index: *ty })?;
            let expected = lift_signature(func_type, &self.types);
            let Some(CoreInstance::Instantiate { module, .. }) =
                self.core_instances.get(*instance as usize)
            else {
                return Err(ComponentError::LiftMismatch {
                    export: export.clone(),
                    message: "the function comes from an inline-exports instance".to_owned(),
                });
            };
            let actual = core_signature(&self.core_modules[*module as usize], export).map_err(
                |message| ComponentError::LiftMismatch {
                    export: export.clone(),
                    message,
                },
            )?;
            if actual != expected {
                return Err(ComponentError::LiftMismatch {
                    export: export.clone(),
                    message: format!(
                        "the core function has type {actual:?}, but the component type needs {expected:?}"
                    ),
                });
            }
            if let Some(post_index) = post_return {
                let CoreFunction::Alias {
                    export: post_export,
                    ..
                } = &self.core_functions[*post_index as usize]
                else {
                    return Err(ComponentError::LiftMismatch {
                        export: export.clone(),
                        message: "the post-return is a lowered function".to_owned(),
                    });
                };
                let expected_post = post_return_signature(func_type, &self.types);
                let actual_post = core_signature(&self.core_modules[*module as usize], post_export)
                    .map_err(|message| ComponentError::LiftMismatch {
                        export: post_export.clone(),
                        message,
                    })?;
                if actual_post != expected_post {
                    return Err(ComponentError::LiftMismatch {
                        export: post_export.clone(),
                        message: format!(
                            "the post-return has type {actual_post:?}, but the result needs {expected_post:?}"
                        ),
                    });
                }
            }
            let runtime = match &slots[*instance as usize] {
                Slot::Runtime(runtime) => runtime,
                Slot::Exports(_) => {
                    return Err(ComponentError::LiftMismatch {
                        export: export.clone(),
                        message: "the function comes from an inline-exports instance".to_owned(),
                    })
                }
            };
            if let Some(index) = memory {
                let name = &self.core_memories[*index as usize].export;
                runtime
                    .read_memory(name, 0, 0)
                    .map_err(|error| ComponentError::LiftMismatch {
                        export: export.clone(),
                        message: format!("the memory {name} is missing: {error}"),
                    })?;
            }
            if let Some(index) = realloc {
                let CoreFunction::Alias {
                    export: realloc_name,
                    ..
                } = &self.core_functions[*index as usize]
                else {
                    return Err(ComponentError::LiftMismatch {
                        export: export.clone(),
                        message: "the realloc option names a lowered function".to_owned(),
                    });
                };
                runtime.get_function(realloc_name).map_err(|error| {
                    ComponentError::LiftMismatch {
                        export: export.clone(),
                        message: format!("the realloc {realloc_name} is missing: {error}"),
                    }
                })?;
            }
        }

        let mut exports = Vec::with_capacity(self.exports.len());
        for export in &self.exports {
            let ComponentFunction::Lift {
                core_function,
                ty,
                memory,
                realloc,
                encoding,
                post_return,
            } = &self.functions[export.function as usize]
            else {
                return Err(ComponentError::NoSuchComponentFunction {
                    function: export.function,
                });
            };
            let CoreFunction::Alias {
                instance,
                export: core_export,
            } = &self.core_functions[*core_function as usize]
            else {
                return Err(ComponentError::NoSuchFunction {
                    function: *core_function,
                });
            };
            let func_type = self
                .func_type(*ty)
                .ok_or(ComponentError::NoSuchType { index: *ty })?;
            exports.push(LiftedExport {
                name: export.name.clone(),
                params: func_type.params.clone(),
                result: func_type.result,
                core_instance: *instance as usize,
                core_export: core_export.clone(),
                memory: memory.map(|index| self.core_memories[index as usize].export.clone()),
                realloc: realloc.and_then(|index| match &self.core_functions[index as usize] {
                    CoreFunction::Alias { export, .. } => Some(export.clone()),
                    CoreFunction::Lowered { .. } => None,
                }),
                encoding: *encoding,
                post_return: post_return.and_then(|index| {
                    match &self.core_functions[index as usize] {
                        CoreFunction::Alias { export, .. } => Some(export.clone()),
                        CoreFunction::Lowered { .. } => None,
                    }
                }),
            });
        }
        Ok(ComponentInstance {
            slots,
            exports,
            types: Arc::new(self.types.clone()),
        })
    }

    /// The function type at type index `index`, if that definition is one.
    fn func_type(&self, index: u32) -> Option<&ComponentFuncType> {
        match self.types.get(index as usize)? {
            ComponentTypeDef::Func(func) => Some(func),
            ComponentTypeDef::Record(_)
            | ComponentTypeDef::List(_)
            | ComponentTypeDef::Option(_)
            | ComponentTypeDef::Enum(_)
            | ComponentTypeDef::Flags(_)
            | ComponentTypeDef::Tuple(_)
            | ComponentTypeDef::Result { .. }
            | ComponentTypeDef::Variant { .. } => None,
        }
    }

    /// The host function a lowered import is, given the embedder's implementation.
    fn lowered(&self, function: u32, imports: &Imports) -> Result<LoweredHost, String> {
        let ComponentFunction::Import { name, ty } = &self.functions[function as usize] else {
            return Err("a lowered function is not an import".to_owned());
        };
        let supplied = imports
            .functions
            .get(name)
            .ok_or_else(|| format!("the import {name} was not supplied"))?;
        let declared = self
            .func_type(*ty)
            .ok_or_else(|| format!("the import {name} is not a function"))?;
        // A parameter or a result that holds a string or a list lives in memory, which a lowered
        // import has no caller memory for, so it is refused here. A result that flattens to more
        // than one value is returned through memory, and is refused for the same reason.
        if declared
            .params
            .iter()
            .chain(declared.result.iter())
            .any(|ty| holds_string(ty) || holds_list(ty))
        {
            return Err(format!(
                "the import {name} takes or returns a string or a list"
            ));
        }
        if let Some(result) = &declared.result {
            if flat_of(result, &self.types).len() > MAX_FLAT_RESULTS {
                return Err(format!("the import {name} returns a value through memory"));
            }
        }
        Ok(LoweredHost {
            name: name.clone(),
            closure: supplied.closure.clone(),
            params: declared.params.clone(),
            result: declared.result,
            types: self.types.clone(),
            capabilities: imports.capabilities.clone(),
        })
    }
}

/// The size and alignment a component type takes in memory, by the canonical ABI. A discriminant
/// is one byte for up to 256 cases, two bytes up to 65536, and four beyond that.
fn size_and_align_of(ty: &ComponentType, types: &[ComponentTypeDef]) -> (usize, usize) {
    match ty {
        ComponentType::Scalar(scalar) => scalar.size_and_align(),
        ComponentType::List(_) => (8, 4),
        ComponentType::Record(index) => match types.get(*index as usize) {
            Some(ComponentTypeDef::Record(fields)) => {
                let (_, size, align) = record_layout(fields);
                (size, align)
            }
            _ => (0, 1),
        },
        ComponentType::Tuple(index) => match types.get(*index as usize) {
            Some(ComponentTypeDef::Tuple(scalars)) => {
                let fields = tuple_fields(scalars);
                let (_, size, align) = record_layout(&fields);
                (size, align)
            }
            _ => (0, 1),
        },
        ComponentType::Enum(index) => match types.get(*index as usize) {
            Some(ComponentTypeDef::Enum(labels)) => {
                let size = discriminant_size(labels.len());
                (size, size)
            }
            _ => (1, 1),
        },
        ComponentType::Flags(index) => match types.get(*index as usize) {
            Some(ComponentTypeDef::Flags(labels)) => {
                let size = match labels.len() {
                    0..=8 => 1,
                    9..=16 => 2,
                    _ => 4,
                };
                (size, size)
            }
            _ => (1, 1),
        },
        ComponentType::Option(index) => match types.get(*index as usize) {
            Some(ComponentTypeDef::Option(payload)) => case_layout(&[Some(*payload)]),
            _ => (1, 1),
        },
        ComponentType::Result(index) => match types.get(*index as usize) {
            Some(ComponentTypeDef::Result { ok, err }) => case_layout(&[*ok, *err]),
            _ => (1, 1),
        },
        ComponentType::Variant(index) => match types.get(*index as usize) {
            Some(ComponentTypeDef::Variant { cases }) => {
                let payloads: Vec<Option<Scalar>> = cases.iter().map(|(_, p)| *p).collect();
                case_layout(&payloads)
            }
            _ => (1, 1),
        },
    }
}

/// The size and alignment of a value with cases, each holding an optional scalar. The case
/// index is one byte; the payload follows at the offset its widest case's alignment allows.
fn case_layout(payloads: &[Option<Scalar>]) -> (usize, usize) {
    let align = payloads
        .iter()
        .flatten()
        .map(|payload| payload.size_and_align().1)
        .fold(1, usize::max);
    let payload_size = payloads
        .iter()
        .flatten()
        .map(|payload| payload.size_and_align().0)
        .fold(0, usize::max);
    (align_to(align_to(1, align) + payload_size, align), align)
}

/// The number of bytes a discriminant of `count` cases takes.
fn discriminant_size(count: usize) -> usize {
    match count {
        0..=256 => 1,
        257..=65536 => 2,
        _ => 4,
    }
}

/// The fields of a tuple of scalars, unlabelled, for laying it out as a record is.
fn tuple_fields(scalars: &[Scalar]) -> Vec<RecordField> {
    scalars
        .iter()
        .map(|ty| RecordField {
            name: String::new(),
            ty: *ty,
        })
        .collect()
}

/// The offsets of a list of values laid out one after another, and the size and alignment of
/// the whole, by the canonical record rules.
fn layout_of(types: &[ComponentType], defined: &[ComponentTypeDef]) -> (Vec<usize>, usize, usize) {
    let mut offsets = Vec::with_capacity(types.len());
    let mut size = 0usize;
    let mut align = 1usize;
    for ty in types {
        let (field_size, field_align) = size_and_align_of(ty, defined);
        align = align.max(field_align);
        let offset = align_to(size, field_align);
        offsets.push(offset);
        size = offset + field_size;
    }
    (offsets, align_to(size, align), align)
}

/// Write `bytes` into `buffer` at `at`, refusing a write past its end.
fn write_at(buffer: &mut [u8], at: usize, bytes: &[u8]) -> Result<(), String> {
    let end = at
        .checked_add(bytes.len())
        .filter(|end| *end <= buffer.len())
        .ok_or_else(|| "a value is written past the end of its parameters".to_owned())?;
    buffer[at..end].copy_from_slice(bytes);
    Ok(())
}

/// Store a value of type `ty` into `buffer` at `at`, in the canonical layout. Strings and lists
/// are refused, since their contents would need copying into memory of their own.
fn store(
    value: &ComponentValue,
    ty: &ComponentType,
    types: &[ComponentTypeDef],
    buffer: &mut [u8],
    at: usize,
) -> Result<(), String> {
    let mismatch = || "a value does not match its type".to_owned();
    match (value, ty) {
        (ComponentValue::String(_) | ComponentValue::List(_), _) => {
            Err("a string or list among indirect parameters is not stored".to_owned())
        }
        (value, ComponentType::Scalar(scalar)) => {
            let bytes = scalar_bytes(value, *scalar).ok_or_else(mismatch)?;
            write_at(buffer, at, &bytes)
        }
        (ComponentValue::Record(fields), ComponentType::Record(index)) => {
            let Some(ComponentTypeDef::Record(defined)) = types.get(*index as usize) else {
                return Err(mismatch());
            };
            let (offsets, _, _) = record_layout(defined);
            if fields.len() != defined.len() {
                return Err(mismatch());
            }
            for (((_, value), field), offset) in fields.iter().zip(defined).zip(offsets) {
                store(
                    value,
                    &ComponentType::Scalar(field.ty),
                    types,
                    buffer,
                    at + offset,
                )?;
            }
            Ok(())
        }
        (ComponentValue::Tuple(items), ComponentType::Tuple(index)) => {
            let Some(ComponentTypeDef::Tuple(scalars)) = types.get(*index as usize) else {
                return Err(mismatch());
            };
            let (offsets, _, _) = record_layout(&tuple_fields(scalars));
            if items.len() != scalars.len() {
                return Err(mismatch());
            }
            for ((item, scalar), offset) in items.iter().zip(scalars).zip(offsets) {
                store(
                    item,
                    &ComponentType::Scalar(*scalar),
                    types,
                    buffer,
                    at + offset,
                )?;
            }
            Ok(())
        }
        (ComponentValue::Enum(label), ComponentType::Enum(index)) => {
            let Some(ComponentTypeDef::Enum(labels)) = types.get(*index as usize) else {
                return Err(mismatch());
            };
            let case = labels
                .iter()
                .position(|candidate| candidate == label)
                .ok_or_else(mismatch)?;
            let size = discriminant_size(labels.len());
            write_at(buffer, at, &(case as u32).to_le_bytes()[..size])
        }
        (ComponentValue::Flags(set), ComponentType::Flags(index)) => {
            let Some(ComponentTypeDef::Flags(defined)) = types.get(*index as usize) else {
                return Err(mismatch());
            };
            let mut bits: u32 = 0;
            for label in set {
                let position = defined
                    .iter()
                    .position(|candidate| candidate == label)
                    .ok_or_else(mismatch)?;
                bits |= 1u32 << position;
            }
            let (size, _) = size_and_align_of(ty, types);
            write_at(buffer, at, &bits.to_le_bytes()[..size])
        }
        (ComponentValue::Option(payload), ComponentType::Option(index)) => {
            let Some(ComponentTypeDef::Option(element)) = types.get(*index as usize) else {
                return Err(mismatch());
            };
            let (_, align) = case_layout(&[Some(*element)]);
            match payload.as_deref() {
                None => write_at(buffer, at, &[0u8]),
                Some(value) => {
                    write_at(buffer, at, &[1u8])?;
                    store(
                        value,
                        &ComponentType::Scalar(*element),
                        types,
                        buffer,
                        at + align_to(1, align),
                    )
                }
            }
        }
        (ComponentValue::Result(outcome), ComponentType::Result(index)) => {
            let Some(ComponentTypeDef::Result { ok, err }) = types.get(*index as usize) else {
                return Err(mismatch());
            };
            let (discriminant, case_type, payload) = match outcome {
                Ok(payload) => (0u8, *ok, payload.as_deref()),
                Err(payload) => (1u8, *err, payload.as_deref()),
            };
            // The payload sits at the alignment of the widest payload of either case.
            let (_, align) = case_layout(&[*ok, *err]);
            write_at(buffer, at, &[discriminant])?;
            match (case_type, payload) {
                (None, None) => Ok(()),
                (Some(scalar), Some(value)) => store(
                    value,
                    &ComponentType::Scalar(scalar),
                    types,
                    buffer,
                    at + align_to(1, align),
                ),
                _ => Err(mismatch()),
            }
        }
        (ComponentValue::Variant(label, payload), ComponentType::Variant(index)) => {
            let Some(ComponentTypeDef::Variant { cases }) = types.get(*index as usize) else {
                return Err(mismatch());
            };
            let case = cases
                .iter()
                .position(|(name, _)| name == label)
                .ok_or_else(mismatch)?;
            let payloads: Vec<Option<Scalar>> = cases.iter().map(|(_, p)| *p).collect();
            let (_, align) = case_layout(&payloads);
            write_at(buffer, at, &[case as u8])?;
            match (cases[case].1, payload.as_deref()) {
                (None, None) => Ok(()),
                (Some(scalar), Some(value)) => store(
                    value,
                    &ComponentType::Scalar(scalar),
                    types,
                    buffer,
                    at + align_to(1, align),
                ),
                _ => Err(mismatch()),
            }
        }
        _ => Err(mismatch()),
    }
}

/// The core signature a post-return must have: it takes the flat values of the lifted result, or
/// one pointer when they do not fit, and returns nothing.
fn post_return_signature(func: &ComponentFuncType, types: &[ComponentTypeDef]) -> FunctionType {
    let params = match func.result {
        None => Vec::new(),
        Some(ty) => {
            let flat = flat_of(&ty, types);
            if flat.len() > MAX_FLAT_RESULTS {
                vec![ValueType::I32]
            } else {
                flat
            }
        }
    };
    FunctionType {
        params: ResultType(params),
        results: ResultType(Vec::new()),
    }
}

/// The core signature of a lifted function. Past 16 flat parameters, the parameters are one pointer
/// to them in memory, so the core function takes one `i32`.
fn lift_signature(func: &ComponentFuncType, types: &[ComponentTypeDef]) -> FunctionType {
    let mut signature = flat_signature(func, types);
    if flat_param_count(func, types) > MAX_FLAT_PARAMS {
        signature.params = ResultType(vec![ValueType::I32]);
    }
    signature
}

/// The core signature a component function of type `func` flattens to. A result that flattens
/// to more than one core value is returned through a pointer, so it is one `i32`.
fn flat_signature(func: &ComponentFuncType, types: &[ComponentTypeDef]) -> FunctionType {
    let params = func
        .params
        .iter()
        .flat_map(|ty| flat_of(ty, types))
        .collect();
    let results = match func.result {
        None => Vec::new(),
        Some(ty) => {
            let flat = flat_of(&ty, types);
            if flat.len() > MAX_FLAT_RESULTS {
                vec![ValueType::I32]
            } else {
                flat
            }
        }
    };
    FunctionType {
        params: ResultType(params),
        results: ResultType(results),
    }
}

/// The core values a component type flattens to: a record flattens to its fields in order.
fn flat_of(ty: &ComponentType, types: &[ComponentTypeDef]) -> Vec<ValueType> {
    match ty {
        ComponentType::Scalar(scalar) => scalar.flat_params(),
        ComponentType::Record(index) => match types.get(*index as usize) {
            Some(ComponentTypeDef::Record(fields)) => fields
                .iter()
                .flat_map(|field| field.ty.flat_params())
                .collect(),
            _ => Vec::new(),
        },
        // A list is its pointer and its length, as a string is.
        ComponentType::List(_) => vec![ValueType::I32, ValueType::I32],
        ComponentType::Tuple(index) => match types.get(*index as usize) {
            Some(ComponentTypeDef::Tuple(scalars)) => scalars
                .iter()
                .flat_map(|scalar| scalar.flat_params())
                .collect(),
            _ => Vec::new(),
        },
        // An enum is its case index.
        ComponentType::Enum(_) | ComponentType::Flags(_) => vec![ValueType::I32],
        // An option is its discriminant, then its payload's flat values.
        ComponentType::Option(index) => {
            let mut flat = vec![ValueType::I32];
            if let Some(ComponentTypeDef::Option(payload)) = types.get(*index as usize) {
                flat.extend(payload.flat_params());
            }
            flat
        }
        // A result is its discriminant, then the joined flat value of its payloads.
        ComponentType::Result(index) => {
            let mut flat = vec![ValueType::I32];
            if let Some(ComponentTypeDef::Result { ok, err }) = types.get(*index as usize) {
                if let Some(joined) = joined_flat([*ok, *err].into_iter().flatten()) {
                    flat.push(joined);
                }
            }
            flat
        }
        // A variant is its case index, then the one flat value every payload shares.
        ComponentType::Variant(index) => {
            let mut flat = vec![ValueType::I32];
            if let Some(ComponentTypeDef::Variant { cases }) = types.get(*index as usize) {
                if let Some(joined) = joined_flat(cases.iter().filter_map(|(_, p)| *p)) {
                    flat.push(joined);
                }
            }
            flat
        }
    }
}

/// Whether a component type holds a string, at the top level.
fn holds_string(ty: &ComponentType) -> bool {
    matches!(ty, ComponentType::Scalar(Scalar::String))
}

/// Whether a component type is a list, at the top level.
fn holds_list(ty: &ComponentType) -> bool {
    matches!(ty, ComponentType::List(_))
}

/// The offsets of a record's fields in memory, and the record's size and alignment, by the
/// canonical ABI: each field is placed at the next offset its alignment allows, and the
/// record's size is rounded up to its alignment.
fn record_layout(fields: &[RecordField]) -> (Vec<usize>, usize, usize) {
    let mut offsets = Vec::with_capacity(fields.len());
    let mut size = 0usize;
    let mut align = 1usize;
    for field in fields {
        let (field_size, field_align) = field.ty.size_and_align();
        align = align.max(field_align);
        let offset = align_to(size, field_align);
        offsets.push(offset);
        size = offset + field_size;
    }
    (offsets, align_to(size, align), align)
}

/// `value` rounded up to a multiple of `align`.
fn align_to(value: usize, align: usize) -> usize {
    value.div_ceil(align) * align
}

/// The type of the core function exported as `export` by a module, read from its bytes.
fn core_signature(bytes: &[u8], export: &str) -> Result<FunctionType, String> {
    let module = tpt_wasm_decode::decode(bytes)
        .map_err(|error| format!("the module does not decode: {error:?}"))?;
    let desc = module
        .exports
        .iter()
        .find(|candidate| candidate.name == export)
        .map(|candidate| candidate.desc)
        .ok_or_else(|| format!("the module does not export {export}"))?;
    let ExportDesc::Function(index) = desc else {
        return Err(format!("the module's {export} is not a function"));
    };
    let imported: Vec<u32> = module
        .imports
        .iter()
        .filter_map(|import| match import.desc {
            ImportDesc::Function(ty) => Some(ty),
            _ => None,
        })
        .collect();
    let type_index = match (index as usize).checked_sub(imported.len()) {
        None => imported[index as usize],
        Some(defined) => {
            module
                .functions
                .get(defined)
                .ok_or_else(|| format!("the module has no function {index}"))?
                .type_index
        }
    };
    module
        .types
        .get(type_index as usize)
        .cloned()
        .ok_or_else(|| format!("the module has no type {type_index}"))
}

/// Check that the import `name` is granted the `call` operation. With no capability set, nothing
/// is granted.
fn authorize_call(
    capabilities: Option<&Arc<Mutex<CapabilitySet>>>,
    name: &str,
) -> Result<(), String> {
    let Some(capabilities) = capabilities else {
        return Err("no capability set was supplied".to_owned());
    };
    let set = capabilities
        .lock()
        .map_err(|_| "the capability set is poisoned".to_owned())?;
    set.authorize(
        &CapabilityId::new(name),
        &Operation::execute(CALL_OPERATION),
    )
    .map_err(|error| error.to_string())
}

/// What an inline export of a core instance runs.
#[derive(Debug, Clone)]
enum Target {
    /// A function of an instance that was created on the engine.
    Alias { source: usize, export: String },
    /// An embedder function, lowered to a core function.
    Host(LoweredHost),
}

/// One core instance as it exists at run time.
#[derive(Debug)]
enum Slot {
    /// An instance created on the engine. Boxed, so the variants stay close in size.
    Runtime(Box<Instance>),
    /// An inline-exports instance: each export names what runs for it.
    Exports(Vec<BoundExport>),
}

/// One export of an inline-exports instance.
#[derive(Debug, Clone)]
struct BoundExport {
    name: String,
    target: Target,
}

/// An embedder function, as a core function: its arguments are lifted and its results lowered.
#[derive(Clone)]
struct LoweredHost {
    name: String,
    closure: ComponentFn,
    params: Vec<ComponentType>,
    result: Option<ComponentType>,
    types: Vec<ComponentTypeDef>,
    capabilities: Option<Arc<Mutex<CapabilitySet>>>,
}

impl fmt::Debug for LoweredHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LoweredHost({})", self.name)
    }
}

impl LoweredHost {
    /// Lift the core values into the parameters, run the embedder's function, and lower its
    /// result back to core values. Parameters and results pass directly, so none of them needs
    /// memory.
    fn invoke(&self, args: &[Value]) -> Result<Vec<Value>, String> {
        // The grant is checked on every call, so a revoked import stops working at once.
        authorize_call(self.capabilities.as_ref(), &self.name)?;
        let mut remaining = args.iter();
        let mut values = Vec::with_capacity(self.params.len());
        for ty in &self.params {
            values.push(lift_flats(&mut remaining, ty, &self.types)?);
        }
        if remaining.next().is_some() {
            return Err(format!(
                "the import {} was given more core values than its type has",
                self.name
            ));
        }
        let results = (self.closure)(&values)?;
        match (self.result.as_ref(), results.as_slice()) {
            (None, []) => Ok(Vec::new()),
            (Some(ty), [value]) if value_matches(value, ty, &self.types) => {
                let mut lowered = Vec::new();
                lower_flats(value, ty, &self.types, &mut lowered)?;
                Ok(lowered)
            }
            _ => Err(format!(
                "the import {} returned values that do not match its type",
                self.name
            )),
        }
    }
}

impl HostFunction for LoweredHost {
    fn function_type(&self) -> FunctionType {
        FunctionType {
            params: ResultType(
                self.params
                    .iter()
                    .flat_map(|ty| flat_of(ty, &self.types))
                    .collect(),
            ),
            results: ResultType(
                self.result
                    .as_ref()
                    .map(|ty| flat_of(ty, &self.types))
                    .unwrap_or_default(),
            ),
        }
    }

    fn call(&self, args: &[Value]) -> Result<Vec<Value>, String> {
        self.invoke(args)
    }
}

/// An exported component function, ready to call.
#[derive(Debug, Clone)]
struct LiftedExport {
    name: String,
    params: Vec<ComponentType>,
    result: Option<ComponentType>,
    core_instance: usize,
    core_export: String,
    /// The export name of the memory a string or an indirect result crosses through.
    memory: Option<String>,
    /// The export name of the `realloc` a string argument is copied in through.
    realloc: Option<String>,
    /// The encoding the strings of this function are read and written in.
    encoding: StringEncoding,
    /// The export name of the core function run after the result is read, if any.
    post_return: Option<String>,
}

/// Bind the import module `module` to the core instance at `instance`: a runtime instance
/// binds all of its exports, and an inline-exports instance binds each of its functions to
/// what runs for it.
fn bind(linker: &mut Linker, module: &str, slots: &[Slot], instance: usize) -> Result<(), String> {
    match &slots[instance] {
        Slot::Runtime(runtime) => linker
            .define_instance(module, runtime)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        Slot::Exports(bound) => {
            for export in bound {
                match &export.target {
                    Target::Alias {
                        source,
                        export: name,
                    } => {
                        let Slot::Runtime(source) = &slots[*source] else {
                            return Err("an alias names an inline-exports instance".to_owned());
                        };
                        linker
                            .define_export(module, export.name.as_str(), source, name)
                            .map(|_| ())
                            .map_err(|error| error.to_string())?;
                    }
                    Target::Host(host) => {
                        linker.define_function(module, export.name.as_str(), host.clone());
                    }
                }
            }
            Ok(())
        }
    }
}

/// The core instances of a running component, its lifted exports, and its type definitions.
#[derive(Debug)]
pub struct ComponentInstance {
    slots: Vec<Slot>,
    exports: Vec<LiftedExport>,
    types: Arc<Vec<ComponentTypeDef>>,
}

impl ComponentInstance {
    /// How many core instances the component has.
    pub fn core_instance_count(&self) -> usize {
        self.slots.len()
    }

    /// The names of the component's exports, in the order they appear.
    pub fn export_names(&self) -> Vec<&str> {
        self.exports
            .iter()
            .map(|export| export.name.as_str())
            .collect()
    }

    /// Call an exported component function. The arguments must match the export's parameter
    /// types in number and in type. A string argument is copied into the instance's memory
    /// through its `realloc`. A string result, or a record result that flattens to more than one
    /// core value, is read back from memory. The result is lifted back to the export's result
    /// type.
    pub fn call_export(
        &mut self,
        name: &str,
        arguments: Vec<ComponentValue>,
    ) -> Result<Vec<ComponentValue>, ComponentError> {
        let export = self
            .exports
            .iter()
            .find(|export| export.name == name)
            .ok_or_else(|| ComponentError::NoSuchExport {
                name: name.to_owned(),
            })?
            .clone();
        if arguments.len() != export.params.len() {
            return Err(ComponentError::ArgumentMismatch {
                export: name.to_owned(),
                message: format!(
                    "it takes {} arguments, and {} were given",
                    export.params.len(),
                    arguments.len()
                ),
            });
        }
        for (position, (expected, argument)) in export.params.iter().zip(&arguments).enumerate() {
            if !value_matches(argument, expected, &self.types) {
                return Err(ComponentError::ArgumentMismatch {
                    export: name.to_owned(),
                    message: format!(
                        "argument {position} is {argument:?}, but {expected:?} is expected"
                    ),
                });
            }
        }
        let instance = export.core_instance;
        let mut core_arguments = Vec::new();
        let flat_count: usize = export
            .params
            .iter()
            .map(|ty| flat_of(ty, &self.types).len())
            .sum();
        if flat_count > MAX_FLAT_PARAMS {
            // The parameters are stored in one block, laid out as a record of them, and the core
            // function takes a pointer to the block.
            let (offsets, size, align) = layout_of(&export.params, &self.types);
            let mut block = vec![0u8; size];
            for ((argument, ty), offset) in arguments.iter().zip(&export.params).zip(offsets) {
                store(argument, ty, &self.types, &mut block, offset)
                    .map_err(|message| ComponentError::Call { instance, message })?;
            }
            let (Some(realloc), Some(memory)) = (&export.realloc, &export.memory) else {
                return Err(ComponentError::Call {
                    instance,
                    message: "indirect parameters need a memory and a realloc".to_owned(),
                });
            };
            let allocated = self.call_core(
                instance,
                realloc,
                vec![
                    Value::I32(0),
                    Value::I32(0),
                    Value::I32(align as i32),
                    Value::I32(size as i32),
                ],
            )?;
            let pointer = match allocated.as_slice() {
                [Value::I32(pointer)] => *pointer,
                _ => {
                    return Err(ComponentError::Call {
                        instance,
                        message: "realloc did not return one i32".to_owned(),
                    })
                }
            };
            self.write_core_memory(instance, memory, u64::from(pointer as u32), &block)?;
            core_arguments.push(Value::I32(pointer));
        } else {
            for (argument, ty) in arguments.iter().zip(&export.params) {
                self.lower_argument(&export, argument, ty, &mut core_arguments)
                    .map_err(|message| ComponentError::Call { instance, message })?;
            }
        }
        let core_results = self.call_core(instance, &export.core_export, core_arguments)?;
        let lifted = match export.result {
            None if core_results.is_empty() => Ok(Vec::new()),
            None => Err(ComponentError::Call {
                instance,
                message: "the core function returned a result its type does not have".to_owned(),
            }),
            Some(ty) => {
                let value = self
                    .lift_result(&export, &ty, &core_results)
                    .map_err(|message| ComponentError::Call { instance, message })?;
                Ok(vec![value])
            }
        };
        // The post-return runs once the result has been read, with the core values the function
        // returned, so that the module may free what the result pointed into.
        if lifted.is_ok() {
            if let Some(post_return) = &export.post_return {
                self.call_core(instance, post_return, core_results)?;
            }
        }
        lifted
    }

    /// Lower one argument of type `ty` to core values, appending them. A string or a list is
    /// copied into the callee's memory with its `realloc`, and what is appended is its pointer and
    /// length.
    fn lower_argument(
        &mut self,
        export: &LiftedExport,
        argument: &ComponentValue,
        ty: &ComponentType,
        out: &mut Vec<Value>,
    ) -> Result<(), String> {
        match argument {
            ComponentValue::List(items) => {
                let ComponentType::List(index) = ty else {
                    return Err("a list argument is not a list type".to_owned());
                };
                let element = match self.types.get(*index as usize) {
                    Some(ComponentTypeDef::List(element)) => *element,
                    _ => return Err("the list is not defined".to_owned()),
                };
                let (Some(realloc), Some(memory)) = (&export.realloc, &export.memory) else {
                    return Err("a list argument needs a memory and a realloc".to_owned());
                };
                let (size, align) = element.size_and_align();
                let byte_length = items
                    .len()
                    .checked_mul(size)
                    .and_then(|bytes| i32::try_from(bytes).ok())
                    .ok_or_else(|| "a list argument is too long for a core length".to_owned())?;
                let length = i32::try_from(items.len())
                    .map_err(|_| "a list argument is too long for a core length".to_owned())?;
                // Allocate the elements in the callee's memory, as the canonical ABI does:
                // realloc(0, 0, align, byte length).
                let allocated = self
                    .call_core(
                        export.core_instance,
                        realloc,
                        vec![
                            Value::I32(0),
                            Value::I32(0),
                            Value::I32(align as i32),
                            Value::I32(byte_length),
                        ],
                    )
                    .map_err(|error| error.to_string())?;
                let pointer = match allocated.as_slice() {
                    [Value::I32(pointer)] => *pointer,
                    _ => return Err("realloc did not return one i32".to_owned()),
                };
                let mut bytes = Vec::with_capacity(byte_length as usize);
                for item in items {
                    bytes.extend(
                        scalar_bytes(item, element)
                            .ok_or_else(|| "a list element does not match its type".to_owned())?,
                    );
                }
                self.write_core_memory(
                    export.core_instance,
                    memory,
                    u64::from(pointer as u32),
                    &bytes,
                )
                .map_err(|error| error.to_string())?;
                out.push(Value::I32(pointer));
                out.push(Value::I32(length));
                Ok(())
            }
            ComponentValue::String(text) => {
                let (pointer, length) = self.lower_string(export, text)?;
                out.push(Value::I32(pointer));
                out.push(Value::I32(length));
                Ok(())
            }
            other => lower_flats(other, ty, &self.types, out),
        }
    }

    /// Lift a result from the core values the function returned. A result that flattens to more
    /// than one core value is a pointer to its canonical layout in memory.
    fn lift_result(
        &self,
        export: &LiftedExport,
        ty: &ComponentType,
        core_results: &[Value],
    ) -> Result<ComponentValue, String> {
        let flat = flat_of(ty, &self.types);
        if flat.len() > MAX_FLAT_RESULTS {
            let [Value::I32(pointer)] = core_results else {
                return Err("an indirect result did not return one pointer".to_owned());
            };
            let memory = export
                .memory
                .as_deref()
                .ok_or_else(|| "an indirect result needs a memory".to_owned())?;
            return self.load(
                export.core_instance,
                memory,
                *pointer as u32,
                ty,
                export.encoding,
            );
        }
        match (ty, core_results) {
            (ComponentType::Scalar(scalar), [value]) => lift(value.clone(), *scalar),
            // A result with no payloads is its discriminant alone, so it is a direct value.
            // A variant with no payloads is its case index alone, so it is a direct value.
            (ComponentType::Variant(index), [value]) => {
                let Some(ComponentTypeDef::Variant { cases }) = self.types.get(*index as usize)
                else {
                    return Err("the result's variant is not defined".to_owned());
                };
                if shared_payload(cases).is_some() {
                    return Err("a variant with payloads is returned through memory".to_owned());
                }
                let case = enum_case(value, cases.len())?;
                Ok(ComponentValue::Variant(cases[case].0.clone(), None))
            }
            (ComponentType::Result(index), [Value::I32(discriminant)]) => {
                let Some(ComponentTypeDef::Result { ok, err }) = self.types.get(*index as usize)
                else {
                    return Err("the result's result type is not defined".to_owned());
                };
                if ok.is_some() || err.is_some() {
                    return Err("a result with payloads is returned through memory".to_owned());
                }
                match discriminant {
                    0 => Ok(ComponentValue::Result(Ok(None))),
                    1 => Ok(ComponentValue::Result(Err(None))),
                    _ => Err("a result's discriminant is neither 0 nor 1".to_owned()),
                }
            }
            (ComponentType::Tuple(index), values) => {
                let Some(ComponentTypeDef::Tuple(scalars)) = self.types.get(*index as usize) else {
                    return Err("the result's tuple is not defined".to_owned());
                };
                if values.len() != scalars.len() {
                    return Err(
                        "the core function returned the wrong number of tuple values".to_owned(),
                    );
                }
                let mut lifted = Vec::with_capacity(scalars.len());
                for (scalar, value) in scalars.iter().zip(values) {
                    lifted.push(lift(value.clone(), *scalar)?);
                }
                Ok(ComponentValue::Tuple(lifted))
            }
            (ComponentType::Flags(index), [value]) => {
                let Some(ComponentTypeDef::Flags(labels)) = self.types.get(*index as usize) else {
                    return Err("the result's flags are not defined".to_owned());
                };
                let Value::I32(bits) = value else {
                    return Err("a flags result is not an i32".to_owned());
                };
                let bits = *bits as u32;
                // A bit beyond the labels names no flag, which the canonical ABI traps on.
                if labels.len() < 32 && bits >> labels.len() != 0 {
                    return Err(format!("{bits:#x} sets a bit that names no flag"));
                }
                let set = labels
                    .iter()
                    .enumerate()
                    .filter(|(position, _)| bits & (1u32 << position) != 0)
                    .map(|(_, label)| label.clone())
                    .collect();
                Ok(ComponentValue::Flags(set))
            }
            (ComponentType::Enum(index), [value]) => {
                let Some(ComponentTypeDef::Enum(labels)) = self.types.get(*index as usize) else {
                    return Err("the result's enum is not defined".to_owned());
                };
                let case = enum_case(value, labels.len())?;
                Ok(ComponentValue::Enum(labels[case].clone()))
            }
            (ComponentType::Record(index), values) => {
                let Some(ComponentTypeDef::Record(fields)) = self.types.get(*index as usize) else {
                    return Err("the result's record is not defined".to_owned());
                };
                if values.len() != fields.len() {
                    return Err(
                        "the core function returned the wrong number of record fields".to_owned(),
                    );
                }
                let mut lifted = Vec::with_capacity(fields.len());
                for (field, value) in fields.iter().zip(values) {
                    lifted.push((field.name.clone(), lift(value.clone(), field.ty)?));
                }
                Ok(ComponentValue::Record(lifted))
            }
            _ => Err("the core function returned the wrong number of results".to_owned()),
        }
    }

    /// Lower a string into the callee's memory in the lift's encoding, by the canonical ABI's
    /// `realloc` sequence. The result is the pointer and the length in code units, with the
    /// Latin-1 tag bit set when the string is stored as UTF-16 under `latin1+utf16`.
    fn lower_string(&mut self, export: &LiftedExport, text: &str) -> Result<(i32, i32), String> {
        let (Some(realloc), Some(memory)) = (&export.realloc, &export.memory) else {
            return Err("a string argument needs a memory and a realloc".to_owned());
        };
        let instance = export.core_instance;
        let source_units = text.len();
        if source_units > MAX_STRING_BYTE_LENGTH {
            return Err("a string argument is too long for the canonical ABI".to_owned());
        }
        let (pointer, bytes, tagged) = match export.encoding {
            StringEncoding::Utf8 => {
                let pointer = self.canon_alloc(instance, realloc, 1, source_units)?;
                (pointer, text.as_bytes().to_vec(), source_units as u32)
            }
            StringEncoding::Utf16 => {
                let worst = 2 * source_units;
                let mut pointer = self.canon_alloc(instance, realloc, 2, worst)?;
                let encoded = utf16_bytes(text);
                if encoded.len() < worst {
                    pointer =
                        self.canon_realloc(instance, realloc, pointer, worst, 2, encoded.len())?;
                }
                let units = (encoded.len() / 2) as u32;
                (pointer, encoded, units)
            }
            StringEncoding::Latin1Utf16 => {
                let mut pointer = self.canon_alloc(instance, realloc, 2, source_units)?;
                if text.chars().all(|c| (c as u32) < 256) {
                    let latin1: Vec<u8> = text.chars().map(|c| c as u8).collect();
                    if latin1.len() < source_units {
                        pointer = self.canon_realloc(
                            instance,
                            realloc,
                            pointer,
                            source_units,
                            2,
                            latin1.len(),
                        )?;
                    }
                    let units = latin1.len() as u32;
                    (pointer, latin1, units)
                } else {
                    let worst = 2 * source_units;
                    pointer =
                        self.canon_realloc(instance, realloc, pointer, source_units, 2, worst)?;
                    let encoded = utf16_bytes(text);
                    if worst > encoded.len() {
                        pointer = self.canon_realloc(
                            instance,
                            realloc,
                            pointer,
                            worst,
                            2,
                            encoded.len(),
                        )?;
                    }
                    let units = (encoded.len() / 2) as u32 | UTF16_TAG;
                    (pointer, encoded, units)
                }
            }
        };
        self.write_core_memory(instance, memory, u64::from(pointer as u32), &bytes)
            .map_err(|error| error.to_string())?;
        Ok((pointer, tagged as i32))
    }

    /// `realloc(0, 0, align, size)`, which allocates a block in the callee's memory.
    fn canon_alloc(
        &mut self,
        instance: usize,
        realloc: &str,
        align: usize,
        size: usize,
    ) -> Result<i32, String> {
        self.canon_call(
            instance,
            realloc,
            vec![
                Value::I32(0),
                Value::I32(0),
                Value::I32(align as i32),
                Value::I32(size as i32),
            ],
        )
    }

    /// `realloc(pointer, old size, align, new size)`, which resizes a block in the callee's memory.
    fn canon_realloc(
        &mut self,
        instance: usize,
        realloc: &str,
        pointer: i32,
        old_size: usize,
        align: usize,
        new_size: usize,
    ) -> Result<i32, String> {
        self.canon_call(
            instance,
            realloc,
            vec![
                Value::I32(pointer),
                Value::I32(old_size as i32),
                Value::I32(align as i32),
                Value::I32(new_size as i32),
            ],
        )
    }

    fn canon_call(
        &mut self,
        instance: usize,
        realloc: &str,
        arguments: Vec<Value>,
    ) -> Result<i32, String> {
        let allocated = self
            .call_core(instance, realloc, arguments)
            .map_err(|error| error.to_string())?;
        match allocated.as_slice() {
            [Value::I32(pointer)] => Ok(*pointer),
            _ => Err("realloc did not return one i32".to_owned()),
        }
    }

    /// Read a string of the lift's encoding at `pointer`, whose length is `tagged_length` code
    /// units, as the canonical ABI reads it: under `latin1+utf16` the tag bit selects UTF-16, and a
    /// string must be aligned, within the length limit, and valid in its encoding.
    fn load_string(
        &self,
        instance: usize,
        memory: &str,
        pointer: u32,
        tagged_length: u32,
        encoding: StringEncoding,
    ) -> Result<String, String> {
        let (byte_length, alignment, decoding) = match encoding {
            StringEncoding::Utf8 => (u64::from(tagged_length), 1, StringDecoding::Utf8),
            StringEncoding::Utf16 => (2 * u64::from(tagged_length), 2, StringDecoding::Utf16),
            StringEncoding::Latin1Utf16 if tagged_length & UTF16_TAG != 0 => (
                2 * u64::from(tagged_length ^ UTF16_TAG),
                2,
                StringDecoding::Utf16,
            ),
            StringEncoding::Latin1Utf16 => (u64::from(tagged_length), 2, StringDecoding::Latin1),
        };
        if byte_length > MAX_STRING_BYTE_LENGTH as u64 {
            return Err("the string is too long for the canonical ABI".to_owned());
        }
        if !(pointer as usize).is_multiple_of(alignment) {
            return Err("the string is not aligned for its encoding".to_owned());
        }
        let bytes = self
            .read_core_memory(instance, memory, u64::from(pointer), byte_length as usize)
            .map_err(|error| error.to_string())?;
        match decoding {
            StringDecoding::Utf8 => {
                String::from_utf8(bytes).map_err(|_| "the string is not valid UTF-8".to_owned())
            }
            StringDecoding::Utf16 => {
                // A byte length of a UTF-16 string is even, so every code unit is whole.
                let (pairs, _) = bytes.as_chunks::<2>();
                let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
                String::from_utf16(&units).map_err(|_| "the string is not valid UTF-16".to_owned())
            }
            StringDecoding::Latin1 => Ok(bytes.iter().map(|&byte| char::from(byte)).collect()),
        }
    }

    /// Load a value of type `ty` at `pointer` in the memory `memory` of core instance `instance`,
    /// by the canonical layout. A string is read in `encoding`.
    fn load(
        &self,
        instance: usize,
        memory: &str,
        pointer: u32,
        ty: &ComponentType,
        encoding: StringEncoding,
    ) -> Result<ComponentValue, String> {
        match ty {
            ComponentType::Scalar(Scalar::String) => {
                let header = self
                    .read_core_memory(instance, memory, u64::from(pointer), 8)
                    .map_err(|error| error.to_string())?;
                let data_pointer = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
                let tagged_length =
                    u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
                self.load_string(instance, memory, data_pointer, tagged_length, encoding)
                    .map(ComponentValue::String)
            }
            ComponentType::Scalar(scalar) => {
                self.load_scalar(instance, memory, u64::from(pointer), *scalar)
            }
            ComponentType::Tuple(index) => {
                let Some(ComponentTypeDef::Tuple(scalars)) = self.types.get(*index as usize) else {
                    return Err("the tuple is not defined".to_owned());
                };
                // A tuple lays out like a record whose fields have no labels.
                let fields: Vec<RecordField> = scalars
                    .iter()
                    .map(|ty| RecordField {
                        name: String::new(),
                        ty: *ty,
                    })
                    .collect();
                let (offsets, _, _) = record_layout(&fields);
                let mut lifted = Vec::with_capacity(fields.len());
                for (field, offset) in fields.iter().zip(offsets) {
                    lifted.push(self.load_scalar(
                        instance,
                        memory,
                        u64::from(pointer) + offset as u64,
                        field.ty,
                    )?);
                }
                Ok(ComponentValue::Tuple(lifted))
            }
            ComponentType::Enum(_) | ComponentType::Flags(_) => {
                // An enum is only ever a direct value, so it is never read from memory.
                Err("an enum is not read from memory".to_owned())
            }
            ComponentType::Option(index) => {
                let Some(ComponentTypeDef::Option(payload)) = self.types.get(*index as usize)
                else {
                    return Err("the option is not defined".to_owned());
                };
                let payload = *payload;
                // The discriminant is one byte; the payload follows at its aligned offset.
                let discriminant = self
                    .read_core_memory(instance, memory, u64::from(pointer), 1)
                    .map_err(|error| error.to_string())?[0];
                let (_, payload_align) = payload.size_and_align();
                let payload_offset = align_to(1, payload_align) as u64;
                match discriminant {
                    0 => Ok(ComponentValue::Option(None)),
                    1 => {
                        let value = self.load_scalar(
                            instance,
                            memory,
                            u64::from(pointer) + payload_offset,
                            payload,
                        )?;
                        Ok(ComponentValue::Option(Some(Box::new(value))))
                    }
                    _ => Err("an option's discriminant is neither 0 nor 1".to_owned()),
                }
            }
            ComponentType::Variant(index) => {
                let Some(ComponentTypeDef::Variant { cases }) = self.types.get(*index as usize)
                else {
                    return Err("the variant is not defined".to_owned());
                };
                // The discriminant is one byte, and the payload follows at the offset its
                // widest case needs.
                let discriminant = self
                    .read_core_memory(instance, memory, u64::from(pointer), 1)
                    .map_err(|error| error.to_string())?[0];
                let Some((label, case_type)) = cases.get(usize::from(discriminant)) else {
                    return Err("a variant's discriminant names no case".to_owned());
                };
                let payload = match case_type {
                    None => None,
                    Some(scalar) => {
                        let align = cases
                            .iter()
                            .filter_map(|(_, payload)| *payload)
                            .map(|payload| payload.size_and_align().1)
                            .max()
                            .unwrap_or(1);
                        let offset = u64::from(pointer) + align_to(1, align) as u64;
                        Some(Box::new(
                            self.load_scalar(instance, memory, offset, *scalar)?,
                        ))
                    }
                };
                Ok(ComponentValue::Variant(label.clone(), payload))
            }
            ComponentType::Result(index) => {
                let Some(ComponentTypeDef::Result { ok, err }) = self.types.get(*index as usize)
                else {
                    return Err("the result is not defined".to_owned());
                };
                let (ok, err) = (*ok, *err);
                // The discriminant is one byte, and the payload follows at its aligned offset.
                let discriminant = self
                    .read_core_memory(instance, memory, u64::from(pointer), 1)
                    .map_err(|error| error.to_string())?[0];
                let case_type = match discriminant {
                    0 => ok,
                    1 => err,
                    _ => return Err("a result's discriminant is neither 0 nor 1".to_owned()),
                };
                let payload = match case_type {
                    None => None,
                    Some(scalar) => {
                        let (_, align) = scalar.size_and_align();
                        let offset = u64::from(pointer) + align_to(1, align) as u64;
                        Some(Box::new(
                            self.load_scalar(instance, memory, offset, scalar)?,
                        ))
                    }
                };
                Ok(ComponentValue::Result(if discriminant == 0 {
                    Ok(payload)
                } else {
                    Err(payload)
                }))
            }
            ComponentType::List(index) => {
                let Some(ComponentTypeDef::List(element)) = self.types.get(*index as usize) else {
                    return Err("the list is not defined".to_owned());
                };
                let element = *element;
                // The list is its pointer and its length, and its elements are at the pointer.
                let header = self
                    .read_core_memory(instance, memory, u64::from(pointer), 8)
                    .map_err(|error| error.to_string())?;
                let data_pointer = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
                let data_length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
                let (size, _) = element.size_and_align();
                let byte_length = (data_length as usize)
                    .checked_mul(size)
                    .ok_or_else(|| "the list is too long".to_owned())?;
                let bytes = self
                    .read_core_memory(instance, memory, u64::from(data_pointer), byte_length)
                    .map_err(|error| error.to_string())?;
                let mut items = Vec::new();
                for chunk in bytes.chunks_exact(size) {
                    items.push(scalar_from_bytes(element, chunk)?);
                }
                Ok(ComponentValue::List(items))
            }
            ComponentType::Record(index) => {
                let Some(ComponentTypeDef::Record(fields)) = self.types.get(*index as usize) else {
                    return Err("the record is not defined".to_owned());
                };
                let (offsets, _, _) = record_layout(fields);
                let mut lifted = Vec::with_capacity(fields.len());
                for (field, offset) in fields.iter().zip(offsets) {
                    let value = self.load_scalar(
                        instance,
                        memory,
                        u64::from(pointer) + offset as u64,
                        field.ty,
                    )?;
                    lifted.push((field.name.clone(), value));
                }
                Ok(ComponentValue::Record(lifted))
            }
        }
    }

    /// Load one scalar at `offset` in memory. The bytes are read little-endian, zero-extended,
    /// and lifted as the scalar's core value.
    fn load_scalar(
        &self,
        instance: usize,
        memory: &str,
        offset: u64,
        scalar: Scalar,
    ) -> Result<ComponentValue, String> {
        let (size, _) = scalar.size_and_align();
        let bytes = self
            .read_core_memory(instance, memory, offset, size)
            .map_err(|error| error.to_string())?;
        scalar_from_bytes(scalar, &bytes)
    }

    /// Read `length` bytes at `offset` from the memory `memory` of core instance `instance`.
    fn read_core_memory(
        &self,
        instance: usize,
        memory: &str,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, ComponentError> {
        match self.slots.get(instance) {
            Some(Slot::Runtime(runtime)) => {
                runtime
                    .read_memory(memory, offset, length)
                    .map_err(|error| ComponentError::Call {
                        instance,
                        message: error.to_string(),
                    })
            }
            _ => Err(ComponentError::Call {
                instance,
                message: "the memory's instance is not a runtime instance".to_owned(),
            }),
        }
    }

    /// Write `bytes` at `offset` in the memory `memory` of core instance `instance`.
    fn write_core_memory(
        &self,
        instance: usize,
        memory: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), ComponentError> {
        match self.slots.get(instance) {
            Some(Slot::Runtime(runtime)) => {
                runtime
                    .write_memory(memory, offset, bytes)
                    .map_err(|error| ComponentError::Call {
                        instance,
                        message: error.to_string(),
                    })
            }
            _ => Err(ComponentError::Call {
                instance,
                message: "the memory's instance is not a runtime instance".to_owned(),
            }),
        }
    }

    /// Call an export of core instance `instance`. For an inline-exports instance, the call
    /// runs what the export names: a function of its defining instance, or an embedder function.
    pub fn call_core(
        &mut self,
        instance: usize,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Vec<Value>, ComponentError> {
        let target = match self
            .slots
            .get(instance)
            .ok_or(ComponentError::NoSuchInstance { instance })?
        {
            Slot::Runtime(_) => Target::Alias {
                source: instance,
                export: name.to_owned(),
            },
            Slot::Exports(bound) => bound
                .iter()
                .find(|export| export.name == name)
                .map(|export| export.target.clone())
                .ok_or_else(|| ComponentError::Call {
                    instance,
                    message: format!("there is no export named {name}"),
                })?,
        };
        match target {
            Target::Alias { source, export } => {
                let Slot::Runtime(runtime) = &mut self.slots[source] else {
                    return Err(ComponentError::Call {
                        instance,
                        message: "the export's source is not a runtime instance".to_owned(),
                    });
                };
                runtime
                    .call(&export, args)
                    .map_err(|error| ComponentError::Call {
                        instance,
                        message: error.to_string(),
                    })
            }
            Target::Host(host) => host
                .invoke(&args)
                .map_err(|message| ComponentError::Call { instance, message }),
        }
    }
}

/// Whether `value` has the component type `ty`. A record matches a record of the same fields, in
/// the same order, with matching field types.
fn value_matches(value: &ComponentValue, ty: &ComponentType, types: &[ComponentTypeDef]) -> bool {
    match (value, ty) {
        (ComponentValue::Record(fields), ComponentType::Record(index)) => {
            let Some(ComponentTypeDef::Record(defined)) = types.get(*index as usize) else {
                return false;
            };
            fields.len() == defined.len()
                && fields.iter().zip(defined).all(|((name, value), field)| {
                    *name == field.name && value.scalar() == Some(field.ty)
                })
        }
        (ComponentValue::Tuple(items), ComponentType::Tuple(index)) => {
            match types.get(*index as usize) {
                Some(ComponentTypeDef::Tuple(scalars)) => {
                    items.len() == scalars.len()
                        && items
                            .iter()
                            .zip(scalars)
                            .all(|(item, scalar)| item.scalar() == Some(*scalar))
                }
                _ => false,
            }
        }
        (ComponentValue::Flags(set), ComponentType::Flags(index)) => {
            match types.get(*index as usize) {
                Some(ComponentTypeDef::Flags(defined)) => {
                    set.iter().all(|label| defined.contains(label))
                }
                _ => false,
            }
        }
        (ComponentValue::Enum(label), ComponentType::Enum(index)) => {
            matches!(types.get(*index as usize), Some(ComponentTypeDef::Enum(labels)) if labels.contains(label))
        }
        (ComponentValue::Option(payload), ComponentType::Option(index)) => {
            let Some(ComponentTypeDef::Option(element)) = types.get(*index as usize) else {
                return false;
            };
            match payload {
                None => true,
                Some(value) => value.scalar() == Some(*element),
            }
        }
        (ComponentValue::List(items), ComponentType::List(index)) => {
            let Some(ComponentTypeDef::List(element)) = types.get(*index as usize) else {
                return false;
            };
            items.iter().all(|item| item.scalar() == Some(*element))
        }
        (ComponentValue::Variant(label, payload), ComponentType::Variant(index)) => {
            let Some(ComponentTypeDef::Variant { cases }) = types.get(*index as usize) else {
                return false;
            };
            match cases.iter().find(|(name, _)| name == label) {
                Some((_, None)) => payload.is_none(),
                Some((_, Some(scalar))) => payload
                    .as_deref()
                    .is_some_and(|value| value.scalar() == Some(*scalar)),
                None => false,
            }
        }
        (ComponentValue::Result(outcome), ComponentType::Result(index)) => {
            let Some(ComponentTypeDef::Result { ok, err }) = types.get(*index as usize) else {
                return false;
            };
            let (payload, case) = match outcome {
                Ok(payload) => (payload, ok),
                Err(payload) => (payload, err),
            };
            match (payload, case) {
                (None, None) => true,
                (Some(value), Some(scalar)) => value.scalar() == Some(*scalar),
                _ => false,
            }
        }
        (value, ComponentType::Scalar(scalar)) => value.scalar() == Some(*scalar),
        _ => false,
    }
}

/// The little-endian bytes of one list element of scalar type `element`, or `None` if the value is
/// not of that type.
fn scalar_bytes(value: &ComponentValue, element: Scalar) -> Option<Vec<u8>> {
    if value.scalar() != Some(element) {
        return None;
    }
    let (size, _) = element.size_and_align();
    match lower_scalar(value)? {
        Value::I32(value) => Some(value.to_le_bytes()[..size].to_vec()),
        Value::I64(value) => Some(value.to_le_bytes().to_vec()),
        Value::F32(bits) => Some(bits.to_le_bytes().to_vec()),
        Value::F64(bits) => Some(bits.to_le_bytes().to_vec()),
        _ => None,
    }
}

/// The core value a scalar lowers to, or `None` for a string, a record, or a list, which are not
/// single scalars. Signed and unsigned values share their bits.
fn lower_scalar(value: &ComponentValue) -> Option<Value> {
    match value {
        ComponentValue::S32(value) => Some(Value::I32(*value)),
        ComponentValue::U32(value) => Some(Value::I32(*value as i32)),
        ComponentValue::S64(value) => Some(Value::I64(*value)),
        ComponentValue::U64(value) => Some(Value::I64(*value as i64)),
        ComponentValue::F32(value) => Some(Value::F32(value.to_bits())),
        ComponentValue::F64(value) => Some(Value::F64(value.to_bits())),
        ComponentValue::Bool(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::S8(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::U8(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::S16(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::U16(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::Char(value) => Some(Value::I32(*value as u32 as i32)),
        ComponentValue::String(_)
        | ComponentValue::Record(_)
        | ComponentValue::List(_)
        | ComponentValue::Option(_)
        | ComponentValue::Enum(_)
        | ComponentValue::Flags(_)
        | ComponentValue::Tuple(_)
        | ComponentValue::Result(_)
        | ComponentValue::Variant(..) => None,
    }
}

/// The case index an enum's core value names, which must be one of its `count` cases.
fn enum_case(value: &Value, count: usize) -> Result<usize, String> {
    match value {
        Value::I32(index) if (*index as u32 as usize) < count => Ok(*index as u32 as usize),
        other => Err(format!(
            "{other:?} is not a case of an enum of {count} cases"
        )),
    }
}

/// The zero core value of a flat type: what an option's payload is when the option is `None`.
fn zero_value(ty: ValueType) -> Value {
    match ty {
        ValueType::I64 => Value::I64(0),
        ValueType::F32 => Value::F32(0),
        ValueType::F64 => Value::F64(0),
        _ => Value::I32(0),
    }
}

/// The scalar that `bytes`, read little-endian and zero-extended, lifts to. `bytes` is exactly the
/// scalar's size in memory.
fn scalar_from_bytes(scalar: Scalar, bytes: &[u8]) -> Result<ComponentValue, String> {
    let (size, _) = scalar.size_and_align();
    if bytes.len() != size {
        return Err(format!("{} bytes do not hold a {scalar:?}", bytes.len()));
    }
    let mut buffer = [0u8; 8];
    buffer[..size].copy_from_slice(bytes);
    let value = match scalar {
        Scalar::S64 | Scalar::U64 => Value::I64(i64::from_le_bytes(buffer)),
        Scalar::F64 => Value::F64(u64::from_le_bytes(buffer)),
        Scalar::F32 => Value::F32(u32::from_le_bytes([
            buffer[0], buffer[1], buffer[2], buffer[3],
        ])),
        _ => Value::I32(i32::from_le_bytes([
            buffer[0], buffer[1], buffer[2], buffer[3],
        ])),
    };
    lift(value, scalar)
}

/// The scalar a core value lifts to, when the core value has the scalar's type. A string is not
/// lifted from a single value: it is read from memory.
fn lift(value: Value, scalar: Scalar) -> Result<ComponentValue, String> {
    match (scalar, value) {
        (Scalar::S32, Value::I32(value)) => Ok(ComponentValue::S32(value)),
        (Scalar::U32, Value::I32(value)) => Ok(ComponentValue::U32(value as u32)),
        (Scalar::S64, Value::I64(value)) => Ok(ComponentValue::S64(value)),
        (Scalar::U64, Value::I64(value)) => Ok(ComponentValue::U64(value as u64)),
        (Scalar::F32, Value::F32(bits)) => Ok(ComponentValue::F32(f32::from_bits(bits))),
        (Scalar::F64, Value::F64(bits)) => Ok(ComponentValue::F64(f64::from_bits(bits))),
        // A small integer keeps the low bits of the core value, and a signed one sign-extends
        // them, as the canonical ABI's lift does. A boolean is true for any non-zero value.
        (Scalar::Bool, Value::I32(value)) => Ok(ComponentValue::Bool(value != 0)),
        (Scalar::S8, Value::I32(value)) => Ok(ComponentValue::S8(value as i8)),
        (Scalar::U8, Value::I32(value)) => Ok(ComponentValue::U8(value as u8)),
        (Scalar::S16, Value::I32(value)) => Ok(ComponentValue::S16(value as i16)),
        (Scalar::U16, Value::I32(value)) => Ok(ComponentValue::U16(value as u16)),
        (Scalar::Char, Value::I32(value)) => char::from_u32(value as u32)
            .map(ComponentValue::Char)
            .ok_or_else(|| format!("{value:#x} is not a Unicode scalar value")),
        (scalar, value) => Err(format!(
            "a core value {value:?} does not lift to {scalar:?}"
        )),
    }
}

/// Decode an alias section's contents: a count, then each alias. An alias takes an export of a
/// core instance that is a module instance: a function defines a core function, and a memory
/// defines a core memory. The other forms are refused.
fn decode_aliases(
    reader: &mut Reader<'_>,
    instances: &[CoreInstance],
    functions: &mut Vec<CoreFunction>,
    memories: &mut Vec<CoreMemory>,
) -> Result<(), ComponentError> {
    let count = reader.u32()?;
    for _ in 0..count {
        let at = reader.position();
        if reader.byte()? != SORT_CORE_WRAPPER {
            return Err(ComponentError::UnsupportedAlias {
                what: "alias of a non-core sort",
                offset: at,
            });
        }
        let sort = reader.byte()?;
        if sort != SORT_FUNC && sort != SORT_CORE_MEMORY {
            return Err(ComponentError::UnsupportedAlias {
                what: "alias of a core sort other than function or memory",
                offset: at,
            });
        }
        if reader.byte()? != ALIAS_CORE_EXPORT {
            return Err(ComponentError::UnsupportedAlias {
                what: "outer alias",
                offset: at,
            });
        }
        let instance = reader.u32()?;
        let export = reader.name()?;
        let source = instances
            .get(instance as usize)
            .ok_or(ComponentError::NoSuchInstance {
                instance: instance as usize,
            })?;
        if let CoreInstance::FromExports { .. } = source {
            return Err(ComponentError::UnsupportedAlias {
                what: "alias of an inline-exports instance",
                offset: at,
            });
        }
        if sort == SORT_FUNC {
            functions.push(CoreFunction::Alias { instance, export });
        } else {
            memories.push(CoreMemory { instance, export });
        }
    }
    Ok(())
}

/// Decode a core-instance section's contents: a count, then each instance expression, which
/// are appended to `instances`. An argument or an inline export may name only what is
/// already declared, so every index is checked against what is there.
fn decode_instances(
    reader: &mut Reader<'_>,
    functions: &[CoreFunction],
    instances: &mut Vec<CoreInstance>,
) -> Result<(), ComponentError> {
    let count = reader.u32()?;
    for _ in 0..count {
        let at = reader.position();
        match reader.byte()? {
            INSTANTIATE => {
                let module = reader.u32()?;
                let argument_count = reader.u32()?;
                let mut arguments: Vec<InstanceArgument> = Vec::new();
                for _ in 0..argument_count {
                    let name = reader.name()?;
                    let tag_at = reader.position();
                    if reader.byte()? != SORT_CORE_INSTANCE {
                        return Err(ComponentError::Malformed { offset: tag_at });
                    }
                    let instance = reader.u32()?;
                    if instance as usize >= instances.len() {
                        return Err(ComponentError::NoSuchInstance {
                            instance: instance as usize,
                        });
                    }
                    if arguments.iter().any(|argument| argument.name == name) {
                        return Err(ComponentError::Malformed { offset: tag_at });
                    }
                    arguments.push(InstanceArgument { name, instance });
                }
                instances.push(CoreInstance::Instantiate { module, arguments });
            }
            INLINE_EXPORTS => {
                let export_count = reader.u32()?;
                let mut exports: Vec<InlineExport> = Vec::new();
                for _ in 0..export_count {
                    let name = reader.name()?;
                    let sort_at = reader.position();
                    if reader.byte()? != SORT_FUNC {
                        return Err(ComponentError::UnsupportedInstance {
                            what: "inline export of a sort other than function",
                            offset: sort_at,
                        });
                    }
                    let function = reader.u32()?;
                    if function as usize >= functions.len() {
                        return Err(ComponentError::NoSuchFunction { function });
                    }
                    if exports.iter().any(|export| export.name == name) {
                        return Err(ComponentError::Malformed { offset: sort_at });
                    }
                    exports.push(InlineExport { name, function });
                }
                instances.push(CoreInstance::FromExports { exports });
            }
            _ => return Err(ComponentError::Malformed { offset: at }),
        }
    }
    Ok(())
}

/// Decode a scalar value type byte at `offset`, refusing every type this crate does not support.
fn scalar(byte: u8, offset: usize) -> Result<Scalar, ComponentError> {
    match byte {
        VALTYPE_S32 => Ok(Scalar::S32),
        VALTYPE_U32 => Ok(Scalar::U32),
        VALTYPE_S64 => Ok(Scalar::S64),
        VALTYPE_U64 => Ok(Scalar::U64),
        VALTYPE_F32 => Ok(Scalar::F32),
        VALTYPE_F64 => Ok(Scalar::F64),
        VALTYPE_STRING => Ok(Scalar::String),
        VALTYPE_BOOL => Ok(Scalar::Bool),
        VALTYPE_S8 => Ok(Scalar::S8),
        VALTYPE_U8 => Ok(Scalar::U8),
        VALTYPE_S16 => Ok(Scalar::S16),
        VALTYPE_U16 => Ok(Scalar::U16),
        VALTYPE_CHAR => Ok(Scalar::Char),
        _ => Err(ComponentError::UnsupportedType {
            what: "a value type this crate does not support",
            offset,
        }),
    }
}

/// Decode a value type at the reader's position. A primitive is one byte with its high bit
/// pattern set; any other value is the index of a type defined earlier, as an unsigned LEB128.
fn value_type(
    reader: &mut Reader<'_>,
    types: &[ComponentTypeDef],
) -> Result<ComponentType, ComponentError> {
    let at = reader.position();
    let first = reader.byte()?;
    if first & 0x40 != 0 && first & 0x80 == 0 {
        return Ok(ComponentType::Scalar(scalar(first, at)?));
    }
    reader.unread();
    let index = reader.u32()?;
    match types.get(index as usize) {
        Some(ComponentTypeDef::Record(_)) => Ok(ComponentType::Record(index)),
        Some(ComponentTypeDef::List(_)) => Ok(ComponentType::List(index)),
        Some(ComponentTypeDef::Option(_)) => Ok(ComponentType::Option(index)),
        Some(ComponentTypeDef::Enum(_)) => Ok(ComponentType::Enum(index)),
        Some(ComponentTypeDef::Flags(_)) => Ok(ComponentType::Flags(index)),
        Some(ComponentTypeDef::Tuple(_)) => Ok(ComponentType::Tuple(index)),
        Some(ComponentTypeDef::Result { .. }) => Ok(ComponentType::Result(index)),
        Some(ComponentTypeDef::Variant { .. }) => Ok(ComponentType::Variant(index)),
        Some(ComponentTypeDef::Func(_)) => Err(ComponentError::UnsupportedType {
            what: "a function type used as a value",
            offset: at,
        }),
        None => Err(ComponentError::NoSuchType { index }),
    }
}

/// Decode a type section's contents: a count, then each type definition. A function type is its
/// tag, a labelled parameter list, and a result: `0x00` and a value type, or `0x01 0x00` for none.
/// A record is its tag, then its labelled fields, each a scalar; a record field may not be a
/// string, a record, or another compound type.
fn decode_types(
    reader: &mut Reader<'_>,
    types: &mut Vec<ComponentTypeDef>,
) -> Result<(), ComponentError> {
    let count = reader.u32()?;
    for _ in 0..count {
        let at = reader.position();
        match reader.byte()? {
            TYPE_FUNC => {
                let parameter_count = reader.u32()?;
                let mut params = Vec::new();
                for _ in 0..parameter_count {
                    let _label = reader.name()?;
                    params.push(value_type(reader, types)?);
                }
                let at_result = reader.position();
                let result = match reader.byte()? {
                    0x00 => Some(value_type(reader, types)?),
                    0x01 => {
                        if reader.byte()? != 0x00 {
                            return Err(ComponentError::Malformed { offset: at_result });
                        }
                        None
                    }
                    _ => return Err(ComponentError::Malformed { offset: at_result }),
                };
                types.push(ComponentTypeDef::Func(ComponentFuncType { params, result }));
            }
            TYPE_RECORD => {
                let field_count = reader.u32()?;
                if field_count == 0 {
                    return Err(ComponentError::Malformed { offset: at });
                }
                let mut fields = Vec::new();
                for _ in 0..field_count {
                    let name = reader.name()?;
                    let at_field = reader.position();
                    match value_type(reader, types)? {
                        ComponentType::Scalar(Scalar::String) => {
                            return Err(ComponentError::UnsupportedType {
                                what: "a string in a record",
                                offset: at_field,
                            })
                        }
                        ComponentType::Scalar(ty) => fields.push(RecordField { name, ty }),
                        ComponentType::Option(_)
                        | ComponentType::Enum(_)
                        | ComponentType::Flags(_)
                        | ComponentType::Tuple(_)
                        | ComponentType::Result(_)
                        | ComponentType::Variant(_) => {
                            return Err(ComponentError::UnsupportedType {
                                what: "an option or an enum in a record",
                                offset: at_field,
                            })
                        }
                        ComponentType::List(_) => {
                            return Err(ComponentError::UnsupportedType {
                                what: "a list in a record",
                                offset: at_field,
                            })
                        }
                        ComponentType::Record(_) => {
                            return Err(ComponentError::UnsupportedType {
                                what: "a record in a record",
                                offset: at_field,
                            })
                        }
                    }
                }
                types.push(ComponentTypeDef::Record(fields));
            }
            TYPE_OPTION => {
                let payload_at = reader.position();
                let payload = match value_type(reader, types)? {
                    ComponentType::Scalar(Scalar::String) => {
                        return Err(ComponentError::UnsupportedType {
                            what: "an option of a string",
                            offset: payload_at,
                        })
                    }
                    ComponentType::Scalar(scalar) => scalar,
                    ComponentType::Record(_)
                    | ComponentType::List(_)
                    | ComponentType::Option(_)
                    | ComponentType::Enum(_)
                    | ComponentType::Flags(_)
                    | ComponentType::Tuple(_)
                    | ComponentType::Result(_)
                    | ComponentType::Variant(_) => {
                        return Err(ComponentError::UnsupportedType {
                            what: "an option of a record, list, or option",
                            offset: payload_at,
                        })
                    }
                };
                types.push(ComponentTypeDef::Option(payload));
            }
            TYPE_TUPLE => {
                let count = reader.u32()?;
                if count == 0 {
                    return Err(ComponentError::Malformed { offset: at });
                }
                let mut scalars = Vec::new();
                for _ in 0..count {
                    let at_type = reader.position();
                    match value_type(reader, types)? {
                        ComponentType::Scalar(Scalar::String) => {
                            return Err(ComponentError::UnsupportedType {
                                what: "a string in a tuple",
                                offset: at_type,
                            })
                        }
                        ComponentType::Scalar(scalar) => scalars.push(scalar),
                        _ => {
                            return Err(ComponentError::UnsupportedType {
                                what: "a compound type in a tuple",
                                offset: at_type,
                            })
                        }
                    }
                }
                types.push(ComponentTypeDef::Tuple(scalars));
            }
            TYPE_FLAGS => {
                let label_count = reader.u32()?;
                if label_count == 0 || label_count > 32 {
                    return Err(ComponentError::Malformed { offset: at });
                }
                let mut labels: Vec<String> = Vec::new();
                for _ in 0..label_count {
                    let at_label = reader.position();
                    let label = reader.name()?;
                    if labels.contains(&label) {
                        return Err(ComponentError::Malformed { offset: at_label });
                    }
                    labels.push(label);
                }
                types.push(ComponentTypeDef::Flags(labels));
            }
            TYPE_ENUM => {
                let case_count = reader.u32()?;
                if case_count == 0 {
                    return Err(ComponentError::Malformed { offset: at });
                }
                let mut labels: Vec<String> = Vec::new();
                for _ in 0..case_count {
                    let at_label = reader.position();
                    let label = reader.name()?;
                    if labels.contains(&label) {
                        return Err(ComponentError::Malformed { offset: at_label });
                    }
                    labels.push(label);
                }
                types.push(ComponentTypeDef::Enum(labels));
            }
            TYPE_RESULT => {
                // Each case is `0x00` for no payload, or `0x01` and a value type.
                let mut cases = [None, None];
                for case in &mut cases {
                    let at_case = reader.position();
                    match reader.byte()? {
                        0x00 => {}
                        0x01 => {
                            let at_payload = reader.position();
                            *case = Some(match value_type(reader, types)? {
                                ComponentType::Scalar(Scalar::String) => {
                                    return Err(ComponentError::UnsupportedType {
                                        what: "a string in a result",
                                        offset: at_payload,
                                    })
                                }
                                ComponentType::Scalar(scalar) => scalar,
                                _ => {
                                    return Err(ComponentError::UnsupportedType {
                                        what: "a compound type in a result",
                                        offset: at_payload,
                                    })
                                }
                            });
                        }
                        _ => return Err(ComponentError::Malformed { offset: at_case }),
                    }
                }
                let [ok, err] = cases;
                types.push(ComponentTypeDef::Result { ok, err });
            }
            TYPE_VARIANT => {
                let case_count = reader.u32()?;
                if case_count == 0 {
                    return Err(ComponentError::Malformed { offset: at });
                }
                if case_count > 256 {
                    return Err(ComponentError::UnsupportedType {
                        what: "a variant of more than 256 cases",
                        offset: at,
                    });
                }
                let mut cases: Vec<(String, Option<Scalar>)> = Vec::new();
                for _ in 0..case_count {
                    let at_label = reader.position();
                    let label = reader.name()?;
                    if cases.iter().any(|(defined, _)| *defined == label) {
                        return Err(ComponentError::Malformed { offset: at_label });
                    }
                    let at_case = reader.position();
                    let payload = match reader.byte()? {
                        0x00 => None,
                        0x01 => {
                            let at_payload = reader.position();
                            match value_type(reader, types)? {
                                ComponentType::Scalar(Scalar::String) => {
                                    return Err(ComponentError::UnsupportedType {
                                        what: "a string in a variant",
                                        offset: at_payload,
                                    })
                                }
                                ComponentType::Scalar(scalar) => Some(scalar),
                                _ => {
                                    return Err(ComponentError::UnsupportedType {
                                        what: "a compound type in a variant",
                                        offset: at_payload,
                                    })
                                }
                            }
                        }
                        _ => return Err(ComponentError::Malformed { offset: at_case }),
                    };
                    // The case's refines field is `0x00`; a refining case is not supported.
                    let at_refines = reader.position();
                    if reader.byte()? != 0x00 {
                        return Err(ComponentError::Malformed { offset: at_refines });
                    }
                    cases.push((label, payload));
                }
                types.push(ComponentTypeDef::Variant { cases });
            }
            TYPE_LIST => {
                let element_at = reader.position();
                let element = match value_type(reader, types)? {
                    ComponentType::Scalar(Scalar::String) => {
                        return Err(ComponentError::UnsupportedType {
                            what: "a list of strings",
                            offset: element_at,
                        })
                    }
                    ComponentType::Scalar(scalar) => scalar,
                    ComponentType::Record(_)
                    | ComponentType::List(_)
                    | ComponentType::Option(_)
                    | ComponentType::Enum(_)
                    | ComponentType::Flags(_)
                    | ComponentType::Tuple(_)
                    | ComponentType::Result(_)
                    | ComponentType::Variant(_) => {
                        return Err(ComponentError::UnsupportedType {
                            what: "a list of records, lists, or options",
                            offset: element_at,
                        })
                    }
                };
                types.push(ComponentTypeDef::List(element));
            }
            _ => {
                return Err(ComponentError::UnsupportedType {
                    what: "a type other than a function type, a record, or a list",
                    offset: at,
                })
            }
        }
    }
    Ok(())
}

/// Decode an import section's contents: a count, then each import. Only a function import,
/// with no attributes and no record in its type, is supported; the embedder supplies its
/// implementation by name.
fn decode_imports(
    reader: &mut Reader<'_>,
    types: &[ComponentTypeDef],
    functions: &mut Vec<ComponentFunction>,
) -> Result<(), ComponentError> {
    let count = reader.u32()?;
    for _ in 0..count {
        let at = reader.position();
        match reader.byte()? {
            0x00 | 0x01 => {}
            0x02 => {
                return Err(ComponentError::UnsupportedImport {
                    what: "import with attributes",
                    offset: at,
                })
            }
            _ => return Err(ComponentError::Malformed { offset: at }),
        }
        let name = reader.name()?;
        let sort_at = reader.position();
        if reader.byte()? != EXTERN_FUNC {
            return Err(ComponentError::UnsupportedImport {
                what: "import of a sort other than function",
                offset: sort_at,
            });
        }
        let ty = reader.u32()?;
        let Some(ComponentTypeDef::Func(func)) = types.get(ty as usize) else {
            return Err(ComponentError::NoSuchType { index: ty });
        };
        // A string or a list in an import would live in the caller's memory, which a lowered
        // import cannot reach. Every other value type passes directly, so it is accepted here.
        let has_memory_type = func
            .params
            .iter()
            .chain(func.result.iter())
            .any(|ty| holds_string(ty) || holds_list(ty));
        if has_memory_type {
            return Err(ComponentError::UnsupportedImport {
                what: "import with a string or a list in its type",
                offset: at,
            });
        }
        let duplicate = functions.iter().any(|function| {
            matches!(function, ComponentFunction::Import { name: existing, .. } if *existing == name)
        });
        if duplicate {
            return Err(ComponentError::Malformed { offset: at });
        }
        functions.push(ComponentFunction::Import { name, ty });
    }
    Ok(())
}

/// The flat core parameters a function of type `func` takes directly.
fn flat_param_count(func: &ComponentFuncType, types: &[ComponentTypeDef]) -> usize {
    func.params.iter().map(|ty| flat_of(ty, types).len()).sum()
}

/// Decode a canon section's contents: a count, then each canonical function.
///
/// A lift turns a core function into a component function. Its options may name a memory and a
/// `realloc` of the same instance. A string in its type needs them: a string parameter needs both,
/// and a string result needs the memory. A result that flattens to more than one core value is
/// read from memory, so it too needs the memory. A lower turns an import into a core function, and
/// takes no options. Lowering a component's own function, lifting a lowered one, lowering an
/// import with strings or records, and passing more than 16 flat parameters are refused.
fn decode_canon(
    reader: &mut Reader<'_>,
    core_functions: &mut Vec<CoreFunction>,
    core_memories: &[CoreMemory],
    types: &[ComponentTypeDef],
    functions: &mut Vec<ComponentFunction>,
) -> Result<(), ComponentError> {
    let count = reader.u32()?;
    for _ in 0..count {
        let at = reader.position();
        let form = reader.byte()?;
        if form != CANON_LIFT && form != CANON_LOWER {
            return Err(ComponentError::Malformed { offset: at });
        }
        if reader.byte()? != SORT_FUNC {
            return Err(ComponentError::Malformed { offset: at });
        }
        let index = reader.u32()?;
        if form == CANON_LOWER {
            let options = reader.u32()?;
            if options != 0 {
                return Err(ComponentError::UnsupportedCanon {
                    what: "canonical options on a lower",
                    offset: at,
                });
            }
            match functions.get(index as usize) {
                None => return Err(ComponentError::NoSuchComponentFunction { function: index }),
                Some(ComponentFunction::Lift { .. }) => {
                    return Err(ComponentError::UnsupportedCanon {
                        what: "lowering a function the component defines",
                        offset: at,
                    })
                }
                Some(ComponentFunction::Import { ty, .. }) => {
                    let Some(ComponentTypeDef::Func(func)) = types.get(*ty as usize) else {
                        return Err(ComponentError::NoSuchType { index: *ty });
                    };
                    if flat_param_count(func, types) > MAX_FLAT_PARAMS {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "lowering a function with more than 16 flat parameters",
                            offset: at,
                        });
                    }
                    // A string or a list lives in the caller's memory, which a lowered import has
                    // no way to reach, and a result of more than one value is returned through
                    // memory. Everything else passes directly.
                    if func
                        .params
                        .iter()
                        .chain(func.result.iter())
                        .any(|ty| holds_string(ty) || holds_list(ty))
                    {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "lowering a function with a string or a list",
                            offset: at,
                        });
                    }
                    if func
                        .result
                        .iter()
                        .any(|ty| flat_of(ty, types).len() > MAX_FLAT_RESULTS)
                    {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "lowering a function whose result is returned through memory",
                            offset: at,
                        });
                    }
                    core_functions.push(CoreFunction::Lowered { function: index });
                }
            }
            continue;
        }

        // A lift: the core function it names must exist and be an alias, not a lowering.
        let Some(core) = core_functions.get(index as usize) else {
            return Err(ComponentError::NoSuchFunction { function: index });
        };
        let CoreFunction::Alias {
            instance: core_instance,
            ..
        } = core
        else {
            return Err(ComponentError::UnsupportedCanon {
                what: "lifting a lowered function",
                offset: at,
            });
        };
        let core_instance = *core_instance;

        let option_count = reader.u32()?;
        let mut memory: Option<u32> = None;
        let mut realloc: Option<u32> = None;
        let mut post_return: Option<u32> = None;
        let mut string_encoding: Option<StringEncoding> = None;
        for _ in 0..option_count {
            let option_at = reader.position();
            match reader.byte()? {
                tag @ (OPTION_UTF8 | OPTION_UTF16 | OPTION_LATIN1_UTF16) => {
                    // A lift names at most one string encoding.
                    if string_encoding.is_some() {
                        return Err(ComponentError::Malformed { offset: option_at });
                    }
                    string_encoding = Some(match tag {
                        OPTION_UTF8 => StringEncoding::Utf8,
                        OPTION_UTF16 => StringEncoding::Utf16,
                        _ => StringEncoding::Latin1Utf16,
                    });
                }
                OPTION_MEMORY => {
                    let memory_index = reader.u32()?;
                    let Some(found) = core_memories.get(memory_index as usize) else {
                        return Err(ComponentError::NoSuchMemory {
                            memory: memory_index,
                        });
                    };
                    if memory.is_some() {
                        return Err(ComponentError::Malformed { offset: option_at });
                    }
                    if found.instance != core_instance {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "a memory of another instance",
                            offset: option_at,
                        });
                    }
                    memory = Some(memory_index);
                }
                OPTION_REALLOC => {
                    let realloc_index = reader.u32()?;
                    let Some(found) = core_functions.get(realloc_index as usize) else {
                        return Err(ComponentError::NoSuchFunction {
                            function: realloc_index,
                        });
                    };
                    let CoreFunction::Alias {
                        instance: realloc_instance,
                        ..
                    } = found
                    else {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "a realloc that is a lowered function",
                            offset: option_at,
                        });
                    };
                    if *realloc_instance != core_instance {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "a realloc of another instance",
                            offset: option_at,
                        });
                    }
                    if realloc.is_some() {
                        return Err(ComponentError::Malformed { offset: option_at });
                    }
                    realloc = Some(realloc_index);
                }
                OPTION_POST_RETURN => {
                    let post_index = reader.u32()?;
                    let Some(found) = core_functions.get(post_index as usize) else {
                        return Err(ComponentError::NoSuchFunction {
                            function: post_index,
                        });
                    };
                    let CoreFunction::Alias {
                        instance: post_instance,
                        ..
                    } = found
                    else {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "a post-return that is a lowered function",
                            offset: option_at,
                        });
                    };
                    if *post_instance != core_instance {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "a post-return of another instance",
                            offset: option_at,
                        });
                    }
                    if post_return.is_some() {
                        return Err(ComponentError::Malformed { offset: option_at });
                    }
                    post_return = Some(post_index);
                }
                _ => {
                    return Err(ComponentError::UnsupportedCanon {
                        what: "a canonical option other than a string encoding, memory, realloc, or post-return",
                        offset: option_at,
                    })
                }
            }
        }

        let ty = reader.u32()?;
        let Some(ComponentTypeDef::Func(func)) = types.get(ty as usize) else {
            return Err(ComponentError::NoSuchType { index: ty });
        };
        // Past 16 flat parameters, they are passed as one pointer to them in memory, which is
        // copied in through the memory and `realloc` options. A string or a list cannot be laid
        // out that way here.
        if flat_param_count(func, types) > MAX_FLAT_PARAMS {
            if func
                .params
                .iter()
                .any(|ty| holds_string(ty) || holds_list(ty))
            {
                return Err(ComponentError::UnsupportedType {
                    what: "a string or list among more than 16 flat parameters",
                    offset: at,
                });
            }
            if memory.is_none() || realloc.is_none() {
                return Err(ComponentError::UnsupportedCanon {
                    what: "more than 16 flat parameters without a memory and a realloc",
                    offset: at,
                });
            }
        }
        // A string or a list lives in memory, as does a result that is returned through a pointer.
        let needs_memory = func
            .params
            .iter()
            .chain(func.result.iter())
            .any(|ty| holds_string(ty) || holds_list(ty));
        let indirect_result = func
            .result
            .map(|ty| flat_of(&ty, types).len() > MAX_FLAT_RESULTS)
            .unwrap_or(false);
        if (needs_memory || indirect_result) && memory.is_none() {
            return Err(ComponentError::UnsupportedCanon {
                what: "a string, list, or indirect result without a memory option",
                offset: at,
            });
        }
        if func
            .params
            .iter()
            .any(|ty| holds_string(ty) || holds_list(ty))
            && realloc.is_none()
        {
            return Err(ComponentError::UnsupportedCanon {
                what: "a string or list parameter without a realloc option",
                offset: at,
            });
        }
        functions.push(ComponentFunction::Lift {
            core_function: index,
            ty,
            memory,
            realloc,
            encoding: string_encoding.unwrap_or(StringEncoding::Utf8),
            post_return,
        });
    }
    Ok(())
}

/// Decode an export section's contents: a count, then each export of a lifted component
/// function with no extern type.
fn decode_exports(
    reader: &mut Reader<'_>,
    functions: &[ComponentFunction],
    exports: &mut Vec<ComponentExport>,
) -> Result<(), ComponentError> {
    let count = reader.u32()?;
    for _ in 0..count {
        let at = reader.position();
        match reader.byte()? {
            0x00 | 0x01 => {}
            0x02 => {
                return Err(ComponentError::UnsupportedExport {
                    what: "export with attributes",
                    offset: at,
                })
            }
            _ => return Err(ComponentError::Malformed { offset: at }),
        }
        let name = reader.name()?;
        let sort_at = reader.position();
        if reader.byte()? != COMPONENT_SORT_FUNC {
            return Err(ComponentError::UnsupportedExport {
                what: "export of a sort other than function",
                offset: sort_at,
            });
        }
        let function = reader.u32()?;
        match functions.get(function as usize) {
            None => return Err(ComponentError::NoSuchComponentFunction { function }),
            Some(ComponentFunction::Import { .. }) => {
                return Err(ComponentError::UnsupportedExport {
                    what: "export of an imported function",
                    offset: sort_at,
                })
            }
            Some(ComponentFunction::Lift { .. }) => {}
        }
        let flag_at = reader.position();
        match reader.byte()? {
            0x00 => {}
            0x01 => {
                return Err(ComponentError::UnsupportedExport {
                    what: "export with an extern type",
                    offset: flag_at,
                })
            }
            _ => return Err(ComponentError::Malformed { offset: flag_at }),
        }
        if exports.iter().any(|export| export.name == name) {
            return Err(ComponentError::Malformed { offset: at });
        }
        exports.push(ComponentExport { name, function });
    }
    Ok(())
}

/// A reader over `bytes[position..end]`. Every read is bounded by `end`, so a section can
/// never read past its own contents.
struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], position: usize, end: usize) -> Self {
        Self {
            bytes,
            position,
            end,
        }
    }

    fn position(&self) -> usize {
        self.position
    }

    fn at_end(&self) -> bool {
        self.position >= self.end
    }

    fn byte(&mut self) -> Result<u8, ComponentError> {
        if self.position >= self.end {
            return Err(ComponentError::Truncated {
                offset: self.position,
            });
        }
        let byte = self.bytes[self.position];
        self.position += 1;
        Ok(byte)
    }

    /// Step back over the byte just read, so the next read sees it again.
    fn unread(&mut self) {
        self.position -= 1;
    }

    /// A name: a `u32` length, then that many bytes of UTF-8.
    fn name(&mut self) -> Result<String, ComponentError> {
        let start = self.position;
        let length = self.u32()? as usize;
        let end = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.end)
            .ok_or(ComponentError::Truncated {
                offset: self.position,
            })?;
        let text = std::str::from_utf8(&self.bytes[self.position..end])
            .map_err(|_| ComponentError::Malformed { offset: start })?
            .to_owned();
        self.position = end;
        Ok(text)
    }

    /// A `u32` in unsigned `LEB128` form, at most five bytes and no more than `u32::MAX`.
    fn u32(&mut self) -> Result<u32, ComponentError> {
        let start = self.position;
        let mut value: u64 = 0;
        for index in 0..5 {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7f) << (7 * index);
            if byte & 0x80 == 0 {
                return u32::try_from(value)
                    .map_err(|_| ComponentError::InvalidInteger { offset: start });
            }
        }
        Err(ComponentError::InvalidInteger { offset: start })
    }
}

#[cfg(test)]
mod tests;

/// The scalar every payload of a variant's cases shares, if any case has one. Decoding refuses a
/// variant whose payloads differ in flat type, so the first payload found is the one all share.
fn shared_payload(cases: &[(String, Option<Scalar>)]) -> Option<Scalar> {
    cases.iter().find_map(|(_, payload)| *payload)
}

/// The flat type that the canonical ABI's join gives the payloads of a variant or a result: equal
/// types stay as they are, `i32` and `f32` join as `i32`, and any other two join as `i64`.
fn join_flat(a: ValueType, b: ValueType) -> ValueType {
    match (a, b) {
        _ if a == b => a,
        (ValueType::I32, ValueType::F32) | (ValueType::F32, ValueType::I32) => ValueType::I32,
        _ => ValueType::I64,
    }
}

/// The one flat type that the payloads of some cases join to, or `None` if no case has one.
fn joined_flat(payloads: impl Iterator<Item = Scalar>) -> Option<ValueType> {
    payloads
        .filter_map(|scalar| scalar.flat_params().first().copied())
        .reduce(join_flat)
}

/// Widen a lowered payload to the joined flat type of its variant or result, as the canonical ABI
/// does: `i32` to `i64` by its unsigned value, and `f32` or `f64` to the integer of its bits.
fn widen(value: Value, to: ValueType) -> Result<Value, String> {
    Ok(match (value, to) {
        (same @ Value::I32(_), ValueType::I32)
        | (same @ Value::I64(_), ValueType::I64)
        | (same @ Value::F32(_), ValueType::F32)
        | (same @ Value::F64(_), ValueType::F64) => same,
        (Value::I32(bits), ValueType::I64) => Value::I64(i64::from(bits as u32)),
        (Value::F32(bits), ValueType::I32) => Value::I32(bits as i32),
        (Value::F32(bits), ValueType::I64) => Value::I64(i64::from(bits)),
        (Value::F64(bits), ValueType::I64) => Value::I64(bits as i64),
        _ => return Err("a payload does not widen to the flat type of its variant".to_owned()),
    })
}

/// Lower a value of type `ty` to the core values it flattens to, appending them, for the types
/// that need no memory: scalars, enums, flags, tuples, records, options, results, and variants. A
/// string or a list lives in memory, so it is refused here.
fn lower_flats(
    value: &ComponentValue,
    ty: &ComponentType,
    types: &[ComponentTypeDef],
    out: &mut Vec<Value>,
) -> Result<(), String> {
    let mismatch = || format!("{value:?} does not match its type");
    match (value, ty) {
        (ComponentValue::String(_) | ComponentValue::List(_), _) => {
            Err("a string or list is passed through memory".to_owned())
        }
        (value, ComponentType::Scalar(scalar)) => {
            if value.scalar() != Some(*scalar) {
                return Err(mismatch());
            }
            out.push(lower_scalar(value).ok_or_else(mismatch)?);
            Ok(())
        }
        (ComponentValue::Tuple(items), ComponentType::Tuple(index)) => {
            let Some(ComponentTypeDef::Tuple(scalars)) = types.get(*index as usize) else {
                return Err("the tuple is not defined".to_owned());
            };
            if items.len() != scalars.len() {
                return Err("the tuple has the wrong number of values".to_owned());
            }
            for (item, scalar) in items.iter().zip(scalars) {
                if item.scalar() != Some(*scalar) {
                    return Err("a tuple value does not match its type".to_owned());
                }
                out.push(lower_scalar(item).ok_or_else(mismatch)?);
            }
            Ok(())
        }
        (ComponentValue::Record(fields), ComponentType::Record(index)) => {
            let Some(ComponentTypeDef::Record(defined)) = types.get(*index as usize) else {
                return Err("the record is not defined".to_owned());
            };
            if fields.len() != defined.len() {
                return Err("the record has the wrong number of fields".to_owned());
            }
            for ((_, value), field) in fields.iter().zip(defined) {
                if value.scalar() != Some(field.ty) {
                    return Err("a record field does not match its type".to_owned());
                }
                out.push(lower_scalar(value).ok_or_else(mismatch)?);
            }
            Ok(())
        }
        (ComponentValue::Flags(set), ComponentType::Flags(index)) => {
            let Some(ComponentTypeDef::Flags(defined)) = types.get(*index as usize) else {
                return Err("the flags are not defined".to_owned());
            };
            let mut bits: u32 = 0;
            for label in set {
                let position = defined
                    .iter()
                    .position(|candidate| candidate == label)
                    .ok_or_else(|| format!("the flags have no label {label}"))?;
                bits |= 1u32 << position;
            }
            out.push(Value::I32(bits as i32));
            Ok(())
        }
        (ComponentValue::Enum(label), ComponentType::Enum(index)) => {
            let Some(ComponentTypeDef::Enum(labels)) = types.get(*index as usize) else {
                return Err("the enum is not defined".to_owned());
            };
            let case = labels
                .iter()
                .position(|candidate| candidate == label)
                .ok_or_else(|| format!("the enum has no case {label}"))?;
            out.push(Value::I32(case as i32));
            Ok(())
        }
        (ComponentValue::Option(payload), ComponentType::Option(index)) => {
            let Some(ComponentTypeDef::Option(element)) = types.get(*index as usize) else {
                return Err("the option is not defined".to_owned());
            };
            match payload.as_deref() {
                None => {
                    out.push(Value::I32(0));
                    out.extend(element.flat_params().into_iter().map(zero_value));
                }
                Some(value) => {
                    if value.scalar() != Some(*element) {
                        return Err("an option payload does not match its type".to_owned());
                    }
                    out.push(Value::I32(1));
                    out.push(lower_scalar(value).ok_or_else(mismatch)?);
                }
            }
            Ok(())
        }
        (ComponentValue::Result(outcome), ComponentType::Result(index)) => {
            let Some(ComponentTypeDef::Result { ok, err }) = types.get(*index as usize) else {
                return Err("the result is not defined".to_owned());
            };
            let (discriminant, case_type, payload) = match outcome {
                Ok(payload) => (0, *ok, payload.as_deref()),
                Err(payload) => (1, *err, payload.as_deref()),
            };
            out.push(Value::I32(discriminant));
            let joined = joined_flat([*ok, *err].into_iter().flatten());
            lower_payload(case_type, payload, joined, out)
        }
        (ComponentValue::Variant(label, payload), ComponentType::Variant(index)) => {
            let Some(ComponentTypeDef::Variant { cases }) = types.get(*index as usize) else {
                return Err("the variant is not defined".to_owned());
            };
            let case = cases
                .iter()
                .position(|(name, _)| name == label)
                .ok_or_else(|| format!("the variant has no case {label}"))?;
            out.push(Value::I32(case as i32));
            let joined = joined_flat(cases.iter().filter_map(|(_, p)| *p));
            lower_payload(cases[case].1, payload.as_deref(), joined, out)
        }
        _ => Err(mismatch()),
    }
}

/// Lower the payload slot of a result or a variant. A case with a value fills the slot with that
/// value, widened to the joined flat type; a case without one fills it with zero. The slot exists
/// only when some case has a payload, so nothing is pushed otherwise.
fn lower_payload(
    case_type: Option<Scalar>,
    payload: Option<&ComponentValue>,
    joined: Option<ValueType>,
    out: &mut Vec<Value>,
) -> Result<(), String> {
    match (case_type, payload) {
        (None, None) => {
            if let Some(joined) = joined {
                out.push(zero_value(joined));
            }
            Ok(())
        }
        (Some(scalar), Some(value)) => {
            if value.scalar() != Some(scalar) {
                return Err("a payload does not match its case".to_owned());
            }
            let lowered =
                lower_scalar(value).ok_or_else(|| "a payload did not lower".to_owned())?;
            if let Some(joined) = joined {
                out.push(widen(lowered, joined)?);
            }
            Ok(())
        }
        _ => Err("a payload does not match its case".to_owned()),
    }
}

/// Lift the core values of one parameter or result of type `ty` from `values`, consuming exactly
/// the values it flattens to, in the canonical order. A payload of a result or a variant arrives
/// as the joined flat type, and is narrowed to the type of its case first.
fn lift_flats(
    values: &mut std::slice::Iter<'_, Value>,
    ty: &ComponentType,
    types: &[ComponentTypeDef],
) -> Result<ComponentValue, String> {
    let mut take = || {
        values
            .next()
            .cloned()
            .ok_or_else(|| "the core function passed too few values".to_owned())
    };
    match ty {
        ComponentType::Scalar(Scalar::String) | ComponentType::List(_) => {
            Err("a string or list is passed through memory".to_owned())
        }
        ComponentType::Scalar(scalar) => lift(take()?, *scalar),
        ComponentType::Enum(index) => {
            let Some(ComponentTypeDef::Enum(labels)) = types.get(*index as usize) else {
                return Err("the enum is not defined".to_owned());
            };
            let case = enum_case(&take()?, labels.len())?;
            Ok(ComponentValue::Enum(labels[case].clone()))
        }
        ComponentType::Flags(index) => {
            let Some(ComponentTypeDef::Flags(labels)) = types.get(*index as usize) else {
                return Err("the flags are not defined".to_owned());
            };
            let Value::I32(bits) = take()? else {
                return Err("a flags value is not an i32".to_owned());
            };
            let bits = bits as u32;
            if labels.len() < 32 && bits >> labels.len() != 0 {
                return Err(format!("{bits:#x} sets a bit that names no flag"));
            }
            let set = labels
                .iter()
                .enumerate()
                .filter(|(position, _)| bits & (1u32 << position) != 0)
                .map(|(_, label)| label.clone())
                .collect();
            Ok(ComponentValue::Flags(set))
        }
        ComponentType::Tuple(index) => {
            let Some(ComponentTypeDef::Tuple(scalars)) = types.get(*index as usize) else {
                return Err("the tuple is not defined".to_owned());
            };
            let mut lifted = Vec::with_capacity(scalars.len());
            for scalar in scalars {
                lifted.push(lift(take()?, *scalar)?);
            }
            Ok(ComponentValue::Tuple(lifted))
        }
        ComponentType::Record(index) => {
            let Some(ComponentTypeDef::Record(fields)) = types.get(*index as usize) else {
                return Err("the record is not defined".to_owned());
            };
            let mut lifted = Vec::with_capacity(fields.len());
            for field in fields {
                lifted.push((field.name.clone(), lift(take()?, field.ty)?));
            }
            Ok(ComponentValue::Record(lifted))
        }
        ComponentType::Option(index) => {
            let Some(ComponentTypeDef::Option(element)) = types.get(*index as usize) else {
                return Err("the option is not defined".to_owned());
            };
            let discriminant = take()?;
            // The payload slot is always present; its value is read only for the some case.
            let payload = take()?;
            match discriminant {
                Value::I32(0) => Ok(ComponentValue::Option(None)),
                Value::I32(1) => Ok(ComponentValue::Option(Some(Box::new(lift(
                    payload, *element,
                )?)))),
                _ => Err("an option's discriminant is neither 0 nor 1".to_owned()),
            }
        }
        ComponentType::Result(index) => {
            let Some(ComponentTypeDef::Result { ok, err }) = types.get(*index as usize) else {
                return Err("the result is not defined".to_owned());
            };
            let discriminant = take()?;
            let joined = joined_flat([*ok, *err].into_iter().flatten());
            let slot = if joined.is_some() {
                Some(take()?)
            } else {
                None
            };
            let (is_ok, case_type) = match discriminant {
                Value::I32(0) => (true, *ok),
                Value::I32(1) => (false, *err),
                _ => return Err("a result's discriminant is neither 0 nor 1".to_owned()),
            };
            let payload = lift_payload(case_type, slot)?;
            Ok(ComponentValue::Result(if is_ok {
                Ok(payload)
            } else {
                Err(payload)
            }))
        }
        ComponentType::Variant(index) => {
            let Some(ComponentTypeDef::Variant { cases }) = types.get(*index as usize) else {
                return Err("the variant is not defined".to_owned());
            };
            let discriminant = take()?;
            let joined = joined_flat(cases.iter().filter_map(|(_, p)| *p));
            let slot = if joined.is_some() {
                Some(take()?)
            } else {
                None
            };
            let case = enum_case(&discriminant, cases.len())?;
            let (label, case_type) = &cases[case];
            let payload = lift_payload(*case_type, slot)?;
            Ok(ComponentValue::Variant(label.clone(), payload))
        }
    }
}

/// Lift the payload of a case from its slot, narrowing the joined value to the case's type.
fn lift_payload(
    case_type: Option<Scalar>,
    slot: Option<Value>,
) -> Result<Option<Box<ComponentValue>>, String> {
    match (case_type, slot) {
        (None, _) => Ok(None),
        (Some(scalar), Some(slot)) => {
            let narrowed = narrow(slot, scalar.flat_params()[0])?;
            Ok(Some(Box::new(lift(narrowed, scalar)?)))
        }
        (Some(_), None) => Err("a payload has no slot".to_owned()),
    }
}

/// Narrow a joined core value to the flat type of its case, as the canonical ABI coerces it: an
/// i64 wraps to an i32, and the bits of an integer reinterpret as a float.
fn narrow(value: Value, want: ValueType) -> Result<Value, String> {
    Ok(match (value, want) {
        (same @ Value::I32(_), ValueType::I32)
        | (same @ Value::I64(_), ValueType::I64)
        | (same @ Value::F32(_), ValueType::F32)
        | (same @ Value::F64(_), ValueType::F64) => same,
        (Value::I64(bits), ValueType::I32) => Value::I32(bits as i32),
        (Value::I32(bits), ValueType::F32) => Value::F32(bits as u32),
        (Value::I64(bits), ValueType::F32) => Value::F32(bits as u32),
        (Value::I64(bits), ValueType::F64) => Value::F64(bits as u64),
        _ => return Err("a joined value does not narrow to the type of its case".to_owned()),
    })
}
