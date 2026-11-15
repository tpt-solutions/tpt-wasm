// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Source locations for debug sessions, from a module's DWARF line tables.
//!
//! A module that carries a `.debug_line` custom section (and optionally `.debug_line_str`) names
//! the source line of each instruction. The address a line row names is the byte offset of the
//! instruction in the module, counted from the first byte of the file, which is how WebAssembly
//! DWARF is written. A frame's program counter is an instruction index, so its location is found by
//! decoding the function's body with each instruction's byte offset, adding the body's place in the
//! module, and looking that address up.
//!
//! A module with no line information has no source map at all. A module whose line information is
//! present but cannot be read is reported as an error at the point a location is asked for, rather
//! than being refused at instantiation or quietly given no locations.

use std::sync::Arc;

use tpt_wasm_decode::{decode, function_body_offsets};
use tpt_wasm_format::debug_line::{self, LineTable};
use tpt_wasm_format::ImportDesc;
use tpt_wasm_micro::instr::decode_body_with_offsets;

/// Where a frame is in the source: the file, and the line and column of the instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocation {
    /// The file as the line table names it, joined to its directory where it has one.
    pub file: String,
    /// The line, counted from 1.
    pub line: u64,
    /// The column, counted from 1, or 0 for the left edge.
    pub column: u64,
}

/// The line tables of one module, and the body of each function it defines.
#[derive(Debug)]
pub(crate) struct SourceMap {
    tables: Vec<LineTable>,
    /// Indexed by function index, imports first. An imported function has no body here.
    bodies: Vec<Option<Body>>,
}

#[derive(Debug)]
struct Body {
    /// The module byte offset of the body's first instruction.
    start: usize,
    /// The body's bytes, from its first instruction to its final `end`.
    bytes: Vec<u8>,
}

/// Load the source map of a module from its bytes. `Ok(None)` means the module has no line
/// information. The name check is a byte search, so a module without the section is not decoded
/// a second time on every instantiation.
pub(crate) fn load(bytes: &[u8]) -> Result<Option<Arc<SourceMap>>, String> {
    if !contains(bytes, b".debug_line") {
        return Ok(None);
    }
    let module = decode(bytes).map_err(|error| error.to_string())?;
    let line_sections: Vec<&[u8]> = module
        .custom_sections
        .iter()
        .filter(|section| section.name == ".debug_line")
        .map(|section| section.data.as_slice())
        .collect();
    if line_sections.is_empty() {
        return Ok(None);
    }
    let line_str: &[u8] = module
        .custom_sections
        .iter()
        .find(|section| section.name == ".debug_line_str")
        .map_or(&[], |section| section.data.as_slice());
    let mut tables = Vec::new();
    for section in line_sections {
        tables.extend(debug_line::parse(section, line_str).map_err(|error| error.to_string())?);
    }

    let imported = module
        .imports
        .iter()
        .filter(|import| matches!(import.desc, ImportDesc::Function(_)))
        .count();
    let starts = function_body_offsets(bytes).map_err(|error| error.to_string())?;
    if starts.len() != module.functions.len() {
        return Err("the code section and the function section disagree".to_owned());
    }
    let mut bodies: Vec<Option<Body>> = (0..imported).map(|_| None).collect();
    for (function, start) in module.functions.iter().zip(starts) {
        bodies.push(Some(Body {
            start,
            bytes: function.body.clone(),
        }));
    }
    Ok(Some(Arc::new(SourceMap { tables, bodies })))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

impl SourceMap {
    /// The source location of the instruction at `pc` in function `function`, if the line tables
    /// cover it. `Err` means the function's body could not be decoded, which the module's own
    /// validation rules out for a module that was instantiated.
    pub(crate) fn location(
        &self,
        function: u32,
        pc: usize,
    ) -> Result<Option<SourceLocation>, String> {
        let Some(Some(body)) = self.bodies.get(function as usize) else {
            return Ok(None);
        };
        let (_, offsets) = decode_body_with_offsets(&body.bytes).map_err(|e| e.to_string())?;
        let Some(&within) = offsets.get(pc) else {
            return Ok(None);
        };
        let address = (body.start + within) as u64;
        for table in &self.tables {
            if let Some(row) = table.lookup(address) {
                return Ok(Some(SourceLocation {
                    file: table.file_name(row.file).unwrap_or_default().to_owned(),
                    line: row.line,
                    column: row.column,
                }));
            }
        }
        Ok(None)
    }
}
