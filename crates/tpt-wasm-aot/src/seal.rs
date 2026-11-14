// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Authentication of AOT artifacts with a key the loader holds.
//!
//! An artifact is executable code, so the checksum in its header is not enough: it is
//! recomputable by anyone who can write the file. A sealed artifact adds an HMAC-SHA256 tag,
//! computed with a shared secret, over the whole artifact. [`open`] returns the artifact only
//! when the tag verifies, and [`ModuleArtifact::from_bytes`](crate::ModuleArtifact::from_bytes)
//! or [`Artifact::from_bytes`](crate::Artifact::from_bytes) then parses it as before.
//!
//! ## Layout
//!
//! | Field | Size | Contents |
//! |---|---|---|
//! | magic | 8 | `TPTSEAL\0` and the seal version byte, `1` |
//! | tag | 32 | HMAC-SHA256 of the artifact bytes, under the key |
//! | artifact | n | the unchanged artifact, as [`Artifact::to_bytes`](crate::Artifact::to_bytes) writes it |
//!
//! ## Keys
//!
//! The key is supplied by the embedder and must be at least [`MIN_KEY_LEN`] bytes. Shorter keys
//! are refused rather than stretched, because a weak key makes the tag forgeable by search. The
//! key is a shared secret: whoever can seal can also load, so it belongs with the code that
//! produces artifacts, not beside the artifacts.
//!
//! ## What it does not do
//!
//! Sealing does not make an artifact safe to run in the sense of the code it contains. It makes
//! it attributable to a holder of the key. Choosing the key, rotating it, and deciding which
//! artifacts a deployment trusts are policy, and this module does not make them.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::AotError;

type HmacSha256 = Hmac<Sha256>;

/// The magic and seal version that open a sealed artifact.
pub const SEAL_MAGIC: &[u8; 8] = b"TPTSEAL\x01";

/// The shortest key [`seal`] and [`open`] accept, in bytes.
pub const MIN_KEY_LEN: usize = 32;

/// The length of the HMAC-SHA256 tag.
pub const TAG_LEN: usize = 32;

/// Seal an artifact under `key`, returning the sealed bytes.
///
/// # Errors
///
/// [`AotError::KeyTooShort`] when the key is shorter than [`MIN_KEY_LEN`].
pub fn seal(artifact: &[u8], key: &[u8]) -> Result<Vec<u8>, AotError> {
    let mut mac = keyed(key)?;
    mac.update(artifact);
    let tag = mac.finalize().into_bytes();
    let mut sealed = Vec::with_capacity(SEAL_MAGIC.len() + TAG_LEN + artifact.len());
    sealed.extend_from_slice(SEAL_MAGIC);
    sealed.extend_from_slice(&tag);
    sealed.extend_from_slice(artifact);
    Ok(sealed)
}

/// Check a sealed artifact against `key` and return the artifact bytes inside it.
///
/// The tag is compared in constant time. The returned slice is the artifact exactly as sealed,
/// so it can be handed straight to an artifact's `from_bytes`.
///
/// # Errors
///
/// - [`AotError::KeyTooShort`] when the key is shorter than [`MIN_KEY_LEN`].
/// - [`AotError::BadMagic`] when the bytes do not start with [`SEAL_MAGIC`].
/// - [`AotError::Truncated`] when the bytes end before the tag does.
/// - [`AotError::Unauthenticated`] when the tag does not match: a wrong key, or an artifact or
///   tag that was changed after sealing.
pub fn open<'a>(sealed: &'a [u8], key: &[u8]) -> Result<&'a [u8], AotError> {
    let magic = sealed.get(..SEAL_MAGIC.len()).ok_or(AotError::Truncated)?;
    if magic != SEAL_MAGIC {
        return Err(AotError::BadMagic);
    }
    let tag_end = SEAL_MAGIC.len() + TAG_LEN;
    let tag = sealed
        .get(SEAL_MAGIC.len()..tag_end)
        .ok_or(AotError::Truncated)?;
    let artifact = &sealed[tag_end..];
    let mut mac = keyed(key)?;
    mac.update(artifact);
    mac.verify_slice(tag)
        .map_err(|_| AotError::Unauthenticated)?;
    Ok(artifact)
}

fn keyed(key: &[u8]) -> Result<HmacSha256, AotError> {
    if key.len() < MIN_KEY_LEN {
        return Err(AotError::KeyTooShort { length: key.len() });
    }
    HmacSha256::new_from_slice(key).map_err(|_| AotError::KeyTooShort { length: key.len() })
}

#[cfg(test)]
mod tests {
    use super::{open, seal, AotError, MIN_KEY_LEN, SEAL_MAGIC, TAG_LEN};

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";
    const ARTIFACT: &[u8] = b"not really machine code, but the seal does not read it";

    #[test]
    fn a_sealed_artifact_opens_under_its_key_to_the_same_bytes() {
        let sealed = seal(ARTIFACT, KEY).expect("a 32-byte key is accepted");
        assert_eq!(&sealed[..SEAL_MAGIC.len()], SEAL_MAGIC);
        assert_eq!(sealed.len(), SEAL_MAGIC.len() + TAG_LEN + ARTIFACT.len());
        assert_eq!(open(&sealed, KEY), Ok(ARTIFACT));
    }

    #[test]
    fn a_wrong_key_is_refused_as_unauthenticated() {
        let sealed = seal(ARTIFACT, KEY).expect("sealed");
        let other = b"fedcba9876543210fedcba9876543210";
        assert_eq!(open(&sealed, other), Err(AotError::Unauthenticated));
    }

    #[test]
    fn a_changed_byte_in_the_artifact_is_refused() {
        let mut sealed = seal(ARTIFACT, KEY).expect("sealed");
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert_eq!(open(&sealed, KEY), Err(AotError::Unauthenticated));
    }

    #[test]
    fn a_changed_byte_in_the_tag_is_refused() {
        let mut sealed = seal(ARTIFACT, KEY).expect("sealed");
        sealed[SEAL_MAGIC.len()] ^= 0x80;
        assert_eq!(open(&sealed, KEY), Err(AotError::Unauthenticated));
    }

    #[test]
    fn every_truncation_is_refused_and_none_is_accepted() {
        let sealed = seal(ARTIFACT, KEY).expect("sealed");
        for length in 0..SEAL_MAGIC.len() + TAG_LEN {
            assert_eq!(
                open(&sealed[..length], KEY),
                Err(AotError::Truncated),
                "a prefix of {length} bytes"
            );
        }
        // Cutting the artifact itself changes what the tag covers.
        for length in SEAL_MAGIC.len() + TAG_LEN..sealed.len() {
            assert_eq!(
                open(&sealed[..length], KEY),
                Err(AotError::Unauthenticated),
                "a prefix of {length} bytes"
            );
        }
    }

    #[test]
    fn bytes_without_the_seal_magic_are_refused_before_the_tag_is_read() {
        let mut sealed = seal(ARTIFACT, KEY).expect("sealed");
        sealed[0] = b'X';
        assert_eq!(open(&sealed, KEY), Err(AotError::BadMagic));
    }

    #[test]
    fn a_short_key_is_refused_for_sealing_and_for_opening() {
        let short = &KEY[..MIN_KEY_LEN - 1];
        assert_eq!(
            seal(ARTIFACT, short),
            Err(AotError::KeyTooShort {
                length: MIN_KEY_LEN - 1
            })
        );
        let sealed = seal(ARTIFACT, KEY).expect("sealed");
        assert_eq!(
            open(&sealed, short),
            Err(AotError::KeyTooShort {
                length: MIN_KEY_LEN - 1
            })
        );
    }
}
