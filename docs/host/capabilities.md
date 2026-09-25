# Capability Architecture

**Crates:** `tpt-wasm-capability`, `tpt-wasm-host`

---

## Core principle

No ambient authority.

WebAssembly programs may only interact with host resources through explicitly
granted capabilities. There is no default filesystem, network, or clock access.

---

## Types

```rust
pub struct CapabilityId(String);

pub struct CapabilityDescriptor {
    pub id: CapabilityId,
    pub operations: Vec<Operation>,
}

pub struct CapabilityGrant {
    pub capability: CapabilityId,
    pub permissions: Permissions,
}
```

---

## Invocation

```rust
pub trait Capability {
    fn id(&self) -> &CapabilityId;
    fn invoke(&mut self, operation: &Operation, args: &[Value]) -> Result<Vec<Value>, HostError>;
}
```

The trait is not tied to filesystem, sockets, or POSIX. Implementations are
`Send + Sync` so a capability-backed runtime import can safely be registered
with the runtime's shared execution context.

## Runtime bridge

A runtime import can be backed by one explicitly granted capability:

```rust
let function = CapabilityHostFunction::new(
    Arc::new(Mutex::new(host)),
    capability_id,
    Operation::execute("read"),
    function_type,
);
engine.linker_mut().define_function("env", "call", function);
```

The adapter authorizes and invokes through `Host::invoke` on every call. It
does not grant authority implicitly, and ungranted calls do not reach the
capability implementation or append an effect record.

---

## TPT capability namespace (future)

Eventually, TPT domain systems expose capabilities:

```
tpt.math
tpt.geometry
tpt.physics
tpt.engineering
tpt.fem
tpt.robotics
tpt.workflow
tpt.system
```

These are external capability implementations, not built into Core Wasm.

---

## Formal invariant

```
Authority(WasmInstance) ⊆ GrantedCapabilities(WasmInstance)
```

and:

```
Invocation(WasmInstance, Capability) ⇒ Capability ∈ GrantedCapabilities
```

---

## Effect tracking

```rust
pub enum Effect {
    Pure,
    Read(CapabilityId),
    Write(CapabilityId),
    Execute(CapabilityId),
    External(CapabilityId),
}
```

Effect tracking feeds: formal verification, deterministic replay, compiler
optimization, security auditing, and TPT provenance.
