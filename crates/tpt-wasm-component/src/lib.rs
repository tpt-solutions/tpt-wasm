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
use tpt_wasm_runtime::{Caller, Engine, HostFunction, Instance, Linker, MemoryView};
use tpt_wasm_types::{FunctionType, GuestMemory, ResultType, Value, ValueType};

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
    /// The field's type, which may be any component type.
    pub ty: ComponentType,
}

/// One definition of the component's type index space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentTypeDef {
    /// A function type.
    Func(ComponentFuncType),
    /// A record type, with its fields in order.
    Record(Vec<RecordField>),
    /// A list type, whose elements are of one type.
    List(ComponentType),
    /// An option type, whose payload is of one type.
    Option(ComponentType),
    /// An enum type: its case labels, in order. A value is the index of one of them.
    Enum(Vec<String>),
    /// A tuple, whose items are of any types, in order.
    Tuple(Vec<ComponentType>),
    /// A result: the payload types of its ok and error cases, each optional.
    Result {
        ok: Option<ComponentType>,
        err: Option<ComponentType>,
    },
    /// A variant: each case's label and the scalar its payload holds, if it has one. Every
    /// payload shares one flat type.
    Variant {
        cases: Vec<(String, Option<ComponentType>)>,
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

pub fn same_function_type(
    a_types: &[ComponentTypeDef],
    a: &ComponentFuncType,
    b_types: &[ComponentTypeDef],
    b: &ComponentFuncType,
) -> bool {
    a.params.len() == b.params.len()
        && a.params
            .iter()
            .zip(&b.params)
            .all(|(x, y)| canon::same_type(a_types, x, b_types, y))
        && match (a.result, b.result) {
            (None, None) => true,
            (Some(x), Some(y)) => canon::same_type(a_types, &x, b_types, &y),
            _ => false,
        }
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
    /// A component import, lowered so that core code can call it. A lower may name a memory, which
    /// the host function reads the strings and lists it is given from.
    Lowered {
        function: u32,
        memory: Option<u32>,
        /// The export name of the `realloc` a string or list result is allocated through, in the instance
        /// that the memory belongs to.
        realloc: Option<String>,
    },
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
    /// What the export names: a core function, or a core memory.
    pub target: InlineTarget,
}

/// What an inline export of a core instance names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineTarget {
    /// A core function, by its index in the core function index space.
    Function(u32),
    /// A core memory, by its index in the core memory index space. A consumer that imports it shares
    /// the memory the named instance exports, rather than receiving a copy.
    Memory(u32),
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
    /// The type table and declared function type of the import `name`, if the component has a
    /// function import of that name. Pass both to [`same_function_type`] to compare the import
    /// with a signature from elsewhere, such as a WIT document.
    pub fn import_signature(
        &self,
        name: &str,
    ) -> Option<(&[ComponentTypeDef], &ComponentFuncType)> {
        self.functions.iter().find_map(|function| match function {
            ComponentFunction::Import { name: import, ty } if import == name => {
                Some((self.types.as_slice(), self.func_type(*ty)?))
            }
            _ => None,
        })
    }

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
                    &component.core_memories,
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
                    // A lowered import that names a memory is only meaningful to a consumer that shares that
                    // memory, since its pointers are into it. Refuse one that does not.
                    for argument in arguments {
                        let Slot::Exports(bound) = &slots[argument.instance as usize] else {
                            continue;
                        };
                        for export in bound {
                            let Target::Host(host) = &export.target else {
                                continue;
                            };
                            let Some(named) = host.memory.as_ref() else {
                                continue;
                            };
                            let shares = runtime.own_memory().is_ok_and(|own| own.is_same(named));
                            if !shares {
                                return Err(ComponentError::Instantiate {
                                    instance: index,
                                    message: format!(
                                        "the import {} names a memory this instance does not share",
                                        export.name
                                    ),
                                });
                            }
                        }
                    }
                    Slot::Runtime(Box::new(runtime))
                }
                CoreInstance::FromExports { exports } => {
                    let mut bound = Vec::with_capacity(exports.len());
                    for export in exports {
                        let target = match &export.target {
                            InlineTarget::Memory(memory) => {
                                // The memory belongs to an instance created before this one, so it is
                                // already in the slots, and it must be exported under the name given.
                                let named = &self.core_memories[*memory as usize];
                                let Some(Slot::Runtime(provider)) =
                                    slots.get(named.instance as usize)
                                else {
                                    return Err(ComponentError::Instantiate {
                                        instance: index,
                                        message:
                                            "an inline export names a memory of a later instance"
                                                .to_owned(),
                                    });
                                };
                                provider.memory_view(&named.export).map_err(|error| {
                                    ComponentError::Instantiate {
                                        instance: index,
                                        message: error.to_string(),
                                    }
                                })?;
                                Target::Alias {
                                    source: named.instance as usize,
                                    export: named.export.clone(),
                                }
                            }
                            InlineTarget::Function(function) => match &self.core_functions
                                [*function as usize]
                            {
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
                                CoreFunction::Lowered {
                                    function,
                                    memory,
                                    realloc,
                                } => {
                                    let instantiate =
                                        |message: String| ComponentError::Instantiate {
                                            instance: index,
                                            message,
                                        };
                                    let mut host =
                                        self.lowered(*function, imports).map_err(instantiate)?;
                                    if let Some(memory) = memory {
                                        // The memory belongs to an instance created before this one,
                                        // so it is already in the slots.
                                        let named = &self.core_memories[*memory as usize];
                                        let Some(Slot::Runtime(provider)) =
                                            slots.get(named.instance as usize)
                                        else {
                                            return Err(instantiate(
                                            "the memory a lower names is not an instance created before it"
                                                .to_owned(),
                                        ));
                                        };
                                        host.memory = Some(
                                            provider
                                                .memory_view(&named.export)
                                                .map_err(|error| instantiate(error.to_string()))?,
                                        );
                                        if let Some(name) = realloc {
                                            host.realloc =
                                                Some(provider.get_function(name).map_err(
                                                    |error| instantiate(error.to_string()),
                                                )?);
                                        }
                                    }
                                    Target::Host(host)
                                }
                            },
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
        Ok(LoweredHost {
            name: name.clone(),
            closure: supplied.closure.clone(),
            params: declared.params.clone(),
            result: declared.result,
            types: self.types.clone(),
            capabilities: imports.capabilities.clone(),
            memory: None,
            realloc: None,
        })
    }
}

/// The core signature a post-return must have: it takes the flat values of the lifted result, or
/// one pointer when they do not fit, and returns nothing.
fn post_return_signature(func: &ComponentFuncType, types: &[ComponentTypeDef]) -> FunctionType {
    let params = match func.result {
        None => Vec::new(),
        Some(ty) => {
            let flat = canon::flat(&ty, types);
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
        .flat_map(|ty| canon::flat(ty, types))
        .collect();
    let results = match func.result {
        None => Vec::new(),
        Some(ty) => {
            let flat = canon::flat(&ty, types);
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
    /// The memory the lower names, which the strings and lists of the arguments are read from when no
    /// instance is running.
    memory: Option<MemoryView>,
    /// The store address of the `realloc` a string or list result is allocated through.
    realloc: Option<u32>,
}

impl LoweredHost {
    fn no_memory(&self) -> String {
        format!(
            "the import {} needs a memory, which its lower does not name",
            self.name
        )
    }

    /// The bytes at `pointer`, from the calling instance's memory during a call, or from the named
    /// memory when no instance is running.
    fn read_guest(
        &self,
        caller: &mut Option<&mut dyn Caller>,
        pointer: u32,
        length: u64,
    ) -> Result<Vec<u8>, String> {
        let bytes = match caller.as_mut() {
            Some(caller) => {
                let memory = caller.memory().ok_or_else(|| self.no_memory())?;
                GuestMemory::read(memory, u64::from(pointer), length)
            }
            None => {
                let memory = self.memory.as_ref().ok_or_else(|| self.no_memory())?;
                GuestMemory::read(memory, u64::from(pointer), length)
            }
        };
        bytes.ok_or_else(|| {
            format!(
                "the import {} was given bytes outside its memory, or the memory is in use by a running call",
                self.name
            )
        })
    }

    /// Write `bytes` at `pointer`, to the same memory `read_guest` reads.
    fn write_guest(
        &self,
        caller: &mut Option<&mut dyn Caller>,
        pointer: u32,
        bytes: &[u8],
    ) -> Result<(), String> {
        let written = match caller.as_mut() {
            Some(caller) => {
                let memory = caller.memory().ok_or_else(|| self.no_memory())?;
                GuestMemory::write(memory, u64::from(pointer), bytes)
            }
            None => {
                let mut memory = self.memory.clone().ok_or_else(|| self.no_memory())?;
                GuestMemory::write(&mut memory, u64::from(pointer), bytes)
            }
        };
        if written {
            Ok(())
        } else {
            Err(format!("the import {} wrote outside its memory", self.name))
        }
    }

    /// Lift the core values into the parameters, run the embedder's function, and lower its result.
    /// A result that flattens to more than one core value is stored through the return pointer the
    /// caller passes last. Parameters that flatten to more than the core limit come from one block
    /// the caller points to.
    fn invoke(
        &self,
        args: &[Value],
        mut caller: Option<&mut dyn Caller>,
    ) -> Result<Vec<Value>, String> {
        // The grant is checked on every call, so a revoked import stops working at once.
        authorize_call(self.capabilities.as_ref(), &self.name)?;
        let encoding = StringEncoding::Utf8;
        let mut memory = HostMemory {
            host: self,
            caller: &mut caller,
        };
        let mut remaining = args.iter();
        let flat_count: usize = self
            .params
            .iter()
            .map(|ty| flat(ty, &self.types).len())
            .sum();
        let mut values = Vec::with_capacity(self.params.len());
        if flat_count > MAX_FLAT_PARAMS {
            let pointer = match remaining.next() {
                Some(Value::I32(pointer)) => *pointer as u32,
                _ => {
                    return Err(format!(
                        "the import {} was not given its parameter block",
                        self.name
                    ))
                }
            };
            let (offsets, _, _) = layout(&self.params, &self.types);
            for (ty, offset) in self.params.iter().zip(offsets) {
                let at = pointer.checked_add(offset as u32).ok_or_else(|| {
                    format!("the import {} was given a block past memory", self.name)
                })?;
                values.push(canon::load(&mut memory, encoding, at, ty, &self.types)?);
            }
        } else {
            for ty in &self.params {
                values.push(canon::lift(
                    &mut memory,
                    encoding,
                    &mut remaining,
                    ty,
                    &self.types,
                )?);
            }
        }
        let return_pointer = match self.result.as_ref() {
            Some(ty) if flat(ty, &self.types).len() > MAX_FLAT_RESULTS => match remaining.next() {
                Some(Value::I32(pointer)) => Some(*pointer as u32),
                _ => {
                    return Err(format!(
                        "the import {} was not given its return pointer",
                        self.name
                    ))
                }
            },
            _ => None,
        };
        if remaining.next().is_some() {
            return Err(format!(
                "the import {} was given more core values than its type has",
                self.name
            ));
        }
        let results = (self.closure)(&values)?;
        match (self.result.as_ref(), results.as_slice()) {
            (None, []) => Ok(Vec::new()),
            (Some(ty), [value]) if canon::matches(value, ty, &self.types) => match return_pointer {
                Some(pointer) => {
                    canon::store(&mut memory, encoding, value, ty, &self.types, pointer)?;
                    Ok(Vec::new())
                }
                None => canon::lower(&mut memory, encoding, value, ty, &self.types),
            },
            _ => Err(format!(
                "the import {} returned values that do not match its type",
                self.name
            )),
        }
    }
}

impl fmt::Debug for LoweredHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LoweredHost({})", self.name)
    }
}

impl HostFunction for LoweredHost {
    fn function_type(&self) -> FunctionType {
        let flat_count: usize = self
            .params
            .iter()
            .map(|ty| flat(ty, &self.types).len())
            .sum();
        let mut params: Vec<ValueType> = if flat_count > MAX_FLAT_PARAMS {
            vec![ValueType::I32]
        } else {
            self.params
                .iter()
                .flat_map(|ty| flat(ty, &self.types))
                .collect()
        };
        let results = match self.result.as_ref() {
            Some(ty) if flat(ty, &self.types).len() > MAX_FLAT_RESULTS => {
                params.push(ValueType::I32);
                Vec::new()
            }
            Some(ty) => flat(ty, &self.types),
            None => Vec::new(),
        };
        FunctionType {
            params: ResultType(params),
            results: ResultType(results),
        }
    }

    fn call(&self, args: &[Value]) -> Result<Vec<Value>, String> {
        self.invoke(args, None)
    }

    fn call_with_caller(
        &self,
        caller: &mut dyn Caller,
        args: &[Value],
    ) -> Result<Vec<Value>, String> {
        self.invoke(args, Some(caller))
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

/// A component instance's memory during a call: the memory and the `realloc` that the export's
/// canonical options name.
struct InstanceMemory<'a> {
    host: &'a mut ComponentInstance,
    instance: usize,
    memory: Option<String>,
    realloc: Option<String>,
}

impl Memory for InstanceMemory<'_> {
    fn read(&mut self, address: u32, length: u32) -> Result<Vec<u8>, String> {
        let memory = self
            .memory
            .as_deref()
            .ok_or_else(|| "a value needs a memory, which the export does not name".to_owned())?;
        self.host
            .read_core_memory(self.instance, memory, u64::from(address), length as usize)
            .map_err(|error| error.to_string())
    }

    fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), String> {
        let memory = self
            .memory
            .as_deref()
            .ok_or_else(|| "a value needs a memory, which the export does not name".to_owned())?;
        self.host
            .write_core_memory(self.instance, memory, u64::from(address), bytes)
            .map_err(|error| error.to_string())
    }

    fn realloc(
        &mut self,
        old: u32,
        old_size: u32,
        align: u32,
        new_size: u32,
    ) -> Result<u32, String> {
        let realloc = self
            .realloc
            .as_deref()
            .ok_or_else(|| "a value needs a realloc, which the export does not name".to_owned())?;
        let results = self
            .host
            .call_core(
                self.instance,
                realloc,
                vec![
                    Value::I32(old as i32),
                    Value::I32(old_size as i32),
                    Value::I32(align as i32),
                    Value::I32(new_size as i32),
                ],
            )
            .map_err(|error| error.to_string())?;
        match results.as_slice() {
            [Value::I32(pointer)] => Ok(*pointer as u32),
            _ => Err("realloc did not return one i32".to_owned()),
        }
    }
}

/// A host import's view of guest memory during a call: the calling instance's memory and
/// `realloc` while one is running, or the memory the lower names when none is.
struct HostMemory<'a, 'c> {
    host: &'a LoweredHost,
    caller: &'a mut Option<&'c mut dyn Caller>,
}

impl Memory for HostMemory<'_, '_> {
    fn read(&mut self, address: u32, length: u32) -> Result<Vec<u8>, String> {
        self.host
            .read_guest(&mut *self.caller, address, u64::from(length))
    }

    fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), String> {
        self.host.write_guest(&mut *self.caller, address, bytes)
    }

    fn realloc(
        &mut self,
        old: u32,
        old_size: u32,
        align: u32,
        new_size: u32,
    ) -> Result<u32, String> {
        let name = self.host.name.clone();
        let realloc = self.host.realloc.ok_or_else(|| {
            format!("the import {name} needs a realloc option, which its lower does not name")
        })?;
        let caller = self.caller.as_mut().ok_or_else(|| {
            format!("the import {name} needs a calling instance to allocate, and none is running")
        })?;
        let results = caller.call(
            realloc,
            vec![
                Value::I32(old as i32),
                Value::I32(old_size as i32),
                Value::I32(align as i32),
                Value::I32(new_size as i32),
            ],
        )?;
        match results.as_slice() {
            [Value::I32(pointer)] => Ok(*pointer as u32),
            _ => Err("realloc did not return one i32".to_owned()),
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
    /// types in number and in type. Arguments are lowered into the instance's memory, through its
    /// `realloc`, where they hold strings or lists. The result is lifted from the core values it
    /// returned, or from the memory it points into when it is returned through a pointer.
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
        let types = Arc::clone(&self.types);
        for (position, (expected, argument)) in export.params.iter().zip(&arguments).enumerate() {
            if !canon::matches(argument, expected, &types) {
                return Err(ComponentError::ArgumentMismatch {
                    export: name.to_owned(),
                    message: format!(
                        "argument {position} is {argument:?}, but {expected:?} is expected"
                    ),
                });
            }
        }
        let instance = export.core_instance;
        let encoding = export.encoding;
        let mut memory = InstanceMemory {
            host: self,
            instance,
            memory: export.memory.clone(),
            realloc: export.realloc.clone(),
        };
        let mut core_arguments = Vec::new();
        let flat_count: usize = export
            .params
            .iter()
            .map(|ty| canon::flat(ty, &types).len())
            .sum();
        if flat_count > MAX_FLAT_PARAMS {
            // The parameters are stored in one block, laid out as a record of them, and the core
            // function takes a pointer to the block.
            let (offsets, size, align) = canon::layout(&export.params, &types);
            let block = memory
                .realloc(0, 0, align as u32, size as u32)
                .map_err(|message| ComponentError::Call { instance, message })?;
            for ((argument, ty), offset) in arguments.iter().zip(&export.params).zip(offsets) {
                let at = block
                    .checked_add(offset as u32)
                    .ok_or_else(|| ComponentError::Call {
                        instance,
                        message: "the parameter block is past the end of memory".to_owned(),
                    })?;
                canon::store(&mut memory, encoding, argument, ty, &types, at)
                    .map_err(|message| ComponentError::Call { instance, message })?;
            }
            core_arguments.push(Value::I32(block as i32));
        } else {
            for (argument, ty) in arguments.iter().zip(&export.params) {
                let lowered = canon::lower(&mut memory, encoding, argument, ty, &types)
                    .map_err(|message| ComponentError::Call { instance, message })?;
                core_arguments.extend(lowered);
            }
        }
        let core_results = memory
            .host
            .call_core(instance, &export.core_export, core_arguments)?;
        let lifted: Result<Vec<ComponentValue>, String> = match export.result {
            None if core_results.is_empty() => Ok(Vec::new()),
            None => Err("the core function returned a result its type does not have".to_owned()),
            Some(ty) if canon::flat(&ty, &types).len() > MAX_FLAT_RESULTS => {
                match core_results.as_slice() {
                    [Value::I32(pointer)] => {
                        canon::load(&mut memory, encoding, *pointer as u32, &ty, &types)
                            .map(|value| vec![value])
                    }
                    _ => Err("an indirect result did not return one pointer".to_owned()),
                }
            }
            Some(ty) => {
                let mut remaining = core_results.iter();
                canon::lift(&mut memory, encoding, &mut remaining, &ty, &types).and_then(|value| {
                    if remaining.next().is_some() {
                        Err("the core function returned the wrong number of results".to_owned())
                    } else {
                        Ok(vec![value])
                    }
                })
            }
        };
        let lifted = lifted.map_err(|message| ComponentError::Call { instance, message });
        // The post-return runs once the result has been read, with the core values the function
        // returned, so that the module may free what the result pointed into.
        if lifted.is_ok() {
            if let Some(post_return) = &export.post_return {
                memory.host.call_core(instance, post_return, core_results)?;
            }
        }
        lifted
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
                .invoke(&args, None)
                .map_err(|message| ComponentError::Call { instance, message }),
        }
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
    memories: &[CoreMemory],
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
                    let sort = reader.byte()?;
                    let index = reader.u32()?;
                    let target = match sort {
                        SORT_FUNC => {
                            if index as usize >= functions.len() {
                                return Err(ComponentError::NoSuchFunction { function: index });
                            }
                            InlineTarget::Function(index)
                        }
                        SORT_CORE_MEMORY => {
                            if index as usize >= memories.len() {
                                return Err(ComponentError::NoSuchMemory { memory: index });
                            }
                            InlineTarget::Memory(index)
                        }
                        _ => {
                            return Err(ComponentError::UnsupportedInstance {
                                what: "inline export of a sort other than a function or a memory",
                                offset: sort_at,
                            })
                        }
                    };
                    if exports.iter().any(|export| export.name == name) {
                        return Err(ComponentError::Malformed { offset: sort_at });
                    }
                    exports.push(InlineExport { name, target });
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
/// A record is its tag, then its labelled fields, each a value type. Every value type names an
/// earlier definition, so the table cannot be cyclic.
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
                    let ty = value_type(reader, types)?;
                    fields.push(RecordField { name, ty });
                }
                types.push(ComponentTypeDef::Record(fields));
            }
            TYPE_OPTION => {
                let payload = value_type(reader, types)?;
                types.push(ComponentTypeDef::Option(payload));
            }
            TYPE_TUPLE => {
                let count = reader.u32()?;
                if count == 0 {
                    return Err(ComponentError::Malformed { offset: at });
                }
                let mut items = Vec::new();
                for _ in 0..count {
                    items.push(value_type(reader, types)?);
                }
                types.push(ComponentTypeDef::Tuple(items));
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
                let mut cases: [Option<ComponentType>; 2] = [None, None];
                for case in &mut cases {
                    let at_case = reader.position();
                    match reader.byte()? {
                        0x00 => {}
                        0x01 => *case = Some(value_type(reader, types)?),
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
                let mut cases: Vec<(String, Option<ComponentType>)> = Vec::new();
                for _ in 0..case_count {
                    let at_label = reader.position();
                    let label = reader.name()?;
                    if cases.iter().any(|(defined, _)| *defined == label) {
                        return Err(ComponentError::Malformed { offset: at_label });
                    }
                    let at_case = reader.position();
                    let payload = match reader.byte()? {
                        0x00 => None,
                        0x01 => Some(value_type(reader, types)?),
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
                let element = value_type(reader, types)?;
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
        let Some(ComponentTypeDef::Func(_)) = types.get(ty as usize) else {
            return Err(ComponentError::NoSuchType { index: ty });
        };
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
    func.params
        .iter()
        .map(|ty| canon::flat(ty, types).len())
        .sum()
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
            // A lower takes at most one memory, and a string encoding of UTF-8, which is the default.
            // A realloc or post-return option has no meaning on a lower.
            let option_count = reader.u32()?;
            let mut memory = None;
            let mut realloc_alias: Option<(u32, String)> = None;
            for _ in 0..option_count {
                let option_at = reader.position();
                match reader.byte()? {
                    OPTION_UTF8 => {}
                    OPTION_UTF16 | OPTION_LATIN1_UTF16 => {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "a string encoding other than UTF-8 on a lower",
                            offset: option_at,
                        })
                    }
                    OPTION_MEMORY => {
                        let memory_index = reader.u32()?;
                        if core_memories.get(memory_index as usize).is_none() {
                            return Err(ComponentError::NoSuchMemory {
                                memory: memory_index,
                            });
                        }
                        if memory.replace(memory_index).is_some() {
                            return Err(ComponentError::Malformed { offset: option_at });
                        }
                    }
                    OPTION_REALLOC => {
                        let function = reader.u32()?;
                        let Some(core) = core_functions.get(function as usize) else {
                            return Err(ComponentError::NoSuchFunction { function });
                        };
                        let CoreFunction::Alias { instance, export } = core else {
                            return Err(ComponentError::UnsupportedCanon {
                                what: "a realloc that is not an export of an instance",
                                offset: option_at,
                            });
                        };
                        if realloc_alias.replace((*instance, export.clone())).is_some() {
                            return Err(ComponentError::Malformed { offset: option_at });
                        }
                    }
                    _ => return Err(ComponentError::UnsupportedCanon {
                        what:
                            "a canonical option on a lower other than a memory, realloc, or a string encoding",
                        offset: option_at,
                    }),
                }
            }
            // A result is allocated in the memory the lower names, so its realloc must be an export of the
            // instance that memory belongs to.
            let realloc = match (realloc_alias, memory) {
                (None, _) => None,
                (Some(_), None) => {
                    return Err(ComponentError::UnsupportedCanon {
                        what: "a realloc without a memory option",
                        offset: at,
                    })
                }
                (Some((instance, export)), Some(memory_index)) => {
                    if core_memories[memory_index as usize].instance != instance {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "a realloc of another instance than the memory",
                            offset: at,
                        });
                    }
                    Some(export)
                }
            };
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
                    // Strings and lists among the parameters are read through the memory the lower
                    // names, so such a lower needs one. A string or list result would be written into
                    // memory the host cannot reach, so it is refused.
                    if func.params.iter().any(|ty| canon::needs_memory(ty, types))
                        && memory.is_none()
                    {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "a string or list parameter of a lowered import without a memory option",
                            offset: at,
                        });
                    }
                    // A string or list result is written through the realloc and the memory, which both need the
                    // options. It is returned through a pointer, so it does not count as flat values here.
                    if func.result.iter().any(|ty| canon::needs_memory(ty, types))
                        && (memory.is_none() || realloc.is_none())
                    {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "a string or list result of a lowered import without a memory and a realloc option",
                            offset: at,
                        });
                    }
                    if func
                        .result
                        .iter()
                        .any(|ty| canon::flat(ty, types).len() > MAX_FLAT_RESULTS)
                        && memory.is_none()
                    {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "a result returned through memory of a lowered import without a memory option",
                            offset: at,
                        });
                    }
                    core_functions.push(CoreFunction::Lowered {
                        function: index,
                        memory,
                        realloc,
                    });
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
        if flat_param_count(func, types) > MAX_FLAT_PARAMS
            && (memory.is_none() || realloc.is_none())
        {
            return Err(ComponentError::UnsupportedCanon {
                what: "more than 16 flat parameters without a memory and a realloc",
                offset: at,
            });
        }
        // A string or a list lives in memory, as does a result that is returned through a pointer.
        let needs_memory = func
            .params
            .iter()
            .chain(func.result.iter())
            .any(|ty| canon::needs_memory(ty, types));
        let indirect_result = func
            .result
            .map(|ty| canon::flat(&ty, types).len() > MAX_FLAT_RESULTS)
            .unwrap_or(false);
        if (needs_memory || indirect_result) && memory.is_none() {
            return Err(ComponentError::UnsupportedCanon {
                what: "a string, list, or indirect result without a memory option",
                offset: at,
            });
        }
        if func.params.iter().any(|ty| canon::needs_memory(ty, types)) && realloc.is_none() {
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

/// Reading WIT, and mapping a world's imports to the capability ids they name.
mod canon;
use canon::{flat, layout, Memory};
pub mod wit;
