# tpt-wasm-aot

Ahead-of-time compilation of the JIT subset into a portable artifact, for
[TPT-Wasm](https://github.com/tpt-solutions/tpt-wasm).

> **Status: partial.** Artifacts hold the machine code the JIT generates for the functions in
> its subset, for the host that generated them. Nothing here lowers other code, and nothing
> connects an artifact to `Engine`.

## Use

```rust
let artifact = tpt_wasm_aot::compile_artifact(function)?;   // generate once
std::fs::write("add.tptaot", artifact.to_bytes())?;

let bytes = std::fs::read("add.tptaot")?;                    // later
let loaded = tpt_wasm_aot::Artifact::from_bytes(&bytes)?.load()?;
let result = loaded.call(&[2, 3])?;
```

## What an artifact is

An artifact is executable code. `load` runs it, so it is a trust decision: load only artifacts
from a store you trust. The header's FNV-1a checksum detects corruption; it does not
authenticate the artifact, and a forged artifact can carry a valid checksum. Authentication by
signature is not implemented.

Artifacts are target-specific: the target names are `x86_64-windows`, `x86_64-unix`,
`aarch64-unix`, and `riscv64-unix`, and on x86-64 a host with SSE4.1 names its target with a
`+sse4.1` suffix, such as `x86_64-unix+sse4.1`. The suffix is recorded because the rounding
functions need that instruction set. `load` refuses an artifact generated for another target,
so an artifact from an SSE4.1 host does not load on an older one, even when it uses no rounding.

The format is documented in the crate's module documentation.
