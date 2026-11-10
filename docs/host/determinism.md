# Deterministic Execution

Deterministic execution mode makes all host interactions reproducible.

```rust
pub enum ExecutionMode {
    Deterministic,
    HostDependent,
    RecordReplay,
}
```

## Deterministic mode

| Resource | Deterministic behavior |
|---|---|
| Clock | Virtual clock — no wall time |
| Randomness | Seeded RNG — same seed, same output |
| Filesystem | Controlled snapshot — no live filesystem |
| Network | Prohibited or recorded |
| Scheduler | Deterministic ordering |

## Value for TPT

- Testing: reproducible failures
- Formal verification: closed-world reasoning
- Scientific computation: reproducible results
- Simulation: verifiable outputs
- TPT workflows: auditable execution

## ExecutionArtifact

Implemented in `crates/tpt-wasm-runtime/src/provenance.rs`.

```rust
pub struct ExecutionMetadata {
    pub module_hash: Hash,          // SHA-256 of the module bytes
    pub engine_version: &'static str,
    pub semantics_version: &'static str,
    pub host_version: &'static str,  // the runtime's version: the host boundary lives there
    pub compiler_version: &'static str,
    pub capabilities: Vec<CapabilityId>, // the module's function imports, as `module::name`
    pub configuration: Configuration,    // mode, features, limits, deterministic
}

pub struct ExecutionArtifact {
    pub metadata: ExecutionMetadata,
    pub export: String,
    pub inputs: Vec<Value>,
    pub outcome: Outcome, // Returned(values) or Trapped(trap)
}
```

`Engine::record` runs an export and returns the artifact; `ExecutionArtifact::digest`
is SHA-256 over a canonical encoding, with numeric values taken by their bits so a
NaN payload is part of the result. `Engine::reproduce` checks, in order, the module
hash, the versions, and the configuration, then re-runs the export and compares
digests, returning which precondition failed rather than a bare "no".

Not covered by the digest: the time a run took, and the debugging and profiling flags,
which observe a run without changing it. Reference values and traps are encoded
through their `Debug` form, so the digest is stable within one version of the crate.
The artifact records the versions, so a verifier can tell when that is not enough.

Provides scientific provenance for simulations, engineering, robotics, and reproducibility.
