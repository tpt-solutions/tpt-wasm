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

## ExecutionArtifact (future)

```rust
pub struct ExecutionMetadata {
    pub module_hash: Hash,
    pub engine_version: Version,
    pub semantics_version: Version,
    pub host_version: Version,
    pub capabilities: Vec<CapabilityId>,
}
```

Provides scientific provenance for simulations, engineering, robotics, and reproducibility.
