// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

#![forbid(unsafe_code)]
//! The Component Model envelope.
//!
//! A component is a binary with the Core Wasm magic, the version `0x0d` and the layer
//! `0x01`, so it cannot be mistaken for a core module, whose layer is zero. After the
//! preamble comes a sequence of sections: each is an id byte, a `u32` size, and the
//! contents. This crate reads that framing and collects the core modules a component
//! nests. Every other section is refused by id, so nothing is silently dropped.

use std::fmt;

/// The component preamble: the magic, then version `0x0d` and layer `0x01`.
pub const PREAMBLE: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];

/// The section id of a custom section, which is skipped.
const SECTION_CUSTOM: u8 = 0;
/// The section id of a nested core module.
const SECTION_CORE_MODULE: u8 = 1;

/// Why a component was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentError {
    /// The bytes are not a component: the magic, version or layer is wrong. A core module
    /// is refused this way, because its layer is zero.
    NotAComponent,
    /// The bytes end inside a section's framing or contents.
    Truncated { offset: usize },
    /// A section size is not a `u32` in canonical `LEB128` form.
    InvalidSize { offset: usize },
    /// A section of a kind this crate does not implement yet.
    UnsupportedSection { id: u8, offset: usize },
}

impl fmt::Display for ComponentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ComponentError::NotAComponent => write!(f, "not a component"),
            ComponentError::Truncated { offset } => {
                write!(f, "the component ends inside a section at byte {offset}")
            }
            ComponentError::InvalidSize { offset } => {
                write!(f, "invalid section size at byte {offset}")
            }
            ComponentError::UnsupportedSection { id, offset } => {
                write!(f, "section id {id} at byte {offset} is not supported yet")
            }
        }
    }
}

impl std::error::Error for ComponentError {}

/// A decoded component envelope: the core modules it nests, in the order they appear.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Component {
    core_modules: Vec<Vec<u8>>,
}

impl Component {
    /// Decode a component envelope.
    pub fn decode(bytes: &[u8]) -> Result<Self, ComponentError> {
        if bytes.len() < PREAMBLE.len() || bytes[..PREAMBLE.len()] != PREAMBLE {
            return Err(ComponentError::NotAComponent);
        }
        let mut component = Component::default();
        let mut position = PREAMBLE.len();
        while position < bytes.len() {
            let id = bytes[position];
            let (size, contents) = read_size(bytes, position + 1)?;
            let end = contents
                .checked_add(size as usize)
                .filter(|end| *end <= bytes.len())
                .ok_or(ComponentError::Truncated { offset: contents })?;
            match id {
                SECTION_CUSTOM => {}
                SECTION_CORE_MODULE => component.core_modules.push(bytes[contents..end].to_vec()),
                other => {
                    return Err(ComponentError::UnsupportedSection {
                        id: other,
                        offset: position,
                    })
                }
            }
            position = end;
        }
        Ok(component)
    }

    /// The core modules the component nests, as their raw bytes.
    pub fn core_modules(&self) -> &[Vec<u8>] {
        &self.core_modules
    }
}

/// Read a section size starting at `position`, returning it and the offset after it.
fn read_size(bytes: &[u8], position: usize) -> Result<(u32, usize), ComponentError> {
    let mut value: u64 = 0;
    for index in 0..5 {
        let at = position + index;
        let Some(byte) = bytes.get(at) else {
            return Err(ComponentError::Truncated { offset: at });
        };
        value |= u64::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return u32::try_from(value)
                .map(|size| (size, at + 1))
                .map_err(|_| ComponentError::InvalidSize { offset: position });
        }
    }
    Err(ComponentError::InvalidSize { offset: position })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A core module with the magic and version 1 and no sections.
    const EMPTY_MODULE: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    fn with(sections: &[u8]) -> Vec<u8> {
        let mut bytes = PREAMBLE.to_vec();
        bytes.extend_from_slice(sections);
        bytes
    }

    #[test]
    fn a_preamble_alone_is_an_empty_component() {
        let component = Component::decode(&PREAMBLE).expect("decodes");
        assert!(component.core_modules().is_empty());
    }

    #[test]
    fn a_core_module_section_is_collected_with_its_bytes() {
        let mut section = vec![SECTION_CORE_MODULE, EMPTY_MODULE.len() as u8];
        section.extend_from_slice(&EMPTY_MODULE);
        let component = Component::decode(&with(&section)).expect("decodes");
        assert_eq!(component.core_modules(), [EMPTY_MODULE.to_vec()]);
    }

    #[test]
    fn custom_sections_are_skipped_and_modules_keep_their_order() {
        let mut bytes = with(&[SECTION_CUSTOM, 3, 0x61, 0x62, 0x63]);
        bytes.push(SECTION_CORE_MODULE);
        bytes.push(EMPTY_MODULE.len() as u8);
        bytes.extend_from_slice(&EMPTY_MODULE);
        bytes.push(SECTION_CORE_MODULE);
        bytes.push(EMPTY_MODULE.len() as u8);
        bytes.extend_from_slice(&EMPTY_MODULE);
        let component = Component::decode(&bytes).expect("decodes");
        assert_eq!(component.core_modules().len(), 2);
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
    fn a_size_past_u32_is_invalid() {
        assert_eq!(
            Component::decode(&with(&[SECTION_CORE_MODULE, 0x80, 0x80, 0x80, 0x80, 0x10])),
            Err(ComponentError::InvalidSize { offset: 9 })
        );
    }
}
