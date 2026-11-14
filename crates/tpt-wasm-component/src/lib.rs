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
/// The canon tag of a lift, and of a lower.
const CANON_LIFT: u8 = 0x00;
const CANON_LOWER: u8 = 0x01;
/// The canonical option tags: the string encoding (UTF-8 only), a memory, and a `realloc`.
const OPTION_UTF8: u8 = 0x00;
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
}

impl ComponentValue {
    /// The scalar type of this value, or `None` for a record.
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
            ComponentValue::Record(_) => None,
        }
    }
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
            let expected = flat_signature(func_type, &self.types);
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
            ComponentTypeDef::Record(_) => None,
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
        let params = declared
            .params
            .iter()
            .map(|ty| match ty {
                ComponentType::Scalar(scalar) => Ok(*scalar),
                ComponentType::Record(_) => Err(format!("the import {name} takes a record")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let result = match declared.result {
            None => None,
            Some(ComponentType::Scalar(scalar)) => Some(scalar),
            Some(ComponentType::Record(_)) => {
                return Err(format!("the import {name} returns a record"))
            }
        };
        Ok(LoweredHost {
            name: name.clone(),
            closure: supplied.closure.clone(),
            params,
            result,
            capabilities: imports.capabilities.clone(),
        })
    }
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
    }
}

/// Whether a component type holds a string, at the top level.
fn holds_string(ty: &ComponentType) -> bool {
    matches!(ty, ComponentType::Scalar(Scalar::String))
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
    params: Vec<Scalar>,
    result: Option<Scalar>,
    capabilities: Option<Arc<Mutex<CapabilitySet>>>,
}

impl fmt::Debug for LoweredHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LoweredHost({})", self.name)
    }
}

impl LoweredHost {
    /// Run the embedder's function on the lifted arguments, and lower its results.
    fn invoke(&self, args: &[Value]) -> Result<Vec<Value>, String> {
        // The grant is checked on every call, so a revoked import stops working at once.
        authorize_call(self.capabilities.as_ref(), &self.name)?;
        if args.len() != self.params.len() {
            return Err(format!(
                "the import {} takes {} arguments, and {} were given",
                self.name,
                self.params.len(),
                args.len()
            ));
        }
        let mut values = Vec::with_capacity(args.len());
        for (arg, scalar) in args.iter().zip(&self.params) {
            values.push(lift(arg.clone(), *scalar)?);
        }
        let results = (self.closure)(&values)?;
        match (self.result, results.as_slice()) {
            (None, []) => Ok(Vec::new()),
            (Some(scalar), [value]) if value.scalar() == Some(scalar) => lower_scalar(value)
                .map(|lowered| vec![lowered])
                .ok_or_else(|| format!("the import {} returned a string", self.name)),
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
                    .flat_map(|scalar| scalar.flat_params())
                    .collect(),
            ),
            results: ResultType(
                self.result
                    .map(|scalar| scalar.flat_params())
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
        for argument in &arguments {
            self.lower_argument(&export, argument, &mut core_arguments)
                .map_err(|message| ComponentError::Call { instance, message })?;
        }
        let core_results = self.call_core(instance, &export.core_export, core_arguments)?;
        match export.result {
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
        }
    }

    /// Lower one argument to core values, appending them. A string is copied into the callee's
    /// memory with its `realloc`, and what is appended is its pointer and length.
    fn lower_argument(
        &mut self,
        export: &LiftedExport,
        argument: &ComponentValue,
        out: &mut Vec<Value>,
    ) -> Result<(), String> {
        match argument {
            ComponentValue::String(text) => {
                let (Some(realloc), Some(memory)) = (&export.realloc, &export.memory) else {
                    return Err("a string argument needs a memory and a realloc".to_owned());
                };
                let length = i32::try_from(text.len())
                    .map_err(|_| "a string argument is too long for a core length".to_owned())?;
                // Allocate the bytes in the callee's memory, as the canonical ABI does for a
                // UTF-8 copy: realloc(0, 0, 1, length).
                let allocated = self
                    .call_core(
                        export.core_instance,
                        realloc,
                        vec![
                            Value::I32(0),
                            Value::I32(0),
                            Value::I32(1),
                            Value::I32(length),
                        ],
                    )
                    .map_err(|error| error.to_string())?;
                let pointer = match allocated.as_slice() {
                    [Value::I32(pointer)] => *pointer,
                    _ => return Err("realloc did not return one i32".to_owned()),
                };
                self.write_core_memory(
                    export.core_instance,
                    memory,
                    u64::from(pointer as u32),
                    text.as_bytes(),
                )
                .map_err(|error| error.to_string())?;
                out.push(Value::I32(pointer));
                out.push(Value::I32(length));
                Ok(())
            }
            ComponentValue::Record(fields) => {
                for (_, value) in fields {
                    out.push(
                        lower_scalar(value)
                            .ok_or_else(|| "a record field is a string".to_owned())?,
                    );
                }
                Ok(())
            }
            scalar => {
                out.push(lower_scalar(scalar).ok_or_else(|| "a scalar did not lower".to_owned())?);
                Ok(())
            }
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
            return self.load(export.core_instance, memory, *pointer as u32, ty);
        }
        match (ty, core_results) {
            (ComponentType::Scalar(scalar), [value]) => lift(value.clone(), *scalar),
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

    /// Load a value of type `ty` at `pointer` in the memory `memory` of core instance `instance`,
    /// by the canonical layout.
    fn load(
        &self,
        instance: usize,
        memory: &str,
        pointer: u32,
        ty: &ComponentType,
    ) -> Result<ComponentValue, String> {
        match ty {
            ComponentType::Scalar(Scalar::String) => {
                let header = self
                    .read_core_memory(instance, memory, u64::from(pointer), 8)
                    .map_err(|error| error.to_string())?;
                let data_pointer = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
                let data_length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
                let bytes = self
                    .read_core_memory(
                        instance,
                        memory,
                        u64::from(data_pointer),
                        data_length as usize,
                    )
                    .map_err(|error| error.to_string())?;
                String::from_utf8(bytes)
                    .map(ComponentValue::String)
                    .map_err(|_| "the string is not valid UTF-8".to_owned())
            }
            ComponentType::Scalar(scalar) => {
                self.load_scalar(instance, memory, u64::from(pointer), *scalar)
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
        let mut buffer = [0u8; 8];
        buffer[..size].copy_from_slice(&bytes);
        let value = match scalar {
            Scalar::S64 | Scalar::U64 | Scalar::F64 => Value::I64(i64::from_le_bytes(buffer)),
            Scalar::F32 => Value::F32(u32::from_le_bytes([
                buffer[0], buffer[1], buffer[2], buffer[3],
            ])),
            _ => Value::I32(i32::from_le_bytes([
                buffer[0], buffer[1], buffer[2], buffer[3],
            ])),
        };
        let value = match (scalar, value) {
            (Scalar::F64, Value::I64(bits)) => Value::F64(bits as u64),
            (_, value) => value,
        };
        lift(value, scalar)
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
        (value, ComponentType::Scalar(scalar)) => value.scalar() == Some(*scalar),
        _ => false,
    }
}

/// The core value a scalar lowers to, or `None` for a string or a record, which are not single
/// scalars. Signed and unsigned values share their bits.
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
        ComponentValue::String(_) | ComponentValue::Record(_) => None,
    }
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
            _ => {
                return Err(ComponentError::UnsupportedType {
                    what: "a type other than a function type or a record",
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
        let has_record = func
            .params
            .iter()
            .chain(func.result.iter())
            .any(|ty| matches!(ty, ComponentType::Record(_)));
        if has_record {
            return Err(ComponentError::UnsupportedImport {
                what: "import with a record in its type",
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
                    if func
                        .params
                        .iter()
                        .chain(func.result.iter())
                        .any(|ty| holds_string(ty) || matches!(ty, ComponentType::Record(_)))
                    {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "lowering a function with strings or records",
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
        let mut utf8 = false;
        for _ in 0..option_count {
            let option_at = reader.position();
            match reader.byte()? {
                OPTION_UTF8 => {
                    if utf8 {
                        return Err(ComponentError::Malformed { offset: option_at });
                    }
                    utf8 = true;
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
                _ => {
                    return Err(ComponentError::UnsupportedCanon {
                        what: "a canonical option other than utf8, memory, or realloc",
                        offset: option_at,
                    })
                }
            }
        }

        let ty = reader.u32()?;
        let Some(ComponentTypeDef::Func(func)) = types.get(ty as usize) else {
            return Err(ComponentError::NoSuchType { index: ty });
        };
        if flat_param_count(func, types) > MAX_FLAT_PARAMS {
            return Err(ComponentError::UnsupportedCanon {
                what: "more than 16 flat parameters",
                offset: at,
            });
        }
        let has_string = func
            .params
            .iter()
            .chain(func.result.iter())
            .any(holds_string);
        let indirect_result = func
            .result
            .map(|ty| flat_of(&ty, types).len() > MAX_FLAT_RESULTS)
            .unwrap_or(false);
        if (has_string || indirect_result) && memory.is_none() {
            return Err(ComponentError::UnsupportedCanon {
                what: "a string or an indirect result without a memory option",
                offset: at,
            });
        }
        if func.params.iter().any(holds_string) && realloc.is_none() {
            return Err(ComponentError::UnsupportedCanon {
                what: "a string parameter without a realloc option",
                offset: at,
            });
        }
        functions.push(ComponentFunction::Lift {
            core_function: index,
            ty,
            memory,
            realloc,
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
