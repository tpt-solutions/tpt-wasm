// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

#![forbid(unsafe_code)]
//! The Component Model envelope: types, imports, canonical functions, aliases, core
//! instances, and exports.
//!
//! A component is a binary with the Core Wasm magic, the version `0x0d` and the layer
//! `0x01`, so it cannot be mistaken for a core module, whose layer is zero. After the
//! preamble comes a sequence of sections: each is an id byte, a `u32` size, and the
//! contents. This crate reads that framing, collects the core modules a component nests,
//! decodes the type (7), import (10), canon (8), alias (6), core-instance (2) and export (11)
//! sections, and runs the instances on a runtime `Engine`.
//!
//! The values that cross the boundary are scalars (`s32`, `u32`, `s64`, `u64`, `f32`, `f64`)
//! and UTF-8 strings. A scalar is one core value reinterpreted at the boundary. A string is
//! two core values, a pointer and a length, into the instance's linear memory, and it is
//! copied in through the instance's `realloc`, and copied out from its memory. A component
//! function is either an import the embedder supplies, or a core function lifted to a
//! component type. An import can be lowered to a core function, which a core module calls
//! back into the embedder through. Every other section, and every form the crate does not
//! implement, is refused by name and offset rather than skipped.

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

/// The component type tag of a function type.
const TYPE_FUNC: u8 = 0x40;
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
const VALTYPE_STRING: u8 = 0x73;
const VALTYPE_CHAR: u8 = 0x74;
const VALTYPE_U16: u8 = 0x7b;
const VALTYPE_S16: u8 = 0x7c;
const VALTYPE_U8: u8 = 0x7d;
const VALTYPE_S8: u8 = 0x7e;
const VALTYPE_BOOL: u8 = 0x7f;

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
    /// An import is not granted the `call` operation on its capability.
    CapabilityDenied { name: String, message: String },
    /// The embedder supplied a function whose type differs from the component's import.
    ImportMismatch { name: String, message: String },
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
            ComponentError::CapabilityDenied { name, message } => {
                write!(f, "the import {name} is not granted: {message}")
            }
            ComponentError::ImportMismatch { name, message } => {
                write!(
                    f,
                    "the supplied {name} does not match the component's import: {message}"
                )
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

/// A component value type: a scalar, or a UTF-8 string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scalar {
    S32,
    U32,
    S64,
    U64,
    F32,
    F64,
    /// A UTF-8 string, passed as a pointer and a length into linear memory.
    String,
    /// A boolean, one `i32`: zero is false, and any other value is true.
    Bool,
    S8,
    U8,
    S16,
    U16,
    /// A Unicode scalar value, one `i32`, which must not be a surrogate or above `0x10FFFF`.
    Char,
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

    /// The core result this type flattens to: a string is returned as a pointer to its
    /// pointer and length in memory.
    fn flat_result(self) -> Vec<ValueType> {
        vec![self.single()]
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
}

impl ComponentValue {
    /// The type of this value.
    pub fn scalar(&self) -> Scalar {
        match self {
            ComponentValue::S32(_) => Scalar::S32,
            ComponentValue::U32(_) => Scalar::U32,
            ComponentValue::S64(_) => Scalar::S64,
            ComponentValue::U64(_) => Scalar::U64,
            ComponentValue::F32(_) => Scalar::F32,
            ComponentValue::F64(_) => Scalar::F64,
            ComponentValue::String(_) => Scalar::String,
            ComponentValue::Bool(_) => Scalar::Bool,
            ComponentValue::S8(_) => Scalar::S8,
            ComponentValue::U8(_) => Scalar::U8,
            ComponentValue::S16(_) => Scalar::S16,
            ComponentValue::U16(_) => Scalar::U16,
            ComponentValue::Char(_) => Scalar::Char,
        }
    }
}

/// A component function type: parameter types and at most one result type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentFuncType {
    /// The parameter types, in order.
    pub params: Vec<Scalar>,
    /// The result type, if the function returns one.
    pub result: Option<Scalar>,
}

impl ComponentFuncType {
    /// Whether any parameter or the result is a string.
    fn has_string(&self) -> bool {
        self.params.contains(&Scalar::String) || self.result == Some(Scalar::String)
    }

    /// The number of core parameters the function takes directly.
    fn flat_param_count(&self) -> usize {
        self.params
            .iter()
            .map(|scalar| scalar.flat_params().len())
            .sum()
    }
}

/// A component function: one entry of the component function index space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentFunction {
    /// A function the embedder supplies, under this name, of the type with index `ty`.
    Import { name: String, ty: u32 },
    /// A core function lifted to the type with index `ty`. Its `memory` and `realloc` are the
    /// core memory and function that a string crosses through, when it has one.
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

/// The operation an import's capability supports: calling the embedder's function.
const CALL_OPERATION: &str = "call";

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
    types: Vec<ComponentFuncType>,
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

    /// The component function types, in index order.
    pub fn component_types(&self) -> &[ComponentFuncType] {
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
    /// Every import must be supplied. Each lifted function's core type is then checked against
    /// its component type, its memory and `realloc` are checked to exist, and the exports are
    /// recorded.
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
                let declared = &self.types[*ty as usize];
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
            let expected = flat_signature(&self.types[*ty as usize]);
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
            let ty = &self.types[*ty as usize];
            exports.push(LiftedExport {
                name: export.name.clone(),
                params: ty.params.clone(),
                result: ty.result,
                core_instance: *instance as usize,
                core_export: core_export.clone(),
                memory: memory.map(|index| self.core_memories[index as usize].export.clone()),
                realloc: realloc.and_then(|index| match &self.core_functions[index as usize] {
                    CoreFunction::Alias { export, .. } => Some(export.clone()),
                    CoreFunction::Lowered { .. } => None,
                }),
            });
        }
        Ok(ComponentInstance { slots, exports })
    }

    /// The host function a lowered import is, given the embedder's implementation.
    fn lowered(&self, function: u32, imports: &Imports) -> Result<LoweredHost, String> {
        let ComponentFunction::Import { name, ty } = &self.functions[function as usize] else {
            return Err("a lowered function is not an import".to_owned());
        };
        let closure = imports
            .functions
            .get(name)
            .map(|supplied| supplied.closure.clone())
            .ok_or_else(|| format!("the import {name} was not supplied"))?;
        let ty = &self.types[*ty as usize];
        Ok(LoweredHost {
            name: name.clone(),
            closure,
            params: ty.params.clone(),
            result: ty.result,
            capabilities: imports.capabilities.clone(),
        })
    }
}

/// The core signature a component function of type `ty` flattens to.
fn flat_signature(ty: &ComponentFuncType) -> FunctionType {
    FunctionType {
        params: ResultType(
            ty.params
                .iter()
                .flat_map(|scalar| scalar.flat_params())
                .collect(),
        ),
        results: ResultType(ty.result.map(Scalar::flat_result).unwrap_or_default()),
    }
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
            (Some(scalar), [value]) if value.scalar() == scalar => lower_scalar(value)
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
            results: ResultType(self.result.map(Scalar::flat_result).unwrap_or_default()),
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
    params: Vec<Scalar>,
    result: Option<Scalar>,
    core_instance: usize,
    core_export: String,
    /// The export name of the memory a string crosses through, when the function has one.
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

/// The core instances of a running component, and its lifted exports.
#[derive(Debug)]
pub struct ComponentInstance {
    slots: Vec<Slot>,
    exports: Vec<LiftedExport>,
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
    /// through its `realloc`, and a string result is copied out of it. The result is lifted
    /// back to the export's result type.
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
        let instance = export.core_instance;
        let mut core_arguments = Vec::with_capacity(arguments.len());
        for (position, (expected, argument)) in export.params.iter().zip(&arguments).enumerate() {
            if argument.scalar() != *expected {
                return Err(ComponentError::ArgumentMismatch {
                    export: name.to_owned(),
                    message: format!(
                        "argument {position} is {:?}, but {:?} is expected",
                        argument.scalar(),
                        expected
                    ),
                });
            }
            match argument {
                ComponentValue::String(text) => {
                    let (Some(realloc), Some(memory)) = (&export.realloc, &export.memory) else {
                        return Err(ComponentError::Call {
                            instance,
                            message: "a string argument needs a memory and a realloc".to_owned(),
                        });
                    };
                    let length = i32::try_from(text.len()).map_err(|_| {
                        ComponentError::ArgumentMismatch {
                            export: name.to_owned(),
                            message: format!("argument {position} is too long for a core length"),
                        }
                    })?;
                    // Allocate the bytes in the callee's memory, as the canonical ABI does for a
                    // UTF-8 copy: realloc(0, 0, 1, length).
                    let allocated = self.call_core(
                        instance,
                        realloc,
                        vec![
                            Value::I32(0),
                            Value::I32(0),
                            Value::I32(1),
                            Value::I32(length),
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
                    self.write_core_memory(
                        instance,
                        memory,
                        u64::from(pointer as u32),
                        text.as_bytes(),
                    )?;
                    core_arguments.push(Value::I32(pointer));
                    core_arguments.push(Value::I32(length));
                }
                scalar => {
                    let value = lower_scalar(scalar).ok_or_else(|| ComponentError::Call {
                        instance,
                        message: "a scalar argument did not lower".to_owned(),
                    })?;
                    core_arguments.push(value);
                }
            }
        }
        let core_results = self.call_core(instance, &export.core_export, core_arguments)?;
        match (export.result, core_results.as_slice()) {
            (None, []) => Ok(Vec::new()),
            (Some(Scalar::String), [Value::I32(pointer)]) => {
                let memory = export
                    .memory
                    .as_deref()
                    .ok_or_else(|| ComponentError::Call {
                        instance,
                        message: "a string result needs a memory".to_owned(),
                    })?;
                // The core function returns a pointer to the string's pointer and length.
                let header =
                    self.read_core_memory(instance, memory, u64::from(*pointer as u32), 8)?;
                let data_pointer = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
                let data_length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
                let bytes = self.read_core_memory(
                    instance,
                    memory,
                    u64::from(data_pointer),
                    data_length as usize,
                )?;
                let text = String::from_utf8(bytes).map_err(|_| ComponentError::Call {
                    instance,
                    message: "the string is not valid UTF-8".to_owned(),
                })?;
                Ok(vec![ComponentValue::String(text)])
            }
            (Some(scalar), [value]) => lift(value.clone(), scalar)
                .map(|value| vec![value])
                .map_err(|message| ComponentError::Call { instance, message }),
            _ => Err(ComponentError::Call {
                instance,
                message: "the core function returned a different number of results than its type"
                    .to_owned(),
            }),
        }
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

/// The core value a scalar lowers to, or `None` for a string, which is copied through memory.
/// Signed and unsigned values share their bits.
fn lower_scalar(value: &ComponentValue) -> Option<Value> {
    match value {
        ComponentValue::S32(value) => Some(Value::I32(*value)),
        ComponentValue::U32(value) => Some(Value::I32(*value as i32)),
        ComponentValue::S64(value) => Some(Value::I64(*value)),
        ComponentValue::U64(value) => Some(Value::I64(*value as i64)),
        ComponentValue::F32(value) => Some(Value::F32(value.to_bits())),
        ComponentValue::F64(value) => Some(Value::F64(value.to_bits())),
        ComponentValue::String(_) => None,
        ComponentValue::Bool(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::S8(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::U8(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::S16(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::U16(value) => Some(Value::I32(i32::from(*value))),
        ComponentValue::Char(value) => Some(Value::I32(*value as u32 as i32)),
    }
}

/// The scalar a core value lifts to, when the core value has the scalar's type. A string is
/// not lifted from a single value: it is read from memory.
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

/// Decode a value type byte at `offset`, refusing every type this crate does not support.
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

/// Decode a type section's contents: a count, then each function type. A function type is
/// its tag, a labelled parameter list, and a result: `0x00` and a value type, or `0x01 0x00`
/// for none.
fn decode_types(
    reader: &mut Reader<'_>,
    types: &mut Vec<ComponentFuncType>,
) -> Result<(), ComponentError> {
    let count = reader.u32()?;
    for _ in 0..count {
        let at = reader.position();
        if reader.byte()? != TYPE_FUNC {
            return Err(ComponentError::UnsupportedType {
                what: "a type other than a function type",
                offset: at,
            });
        }
        let parameter_count = reader.u32()?;
        let mut params = Vec::new();
        for _ in 0..parameter_count {
            let _label = reader.name()?;
            let at_type = reader.position();
            params.push(scalar(reader.byte()?, at_type)?);
        }
        let at_result = reader.position();
        let result = match reader.byte()? {
            0x00 => {
                let at_type = reader.position();
                Some(scalar(reader.byte()?, at_type)?)
            }
            0x01 => {
                if reader.byte()? != 0x00 {
                    return Err(ComponentError::Malformed { offset: at_result });
                }
                None
            }
            _ => return Err(ComponentError::Malformed { offset: at_result }),
        };
        types.push(ComponentFuncType { params, result });
    }
    Ok(())
}

/// Decode an import section's contents: a count, then each import. Only a function import,
/// with no attributes, is supported; the embedder supplies its implementation by name.
fn decode_imports(
    reader: &mut Reader<'_>,
    types: &[ComponentFuncType],
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
        if ty as usize >= types.len() {
            return Err(ComponentError::NoSuchType { index: ty });
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

/// Decode a canon section's contents: a count, then each canonical function.
///
/// A lift turns a core function into a component function. Its options may name a memory
/// and a `realloc` of the same instance, and a string in its type needs them: a string
/// parameter needs both, and a string result needs the memory. A lower turns an import into
/// a core function, and takes no options. Lowering a component's own function, lifting a
/// lowered one, and lowering an import with strings are refused.
fn decode_canon(
    reader: &mut Reader<'_>,
    core_functions: &mut Vec<CoreFunction>,
    core_memories: &[CoreMemory],
    types: &[ComponentFuncType],
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
                    if types[*ty as usize].has_string() {
                        return Err(ComponentError::UnsupportedCanon {
                            what: "lowering a function with strings",
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
        let Some(component_type) = types.get(ty as usize) else {
            return Err(ComponentError::NoSuchType { index: ty });
        };
        if component_type.flat_param_count() > MAX_FLAT_PARAMS {
            return Err(ComponentError::UnsupportedCanon {
                what: "more than 16 flat parameters",
                offset: at,
            });
        }
        if component_type.has_string() && memory.is_none() {
            return Err(ComponentError::UnsupportedCanon {
                what: "a string without a memory option",
                offset: at,
            });
        }
        if component_type.params.contains(&Scalar::String) && realloc.is_none() {
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
mod tests {
    use super::*;

    /// A core module with the magic and version 1 and no sections.
    const EMPTY_MODULE: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    /// A core module that exports `answer`, a function of no parameters returning `i32` 42.
    const ANSWER_MODULE: [u8; 39] = [
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic and version
        0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, // type: () -> i32
        0x03, 0x02, 0x01, 0x00, // function 0 has type 0
        0x07, 0x0a, 0x01, 0x06, 0x61, 0x6e, 0x73, 0x77, 0x65, 0x72, 0x00,
        0x00, // export "answer"
        0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x2a, 0x0b, // code: i32.const 42
    ];

    /// A core module that imports `env.answer`, a function of no parameters returning `i32`,
    /// and exports `relay`, which returns what `answer` returns.
    const RELAY_MODULE: [u8; 54] = [
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // magic and version
        0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, // type: () -> i32
        0x02, 0x0e, 0x01, 0x03, 0x65, 0x6e, 0x76, 0x06, 0x61, 0x6e, 0x73, 0x77, 0x65, 0x72, 0x00,
        0x00, // import env.answer, of type 0
        0x03, 0x02, 0x01, 0x00, // function 1 has type 0
        0x07, 0x09, 0x01, 0x05, 0x72, 0x65, 0x6c, 0x61, 0x79, 0x00, 0x01, // export "relay"
        0x0a, 0x06, 0x01, 0x04, 0x00, 0x10, 0x00, 0x0b, // code: call 0, end
    ];

    /// A component type `() -> s32`.
    const TYPE_NO_ARGUMENTS_S32: [u8; 7] = [SECTION_TYPE, 5, 1, TYPE_FUNC, 0, 0, VALTYPE_S32];
    /// A component type `() -> u32`.
    const TYPE_NO_ARGUMENTS_U32: [u8; 7] = [SECTION_TYPE, 5, 1, TYPE_FUNC, 0, 0, VALTYPE_U32];
    /// A component type `() -> s64`, which does not match the i32 the module returns.
    const TYPE_NO_ARGUMENTS_S64: [u8; 7] = [SECTION_TYPE, 5, 1, TYPE_FUNC, 0, 0, VALTYPE_S64];
    /// A component type `() -> ()`, which does not match the module's result.
    const TYPE_NO_RESULT: [u8; 7] = [SECTION_TYPE, 5, 1, TYPE_FUNC, 0, 1, 0];
    /// The canon that lifts core function 0 with no options, using type 0.
    const CANON_LIFT_ANSWER: [u8; 8] = [SECTION_CANON, 6, 1, CANON_LIFT, SORT_FUNC, 0, 0, 0];
    /// The export of component function 0 as `answer`.
    const EXPORT_ANSWER: [u8; 14] = [
        SECTION_EXPORT,
        12,
        1,
        0x00,
        6,
        b'a',
        b'n',
        b's',
        b'w',
        b'e',
        b'r',
        COMPONENT_SORT_FUNC,
        0,
        0x00,
    ];

    fn with(sections: &[u8]) -> Vec<u8> {
        let mut bytes = PREAMBLE.to_vec();
        bytes.extend_from_slice(sections);
        bytes
    }

    /// A section with `content`, whose size fits in one byte.
    fn section(id: u8, content: &[u8]) -> Vec<u8> {
        assert!(content.len() < 128, "the test section is too large");
        let mut out = vec![id, content.len() as u8];
        out.extend_from_slice(content);
        out
    }

    fn module_section(module: &[u8]) -> Vec<u8> {
        section(SECTION_CORE_MODULE, module)
    }

    /// Core instance 0 instantiates module 0 with no arguments.
    fn instantiate_module_zero() -> Vec<u8> {
        vec![SECTION_CORE_INSTANCE, 4, 1, INSTANTIATE, 0, 0]
    }

    /// Core function 0 is the export `answer` of core instance 0.
    fn alias_answer() -> Vec<u8> {
        vec![
            SECTION_ALIAS,
            12,
            1,
            SORT_CORE_WRAPPER,
            SORT_FUNC,
            ALIAS_CORE_EXPORT,
            0,
            6,
            b'a',
            b'n',
            b's',
            b'w',
            b'e',
            b'r',
        ]
    }

    /// Core instance 1 is an inline-exports instance exporting core function 0 as `answer`.
    fn inline_answer() -> Vec<u8> {
        vec![
            SECTION_CORE_INSTANCE,
            12,
            1,
            INLINE_EXPORTS,
            1,
            6,
            b'a',
            b'n',
            b's',
            b'w',
            b'e',
            b'r',
            SORT_FUNC,
            0,
        ]
    }

    /// A component that lifts `answer` of the answer module, with the given type, canon, and
    /// export sections.
    fn answer_component(types: &[u8], canon: &[u8], export: &[u8]) -> Vec<u8> {
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend(instantiate_module_zero());
        bytes.extend(alias_answer());
        bytes.extend_from_slice(types);
        bytes.extend_from_slice(canon);
        bytes.extend_from_slice(export);
        bytes
    }

    fn engine() -> Engine {
        Engine::new(tpt_wasm_runtime::Config::default()).expect("engine")
    }

    /// A core module that imports `env.add`, a function of two `i32` returning an `i32`, and
    /// exports `run`, which calls it with 2 and 3.
    fn host_user_module() -> Vec<u8> {
        let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        bytes.extend(section(
            1,
            &[2, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 0, 1, 0x7f],
        ));
        bytes.extend(section(
            2,
            &[1, 3, b'e', b'n', b'v', 3, b'a', b'd', b'd', 0, 0],
        ));
        bytes.extend(section(3, &[1, 1]));
        bytes.extend(section(7, &[1, 3, b'r', b'u', b'n', 0, 1]));
        bytes.extend(section(10, &[1, 8, 0, 0x41, 2, 0x41, 3, 0x10, 0, 0x0b]));
        bytes
    }

    /// A component that imports `add: (s32, s32) -> s32`, lowers it, instantiates the host
    /// user module with it, and exports `run: () -> s32`, which the module computes as
    /// `add(2, 3)`.
    fn host_component() -> Vec<u8> {
        // Type 0 is (s32, s32) -> s32; type 1 is () -> s32.
        let types = section(
            SECTION_TYPE,
            &[
                2,
                0x40,
                2,
                1,
                b'a',
                VALTYPE_S32,
                1,
                b'b',
                VALTYPE_S32,
                0,
                VALTYPE_S32,
                0x40,
                0,
                0,
                VALTYPE_S32,
            ],
        );
        let import = section(
            SECTION_IMPORT,
            &[1, 0x00, 3, b'a', b'd', b'd', EXTERN_FUNC, 0],
        );
        // Lower component function 0 (the import) into core function 0.
        let lower = section(SECTION_CANON, &[1, CANON_LOWER, SORT_FUNC, 0, 0]);
        // Core instance 0 is the inline "add"; core instance 1 runs the host user with "env"
        // bound to instance 0.
        let instances = section(
            SECTION_CORE_INSTANCE,
            &[
                2,
                0x01,
                1,
                3,
                b'a',
                b'd',
                b'd',
                SORT_FUNC,
                0,
                0x00,
                0,
                1,
                3,
                b'e',
                b'n',
                b'v',
                SORT_CORE_INSTANCE,
                0,
            ],
        );
        // Core function 1 is the export `run` of core instance 1.
        let alias = section(SECTION_ALIAS, &[1, 0, 0, 1, 1, 3, b'r', b'u', b'n']);
        // Component function 1 lifts core function 1 with type 1.
        let lift = section(SECTION_CANON, &[1, CANON_LIFT, SORT_FUNC, 1, 0, 1]);
        let export = section(
            SECTION_EXPORT,
            &[1, 0x00, 3, b'r', b'u', b'n', COMPONENT_SORT_FUNC, 1, 0x00],
        );
        let mut bytes = with(&module_section(&host_user_module()));
        bytes.extend(types);
        bytes.extend(import);
        bytes.extend(lower);
        bytes.extend(instances);
        bytes.extend(alias);
        bytes.extend(lift);
        bytes.extend(export);
        bytes
    }

    /// A core module with linear memory and `realloc`, and two functions over strings:
    /// `len(ptr, len) -> i32` returns its length argument, and `greet() -> i32` returns the
    /// address of a return area that holds a pointer and length to `"hello"`.
    fn string_module() -> Vec<u8> {
        let mut bytes = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        // Types: (i32, i32) -> i32; (i32, i32, i32, i32) -> i32; () -> i32.
        bytes.extend(section(
            1,
            &[
                3, 0x60, 2, 0x7f, 0x7f, 1, 0x7f, 0x60, 4, 0x7f, 0x7f, 0x7f, 0x7f, 1, 0x7f, 0x60, 0,
                1, 0x7f,
            ],
        ));
        // Functions: realloc has type 1, len has type 0, greet has type 2.
        bytes.extend(section(3, &[3, 1, 0, 2]));
        // One memory of one page.
        bytes.extend(section(5, &[1, 0x00, 1]));
        // Exports: "memory", "realloc" (function 0), "len" (function 1), "greet" (function 2).
        bytes.extend(section(
            7,
            &[
                4, 6, b'm', b'e', b'm', b'o', b'r', b'y', 2, 0, 7, b'r', b'e', b'a', b'l', b'l',
                b'o', b'c', 0, 0, 3, b'l', b'e', b'n', 0, 1, 5, b'g', b'r', b'e', b'e', b't', 0, 2,
            ],
        ));
        // Bodies: realloc returns 1024; len returns its second parameter; greet returns 2048.
        bytes.extend(section(
            10,
            &[
                3, 5, 0x00, 0x41, 0x80, 0x08, 0x0b, 4, 0x00, 0x20, 0x01, 0x0b, 5, 0x00, 0x41, 0x80,
                0x10, 0x0b,
            ],
        ));
        // Data at 2048: a pointer to "hello" (at 2056) and its length, then "hello".
        bytes.extend(section(
            11,
            &[
                1, 0x00, 0x41, 0x80, 0x10, 0x0b, 13, 0x08, 0x08, 0, 0, 5, 0, 0, 0, b'h', b'e',
                b'l', b'l', b'o',
            ],
        ));
        bytes
    }

    /// A component over `string_module`. Its `len` lifts with the given canon entry, and
    /// `greet` lifts with a memory, so the string result can be read.
    fn string_component(len_canon: &[u8]) -> Vec<u8> {
        // Types: 0 is (string) -> s32; 1 is () -> string.
        let types = section(
            SECTION_TYPE,
            &[
                2,
                0x40,
                1,
                1,
                b's',
                VALTYPE_STRING,
                0,
                VALTYPE_S32,
                0x40,
                0,
                0,
                VALTYPE_STRING,
            ],
        );
        let instances = section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]);
        // Core functions: realloc (0), len (1), greet (2). Core memory: memory (0).
        let alias = section(
            SECTION_ALIAS,
            &[
                4, 0, 0, 1, 0, 7, b'r', b'e', b'a', b'l', b'l', b'o', b'c', 0, 0, 1, 0, 3, b'l',
                b'e', b'n', 0, 0, 1, 0, 5, b'g', b'r', b'e', b'e', b't', 0, 2, 1, 0, 6, b'm', b'e',
                b'm', b'o', b'r', b'y',
            ],
        );
        // Canon: lift len with `len_canon`; lift greet with the memory option.
        let mut canon = vec![2];
        canon.extend_from_slice(len_canon);
        canon.extend_from_slice(&[CANON_LIFT, SORT_FUNC, 2, 1, OPTION_MEMORY, 0, 1]);
        let canon = section(SECTION_CANON, &canon);
        let export = section(
            SECTION_EXPORT,
            &[
                2,
                0x00,
                3,
                b'l',
                b'e',
                b'n',
                COMPONENT_SORT_FUNC,
                0,
                0x00,
                0x00,
                5,
                b'g',
                b'r',
                b'e',
                b'e',
                b't',
                COMPONENT_SORT_FUNC,
                1,
                0x00,
            ],
        );
        let mut bytes = with(&module_section(&string_module()));
        bytes.extend(types);
        bytes.extend(instances);
        bytes.extend(alias);
        bytes.extend(canon);
        bytes.extend(export);
        bytes
    }

    /// The canon entry that lifts `len` with a memory and a realloc, which is complete.
    fn complete_len_canon() -> Vec<u8> {
        vec![
            CANON_LIFT,
            SORT_FUNC,
            1,
            2,
            OPTION_MEMORY,
            0,
            OPTION_REALLOC,
            0,
            0,
        ]
    }

    #[test]
    fn a_preamble_alone_is_an_empty_component() {
        let component = Component::decode(&PREAMBLE).expect("decodes");
        assert!(component.core_modules().is_empty());
        assert!(component.core_instances().is_empty());
        assert!(component.core_functions().is_empty());
        assert!(component.core_memories().is_empty());
        assert!(component.component_types().is_empty());
        assert!(component.component_exports().is_empty());
    }

    #[test]
    fn a_core_module_section_is_collected_with_its_bytes() {
        let component = Component::decode(&with(&module_section(&EMPTY_MODULE))).expect("decodes");
        assert_eq!(component.core_modules(), [EMPTY_MODULE.to_vec()]);
    }

    #[test]
    fn custom_sections_are_skipped_and_modules_keep_their_order() {
        let mut bytes = with(&[SECTION_CUSTOM, 3, 0x61, 0x62, 0x63]);
        bytes.extend(module_section(&EMPTY_MODULE));
        bytes.extend(module_section(&ANSWER_MODULE));
        let component = Component::decode(&bytes).expect("decodes");
        assert_eq!(component.core_modules().len(), 2);
        assert_eq!(component.core_modules()[1], ANSWER_MODULE.to_vec());
    }

    #[test]
    fn a_core_module_is_not_a_component() {
        assert_eq!(
            Component::decode(&EMPTY_MODULE),
            Err(ComponentError::NotAComponent)
        );
    }

    #[test]
    fn the_wrong_magic_or_layer_is_not_a_component() {
        let mut bad_magic = PREAMBLE;
        bad_magic[0] = 0x01;
        assert_eq!(
            Component::decode(&bad_magic),
            Err(ComponentError::NotAComponent)
        );
        let mut bad_layer = PREAMBLE;
        bad_layer[6] = 0x00;
        assert_eq!(
            Component::decode(&bad_layer),
            Err(ComponentError::NotAComponent)
        );
        assert_eq!(Component::decode(&[]), Err(ComponentError::NotAComponent));
    }

    #[test]
    fn a_section_kind_not_yet_implemented_is_refused_by_id() {
        // Id 4 is the nested component section.
        assert_eq!(
            Component::decode(&with(&[4, 0])),
            Err(ComponentError::UnsupportedSection { id: 4, offset: 8 })
        );
    }

    #[test]
    fn a_section_that_runs_past_the_end_is_truncated() {
        assert_eq!(
            Component::decode(&with(&[SECTION_CORE_MODULE, 9, 0, 0])),
            Err(ComponentError::Truncated { offset: 10 })
        );
    }

    #[test]
    fn an_unterminated_size_is_truncated() {
        assert_eq!(
            Component::decode(&with(&[SECTION_CORE_MODULE, 0x80])),
            Err(ComponentError::Truncated { offset: 10 })
        );
    }

    #[test]
    fn a_size_past_u32_is_an_invalid_integer() {
        assert_eq!(
            Component::decode(&with(&[SECTION_CORE_MODULE, 0x80, 0x80, 0x80, 0x80, 0x10])),
            Err(ComponentError::InvalidInteger { offset: 9 })
        );
    }

    #[test]
    fn an_instantiate_of_a_nested_module_is_decoded() {
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend(instantiate_module_zero());
        let component = Component::decode(&bytes).expect("decodes");
        assert_eq!(
            component.core_instances(),
            [CoreInstance::Instantiate {
                module: 0,
                arguments: Vec::new(),
            }]
        );
    }

    #[test]
    fn an_instance_naming_a_missing_module_is_refused() {
        let bytes = with(&[SECTION_CORE_INSTANCE, 4, 1, INSTANTIATE, 3, 0]);
        assert_eq!(
            Component::decode(&bytes),
            Err(ComponentError::NoSuchModule { module: 3 })
        );
    }

    #[test]
    fn an_instance_section_with_leftover_bytes_is_malformed() {
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend_from_slice(&[SECTION_CORE_INSTANCE, 5, 0, 0xff, 0xff, 0xff, 0xff]);
        assert!(matches!(
            Component::decode(&bytes),
            Err(ComponentError::Malformed { .. })
        ));
    }

    #[test]
    fn an_alias_of_a_core_export_defines_a_core_function() {
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend(instantiate_module_zero());
        bytes.extend(alias_answer());
        let component = Component::decode(&bytes).expect("decodes");
        assert_eq!(
            component.core_functions(),
            [CoreFunction::Alias {
                instance: 0,
                export: "answer".to_owned(),
            }]
        );
    }

    #[test]
    fn an_alias_of_an_undeclared_instance_is_refused() {
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend(alias_answer());
        assert_eq!(
            Component::decode(&bytes),
            Err(ComponentError::NoSuchInstance { instance: 0 })
        );
    }

    #[test]
    fn an_alias_of_another_core_sort_is_refused() {
        // A table: core sort 1 instead of function or memory.
        let bytes = with(&[
            SECTION_ALIAS,
            6,
            1,
            SORT_CORE_WRAPPER,
            1,
            ALIAS_CORE_EXPORT,
            0,
            0,
        ]);
        assert_eq!(
            Component::decode(&bytes),
            Err(ComponentError::UnsupportedAlias {
                what: "alias of a core sort other than function or memory",
                offset: 11,
            })
        );
    }

    #[test]
    fn an_alias_of_a_memory_defines_a_core_memory() {
        let mut bytes = with(&module_section(&string_module()));
        bytes.extend(section(SECTION_CORE_INSTANCE, &[1, INSTANTIATE, 0, 0]));
        bytes.extend(section(
            SECTION_ALIAS,
            &[
                1,
                0,
                SORT_CORE_MEMORY,
                1,
                0,
                6,
                b'm',
                b'e',
                b'm',
                b'o',
                b'r',
                b'y',
            ],
        ));
        let component = Component::decode(&bytes).expect("decodes");
        assert_eq!(
            component.core_memories(),
            [CoreMemory {
                instance: 0,
                export: "memory".to_owned(),
            }]
        );
    }

    #[test]
    fn an_inline_export_of_a_missing_function_is_refused() {
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend(instantiate_module_zero());
        let mut section = inline_answer();
        let last = section.len() - 1;
        section[last] = 3;
        bytes.extend(section);
        assert_eq!(
            Component::decode(&bytes),
            Err(ComponentError::NoSuchFunction { function: 3 })
        );
    }

    #[test]
    fn an_inline_exports_instance_runs_the_function_it_names() {
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend(instantiate_module_zero());
        bytes.extend(alias_answer());
        bytes.extend(inline_answer());
        let component = Component::decode(&bytes).expect("decodes");
        let mut instance = component.instantiate(&engine()).expect("instantiates");
        assert_eq!(instance.core_instance_count(), 2);
        assert_eq!(
            instance.call_core(1, "answer", Vec::new()),
            Ok(vec![Value::I32(42)])
        );
        assert_eq!(
            instance.call_core(0, "answer", Vec::new()),
            Ok(vec![Value::I32(42)])
        );
        assert!(matches!(
            instance.call_core(1, "missing", Vec::new()),
            Err(ComponentError::Call { instance: 1, .. })
        ));
    }

    #[test]
    fn an_import_binds_to_an_inline_export() {
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend(module_section(&RELAY_MODULE));
        bytes.extend(instantiate_module_zero());
        bytes.extend(alias_answer());
        bytes.extend(inline_answer());
        // Core instance 2 instantiates module 1 (relay) with "env" bound to instance 1.
        bytes.extend_from_slice(&[
            SECTION_CORE_INSTANCE,
            10,
            1,
            INSTANTIATE,
            1,
            1,
            3,
            b'e',
            b'n',
            b'v',
            SORT_CORE_INSTANCE,
            1,
        ]);
        let component = Component::decode(&bytes).expect("decodes");
        let mut instance = component.instantiate(&engine()).expect("instantiates");
        assert_eq!(
            instance.call_core(2, "relay", Vec::new()),
            Ok(vec![Value::I32(42)])
        );
    }

    #[test]
    fn an_inline_export_of_a_name_the_source_lacks_fails_at_instantiation() {
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend(instantiate_module_zero());
        // An alias to "missing", which the module does not export.
        bytes.extend_from_slice(&[
            SECTION_ALIAS,
            13,
            1,
            SORT_CORE_WRAPPER,
            SORT_FUNC,
            ALIAS_CORE_EXPORT,
            0,
            7,
            b'm',
            b'i',
            b's',
            b's',
            b'i',
            b'n',
            b'g',
        ]);
        bytes.extend(inline_answer());
        let component = Component::decode(&bytes).expect("decodes");
        let error = component
            .instantiate(&engine())
            .expect_err("the export is missing");
        assert!(matches!(
            error,
            ComponentError::Instantiate { instance: 1, .. }
        ));
    }

    #[test]
    fn an_argument_naming_a_later_instance_is_refused() {
        let mut bytes = with(&module_section(&RELAY_MODULE));
        // Module 0 with "env" bound to core instance 5, which is not declared yet.
        bytes.extend_from_slice(&[
            SECTION_CORE_INSTANCE,
            10,
            1,
            INSTANTIATE,
            0,
            1,
            3,
            b'e',
            b'n',
            b'v',
            SORT_CORE_INSTANCE,
            5,
        ]);
        assert_eq!(
            Component::decode(&bytes),
            Err(ComponentError::NoSuchInstance { instance: 5 })
        );
    }

    #[test]
    fn a_duplicate_argument_name_is_malformed() {
        let mut bytes = with(&module_section(&RELAY_MODULE));
        bytes.extend(instantiate_module_zero());
        let env = [3, b'e', b'n', b'v', SORT_CORE_INSTANCE, 0];
        let mut section = vec![SECTION_CORE_INSTANCE, 16, 1, INSTANTIATE, 0, 2];
        section.extend_from_slice(&env);
        section.extend_from_slice(&env);
        bytes.extend_from_slice(&section);
        assert!(matches!(
            Component::decode(&bytes),
            Err(ComponentError::Malformed { .. })
        ));
    }

    #[test]
    fn an_unresolved_import_fails_to_instantiate() {
        let mut bytes = with(&module_section(&RELAY_MODULE));
        bytes.extend(instantiate_module_zero());
        let component = Component::decode(&bytes).expect("decodes");
        let error = component
            .instantiate(&engine())
            .expect_err("the import is missing");
        assert!(matches!(
            error,
            ComponentError::Instantiate { instance: 0, .. }
        ));
    }

    #[test]
    fn a_nested_module_instantiates_and_its_export_runs() {
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend(instantiate_module_zero());
        let component = Component::decode(&bytes).expect("decodes");
        let mut instance = component.instantiate(&engine()).expect("instantiates");
        assert_eq!(instance.core_instance_count(), 1);
        assert_eq!(
            instance.call_core(0, "answer", Vec::new()),
            Ok(vec![Value::I32(42)])
        );
        assert!(matches!(
            instance.call_core(1, "answer", Vec::new()),
            Err(ComponentError::NoSuchInstance { instance: 1 })
        ));
    }

    #[test]
    fn a_component_with_no_instances_instantiates_to_nothing() {
        let component = Component::decode(&with(&module_section(&EMPTY_MODULE))).expect("decodes");
        let instance = component.instantiate(&engine()).expect("instantiates");
        assert_eq!(instance.core_instance_count(), 0);
        assert!(instance.export_names().is_empty());
    }

    #[test]
    fn a_lifted_export_returns_a_typed_value() {
        let bytes = answer_component(&TYPE_NO_ARGUMENTS_S32, &CANON_LIFT_ANSWER, &EXPORT_ANSWER);
        let component = Component::decode(&bytes).expect("decodes");
        assert_eq!(component.component_exports()[0].name, "answer");
        let mut instance = component.instantiate(&engine()).expect("instantiates");
        assert_eq!(instance.export_names(), ["answer"]);
        assert_eq!(
            instance.call_export("answer", Vec::new()),
            Ok(vec![ComponentValue::S32(42)])
        );
    }

    #[test]
    fn the_same_bits_lift_to_the_declared_signedness() {
        let bytes = answer_component(&TYPE_NO_ARGUMENTS_U32, &CANON_LIFT_ANSWER, &EXPORT_ANSWER);
        let mut instance = Component::decode(&bytes)
            .expect("decodes")
            .instantiate(&engine())
            .expect("instantiates");
        assert_eq!(
            instance.call_export("answer", Vec::new()),
            Ok(vec![ComponentValue::U32(42)])
        );
    }

    #[test]
    fn a_result_of_the_wrong_core_type_is_a_lift_mismatch() {
        let bytes = answer_component(&TYPE_NO_ARGUMENTS_S64, &CANON_LIFT_ANSWER, &EXPORT_ANSWER);
        let error = Component::decode(&bytes)
            .expect("decodes")
            .instantiate(&engine())
            .expect_err("i64 is not i32");
        assert!(matches!(
            error,
            ComponentError::LiftMismatch { ref export, .. } if export == "answer"
        ));
    }

    #[test]
    fn a_missing_result_is_a_lift_mismatch() {
        let bytes = answer_component(&TYPE_NO_RESULT, &CANON_LIFT_ANSWER, &EXPORT_ANSWER);
        let error = Component::decode(&bytes)
            .expect("decodes")
            .instantiate(&engine())
            .expect_err("the module returns a value");
        assert!(matches!(error, ComponentError::LiftMismatch { .. }));
    }

    #[test]
    fn calls_check_the_argument_count_and_the_export_name() {
        let bytes = answer_component(&TYPE_NO_ARGUMENTS_S32, &CANON_LIFT_ANSWER, &EXPORT_ANSWER);
        let mut instance = Component::decode(&bytes)
            .expect("decodes")
            .instantiate(&engine())
            .expect("instantiates");
        assert!(matches!(
            instance.call_export("answer", vec![ComponentValue::S32(1)]),
            Err(ComponentError::ArgumentMismatch { .. })
        ));
        assert!(matches!(
            instance.call_export("missing", Vec::new()),
            Err(ComponentError::NoSuchExport { .. })
        ));
    }

    #[test]
    fn canon_options_are_refused_and_a_lower_needs_an_import() {
        // A lift with one option that is not utf8, memory, or realloc.
        let with_options = [SECTION_CANON, 6, 1, CANON_LIFT, SORT_FUNC, 0, 1, 0x05];
        assert!(matches!(
            Component::decode(&answer_component(
                &TYPE_NO_ARGUMENTS_S32,
                &with_options,
                &[]
            )),
            Err(ComponentError::UnsupportedCanon {
                what: "a canonical option other than utf8, memory, or realloc",
                ..
            })
        ));
        // With no functions defined, a lower of function 0 names nothing.
        let lower = [SECTION_CANON, 5, 1, CANON_LOWER, SORT_FUNC, 0, 0];
        assert_eq!(
            Component::decode(&answer_component(&TYPE_NO_ARGUMENTS_S32, &lower, &[])),
            Err(ComponentError::NoSuchComponentFunction { function: 0 })
        );
    }

    #[test]
    fn a_type_outside_the_supported_set_is_refused() {
        // error-context, a primitive type this crate does not support yet.
        let bool_result = [SECTION_TYPE, 5, 1, TYPE_FUNC, 0, 0, 0x64];
        assert!(matches!(
            Component::decode(&answer_component(&bool_result, &CANON_LIFT_ANSWER, &[])),
            Err(ComponentError::UnsupportedType { .. })
        ));
    }

    #[test]
    fn an_export_of_another_sort_or_a_missing_function_is_refused() {
        let value_sort = [
            SECTION_EXPORT,
            12,
            1,
            0x00,
            6,
            b'a',
            b'n',
            b's',
            b'w',
            b'e',
            b'r',
            0x02,
            0,
            0x00,
        ];
        assert!(matches!(
            Component::decode(&answer_component(
                &TYPE_NO_ARGUMENTS_S32,
                &CANON_LIFT_ANSWER,
                &value_sort
            )),
            Err(ComponentError::UnsupportedExport { .. })
        ));
        let missing_function = [
            SECTION_EXPORT,
            12,
            1,
            0x00,
            6,
            b'a',
            b'n',
            b's',
            b'w',
            b'e',
            b'r',
            COMPONENT_SORT_FUNC,
            5,
            0x00,
        ];
        assert_eq!(
            Component::decode(&answer_component(
                &TYPE_NO_ARGUMENTS_S32,
                &CANON_LIFT_ANSWER,
                &missing_function
            )),
            Err(ComponentError::NoSuchComponentFunction { function: 5 })
        );
    }

    #[test]
    fn an_import_with_attributes_or_another_sort_is_refused() {
        let with_attributes = section(SECTION_IMPORT, &[1, 0x02, 3, b'a', b'd', b'd', 0, 0]);
        assert!(matches!(
            Component::decode(&with(&with_attributes)),
            Err(ComponentError::UnsupportedImport { .. })
        ));
        let instance_sort = section(SECTION_IMPORT, &[1, 0x00, 3, b'a', b'd', b'd', 0x05, 0]);
        assert!(matches!(
            Component::decode(&with(&instance_sort)),
            Err(ComponentError::UnsupportedImport { .. })
        ));
    }

    #[test]
    fn an_export_of_an_imported_function_is_refused() {
        let types = section(SECTION_TYPE, &[1, 0x40, 0, 0, VALTYPE_S32]);
        let import = section(
            SECTION_IMPORT,
            &[1, 0x00, 3, b'a', b'd', b'd', EXTERN_FUNC, 0],
        );
        let export = section(
            SECTION_EXPORT,
            &[1, 0x00, 3, b'a', b'd', b'd', COMPONENT_SORT_FUNC, 0, 0x00],
        );
        let mut bytes = with(&types);
        bytes.extend(import);
        bytes.extend(export);
        assert!(matches!(
            Component::decode(&bytes),
            Err(ComponentError::UnsupportedExport {
                what: "export of an imported function",
                ..
            })
        ));
    }

    #[test]
    fn lifting_a_lowered_function_is_refused() {
        let types = section(SECTION_TYPE, &[1, 0x40, 0, 0, VALTYPE_S32]);
        let import = section(
            SECTION_IMPORT,
            &[1, 0x00, 3, b'a', b'd', b'd', EXTERN_FUNC, 0],
        );
        let lower = section(SECTION_CANON, &[1, CANON_LOWER, SORT_FUNC, 0, 0]);
        let lift_lowered = section(SECTION_CANON, &[1, CANON_LIFT, SORT_FUNC, 0, 0, 0]);
        let mut bytes = with(&types);
        bytes.extend(import);
        bytes.extend(lower);
        bytes.extend(lift_lowered);
        assert!(matches!(
            Component::decode(&bytes),
            Err(ComponentError::UnsupportedCanon {
                what: "lifting a lowered function",
                ..
            })
        ));
    }

    #[test]
    fn a_lowered_import_calls_the_embedder_function() {
        let bytes = host_component();
        let component = Component::decode(&bytes).expect("decodes");
        assert_eq!(component.component_functions().len(), 2);
        let mut imports = granted("add").0;
        imports.define_function("add", add_type(), |args| match args {
            [ComponentValue::S32(a), ComponentValue::S32(b)] => {
                Ok(vec![ComponentValue::S32(a + b)])
            }
            _ => Err("add takes two s32 values".to_owned()),
        });
        let mut instance = component
            .instantiate_with(&engine(), &imports)
            .expect("instantiates");
        // The module computes add(2, 3) through the lowered import.
        assert_eq!(
            instance.call_export("run", Vec::new()),
            Ok(vec![ComponentValue::S32(5)])
        );
    }

    /// Imports that draw on a capability set in which the import `name` is granted the `call`
    /// operation. The set is returned too, so a test can revoke the grant.
    fn granted(name: &str) -> (Imports, Arc<Mutex<CapabilitySet>>) {
        let mut set = CapabilitySet::default();
        set.register(tpt_wasm_capability::CapabilityDescriptor {
            id: CapabilityId::new(name),
            operations: vec![Operation::execute(CALL_OPERATION)],
        })
        .expect("registers");
        set.grant(tpt_wasm_capability::CapabilityGrant {
            capability: CapabilityId::new(name),
            permissions: tpt_wasm_capability::Permissions {
                execute: true,
                ..tpt_wasm_capability::Permissions::default()
            },
        })
        .expect("grants");
        let set = Arc::new(Mutex::new(set));
        (Imports::with_capabilities(set.clone()), set)
    }

    #[test]
    fn an_import_without_a_grant_is_refused_before_anything_runs() {
        let component = Component::decode(&host_component()).expect("decodes");
        let (mut imports, set) = granted("add");
        set.lock().expect("lock").revoke(&CapabilityId::new("add"));
        imports.define_function("add", add_type(), |_| Ok(vec![ComponentValue::S32(0)]));
        let error = component
            .instantiate_with(&engine(), &imports)
            .expect_err("the import is not granted");
        assert!(matches!(
            error,
            ComponentError::CapabilityDenied { ref name, .. } if name == "add"
        ));
    }

    #[test]
    fn an_import_with_no_capability_set_is_refused() {
        let component = Component::decode(&host_component()).expect("decodes");
        let mut imports = Imports::new();
        imports.define_function("add", add_type(), |_| Ok(vec![ComponentValue::S32(0)]));
        let error = component
            .instantiate_with(&engine(), &imports)
            .expect_err("nothing is granted without a set");
        assert!(matches!(error, ComponentError::CapabilityDenied { .. }));
    }

    #[test]
    fn a_revoked_import_stops_the_next_call() {
        let component = Component::decode(&host_component()).expect("decodes");
        let (mut imports, set) = granted("add");
        imports.define_function("add", add_type(), |args| match args {
            [ComponentValue::S32(a), ComponentValue::S32(b)] => {
                Ok(vec![ComponentValue::S32(a + b)])
            }
            _ => Err("add takes two s32 values".to_owned()),
        });
        let mut instance = component
            .instantiate_with(&engine(), &imports)
            .expect("instantiates");
        assert_eq!(
            instance.call_export("run", Vec::new()),
            Ok(vec![ComponentValue::S32(5)])
        );
        set.lock().expect("lock").revoke(&CapabilityId::new("add"));
        assert!(matches!(
            instance.call_export("run", Vec::new()),
            Err(ComponentError::Call { .. })
        ));
    }

    /// The type of the host component's import `add`: (s32, s32) -> s32.
    fn add_type() -> ComponentFuncType {
        ComponentFuncType {
            params: vec![Scalar::S32, Scalar::S32],
            result: Some(Scalar::S32),
        }
    }

    #[test]
    fn a_supplied_import_of_another_type_is_refused_before_anything_runs() {
        let component = Component::decode(&host_component()).expect("decodes");
        let mut imports = Imports::new();
        // The component declares (s32, s32) -> s32; this supplies (s64) -> s64.
        imports.define_function(
            "add",
            ComponentFuncType {
                params: vec![Scalar::S64],
                result: Some(Scalar::S64),
            },
            |_| Ok(vec![ComponentValue::S64(0)]),
        );
        let error = component
            .instantiate_with(&engine(), &imports)
            .expect_err("the type differs");
        assert!(matches!(
            error,
            ComponentError::ImportMismatch { ref name, .. } if name == "add"
        ));
    }

    #[test]
    fn a_missing_import_is_refused_before_anything_runs() {
        let component = Component::decode(&host_component()).expect("decodes");
        let error = component
            .instantiate(&engine())
            .expect_err("the import is not supplied");
        assert_eq!(
            error,
            ComponentError::MissingImport {
                name: "add".to_owned()
            }
        );
    }

    #[test]
    fn an_embedder_function_error_stops_the_call() {
        let component = Component::decode(&host_component()).expect("decodes");
        let mut imports = granted("add").0;
        imports.define_function("add", add_type(), |_| Err("refused".to_owned()));
        let mut instance = component
            .instantiate_with(&engine(), &imports)
            .expect("instantiates");
        assert!(matches!(
            instance.call_export("run", Vec::new()),
            Err(ComponentError::Call { .. })
        ));
    }

    #[test]
    fn a_string_crosses_into_and_out_of_linear_memory() {
        let bytes = string_component(&complete_len_canon());
        let component = Component::decode(&bytes).expect("decodes");
        let mut instance = component.instantiate(&engine()).expect("instantiates");
        // The argument is copied in through realloc and the module returns its length.
        assert_eq!(
            instance.call_export("len", vec![ComponentValue::String("hello".to_owned())]),
            Ok(vec![ComponentValue::S32(5)])
        );
        assert_eq!(
            instance.call_export("len", vec![ComponentValue::String(String::new())]),
            Ok(vec![ComponentValue::S32(0)])
        );
        // The result is read out of memory, from the pointer and length the module returns.
        assert_eq!(
            instance.call_export("greet", Vec::new()),
            Ok(vec![ComponentValue::String("hello".to_owned())])
        );
    }

    #[test]
    fn a_string_parameter_without_realloc_is_refused() {
        // Memory only: a string parameter has nowhere to be copied to.
        let memory_only = [CANON_LIFT, SORT_FUNC, 1, 1, OPTION_MEMORY, 0, 0];
        assert!(matches!(
            Component::decode(&string_component(&memory_only)),
            Err(ComponentError::UnsupportedCanon {
                what: "a string parameter without a realloc option",
                ..
            })
        ));
    }

    #[test]
    fn a_string_without_memory_is_refused() {
        // No options at all: the string result has no memory to be read from.
        let no_options = [CANON_LIFT, SORT_FUNC, 1, 0, 0];
        assert!(matches!(
            Component::decode(&string_component(&no_options)),
            Err(ComponentError::UnsupportedCanon {
                what: "a string without a memory option",
                ..
            })
        ));
    }

    #[test]
    fn a_string_crosses_on_the_compiled_backend_too() {
        // The baseline backend keeps its own memory, so the host's writes and reads must go
        // through it as well.
        let config = tpt_wasm_runtime::Config {
            engine_mode: tpt_wasm_runtime::EngineMode::Baseline,
            ..tpt_wasm_runtime::Config::default()
        };
        let engine = Engine::new(config).expect("engine");
        let bytes = string_component(&complete_len_canon());
        let component = Component::decode(&bytes).expect("decodes");
        let mut instance = component.instantiate(&engine).expect("instantiates");
        assert_eq!(
            instance.call_export("len", vec![ComponentValue::String("hello".to_owned())]),
            Ok(vec![ComponentValue::S32(5)])
        );
        assert_eq!(
            instance.call_export("greet", Vec::new()),
            Ok(vec![ComponentValue::String("hello".to_owned())])
        );
    }

    #[test]
    fn a_string_argument_is_checked_against_its_type() {
        let bytes = string_component(&complete_len_canon());
        let mut instance = Component::decode(&bytes)
            .expect("decodes")
            .instantiate(&engine())
            .expect("instantiates");
        assert!(matches!(
            instance.call_export("len", vec![ComponentValue::S32(5)]),
            Err(ComponentError::ArgumentMismatch { .. })
        ));
    }

    /// A component whose `answer` returns `valtype`, lifted from the answer module's 42.
    fn answer_returning(valtype: u8) -> Vec<u8> {
        answer_component(
            &[SECTION_TYPE, 5, 1, TYPE_FUNC, 0, 0, valtype],
            &CANON_LIFT_ANSWER,
            &EXPORT_ANSWER,
        )
    }

    #[test]
    fn the_small_and_boolean_types_lift_from_their_core_value() {
        assert_eq!(
            lift(Value::I32(2), Scalar::Bool),
            Ok(ComponentValue::Bool(true))
        );
        assert_eq!(
            lift(Value::I32(0), Scalar::Bool),
            Ok(ComponentValue::Bool(false))
        );
        // The unsigned types keep the low bits; the signed ones sign-extend them.
        assert_eq!(
            lift(Value::I32(0x1ff), Scalar::U8),
            Ok(ComponentValue::U8(0xff))
        );
        assert_eq!(
            lift(Value::I32(0xff), Scalar::S8),
            Ok(ComponentValue::S8(-1))
        );
        assert_eq!(
            lift(Value::I32(0x8000), Scalar::S16),
            Ok(ComponentValue::S16(i16::MIN))
        );
        assert_eq!(
            lift(Value::I32(0x1_ffff), Scalar::U16),
            Ok(ComponentValue::U16(0xffff))
        );
        assert_eq!(
            lift(Value::I32(42), Scalar::Char),
            Ok(ComponentValue::Char('*'))
        );
    }

    #[test]
    fn a_character_that_is_not_a_scalar_value_traps() {
        assert!(lift(Value::I32(0xD800), Scalar::Char).is_err());
        assert!(lift(Value::I32(0xDFFF), Scalar::Char).is_err());
        assert!(lift(Value::I32(0x11_0000), Scalar::Char).is_err());
        assert!(lift(Value::I32(-1), Scalar::Char).is_err());
    }

    #[test]
    fn the_small_and_boolean_types_lower_to_one_i32() {
        assert_eq!(
            lower_scalar(&ComponentValue::Bool(true)),
            Some(Value::I32(1))
        );
        assert_eq!(lower_scalar(&ComponentValue::S8(-1)), Some(Value::I32(-1)));
        assert_eq!(
            lower_scalar(&ComponentValue::U8(255)),
            Some(Value::I32(255))
        );
        assert_eq!(lower_scalar(&ComponentValue::S16(-2)), Some(Value::I32(-2)));
        assert_eq!(
            lower_scalar(&ComponentValue::U16(65535)),
            Some(Value::I32(65535))
        );
        assert_eq!(
            lower_scalar(&ComponentValue::Char('A')),
            Some(Value::I32(65))
        );
    }

    #[test]
    fn exports_return_the_small_and_boolean_types() {
        let cases = [
            (VALTYPE_BOOL, ComponentValue::Bool(true)),
            (VALTYPE_U8, ComponentValue::U8(42)),
            (VALTYPE_S8, ComponentValue::S8(42)),
            (VALTYPE_U16, ComponentValue::U16(42)),
            (VALTYPE_S16, ComponentValue::S16(42)),
            (VALTYPE_CHAR, ComponentValue::Char('*')),
        ];
        for (valtype, expected) in cases {
            let mut instance = Component::decode(&answer_returning(valtype))
                .expect("decodes")
                .instantiate(&engine())
                .expect("instantiates");
            assert_eq!(
                instance.call_export("answer", Vec::new()),
                Ok(vec![expected]),
                "valtype {valtype:#x}"
            );
        }
    }

    /// Every truncation and every single-byte corruption of the fixtures is decoded, and every
    /// one that decodes is instantiated. None may panic: the bytes are untrusted, and a panic
    /// here is a denial of service in the embedder.
    #[test]
    fn no_truncation_or_corruption_of_a_component_panics() {
        let engine = engine();
        let fixtures = [
            host_component(),
            answer_component(&TYPE_NO_ARGUMENTS_S32, &CANON_LIFT_ANSWER, &EXPORT_ANSWER),
            string_component(&complete_len_canon()),
        ];
        let mut decoded = 0usize;
        let mut instantiated = 0usize;
        for fixture in &fixtures {
            for length in 0..fixture.len() {
                let _ = Component::decode(&fixture[..length]);
            }
            for position in 0..fixture.len() {
                for mask in [0x01u8, 0x40, 0x80, 0xff] {
                    let mut bytes = fixture.clone();
                    bytes[position] ^= mask;
                    let Ok(component) = Component::decode(&bytes) else {
                        continue;
                    };
                    decoded += 1;
                    let mut imports = granted("add").0;
                    imports.define_function("add", add_type(), |args| match args {
                        [ComponentValue::S32(a), ComponentValue::S32(b)] => {
                            Ok(vec![ComponentValue::S32(a.wrapping_add(*b))])
                        }
                        _ => Err("add takes two s32 values".to_owned()),
                    });
                    if let Ok(mut instance) = component.instantiate_with(&engine, &imports) {
                        instantiated += 1;
                        let names: Vec<String> = instance
                            .export_names()
                            .into_iter()
                            .map(str::to_owned)
                            .collect();
                        for name in names {
                            let _ = instance.call_export(&name, Vec::new());
                            let _ = instance
                                .call_export(&name, vec![ComponentValue::String("hi".to_owned())]);
                        }
                    }
                }
            }
        }
        assert!(decoded > 0, "some corruptions must still decode");
        assert!(instantiated > 0, "some corruptions must still instantiate");
    }

    #[test]
    fn scalars_lower_and_lift_by_their_bits() {
        assert_eq!(
            lower_scalar(&ComponentValue::U32(u32::MAX)),
            Some(Value::I32(-1))
        );
        assert_eq!(
            lift(Value::I32(-1), Scalar::U32),
            Ok(ComponentValue::U32(u32::MAX))
        );
        assert_eq!(
            lift(Value::I32(-1), Scalar::S32),
            Ok(ComponentValue::S32(-1))
        );
        // A NaN with a payload keeps its bits through the round trip.
        let nan = f32::from_bits(0x7fc0_1234);
        let lowered = lower_scalar(&ComponentValue::F32(nan)).expect("a scalar lowers");
        assert_eq!(lowered, Value::F32(0x7fc0_1234));
        assert_eq!(
            lift(lowered, Scalar::F32).map(|value| match value {
                ComponentValue::F32(value) => value.to_bits(),
                _ => 0,
            }),
            Ok(0x7fc0_1234)
        );
        assert!(lift(Value::I64(1), Scalar::S32).is_err());
        assert_eq!(lower_scalar(&ComponentValue::String("x".to_owned())), None);
    }
}
