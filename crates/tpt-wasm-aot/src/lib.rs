// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Ahead-of-time compilation of the JIT subset into a portable artifact.
//!
//! [`compile_artifact`] generates the machine code for an IR function once, and
//! [`Artifact::to_bytes`] writes it out with a header. Later, [`Artifact::from_bytes`] reads it
//! back and [`Artifact::load`] installs it as a callable function, on the target it was
//! generated for. The generated code has no relocations: jumps are relative and memory is
//! addressed from the frame pointer, so the same bytes run wherever they are mapped.
//!
//! ## What an artifact is, and is not
//!
//! An artifact is executable code. Loading one runs it, so an artifact is not a data format
//! to parse from an untrusted source. The header's checksum catches corruption in storage or
//! transit. It is an FNV-1a 64-bit digest, which detects accidental change but does not
//! authenticate the artifact: a deliberate forgery can carry a matching checksum. Authenticating
//! an artifact, by a signature from a key the loader trusts, is not done here, and is the
//! condition for loading artifacts from anywhere but a trusted store.
//!
//! ## Format
//!
//! All integers are little-endian.
//!
//! | Field | Size | Contents |
//! |---|---|---|
//! | magic | 8 | `TPTAOT\0` and the format version byte, `2` |
//! | target length | 1 | length of the target name |
//! | target | n | [`tpt_wasm_jit::target_id`] of the generating host |
//! | parameters | 1 + n | the number of parameters, then one kind byte each: `0` for `i32`, `1` for `i64`, `2` for `f32`, `3` for `f64` |
//! | result | 1 | the result kind, with the same encoding |
//! | code length | 4 | length of the machine code |
//! | code | m | the machine code |
//! | checksum | 8 | FNV-1a 64 over every byte before it |
//!
//! A module artifact ([`ModuleArtifact`]) has the magic `TPTAOT\0\x03` and holds a whole module:
//! its target, the call-depth limit it was compiled against, whether its indirect calls are
//! compiled, then each function as a present byte and, when present, its signature and code, then
//! the table as a count and one entry per slot: a function index (or `0xFFFFFFFF` for null) and
//! the slot's signature id. Its checksum covers every byte before it, as above.

use std::fmt;

use tpt_wasm_ir::{IrFunction, IrModule};
use tpt_wasm_jit::{
    compile, compile_module, target_id, CompiledFunction, CompiledModule, FunctionParts, JitError,
    ModuleParts, TableSlotParts,
};
use tpt_wasm_types::ValueType;

/// The magic bytes of a single-function artifact, ending with the format version.
const MAGIC: [u8; 8] = *b"TPTAOT\0\x02";
/// The magic bytes of a module artifact: the same family, with its own version.
const MODULE_MAGIC: [u8; 8] = *b"TPTAOT\0\x03";
/// The function index a null table slot holds in a module artifact.
const NO_FUNCTION: u32 = u32::MAX;

/// Why an artifact could not be produced, read, or loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AotError {
    /// The function could not be compiled.
    Compile(JitError),
    /// The bytes end before the header or the declared code does.
    Truncated,
    /// The bytes do not start with the artifact magic and version.
    BadMagic,
    /// The stored checksum does not match the contents.
    ChecksumMismatch { expected: u64, found: u64 },
    /// The artifact was generated for another target than this host.
    TargetMismatch {
        artifact: String,
        host: &'static str,
    },
    /// The header is internally inconsistent: a length that does not fit, or bytes after the
    /// checksum.
    Malformed(String),
    /// The code was read but could not be installed.
    Load(JitError),
}

impl fmt::Display for AotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AotError::Compile(error) => write!(f, "compile: {error}"),
            AotError::Truncated => write!(f, "the artifact is truncated"),
            AotError::BadMagic => write!(f, "not a TPT-Wasm AOT artifact, or an unknown version"),
            AotError::ChecksumMismatch { expected, found } => write!(
                f,
                "checksum mismatch: stored {expected:#018x}, computed {found:#018x}"
            ),
            AotError::TargetMismatch { artifact, host } => {
                write!(f, "the artifact is for {artifact}, but this host is {host}")
            }
            AotError::Malformed(what) => write!(f, "malformed artifact: {what}"),
            AotError::Load(error) => write!(f, "load: {error}"),
        }
    }
}

impl std::error::Error for AotError {}

/// A compiled function in portable form: its target, arity, and machine code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    target: String,
    params: Vec<ValueType>,
    result: ValueType,
    code: Vec<u8>,
}

/// The one-byte encoding of a scalar type in an artifact header. A type the compiler never
/// generates code for encodes as `0xFF`, which no reader accepts, so such an artifact is refused
/// when it is read rather than read as some other type.
fn kind_byte(value_type: ValueType) -> u8 {
    match value_type {
        ValueType::I32 => 0,
        ValueType::I64 => 1,
        ValueType::F32 => 2,
        ValueType::F64 => 3,
        ValueType::V128 | ValueType::Ref(_) => 0xFF,
    }
}

/// The scalar type a header byte names.
fn kind_from_byte(byte: u8) -> Result<ValueType, AotError> {
    match byte {
        0 => Ok(ValueType::I32),
        1 => Ok(ValueType::I64),
        2 => Ok(ValueType::F32),
        3 => Ok(ValueType::F64),
        other => Err(AotError::Malformed(format!(
            "an unknown value kind {other}"
        ))),
    }
}

/// Generate the machine code for `function` and wrap it as an artifact.
///
/// The function must be in the JIT's subset; anything else is refused by the compiler.
pub fn compile_artifact(function: &IrFunction) -> Result<Artifact, AotError> {
    let compiled = compile(function).map_err(AotError::Compile)?;
    Ok(Artifact {
        target: target_id().to_owned(),
        params: compiled.params().to_vec(),
        result: compiled.result(),
        code: compiled.code_bytes().to_vec(),
    })
}

impl Artifact {
    /// The target the code was generated for.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The number of arguments the function takes.
    pub fn arity(&self) -> usize {
        self.params.len()
    }

    /// The parameter types, in order.
    pub fn params(&self) -> &[ValueType] {
        &self.params
    }

    /// The result type.
    pub fn result(&self) -> ValueType {
        self.result
    }

    /// The machine code.
    pub fn code(&self) -> &[u8] {
        &self.code
    }

    /// Serialize the artifact: the header, the code, and the checksum.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(MAGIC.len() + 1 + self.target.len() + 1 + 4 + self.code.len() + 8);
        out.extend_from_slice(&MAGIC);
        out.push(self.target.len() as u8);
        out.extend_from_slice(self.target.as_bytes());
        out.push(self.params.len() as u8);
        out.extend(self.params.iter().map(|value_type| kind_byte(*value_type)));
        out.push(kind_byte(self.result));
        out.extend_from_slice(&(self.code.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.code);
        let checksum = fnv1a(&out);
        out.extend_from_slice(&checksum.to_le_bytes());
        out
    }

    /// Read an artifact back from its bytes, checking the magic, the lengths, and the checksum.
    ///
    /// This does not check the target: that happens in [`Artifact::load`], because a serialized
    /// artifact is a valid thing to hold on any host.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AotError> {
        const CHECKSUM: usize = 8;
        if bytes.len() < MAGIC.len() {
            return Err(AotError::Truncated);
        }
        if bytes[..MAGIC.len()] != MAGIC {
            return Err(AotError::BadMagic);
        }
        let mut cursor = Cursor {
            bytes,
            position: MAGIC.len(),
        };
        let target_length = usize::from(cursor.byte()?);
        let target = std::str::from_utf8(cursor.take(target_length)?)
            .map_err(|_| AotError::Malformed("the target name is not UTF-8".into()))?
            .to_owned();
        let parameter_count = usize::from(cursor.byte()?);
        let params = cursor
            .take(parameter_count)?
            .iter()
            .map(|byte| kind_from_byte(*byte))
            .collect::<Result<Vec<_>, _>>()?;
        let result = kind_from_byte(cursor.byte()?)?;
        let code_length =
            u32::from_le_bytes(cursor.take(4)?.try_into().expect("four bytes")) as usize;
        let code = cursor.take(code_length)?.to_vec();

        // The checksum covers everything before it, so it is read after the code.
        let covered = cursor.position;
        let stored = u64::from_le_bytes(cursor.take(CHECKSUM)?.try_into().expect("eight bytes"));
        if cursor.position != bytes.len() {
            return Err(AotError::Malformed("bytes after the checksum".into()));
        }
        let computed = fnv1a(&bytes[..covered]);
        if computed != stored {
            return Err(AotError::ChecksumMismatch {
                expected: stored,
                found: computed,
            });
        }
        Ok(Self {
            target,
            params,
            result,
            code,
        })
    }

    /// Install the code as a callable function.
    ///
    /// Refused if the artifact was generated for another target. Otherwise the code is
    /// installed as it is: see the crate documentation for why this is the trust boundary.
    pub fn load(&self) -> Result<CompiledFunction, AotError> {
        let host = target_id();
        if self.target != host {
            return Err(AotError::TargetMismatch {
                artifact: self.target.clone(),
                host,
            });
        }
        CompiledFunction::from_machine_code(&self.code, self.params.clone(), self.result)
            .map_err(AotError::Load)
    }
}

#[cfg(test)]
mod module_tests;

/// A module compiled together, in portable form: each function's code, and the module's table.
///
/// The table is stored as function indices and signature ids, not as addresses. The addresses
/// exist only once the code is installed, so [`ModuleArtifact::load`] rebuilds them. Calls need no
/// relocation for the same reason: a compiled caller reaches its callee through the table in its
/// context, which is filled at load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleArtifact {
    target: String,
    parts: ModuleParts,
}

/// Compile every function of `module` together, as the runtime does, and wrap the result.
///
/// `table_private` says whether no other instance can write the module's table, which is what
/// lets its indirect calls be compiled. It is the caller's statement, and the runtime derives it
/// from the module's exports; see [`tpt_wasm_jit::compile_module`].
///
/// # Errors
///
/// A function with more parameters than a header can count is refused, since the artifact could
/// not describe it.
pub fn compile_module_artifact(
    module: &IrModule,
    max_call_depth: u64,
    table_private: bool,
) -> Result<ModuleArtifact, AotError> {
    let parts = compile_module(module, max_call_depth, table_private).parts();
    for function in parts.functions.iter().flatten() {
        if function.params.len() > usize::from(u8::MAX) {
            return Err(AotError::Compile(JitError::Unsupported(
                "a function with more parameters than an artifact header counts".into(),
            )));
        }
    }
    Ok(ModuleArtifact {
        target: target_id().to_owned(),
        parts,
    })
}

impl ModuleArtifact {
    /// The target the code was generated for.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Each function by index: its code, or `None` where it was not compiled.
    pub fn functions(&self) -> &[Option<FunctionParts>] {
        &self.parts.functions
    }

    /// The module's table, slot by slot.
    pub fn table(&self) -> &[TableSlotParts] {
        &self.parts.table
    }

    /// Whether the module's indirect calls are compiled.
    pub fn indirect(&self) -> bool {
        self.parts.indirect
    }

    /// The call-depth limit the calls were compiled against.
    pub fn max_call_depth(&self) -> u64 {
        self.parts.max_call_depth
    }

    /// Serialize the artifact: the header, each function, the table, and the checksum.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MODULE_MAGIC);
        out.push(self.target.len() as u8);
        out.extend_from_slice(self.target.as_bytes());
        out.extend_from_slice(&self.parts.max_call_depth.to_le_bytes());
        out.push(u8::from(self.parts.indirect));
        out.extend_from_slice(&(self.parts.functions.len() as u32).to_le_bytes());
        for function in &self.parts.functions {
            match function {
                None => out.push(0),
                Some(function) => {
                    out.push(1);
                    out.push(function.params.len() as u8);
                    out.extend(
                        function
                            .params
                            .iter()
                            .map(|value_type| kind_byte(*value_type)),
                    );
                    out.push(kind_byte(function.result));
                    out.extend_from_slice(&(function.code.len() as u32).to_le_bytes());
                    out.extend_from_slice(&function.code);
                }
            }
        }
        out.extend_from_slice(&(self.parts.table.len() as u32).to_le_bytes());
        for slot in &self.parts.table {
            let function = slot.function.unwrap_or(NO_FUNCTION);
            out.extend_from_slice(&function.to_le_bytes());
            out.extend_from_slice(&slot.type_id.to_le_bytes());
        }
        let checksum = fnv1a(&out);
        out.extend_from_slice(&checksum.to_le_bytes());
        out
    }

    /// Read a module artifact back, checking the magic, the lengths, the checksum, and that every
    /// table slot names a function the artifact defines. Whether the parts are consistent with
    /// each other is checked again at [`ModuleArtifact::load`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AotError> {
        const CHECKSUM: usize = 8;
        if bytes.len() < MODULE_MAGIC.len() {
            return Err(AotError::Truncated);
        }
        if bytes[..MODULE_MAGIC.len()] != MODULE_MAGIC {
            return Err(AotError::BadMagic);
        }
        let mut cursor = Cursor {
            bytes,
            position: MODULE_MAGIC.len(),
        };
        let target_length = usize::from(cursor.byte()?);
        let target = std::str::from_utf8(cursor.take(target_length)?)
            .map_err(|_| AotError::Malformed("the target name is not UTF-8".into()))?
            .to_owned();
        let max_call_depth = cursor.u64()?;
        let indirect = match cursor.byte()? {
            0 => false,
            1 => true,
            other => {
                return Err(AotError::Malformed(format!(
                    "an indirect flag of {other}, which is neither 0 nor 1"
                )))
            }
        };
        // Each function takes at least one byte, so a count larger than the rest of the bytes
        // cannot be honest. Checking it first keeps a hostile count from reserving memory.
        let function_count = cursor.count(1)?;
        let mut functions = Vec::with_capacity(function_count);
        for _ in 0..function_count {
            match cursor.byte()? {
                0 => functions.push(None),
                1 => {
                    let parameter_count = usize::from(cursor.byte()?);
                    let params = cursor
                        .take(parameter_count)?
                        .iter()
                        .map(|byte| kind_from_byte(*byte))
                        .collect::<Result<Vec<_>, _>>()?;
                    let result = kind_from_byte(cursor.byte()?)?;
                    let code_length = cursor.u32()? as usize;
                    let code = cursor.take(code_length)?.to_vec();
                    functions.push(Some(FunctionParts {
                        params,
                        result,
                        code,
                    }));
                }
                other => {
                    return Err(AotError::Malformed(format!(
                        "a function tag of {other}, which is neither 0 nor 1"
                    )))
                }
            }
        }
        // Each slot takes twelve bytes.
        let slot_count = cursor.count(12)?;
        let mut table = Vec::with_capacity(slot_count);
        for _ in 0..slot_count {
            let function = cursor.u32()?;
            let type_id = cursor.u64()?;
            let function = if function == NO_FUNCTION {
                None
            } else if (function as usize) < functions.len() {
                Some(function)
            } else {
                return Err(AotError::Malformed(format!(
                    "a table slot names function {function}, of {} defined",
                    functions.len()
                )));
            };
            table.push(TableSlotParts { function, type_id });
        }

        // The checksum covers everything before it, so it is read after the table.
        let covered = cursor.position;
        let stored = u64::from_le_bytes(cursor.take(CHECKSUM)?.try_into().expect("eight bytes"));
        if cursor.position != bytes.len() {
            return Err(AotError::Malformed("bytes after the checksum".into()));
        }
        let computed = fnv1a(&bytes[..covered]);
        if computed != stored {
            return Err(AotError::ChecksumMismatch {
                expected: stored,
                found: computed,
            });
        }
        Ok(Self {
            target,
            parts: ModuleParts {
                functions,
                table,
                indirect,
                max_call_depth,
            },
        })
    }

    /// Install the module: each function's code, then the table, rebuilt from the installed
    /// entries.
    ///
    /// Refused if the artifact was generated for another target. The code is installed as it is,
    /// which is the trust boundary of every artifact: see the crate documentation.
    pub fn load(&self) -> Result<CompiledModule, AotError> {
        let host = target_id();
        if self.target != host {
            return Err(AotError::TargetMismatch {
                artifact: self.target.clone(),
                host,
            });
        }
        CompiledModule::from_parts(self.parts.clone()).map_err(AotError::Load)
    }
}

/// A reader over the artifact bytes that refuses to read past the end.
struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], AotError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| AotError::Malformed("a length overflows".into()))?;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or(AotError::Truncated)?;
        self.position = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8, AotError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, AotError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64, AotError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }

    /// A count of items that each take at least `size` bytes. A count that cannot fit in the bytes
    /// that remain is refused before anything is reserved for it.
    fn count(&mut self, size: usize) -> Result<usize, AotError> {
        let count = self.u32()? as usize;
        let remaining = self.bytes.len() - self.position;
        if count > remaining / size {
            return Err(AotError::Truncated);
        }
        Ok(count)
    }
}

/// FNV-1a, 64-bit: a simple, fixed digest for detecting accidental change.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(target: &str, code: Vec<u8>) -> Artifact {
        Artifact {
            target: target.to_owned(),
            params: vec![tpt_wasm_types::ValueType::I32; 2],
            result: tpt_wasm_types::ValueType::I32,
            code,
        }
    }

    #[test]
    fn the_format_round_trips_without_compiling() {
        let artifact = sample("x86_64-unix", vec![0x31, 0xC0, 0xC3]);
        let bytes = artifact.to_bytes();
        assert_eq!(
            &bytes[..8],
            b"TPTAOT\0\x02",
            "the magic and version come first"
        );
        assert_eq!(Artifact::from_bytes(&bytes).expect("reads"), artifact);
    }

    #[test]
    fn a_flipped_byte_is_a_checksum_mismatch() {
        let mut bytes = sample("x86_64-unix", vec![0x31, 0xC0, 0xC3]).to_bytes();
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0x01;
        assert!(matches!(
            Artifact::from_bytes(&bytes),
            Err(AotError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn a_truncated_artifact_is_refused_at_every_length() {
        let bytes = sample("x86_64-unix", vec![0x31, 0xC0, 0xC3]).to_bytes();
        for length in 0..bytes.len() {
            assert!(
                Artifact::from_bytes(&bytes[..length]).is_err(),
                "a {length}-byte prefix must not read as an artifact"
            );
        }
    }

    #[test]
    fn bytes_after_the_checksum_are_refused() {
        let mut bytes = sample("x86_64-unix", vec![0xC3]).to_bytes();
        bytes.push(0);
        assert!(matches!(
            Artifact::from_bytes(&bytes),
            Err(AotError::Malformed(_))
        ));
    }

    #[test]
    fn a_wrong_magic_is_refused() {
        let mut bytes = sample("x86_64-unix", vec![0xC3]).to_bytes();
        bytes[0] = b'X';
        assert_eq!(Artifact::from_bytes(&bytes), Err(AotError::BadMagic));
    }

    #[test]
    fn an_artifact_for_another_target_does_not_load() {
        let artifact = sample("x86_64-elsewhere", vec![0xC3]);
        assert!(matches!(
            artifact.load(),
            Err(AotError::TargetMismatch { ref artifact, .. }) if artifact == "x86_64-elsewhere"
        ));
    }
}

#[cfg(all(
    test,
    any(
        all(windows, target_arch = "x86_64"),
        all(
            unix,
            any(
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "riscv64"
            )
        )
    )
))]
mod execution {
    use super::*;
    use tpt_wasm_format::{Export, ExportDesc, Function, Module};
    use tpt_wasm_types::{FunctionType, ResultType, ValueType};

    /// `(p0 * 3 + p1) ^ (p0 >> 2)`: straight-line code over two parameters.
    fn sample_function() -> IrFunction {
        let module = Module {
            types: vec![FunctionType {
                params: ResultType(vec![ValueType::I32, ValueType::I32]),
                results: ResultType(vec![ValueType::I32]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![
                    0x20, 0x00, 0x41, 0x03, 0x6C, // p0 * 3
                    0x20, 0x01, 0x6A, // + p1
                    0x20, 0x00, 0x41, 0x02, 0x76, // p0 >>u 2
                    0x73, 0x0B, // xor, end
                ],
            }],
            exports: vec![Export {
                name: "run".into(),
                desc: ExportDesc::Function(0),
            }],
            ..Module::default()
        };
        let validated = tpt_wasm_validate::validate(module).expect("validates");
        let verified = tpt_wasm_ir::lower_and_verify(&validated).expect("lowers");
        verified.module().functions[0].clone()
    }

    #[test]
    fn a_loaded_artifact_computes_what_the_compiler_computes() {
        let function = sample_function();
        let direct = compile(&function).expect("compiles");
        let artifact = compile_artifact(&function).expect("compiles to an artifact");
        // Through the bytes, as a store would hold it.
        let restored = Artifact::from_bytes(&artifact.to_bytes()).expect("reads back");
        assert_eq!(restored, artifact);
        let loaded = restored.load().expect("loads on this host");
        for (a, b) in [
            (0, 0),
            (1, 2),
            (-5, 77),
            (i32::MIN, i32::MAX),
            (123_456, -9),
        ] {
            assert_eq!(
                loaded.call(&[a, b]).expect("runs"),
                direct.call(&[a, b]).expect("runs"),
                "args {a}, {b}"
            );
        }
    }

    #[test]
    fn the_same_function_gives_the_same_bytes_every_time() {
        let function = sample_function();
        assert_eq!(
            compile_artifact(&function).expect("compiles").to_bytes(),
            compile_artifact(&function).expect("compiles").to_bytes(),
            "generation is deterministic, so an artifact can be compared by its bytes"
        );
    }
}
