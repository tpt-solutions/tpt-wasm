// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! DWARF line tables, read from a module's `.debug_line` custom section.
//!
//! A WebAssembly toolchain records source locations as a DWARF line program, and a WebAssembly
//! address is a byte offset into the module, counted from the start of the file. This module
//! reads the line programs of versions 4 and 5, and answers "which source line produced the code
//! at this offset". It is a reader only: nothing here runs a program, and a malformed program is
//! refused with the byte offset it failed at, never skipped.
//!
//! Supported: 32-bit DWARF units; the header forms `DW_FORM_string`, `DW_FORM_line_strp` (which
//! needs the `.debug_line_str` section), `DW_FORM_udata`, the fixed-size data forms, and
//! `DW_FORM_block`; every standard opcode; the extended opcodes `end_sequence`, `set_address`,
//! `define_file` and `set_discriminator`, with any other extended opcode skipped by its length.
//! Refused: 64-bit DWARF, `DW_FORM_strp` (which needs `.debug_str`), and versions other than 4 and 5.

use std::fmt;

/// Why a line table could not be read, with the byte offset in `.debug_line` where it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineError {
    /// The section ended inside a field.
    Truncated { offset: usize },
    /// A unit declared a DWARF version this reader does not support.
    UnsupportedVersion { offset: usize, version: u16 },
    /// A unit uses a form, or the 64-bit format, that this reader refuses.
    Unsupported { offset: usize, what: &'static str },
    /// A field has a value the format does not allow.
    Malformed { offset: usize, what: &'static str },
}

impl fmt::Display for LineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { offset } => write!(f, "debug line: truncated at byte {offset}"),
            Self::UnsupportedVersion { offset, version } => write!(
                f,
                "debug line: unit at byte {offset} has unsupported version {version}"
            ),
            Self::Unsupported { offset, what } => {
                write!(f, "debug line: {what} at byte {offset} is not supported")
            }
            Self::Malformed { offset, what } => {
                write!(f, "debug line: {what} at byte {offset} is malformed")
            }
        }
    }
}

impl std::error::Error for LineError {}

/// One row of the matrix: the location of the code at `address`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRow {
    /// The byte offset in the module the row starts at.
    pub address: u64,
    /// The file register: an index into the unit's file names (see [`LineTable::file_name`]).
    pub file: u64,
    /// The source line, counted from 1; `0` means no line.
    pub line: u64,
    /// The source column, counted from 1; `0` means the left edge.
    pub column: u64,
    /// Whether the row is a recommended breakpoint location.
    pub is_stmt: bool,
    /// Whether this row ends a sequence (its address is one past the last byte of the sequence).
    pub end_sequence: bool,
}

/// The line rows of one contiguous run of addresses, from `set_address` to `end_sequence`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sequence {
    rows: Vec<LineRow>,
}

/// The line table of one unit: its file names and its sequences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineTable {
    version: u16,
    /// The file names as the header lists them, each joined to its directory when it has one.
    files: Vec<String>,
    sequences: Vec<Sequence>,
}

impl LineTable {
    /// The DWARF version the unit was written in (4 or 5).
    pub fn version(&self) -> u16 {
        self.version
    }

    /// The name of the file a file register names. Version 4 numbers files from 1, version 5
    /// from 0, so the same register means different entries in the two.
    pub fn file_name(&self, register: u64) -> Option<&str> {
        let index = if self.version >= 5 {
            register
        } else {
            register.checked_sub(1)?
        };
        self.files
            .get(usize::try_from(index).ok()?)
            .map(String::as_str)
    }

    /// Every row of the table, in the order the program wrote them.
    pub fn rows(&self) -> impl Iterator<Item = &LineRow> {
        self.sequences.iter().flat_map(|s| s.rows.iter())
    }

    /// The source location of the code at `address`: the last row whose address is at or before
    /// it, within the sequence that covers it. A row written later at the same address wins, as
    /// the standard requires. An address past a sequence's end, or outside every sequence, has
    /// no location.
    pub fn lookup(&self, address: u64) -> Option<&LineRow> {
        for sequence in &self.sequences {
            let Some(end) = sequence.rows.last().filter(|r| r.end_sequence) else {
                continue;
            };
            let Some(first) = sequence.rows.first() else {
                continue;
            };
            if address < first.address || address >= end.address {
                continue;
            }
            let row = sequence.rows.iter().rev().find(|r| r.address <= address)?;
            return if row.end_sequence { None } else { Some(row) };
        }
        None
    }
}

/// Read every line table in a `.debug_line` section. `debug_line_str` is the `.debug_line_str`
/// section, which header strings may refer to; pass an empty slice if the module has none.
pub fn parse(debug_line: &[u8], debug_line_str: &[u8]) -> Result<Vec<LineTable>, LineError> {
    let mut tables = Vec::new();
    let mut offset = 0usize;
    while offset < debug_line.len() {
        let (table, next) = unit(debug_line, debug_line_str, offset)?;
        tables.push(table);
        offset = next;
    }
    Ok(tables)
}

/// A cursor over one section with the offsets every error names.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], at: usize) -> Self {
        Self { bytes, at }
    }

    fn end(&self) -> usize {
        self.bytes.len()
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], LineError> {
        let end = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(LineError::Truncated { offset: self.at })?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, LineError> {
        Ok(self.take(1)?[0])
    }

    fn i8(&mut self) -> Result<i8, LineError> {
        Ok(self.u8()? as i8)
    }

    fn u16(&mut self) -> Result<u16, LineError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, LineError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn uleb(&mut self) -> Result<u64, LineError> {
        let start = self.at;
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = self.u8()?;
            if shift >= 64 {
                if byte & 0x7f != 0 {
                    return Err(LineError::Malformed {
                        offset: start,
                        what: "a LEB128 value wider than 64 bits",
                    });
                }
            } else {
                result |= u64::from(byte & 0x7f) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
        }
    }

    fn sleb(&mut self) -> Result<i64, LineError> {
        let start = self.at;
        let mut result = 0i64;
        let mut shift = 0u32;
        loop {
            let byte = self.u8()?;
            if shift >= 64 {
                if byte != 0x00 && byte != 0x7f {
                    return Err(LineError::Malformed {
                        offset: start,
                        what: "a LEB128 value wider than 64 bits",
                    });
                }
            } else {
                result |= i64::from(byte & 0x7f) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    result |= -1i64 << shift;
                }
                return Ok(result);
            }
        }
    }

    /// A NUL-terminated string.
    fn cstr(&mut self) -> Result<String, LineError> {
        let start = self.at;
        let rest = &self.bytes[self.at..];
        let len = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or(LineError::Truncated { offset: start })?;
        let text = std::str::from_utf8(&rest[..len]).map_err(|_| LineError::Malformed {
            offset: start,
            what: "a file name that is not UTF-8",
        })?;
        self.at += len + 1;
        Ok(text.to_owned())
    }
}

/// The attribute a header entry format describes.
const LNCT_PATH: u64 = 0x1;
const LNCT_DIRECTORY_INDEX: u64 = 0x2;

const FORM_BLOCK: u64 = 0x09;
const FORM_DATA1: u64 = 0x0b;
const FORM_DATA2: u64 = 0x05;
const FORM_DATA4: u64 = 0x06;
const FORM_DATA8: u64 = 0x07;
const FORM_DATA16: u64 = 0x1e;
const FORM_STRING: u64 = 0x08;
const FORM_UDATA: u64 = 0x0f;
const FORM_LINE_STRP: u64 = 0x1f;
const FORM_STRP: u64 = 0x0e;

/// One value read from a version 5 header entry.
enum EntryValue {
    Text(String),
    Number(u64),
    Skipped,
}

/// Read a value of `form` at the cursor.
fn form_value(
    reader: &mut Reader<'_>,
    form: u64,
    debug_line_str: &[u8],
) -> Result<EntryValue, LineError> {
    let at = reader.at;
    Ok(match form {
        FORM_STRING => EntryValue::Text(reader.cstr()?),
        FORM_LINE_STRP => {
            let offset = reader.u32()? as usize;
            let rest = debug_line_str.get(offset..).ok_or(LineError::Malformed {
                offset: at,
                what: "a .debug_line_str offset past the section",
            })?;
            let len = rest
                .iter()
                .position(|b| *b == 0)
                .ok_or(LineError::Malformed {
                    offset: at,
                    what: "a .debug_line_str string without its terminator",
                })?;
            EntryValue::Text(
                std::str::from_utf8(&rest[..len])
                    .map_err(|_| LineError::Malformed {
                        offset: at,
                        what: "a file name that is not UTF-8",
                    })?
                    .to_owned(),
            )
        }
        FORM_STRP => {
            return Err(LineError::Unsupported {
                offset: at,
                what: "DW_FORM_strp (.debug_str)",
            })
        }
        FORM_UDATA => EntryValue::Number(reader.uleb()?),
        FORM_DATA1 => EntryValue::Number(u64::from(reader.u8()?)),
        FORM_DATA2 => EntryValue::Number(u64::from(reader.u16()?)),
        FORM_DATA4 => EntryValue::Number(u64::from(reader.u32()?)),
        FORM_DATA8 => {
            let bytes = reader.take(8)?;
            let mut array = [0u8; 8];
            array.copy_from_slice(bytes);
            EntryValue::Number(u64::from_le_bytes(array))
        }
        FORM_DATA16 => {
            reader.take(16)?;
            EntryValue::Skipped
        }
        FORM_BLOCK => {
            let len = reader.uleb()? as usize;
            reader.take(len)?;
            EntryValue::Skipped
        }
        _ => {
            return Err(LineError::Unsupported {
                offset: at,
                what: "a header form",
            })
        }
    })
}

/// Read one unit starting at `start`, and return its table and the offset of the next unit.
fn unit(
    section: &[u8],
    debug_line_str: &[u8],
    start: usize,
) -> Result<(LineTable, usize), LineError> {
    let mut reader = Reader::new(section, start);
    let length = reader.u32()?;
    if length == 0xffff_ffff {
        return Err(LineError::Unsupported {
            offset: start,
            what: "64-bit DWARF",
        });
    }
    let unit_end = reader
        .at
        .checked_add(length as usize)
        .filter(|end| *end <= section.len())
        .ok_or(LineError::Truncated { offset: start })?;
    let version = reader.u16()?;
    if version != 4 && version != 5 {
        return Err(LineError::UnsupportedVersion {
            offset: start,
            version,
        });
    }
    if version >= 5 {
        let _address_size = reader.u8()?;
        let _segment_selector_size = reader.u8()?;
    }
    let header_length = reader.u32()? as usize;
    let program_start = reader
        .at
        .checked_add(header_length)
        .filter(|at| *at <= unit_end)
        .ok_or(LineError::Malformed {
            offset: start,
            what: "a header length past the unit",
        })?;
    let min_inst_length = u64::from(reader.u8()?);
    if version >= 4 {
        let _max_ops_per_instruction = reader.u8()?;
    }
    let default_is_stmt = reader.u8()? != 0;
    let line_base = i64::from(reader.i8()?);
    let line_range = reader.u8()?;
    if line_range == 0 {
        return Err(LineError::Malformed {
            offset: reader.at - 1,
            what: "a line range of zero",
        });
    }
    let opcode_base = reader.u8()?;
    if opcode_base == 0 {
        return Err(LineError::Malformed {
            offset: reader.at - 1,
            what: "an opcode base of zero",
        });
    }
    let mut standard_lengths = Vec::with_capacity(opcode_base as usize);
    standard_lengths.push(0u8);
    for _ in 1..opcode_base {
        standard_lengths.push(reader.u8()?);
    }

    let files = if version >= 5 {
        let directories = read_v5_entries(&mut reader, debug_line_str)?;
        let names = read_v5_entries(&mut reader, debug_line_str)?;
        join_files(&names, &directories)
    } else {
        let mut directories = Vec::new();
        loop {
            let name = reader.cstr()?;
            if name.is_empty() {
                break;
            }
            directories.push(name);
        }
        let mut files = Vec::new();
        loop {
            let name = reader.cstr()?;
            if name.is_empty() {
                break;
            }
            let directory = reader.uleb()?;
            let _mtime = reader.uleb()?;
            let _length = reader.uleb()?;
            // Version 4 numbers directories from 1 for the first listed one; 0 is the compilation
            // directory, which the header does not list, so such a name stays as it is.
            let listed = directory.checked_sub(1).unwrap_or(u64::MAX);
            files.push(join_name(&name, listed, &directories));
        }
        files
    };
    if reader.at > program_start {
        return Err(LineError::Malformed {
            offset: reader.at,
            what: "a header that runs past its declared length",
        });
    }

    let program = Program {
        min_inst_length,
        default_is_stmt,
        line_base,
        line_range: u64::from(line_range),
        opcode_base,
        standard_lengths,
    };
    let mut program_reader = Reader {
        bytes: &section[..unit_end],
        at: program_start,
    };
    let sequences = program.run(&mut program_reader)?;
    Ok((
        LineTable {
            version,
            files,
            sequences,
        },
        unit_end,
    ))
}

/// Read a version 5 entry table: its format, then its entries. Each entry's path becomes a
/// string, and its directory index a number; other attributes are read past.
fn read_v5_entries(
    reader: &mut Reader<'_>,
    debug_line_str: &[u8],
) -> Result<Vec<(String, u64)>, LineError> {
    let format_count = reader.u8()?;
    let mut format = Vec::with_capacity(format_count as usize);
    for _ in 0..format_count {
        let content = reader.uleb()?;
        let form = reader.uleb()?;
        format.push((content, form));
    }
    let count = reader.uleb()?;
    let mut entries = Vec::new();
    for _ in 0..count {
        let mut path = String::new();
        let mut directory = 0u64;
        for (content, form) in &format {
            let value = form_value(reader, *form, debug_line_str)?;
            match (*content, value) {
                (LNCT_PATH, EntryValue::Text(text)) => path = text,
                (LNCT_DIRECTORY_INDEX, EntryValue::Number(number)) => directory = number,
                _ => {}
            }
        }
        entries.push((path, directory));
    }
    Ok(entries)
}

/// Join file names to their directories: version 5 names carry a directory index, and version 5
/// directories are numbered from 0.
fn join_files(names: &[(String, u64)], directories: &[(String, u64)]) -> Vec<String> {
    let dirs: Vec<String> = directories.iter().map(|(path, _)| path.clone()).collect();
    names
        .iter()
        .map(|(name, directory)| join_name(name, *directory, &dirs))
        .collect()
}

/// A file name joined to its directory, when the index names one and the name is not absolute.
fn join_name(name: &str, directory: u64, directories: &[String]) -> String {
    if name.starts_with('/') || name.contains(':') {
        return name.to_owned();
    }
    match usize::try_from(directory)
        .ok()
        .and_then(|index| directories.get(index))
    {
        Some(dir) if !dir.is_empty() => format!("{dir}/{name}"),
        _ => name.to_owned(),
    }
}

/// The parts of a unit header the line program needs.
struct Program {
    min_inst_length: u64,
    default_is_stmt: bool,
    line_base: i64,
    line_range: u64,
    opcode_base: u8,
    standard_lengths: Vec<u8>,
}

/// The state machine's registers.
#[derive(Clone, Copy)]
struct Registers {
    address: u64,
    file: u64,
    line: u64,
    column: u64,
    is_stmt: bool,
}

impl Program {
    fn run(&self, reader: &mut Reader<'_>) -> Result<Vec<Sequence>, LineError> {
        let initial = Registers {
            address: 0,
            file: 1,
            line: 1,
            column: 0,
            is_stmt: self.default_is_stmt,
        };
        let mut regs = initial;
        let mut sequences = Vec::new();
        let mut rows: Vec<LineRow> = Vec::new();

        while reader.at < reader.end() {
            let at = reader.at;
            let opcode = reader.u8()?;
            if opcode >= self.opcode_base {
                let adjusted = u64::from(opcode - self.opcode_base);
                regs.address = regs
                    .address
                    .wrapping_add((adjusted / self.line_range).wrapping_mul(self.min_inst_length));
                let delta = self.line_base + (adjusted % self.line_range) as i64;
                regs.line = apply_line_delta(regs.line, delta, at)?;
                rows.push(row(&regs, false));
                continue;
            }
            match opcode {
                0 => {
                    let len = reader.uleb()? as usize;
                    let end = reader
                        .at
                        .checked_add(len)
                        .filter(|end| *end <= reader.end())
                        .ok_or(LineError::Truncated { offset: at })?;
                    if len == 0 {
                        return Err(LineError::Malformed {
                            offset: at,
                            what: "an empty extended opcode",
                        });
                    }
                    let sub = reader.u8()?;
                    match sub {
                        1 => {
                            rows.push(row(&regs, true));
                            sequences.push(Sequence {
                                rows: std::mem::take(&mut rows),
                            });
                            regs = initial;
                        }
                        2 => {
                            let width = len - 1;
                            if width == 0 || width > 8 {
                                return Err(LineError::Malformed {
                                    offset: at,
                                    what: "an address of an unsupported width",
                                });
                            }
                            let mut bytes = [0u8; 8];
                            bytes[..width].copy_from_slice(reader.take(width)?);
                            regs.address = u64::from_le_bytes(bytes);
                        }
                        _ => {}
                    }
                    reader.at = end;
                }
                1 => rows.push(row(&regs, false)),
                2 => {
                    let operand = reader.uleb()?;
                    regs.address = regs
                        .address
                        .wrapping_add(operand.wrapping_mul(self.min_inst_length));
                }
                3 => {
                    let delta = reader.sleb()?;
                    regs.line = apply_line_delta(regs.line, delta, at)?;
                }
                4 => regs.file = reader.uleb()?,
                5 => regs.column = reader.uleb()?,
                6 => regs.is_stmt = !regs.is_stmt,
                7 | 10 | 11 => {}
                8 => {
                    let adjusted = u64::from(255 - self.opcode_base);
                    regs.address = regs.address.wrapping_add(
                        (adjusted / self.line_range).wrapping_mul(self.min_inst_length),
                    );
                }
                9 => regs.address = regs.address.wrapping_add(u64::from(reader.u16()?)),
                12 => {
                    reader.uleb()?;
                }
                _ => {
                    // An opcode this reader does not know: the header says how many ULEB128
                    // operands it takes, so they are read past without interpretation.
                    let count = self.standard_lengths[opcode as usize];
                    for _ in 0..count {
                        reader.uleb()?;
                    }
                }
            }
        }
        if !rows.is_empty() {
            return Err(LineError::Malformed {
                offset: reader.end(),
                what: "a line program that ends before end_sequence",
            });
        }
        Ok(sequences)
    }
}

fn row(regs: &Registers, end_sequence: bool) -> LineRow {
    LineRow {
        address: regs.address,
        file: regs.file,
        line: regs.line,
        column: regs.column,
        is_stmt: regs.is_stmt,
        end_sequence,
    }
}

fn apply_line_delta(line: u64, delta: i64, at: usize) -> Result<u64, LineError> {
    let next = i128::from(line) + i128::from(delta);
    u64::try_from(next).map_err(|_| LineError::Malformed {
        offset: at,
        what: "a line number before line zero",
    })
}
