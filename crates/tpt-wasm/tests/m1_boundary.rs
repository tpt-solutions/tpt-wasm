// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

use tpt_wasm::{decode, validate, DecodeError, ValidationError};

#[test]
fn unresolved_type_index_is_a_validation_error_not_a_decode_error() {
    let mut bytes = b"\0asm\x01\0\0\0".to_vec();
    // Type section is intentionally absent; the function section still has a
    // structurally valid type index.
    bytes.extend_from_slice(&[3, 2, 1, 0]);
    bytes.extend_from_slice(&[10, 4, 1, 2, 0, 0x0b]);

    let module = decode(&bytes).expect("binary structure should decode");
    assert!(matches!(
        validate(module),
        Err(ValidationError::UnknownType(0))
    ));
}

#[test]
fn invalid_header_is_reported_by_decoder() {
    assert_eq!(decode(b"not-wasm"), Err(DecodeError::InvalidMagic));
}
