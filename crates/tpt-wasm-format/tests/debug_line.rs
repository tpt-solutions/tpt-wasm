// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Line-table reading, against units built by hand from the DWARF rules.

use tpt_wasm_format::debug_line::{parse, LineError};

/// The standard header fields that follow `opcode_base`: one ULEB operand count per standard
/// opcode 1 through 12, as the DWARF standard gives them.
const STANDARD_LENGTHS: [u8; 12] = [0, 1, 1, 1, 1, 0, 0, 0, 1, 0, 0, 1];

/// `DW_LNS_*` and `DW_LNE_*`, written out so the programs below read as their opcodes.
const END_SEQUENCE: [u8; 3] = [0x00, 0x01, 0x01];

/// `DW_LNE_set_address` to an 8-byte address.
fn set_address(address: u64) -> Vec<u8> {
    let mut bytes = vec![0x00, 0x09, 0x02];
    bytes.extend_from_slice(&address.to_le_bytes());
    bytes
}

/// The parameters every test unit uses: `line_base` -5, `line_range` 14, `opcode_base` 13.
/// A special opcode with address advance `a` and line advance `l` is `(l + 5) + 14 * a + 13`.
fn special(address_advance: u64, line_advance: i64) -> u8 {
    let op = (line_advance + 5) + 14 * address_advance as i64 + 13;
    u8::try_from(op).expect("a special opcode")
}

/// A version 4 unit with one file, `a.c`, in directory `src`, around `program`.
fn v4_unit(program: &[u8]) -> Vec<u8> {
    let mut header = vec![
        1,    // minimum_instruction_length
        1,    // maximum_operations_per_instruction
        1,    // default_is_stmt
        0xfb, // line_base = -5
        14,   // line_range
        13,   // opcode_base
    ];
    header.extend_from_slice(&STANDARD_LENGTHS);
    header.extend_from_slice(b"src\0\0"); // one directory, then the end of the list
    header.extend_from_slice(b"a.c\0"); // file 1: name, directory 1, mtime 0, length 0
    header.extend_from_slice(&[1, 0, 0]);
    header.push(0); // end of the file list

    let mut after_length = Vec::new();
    after_length.extend_from_slice(&4u16.to_le_bytes());
    after_length.extend_from_slice(&(header.len() as u32).to_le_bytes());
    after_length.extend_from_slice(&header);
    after_length.extend_from_slice(program);

    let mut unit = (after_length.len() as u32).to_le_bytes().to_vec();
    unit.extend_from_slice(&after_length);
    unit
}

/// A version 5 unit: the directory `/proj` and `src`, one file `main.c` in `src` (index 1), with
/// the strings in a `.debug_line_str` section, and the file's MD5 as a 16-byte block.
fn v5_unit(program: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let line_str = b"main.c\0".to_vec();
    let mut header = vec![
        1,    // minimum_instruction_length
        1,    // maximum_operations_per_instruction
        1,    // default_is_stmt
        0xfb, // line_base
        14,   // line_range
        13,   // opcode_base
    ];
    header.extend_from_slice(&STANDARD_LENGTHS);
    // Directories: one format (path as an inline string), two entries.
    header.extend_from_slice(&[1, 0x01, 0x08, 2]);
    header.extend_from_slice(b"/proj\0src\0");
    // Files: three formats (path as a line_strp, directory index as udata, MD5 as data16), one
    // entry: main.c in directory 1.
    header.extend_from_slice(&[3, 0x01, 0x1f, 0x02, 0x0f, 0x05, 0x1e, 1]);
    header.extend_from_slice(&0u32.to_le_bytes());
    header.push(1);
    header.extend_from_slice(&[0u8; 16]);

    let mut after_length = Vec::new();
    after_length.extend_from_slice(&5u16.to_le_bytes());
    after_length.push(8); // address_size
    after_length.push(0); // segment_selector_size
    after_length.extend_from_slice(&(header.len() as u32).to_le_bytes());
    after_length.extend_from_slice(&header);
    after_length.extend_from_slice(program);

    let mut unit = (after_length.len() as u32).to_le_bytes().to_vec();
    unit.extend_from_slice(&after_length);
    (unit, line_str)
}

/// Sequence at 0x10: line 5 at 0x10, then line 6 at 0x18 in column 3, ending at 0x20.
fn two_row_program() -> Vec<u8> {
    let mut program = set_address(0x10);
    program.extend_from_slice(&[0x03, 0x04]); // advance_line 4: line 5
    program.push(0x01); // copy: row 0x10, line 5
    program.extend_from_slice(&[0x02, 0x06]); // advance_pc 6: 0x16
    program.extend_from_slice(&[0x05, 0x03]); // set_column 3
    program.push(special(2, 1)); // address +2, line +1: row 0x18, line 6, column 3
    program.extend_from_slice(&[0x02, 0x08]); // advance_pc 8: 0x20
    program.extend_from_slice(&END_SEQUENCE);
    program
}

#[test]
fn a_version_4_unit_gives_its_file_names_and_rows() {
    let tables = parse(&v4_unit(&two_row_program()), &[]).expect("a valid unit");
    assert_eq!(tables.len(), 1);
    let table = &tables[0];
    assert_eq!(table.version(), 4);
    // Version 4 numbers files from 1, so register 1 is the first entry, joined to its directory.
    assert_eq!(table.file_name(1), Some("src/a.c"));
    assert_eq!(table.file_name(0), None);

    let rows: Vec<_> = table
        .rows()
        .map(|r| (r.address, r.line, r.column))
        .collect();
    assert_eq!(
        rows,
        vec![(0x10, 5, 0), (0x18, 6, 3), (0x20, 6, 3)],
        "the last row is the end of the sequence"
    );
    assert!(table.rows().last().expect("a row").end_sequence);
}

#[test]
fn an_address_maps_to_the_row_that_covers_it() {
    let tables = parse(&v4_unit(&two_row_program()), &[]).expect("a valid unit");
    let table = &tables[0];
    assert_eq!(table.lookup(0x0f), None, "before the sequence");
    assert_eq!(table.lookup(0x10).map(|r| r.line), Some(5));
    assert_eq!(table.lookup(0x17).map(|r| r.line), Some(5));
    assert_eq!(table.lookup(0x18).map(|r| r.line), Some(6));
    assert_eq!(table.lookup(0x1f).map(|r| (r.line, r.column)), Some((6, 3)));
    assert_eq!(table.lookup(0x20), None, "one past the end of the sequence");
}

#[test]
fn a_later_row_at_the_same_address_wins() {
    let mut program = set_address(0x40);
    program.extend_from_slice(&[0x03, 0x01]); // line 2
    program.push(0x01); // row 0x40, line 2
    program.extend_from_slice(&[0x03, 0x02]); // line 4
    program.push(0x01); // row 0x40, line 4
    program.extend_from_slice(&[0x02, 0x02]); // 0x42
    program.extend_from_slice(&END_SEQUENCE);
    let tables = parse(&v4_unit(&program), &[]).expect("a valid unit");
    assert_eq!(tables[0].lookup(0x40).map(|r| r.line), Some(4));
    assert_eq!(tables[0].lookup(0x41).map(|r| r.line), Some(4));
}

#[test]
fn two_sequences_are_each_found_at_their_own_addresses() {
    let mut program = set_address(0x100);
    program.push(0x01);
    program.extend_from_slice(&[0x02, 0x04]);
    program.extend_from_slice(&END_SEQUENCE);
    program.extend_from_slice(&set_address(0x200));
    program.extend_from_slice(&[0x03, 0x09]); // line 10
    program.push(0x01);
    program.extend_from_slice(&[0x02, 0x04]);
    program.extend_from_slice(&END_SEQUENCE);
    let tables = parse(&v4_unit(&program), &[]).expect("a valid unit");
    let table = &tables[0];
    assert_eq!(table.lookup(0x101).map(|r| r.line), Some(1));
    assert_eq!(table.lookup(0x150), None, "between the sequences");
    assert_eq!(table.lookup(0x201).map(|r| r.line), Some(10));
}

#[test]
fn a_version_5_unit_reads_its_strings_from_debug_line_str() {
    let mut program = set_address(0x40);
    program.extend_from_slice(&[0x04, 0x00]); // set_file 0
    program.push(0x01); // row 0x40 in file 0
    program.extend_from_slice(&[0x02, 0x04]);
    program.extend_from_slice(&END_SEQUENCE);
    let (unit, line_str) = v5_unit(&program);
    let tables = parse(&unit, &line_str).expect("a valid unit");
    let table = &tables[0];
    assert_eq!(table.version(), 5);
    // Version 5 numbers files from 0, and the joined name is the directory plus the file.
    assert_eq!(table.file_name(0), Some("src/main.c"));
    assert_eq!(table.file_name(1), None);
    assert_eq!(table.lookup(0x41).map(|r| r.file), Some(0));
}

#[test]
fn a_version_5_line_str_offset_past_the_section_is_malformed() {
    let (unit, _) = v5_unit(&[0x00, 0x01, 0x01]);
    let error = parse(&unit, &[]).expect_err("no .debug_line_str to read the name from");
    assert!(matches!(error, LineError::Malformed { .. }), "{error:?}");
}

#[test]
fn an_unknown_version_is_refused_with_its_number() {
    let mut unit = v4_unit(&two_row_program());
    unit[4] = 3; // version 3
    unit[5] = 0;
    assert_eq!(
        parse(&unit, &[]),
        Err(LineError::UnsupportedVersion {
            offset: 0,
            version: 3
        })
    );
}

#[test]
fn a_64_bit_unit_is_refused() {
    let mut unit = vec![0xff, 0xff, 0xff, 0xff];
    unit.extend_from_slice(&[0; 12]);
    assert!(matches!(
        parse(&unit, &[]),
        Err(LineError::Unsupported { offset: 0, .. })
    ));
}

#[test]
fn a_unit_cut_short_is_truncated_not_read_past_its_end() {
    let unit = v4_unit(&two_row_program());
    for cut in 1..unit.len() {
        let result = parse(&unit[..cut], &[]);
        assert!(
            result.is_err(),
            "a unit cut at {cut} of {} parsed",
            unit.len()
        );
    }
}

#[test]
fn a_program_without_end_sequence_is_malformed() {
    let mut program = set_address(0x10);
    program.push(0x01);
    let error = parse(&v4_unit(&program), &[]).expect_err("no end_sequence");
    assert!(matches!(error, LineError::Malformed { .. }), "{error:?}");
}

#[test]
fn a_line_before_zero_is_malformed() {
    let mut program = set_address(0x10);
    program.extend_from_slice(&[0x03, 0x7e]); // advance_line -2 from line 1
    program.push(0x01);
    program.extend_from_slice(&END_SEQUENCE);
    let error = parse(&v4_unit(&program), &[]).expect_err("line -1");
    assert!(matches!(error, LineError::Malformed { .. }), "{error:?}");
}

#[test]
fn a_garbage_section_never_panics() {
    // Every single-byte corruption of a good unit must parse or fail cleanly.
    let good = v4_unit(&two_row_program());
    for at in 0..good.len() {
        for value in [0x00, 0x01, 0x7f, 0x80, 0xff] {
            let mut bytes = good.clone();
            bytes[at] = value;
            let _ = parse(&bytes, &[]);
        }
    }
}

#[test]
fn several_units_in_one_section_are_all_read() {
    let mut section = v4_unit(&two_row_program());
    section.extend_from_slice(&v4_unit(&two_row_program()));
    let tables = parse(&section, &[]).expect("two valid units");
    assert_eq!(tables.len(), 2);
}
