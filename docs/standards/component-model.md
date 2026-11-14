# Component Model

**Status:** Partly done (M10). The envelope decodes in `crates/tpt-wasm-component`; instantiation and the WIT mapping are not started.  
**Specification:** https://component-model.bytecodealliance.org/  

The Component Model is a higher-level layer over Core WebAssembly.

```
Component
   │
   ├── Core Wasm module(s)
   │
   ▼
Component runtime
   │
   ▼
TPT Host
```

The Micro Interpreter must not be polluted with Component Model concepts.
Core Wasm is the foundation; the Component Model builds on top.

## Integration path

```
Core Module → Component → WIT Interface → TPT Capability
```

The Component Model emphasizes capability-safe interfaces and fine-grained sandboxing,
making it a natural fit for the TPT Host architecture.

## Scope so far

`tpt-wasm-component` is a separate crate above Core Wasm, so the Micro interpreter and the
runtime do not depend on it. It decodes the component preamble (the magic, version `0x0d`,
layer `0x01`), the section framing, and the core modules a component nests, as raw bytes.

Decided on the recommendation: a section of any kind other than custom sections (skipped)
and core modules (collected) is refused with its id and offset. Those sections carry types,
instances, canonical functions, imports, and exports, and running a component without them
would be a silent partial run. Each is added with its own decoder and tests.

Not done: instantiating the nested core modules, the component's type and import checks,
canonical lifting and lowering, and the WIT mapping to capabilities.

## Design constraint

The TPT Host ABI must not be designed in a way that makes future Component Model
integration impossible. This informs the capability and resource model design.
