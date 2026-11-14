// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

#![forbid(unsafe_code)]
//! The Component Model envelope and its core instances.
//!
//! A component is a binary with the Core Wasm magic, the version `0x0d` and the layer
//! `0x01`, so it cannot be mistaken for a core module, whose layer is zero. After the
//! preamble comes a sequence of sections: each is an id byte, a `u32` size, and the
//! contents. This crate reads that framing, collects the core modules a component nests,
//! and decodes the core-instance section (id 2). Every other section, and every instance
//! form it does not implement, is refused rather than skipped.

use std::fmt;

use tpt_wasm_runtime::{Engine, Instance, Linker};
use tpt_wasm_types::Value;

/// The component preamble: the magic, then version `0x0d` and layer `0x01`.
pub const PREAMBLE: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];

/// The section id of a custom section, which is skipped.
const SECTION_CUSTOM: u8 = 0;
/// The section id of a nested core module.
const SECTION_CORE_MODULE: u8 = 1;
/// The section id of core instances.
const SECTION_CORE_INSTANCE: u8 = 2;

/// The core-instance expression tag that instantiates a module.
const INSTANTIATE: u8 = 0x00;
/// The core-instance expression tag that builds an instance from inline exports.
const INLINE_EXPORTS: u8 = 0x01;
/// The sort byte of a core instance, which an instance argument names.
const SORT_CORE_INSTANCE: u8 = 0x12;

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
    /// A tag or index that the grammar does not allow here.
    Malformed { offset: usize },
    /// A core instance names a module the component does not nest.
    NoSuchModule { module: u32 },
    /// A core instance could not be instantiated, with the runtime's reason.
    Instantiate { instance: usize, message: String },
    /// No core instance has this index.
    NoSuchInstance { instance: usize },
    /// A call into a core instance failed, with the runtime's reason.
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
            ComponentError::UnsupportedInstance { what, offset } => {
                write!(f, "{what} at byte {offset} is not supported yet")
            }
            ComponentError::Malformed { offset } => write!(f, "malformed entry at byte {offset}"),
            ComponentError::NoSuchModule { module } => {
                write!(
                    f,
                    "core instance names module {module}, which the component does not nest"
                )
            }
            ComponentError::Instantiate { instance, message } => {
                write!(f, "core instance {instance} did not instantiate: {message}")
            }
            ComponentError::NoSuchInstance { instance } => {
                write!(f, "there is no core instance {instance}")
            }
            ComponentError::Call { instance, message } => {
                write!(f, "a call into core instance {instance} failed: {message}")
            }
        }
    }
}

impl std::error::Error for ComponentError {}

/// One core instance a component declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreInstance {
    /// Instantiate the nested core module with index `module`. Each argument binds one of
    /// the module's import modules to an earlier core instance.
    Instantiate {
        module: u32,
        arguments: Vec<InstanceArgument>,
    },
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

/// A decoded component envelope: the core modules it nests, and the core instances it
/// declares, in the order they appear.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Component {
    core_modules: Vec<Vec<u8>>,
    core_instances: Vec<CoreInstance>,
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
            match id {
                SECTION_CUSTOM => {}
                SECTION_CORE_MODULE => component.core_modules.push(bytes[start..end].to_vec()),
                SECTION_CORE_INSTANCE => {
                    // The instances must fill the section exactly, with no bytes left over.
                    let mut section = Reader::new(bytes, start, end);
                    decode_instances(&mut section, &mut component.core_instances)?;
                    if !section.at_end() {
                        return Err(ComponentError::Malformed {
                            offset: section.position(),
                        });
                    }
                }
                other => {
                    return Err(ComponentError::UnsupportedSection {
                        id: other,
                        offset: size_at - 1,
                    })
                }
            }
            reader = Reader::new(bytes, end, bytes.len());
        }
        for instance in &component.core_instances {
            let CoreInstance::Instantiate { module, .. } = instance;
            if *module as usize >= component.core_modules.len() {
                return Err(ComponentError::NoSuchModule { module: *module });
            }
        }
        Ok(component)
    }

    /// The core modules the component nests, as their raw bytes.
    pub fn core_modules(&self) -> &[Vec<u8>] {
        &self.core_modules
    }

    /// The core instances the component declares, in order.
    pub fn core_instances(&self) -> &[CoreInstance] {
        &self.core_instances
    }

    /// Instantiate each core instance, in order, on `engine`. A component with no core
    /// instances runs nothing.
    pub fn instantiate(&self, engine: &Engine) -> Result<ComponentInstance, ComponentError> {
        let mut instances: Vec<Instance> = Vec::with_capacity(self.core_instances.len());
        for (index, instance) in self.core_instances.iter().enumerate() {
            let CoreInstance::Instantiate { module, arguments } = instance;
            let bytes = &self.core_modules[*module as usize];
            // Each core instance gets a linker of its own, so one instance's imports cannot
            // leak into another.
            let mut linker = Linker::new();
            for argument in arguments {
                let source = &instances[argument.instance as usize];
                linker
                    .define_instance(argument.name.as_str(), source)
                    .map_err(|error| ComponentError::Instantiate {
                        instance: index,
                        message: error.to_string(),
                    })?;
            }
            let runtime = engine
                .instantiate_bytes_with_linker(bytes, &linker)
                .map_err(|error| ComponentError::Instantiate {
                    instance: index,
                    message: error.to_string(),
                })?;
            instances.push(runtime);
        }
        Ok(ComponentInstance { instances })
    }
}

/// The core instances of a running component, in the order the component declares them.
#[derive(Debug)]
pub struct ComponentInstance {
    instances: Vec<Instance>,
}

impl ComponentInstance {
    /// How many core instances the component has.
    pub fn core_instance_count(&self) -> usize {
        self.instances.len()
    }

    /// Call an export of core instance `instance`.
    pub fn call_core(
        &mut self,
        instance: usize,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Vec<Value>, ComponentError> {
        let runtime = self
            .instances
            .get_mut(instance)
            .ok_or(ComponentError::NoSuchInstance { instance })?;
        runtime
            .call(name, args)
            .map_err(|error| ComponentError::Call {
                instance,
                message: error.to_string(),
            })
    }
}

/// Decode a core-instance section's contents: a count, then each instance expression, which
/// are appended to `instances`. An argument may name only an instance declared before it,
/// so every index is checked against the instances already there.
fn decode_instances(
    reader: &mut Reader<'_>,
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
                return Err(ComponentError::UnsupportedInstance {
                    what: "inline exports",
                    offset: at,
                })
            }
            _ => return Err(ComponentError::Malformed { offset: at }),
        }
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

    /// A core name: a `u32` length, then that many bytes of UTF-8.
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

    fn with(sections: &[u8]) -> Vec<u8> {
        let mut bytes = PREAMBLE.to_vec();
        bytes.extend_from_slice(sections);
        bytes
    }

    fn module_section(module: &[u8]) -> Vec<u8> {
        let mut section = vec![SECTION_CORE_MODULE, module.len() as u8];
        section.extend_from_slice(module);
        section
    }

    #[test]
    fn a_preamble_alone_is_an_empty_component() {
        let component = Component::decode(&PREAMBLE).expect("decodes");
        assert!(component.core_modules().is_empty());
        assert!(component.core_instances().is_empty());
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
        // One core instance: instantiate module 0 with no arguments.
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend_from_slice(&[SECTION_CORE_INSTANCE, 4, 1, INSTANTIATE, 0, 0]);
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
    fn inline_exports_are_refused_for_now() {
        let mut inline = with(&module_section(&ANSWER_MODULE));
        inline.extend_from_slice(&[SECTION_CORE_INSTANCE, 2, 1, INLINE_EXPORTS]);
        // 8 bytes of preamble, then the 41-byte module section, then 2 bytes of section
        // header and 1 of count: the tag is at byte 52.
        assert_eq!(
            Component::decode(&inline),
            Err(ComponentError::UnsupportedInstance {
                what: "inline exports",
                offset: 52,
            })
        );
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
        bytes.extend_from_slice(&[SECTION_CORE_INSTANCE, 4, 1, INSTANTIATE, 0, 0]);
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
    fn an_argument_links_an_import_to_an_earlier_instance() {
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend(module_section(&RELAY_MODULE));
        // Instance 0 is the answer module. Instance 1 is the relay module, whose import
        // env.answer is bound to instance 0.
        bytes.extend_from_slice(&[SECTION_CORE_INSTANCE, 4, 1, INSTANTIATE, 0, 0]);
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
            0,
        ]);
        let component = Component::decode(&bytes).expect("decodes");
        assert_eq!(
            component.core_instances()[1],
            CoreInstance::Instantiate {
                module: 1,
                arguments: vec![InstanceArgument {
                    name: "env".to_owned(),
                    instance: 0,
                }],
            }
        );
        let engine = Engine::new(tpt_wasm_runtime::Config::default()).expect("engine");
        let mut instance = component.instantiate(&engine).expect("instantiates");
        assert_eq!(
            instance.call_core(1, "relay", Vec::new()),
            Ok(vec![Value::I32(42)])
        );
    }

    #[test]
    fn an_unresolved_import_fails_to_instantiate() {
        let mut bytes = with(&module_section(&RELAY_MODULE));
        bytes.extend_from_slice(&[SECTION_CORE_INSTANCE, 4, 1, INSTANTIATE, 0, 0]);
        let component = Component::decode(&bytes).expect("decodes");
        let engine = Engine::new(tpt_wasm_runtime::Config::default()).expect("engine");
        let error = component
            .instantiate(&engine)
            .expect_err("the import is missing");
        assert!(matches!(
            error,
            ComponentError::Instantiate { instance: 0, .. }
        ));
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
    fn a_nested_module_instantiates_and_its_export_runs() {
        let mut bytes = with(&module_section(&ANSWER_MODULE));
        bytes.extend_from_slice(&[SECTION_CORE_INSTANCE, 4, 1, INSTANTIATE, 0, 0]);
        let component = Component::decode(&bytes).expect("decodes");
        let engine = Engine::new(tpt_wasm_runtime::Config::default()).expect("engine");
        let mut instance = component.instantiate(&engine).expect("instantiates");
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
        let engine = Engine::new(tpt_wasm_runtime::Config::default()).expect("engine");
        let instance = component.instantiate(&engine).expect("instantiates");
        assert_eq!(instance.core_instance_count(), 0);
    }
}
