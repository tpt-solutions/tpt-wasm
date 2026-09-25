# Layer Boundaries

## Decode → Validate

Input: raw bytes  
Output: `Module` (structural representation)

The decoder only decodes. It does not validate types, check indices, or instantiate.

```
bytes → decode() → Module
```

## Validate → Execute

Input: `Module`  
Output: `ValidatedModule` (with `ValidationCertificate`)

The validator establishes static properties. A `ValidatedModule` can be safely
passed to any execution backend.

```
Module → validate() → ValidatedModule
```

## Interpreter → Host

The interpreter calls into the host via `Step::HostCall`. The host has no knowledge
of the interpreter's internal state. The capability system mediates all interaction.

```
step() → Step::HostCall → capability.invoke() → Result<Vec<Value>, HostError>
```

## Interpreter → Formal Model

The formal model (Lean 4) defines transitions C → C'. The Rust `step()` function
implements the same transitions. The correspondence is:

```
FormalRule(op) ≈ Interpreter(op)   (per instruction)
```

## Validate → Instantiate and Link

Input: `ValidatedModule` plus explicit host/instance definitions
Output: `Instance` sharing an engine-owned `Store`

Instantiation resolves imports against typed linker definitions. Wasm function,
table, memory, and global exports are represented by addresses in one shared
store; mutable state therefore aliases the provider instance rather than being
copied. Function signatures and Wasm limit/global types are checked before the
module is committed. Definitions from unrelated stores are rejected.

```
ValidatedModule → Linker resolution → shared Store → Instance
```

## Validate → IR

The compiler receives a `ValidatedModule` and produces IR. It does not re-validate.

```
ValidatedModule → IR lowering → IrFunction
```

## IR → Machine Code

```
IrFunction → codegen → CompiledFunction { arch, code }
```

Unsafe code is confined to the boundary between the codegen crate and
executable memory mapping. Each `unsafe` block documents its safety invariant.

## TPT Host → TPT System (future)

```
tpt-wasm-host → tpt-system adapter stub → tpt-system (future)
```

The adapter prevents the core runtime from depending on the TPT ecosystem.
