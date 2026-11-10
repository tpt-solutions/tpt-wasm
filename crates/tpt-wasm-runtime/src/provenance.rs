// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 TPT Solutions

//! Execution provenance: what a run was, and what it returned, in a form a second
//! party can check.
//!
//! The spec (section 53) names the record: module hash, engine, semantics, and host
//! versions, and the capabilities the module reaches. Section 52 adds the inputs,
//! the configuration, and the result, so that a simulation can be re-run and its
//! output compared. [`ExecutionMetadata`] is the identity of a run and
//! [`ExecutionArtifact`] is the run itself, with a [`digest`](ExecutionArtifact::digest)
//! that two runs share exactly when they are the same run.
//!
//! ## What the digest covers, and what it does not
//!
//! The digest is SHA-256 over a canonical, length-prefixed encoding of the metadata,
//! the export, the inputs, and the outcome. Numeric values are encoded by their bits,
//! so a NaN payload is part of the result. Reference values and the trap are encoded
//! through their `Debug` form, which is stable within one version of the crate; the
//! artifact records the versions, so a verifier can tell when that is not enough.
//!
//! The digest does not cover the time a run took, the host's own state outside what
//! the module returned, or the debugging and profiling flags, because none of those
//! change the result.

use sha2::{Digest, Sha256};
use tpt_wasm_capability::CapabilityId;
use tpt_wasm_format::{ImportDesc, Module};
use tpt_wasm_types::{ResourceLimits, Trap, Value};

use crate::cache::{COMPILER_VERSION, ENGINE_VERSION, SEMANTICS_VERSION};
use crate::{Config, Engine, EngineMode, Features, RuntimeError};

/// A SHA-256 digest.
pub type Hash = [u8; 32];

/// SHA-256 of `bytes`.
pub fn hash_bytes(bytes: &[u8]) -> Hash {
    Sha256::digest(bytes).into()
}

/// The settings that decide what a run computes.
///
/// `debugging` and `profiling` are left out on purpose: they observe a run and do
/// not change it, so two runs that differ only in them are the same run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Configuration {
    pub engine_mode: EngineMode,
    pub features: Features,
    pub limits: ResourceLimits,
    pub deterministic: bool,
}

impl Configuration {
    pub fn of(config: &Config) -> Self {
        Self {
            engine_mode: config.engine_mode,
            features: config.features,
            limits: config.limits,
            deterministic: config.deterministic,
        }
    }
}

/// The identity of a run: what module, which engine, and which settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionMetadata {
    pub module_hash: Hash,
    pub engine_version: &'static str,
    pub semantics_version: &'static str,
    /// The version of the host boundary. The boundary lives in this crate, so this
    /// is the runtime's version.
    pub host_version: &'static str,
    pub compiler_version: &'static str,
    /// The function imports, by `module::name`. Only a function import can reach a
    /// host capability; table, memory, and global imports cannot, so they are not
    /// listed.
    pub capabilities: Vec<CapabilityId>,
    pub configuration: Configuration,
}

impl ExecutionMetadata {
    /// The metadata for `bytes`, whose decoded form is `module`, run under `config`.
    pub fn for_module(bytes: &[u8], module: &Module, config: &Config) -> Self {
        let capabilities = module
            .imports
            .iter()
            .filter_map(|import| match import.desc {
                ImportDesc::Function(_) => Some(CapabilityId::new(format!(
                    "{}::{}",
                    import.module, import.name
                ))),
                _ => None,
            })
            .collect();
        Self {
            module_hash: hash_bytes(bytes),
            engine_version: ENGINE_VERSION,
            semantics_version: SEMANTICS_VERSION,
            host_version: ENGINE_VERSION,
            compiler_version: COMPILER_VERSION,
            capabilities,
            configuration: Configuration::of(config),
        }
    }
}

/// How a run ended, as an embedder can observe it.
///
/// A trap is an outcome rather than an error: a module that traps has still
/// computed something worth recording. Errors that stop a run from starting, such
/// as a missing export, are not outcomes and are reported by `Engine::record`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Returned(Vec<Value>),
    Trapped(Trap),
}

/// One run of an exported function: its identity, its inputs, and its outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionArtifact {
    pub metadata: ExecutionMetadata,
    pub export: String,
    pub inputs: Vec<Value>,
    pub outcome: Outcome,
}

/// The reproduction check's verdict. Anything other than `Reproduced` says which
/// precondition failed, so a mismatch is not reported as a bare "no".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reproduction {
    /// Re-running the export gave an artifact with the same digest.
    Reproduced,
    /// The bytes do not hash to the artifact's module hash.
    ModuleMismatch,
    /// The artifact was made by a different engine, semantics, or optimizer.
    VersionMismatch,
    /// The engine's configuration differs from the one recorded.
    ConfigurationMismatch,
    /// The re-run's digest differs from the artifact's.
    OutcomeDiverged,
}

impl ExecutionArtifact {
    /// SHA-256 over the canonical encoding of this run.
    pub fn digest(&self) -> Hash {
        let mut out = Vec::new();
        put_bytes(&mut out, b"tpt-wasm/execution-artifact/v1");
        put_bytes(&mut out, &self.metadata.module_hash);
        put_str(&mut out, self.metadata.engine_version);
        put_str(&mut out, self.metadata.semantics_version);
        put_str(&mut out, self.metadata.host_version);
        put_str(&mut out, self.metadata.compiler_version);
        put_u64(&mut out, self.metadata.capabilities.len() as u64);
        for capability in &self.metadata.capabilities {
            put_str(&mut out, capability.as_str());
        }
        put_str(&mut out, &format!("{:?}", self.metadata.configuration));
        put_str(&mut out, &self.export);
        put_u64(&mut out, self.inputs.len() as u64);
        for value in &self.inputs {
            put_value(&mut out, value);
        }
        match &self.outcome {
            Outcome::Returned(values) => {
                out.push(0);
                put_u64(&mut out, values.len() as u64);
                for value in values {
                    put_value(&mut out, value);
                }
            }
            Outcome::Trapped(trap) => {
                out.push(1);
                put_str(&mut out, &format!("{trap:?}"));
            }
        }
        Sha256::digest(&out).into()
    }
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_u64(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    put_bytes(out, text.as_bytes());
}

/// One value: a type tag, then its bits. Floats are their raw bits, so NaN payloads
/// are encoded as they are.
fn put_value(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::I32(x) => {
            out.push(0);
            out.extend_from_slice(&x.to_le_bytes());
        }
        Value::I64(x) => {
            out.push(1);
            out.extend_from_slice(&x.to_le_bytes());
        }
        Value::F32(bits) => {
            out.push(2);
            out.extend_from_slice(&bits.to_le_bytes());
        }
        Value::F64(bits) => {
            out.push(3);
            out.extend_from_slice(&bits.to_le_bytes());
        }
        Value::V128(x) => {
            out.push(4);
            out.extend_from_slice(&x.to_le_bytes());
        }
        Value::Ref(reference) => {
            out.push(5);
            put_str(out, &format!("{reference:?}"));
        }
    }
}

impl Engine {
    /// Run `export` on a fresh instance of `bytes` and record the run.
    ///
    /// The config is the engine's own, so the artifact is only reproducible on an
    /// engine configured the same way; `reproduce` checks that.
    pub fn record(
        &self,
        bytes: &[u8],
        export: &str,
        inputs: Vec<Value>,
    ) -> Result<ExecutionArtifact, RuntimeError> {
        let module = tpt_wasm_decode::decode(bytes).map_err(RuntimeError::Decode)?;
        let metadata = ExecutionMetadata::for_module(bytes, &module, self.config());
        let mut instance = self.instantiate_bytes(bytes)?;
        let outcome = match instance.call(export, inputs.clone()) {
            Ok(values) => Outcome::Returned(values),
            Err(RuntimeError::Trap(trap)) => Outcome::Trapped(trap),
            Err(error) => return Err(error),
        };
        Ok(ExecutionArtifact {
            metadata,
            export: export.to_owned(),
            inputs,
            outcome,
        })
    }

    /// Check that `artifact` is what this engine produces for `bytes`.
    ///
    /// The preconditions are checked first, in the order that makes each failure
    /// mean one thing: the bytes, then the versions, then the configuration. Only
    /// when all three agree is the export run again and its digest compared.
    pub fn reproduce(
        &self,
        artifact: &ExecutionArtifact,
        bytes: &[u8],
    ) -> Result<Reproduction, RuntimeError> {
        let metadata = &artifact.metadata;
        if hash_bytes(bytes) != metadata.module_hash {
            return Ok(Reproduction::ModuleMismatch);
        }
        if metadata.engine_version != ENGINE_VERSION
            || metadata.semantics_version != SEMANTICS_VERSION
            || metadata.host_version != ENGINE_VERSION
            || metadata.compiler_version != COMPILER_VERSION
        {
            return Ok(Reproduction::VersionMismatch);
        }
        if metadata.configuration != Configuration::of(self.config()) {
            return Ok(Reproduction::ConfigurationMismatch);
        }
        let rerun = self.record(bytes, &artifact.export, artifact.inputs.clone())?;
        if rerun.digest() == artifact.digest() {
            Ok(Reproduction::Reproduced)
        } else {
            Ok(Reproduction::OutcomeDiverged)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_wasm_format::{Export, ExportDesc, Function};
    use tpt_wasm_types::{FunctionType, ResultType, ValueType};

    /// `run` (no parameters, returns `i32`) whose body is `unreachable`, so it traps.
    fn trapping_module() -> Module {
        Module {
            types: vec![FunctionType {
                params: ResultType(Vec::new()),
                results: ResultType(vec![ValueType::I32]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![0x00, 0x0b],
            }],
            exports: vec![Export {
                name: "run".into(),
                desc: ExportDesc::Function(0),
            }],
            ..Module::default()
        }
    }

    /// `add` (two `i32` parameters, returns their sum), so inputs affect the result.
    fn adding_module() -> Module {
        Module {
            types: vec![FunctionType {
                params: ResultType(vec![ValueType::I32, ValueType::I32]),
                results: ResultType(vec![ValueType::I32]),
            }],
            functions: vec![Function {
                type_index: 0,
                locals: Vec::new(),
                body: vec![0x20, 0x00, 0x20, 0x01, 0x6a, 0x0b],
            }],
            exports: vec![Export {
                name: "add".into(),
                desc: ExportDesc::Function(0),
            }],
            ..Module::default()
        }
    }

    fn engine(mode: EngineMode) -> Engine {
        Engine::new(Config {
            engine_mode: mode,
            ..Config::default()
        })
        .expect("the mode should be accepted")
    }

    fn bytes_of(module: &Module) -> Vec<u8> {
        tpt_wasm_decode::encode(module).expect("the fixture must encode")
    }

    #[test]
    fn sha256_matches_the_published_test_vector() {
        // FIPS 180-2, "abc": if the hash were wrong every module hash would be too.
        let digest = hash_bytes(b"abc");
        assert_eq!(
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn the_same_run_has_the_same_digest_and_a_different_input_does_not() {
        let bytes = bytes_of(&adding_module());
        let engine = engine(EngineMode::Micro);
        let first = engine
            .record(&bytes, "add", vec![Value::I32(2), Value::I32(3)])
            .expect("the run should record");
        let again = engine
            .record(&bytes, "add", vec![Value::I32(2), Value::I32(3)])
            .expect("the run should record");
        let other = engine
            .record(&bytes, "add", vec![Value::I32(2), Value::I32(4)])
            .expect("the run should record");
        assert_eq!(first.outcome, Outcome::Returned(vec![Value::I32(5)]));
        assert_eq!(first.digest(), again.digest());
        assert_ne!(first.digest(), other.digest());
    }

    #[test]
    fn a_trap_is_recorded_as_an_outcome_and_differs_from_a_return() {
        let bytes = bytes_of(&trapping_module());
        let artifact = engine(EngineMode::Micro)
            .record(&bytes, "run", Vec::new())
            .expect("a trapping run still records");
        assert_eq!(artifact.outcome, Outcome::Trapped(Trap::Unreachable));
        assert_ne!(
            artifact.digest(),
            ExecutionArtifact {
                outcome: Outcome::Returned(vec![Value::I32(0)]),
                ..artifact.clone()
            }
            .digest()
        );
    }

    #[test]
    fn a_missing_export_is_an_error_not_an_outcome() {
        let bytes = bytes_of(&adding_module());
        assert!(engine(EngineMode::Micro)
            .record(&bytes, "nope", Vec::new())
            .is_err());
    }

    #[test]
    fn a_recorded_run_reproduces_on_the_same_engine() {
        let bytes = bytes_of(&adding_module());
        let engine = engine(EngineMode::Optimizing);
        let artifact = engine
            .record(&bytes, "add", vec![Value::I32(7), Value::I32(8)])
            .expect("the run should record");
        assert_eq!(
            engine
                .reproduce(&artifact, &bytes)
                .expect("the check should run"),
            Reproduction::Reproduced
        );
    }

    #[test]
    fn reproduction_names_the_precondition_that_failed() {
        let bytes = bytes_of(&adding_module());
        let engine = engine(EngineMode::Micro);
        let artifact = engine
            .record(&bytes, "add", vec![Value::I32(1), Value::I32(1)])
            .expect("the run should record");

        let mut other_bytes = bytes.clone();
        *other_bytes.last_mut().expect("non-empty") ^= 0x01;
        assert_eq!(
            engine.reproduce(&artifact, &other_bytes).expect("runs"),
            Reproduction::ModuleMismatch
        );

        let mut older = artifact.clone();
        older.metadata.engine_version = "0.0.0";
        assert_eq!(
            engine.reproduce(&older, &bytes).expect("runs"),
            Reproduction::VersionMismatch
        );

        let changed = engine_with_limits_changed();
        assert_eq!(
            changed.reproduce(&artifact, &bytes).expect("runs"),
            Reproduction::ConfigurationMismatch
        );

        let mut tampered = artifact.clone();
        tampered.outcome = Outcome::Returned(vec![Value::I32(99)]);
        assert_eq!(
            engine.reproduce(&tampered, &bytes).expect("runs"),
            Reproduction::OutcomeDiverged
        );
    }

    /// An engine that differs from the default only in a resource limit, which is
    /// part of the configuration a run is reproduced under.
    fn engine_with_limits_changed() -> Engine {
        let limits = ResourceLimits {
            max_execution_steps: Some(12_345),
            ..ResourceLimits::default()
        };
        Engine::new(Config {
            engine_mode: EngineMode::Micro,
            limits,
            ..Config::default()
        })
        .expect("the mode should be accepted")
    }

    #[test]
    fn metadata_lists_function_imports_as_capabilities() {
        let module = adding_module();
        let metadata =
            ExecutionMetadata::for_module(&bytes_of(&module), &module, &Config::default());
        assert!(metadata.capabilities.is_empty());
        let mut with_import = module.clone();
        with_import.imports.push(tpt_wasm_format::Import {
            module: "clock".into(),
            name: "now".into(),
            desc: ImportDesc::Function(0),
        });
        let metadata = ExecutionMetadata::for_module(
            &bytes_of(&with_import),
            &with_import,
            &Config::default(),
        );
        assert_eq!(metadata.capabilities, vec![CapabilityId::new("clock::now")]);
    }
}
